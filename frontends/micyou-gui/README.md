# micyou-gui — 原版 MicYou 桌面 GUI × libmicyou 实验性后端

把上游 [MicYou](https://github.com/LanRhyme/MicYou) 的 Tauri/Vue 桌面前端
（`tauri-app/`）整体移植到 libmicyou 的解耦后端上：

- **Vue 源码零改动**：`src/` 原样拷贝自上游（Vue 3 + Vite + Tailwind +
  vue-i18n + reka-ui），保留上游版权头与 GPL-3.0-or-later WITH
  MicYou-Plugin-Exception 许可。
- **所有业务逻辑在 `micyou-daemon` sidecar**：壳（`src-tauri/`）spawn
  `micyou-daemon --stdio --no-mode-lock`，通过换行分隔 JSON-RPC 通信。
- 上游壳内嵌的服务器/DSP/插件宿主实现全部由 libmicyou 后端契约
  （`crates/micyou-api`）取代。

## 架构

```
┌────────────────────────── micyou-gui（Tauri 壳） ─────────────────────────┐
│  Vue 3 bundle（上游原样）                                                  │
│    invoke('start_server', …) ──► src/adapter/tauri-core.ts（构建期垫片）   │
│                                    │  命令名/参数/结果形状映射              │
│                                    ▼                                       │
│    本地命令（窗口/托盘/主题/日志/对话框）      backend_rpc(method, params)  │
│                                              │ JSON-RPC over stdio         │
└──────────────────────────────────────────────┼─────────────────────────────┘
                                               ▼
                                   micyou-daemon（sidecar 子进程）
                                   libmicyou 全部后端能力
```

- **命令垫片**（`src/adapter/tauri-core.ts`）：`vite.config.ts` 里的
  `adapterShim` 插件在打包时把应用源码对 `@tauri-apps/api/core` 的导入重定向
  到垫片。垫片把上游 45 个后端命令映射为单个 `backend_rpc` 透传（含少量
  参数/结果形状转换，如 `set_mute_state{isMuted}` → `audio/mute/set{muted}`、
  `check_pipewire` 结果的 camelCase→snake_case）；其余 20 个命令由壳本地实现
  （原名注册，直接透传）。
- **事件桥**（`src-tauri/src/events.rs`）：daemon 的 `ServerEvent` 流被还原为
  上游 Tauri 事件名与载荷形状（`audio-level` 裸数值、`mute-state-changed`
  裸布尔、`audio-spectrum` 仅发主窗口等），Vue 的 23 处 `listen` 无需改动。
- **uiRequest**：插件请求打开面板时（`uiRequest::openPluginPanel`），壳按上游
  规则创建 `plugin-window-*` 窗口并加载 `#/plugin/<id>/<panel>` 路由。
- **托盘**：上游 tray 实现原样移植（本地化菜单字符串由 Vue 推送，点击事件以
  `tray-action` 回发）。
- **主题包**（installed themes）与**壳日志**（`<app_log_dir>/micyou.log`，
  供设置页查看/导出）为 GUI 本地文件状态，按上游语义实现。
- `switch_to_cli/tui`：实验性前端未捆绑 CLI/TUI sidecar，返回明确错误。

## 构建与运行

```bash
# 1. 前端资产（license 报告 + vue-tsc 类型检查 + vite 打包 → dist/）
npm ci
npm run build

# 2. Tauri 壳（需要系统 webkit2gtk/gtk 开发包，见 CI 工作流）
cd src-tauri
cargo run            # debug；自动 spawn 同目录/PATH/$MICYOU_DAEMON 的 daemon
```

daemon 解析顺序：`$MICYOU_DAEMON` → 壳可执行文件同目录的 `micyou-daemon`
→ `PATH`。先在仓库根 `cargo build -p micyou-daemon` 并把二进制放到壳旁边
（或设置环境变量）即可联调。

开发模式（HMR）：`npm run dev`（vite :1420）+ `cargo tauri dev`（需
@tauri-apps/cli，已在 devDependencies）。

## 与上游的差异

| 方面 | 上游 tauri-app | 本前端 |
|---|---|---|
| 后端 | 壳内嵌（同一进程） | `micyou-daemon` sidecar（stdio JSON-RPC） |
| invoke 命令 | 76 个 Tauri 命令 | 20 个本地命令 + `backend_rpc` 透传 45 个 |
| license 报告 | `cargo-about` 全量 | 轻量版：npm 依赖全文 + 后端 crate 索引 |
| CLI/TUI 切换 | spawn 捆绑 sidecar | 显式报错（未捆绑） |
| 模式锁 | GUI 抢占 `mode.lock` | daemon `--no-mode-lock`（与正式版共存） |
| identifier | `com.lanrhyme.micyou` | `dev.libmicyou.micyou-gui`（数据目录隔离） |

CI：[ci.yml](../../.github/workflows/ci.yml) 的 `frontend-gui` 作业在三平台做
`npm ci + npm run build + cargo build`；
[build-micyou-gui.yml](../../.github/workflows/build-micyou-gui.yml)
（手动触发）产出 `micyou-gui-{linux-x64,windows-x64,macos-arm64}` 开发构建
zip（GUI + daemon + RUN-README）。
