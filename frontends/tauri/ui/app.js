/*
 * libmicyou — Tauri 2 reference frontend logic.
 * Drives the full backend JSON-RPC surface through the generic
 * `backend_call` Tauri command + forwarded `backend-event`s.
 * Copyright (C) 2026 OrientCOMPASS — GPL-3.0-or-later + MicYou Plugin Exception.
 */
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

/* ── helpers ─────────────────────────────────────────────── */
const $ = (id) => document.getElementById(id);
const call = (method, params) => invoke("backend_call", { method, params });

function toast(msg, kind = "") {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = msg;
  $("toasts").appendChild(el);
  setTimeout(() => el.remove(), kind === "err" ? 6500 : 3500);
}

const EVLOG_MAX = 200;
function evlog(line) {
  const box = $("evlog");
  const at = new Date().toLocaleTimeString();
  box.textContent = `[${at}] ${line}\n` + box.textContent;
  const lines = box.textContent.split("\n");
  if (lines.length > EVLOG_MAX) box.textContent = lines.slice(0, EVLOG_MAX).join("\n");
}

function kv(obj) {
  return Object.entries(obj)
    .map(([k, v]) => `${k}: <b>${v ?? "-"}</b>`)
    .join(" · ");
}

async function tryCall(method, params, okMsg) {
  try {
    const r = await call(method, params);
    if (okMsg) toast(okMsg, "ok");
    return r;
  } catch (e) {
    toast(`${method} 失败: ${e}`, "err");
    throw e;
  }
}

/* ── app state ───────────────────────────────────────────── */
const S = {
  connected: false,
  info: null,
  status: null,
  dsp: null,
  dspDirty: false,
  devices: [],
  plugins: [],
  selectedPlugin: null,
  os: "",
};

/* ── navigation ──────────────────────────────────────────── */
const PAGE_LOADERS = {
  overview: () => refreshStatus(),
  audio: loadAudioPage,
  connection: loadConnectionPage,
  devices: loadDevicesPage,
  plugins: loadPluginsPage,
  settings: loadSettingsPage,
  system: loadSystemPage,
};

function showPage(id) {
  document.querySelectorAll(".page").forEach((p) => p.classList.toggle("active", p.id === `page-${id}`));
  document.querySelectorAll(".nav-item").forEach((b) => b.classList.toggle("active", b.dataset.page === id));
  if (S.connected && PAGE_LOADERS[id]) PAGE_LOADERS[id]().catch((e) => console.warn(e));
}
document.querySelectorAll(".nav-item").forEach((b) => (b.onclick = () => showPage(b.dataset.page)));

/* ── topbar / status ─────────────────────────────────────── */
async function refreshStatus() {
  if (!S.connected) return;
  const st = await invoke("backend_status").catch(() => null);
  S.status = st;
  if (!st) return;
  const phase = $("pill-phase");
  phase.textContent = st.phase;
  phase.className = `pill ${st.isServerRunning ? "run" : ""}`;
  const dev = $("pill-device");
  dev.textContent = st.isConnected ? "设备已连接" : "无设备";
  dev.className = `pill ${st.isConnected ? "on" : ""}`;
  $("pill-mode").textContent = st.mode ? `${st.mode}:${st.port ?? "?"}` : "-";

  $("btn-mute").classList.toggle("primary", st.isMuted);
  $("btn-monitor").classList.toggle("primary", st.isMonitoring);
  syncCheck("dsp-mute", st.isMuted);
  syncCheck("dsp-monitor", st.isMonitoring);

  $("ov-status").innerHTML = kv({
    相态: st.phase,
    运行: st.isServerRunning ? "是" : "否",
    设备: st.isConnected ? "已连接" : "无",
    静音: st.isMuted ? "是" : "否",
    监听: st.isMonitoring ? "是" : "否",
  });
}

function syncCheck(id, value) {
  const el = $(id);
  if (el && el.checked !== value) el.checked = value;
}

