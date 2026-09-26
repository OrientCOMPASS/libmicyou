/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 * See LICENSE for details.
 */

//! Plugin management methods of the [`Backend`] service (RPC `plugins/*`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use micyou_api::methods::*;
use micyou_plugin::manifest::PluginManifest;

use crate::events::ServerEvent;
use crate::service::Backend;

static DOWNLOAD_CANCELLATIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

fn get_cancel_flag(id: &str) -> Arc<AtomicBool> {
    let map = DOWNLOAD_CANCELLATIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap();
    guard
        .entry(id.to_string())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

fn clear_cancel_flag(id: &str) {
    if let Some(map) = DOWNLOAD_CANCELLATIONS.get() {
        if let Ok(mut guard) = map.lock() {
            guard.remove(id);
        }
    }
}

const MANAGER_POISONED: &str = "plugin manager lock poisoned";

impl Backend {
    /// List all installed plugins (registry + load state). Enabled-but-failed
    /// plugins are lazily retried and their error surfaced in the view.
    pub fn plugins_list(&self) -> Result<Vec<PluginView>, String> {
        let plugins = &self.core.plugins;
        let manager = plugins
            .manager
            .lock()
            .map_err(|_| MANAGER_POISONED.to_string())?;
        let dsp_ids = plugins.dsp_registry.plugin_ids();
        let mut views: Vec<PluginView> = manager
            .entries()
            .into_iter()
            .map(|entry| {
                let m = &entry.manifest;
                let id = m.id.clone();
                PluginView {
                    dsp_node: dsp_ids.contains(&id),
                    loaded: manager.is_loaded(&id),
                    enabled: entry.state.is_enabled(),
                    error: None,
                    id: m.id.clone(),
                    name: m.name.clone(),
                    name_i18n: m.name_i18n.clone(),
                    description_i18n: m.description_i18n.clone(),
                    dependencies: m.dependencies.clone(),
                    config_schema: m.config_schema.clone(),
                    version: m.version.clone(),
                    author: m.author.clone(),
                    description: m.description.clone(),
                    runtime: m.runtime.to_string(),
                    kind: m.kind.to_string(),
                    platforms: m.platforms.clone(),
                    capabilities: m.capabilities.clone(),
                    ui: m.ui.clone(),
                }
            })
            .collect();

        // Re-attempt loading enabled-but-failed plugins lazily and report errors.
        let ids: Vec<String> = views
            .iter()
            .filter(|v| v.enabled && !v.loaded)
            .map(|v| v.id.clone())
            .collect();
        drop(manager);
        let mut retried = false;
        for id in ids {
            retried = true;
            if let Err(e) = plugins.enable_plugin(&id) {
                if let Some(view) = views.iter_mut().find(|v| v.id == id) {
                    view.error = Some(e.to_string());
                }
            }
        }
        if retried {
            // Lazy reloads may have registered new DSP nodes: sync the
            // runtime processing chain (#347).
            plugins.ensure_plugin_chain_node(&self.core.dsp_settings);
        }
        Ok(views)
    }

    /// Enable or disable a plugin (loads/unloads the runtime, updates DSP
    /// chain nodes, notifies frontends).
    pub fn plugins_set_enabled(&self, params: PluginIdEnabledParams) -> Result<Ack, String> {
        let result = if params.enabled {
            self.core.plugins.enable_plugin(&params.id)
        } else {
            self.core.plugins.disable_plugin(&params.id)
        };
        if result.is_ok() {
            self.core
                .plugins
                .ensure_plugin_chain_node(&self.core.dsp_settings);
            self.plugin_list_changed(&params.id);
        }
        result.map(|()| Ack::ok()).map_err(|e| e.to_string())
    }

    /// Uninstall a plugin (deletes its directory).
    pub fn plugins_uninstall(&self, params: PluginIdParams) -> Result<Ack, String> {
        let result = self.core.plugins.uninstall_plugin(&params.id);
        if result.is_ok() {
            self.core
                .plugins
                .ensure_plugin_chain_node(&self.core.dsp_settings);
            self.plugin_list_changed(&params.id);
        }
        result.map(|()| Ack::ok()).map_err(|e| e.to_string())
    }

    /// Read a plugin's persisted config (merged defaults + overrides).
    pub fn plugins_config_get(&self, params: PluginIdParams) -> Result<serde_json::Value, String> {
        let manager = self
            .core
            .plugins
            .manager
            .lock()
            .map_err(|_| MANAGER_POISONED.to_string())?;
        let map = manager
            .plugin_config(&params.id)
            .map_err(|e| e.to_string())?;
        Ok(serde_json::Value::Object(map))
    }

    /// Write one plugin config value and notify the plugin (`config:changed`).
    pub fn plugins_config_set(&self, params: PluginConfigSetParams) -> Result<Ack, String> {
        {
            let manager = self
                .core
                .plugins
                .manager
                .lock()
                .map_err(|_| MANAGER_POISONED.to_string())?;
            manager
                .set_plugin_config(&params.id, &params.key, params.value.clone())
                .map_err(|e| e.to_string())?;
        }

        let payload = serde_json::json!({ "key": params.key, "value": params.value });
        let msg = micyou_plugin::bus::PluginMessage::new(
            "host",
            &params.id,
            "config:changed",
            payload.to_string().into_bytes(),
        );
        self.core.plugins.bus.handle_incoming(&msg);
        Ok(Ack::ok())
    }

    /// Recent log lines emitted by a plugin.
    pub fn plugins_logs(&self, params: PluginIdParams) -> Result<Vec<String>, String> {
        Ok(self.core.plugins.logs.lines(&params.id))
    }

    /// Cross-device plugin sync status.
    pub fn plugins_sync_status(&self) -> PluginSyncStatus {
        let connected = self.core.plugins.sync.is_connected();
        PluginSyncStatus {
            device_connected: connected,
            transport_ready: connected,
        }
    }

    /// Absolute plugins directory (created on demand). Graphical frontends
    /// open it themselves; `open::that` is attempted best-effort for
    /// session-bound daemons.
    pub fn plugins_dir(&self) -> Result<PluginDirResult, String> {
        let dir = self
            .core
            .plugins
            .manager
            .lock()
            .map_err(|_| MANAGER_POISONED.to_string())?
            .plugins_dir()
            .to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Best effort: fails silently on headless servers without a session.
        let _ = open::that(&dir);
        Ok(PluginDirResult {
            path: dir.display().to_string(),
        })
    }

    /// Preview a local plugin zip's manifest (permission prompt input).
    pub fn plugins_preview_zip(
        &self,
        params: PluginPreviewZipParams,
    ) -> Result<PluginPreview, String> {
        let (manifest, _prefix) = read_manifest_from_zip(&std::path::PathBuf::from(&params.path))?;
        Ok(preview_from_manifest(&manifest))
    }

    /// Preview a remote plugin manifest (marketplace permission prompt).
    pub async fn plugins_preview_url(
        &self,
        params: PluginPreviewUrlParams,
    ) -> Result<PluginPreview, String> {
        let manifest_url = params.manifest_url;
        tokio::task::spawn_blocking(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .map_err(|e| format!("http client: {e}"))?;
            let text = client
                .get(&manifest_url)
                .send()
                .map_err(|e| format!("fetch manifest: {e}"))?
                .error_for_status()
                .map_err(|e| format!("fetch manifest: {e}"))?
                .text()
                .map_err(|e| format!("read manifest: {e}"))?;
            let manifest = PluginManifest::from_json(&text)
                .map_err(|e| format!("invalid plugin manifest: {e}"))?;
            Ok(preview_from_manifest(&manifest))
        })
        .await
        .map_err(|e| format!("preview task panicked: {e}"))?
    }

    /// Download a plugin zip (resumable, cancellable, progress events) and
    /// install + auto-enable it.
    pub async fn plugins_install_url(
        &self,
        params: PluginInstallParams,
    ) -> Result<PluginIdParams, String> {
        let core = self.core.clone();
        let PluginInstallParams { id, zip_url } = params;
        let id_result = tokio::task::spawn_blocking(move || {
            let client = reqwest::blocking::Client::new();

            let temp_dir = std::env::temp_dir();
            let temp_zip_path = temp_dir.join(format!("micyou-market-{id}.zip"));

            let mut downloaded_bytes: u64 = 0;
            if temp_zip_path.exists() {
                downloaded_bytes = std::fs::metadata(&temp_zip_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
            }

            let mut request = client.get(&zip_url);
            if downloaded_bytes > 0 {
                request = request.header(
                    reqwest::header::RANGE,
                    format!("bytes={}-", downloaded_bytes),
                );
            }

            let mut response = request
                .send()
                .map_err(|e| format!("plugin download failed: {e}"))?
                .error_for_status()
                .map_err(|e| {
                    format!(
                        "plugin download failed (manifest may be stale, refresh the market): {e}"
                    )
                })?;

            let is_append = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            let total_size = response.content_length().unwrap_or(0)
                + if is_append { downloaded_bytes } else { 0 };

            let mut out_file = if is_append {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&temp_zip_path)
                    .map_err(|e| format!("open temp file failed: {e}"))?
            } else {
                downloaded_bytes = 0;
                std::fs::File::create(&temp_zip_path)
                    .map_err(|e| format!("create temp file failed: {e}"))?
            };

            let cancel_flag = get_cancel_flag(&id);
            cancel_flag.store(false, Ordering::SeqCst);

            let mut buffer = [0u8; 8192];
            let mut last_emit = std::time::Instant::now();

            loop {
                if cancel_flag.load(Ordering::SeqCst) {
                    let _ = std::fs::remove_file(&temp_zip_path);
                    clear_cancel_flag(&id);
                    return Err("download cancelled".to_string());
                }

                let bytes_read = response.read(&mut buffer).map_err(|e| {
                    format!("read plugin package failed (network interrupted): {e}")
                })?;
                if bytes_read == 0 {
                    break;
                }

                out_file
                    .write_all(&buffer[..bytes_read])
                    .map_err(|e| format!("write temp file failed: {e}"))?;
                downloaded_bytes += bytes_read as u64;

                if last_emit.elapsed().as_millis() > 200 {
                    core.bus.publish(ServerEvent::PluginDownloadProgress {
                        id: id.clone(),
                        downloaded: downloaded_bytes,
                        total: total_size,
                        done: false,
                    });
                    last_emit = std::time::Instant::now();
                }
            }

            core.bus.publish(ServerEvent::PluginDownloadProgress {
                id: id.clone(),
                downloaded: downloaded_bytes,
                total: total_size,
                done: true,
            });
            clear_cancel_flag(&id);

            // Temp download, then the standard zip import (path-traversal guarded).
            let result = (|| {
                let plugins_dir = core
                    .plugins
                    .manager
                    .lock()
                    .map_err(|_| MANAGER_POISONED.to_string())?
                    .plugins_dir()
                    .to_path_buf();
                std::fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;
                let extracted_id = match import_plugin_zip(&temp_zip_path, &plugins_dir) {
                    Ok(imported) => imported,
                    Err(e) if e.contains("already installed") => {
                        // Idempotent: already-installed counts as success;
                        // the frontend refreshes the list afterwards.
                        read_manifest_from_zip(&temp_zip_path)
                            .map_err(|e| e.to_string())?
                            .0
                            .id
                    }
                    Err(e) => return Err(e),
                };
                {
                    let mut manager = core
                        .plugins
                        .manager
                        .lock()
                        .map_err(|_| MANAGER_POISONED.to_string())?;
                    let _ = manager.discover_plugin(plugins_dir.join(&extracted_id));
                }
                Ok::<String, String>(extracted_id)
            })();
            let _ = std::fs::remove_file(&temp_zip_path);
            let extracted_id = result?;

            // Permissions were confirmed by the frontend before the call;
            // auto-enable after install (failure does not block the install).
            if let Err(e) = core.plugins.enable_plugin(&extracted_id) {
                log::warn!("[plugins] auto-enable after install failed for {extracted_id}: {e}");
            }
            core.plugins.ensure_plugin_chain_node(&core.dsp_settings);
            core.bus.publish(ServerEvent::PluginListChanged {
                plugin_id: extracted_id.clone(),
            });
            Ok(extracted_id)
        })
        .await
        .map_err(|e| format!("download task panicked: {e}"))??;

        Ok(PluginIdParams { id: id_result })
    }

    /// Cancel an in-flight marketplace download.
    pub fn plugins_install_cancel(&self, params: PluginIdParams) -> Result<Ack, String> {
        get_cancel_flag(&params.id).store(true, Ordering::SeqCst);
        Ok(Ack::ok())
    }

    /// Check all installed plugins against their declared `updateUrl`.
    pub async fn plugins_update_check(&self) -> Result<Vec<PluginUpdate>, String> {
        let plugins = self.core.plugins.clone();
        tokio::task::spawn_blocking(move || {
            let manager = plugins
                .manager
                .lock()
                .map_err(|_| MANAGER_POISONED.to_string())?;
            let entries = manager.entries();
            let updates = entries
                .into_iter()
                .filter_map(|entry| {
                    let m = &entry.manifest;
                    let url = m.update_url.as_ref()?;
                    let current = semver::Version::parse(&m.version).ok()?;

                    let client = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .build()
                        .ok()?;
                    let text = client.get(url).send().ok()?.text().ok()?;
                    let remote = PluginManifest::from_json(&text).ok()?;
                    let latest = semver::Version::parse(&remote.version).ok()?;

                    if latest > current {
                        Some(PluginUpdate {
                            id: m.id.clone(),
                            current_version: m.version.clone(),
                            latest_version: remote.version.clone(),
                            update_url: url.clone(),
                        })
                    } else {
                        None
                    }
                })
                .collect();
            Ok(updates)
        })
        .await
        .map_err(|e| format!("update check panicked: {e}"))?
    }

    /// Apply a declared plugin update (download zip, replace, re-enable).
    pub async fn plugins_update_apply(
        &self,
        params: PluginIdParams,
    ) -> Result<PluginUpdateApplied, String> {
        let core = self.core.clone();
        let id = params.id;
        let version = tokio::task::spawn_blocking(move || {
            let (update_url, enabled) = {
                let manager = core
                    .plugins
                    .manager
                    .lock()
                    .map_err(|_| MANAGER_POISONED.to_string())?;
                let entry = manager
                    .entry(&id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("unknown plugin {id}"))?;
                let url = entry
                    .manifest
                    .update_url
                    .clone()
                    .ok_or_else(|| format!("plugin {id} declares no updateUrl"))?;
                (url, entry.state.is_enabled())
            };

            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|e| e.to_string())?;
            let text = client
                .get(&update_url)
                .send()
                .map_err(|e| format!("fetch update manifest: {e}"))?
                .text()
                .map_err(|e| e.to_string())?;
            let remote = PluginManifest::from_json(&text)
                .map_err(|e| format!("remote manifest invalid: {e}"))?;
            if remote.id != id {
                return Err(format!(
                    "remote manifest id mismatch: {} != {id}",
                    remote.id
                ));
            }

            // Zip URL resolution order:
            //  1. explicit `downloadUrl` in the remote manifest (versioned assets),
            //  2. legacy derivation: updateUrl with `.json` replaced by `.zip`.
            let zip_url = remote.download_url.clone().unwrap_or_else(|| {
                let p = std::path::Path::new(&update_url);
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let parent = p
                    .parent()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                format!("{parent}/{stem}.zip")
            });

            let tmp_zip = std::env::temp_dir().join(format!("micyou-update-{id}.zip"));
            let bytes = client
                .get(&zip_url)
                .send()
                .map_err(|e| format!("download update: {e}"))?
                .bytes()
                .map_err(|e| format!("read update: {e}"))?;
            std::fs::write(&tmp_zip, &bytes).map_err(|e| format!("write temp zip: {e}"))?;

            core.plugins.disable_plugin(&id).ok();
            let plugins_dir = core
                .plugins
                .manager
                .lock()
                .map_err(|_| MANAGER_POISONED.to_string())?
                .plugins_dir()
                .to_path_buf();
            let dest = plugins_dir.join(&id);
            if dest.exists() {
                std::fs::remove_dir_all(&dest).map_err(|e| format!("remove old install: {e}"))?;
            }
            import_plugin_zip(&tmp_zip, &plugins_dir)
                .map_err(|e| format!("install update: {e}"))?;
            let _ = std::fs::remove_file(&tmp_zip);

            if enabled {
                core.plugins.enable_plugin(&id).map_err(|e| e.to_string())?;
            }
            // Update disables then (optionally) re-enables: sync per-plugin
            // chain nodes (#347).
            core.plugins.ensure_plugin_chain_node(&core.dsp_settings);
            core.bus.publish(ServerEvent::PluginListChanged {
                plugin_id: id.clone(),
            });
            Ok(remote.version)
        })
        .await
        .map_err(|e| format!("update task panicked: {e}"))??;

        Ok(PluginUpdateApplied { version })
    }

    /// Import a plugin from a local directory or .zip file.
    pub fn plugins_import(&self, params: PluginImportParams) -> Result<PluginIdParams, String> {
        let src = std::path::PathBuf::from(&params.source);
        if !src.exists() {
            return Err(format!("source not found: {}", src.display()));
        }

        let plugins_dir = self
            .core
            .plugins
            .manager
            .lock()
            .map_err(|_| MANAGER_POISONED.to_string())?
            .plugins_dir()
            .to_path_buf();
        std::fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;

        let id = if src.is_dir() {
            import_plugin_dir(&src, &plugins_dir)
        } else if src
            .extension()
            .map(|e| e.eq_ignore_ascii_case("zip"))
            .unwrap_or(false)
        {
            import_plugin_zip(&src, &plugins_dir)
        } else {
            return Err("unsupported source: expected a directory or a .zip file".into());
        }
        .map_err(|e| e.to_string())?;

        {
            let mut manager = self
                .core
                .plugins
                .manager
                .lock()
                .map_err(|_| MANAGER_POISONED.to_string())?;
            manager
                .discover_plugin(plugins_dir.join(&id))
                .map_err(|e| e.to_string())?;
        }

        if let Err(e) = self.core.plugins.enable_plugin(&id) {
            log::warn!("[plugins] auto-enable after import failed for {id}: {e}");
        }
        self.core
            .plugins
            .ensure_plugin_chain_node(&self.core.dsp_settings);
        self.plugin_list_changed(&id);
        Ok(PluginIdParams { id })
    }

    /// Deliver a UI trigger to a plugin (`ui:<action>` bus message).
    pub fn plugins_trigger(&self, params: PluginTriggerParams) -> Result<Ack, String> {
        let bytes = params.payload.unwrap_or_default().into_bytes();
        self.core
            .plugins
            .trigger(&params.plugin_id, &params.action, &bytes)
            .map_err(|e| e.to_string())?;
        Ok(Ack::ok())
    }

    /// Panel document (HTML) + suggested title for frontend rendering.
    pub fn plugins_panel(&self, params: PluginPanelParams) -> Result<PluginPanel, String> {
        let manager = self
            .core
            .plugins
            .manager
            .lock()
            .map_err(|_| MANAGER_POISONED.to_string())?;
        let entry = manager
            .entry(&params.plugin_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("unknown plugin {}", params.plugin_id))?;
        let panel = entry
            .manifest
            .ui
            .as_ref()
            .and_then(|u| u.panels.iter().find(|p| p.id == params.panel_id))
            .ok_or_else(|| format!("unknown panel {}", params.panel_id))?;
        let path = entry.dir.join(&panel.entry);
        let html =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Ok(PluginPanel {
            html,
            title: format!("{} · {}", entry.manifest.name, panel.label),
        })
    }

    /// Panel icons set by plugins via `set_panel_icon`.
    pub fn plugins_panel_icons(&self, params: PluginIdParams) -> HashMap<String, String> {
        self.core
            .plugins
            .panel_icons
            .lock()
            .map(|m| m.get(&params.id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Ask the attached graphical frontend to open a plugin panel window
    /// (fails with a clear error when no frontend is attached).
    pub fn plugins_window_open(&self, params: PluginPanelParams) -> Result<Ack, String> {
        // Validate the panel exists before bothering the frontend.
        let _panel = self.plugins_panel(PluginPanelParams {
            plugin_id: params.plugin_id.clone(),
            panel_id: params.panel_id.clone(),
        })?;
        self.core
            .plugins
            .ui
            .open_panel(&params.plugin_id, &params.panel_id)
            .map_err(|e| e.to_string())?;
        Ok(Ack::ok())
    }

    fn plugin_list_changed(&self, plugin_id: &str) {
        self.core.bus.publish(ServerEvent::PluginListChanged {
            plugin_id: plugin_id.to_string(),
        });
    }
}

fn preview_from_manifest(manifest: &PluginManifest) -> PluginPreview {
    PluginPreview {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        author: manifest.author.clone(),
        description: manifest.description.clone(),
        runtime: manifest.runtime.to_string(),
        kind: format!("{:?}", manifest.kind).to_lowercase(),
        capabilities: manifest.capabilities.clone(),
        license: manifest.license.clone(),
        homepage: manifest.homepage.clone(),
    }
}

fn read_manifest_from_zip(
    zip_path: &std::path::Path,
) -> Result<(PluginManifest, std::path::PathBuf), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;

    let mut manifest_name: Option<String> = None;
    for i in 0..archive.len() {
        let name = archive
            .by_index(i)
            .map_err(|e| format!("zip entry: {e}"))?
            .name()
            .to_string();
        if name == "plugin.json" || name.ends_with("/plugin.json") {
            manifest_name = Some(name);
            break;
        }
    }
    let manifest_name = manifest_name.ok_or("zip contains no plugin.json")?;

    let manifest_text = {
        let mut entry = archive
            .by_name(&manifest_name)
            .map_err(|e| format!("read manifest: {e}"))?;
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|e| format!("read manifest: {e}"))?;
        text
    };

    let manifest =
        PluginManifest::from_json(&manifest_text).map_err(|e| format!("invalid plugin: {e}"))?;
    let prefix = std::path::Path::new(&manifest_name)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    Ok((manifest, prefix))
}

