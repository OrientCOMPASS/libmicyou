# libmicyou 重构实现计划

> 目标：将 [MicYou](https://github.com/LanRhyme/MicYou) 桌面端后端从 Tauri 应用中剥离，
> 重构为独立的、前后端分离的后端库 + 守护进程（`libmicyou`）。
> 任何前端（Tauri GUI / CLI / TUI / Web）都通过统一的 RPC 契约驱动后端。

## 一、现状分析（原实现）

原桌面端 = `tauri-app/src-tauri`（12.4k 行，后端逻辑与 Tauri 深度耦合）+ 3 个已解耦 crate：

| 原模块 | 内容 | Tauri 耦合度 |
|---|---|---|
| `micyou-protocol` | protobuf 线路协议（与手机端兼容） | 无 ✅ |
| `micyou-audio` | cpal 输出引擎、DSP 链（AEC/NS/EQ/AGC/VAD）、环回采集、混音器 | 无 ✅ |
| `micyou-plugin` | 插件框架（native cdylib + WASM 双运行时）、总线、清单、DSP 节点注册 | 无 ✅ |
| `src-tauri/src/*` | TCP/UDP/Web 服务器、抖动缓冲、音频管线、生命周期、配置、平台虚拟设备、插件宿主接线、~40 个 Tauri command | 高 ❌ |

主要问题：
1. 后端核心（`start_server_inner` 等）住在 Tauri 应用 crate 里，CLI/TUI 被迫链接整个 Tauri 依赖树；
2. `start_server_inner` 是 ~760 行的巨型函数，音频线程、插件控制面接线、传输层启动全部内联，
   状态靠 20+ 个 `Arc` 手工克隆传递；
3. 插件 `HostApi` 的宿主实现（`PluginHostApi`）直接持有服务器内部状态，且热键/窗口服务绑定 Tauri；
4. 前端通过 `invoke`/`listen` 进程内直连后端，没有稳定的序列化契约，前后端无法独立部署/升级。

## 二、新架构

### 2.1 crate 拓扑（按功能模块化）

```
                 ┌────────────────────────────── 前端（原仓库，后续改造）───────────────────────────┐
                 │  Tauri GUI (sidecar/子进程)      CLI        TUI        Web/第三方 (WebSocket)     │
                 └──────────────┬────────────────────┬───────────┬──────────────┬──────────────────┘
                                │      micyou-client SDK（JSON-RPC 2.0 over stdio / WebSocket / 进程内）│
                 ┌──────────────▼────────────────────▼───────────▼──────────────▼──────────────────┐
                 │ micyou-daemon（守护进程二进制）  ┃  libmicyou（门面 crate：Backend 构建器）        │
                 ├───────────────────────────────────────────────────────────────────────────────────┤
                 │ micyou-rpc   JSON-RPC 路由 / 会话 / 事件订阅 / 传输(stdio·ws·进程内)              │
                 │ micyou-api   前后端契约：命令与事件的 serde 模式（"UI 模块"的后端侧）              │
                 ├───────────────────────────────────────────────────────────────────────────────────┤
                 │ micyou-core  编排：ServerCore 生命周期 / 音频管线 / 配置 / 统计 /                 │
                 │              平台虚拟设备(VB-Cable·BlackHole·PipeWire) / 插件宿主 / 日志           │
                 ├────────────┬──────────────┬──────────────┬────────────────────────────────────────┤
                 │ micyou-    │ micyou-audio │ micyou-      │ micyou-protocol                        │
                 │ transport  │ (引擎+DSP)   │ plugin       │ (protobuf 线路协议)                    │
                 │ 网络模块：  │              │ 插件框架：    │                                        │
                 │ TCP/UDP/   │              │ native+WASM  │                                        │
                 │ Web(TLS)/  │              │ 双运行时、    │                                        │
                 │ mDNS/ADB/  │              │ 总线、清单    │                                        │
                 │ 抖动缓冲    │              │              │                                        │
                 └────────────┴──────────────┴──────────────┴────────────────────────────────────────┘
```

依赖方向严格单向：`daemon/libmicyou → rpc → api → core → {transport, audio, plugin} → protocol`。

### 2.2 各 crate 职责

| crate | 职责 | 来源 |
|---|---|---|
| `micyou-protocol` | 与手机端的 protobuf 线路协议、魔数、端口常量（**字节级兼容，不可破坏**） | 移植 |
| `micyou-audio` | cpal 输出引擎、重采样、DSP 链（AEC→NS→Dereverb→EQ→AGC→VAD，纯 Rust 推理 VM/RNNoise）、环回采集、音效混音 | 移植 |
| `micyou-plugin` | 插件框架：清单/能力、native(cdylib C-ABI) + WASM(wasmi) 运行时、消息总线、DSP 节点注册、跨设备同步协议；**HostApi v2** | 移植+扩展 |
| `micyou-transport` | 网络模块：TCP 控制面(8554)/UDP 音频面(port+1)/Web 模式(axum TLS WS)/mDNS 广播/ADB 转发；抖动缓冲+FEC；Opus 解码；会话状态与网络统计 | 从 src-tauri 抽取重构 |
| `micyou-core` | 编排模块：`ServerCore`（生命周期状态机、启动/停止事务）、音频管线（原 start_server_inner 内联线程抽为 `AudioPipeline`）、共享配置(settings/server/ui/theme.json)、平台虚拟设备、插件宿主接线（无头热键、UI 请求委托）、守护进程日志 | 重构 |
| `micyou-api` | 前后端契约：全部 RPC 方法参数/返回、事件的 serde 类型；事件目录与订阅过滤规则；UI 委托请求（打开插件面板窗口等） | 新写 |
| `micyou-rpc` | JSON-RPC 2.0：方法路由表、会话管理、事件广播/订阅、stdio 与 WebSocket 传输、进程内通道 | 新写 |
| `micyou-client` | Rust 客户端 SDK：类型化方法 + 事件流，供 CLI/TUI/Tauri(sidecar) 使用 | 新写 |
| `micyou-daemon` | 无头后端二进制：clap 参数（--stdio / --ws-port / --config-dir / --log-level），信号处理，模式锁 | 新写 |
| `libmicyou` | 门面 crate：`Backend` 构建器（可嵌入任何 Rust 宿主），re-export 公共类型 | 新写 |

### 2.3 关键设计决策

1. **传输层解耦**：`micyou-transport` 不依赖 core/audio/plugin。
   - 音频包经 `mpsc<AudioStreamEvent>` 上抛；
   - 控制面事件经 `TransportEvents` trait（core 实现，桥接到 RPC 事件总线）；
   - 插件消息中继经 `PluginRelay` trait（core 接线到插件总线）。
2. **音频管线对象化**：原 760 行内联闭包 → `AudioPipeline` 结构体，
   持有 JitterBuffer/Opus 解码器/重采样器/DSP/环回，`run(rx)` 循环独立可测。
3. **事件总线**：core 内部 `EventBus`（tokio broadcast），`ServerEvents` 兼容层保留；
   RPC 层订阅总线 → 按会话订阅过滤 → JSON-RPC notification 推送。
4. **UI 委托**：后端不拥有窗口。插件 `open_window(panel)`、面板渲染数据等
   通过 `ui/*` 事件发给已连接前端；前端可通过 `UiBridge` 回执。
   热键用 `global-hotkey`（无头事件泵），通知用 `notify-rust`，剪贴板用 `arboard` —— 全部无 GUI 依赖。
5. **插件能力解放（HostApi v2）**：
   - 新增 `call_host(method, params_json)`：能力门控的通用桥，直通 `micyou-api` 全部方法
     （能力映射：`host.server.*`、`host.audio.*`、`host.plugins.*`、`host.net.*`…）；
   - 新增 `subscribe_host_events(filter)`：核心事件（设备连接、指标、静音变化…）投递到插件消息主题；
   - 保留 v1 全部方法（配置/文件/定时器/HTTP/热键/声音/DSP 设置…），ABI 版本升至 2，向后兼容 v1 插件。
6. **配置兼容**：沿用 `~/.config/micyou/{settings,server,ui,theme}.json`（camelCase serde），
   与原 GUI/CLI/TUI 三端共享同一目录，迁移期可互操作。
7. **线路协议零改动**：TCP 魔数 `MicY`、UDP 魔数 `MicU`、protobuf 结构、FEC 组、Opus 封装全部保持，
   现网 Android 客户端无需更新即可连接新后端。

### 2.4 RPC 契约草案（JSON-RPC 2.0）

- 请求：`{"jsonrpc":"2.0","id":1,"method":"server/start","params":{...}}`
- 事件：`{"jsonrpc":"2.0","method":"event","params":{"type":"audio/level","data":...}}`
- 方法命名空间：`server/* audio/* config/* devices/* network/* usb/* plugins/* mode/* system/* ui/*`
- 会话建立后先发 `session/hello`（版本、能力协商），高频事件（电平/频谱/指标）需显式 `session/subscribe`。

方法与原 40 个 Tauri command 的映射表见 `docs/rpc-api.md`（阶段 5 产出）。

## 三、实施阶段（每阶段推送后由 GitHub Actions 验证）

| 阶段 | 内容 | 状态 |
|---|---|---|
| **A** | 仓库骨架：workspace、全部 crate 存根、CI(fmt/clippy/test × 3 OS)、Cargo.lock | ✅ CI 三平台绿（bba9c18） |
| **B** | 移植三个无 Tauri 依赖 crate：protocol → audio → plugin（含原有单测） | ✅ 编译通过（测试路径修复后随 C 验证） |
| **C** | 新建 `micyou-transport`：jitter/stream/stats/tcp/udp/mdns/adb/web + `TransportEvents` 解耦 trait + 单测 | ✅ 编译+测试通过 |
| **D** | 新建 `micyou-core`：config/mode_lock/平台设备/audio_output/sound/AudioPipeline/PluginHost(无头)/ServerCore/日志 + 单测 | ✅ 编译通过 |
| **E** | `micyou-api` + `micyou-rpc` + `micyou-daemon` + `libmicyou` 门面：契约类型、路由器、stdio/ws/local 传输、守护进程、CI 冒烟测试 | ✅ 已实现，CI 验证中 |
| **F** | `micyou-client` SDK + HostApi v3（call_host 桥/事件订阅/能力分级）+ ABI v3 测试 | ✅ 已实现，CI 验证中 |
| **G** | 文档：architecture.md / rpc-api.md / plugin-api-v3.md；README 快速开始 | ✅ 已交付 |

后续（下一里程碑）：
- 示例插件升级演示 call_host/host:event（当前示例保持 apiVersion 1 以持续验证向后兼容）
- WebSocket 传输鉴权（--ws-token + hello 校验）
- 原仓库 Tauri 前端改造为 sidecar 客户端（属前端仓库工作）
- CLI/TUI 前端基于 micyou-client 重写（可选，验证 SDK 人体工学）

### CI 约束（沙盒纪律）

- 沙盒（2 核）**不做任何 cargo 构建/测试**；仅允许 `cargo generate-lockfile` 级别的元数据操作。
- 所有编译验证走 GitHub Actions（ubuntu-22.04 / windows-2022 / macos-14），带 cargo 缓存。
- 单条沙盒命令 < 10 分钟；CI 轮询用后台脚本 + 限时检查。

## 四、风险与对策

| 风险 | 对策 |
|---|---|
| 无本地编译反馈，移植代码可能编译失败 | 尽量保持原代码结构（已验证可编译）；分小批推送；CI 快速失败后按日志修复 |
| 依赖版本漂移（2026 年 crates.io） | 沿用原 Cargo.toml 的精确版本约束 + 提交 Cargo.lock |
| ~~`ort`/ONNX 构建过重~~ | **已移除**：PureVox6/AEC7 静态编译为零依赖纯 Rust VM（`micyou-infer`），二进制自含模型，无需 onnxruntime 动态库（见 docs/pure-rust-inference.md）✅ |
| opus-decoder 依赖 git patch | workspace 根部保留 `[patch.crates-io]` Rusopus |
| 许可证 | 实验性仓库,暂未设定许可证：经上游版权持有人授权,派生代码的版权/许可头已全部移除（根目录无 LICENSE）；仅第三方模型文件保留其 MIT 声明（resources/LICENSE-*.txt） |
| CI macOS/Windows 平台代码路径无法本地验证 | 平台特定代码尽量照搬原实现；矩阵覆盖三平台 |
