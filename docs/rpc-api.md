# libmicyou RPC API 参考

JSON-RPC 2.0 前后端契约。规范源：[`micyou-api`](../crates/micyou-api)（方法名常量与
DTO 的唯一定义处）。本文与 `micyou_api::methods` 保持同步。

## 传输

| 传输 | 端点 | 用途 |
|---|---|---|
| stdio | 每行一条 JSON（`micyou-daemon --stdio`） | GUI sidecar（Tauri 子进程） |
| WebSocket | `ws://<addr>/rpc`（文本帧） | 浏览器/远程/多前端；`GET /health` 探活 |
| 进程内 | `Managed::attach_local()` / `micyou_rpc::local::attach` | 嵌入式 Rust 宿主（CLI/TUI） |

三种传输消息格式完全一致，前端可无缝切换。

## 握手与会话

连接后建议依次调用：

```jsonc
// 1. 身份与能力（ui=true 的会话才会收到 uiRequest 事件）
{"jsonrpc":"2.0","id":1,"method":"session/hello","params":{"name":"tauri-gui","ui":true}}
// → {"backend":"libmicyou","version":"0.1.0","apiVersion":1,"os":"linux","arch":"x86_64"}

// 2. 订阅高频事件（audioLevel≈8Hz / audioMetrics 1Hz / audioSpectrum）
{"jsonrpc":"2.0","id":2,"method":"session/subscribe","params":{"events":["audio","*"]}}
```

- 过滤器：`"*"` 或事件 tag 前缀（`"audio"` → audioLevel/audioMetrics/audioSpectrum；
  `"device"` → deviceConnected/deviceDisconnected）。
- **低频事件默认推送给所有会话**；高频事件仅推给订阅者（节省 stdio 带宽）。
- `session/unsubscribe` 移除过滤器。

## 事件（server → client notification）

```jsonc
{"jsonrpc":"2.0","method":"event","params":{"type":"<tag>","data":{...}}}
```

| tag | data | 频率 |
|---|---|---|
| `deviceConnected` | `{device:{name,ip,latency}}` | 会话开始 |
| `deviceDisconnected` | — | 会话结束 |
| `audioMetrics` | `{bitrate,sampleRate,latencyMs,networkLatencyMs,packetLossRate,jitterMs,bufferDurationMs}` | 1 Hz（订阅） |
| `audioLevel` | `{level:0..100}` | ≈8 Hz（订阅） |
| `audioSpectrum` | `{raw:[f32],processed:[f32]}` | 随电平（订阅 + `audio/spectrum/set`） |
| `muteStateChanged` | `{muted}` | 变化时 |
| `monitoringChanged` | `{enabled}` | 变化时 |
| `spectrumStreamingChanged` | `{enabled}` | 变化时 |
| `serverStopped` | — | 停止后 |
| `webClientCount` | `{count}` | web 模式客户端变化 |
| `udpAudioWarning` | — | Windows wifi 模式 >10s 无 UDP 音频 |
| `aecStatusChanged` | `{status:{available,enabled,reason?}}` | AEC 可用性变化 |
| `installProgress` | `{message}` | VB-CABLE 安装进度 |
| `uiRequest` | `{request:{kind:"openPluginPanel",pluginId,panelId}}` | 插件请求开窗（仅 ui 会话应处理） |
| `pluginListChanged` | `{pluginId}` | 插件安装/卸载/启停 |
| `pluginLog` | `{pluginId,level,message}` | 插件日志 |
| `pluginDownloadProgress` | `{id,downloaded,total,done}` | 市场下载进度 |

## 方法目录

所有方法 params 均为 by-name JSON 对象（camelCase）。错误：标准 JSON-RPC 码 +
应用码（`-32000` 通用失败、`-32001` 状态错误、`-32002` 能力不足、`-32003` 无 UI 前端、
`-32004` 未找到、`-32005` 超时）。

### server — 生命周期

| 方法 | params | result | 说明 |
|---|---|---|---|
| `server/start` | `{port?,mode?,bindAddress?,outputDevice?,usbDeviceSerial?}` | `{message}` | 缺省项取 server.json；mode: `wifi\|usb\|web`；usb 模式自动 `adb reverse` |
| `server/stop` | — | `{message}` | 未运行时返回错误 |
| `server/status` | — | `{phase,isServerRunning,isConnected,isMuted,isMonitoring,mode?,port?}` | phase: `stopped\|starting\|running\|stopping\|stoppingResidual` |
| `server/prefs/get` | — | `ServerPrefs` | `{port,webPort,mode,bindAddress,autoBind,outputDevice,muteSync}` |
| `server/prefs/save` | `ServerPrefs` | `{message}` | 立即生效（muteSync 热更新） |
| `server/prefs/exists` | — | `bool` | 前端迁移提示 |

### audio — 音频与 DSP