/* ── overview actions ────────────────────────────────────── */
$("btn-start").onclick = async () => {
  const port = parseInt($("q-port").value, 10) || undefined;
  const mode = $("q-mode").value;
  $("btn-start").disabled = true;
  try {
    const r = await invoke("backend_start", { port, mode });
    toast(r, "ok");
    evlog(`server/start → ${r}`);
  } catch (e) {
    toast(`启动失败: ${e}`, "err");
  } finally {
    $("btn-start").disabled = false;
    refreshStatus();
  }
};
$("btn-stop").onclick = async () => {
  try {
    toast(await invoke("backend_stop"), "ok");
  } catch (e) {
    toast(`停止失败: ${e}`, "err");
  }
  refreshStatus();
};
$("btn-mute").onclick = () => toggleMute(!(S.status?.isMuted ?? false));
$("btn-monitor").onclick = () => toggleMonitor(!(S.status?.isMonitoring ?? false));

async function toggleMute(muted) {
  try { await invoke("backend_set_mute", { muted }); } catch (e) { toast(`${e}`, "err"); }
}
async function toggleMonitor(enabled) {
  try { await invoke("backend_set_monitoring", { enabled }); } catch (e) { toast(`${e}`, "err"); }
}
$("dsp-mute").onchange = (e) => toggleMute(e.target.checked);
$("dsp-monitor").onchange = (e) => toggleMonitor(e.target.checked);
$("chk-spectrum").onchange = async (e) => {
  await tryCall("audio/spectrum/set", { enabled: e.target.checked });
};

/* ── audio page ──────────────────────────────────────────── */
const EQ_LABELS = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];

function buildEqBands() {
  const wrap = $("eq-bands");
  wrap.innerHTML = "";
  wrap.style.cssText = "display:grid;grid-template-columns:repeat(10,1fr);gap:8px;margin:10px 0;";
  EQ_LABELS.forEach((label, i) => {
    const cell = document.createElement("div");
    cell.className = "field";
    cell.style.cssText = "text-align:center;padding:8px 4px;";
    cell.innerHTML = `
      <div style="color:var(--dim);font-size:10.5px">${label}Hz</div>
      <input type="range" class="eq-band" data-i="${i}" min="-12" max="12" step="0.5"
             style="writing-mode:vertical-lr;direction:rtl;height:90px;width:22px;margin:6px auto;accent-color:var(--accent);" />
      <output id="o-eq${i}" style="font-size:11px">0</output>`;
    wrap.appendChild(cell);
  });
  wrap.querySelectorAll(".eq-band").forEach((el) => {
    el.oninput = () => {
      $(`o-eq${el.dataset.i}`).textContent = el.value;
      markDirty();
    };
  });
}

function markDirty() {
  S.dspDirty = true;
  $("dsp-dirty").textContent = "● 有未保存修改";
  $("dsp-dirty").style.color = "var(--warn)";
}
function clearDirty() {
  S.dspDirty = false;
  $("dsp-dirty").textContent = "";
}

function dspToForm(d) {
  S.dsp = d;
  $("dsp-buffer").value = d.outputBufferMs ?? 300;
  $("dsp-gain").value = d.gain; $("o-gain").textContent = fmt(d.gain);
  syncCheck("dsp-ns", d.nsEnabled);
  $("dsp-ns-type").value = d.nsType || "PureVox";
  $("dsp-ns-intensity").value = d.nsIntensity; $("o-nsi").textContent = fmt(d.nsIntensity);
  syncCheck("dsp-dereverb", d.dereverbEnabled);
  $("dsp-dereverb-level").value = d.dereverbLevel; $("o-drv").textContent = fmt(d.dereverbLevel);
  syncCheck("dsp-agc", d.agcEnabled);
  $("dsp-agc-target").value = d.agcTarget;
  $("dsp-agc-attack").value = d.agcAttack; $("o-agca").textContent = fmt(d.agcAttack);
  $("dsp-agc-decay").value = d.agcDecay; $("o-agcd").textContent = fmt(d.agcDecay);
  syncCheck("dsp-vad", d.vadEnabled);
  $("dsp-vad-threshold").value = d.vadThreshold; $("o-vad").textContent = fmt(d.vadThreshold);
  syncCheck("dsp-aec", d.aecEnabled);
  syncCheck("dsp-eq", d.equalizer?.enabled);
  $("dsp-eq-preamp").value = d.equalizer?.preAmp ?? 0;
  $("o-preamp").textContent = fmt(d.equalizer?.preAmp ?? 0);
  const gains = d.equalizer?.gains ?? new Array(10).fill(0);
  document.querySelectorAll(".eq-band").forEach((el) => {
    const v = gains[+el.dataset.i] ?? 0;
    el.value = v;
    $(`o-eq${el.dataset.i}`).textContent = fmt(v);
  });
  $("chain-chips").innerHTML = (d.processingChain ?? [])
    .map((n) => `<span class="chip ${n.startsWith("Plugin:") ? "accent" : ""}">${n}</span>`)
    .join("");
  clearDirty();
}

