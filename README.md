# libmicyou

MicYou 桌面端后端的独立重构：前后端分离、与 Tauri 解耦的无头音频服务器库 + 守护进程。

> **状态：开发中（阶段 A：仓库骨架）。** 完整路线图见 [PLAN.md](./PLAN.md)。

## 项目定位

[MicYou](https://github.com/LanRhyme/MicYou) 将 Android 手机变成 PC 的高品质麦克风。
原桌面端的后端逻辑内嵌在 Tauri 应用中，CLI/TUI 被迫链接整个 GUI 依赖树。
`libmicyou` 把后端抽取为独立的 Rust workspace：

- **模块化**：协议 / 音频 / 插件框架 / 网络传输 / 核心编排 / RPC 契约各司其职；
- **前后端分离**：任何前端（Tauri GUI、CLI、TUI、Web、第三方）通过 JSON-RPC 2.0
  （stdio / WebSocket / 进程内）驱动后端，事件按订阅推送；
- **插件解放**：插件通过能力门控的 `call_host` 桥与事件订阅访问后端几乎全部接口；
- **线路协议零改动**：现有 Android 客户端无需更新即可连接。

## Workspace 结构

| crate | 职责 |
|---|---|
| [`micyou-protocol`](crates/micyou-protocol) | 与手机端的 protobuf 线路协议（TCP `MicY` / UDP `MicU`） |
| [`micyou-audio`](crates/micyou-audio) | cpal 输出引擎、DSP 链（AEC/NS/Dereverb/EQ/AGC/VAD）、环回采集 |
| [`micyou-plugin`](crates/micyou-plugin) | 插件框架：native cdylib + WASM 双运行时、消息总线、DSP 节点 |
| [`micyou-transport`](crates/micyou-transport) | 网络模块：TCP/UDP/Web(TLS) 服务器、mDNS、ADB、抖动缓冲 + FEC、Opus |
| [`micyou-core`](crates/micyou-core) | 核心编排：生命周期状态机、音频管线、配置、平台虚拟设备、插件宿主 |
| [`micyou-api`](crates/micyou-api) | 前后端契约：RPC 方法与事件的 serde 模式 |
| [`micyou-rpc`](crates/micyou-rpc) | JSON-RPC 2.0 路由 / 会话 / 事件订阅 / stdio·WebSocket 传输 |
| [`micyou-client`](crates/micyou-client) | Rust 客户端 SDK |
| [`micyou-daemon`](crates/micyou-daemon) | 无头守护进程二进制 |
| [`libmicyou`](crates/libmicyou) | 门面 crate：`Backend` 构建器，可嵌入任意 Rust 宿主 |

## 下载开发构建（Development Builds）

各前端均有独立的构建工作流（**仅上传 workflow artifact，不发布 Release**；
正式发布应在打 tag 时另行全量构建）：

- [Tauri Dev Build](../../actions/workflows/build-tauri.yml) → `micyou-tauri-{linux-x64,windows-x64,macos-arm64}`（前端 + daemon sidecar）
- [MicYou GUI Dev Build](../../actions/workflows/build-micyou-gui.yml) → `micyou-gui-{linux-x64,windows-x64,macos-arm64}`（原版 Vue 前端 + daemon sidecar）
- [Qt Dev Build](../../actions/workflows/build-qt.yml) → `micyou-qt-{linux-x64,windows-x64,macos-arm64}`（windeployqt/macdeployqt 打包 + daemon）
- [Flutter Dev Build](../../actions/workflows/build-flutter.yml) → `micyou-flutter-{linux-x64,windows-x64,macos-arm64}`（桌面包 + daemon sidecar）

在 Actions 页 **Run workflow** 触发，构建完成后于 run 页面底部 Artifacts 下载。
各工作流均启用 rust-cache（多 workspace 增量编译）。zip 内附 RUN-README.txt
（Linux webkit2gtk 依赖、macOS 去隔离、Windows SmartScreen/WebView2 说明）。

## 参考前端（frontends/，独立 workspace，CI 三平台构建+无头 e2e 测试）

| 前端 | 传输 | 设计语言 |
|---|---|---|
| [`frontends/micyou-gui`](./frontends/micyou-gui) | stdio（spawn `micyou-daemon` sidecar） | 原版 MicYou 桌面 UI（Vue 3 + Tailwind，自上游 `tauri-app` 整体移植，Vue 源码零改动） |
| [`frontends/tauri`](./frontends/tauri) | stdio（spawn `micyou-daemon` sidecar） | Web SPA：自定义深色 CSS、canvas 频谱、侧边栏 |
| [`frontends/qt`](./frontends/qt) | stdio（spawn sidecar，QProcess） | Qt 原生 Widgets：菜单栏/工具栏/Dock 日志/系统控件外观 |
| [`frontends/flutter`](./frontends/flutter) | stdio sidecar（dart:io Process spawn daemon）；可选远程 WebSocket | Material 3 桌面端：NavigationRail、种子配色、CustomPaint 频谱 |

四个前端都覆盖完整契约面（服务器/音频 DSP 含 10 段 EQ/连接与 IPv6 地址/虚拟设备/
插件管理/设置/系统），但各自采用其框架的原生设计范式，而非同一界面的多框架复刻。
其中 `micyou-gui` 通过一层 `invoke` 垫片（`src/adapter/tauri-core.ts`）把上游 65 个
Tauri 命令映射到 libmicyou 的 JSON-RPC 契约（`backend_rpc` 单命令透传），并把
daemon 的 `ServerEvent` 事件流桥接回上游的 Tauri 事件名与载荷形状。

```bash
# 原版 MicYou GUI（先构建前端资产，再构建/运行 Tauri 壳）
cd frontends/micyou-gui && npm ci && npm run build
cd src-tauri && cargo run    # 自动 spawn 同目录/PATH/$MICYOU_DAEMON 的守护进程

# Tauri 参考前端（自动 spawn 同目录/PATH/$MICYOU_DAEMON 的守护进程）
cd frontends/tauri && cargo run
```

Tauri 参考前端的控制器逻辑与 GUI 解耦，可在 CI 无显示环境下跑完整
「连接→启动→静音→监听→停止」回归（`cargo test`）；后端另有协议级集成测试
`phone_loopback`（模拟 Android 客户端全链路：握手/会话绑定/ping-pong/UDP 音频/
静音同步/插件消息/同端口重启）。

## 文档

- [PLAN.md](./PLAN.md) — 重构路线图与现状分析
- [docs/architecture.md](./docs/architecture.md) — 模块拓扑、数据流、生命周期与并发设计
- [docs/rpc-api.md](./docs/rpc-api.md) — JSON-RPC 契约全表（方法/事件/错误码/传输）
- [docs/plugin-api-v3.md](./docs/plugin-api-v3.md) — 插件解放：`call_host` 桥与事件订阅
- [docs/migration.md](./docs/migration.md) — 原 Tauri/CLI/TUI 前端迁移指南（事件与方法对照表）

## 快速开始

```bash
# 无头守护进程（stdio，供 GUI sidecar）
cargo run -p micyou-daemon -- --stdio

# 本地 WebSocket 服务（浏览器/多前端）
cargo run -p micyou-daemon -- --ws 127.0.0.1:9610

# 启动即开服（按 server.json 配置）
cargo run -p micyou-daemon -- --ws 127.0.0.1:9610 --autostart
```

用任意 JSON-RPC 客户端验证：

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"system/version"}' \
  | cargo run -q -p micyou-daemon -- --stdio --no-mode-lock
```

Rust 前端可嵌入门面或使用 SDK：

```rust
// 进程内嵌入
let managed = libmicyou::Builder::new().build()?;
let mut conn = managed.attach_local();          // JSON-RPC over channels

// 或连接守护进程
let mut client = micyou_client::Client::connect_stdio(
    tokio::process::Command::new("micyou-daemon").arg("--stdio"),
).await?;
client.hello("my-app", true).await?;
let status = client.server_status().await?;
```

## 开发

```bash
cargo build --workspace    # 构建
cargo test --workspace     # 测试
cargo fmt --all --check    # 格式检查
```

CI（GitHub Actions）在 ubuntu / windows / macos 三平台执行 build + test。

## 许可

GPL-3.0-or-later，附 MicYou Plugin Exception (Version 1.0)，详见 [LICENSE](./LICENSE)。
本项目派生自 [LanRhyme/MicYou](https://github.com/LanRhyme/MicYou)（Copyright (C) 2026 LanRhyme），
重构部分 Copyright (C) 2026 OrientCOMPASS。