fn import_plugin_dir(src: &std::path::Path, dest_root: &std::path::Path) -> Result<String, String> {
    let manifest =
        PluginManifest::load_from_dir(src).map_err(|e| format!("invalid plugin: {e}"))?;
    let id = manifest.id.clone();
    let dest = dest_root.join(&id);
    if dest.exists() {
        return Err(format!("plugin {id} already installed"));
    }
    copy_dir_recursive(src, &dest).map_err(|e| format!("copy failed: {e}"))?;
    Ok(id)
}

fn import_plugin_zip(
    zip_path: &std::path::Path,
    dest_root: &std::path::Path,
) -> Result<String, String> {
    let (manifest, prefix) = read_manifest_from_zip(zip_path)?;
    let id = manifest.id.clone();
    let dest = dest_root.join(&id);
    if dest.exists() {
        return Err(format!("plugin {id} already installed"));
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("create dir: {e}"))?;

    let file = std::fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        // enclosed_name guards against path traversal (zip-slip)
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let rel = if rel.starts_with(&prefix) {
            rel.strip_prefix(&prefix).unwrap_or(&rel).to_path_buf()
        } else {
            rel.to_path_buf()
        };
        let target = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| format!("mkdir: {e}"))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            let mut out =
                std::fs::File::create(&target).map_err(|e| format!("create file: {e}"))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| format!("extract: {e}"))?;
        }
    }
    Ok(id)
}

fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