function fmt(v) { return Number(v).toFixed(1).replace(/\.0$/, ""); }

function formToDsp() {
  const d = JSON.parse(JSON.stringify(S.dsp ?? {}));
  d.outputBufferMs = parseInt($("dsp-buffer").value, 10) || 300;
  d.gain = +$("dsp-gain").value;
  d.nsEnabled = $("dsp-ns").checked;
  d.nsType = $("dsp-ns-type").value;
  d.nsIntensity = +$("dsp-ns-intensity").value;
  d.dereverbEnabled = $("dsp-dereverb").checked;
  d.dereverbLevel = +$("dsp-dereverb-level").value;
  d.agcEnabled = $("dsp-agc").checked;
  d.agcTarget = +$("dsp-agc-target").value;
  d.agcAttack = +$("dsp-agc-attack").value;
  d.agcDecay = +$("dsp-agc-decay").value;
  d.vadEnabled = $("dsp-vad").checked;
  d.vadThreshold = +$("dsp-vad-threshold").value;
  d.aecEnabled = $("dsp-aec").checked;
  d.equalizer = d.equalizer ?? { enabled: false, preAmp: 0, gains: new Array(10).fill(0) };
  d.equalizer.enabled = $("dsp-eq").checked;
  d.equalizer.preAmp = +$("dsp-eq-preamp").value;
  d.equalizer.gains = [...document.querySelectorAll(".eq-band")].map((el) => +el.value);
  return d;
}

// mark dirty on any dsp input
["dsp-gain","dsp-ns-intensity","dsp-dereverb-level","dsp-agc-attack","dsp-agc-decay","dsp-vad-threshold","dsp-eq-preamp"].forEach((id) => {
  const el = $(id);
  el.oninput = () => {
    const out = el.parentElement.querySelector("output") ?? $("o-" + id.split("-")[1]);
    if (out) out.textContent = fmt(el.value);
    markDirty();
  };
});
["dsp-ns","dsp-dereverb","dsp-agc","dsp-vad","dsp-aec","dsp-eq"].forEach((id) => ($(id).onchange = markDirty));
$("dsp-ns-type").onchange = markDirty;
$("dsp-buffer").onchange = markDirty;
$("dsp-agc-target").onchange = markDirty;

async function loadAudioPage() {
  // devices
  const devices = await call("audio/devices", {}).catch(() => []);
  S.devices = devices;
  const sel = $("dsp-device");
  const current = S.dsp?.__device ?? "";
  sel.innerHTML =
    `<option value="">（默认 / 虚拟设备）</option>` +
    devices.map((d) => `<option value="${d}">${d}</option>`).join("");
  // settings
  const d = await call("audio/settings/get", {});
  dspToForm(d);
  // output device from server prefs
  const prefs = await call("server/prefs/get", {});
  sel.value = prefs.outputDevice && !["auto", "default"].includes(prefs.outputDevice) ? prefs.outputDevice : "";
  // macOS AEC hint
  $("aec-note").textContent = S.os === "macos" ? "（macOS 不支持，将被后端拒绝）" : "";
  // aec status snapshot
  $("aec-status").innerHTML = "";
}

$("btn-devices-refresh").onclick = () => loadAudioPage().catch(console.warn);
$("dsp-device").onchange = async (e) => {
  const prefs = await call("server/prefs/get", {});
  prefs.outputDevice = e.target.value;
  await tryCall("server/prefs/save", prefs, "输出设备已保存（下次启动生效）");
};
$("btn-dsp-save").onclick = async () => {
  try {
    await call("audio/settings/update", { settings: formToDsp() });
    toast("DSP 设置已保存并热应用", "ok");
    dspToForm(await call("audio/settings/get", {}));
  } catch (e) { toast(`保存失败: ${e}`, "err"); }
};
$("btn-dsp-reset").onclick = async () => {
  dspToForm(await call("audio/settings/get", {}));
  toast("已重载后端设置");
};

