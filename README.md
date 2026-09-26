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
