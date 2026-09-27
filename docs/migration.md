# 前端迁移指南：从进程内 invoke 到 libmicyou 后端

本指南面向原 [MicYou](https://github.com/LanRhyme/MicYou) 仓库的桌面端改造：
Tauri GUI / CLI / TUI 如何从「链接 `tauri_app_lib` 进程内调用」切换到
「libmicyou 守护进程 + JSON-RPC」。

## 1. 目标形态

```
┌────────────── Tauri 应用（纯前端）──────────────┐
│  Vue 3 UI · 窗口/托盘/主题/快捷键 · 面板渲染     │
│  Rust 侧: micyou-client（sidecar stdio 客户端）  │
└──────────────┬──────────────────────────────────┘
               │ spawn + stdin/stdout（JSON-RPC 行协议）
┌──────────────▼──────────────────────────────────┐
│  micyou-daemon（sidecar 二进制，随应用打包）       │
│  音频服务器 · DSP · 插件宿主 · 虚拟设备            │
└─────────────────────────────────────────────────┘
```

- GUI 不再包含任何服务器逻辑；`tauri-app/crates/*` 与 `src-tauri` 中的
  server/transport/plugin 代码由 libmicyou 取代。
- CLI/TUI 直接依赖 `micyou-client`（连 daemon 的 stdio/ws，或进程内
  `libmicyou::Builder` 嵌入），删除对 `micyou-app` 的依赖。

## 2. Tauri 集成步骤

1. **打包 sidecar**：将 `micyou-daemon` 二进制放入 `src-tauri/binaries/`
   （`tauri.conf.json > bundle > externalBin`），命名带 target-triple 后缀。
2. **Rust 侧客户端**（`src-tauri` 仅保留薄壳）：

   ```rust
   use micyou_client::Client;
   use tauri::async_runtime;

   #[tauri::command]
   async fn start_server(state: tauri::State<'_, AppState>, port: u16, mode: String)
       -> Result<String, String> {
       let client = state.client.lock().await;
       let r = client.start_server(micyou_api::methods::StartServerParams {
           port: Some(port), mode: Some(mode), ..Default::default()
       }).await.map_err(|e| e.to_string())?;
       Ok(r.message)
   }
   ```

3. **事件转译**：一个后台任务消费 `client.events`，把 `ServerEvent`
   映射为原有 `app.emit` 事件名（对照表见 §4），前端 Vue 代码几乎不用改。

   ```rust
   tokio::spawn(async move {
       while let Ok(ev) = client.events.recv().await {
           match &*ev {
               ServerEvent::AudioLevel { level } =>
                   { let _ = app.emit("audio-level", level); }
               ServerEvent::DeviceConnected { device } =>
                   { let _ = app.emit("device-connected", device); }
               ServerEvent::UiRequest { request } => handle_ui(&app, request),
               _ => {}
           }
       }
   });
   ```

4. **UI 委托**：hello 时声明 `ui: true`；处理 `uiRequest.openPluginPanel`
   → 用原 `open_plugin_window_impl` 逻辑开窗；面板 HTML 经
   `plugins/panel` 拉取（与原 `get_plugin_panel` 一致）。
5. **托盘/窗口/主题**：留在前端（原本就是 UI 关注点）。托盘动作调用
   client 的 `set_muted`/`start_server` 等方法。
6. **退出流程**：窗口全关 → `client.stop_server()`（可选）→ drop client
   （自动 kill sidecar 子进程）。

## 3. 启动时序

```
app boot → spawn daemon(--stdio) → session/hello{name,ui:true}
        → session/subscribe{events:["audio","device","server","plugin"]}
        → server/status（恢复 UI 状态）→ audio/settings/get、plugins/list …
        → （用户点击开始）server/start
```

daemon 在启动时即预热虚拟麦克风设备（等效原 GUI 的启动即建设备行为）。

## 4. 事件名对照（原 Tauri event → ServerEvent tag）

| 原事件 | 新 tag | data |
|---|---|---|
| `device-connected` | `deviceConnected` | `{device}` |
| `device-disconnected` | `deviceDisconnected` | — |
| `audio-metrics` | `audioMetrics` | `{metrics}`（字段即原 AudioMetrics） |
| `audio-level` | `audioLevel` | `{level}`（原为裸 u32） |
| `audio-spectrum` | `audioSpectrum` | `{raw,processed}` |
| `mute-state-changed` | `muteStateChanged` | `{muted}`（原为裸 bool） |
| `monitoring-enabled-changed` | `monitoringChanged` | `{enabled}` |
| `server-stopped` | `serverStopped` | — |
| `web-client-count` | `webClientCount` | `{count}` |
| `udp_audio_warning` | `udpAudioWarning` | — |
| `aec-status-changed` | `aecStatusChanged` | `{status}` |
| `vbcable-install-progress` | `installProgress` | `{message}` |
| `plugin-download-progress` | `pluginDownloadProgress` | `{id,downloaded,total,done}` |
| —（新） | `uiRequest` | 插件开窗委托 |
| —（新） | `pluginListChanged` / `pluginLog` | 插件面刷新 |

方法名对照见 [`micyou_api::methods`](../crates/micyou-api/src/methods.rs)
模块文档中的完整映射表。

## 5. 配置与共存

- daemon 读写同一 `~/.config/micyou/`（settings/server/ui/theme.json），
  迁移期新旧版本可共存互认。
- `mode.lock` 新增 `daemon` 模式：daemon 与旧版 GUI/CLI/TUI 互斥，
  避免双实例抢占音频设备与端口。
- 插件目录（`<config>/plugins/`）、清单、能力与总线协议不变；
  v1/v2 插件在 v3 宿主上原样运行。声明 `apiVersion: 3` +
  `host.call`/`host.admin`/`host.events` 的插件获得完整后端访问
  （见 [plugin-api-v3.md](./plugin-api-v3.md)）。

## 6. 资源与模型

- AI 降噪（PureVox6）与回声消除（AEC7）模型已静态编译进二进制
  （纯 Rust 推理 VM，见 [pure-rust-inference.md](./pure-rust-inference.md)），
  不再需要 `onnxruntime.{dll,dylib,so}`，也不存在模型文件缺失的降级路径。
- resources 目录只保留 Linux PipeWire 的 ALSA 配置模板：放 sidecar 同级的
  `resources/`（自动发现），或用 `--resources <dir>` / `$MICYOU_RESOURCE_DIR`
  显式指定。旧版打包树中残留的 `.onnx` 文件仍可被识别（兼容 marker），
  但运行时不再读取。