/* ── connection page ─────────────────────────────────────── */
async function loadConnectionPage() {
  const [info, ifaces] = await Promise.all([
    call("network/info", {}).catch(() => ({ ips: [], port: "?" })),
    call("network/interfaces", {}).catch(() => []),
  ]);
  $("net-ips").innerHTML = info.ips.map((ip) => `<span class="chip accent">${ip}</span>`).join("") || `<span class="hint">无可用接口</span>`;
  $("net-info").innerHTML = kv({ 协议端口: info.port });
  $("iface-tbody").innerHTML = ifaces
    .map((i) => `<tr><td>${i.ip}</td><td>${i.interfaceName}</td></tr>`)
    .join("");
  // firewall button only meaningful on windows
  $("btn-firewall").style.display = S.os === "windows" ? "" : "none";

  await refreshUsb();
  await refreshWeb();
}
$("btn-net-refresh").onclick = () => loadConnectionPage().catch(console.warn);
$("btn-firewall").onclick = () => tryCall("network/firewall/allow", {}, "已请求防火墙规则（UAC）");

async function refreshUsb() {
  try {
    const devices = await call("usb/devices", {});
    $("usb-devices").innerHTML =
      `<option value="">（自动选择）</option>` +
      devices.map((d) => `<option value="${d.serial}">${d.name} [${d.serial}]</option>`).join("");
  } catch (e) {
    $("usb-devices").innerHTML = `<option value="">adb 不可用</option>`;
  }
}
$("btn-usb-refresh").onclick = refreshUsb;
$("btn-usb-enable").onclick = async () => {
  const serial = $("usb-devices").value || null;
  const port = parseInt($("q-port").value, 10) || undefined;
  try {
    const r = await call("usb/enable", { port, deviceSerial: serial });
    $("usb-result").innerHTML = kv({ 结果: JSON.stringify(r) });
    toast("USB 模式已配置", "ok");
  } catch (e) {
    $("usb-result").innerHTML = `<span style="color:var(--danger)">${e}</span>`;
  }
};

async function refreshWeb() {
  try {
    const w = await call("web/status", {});
    $("web-status").innerHTML = kv({ 运行: w.running ? "是" : "否", 浏览器客户端: w.clientCount });
    const prefs = await call("server/prefs/get", {});
    const info = await call("network/info", {}).catch(() => ({ ips: [] }));
    $("web-urls").innerHTML = (info.ips ?? [])
      .map((ip) => `<span class="chip">https://${ip}:${prefs.webPort}</span>`)
      .join("");
  } catch (e) {
    $("web-status").textContent = String(e);
  }
}
$("btn-web-refresh").onclick = refreshWeb;

/* ── devices page ────────────────────────────────────────── */
async function loadDevicesPage() {
  $("dev-win").hidden = S.os !== "windows";
  $("dev-mac").hidden = S.os !== "macos";
  $("dev-linux").hidden = S.os !== "linux";
  if (S.os === "windows") await vbcCheck();
  if (S.os === "macos") await bhCheck();
  if (S.os === "linux") await pwCheck();
}
async function vbcCheck() {
  try {
    const r = await call("devices/vbcable/check", {});
    $("vbc-status").innerHTML = kv({ 已安装: r.installed ? "✅ 是" : "❌ 否" });
  } catch (e) { $("vbc-status").textContent = String(e); }
}
$("btn-vbc-check").onclick = vbcCheck;
$("btn-vbc-install").onclick = async () => {
  $("btn-vbc-install").disabled = true;
  $("vbc-progress").textContent = "下载中…（进度见下方 / 事件日志）\n";
  try {
    const r = await call("devices/vbcable/install", {});
    $("vbc-status").innerHTML = kv({ 结果: r.success ? "✅ 成功" : `❌ ${r.errorType ?? ""} ${r.message ?? ""}` });
    await vbcCheck();
  } catch (e) { toast(`${e}`, "err"); }
  $("btn-vbc-install").disabled = false;
};
async function bhCheck() {
  try {
    const r = await call("devices/blackhole/check", {});
    $("bh-status").innerHTML = kv(r);
  } catch (e) { $("bh-status").textContent = String(e); }
}
$("btn-bh-check").onclick = bhCheck;
$("btn-bh-set").onclick = async () => {
  try { toast(JSON.stringify(await call("devices/blackhole/setInput", {})), "ok"); await bhCheck(); }
  catch (e) { toast(`${e}`, "err"); }
};
$("btn-bh-restore").onclick = async () => {
  try { toast(JSON.stringify(await call("devices/blackhole/restore", {})), "ok"); await bhCheck(); }
  catch (e) { toast(`${e}`, "err"); }
};
async function pwCheck() {
  try {
    const r = await call("devices/pipewire/check", {});
    $("pw-status").innerHTML = kv({
      可用: r.available ? "是" : "否",
      已建立: r.setup ? "是" : "否",
      设备存在: r.deviceExists ? "是" : "否",
      发行版: r.distro || "-",
    });
    if (!r.available && r.installCommand)
      $("pw-status").innerHTML += `<div class="hint">安装: ${r.installCommand}</div>`;
  } catch (e) { $("pw-status").textContent = String(e); }
}
$("btn-pw-check").onclick = pwCheck;

