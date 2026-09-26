/*
 * libmicyou — Slint reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

//! Slint reference frontend: embeds the libmicyou backend in-process and
//! drives the full JSON-RPC surface (server / audio+DSP / connection /
//! virtual devices / plugins / settings / system).

use micyou_api::events::ServerEvent;
use micyou_slint_frontend::controller::Session;
use serde_json::{json, Value};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel, Weak};

slint::include_modules!();

// ── UI-thread helpers ────────────────────────────────────────────────────────

fn upd(ui: &Weak<MainWindow>, f: impl Fn(&MainWindow) + Send + 'static) {
    let w = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = w.upgrade() {
            f(&ui);
        }
    });
}

fn ss(v: impl ToString) -> SharedString {
    SharedString::from(v.to_string())
}

fn log_line(ui: &Weak<MainWindow>, line: &str) {
    let line = line.to_string();
    upd(ui, move |u| {
        let current = u.get_evlog().to_string();
        let next = if current.is_empty() {
            line
        } else {
            let mut v: Vec<&str> = current.lines().collect();
            v.insert(0, line.as_str());
            v.truncate(120);
            v.join("\n")
        };
        u.set_evlog(ss(next));
    });
}

fn toast_err(ui: &Weak<MainWindow>, what: &str, e: impl std::fmt::Display) {
    log_line(ui, &format!("✗ {what}: {e}"));
}

fn jbool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}
fn jf32(v: &Value, key: &str, default: f32) -> f32 {
    v.get(key).and_then(Value::as_f64).unwrap_or(default as f64) as f32
}
fn jstr(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// ── commands from the UI thread to the controller task ──────────────────────

#[derive(Debug, Clone)]
enum Cmd {
    Nav(i32),
    Start,
    Stop,
    Mute(bool),
    Monitoring(bool),
    Spectrum(bool),
    DspSave,
    DspReload,
    DevicesRefresh,
    DeviceSelected(i32),
    NetRefresh,
    UsbRefresh,
    UsbEnable,
    Firewall,
    WebRefresh,
    VbcCheck,
    VbcInstall,
    BhCheck,
    BhSet,
    BhRestore,
    PwCheck,
    PluginsRefresh,
    PluginSelect(i32),
    PluginToggle(i32, bool),
    PluginUninstall,
    PluginConfigLoad,
    PluginConfigSave,
    PluginTrigger,
    PluginLogs,
    PluginsDir,
    PrefsSave,
    PrefsReload,
    UiSave,
    SysRefresh,
    UpdateCheck,
    ModeRelease,
    LogRefresh,
    LogExport,
}

// ── controller-side cache ───────────────────────────────────────────────────

#[derive(Default)]
struct Cache {
    os: String,
    dsp: Value,
    devices: Vec<String>,
    plugins: Vec<Value>,
    selected_plugin: Option<usize>,
    usb: Vec<(String, String)>,
}

struct Ctl {
    session: Session,
    ui: Weak<MainWindow>,
    cache: Cache,
}

impl Ctl {
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.session
            .call_raw(method, Some(params))
            .await
            .map_err(|e| e.to_string())
    }

    async fn busy(&self, on: bool, text: &str) {
        let text = if on { text.to_string() } else { String::new() };
        upd(&self.ui, move |u| u.set_busy_text(ss(text)));
    }

    // ── status ──────────────────────────────────────────────────────────
    async fn refresh_status(&self) {
        let Ok(st) = self.session.status().await else {
            return;
        };
        let phase = st.phase.clone();
        let running = st.is_server_running;
        let connected = st.is_connected;
        let muted = st.is_muted;
        let monitoring = st.is_monitoring;
        let mode_port = st
            .mode
            .map(|m| format!("{m}:{}", st.port.unwrap_or(0)))
            .unwrap_or_else(|| "-".into());
        upd(&self.ui, move |u| {
            u.set_phase(ss(phase));
            u.set_running(running);
            u.set_device_connected(connected);
            u.set_muted(muted);
            u.set_monitoring(monitoring);
            u.set_mode_port(ss(mode_port));
        });
    }

    // ── audio / dsp ─────────────────────────────────────────────────────
    async fn load_dsp(&mut self) {
        let settings = match self.call("audio/settings/get", json!({})).await {
            Ok(v) => v,
            Err(e) => {
                toast_err(&self.ui, "读取 DSP 设置", e);
                return;
            }
        };
        self.cache.dsp = settings.clone();

        let devices = self
            .call("audio/devices", json!({}))
            .await
            .ok()
            .and_then(|v| v.as_array().cloned())
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.cache.devices = devices.clone();

        let prefs = self
            .call("server/prefs/get", json!({}))
            .await
            .unwrap_or(json!({}));
        let current_device = jstr(&prefs, "outputDevice");
        let mut combo = vec!["（默认 / 虚拟设备）".to_string()];
        combo.extend(devices.iter().cloned());
        let device_index = combo.iter().position(|d| *d == current_device).unwrap_or(0) as i32;

        let eq = settings.get("equalizer").cloned().unwrap_or(json!({}));
        let gains: Vec<f32> = eq
            .get("gains")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(10)
                    .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                    .collect()
            })
            .unwrap_or_else(|| vec![0.0; 10]);
        let chain = settings
            .get("processingChain")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("  →  ")
            })
            .unwrap_or_default();
        let ns_type_index = match jstr(&settings, "nsType").as_str() {
            "RNNoise" => 1,
            "Speexdsp" => 2,
            _ => 0,
        };
        let aec_supported = self.cache.os != "macos";

        let combo_model: ModelRc<[SharedString]> = ModelRc::new(VecModel::from(
            combo.iter().map(|s| ss(s)).collect::<Vec<_>>(),
        ));
        let gains_model: ModelRc<[f32]> = ModelRc::new(VecModel::from(gains));

        upd(&self.ui, move |u| {
            u.set_dsp_gain(jf32(&settings, "gain", 0.0));
            u.set_dsp_ns(jbool(&settings, "nsEnabled"));
            u.set_dsp_ns_type(ns_type_index);
            u.set_dsp_ns_intensity(jf32(&settings, "nsIntensity", 50.0));
            u.set_dsp_dereverb(jbool(&settings, "dereverbEnabled"));
            u.set_dsp_dereverb_level(jf32(&settings, "dereverbLevel", 50.0));
            u.set_dsp_agc(jbool(&settings, "agcEnabled"));
            u.set_dsp_agc_target(jf32(&settings, "agcTarget", 16000.0));
            u.set_dsp_agc_attack(jf32(&settings, "agcAttack", 50.0));
            u.set_dsp_agc_decay(jf32(&settings, "agcDecay", 50.0));
            u.set_dsp_vad(jbool(&settings, "vadEnabled"));
            u.set_dsp_vad_threshold(jf32(&settings, "vadThreshold", -40.0));
            u.set_dsp_aec(jbool(&settings, "aecEnabled"));
            u.set_dsp_aec_supported(aec_supported);
            u.set_dsp_eq(jbool(&eq, "enabled"));
            u.set_dsp_eq_preamp(jf32(&eq, "preAmp", 0.0));
            u.set_dsp_eq_gains(gains_model);
            u.set_dsp_buffer_ms(jf32(&settings, "outputBufferMs", 300.0));
            u.set_dsp_chain(ss(chain));
            u.set_dsp_devices(combo_model);
            u.set_dsp_device_index(device_index);
            u.set_dsp_dirty(false);
        });
    }

    fn read_dsp_form(&self) -> Option<DspSnapshot> {
        let ui = self.ui.upgrade()?;
        let gains = ui.get_dsp_eq_gains().iter().take(10).collect::<Vec<f32>>();
        Some(DspSnapshot {
            gain: ui.get_dsp_gain(),
            ns: ui.get_dsp_ns(),
            ns_type: ui.get_dsp_ns_type(),
            ns_intensity: ui.get_dsp_ns_intensity(),
            dereverb: ui.get_dsp_dereverb(),
            dereverb_level: ui.get_dsp_dereverb_level(),
            agc: ui.get_dsp_agc(),
            agc_target: ui.get_dsp_agc_target(),
            agc_attack: ui.get_dsp_agc_attack(),
            agc_decay: ui.get_dsp_agc_decay(),
            vad: ui.get_dsp_vad(),
            vad_threshold: ui.get_dsp_vad_threshold(),
            aec: ui.get_dsp_aec(),
            eq: ui.get_dsp_eq(),
            eq_preamp: ui.get_dsp_eq_preamp(),
            eq_gains: gains,
            buffer_ms: ui.get_dsp_buffer_ms(),
        })
    }

    async fn dsp_save(&mut self) {
        let Some(form) = self.read_dsp_form() else {
            return;
        };
        // patch the cached settings so untouched fields (chain etc.) survive
        let mut settings = if self.cache.dsp.is_object() {
            self.cache.dsp.clone()
        } else {
            json!({})
        };
        let obj = settings.as_object_mut().unwrap();
        obj.insert("gain".into(), json!(form.gain));
        obj.insert("nsEnabled".into(), json!(form.ns));
        obj.insert(
            "nsType".into(),
            json!(["PureVox", "RNNoise", "Speexdsp"][form.ns_type.clamp(0, 2) as usize]),
        );
        obj.insert("nsIntensity".into(), json!(form.ns_intensity));
        obj.insert("dereverbEnabled".into(), json!(form.dereverb));
        obj.insert("dereverbLevel".into(), json!(form.dereverb_level));
        obj.insert("agcEnabled".into(), json!(form.agc));
        obj.insert("agcTarget".into(), json!(form.agc_target));
        obj.insert("agcAttack".into(), json!(form.agc_attack));
        obj.insert("agcDecay".into(), json!(form.agc_decay));
        obj.insert("vadEnabled".into(), json!(form.vad));
        obj.insert("vadThreshold".into(), json!(form.vad_threshold));
        obj.insert("aecEnabled".into(), json!(form.aec));
        obj.insert(
            "outputBufferMs".into(),
            json!(form.buffer_ms.round() as u32),
        );
        obj.insert(
            "equalizer".into(),
            json!({
                "enabled": form.eq,
                "preAmp": form.eq_preamp,
                "gains": form.eq_gains,
            }),
        );
        match self
            .call("audio/settings/update", json!({ "settings": settings }))
            .await
        {
            Ok(_) => {
                log_line(&self.ui, "✓ DSP 设置已保存并热应用");
                self.load_dsp().await;
            }
            Err(e) => toast_err(&self.ui, "保存 DSP 设置", e),
        }
    }

    async fn device_selected(&mut self, index: i32) {
        let device = if index <= 0 {
            String::new()
        } else {
            self.cache
                .devices
                .get((index - 1) as usize)
                .cloned()
                .unwrap_or_default()
        };
        let mut prefs = self
            .call("server/prefs/get", json!({}))
            .await
            .unwrap_or(json!({}));
        if let Some(obj) = prefs.as_object_mut() {
            obj.insert("outputDevice".into(), json!(device));
        }
        match self.call("server/prefs/save", prefs).await {
            Ok(_) => log_line(&self.ui, "✓ 输出设备已保存（下次启动服务器生效）"),
            Err(e) => toast_err(&self.ui, "保存输出设备", e),
        }
    }

    // ── connection ──────────────────────────────────────────────────────
    async fn load_connection(&mut self) {
        if let Ok(info) = self.call("network/info", json!({})).await {
            let ips: Vec<SharedString> = info
                .get("ips")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(ss).collect())
                .unwrap_or_default();
            let port = info.get("port").cloned().unwrap_or(json!("-")).to_string();
            upd(&self.ui, move |u| {
                u.set_net_ips(ModelRc::new(VecModel::from(ips)));
                u.set_net_port(ss(port));
            });
        }
        if let Ok(ifaces) = self.call("network/interfaces", json!({})).await {
            let rows: Vec<IfaceRow> = ifaces
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|i| IfaceRow {
                            ip: ss(jstr(i, "ip")),
                            name: ss(jstr(i, "interfaceName")),
                        })
                        .collect()
                })
                .unwrap_or_default();
            upd(&self.ui, move |u| {
                u.set_ifaces(ModelRc::new(VecModel::from(rows)))
            });
        }
        self.load_usb().await;
        self.load_web().await;
    }

    async fn load_usb(&mut self) {
        let mut usb = Vec::new();
        if let Ok(devs) = self.call("usb/devices", json!({})).await {
            if let Some(arr) = devs.as_array() {
                for d in arr {
                    usb.push((jstr(d, "serial"), jstr(d, "name")));
                }
            }
        }
        let combo: Vec<SharedString> = {
            let mut v = vec![ss("（自动选择）")];
            v.extend(usb.iter().map(|(s, n)| ss(format!("{n} [{s}]"))));
            v
        };
        let unavailable = usb.is_empty();
        self.cache.usb = usb;
        upd(&self.ui, move |u| {
            u.set_usb_combo(ModelRc::new(VecModel::from(combo)));
            u.set_usb_index(0);
            if unavailable {
                u.set_usb_result(ss("未发现 adb 设备（需安装 adb 并开启 USB 调试）"));
            }
        });
    }

    async fn usb_enable(&self) {
        let ui = self.ui.upgrade().unwrap();
        let index = ui.get_usb_index();
        let port: u16 = ui.get_q_port().to_string().trim().parse().unwrap_or(18554);
        drop(ui);
        let serial = if index > 0 {
            self.cache
                .usb
                .get((index - 1) as usize)
                .map(|(s, _)| s.clone())
        } else {
            None
        };
        match self
            .call(
                "usb/enable",
                json!({ "port": port, "deviceSerial": serial }),
            )
            .await
        {
            Ok(r) => upd(&self.ui, move |u| u.set_usb_result(ss(format!("✓ {r}")))),
            Err(e) => upd(&self.ui, move |u| u.set_usb_result(ss(format!("✗ {e}")))),
        }
    }

    async fn load_web(&self) {
        let status = self
            .call("web/status", json!({}))
            .await
            .unwrap_or(json!({}));
        let prefs = self
            .call("server/prefs/get", json!({}))
            .await
            .unwrap_or(json!({}));
        let info = self
            .call("network/info", json!({}))
            .await
            .unwrap_or(json!({}));
        let web_port = prefs.get("webPort").cloned().unwrap_or(json!(8443));
        let text = format!(
            "运行: {}   浏览器客户端: {}",
            if jbool(&status, "running") {
                "是"
            } else {
                "否"
            },
            status.get("clientCount").cloned().unwrap_or(json!(0))
        );
        let urls: Vec<SharedString> = info
            .get("ips")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|ip| ss(format!("https://{ip}:{web_port}")))
                    .collect()
            })
            .unwrap_or_default();
        upd(&self.ui, move |u| {
            u.set_web_status(ss(text));
            u.set_web_urls(ModelRc::new(VecModel::from(urls)));
        });
    }

    // ── virtual devices ─────────────────────────────────────────────────
    async fn load_devices_page(&self) {
        match self.cache.os.as_str() {
            "windows" => self.vbc_check().await,
            "macos" => self.bh_check().await,
            "linux" => self.pw_check().await,
            _ => {}
        }
    }
    async fn vbc_check(&self) {
        match self.call("devices/vbcable/check", json!({})).await {
            Ok(r) => {
                let installed = jbool(&r, "installed");
                upd(&self.ui, move |u| {
                    u.set_vbc_status(ss(if installed {
                        "✅ 已安装"
                    } else {
                        "❌ 未安装"
                    }))
                })
            }
            Err(e) => upd(&self.ui, move |u| u.set_vbc_status(ss(format!("✗ {e}")))),
        }
    }
    async fn vbc_install(&self) {
        self.busy(true, "VB-CABLE 下载安装中（留意 UAC 弹窗）…")
            .await;
        let result = self.call("devices/vbcable/install", json!({})).await;
        self.busy(false, "").await;
        match result {
            Ok(r) => {
                let ok = jbool(&r, "success");
                let msg = jstr(&r, "message");
                upd(&self.ui, move |u| {
                    u.set_vbc_status(ss(if ok {
                        "✅ 安装成功"
                    } else {
                        "❌ 安装失败"
                    }))
                });
                log_line(&self.ui, &format!("VB-CABLE install → {ok} {msg}"));
            }
            Err(e) => toast_err(&self.ui, "VB-CABLE 安装", e),
        }
    }
    async fn bh_check(&self) {
        match self.call("devices/blackhole/check", json!({})).await {
            Ok(r) => {
                let text = r.to_string();
                upd(&self.ui, move |u| u.set_bh_status(ss(text)))
            }
            Err(e) => upd(&self.ui, move |u| u.set_bh_status(ss(format!("✗ {e}")))),
        }
    }
    async fn bh_action(&self, method: &str) {
        match self.call(method, json!({})).await {
            Ok(r) => log_line(&self.ui, &format!("{method} → {r}")),
            Err(e) => toast_err(&self.ui, method, e),
        }
        self.bh_check().await;
    }
    async fn pw_check(&self) {
        match self.call("devices/pipewire/check", json!({})).await {
            Ok(r) => {
                let text = format!(
                    "可用: {}   已建立: {}   设备存在: {}   发行版: {}",
                    jbool(&r, "available"),
                    jbool(&r, "setup"),
                    jbool(&r, "deviceExists"),
                    jstr(&r, "distro")
                );
                let install = jstr(&r, "installCommand");
                let text = if !jbool(&r, "available") && !install.is_empty() {
                    format!("{text}\n安装命令: {install}")
                } else {
                    text
                };
                upd(&self.ui, move |u| u.set_pw_status(ss(text)))
            }
            Err(e) => upd(&self.ui, move |u| u.set_pw_status(ss(format!("✗ {e}")))),
        }
    }

    // ── plugins ─────────────────────────────────────────────────────────
    async fn load_plugins(&mut self) {
        let plugins = self
            .call("plugins/list", json!({}))
            .await
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let sync = self
            .call("plugins/syncStatus", json!({}))
            .await
            .unwrap_or(json!({}));
        let selected_id = self
            .cache
            .selected_plugin
            .and_then(|i| plugins.get(i))
            .map(|p| jstr(p, "id"));
        let rows: Vec<PluginRow> = plugins
            .iter()
            .map(|p| {
                let id = jstr(p, "id");
                PluginRow {
                    selected: selected_id.as_deref() == Some(id.as_str()),
                    sub: ss(format!(
                        "{id} · v{}{}",
                        jstr(p, "version"),
                        if jbool(p, "dspNode") {
                            " · DSP 节点"
                        } else {
                            ""
                        }
                    )),
                    id: ss(id),
                    name: ss(jstr(p, "name")),
                    runtime: ss(jstr(p, "runtime")),
                    enabled: jbool(p, "enabled"),
                    has_error: p.get("error").and_then(Value::as_str).is_some(),
                    error: ss(jstr(p, "error")),
                }
            })
            .collect();
        self.cache.plugins = plugins;
        let sync_text = format!(
            "跨设备同步: {}",
            if jbool(&sync, "transportReady") {
                "就绪"
            } else {
                "未连接"
            }
        );
        upd(&self.ui, move |u| {
            u.set_plugins(ModelRc::new(VecModel::from(rows)));
            u.set_plugin_sync(ss(sync_text));
        });
    }

    async fn plugin_select(&mut self, index: i32) {
        let Some(p) = self.cache.plugins.get(index as usize).cloned() else {
            return;
        };
        self.cache.selected_plugin = Some(index as usize);
        let id = jstr(&p, "id");
        let meta = format!(
            "id: {}   版本: {}   运行时: {}   类型: {}   已加载: {}   作者: {}",
            id,
            jstr(&p, "version"),
            jstr(&p, "runtime"),
            jstr(&p, "kind"),
            if jbool(&p, "loaded") { "是" } else { "否" },
            jstr(&p, "author")
        );
        let desc = jstr(&p, "description");
        let caps = p
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("  ")
            })
            .unwrap_or_default();
        let enabled = jbool(&p, "enabled");
        let meta_full = if desc.is_empty() {
            meta
        } else {
            format!("{meta}\n{desc}")
        };
        let id2 = id.clone();
        upd(&self.ui, move |u| {
            u.set_pd_visible(true);
            u.set_pd_name(ss(id2));
            u.set_pd_meta(ss(meta_full));
            u.set_pd_caps(ss(caps));
            u.set_pd_enabled(enabled);
        });
        // re-render list selection highlight
        self.load_plugins().await;
        self.plugin_config_load().await;
        self.plugin_logs().await;
    }

    fn selected_id(&self) -> Option<String> {
        self.cache
            .selected_plugin
            .and_then(|i| self.cache.plugins.get(i))
            .map(|p| jstr(p, "id"))
    }

    async fn plugin_toggle(&mut self, index: i32, enabled: bool) {
        let Some(p) = self.cache.plugins.get(index as usize) else {
            return;
        };
        let id = jstr(p, "id");
        match self
            .call(
                "plugins/setEnabled",
                json!({ "id": id, "enabled": enabled }),
            )
            .await
        {
            Ok(_) => {
                log_line(
                    &self.ui,
                    &format!("✓ {id} 已{}", if enabled { "启用" } else { "禁用" }),
                );
                self.load_plugins().await;
            }
            Err(e) => toast_err(&self.ui, &format!("切换 {id}"), e),
        }
    }

    async fn plugin_uninstall(&mut self) {
        let Some(id) = self.selected_id() else { return };
        match self.call("plugins/uninstall", json!({ "id": id })).await {
            Ok(_) => {
                log_line(&self.ui, &format!("✓ 已卸载 {id}"));
                self.cache.selected_plugin = None;
                upd(&self.ui, |u| u.set_pd_visible(false));
                self.load_plugins().await;
            }
            Err(e) => toast_err(&self.ui, "卸载插件", e),
        }
    }

    async fn plugin_config_load(&self) {
        let Some(id) = self.selected_id() else { return };
        match self.call("plugins/config/get", json!({ "id": id })).await {
            Ok(cfg) => {
                let text = serde_json::to_string_pretty(&cfg).unwrap_or_default();
                upd(&self.ui, move |u| u.set_pd_config(ss(text)));
            }
            Err(e) => {
                let text = format!("// {e}");
                upd(&self.ui, move |u| u.set_pd_config(ss(text)));
            }
        }
    }

    async fn plugin_config_save(&self) {
        let Some(id) = self.selected_id() else { return };
        let Some(ui) = self.ui.upgrade() else { return };
        let text = ui.get_pd_config().to_string();
        drop(ui);
        let obj: Value = match serde_json::from_str(&text) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(_) => {
                log_line(&self.ui, "✗ 配置必须是 JSON 对象");
                return;
            }
            Err(e) => {
                log_line(&self.ui, &format!("✗ JSON 解析失败: {e}"));
                return;
            }
        };
        for (key, value) in obj.as_object().unwrap() {
            if let Err(e) = self
                .call(
                    "plugins/config/set",
                    json!({ "id": id, "key": key, "value": value }),
                )
                .await
            {
                toast_err(&self.ui, &format!("写入 {key}"), e);
                return;
            }
        }
        log_line(&self.ui, "✓ 插件配置已保存");
    }

    async fn plugin_trigger(&self) {
        let Some(id) = self.selected_id() else { return };
        let Some(ui) = self.ui.upgrade() else { return };
        let action = ui.get_pd_action().to_string();
        let payload = ui.get_pd_payload().to_string();
        drop(ui);
        if action.trim().is_empty() {
            log_line(&self.ui, "✗ 请填写 action");
            return;
        }
        match self
            .call(
                "plugins/trigger",
                json!({
                    "pluginId": id,
                    "action": action,
                    "payload": if payload.is_empty() { Value::Null } else { json!(payload) },
                }),
            )
            .await
        {
            Ok(_) => {
                log_line(&self.ui, &format!("✓ 已触发 ui:{action}"));
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                self.plugin_logs().await;
            }
            Err(e) => toast_err(&self.ui, "触发插件", e),
        }
    }

    async fn plugin_logs(&self) {
        let Some(id) = self.selected_id() else { return };
        match self.call("plugins/logs", json!({ "id": id })).await {
            Ok(lines) => {
                let text = lines
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let text = if text.is_empty() {
                    "（无日志）".into()
                } else {
                    text
                };
                upd(&self.ui, move |u| u.set_pd_log(ss(text)));
            }
            Err(e) => toast_err(&self.ui, "读取插件日志", e),
        }
    }

    // ── settings ────────────────────────────────────────────────────────
    async fn load_settings(&self) {
        let prefs = self
            .call("server/prefs/get", json!({}))
            .await
            .unwrap_or(json!({}));
        let exists = self
            .call("server/prefs/exists", json!({}))
            .await
            .map(|v| v.as_bool().unwrap_or(false))
            .unwrap_or(false);
        let ui_prefs = self
            .call("config/ui/get", json!({}))
            .await
            .unwrap_or(json!({}));
        let theme = self
            .call("config/theme/get", json!({}))
            .await
            .unwrap_or(json!({}));
        let theme_summary = theme
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| format!("{k} {}", v.as_str().unwrap_or("-")))
                    .collect::<Vec<_>>()
                    .join("   ")
            })
            .unwrap_or_default();
        upd(&self.ui, move |u| {
            u.set_sp_port(prefs.get("port").and_then(Value::as_f64).unwrap_or(8554.0) as f32);
            u.set_sp_web_port(
                prefs
                    .get("webPort")
                    .and_then(Value::as_f64)
                    .unwrap_or(8443.0) as f32,
            );
            u.set_sp_mode(ss(jstr(&prefs, "mode")));
            u.set_sp_bind(ss(jstr(&prefs, "bindAddress")));
            u.set_sp_auto_bind(jbool(&prefs, "autoBind"));
            u.set_sp_device(ss(jstr(&prefs, "outputDevice")));
            u.set_sp_mute_sync(jbool(&prefs, "muteSync"));
            u.set_prefs_exists(ss(if exists {
                "server.json 已存在"
            } else {
                "server.json 尚未创建（保存后生成）"
            }));
            u.set_ui_lang(ss(jstr(&ui_prefs, "language")));
            let color = jstr(&ui_prefs, "themeColor");
            u.set_ui_color(ss(if color.is_empty() { "#5b7cfa" } else { &color }));
            u.set_theme_summary(ss(theme_summary));
        });
    }

    async fn prefs_save(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let prefs = json!({
            "port": ui.get_sp_port().round() as u16,
            "webPort": ui.get_sp_web_port().round() as u16,
            "mode": ui.get_sp_mode().to_string(),
            "bindAddress": ui.get_sp_bind().to_string(),
            "autoBind": ui.get_sp_auto_bind(),
            "outputDevice": ui.get_sp_device().to_string(),
            "muteSync": ui.get_sp_mute_sync(),
        });
        drop(ui);
        match self.call("server/prefs/save", prefs).await {
            Ok(_) => log_line(&self.ui, "✓ server.json 已保存"),
            Err(e) => toast_err(&self.ui, "保存 server.json", e),
        }
    }

    async fn ui_save(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let body = json!({
            "language": ui.get_ui_lang().to_string(),
            "themeColor": ui.get_ui_color().to_string(),
        });
        drop(ui);
        match self.call("config/ui/save", body).await {
            Ok(_) => log_line(&self.ui, "✓ ui.json 已保存"),
            Err(e) => toast_err(&self.ui, "保存 ui.json", e),
        }
    }

    // ── system ──────────────────────────────────────────────────────────
    async fn load_system(&self) {
        let info = self.session.info();
        let ver = self
            .call("system/version", json!({}))
            .await
            .unwrap_or(json!({}));
        let mode = self
            .call("mode/status", json!({}))
            .await
            .unwrap_or(json!({}));
        let version_text = format!(
            "后端: {} {}   契约: v{}   平台: {}/{}",
            info.backend,
            ver.get("version")
                .and_then(Value::as_str)
                .unwrap_or(&info.version),
            ver.get("apiVersion")
                .cloned()
                .unwrap_or(json!(info.api_version)),
            info.os,
            info.arch
        );
        let mode_text = format!(
            "模式锁: {}   pid: {}   存活: {}",
            jstr(&mode, "mode"),
            mode.get("pid").cloned().unwrap_or(Value::Null),
            if jbool(&mode, "running") {
                "是"
            } else {
                "否"
            }
        );
        upd(&self.ui, move |u| {
            u.set_sys_version(ss(version_text));
            u.set_sys_mode(ss(mode_text));
        });
        if let Ok(p) = self.call("system/log/path", json!({})).await {
            let path = jstr(&p, "path");
            upd(&self.ui, move |u| {
                u.set_log_path(ss(format!("路径: {path}")))
            });
        }
    }

    async fn update_check(&self) {
        upd(&self.ui, |u| u.set_sys_update(ss("检查更新中…")));
        match self.call("system/update/check", json!({})).await {
            Ok(r) => {
                let text = format!(
                    "有更新: {}   当前: {}   最新: {}   渠道: {}\n{}",
                    if jbool(&r, "hasUpdate") { "✅" } else { "否" },
                    jstr(&r, "currentVersion"),
                    jstr(&r, "latestVersion"),
                    if jbool(&r, "isMirror") {
                        "mirror"
                    } else {
                        "github"
                    },
                    jstr(&r, "releaseUrl")
                );
                upd(&self.ui, move |u| u.set_sys_update(ss(text)));
            }
            Err(e) => {
                let text = format!("✗ {e}");
                upd(&self.ui, move |u| u.set_sys_update(ss(text)));
            }
        }
    }

    async fn log_refresh(&self) {
        match self
            .call("system/log/content", json!({ "maxBytes": 65536 }))
            .await
        {
            Ok(r) => {
                let content = jstr(&r, "content");
                let content = if content.is_empty() {
                    "（空）".into()
                } else {
                    content
                };
                upd(&self.ui, move |u| u.set_log_content(ss(content)));
            }
            Err(e) => toast_err(&self.ui, "读取日志", e),
        }
    }

    // ── server control ──────────────────────────────────────────────────
    async fn start(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let port: u16 = ui.get_q_port().to_string().trim().parse().unwrap_or(18554);
        let mode = ui.get_q_mode().to_string().trim().to_lowercase();
        drop(ui);
        let mode = if ["wifi", "usb", "web"].contains(&mode.as_str()) {
            mode
        } else {
            "wifi".into()
        };
        self.busy(true, "正在启动服务器…").await;
        let result = self.session.start(port, &mode).await;
        self.busy(false, "").await;
        match result {
            Ok(m) => log_line(&self.ui, &format!("✓ {m}")),
            Err(e) => toast_err(&self.ui, "启动服务器", e),
        }
        self.refresh_status().await;
    }

    async fn stop(&self) {
        self.busy(true, "正在停止服务器…").await;
        let result = self.session.stop().await;
        self.busy(false, "").await;
        match result {
            Ok(m) => log_line(&self.ui, &format!("✓ {m}")),
            Err(e) => toast_err(&self.ui, "停止服务器", e),
        }
        self.refresh_status().await;
    }

    async fn nav(&mut self, page: i32) {
        match page {
            0 => self.refresh_status().await,
            1 => self.load_dsp().await,
            2 => self.load_connection().await,
            3 => self.load_devices_page().await,
            4 => self.load_plugins().await,
            5 => self.load_settings().await,
            6 => self.load_system().await,
            _ => {}
        }
    }

    // ── events ──────────────────────────────────────────────────────────
    fn handle_event(&mut self, event: &ServerEvent) {
        match event {
            ServerEvent::AudioLevel { level } => {
                let level = *level;
                let text = format!("电平 {level}%");
                upd(&self.ui, move |u| {
                    u.set_level(level as f32);
                    u.set_level_text(ss(text));
                });
            }
            ServerEvent::MuteStateChanged { muted } => {
                let muted = *muted;
                upd(&self.ui, move |u| u.set_muted(muted));
                log_line(&self.ui, &format!("静音 → {muted}"));
            }
            ServerEvent::MonitoringChanged { enabled } => {
                let enabled = *enabled;
                upd(&self.ui, move |u| u.set_monitoring(enabled));
                log_line(&self.ui, &format!("监听 → {enabled}"));
            }
            ServerEvent::DeviceConnected { device } => {
                log_line(
                    &self.ui,
                    &format!("📱 设备连接: {} ({})", device.name, device.ip),
                );
                upd(&self.ui, |u| u.set_device_connected(true));
            }
            ServerEvent::DeviceDisconnected => {
                log_line(&self.ui, "设备断开");
                upd(&self.ui, |u| {
                    u.set_device_connected(false);
                    u.set_level(0.0);
                });
            }
            ServerEvent::ServerStopped => {
                log_line(&self.ui, "服务器已停止");
                upd(&self.ui, |u| {
                    u.set_phase(ss("stopped"));
                    u.set_running(false);
                    u.set_device_connected(false);
                });
            }
            ServerEvent::AudioMetrics { metrics } => {
                log_line(
                    &self.ui,
                    &format!(
                        "指标 延迟{}ms 网络{}ms 抖动{:.1}ms 丢包{:.2}% 码率{}",
                        metrics.latency_ms,
                        metrics.network_latency_ms,
                        metrics.jitter_ms,
                        metrics.packet_loss_rate,
                        metrics.bitrate
                    ),
                );
            }
            ServerEvent::UdpAudioWarning => {
                log_line(&self.ui, "⚠ 长时间未收到 UDP 音频（防火墙？）");
            }
            ServerEvent::AecStatusChanged { status } => {
                let text = if status.available {
                    if status.enabled {
                        "AEC: 已启用".into()
                    } else {
                        "AEC: 可用（未启用）".into()
                    }
                } else {
                    format!(
                        "AEC: 不可用 ({})",
                        status
                            .reason
                            .map(|r| r.to_string())
                            .unwrap_or_else(|| "?".into())
                    )
                };
                upd(&self.ui, move |u| u.set_dsp_aec_status(ss(text)));
            }
            ServerEvent::InstallProgress { message } => {
                let message = message.clone();
                upd(&self.ui, move |u| {
                    let cur = u.get_vbc_progress().to_string();
                    u.set_vbc_progress(ss(format!("{cur}{message}\n")));
                });
            }
            ServerEvent::PluginListChanged { plugin_id } => {
                log_line(&self.ui, &format!("插件变更: {plugin_id}"));
            }
            ServerEvent::PluginLog {
                plugin_id,
                level,
                message,
            } => {
                let line = format!("插件[{plugin_id}] {level}: {message}");
                let sel_matches = self
                    .selected_id()
                    .map(|id| id == *plugin_id)
                    .unwrap_or(false);
                if sel_matches {
                    let message = message.clone();
                    upd(&self.ui, move |u| {
                        let cur = u.get_pd_log().to_string();
                        u.set_pd_log(ss(format!("{cur}\n{message}")));
                    });
                }
                log_line(&self.ui, &line);
            }
            ServerEvent::UiRequest { request } => {
                log_line(
                    &self.ui,
                    &format!("UI 请求（本前端未实现面板窗口）: {request:?}"),
                );
            }
            ServerEvent::WebClientCount { count } => {
                log_line(&self.ui, &format!("Web 客户端: {count}"));
            }
            ServerEvent::PluginDownloadProgress { .. }
            | ServerEvent::AudioSpectrum { .. }
            | ServerEvent::SpectrumStreamingChanged { .. } => {}
        }
    }
}

struct DspSnapshot {
    gain: f32,
    ns: bool,
    ns_type: i32,
    ns_intensity: f32,
    dereverb: bool,
    dereverb_level: f32,
    agc: bool,
    agc_target: f32,
    agc_attack: f32,
    agc_decay: f32,
    vad: bool,
    vad_threshold: f32,
    aec: bool,
    eq: bool,
    eq_preamp: f32,
    eq_gains: Vec<f32>,
    buffer_ms: f32,
}

// ── wiring ───────────────────────────────────────────────────────────────────

fn wire(ui: &MainWindow, tx: &tokio::sync::mpsc::UnboundedSender<Cmd>) {
    macro_rules! cmd {
        ($cb:ident, $variant:expr) => {{
            let tx = tx.clone();
            ui.$cb(move || {
                let _ = tx.send($variant);
            });
        }};
    }
    let weak = ui.as_weak();

    cmd!(on_start, Cmd::Start);
    cmd!(on_stop, Cmd::Stop);
    cmd!(on_dsp_save, Cmd::DspSave);
    cmd!(on_dsp_reload, Cmd::DspReload);
    cmd!(on_devices_refresh, Cmd::DevicesRefresh);
    cmd!(on_net_refresh, Cmd::NetRefresh);
    cmd!(on_usb_refresh, Cmd::UsbRefresh);
    cmd!(on_usb_enable, Cmd::UsbEnable);
    cmd!(on_firewall, Cmd::Firewall);
    cmd!(on_web_refresh, Cmd::WebRefresh);
    cmd!(on_vbc_check, Cmd::VbcCheck);
    cmd!(on_vbc_install, Cmd::VbcInstall);
    cmd!(on_bh_check, Cmd::BhCheck);
    cmd!(on_bh_set, Cmd::BhSet);
    cmd!(on_bh_restore, Cmd::BhRestore);
    cmd!(on_pw_check, Cmd::PwCheck);
    cmd!(on_plugins_refresh, Cmd::PluginsRefresh);
    cmd!(on_plugin_uninstall, Cmd::PluginUninstall);
    cmd!(on_plugin_config_load, Cmd::PluginConfigLoad);
    cmd!(on_plugin_config_save, Cmd::PluginConfigSave);
    cmd!(on_plugin_trigger, Cmd::PluginTrigger);
    cmd!(on_plugin_logs, Cmd::PluginLogs);
    cmd!(on_plugins_dir, Cmd::PluginsDir);
    cmd!(on_prefs_save, Cmd::PrefsSave);
    cmd!(on_prefs_reload, Cmd::PrefsReload);
    cmd!(on_ui_save, Cmd::UiSave);
    cmd!(on_sys_refresh, Cmd::SysRefresh);
    cmd!(on_update_check, Cmd::UpdateCheck);
    cmd!(on_mode_release, Cmd::ModeRelease);
    cmd!(on_log_refresh, Cmd::LogRefresh);
    cmd!(on_log_export, Cmd::LogExport);

    {
        let tx = tx.clone();
        ui.on_nav(move |p| {
            let _ = tx.send(Cmd::Nav(p));
        });
    }
    {
        let tx = tx.clone();
        ui.on_mute_toggled(move |v| {
            let _ = tx.send(Cmd::Mute(v));
        });
    }
    {
        let tx = tx.clone();
        ui.on_monitoring_toggled(move |v| {
            let _ = tx.send(Cmd::Monitoring(v));
        });
    }
    {
        let tx = tx.clone();
        ui.on_spectrum_toggled(move |v| {
            let _ = tx.send(Cmd::Spectrum(v));
        });
    }
    {
        let tx = tx.clone();
        ui.on_device_selected(move |v| {
            let _ = tx.send(Cmd::DeviceSelected(v));
        });
    }
    {
        let tx = tx.clone();
        ui.on_plugin_select(move |v| {
            let _ = tx.send(Cmd::PluginSelect(v));
        });
    }
    {
        let tx = tx.clone();
        ui.on_plugin_toggle(move |i, v| {
            let _ = tx.send(Cmd::PluginToggle(i, v));
        });
    }
    // EQ band editing happens directly on the model (UI thread, no roundtrip)
    {
        ui.on_eq_set(move |idx, value| {
            if let Some(u) = weak.upgrade() {
                let mut gains: Vec<f32> = u.get_dsp_eq_gains().iter().collect();
                if let Some(slot) = gains.get_mut(idx as usize) {
                    *slot = ((value * 2.0).round() / 2.0).clamp(-12.0, 12.0);
                }
                u.set_dsp_eq_gains(ModelRc::new(VecModel::from(gains)));
                u.set_dsp_dirty(true);
            }
        });
    }
}

fn main() {
    let ui = MainWindow::new().expect("create main window");
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
    wire(&ui, &cmd_tx);

    let ui_weak = ui.as_weak();
    std::thread::Builder::new()
        .name("slint-controller".into())
        .spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            runtime.block_on(controller_task(ui_weak, cmd_rx));
        })
        .expect("spawn controller thread");

    ui.run().expect("run slint event loop");
}

async fn controller_task(
    ui: Weak<MainWindow>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
) {
    let session = match Session::embedded().await {
        Ok(s) => s,
        Err(e) => {
            log_line(&ui, &format!("✗ 后端连接失败: {e}"));
            return;
        }
    };
    let os = session.info().os.clone();
    let mut ctl = Ctl {
        session,
        ui: ui.clone(),
        cache: Cache {
            os,
            ..Default::default()
        },
    };

    // platform flags + identity
    {
        let info = ctl.session.info().clone();
        upd(&ui, move |u| {
            u.set_backend_connected(true);
            u.set_os_windows(info.os == "windows");
            u.set_os_macos(info.os == "macos");
            u.set_os_linux(info.os == "linux");
        });
    }
    log_line(
        &ui,
        &format!(
            "✓ 后端已连接（进程内）: {} {} · api v{} · {}/{}",
            ctl.session.info().backend,
            ctl.session.info().version,
            ctl.session.info().api_version,
            ctl.session.info().os,
            ctl.session.info().arch
        ),
    );
    ctl.refresh_status().await;
    // initial prefs for quick-start defaults
    if let Ok(prefs) = ctl.call("server/prefs/get", json!({})).await {
        let port = prefs.get("port").and_then(Value::as_i64).unwrap_or(8554);
        let web_port = prefs.get("webPort").and_then(Value::as_i64).unwrap_or(8443);
        let mode = jstr(&prefs, "mode");
        upd(&ui, move |u| {
            let p = if mode == "web" { web_port } else { port };
            u.set_q_port(ss(p));
            u.set_q_mode(ss(mode));
        });
    }

    let mut events = ctl.session.events();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(4));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            maybe_cmd = cmd_rx.recv() => {
                let Some(cmd) = maybe_cmd else { break };
                handle_cmd(&mut ctl, cmd).await;
            }
            ev = events.recv() => {
                match ev {
                    Ok(event) => ctl.handle_event(&event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = ticker.tick() => {
                ctl.refresh_status().await;
            }
        }
    }
    ctl.session.shutdown();
}

async fn handle_cmd(ctl: &mut Ctl, cmd: Cmd) {
    match cmd {
        Cmd::Nav(p) => {
            upd(&ctl.ui, move |u| u.set_page(p));
            ctl.nav(p).await;
        }
        Cmd::Start => ctl.start().await,
        Cmd::Stop => ctl.stop().await,
        Cmd::Mute(v) => {
            if let Err(e) = ctl.session.set_muted(v).await {
                toast_err(&ctl.ui, "静音", e);
            }
        }
        Cmd::Monitoring(v) => {
            if let Err(e) = ctl.session.set_monitoring(v).await {
                toast_err(&ctl.ui, "监听", e);
            }
        }
        Cmd::Spectrum(v) => {
            if let Err(e) = ctl
                .call("audio/spectrum/set", json!({ "enabled": v }))
                .await
            {
                toast_err(&ctl.ui, "频谱开关", e);
            }
        }
        Cmd::DspSave => ctl.dsp_save().await,
        Cmd::DspReload | Cmd::DevicesRefresh => ctl.load_dsp().await,
        Cmd::DeviceSelected(i) => ctl.device_selected(i).await,
        Cmd::NetRefresh => ctl.load_connection().await,
        Cmd::UsbRefresh => ctl.load_usb().await,
        Cmd::UsbEnable => ctl.usb_enable().await,
        Cmd::Firewall => match ctl.call("network/firewall/allow", json!({})).await {
            Ok(_) => log_line(&ctl.ui, "✓ 已请求防火墙规则"),
            Err(e) => toast_err(&ctl.ui, "防火墙", e),
        },
        Cmd::WebRefresh => ctl.load_web().await,
        Cmd::VbcCheck => ctl.vbc_check().await,
        Cmd::VbcInstall => ctl.vbc_install().await,
        Cmd::BhCheck => ctl.bh_check().await,
        Cmd::BhSet => ctl.bh_action("devices/blackhole/setInput").await,
        Cmd::BhRestore => ctl.bh_action("devices/blackhole/restore").await,
        Cmd::PwCheck => ctl.pw_check().await,
        Cmd::PluginsRefresh => ctl.load_plugins().await,
        Cmd::PluginSelect(i) => ctl.plugin_select(i).await,
        Cmd::PluginToggle(i, v) => ctl.plugin_toggle(i, v).await,
        Cmd::PluginUninstall => ctl.plugin_uninstall().await,
        Cmd::PluginConfigLoad => ctl.plugin_config_load().await,
        Cmd::PluginConfigSave => ctl.plugin_config_save().await,
        Cmd::PluginTrigger => ctl.plugin_trigger().await,
        Cmd::PluginLogs => ctl.plugin_logs().await,
        Cmd::PluginsDir => match ctl.call("plugins/dir", json!({})).await {
            Ok(r) => log_line(&ctl.ui, &format!("插件目录: {}", jstr(&r, "path"))),
            Err(e) => toast_err(&ctl.ui, "插件目录", e),
        },
        Cmd::PrefsSave => ctl.prefs_save().await,
        Cmd::PrefsReload => ctl.load_settings().await,
        Cmd::UiSave => ctl.ui_save().await,
        Cmd::SysRefresh => ctl.load_system().await,
        Cmd::UpdateCheck => ctl.update_check().await,
        Cmd::ModeRelease => match ctl.call("mode/releaseLock", json!({})).await {
            Ok(_) => {
                log_line(&ctl.ui, "✓ 模式锁已释放（若为本进程持有）");
                ctl.load_system().await;
            }
            Err(e) => toast_err(&ctl.ui, "释放模式锁", e),
        },
        Cmd::LogRefresh => ctl.log_refresh().await,
        Cmd::LogExport => match ctl.call("system/log/export", json!({})).await {
            Ok(r) => log_line(&ctl.ui, &format!("✓ 日志已导出: {}", jstr(&r, "path"))),
            Err(e) => toast_err(&ctl.ui, "导出日志", e),
        },
    }
}