| 方法 | params | result |
|---|---|---|
| `audio/devices` | — | `string[]`（系统输出设备名） |
| `audio/settings/get` | — | `AudioDspSettings`（settings.json 优先） |
| `audio/settings/update` | `{settings}` | `{message}`；AEC 固定链首；macOS 拒绝开启 AEC |
| `audio/mute/set` | `{muted}` | `{ok}`；触发 `muteStateChanged`，muteSync 时推送手机端 |
| `audio/mute/get` | — | `{muted}` |
| `audio/monitoring/set` | `{enabled}` | `{ok}`（耳返） |
| `audio/spectrum/set` | `{enabled}` | `{ok}`（频谱流开关） |

### network / usb / web

| 方法 | params | result |
|---|---|---|
| `network/info` | — | `{ips[],port}`（QR 展示用） |
| `network/interfaces` | — | `[{ip,interfaceName}]`（按可达性打分排序） |
| `network/firewall/allow` | — | `{ok}`（Windows UAC；其余平台 no-op） |
| `usb/enable` | `{port?,deviceSerial?}` | `UsbModeResult`（`success\|noDevices\|multipleDevices{devices}`） |
| `usb/devices` | — | `[{serial,name}]` |
| `web/status` | — | `{running,clientCount}` |

### devices — 虚拟音频设备

| 方法 | params | result |
|---|---|---|
| `devices/vbcable/check` | — | `{installed}`（Windows） |
| `devices/vbcable/install` | — | `{success,errorType?,message?}`；进度经 `installProgress` 事件 |
| `devices/blackhole/check` | — | BlackHoleStatus（macOS） |
| `devices/blackhole/setInput` / `restore` | — | BlackHoleResult |
| `devices/pipewire/check` | — | `{available,setup,deviceExists,installCommand,distro}`（Linux） |

### config — 共享偏好

`config/ui/get|save`（`{language,themeColor}`）、`config/theme/get|save`
（Material3 色板，GUI 导出给其它前端）。

### plugins

| 方法 | params | result |
|---|---|---|
| `plugins/list` | — | `PluginView[]`（含 i18n、依赖、configSchema、loadError；失败插件懒重载） |
| `plugins/setEnabled` | `{id,enabled}` | `{ok}` + `pluginListChanged` |
| `plugins/uninstall` | `{id}` | `{ok}` |
| `plugins/config/get` | `{id}` | 任意 JSON |
| `plugins/config/set` | `{id,key,value}` | `{ok}`（触发插件 `config:changed` 消息） |
| `plugins/logs` | `{id}` | `string[]`（环形 500 行） |
| `plugins/syncStatus` | — | `{deviceConnected,transportReady}` |
| `plugins/dir` | — | `{path}`（后端尽力 open，前端可自行打开） |
| `plugins/preview/zip` | `{path}` | `PluginPreview`（安装前权限确认） |
| `plugins/preview/url` | `{manifestUrl}` | `PluginPreview` |
| `plugins/install/url` | `{id,zipUrl}` | `{id}`；断点续传；进度经 `pluginDownloadProgress`；`plugins/install/cancel {id}` 取消 |
| `plugins/update/check` | — | `PluginUpdate[]` |
| `plugins/update/apply` | `{id}` | `{version}` |
| `plugins/import` | `{source}` | `{id}`（本地目录或 zip） |
| `plugins/trigger` | `{pluginId,action,payload?}` | `{ok}`（`ui:<action>` 总线消息） |
| `plugins/panel` | `{pluginId,panelId}` | `{html,title}` |
| `plugins/panelIcons` | `{id}` | `{panelId:icon}` |
| `plugins/window/open` | `{pluginId,panelId}` | `{ok}`；无 UI 会话时 `-32003` |

### mode / system

| 方法 | params | result |
|---|---|---|
| `mode/status` | — | `{mode:"gui\|cli\|tui\|daemon\|none",pid?,running}` |
| `mode/releaseLock` | — | `{ok}`（仅本进程持有时） |
| `system/version` | — | `{version,apiVersion}` |
| `system/log/path` | — | `{path}` |
| `system/log/content` | `{maxBytes?}` | `{content}`（默认尾部 256KiB） |
| `system/log/export` | — | `{path}` |
| `system/update/check` | `{cdk?}` | `{hasUpdate,currentVersion,latestVersion,releaseUrl,releaseNotes?,isMirror,cdkExpiredTime?}` |
| `system/sponsors` | `{apiToken?,userId?}` | `{raw}`（Afdian JSON；凭据缺省取环境变量） |
| `system/locale` | — | `{language}` |

## 与上游 Tauri command 的映射

见 `micyou_api::methods` 模块文档表（1:1 映射，除窗口/托盘/主题 CSS 管理留在前端）。

## 安全注意事项

- **当前无鉴权**：任何能连到传输的进程都可调用全部方法（含插件安装）。
  WebSocket 请绑定回环；跨机暴露前必须加 token（计划 `--ws-token` + hello 校验）。
- stdio 传输天然由父进程独占，风险面等同于上游进程内 invoke。
- 插件 `call_host` 受能力门控（见 [plugin-api-v3.md](./plugin-api-v3.md)），
  与前端会话是两套独立权限路径。