/* ── plugins page ────────────────────────────────────────── */
async function loadPluginsPage() {
  const [plugins, sync] = await Promise.all([
    call("plugins/list", {}).catch(() => []),
    call("plugins/syncStatus", {}).catch(() => ({})),
  ]);
  S.plugins = plugins;
  $("plugin-sync").innerHTML = kv({ 跨设备: sync.transportReady ? "就绪" : "未连接" });
  renderPluginList();
}
function renderPluginList() {
  const wrap = $("plugin-list");
  wrap.innerHTML = "";
  if (!S.plugins.length) {
    wrap.innerHTML = `<div class="hint">暂无插件。将插件放入配置目录 plugins/ 后刷新。</div>`;
    return;
  }
  S.plugins.forEach((p, idx) => {
    const el = document.createElement("div");
    el.className = `pitem ${S.selectedPlugin === p.id ? "sel" : ""}`;
    el.innerHTML = `
      <div class="pinfo">
        <div class="pname">${p.name} <span class="chip dim">${p.runtime}</span> <span class="chip dim">${p.kind}</span></div>
        <div class="psub">${p.id} · v${p.version}${p.dspNode ? " · DSP 节点" : ""}</div>
        ${p.error ? `<div class="perr">${p.error}</div>` : ""}
      </div>
      <input type="checkbox" title="启用" ${p.enabled ? "checked" : ""} />`;
    const chk = el.querySelector("input");
    chk.onclick = async (e) => {
      e.stopPropagation();
      try {
        await call("plugins/setEnabled", { id: p.id, enabled: chk.checked });
        toast(`${p.name} 已${chk.checked ? "启用" : "禁用"}`, "ok");
        loadPluginsPage();
      } catch (err) { toast(`${err}`, "err"); chk.checked = !chk.checked; }
    };
    el.onclick = () => selectPlugin(p.id);
    wrap.appendChild(el);
  });
}
async function selectPlugin(id) {
  S.selectedPlugin = id;
  renderPluginList();
  const p = S.plugins.find((x) => x.id === id);
  if (!p) return;
  $("pd-name").textContent = `${p.name}`;
  $("pd-meta").innerHTML = kv({
    id: p.id, 版本: p.version, 运行时: p.runtime, 类型: p.kind,
    已加载: p.loaded ? "是" : "否", 作者: p.author ?? "-",
  }) + `<div class="hint">${p.description ?? ""}</div>`;
  $("pd-caps").innerHTML = (p.capabilities ?? []).map((c) => `<span class="chip accent">${c}</span>`).join("")
    + (p.platforms ?? []).map((c) => `<span class="chip dim">${c}</span>`).join("");
  $("pd-enabled").checked = p.enabled;
  $("pd-config").value = "";
  $("pd-log").textContent = "";
  await pdLoadConfig();
  await pdLoadLogs();
}
$("pd-enabled").onchange = async (e) => {
  if (!S.selectedPlugin) return;
  try {
    await call("plugins/setEnabled", { id: S.selectedPlugin, enabled: e.target.checked });
    loadPluginsPage();
  } catch (err) { toast(`${err}`, "err"); }
};
$("pd-uninstall").onclick = async () => {
  if (!S.selectedPlugin || !confirm(`卸载插件 ${S.selectedPlugin}？此操作删除其目录。`)) return;
  try {
    await call("plugins/uninstall", { id: S.selectedPlugin });
    toast("已卸载", "ok");
    S.selectedPlugin = null;
    loadPluginsPage();
  } catch (e) { toast(`${e}`, "err"); }
};
async function pdLoadConfig() {
  if (!S.selectedPlugin) return;
  try {
    const cfg = await call("plugins/config/get", { id: S.selectedPlugin });
    $("pd-config").value = JSON.stringify(cfg, null, 2);
  } catch (e) { $("pd-config").value = `// ${e}`; }
}
$("pd-config-load").onclick = pdLoadConfig;
$("pd-config-save").onclick = async () => {
  if (!S.selectedPlugin) return;
  let obj;
  try { obj = JSON.parse($("pd-config").value); }
  catch (e) { toast(`JSON 解析失败: ${e.message}`, "err"); return; }
  try {
    for (const [key, value] of Object.entries(obj))
      await call("plugins/config/set", { id: S.selectedPlugin, key, value });
    toast("插件配置已保存", "ok");
  } catch (e) { toast(`${e}`, "err"); }
};
$("pd-trigger").onclick = async () => {
  if (!S.selectedPlugin) return;
  const action = $("pd-action").value.trim();
  if (!action) return toast("请填写 action", "err");
  try {
    await call("plugins/trigger", {
      pluginId: S.selectedPlugin, action,
      payload: $("pd-payload").value || null,
    });
    toast(`已触发 ui:${action}`, "ok");
    setTimeout(pdLoadLogs, 400);
  } catch (e) { toast(`${e}`, "err"); }
};
async function pdLoadLogs() {
  if (!S.selectedPlugin) return;
  try {
    const lines = await call("plugins/logs", { id: S.selectedPlugin });
    $("pd-log").textContent = lines.join("\n") || "（无日志）";
  } catch (e) { $("pd-log").textContent = String(e); }
}
$("pd-logs").onclick = pdLoadLogs;
$("btn-plugins-refresh").onclick = () => loadPluginsPage().catch(console.warn);
$("btn-plugins-dir").onclick = () =>
  tryCall("plugins/dir", {}).then((r) => toast(`插件目录: ${r.path}`, "ok")).catch(() => {});

