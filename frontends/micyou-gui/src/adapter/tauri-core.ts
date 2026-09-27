/*
 * libmicyou — micyou-gui frontend adapter.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou command semantics)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou adapter shim)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

/**
 * `invoke()` compatibility shim between the original MicYou Vue frontend and
 * the libmicyou backend.
 *
 * At bundle time the `adapterShim` plugin in `vite.config.ts` substitutes this
 * module for `@tauri-apps/api/core` in every application source file, so the
 * ported Vue code keeps calling the stock MicYou command names untouched.
 *
 * - Commands listed in {@link RPC_MAP} are translated to a single generic
 *   `backend_rpc` Tauri command (JSON-RPC method + params), with small
 *   per-command argument/result shape adapters where the original Tauri
 *   command surface and the libmicyou contract differ (e.g. `set_mute_state`
 *   sends `{ isMuted }` but `audio/mute/set` takes `{ muted }`).
 * - Everything else (window/tray/theme/log/locale-shell commands) is
 *   implemented locally by the Tauri shell under the original names and
 *   passes straight through.
 *
 * Backend events need no shim: the shell re-emits the daemon's `ServerEvent`
 * stream under the original Tauri event names and payload shapes.
 */

import {
  invoke as realInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from '@tauri-apps/api/core';

export type { InvokeArgs, InvokeOptions } from '@tauri-apps/api/core';
export { convertFileSrc } from '@tauri-apps/api/core';

type Obj = Record<string, unknown>;

/** Invoke args are always plain objects on the mapped command surface. */
const asObj = (args: InvokeArgs | undefined): Obj => (args ?? {}) as Obj;

interface RpcSpec {
  /** libmicyou JSON-RPC method (see crates/micyou-api/src/methods.rs). */
  method: string;
  /** Map Vue-side invoke args onto RPC params (default: pass through). */
  params?: (args: InvokeArgs | undefined) => unknown;
  /** Map the RPC result onto the shape Vue expects (default: identity). */
  result?: (value: unknown) => unknown;
}

/** `{ message }` → bare string (upstream commands returned String). */
const message = (v: unknown): unknown => (v as Obj).message;

/**
 * Command → RPC mapping. Kept in sync with the original Tauri command table
 * (`tauri-app/src-tauri/src/commands/*`) and the libmicyou method catalogue
 * (`crates/micyou-api/src/methods.rs`), which documents the same pairing.
 */
const RPC_MAP: Record<string, RpcSpec> = {
  // ── server lifecycle & preferences ─────────────────────────────────────
  start_server: { method: 'server/start', result: message },
  stop_server: { method: 'server/stop', result: message },
  get_streaming_status: { method: 'server/status' },
  get_server_prefs: { method: 'server/prefs/get' },
  save_server_prefs: {
    method: 'server/prefs/save',
    // Vue wraps the prefs object; the RPC takes it flat.
    params: (a) => asObj(a).prefs ?? {},
    result: message,
  },
  server_prefs_exists: { method: 'server/prefs/exists' },

  // ── audio ───────────────────────────────────────────────────────────────
  get_audio_devices: { method: 'audio/devices' },
  get_audio_settings: { method: 'audio/settings/get' },
  update_audio_settings: { method: 'audio/settings/update', result: message },
  set_mute_state: {
    method: 'audio/mute/set',
    params: (a) => ({ muted: asObj(a).isMuted }),
  },
  set_monitoring: { method: 'audio/monitoring/set' },
  set_spectrum_streaming: { method: 'audio/spectrum/set' },

  // ── network / usb ───────────────────────────────────────────────────────
  get_network_info: { method: 'network/info' },
  get_network_interfaces: { method: 'network/interfaces' },
  allow_firewall: { method: 'network/firewall/allow' },
  enable_usb_mode: { method: 'usb/enable' },

  // ── virtual audio devices ───────────────────────────────────────────────
  check_vbcable: {
    method: 'devices/vbcable/check',
    // Upstream returned a bare boolean.
    result: (v) => (v as Obj).installed,
  },
  install_vbcable: { method: 'devices/vbcable/install' },
  check_blackhole: { method: 'devices/blackhole/check' },
  set_blackhole_as_input: { method: 'devices/blackhole/setInput' },
  restore_input_device: { method: 'devices/blackhole/restore' },
  check_pipewire: {
    method: 'devices/pipewire/check',
    // Vue reads the upstream snake_case shape; the RPC result is camelCase.
    result: (v) => {
      const s = v as Obj;
      return {
        available: s.available,
        setup: s.setup,
        device_exists: s.deviceExists,
        install_command: s.installCommand,
        distro: s.distro,
      };
    },
  },

  // ── shared config ───────────────────────────────────────────────────────
  save_ui_prefs: { method: 'config/ui/save' },
  save_theme_colors: { method: 'config/theme/save' },

  // ── plugins ─────────────────────────────────────────────────────────────
  list_plugins: { method: 'plugins/list' },
  set_plugin_enabled: { method: 'plugins/setEnabled' },
  uninstall_plugin: { method: 'plugins/uninstall' },
  get_plugin_config: { method: 'plugins/config/get' },
  set_plugin_config: { method: 'plugins/config/set' },
  get_plugin_logs: { method: 'plugins/logs' },
  get_plugin_sync_status: { method: 'plugins/syncStatus' },
  preview_plugin_zip: {
    method: 'plugins/preview/zip',
    params: (a) => ({ path: asObj(a).zipPath }),
  },
  preview_plugin_from_url: { method: 'plugins/preview/url' },
  install_plugin_from_url: { method: 'plugins/install/url' },
  cancel_plugin_download: { method: 'plugins/install/cancel' },
  check_plugin_updates: { method: 'plugins/update/check' },
  update_plugin: {
    method: 'plugins/update/apply',
    // Upstream returned the new version as a bare string.
    result: (v) => (v as Obj).version,
  },
  import_plugin: { method: 'plugins/import' },
  plugin_trigger: { method: 'plugins/trigger' },
  get_plugin_panel: {
    method: 'plugins/panel',
    // Upstream returned the panel HTML as a bare string.
    result: (v) => (v as Obj).html,
  },
  get_plugin_panel_icons: { method: 'plugins/panelIcons' },

  // ── run mode ────────────────────────────────────────────────────────────
  get_mode_status: { method: 'mode/status' },

  // ── system ──────────────────────────────────────────────────────────────
  check_app_update: { method: 'system/update/check' },
  get_sponsors: {
    method: 'system/sponsors',
    // Upstream returned the raw Afdian response body as a string.
    result: (v) => (v as Obj).raw,
  },
  get_app_locale: {
    method: 'system/locale',
    result: (v) => (v as Obj).language,
  },
};

/**
 * Drop-in replacement for the stock `invoke`. Mapped commands are routed
 * through the shell's generic `backend_rpc` bridge; unmapped commands reach
 * the shell's local implementations unchanged.
 */
export async function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  const spec = RPC_MAP[cmd];
  if (!spec) {
    return realInvoke<T>(cmd, args, options);
  }
  const params = spec.params ? spec.params(args) : (args ?? {});
  const value = await realInvoke<unknown>(
    'backend_rpc',
    { method: spec.method, params },
    options,
  );
  return (spec.result ? spec.result(value) : value) as T;
}
