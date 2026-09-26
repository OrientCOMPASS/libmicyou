# libmicyou 架构

> 前后端分离的 MicYou 桌面后端。本文描述模块拓扑、数据流与关键设计决策。
> 路线图与背景见 [PLAN.md](../PLAN.md)。

## 1. 总览

```
┌──────────────────────────── 前端（独立进程/独立技术栈）────────────────────────────┐
│   Tauri GUI (sidecar)        CLI         TUI        Web 页面      第三方工具        │
│        │                      │           │            │              │            │
│        └──── micyou-client SDK 或任意 JSON-RPC 2.0 实现 ────────────────┘            │
└───────────────┬───────────────────┬────────────────────┬───────────────────────────┘
                │ stdio(管道)        │ WebSocket(/rpc)     │ 进程内通道(Rust 宿主)
┌───────────────▼───────────────────▼────────────────────▼───────────────────────────┐
│ micyou-daemon（守护进程） / libmicyou（门面 crate：Builder + Managed）                │
├────────────────────────────────────────────────────────────────────────────────────┤
│ micyou-rpc    JSON-RPC 路由 · 会话与订阅过滤 · 事件泵 · stdio/ws/local 传输 ·        │
│               RpcHostBridge（插件 call_host 桥）· RpcUiBridge（UI 请求委托）          │
│ micyou-api    契约：方法目录(methods) · 事件目录(events) · 配置模式(config) ·        │
│               JSON-RPC 信封(jsonrpc) · 错误码(error) · 插件方法访问分级               │
├────────────────────────────────────────────────────────────────────────────────────┤
│ micyou-core   ServerCore 生命周期事务 · AudioPipeline 音频管线 · EventBus ·          │
│               Backend 服务门面 · 插件宿主(无头) · 配置 · 平台虚拟设备 · 日志           │
├──────────────┬─────────────────────┬─────────────────────┬─────────────────────────┤
│ micyou-      │ micyou-audio        │ micyou-plugin       │ micyou-protocol         │
│ transport    │ cpal 输出引擎        │ native(cdylib)+WASM │ protobuf 线路协议        │
│ TCP/UDP/Web  │ DSP 链(AEC/NS/EQ/   │ 双运行时 · 总线 ·    │ (与 Android 客户端       │
│ mDNS/ADB     │ AGC/VAD) · 环回     │ 清单/能力 · DSP 节点 │  字节级兼容)             │
│ 抖动缓冲+FEC  │ 重采样 · Opus 解码   │ HostApi v3          │                         │
└──────────────┴─────────────────────┴─────────────────────┴─────────────────────────┘
                     依赖方向严格单向：上层依赖下层，反向仅通过 trait
```

## 2. 模块职责与边界

| crate | 功能域 | 关键类型 | 明确不做的事 |
|---|---|---|---|
| `micyou-protocol` | 手机端线路协议 | `micyou::*`(prost)、`AudioFormat`/`Codec` | 不含任何 I/O |
| `micyou-transport` | **网络** | `start_tcp_server`/`start_udp_server`/`WebServer`/`NetworkManager`(mDNS)/`adb`/`JitterBuffer`/`NetworkStats`/`TransportEvents` | 不认识插件框架、配置、前端 |
| `micyou-audio` | **音频处理** | `AudioOutputManager`/`DspProcessor`/`LoopbackCapture`/`SoundMixer`/`RubatoResampler`/`opus::Decoder` | 不认识网络与会话 |
| `micyou-plugin` | **插件框架** | `PluginManager`/`PluginBus`/`HostApi`(v3)/`PluginDspRegistry`/native+wasm 运行时 | 不含宿主实现（宿主接线在 core） |
| `micyou-core` | 编排/宿主 | `ServerCore`/`Backend`/`AudioPipeline`/`EventBus`/`PluginHost`/`UiBridge`/config/platform/logging | 不含任何 GUI；窗口请求委托 `UiBridge` |
| `micyou-api` | **前后端契约（UI 模块的后端侧）** | `methods::*`/`ServerEvent`/`jsonrpc::*`/`config::*`/`method_access` | 无业务逻辑，仅 serde 模式与常量 |
| `micyou-rpc` | 传输与路由 | `RpcService`/`SessionRegistry`/`serve_stdio`/`serve_ws`/`local::attach`/`RpcHostBridge` | 不直接触碰 core 内部状态（只经 `Backend`） |
| `micyou-client` | 前端 SDK | `Client`(stdio/ws)/`ClientError` | — |
| `micyou-daemon` | 部署入口 | clap CLI | — |
| `libmicyou` | 门面 | `Builder`/`Managed` | — |