/* ── settings page ───────────────────────────────────────── */
async function loadSettingsPage() {
  const [prefs, exists, ui, theme] = await Promise.all([
    call("server/prefs/get", {}),
    call("server/prefs/exists", {}),
    call("config/ui/get", {}),
    call("config/theme/get", {}),
  ]);
  $("sp-port").value = prefs.port;
  $("sp-webport").value = prefs.webPort;
  $("sp-mode").value = prefs.mode;
  $("sp-bind").value = prefs.bindAddress;
  $("sp-autobind").checked = prefs.autoBind;
  $("sp-device").value = prefs.outputDevice;
  $("sp-mutesync").checked = prefs.muteSync;
  $("prefs-exists").textContent = exists ? "server.json 已存在" : "server.json 尚未创建（保存后生成）";
  $("ui-lang").value = ui.language ?? "";
  $("ui-color").value = normalizeColor(ui.themeColor) || "#5b7cfa";
  $("theme-colors").innerHTML = Object.entries(theme)
    .map(([k, v]) => `<span class="chip"><span style="display:inline-block;width:9px;height:9px;border-radius:2px;background:${v || "#333"};margin-right:5px"></span>${k}: ${v || "-"}</span>`)
    .join("");
}
function normalizeColor(c) {
  return /^#[0-9a-fA-F]{6}$/.test(c ?? "") ? c : null;
}
$("btn-prefs-save").onclick = async () => {
  const prefs = {
    port: +$("sp-port").value,
    webPort: +$("sp-webport").value,
    mode: $("sp-mode").value,
    bindAddress: $("sp-bind").value.trim() || "0.0.0.0",
    autoBind: $("sp-autobind").checked,
    outputDevice: $("sp-device").value.trim(),
    muteSync: $("sp-mutesync").checked,
  };
  await tryCall("server/prefs/save", prefs, "server.json 已保存");
};
$("btn-prefs-reload").onclick = () => loadSettingsPage().catch(console.warn);
$("btn-ui-save").onclick = async () => {
  await tryCall("config/ui/save", {
    language: $("ui-lang").value.trim(),
    themeColor: $("ui-color").value,
  }, "ui.json 已保存");
};
$("btn-ui-reload").onclick = () => loadSettingsPage().catch(console.warn);

/* ── system page ─────────────────────────────────────────── */
async function loadSystemPage() {
  const [ver, mode] = await Promise.all([
    call("system/version", {}).catch(() => ({})),
    call("mode/status", {}).catch(() => ({})),
  ]);
  $("sys-version").innerHTML = kv({
    后端: S.info?.backend ?? "-",
    版本: ver.version ?? S.info?.version ?? "-",
    契约: `v${ver.apiVersion ?? S.info?.apiVersion ?? "?"}`,
    平台: `${S.info?.os ?? "?"}/${S.info?.arch ?? "?"}`,
  });
  $("sys-mode").innerHTML = kv({
    锁: mode.mode ?? "-",
    pid: mode.pid ?? "-",
    存活: mode.running ? "是" : "否",
  });
  try {
    const p = await call("system/log/path", {});
    $("log-path").innerHTML = kv({ 路径: p.path });
  } catch (e) { $("log-path").textContent = String(e); }
}
$("btn-mode-refresh").onclick = () => loadSystemPage().catch(console.warn);
$("btn-mode-release").onclick = () => tryCall("mode/releaseLock", {}, "模式锁已释放（若为本进程持有）");
$("btn-update-check").onclick = async () => {
  $("sys-update").innerHTML = "检查中…";
  try {
    const r = await call("system/update/check", {});
    $("sys-update").innerHTML = kv({
      有更新: r.hasUpdate ? "✅ 是" : "否",
      当前: r.currentVersion,
      最新: r.latestVersion,
      渠道: r.isMirror ? "mirror" : "github",
    }) + `<div class="hint"><a href="${r.releaseUrl}" style="color:var(--accent)">${r.releaseUrl}</a></div>`;
  } catch (e) { $("sys-update").innerHTML = `<span style="color:var(--danger)">${e}</span>`; }
};
$("btn-log-refresh").onclick = async () => {
  try {
    const r = await call("system/log/content", { maxBytes: 65536 });
    $("log-content").textContent = r.content || "（空）";
    $("log-content").scrollTop = $("log-content").scrollHeight;
  } catch (e) { $("log-content").textContent = String(e); }
};
$("btn-log-export").onclick = async () => {
  try {
    const r = await call("system/log/export", {});
    toast(`日志已导出: ${r.path}`, "ok");
  } catch (e) { toast(`${e}`, "err"); }
};

/* ── events & meters ─────────────────────────────────────── */
let spectrumCtx = null;
function drawSpectrum(data) {
  const canvas = $("spectrum");
  if (!spectrumCtx) spectrumCtx = canvas.getContext("2d");
  const ctx = spectrumCtx;
  const W = canvas.width, H = canvas.height;
  ctx.clearRect(0, 0, W, H);
  const draw = (arr, color) => {
    if (!arr || !arr.length) return;
    ctx.fillStyle = color;
    const bw = W / arr.length;
    arr.forEach((v, i) => {
      const h = Math.max(0, Math.min(1, v)) * (H - 6);
      ctx.fillRect(i * bw + 0.5, H - h - 3, Math.max(1, bw - 1.5), h);
    });
  };
  draw(data.raw, "#5b7cfa55");
  draw(data.processed, "#3ecf8ecc");
}

function setMeters(level, muted) {
  const pct = Math.max(0, Math.min(100, level));
  for (const [fill, text, big] of [
    ["top-meter-fill", null, false],
    ["ov-meter-fill", "ov-meter-text", true],
    ["au-meter-fill", "au-meter-text", true],
  ]) {
    const el = $(fill);
    if (el) {
      el.style.width = `${pct}%`;
      el.parentElement.classList.toggle("muted", !!muted);
    }
    if (text && $(text)) $(text).textContent = muted ? "已静音" : `${pct.toFixed(0)}%`;
  }
}