解耦手段（trait 边界）：

- `transport → core`：`TransportEvents`（设备连接/断开、指标、静音同步、web 客户端数、
  插件消息透传、控制通道交接）。core 的 `CoreTransportBridge` 实现它并扇出到
  EventBus + 插件框架。
- `core → 前端`：`EventBus`（tokio broadcast）→ rpc 事件泵 → 按会话订阅过滤 → JSON-RPC notification。
- `插件 → 后端`：`HostRpc`（rpc 层安装 `RpcHostBridge`）→ 能力门控 → `RpcService::dispatch`。
- `后端 → 前端 UI`：`UiBridge`（rpc 层安装 `RpcUiBridge`）→ `uiRequest` 事件。
- 平台虚拟设备（VB-CABLE/BlackHole/PipeWire）：纯进程调用，安装进度经回调→事件总线。

## 3. 关键数据流

### 3.1 音频（手机 → 虚拟麦克风）

```
Android ── UDP(MicU, port+1) ─▶ udp server ─▶ 会话绑定校验 ─▶ mpsc(128) AudioStreamEvent
      └─ TCP(MicY, 8554) ─▶ tcp server ─▶ 控制面(连接/静音/ping/插件消息) + TCP-only 音频
                                                     │
AudioPipeline 线程:  JitterBuffer(重排+XOR FEC 恢复) → Opus/PCM 解码 → 48k 重采样
   → DSP 链(AEC→NS→Dereverb→EQ→Gain→AGC→VAD + Plugin:<id> 节点) → AudioOutputHandle(cpal ringbuf)
   → 虚拟麦克风(VB-CABLE / BlackHole / PipeWire) ；旁路：电平/频谱/指标 → EventBus
```

### 3.2 控制（前端 → 后端）

```
前端 ── JSON-RPC request ─▶ 传输(stdio/ws/local) ─▶ RpcService::dispatch ─▶ Backend 方法
     ◀── response(result|error) ────────────────────────────────────────────┘
     ◀── notification(event) ◀─ 事件泵 ◀─ EventBus ◀─ core/transport/pipeline
```

### 3.3 插件解放（HostApi v3）

```
插件 ── call_host("audio/mute/set", "{\"muted\":true}") ─▶ PluginHostApi
   → HostRpc(RpcHostBridge) → method_access 分级 + 能力检查(host.call/host.admin)
   → RpcService::dispatch（与前端完全同一代码路径） → JSON 信封返回
插件 ── subscribe_host_events("audio") ─▶ PluginEventSubs
   → core 事件泵匹配 ServerEvent.tag() → bus 消息 topic "host:event"
```

## 4. 生命周期与并发

- `ServerLifecycleGate` 串行化完整 start/stop 事务；音频线程单独跟踪，
  stop 时限 3s join，残留在 `StoppingResidual` 相态并阻止 restart（防双线程驱动同一设备）。
- 输出设备**进程级常驻**（daemon 启动即预热虚拟麦克风），server stop 不拆设备。
- 硬静音是 `AtomicBool`：cpal 回调直接观察，一个回调周期内生效（丢弃排队音频）。
- `mode.lock`（`gui|cli|tui|daemon` + pid 活性检查）保证同一时刻仅一个后端实例。
- 传输任务用 `CancellationToken` + 有界 join(3s) + abort 兜底。

## 5. 配置与兼容性

- 配置目录：`~/.config/micyou/`（Linux）/`%APPDATA%\micyou`（Windows）/`~/Library/...`；
  可用 `set_config_dir()`、`$MICYOU_CONFIG_DIR` 或 `--config-dir` 覆盖（测试/多实例）。
- `settings.json`（DSP）、`server.json`（端口/模式/绑定/输出设备/muteSync）、
  `ui.json`、`theme.json` 与上游 GUI/CLI/TUI **字节兼容**，迁移期可共享目录互操作。
- 手机线路协议零改动：现有 Android 客户端直接可连。

## 6. 安全边界（当前状态与计划）

- WebSocket 传输默认应绑定回环地址；**尚无鉴权**——暴露到局域网前需加 token
  （计划：`--ws-token`，hello 握手校验）。已在 rpc-api.md 标注。
- 插件：WASM 在 wasmi 沙箱内解释执行；native cdylib 拥有进程权限，靠清单能力
  声明 + 用户确认（前端预览 caps）约束。文件访问限于插件目录（sandbox_path）。
- `call_host` 双重门控：清单能力（host.call/host.admin/host.events）×
  方法分级（Denied/Call/Admin）。会话管理方法对插件拒绝。