function handleEvent(ev) {
  switch (ev.type) {
    case "audioLevel":
      setMeters(ev.data.level, S.status?.isMuted);
      break;
    case "audioSpectrum":
      drawSpectrum(ev.data);
      break;
    case "muteStateChanged":
      evlog(`静音 → ${ev.data.muted}`);
      refreshStatus();
      break;
    case "monitoringChanged":
      evlog(`监听 → ${ev.data.enabled}`);
      refreshStatus();
      break;
    case "deviceConnected":
      evlog(`设备连接: ${ev.data.device.name} (${ev.data.device.ip})`);
      toast(`📱 ${ev.data.device.name} 已连接`, "ok");
      refreshStatus();
      break;
    case "deviceDisconnected":
      evlog("设备断开");
      refreshStatus();
      break;
    case "serverStopped":
      evlog("服务器已停止");
      setMeters(0);
      refreshStatus();
      break;
    case "audioMetrics": {
      const m = ev.data.metrics;
      evlog(`指标 延迟${m.latencyMs}ms 网络${m.networkLatencyMs}ms 抖动${m.jitterMs.toFixed(1)}ms 丢包${(m.packetLossRate).toFixed(2)}% 码率${m.bitrate}`);
      break;
    }
    case "udpAudioWarning":
      evlog("⚠ 长时间未收到 UDP 音频（防火墙？）");
      toast("未收到 UDP 音频：请检查防火墙/网络", "err");
      break;
    case "aecStatusChanged":
      $("aec-status").innerHTML = kv({
        AEC: ev.data.status.available ? (ev.data.status.enabled ? "已启用" : "可用未启用") : `不可用(${ev.data.status.reason ?? "?"})`,
      });
      break;
    case "installProgress":
      $("vbc-progress").textContent += ev.data.message + "\n";
      evlog(`安装进度: ${ev.data.message}`);
      break;
    case "pluginListChanged":
      evlog(`插件变更: ${ev.data.pluginId}`);
      if ($("page-plugins").classList.contains("active")) loadPluginsPage();
      break;
    case "pluginLog":
      evlog(`插件[${ev.data.pluginId}] ${ev.data.level}: ${ev.data.message}`);
      if (S.selectedPlugin === ev.data.pluginId) pdLoadLogs();
      break;
    case "pluginDownloadProgress":
      evlog(`下载 ${ev.data.id}: ${ev.data.downloaded}/${ev.data.total}${ev.data.done ? " ✔" : ""}`);
      break;
    case "uiRequest":
      evlog(`UI 请求: ${JSON.stringify(ev.data.request)}（本前端暂未实现插件面板窗口）`);
      toast("插件请求打开面板（未实现）", "err");
      break;
    case "webClientCount":
      evlog(`Web 客户端: ${ev.data.count}`);
      break;
    default:
      evlog(`事件 ${ev.type}`);
  }
}

/* ── boot ────────────────────────────────────────────────── */
(async function boot() {
  buildEqBands();
  await listen("backend-event", (e) => handleEvent(e.payload));
  await listen("backend-state", (e) => {
    evlog(`后端会话: ${e.payload}`);
    if (String(e.payload).startsWith("error")) toast(`后端连接失败: ${e.payload}`, "err");
  });

  for (let i = 0; i < 60; i++) {
    try {
      if (await invoke("backend_connected")) break;
    } catch (_) {}
    await new Promise((r) => setTimeout(r, 500));
  }
  const ok = await invoke("backend_connected").catch(() => false);
  S.connected = !!ok;
  $("session-dot").classList.toggle("off", !ok);
  if (!ok) {
    $("session-label").textContent = "后端未连接";
    toast("无法连接 micyou-daemon（同目录 / PATH / $MICYOU_DAEMON）", "err");
    return;
  }
  $("session-label").textContent = "已连接";
  S.info = await invoke("backend_info").catch(() => null);
  S.os = S.info?.os ?? "";
  evlog(`已连接: ${S.info?.backend} ${S.info?.version} (api v${S.info?.apiVersion}, ${S.os}/${S.info?.arch})`);
  await refreshStatus();
  const prefs = await call("server/prefs/get", {}).catch(() => null);
  if (prefs) {
    $("q-port").value = prefs.mode === "web" ? prefs.webPort : prefs.port;
    $("q-mode").value = prefs.mode;
  }
  showPage("overview");
  setInterval(() => { if ($("page-overview").classList.contains("active")) refreshStatus(); }, 4000);
})();
