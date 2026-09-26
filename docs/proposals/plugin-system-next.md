# 插件系统完全重构设计方案（MPP / Plugin Kernel v2）

> **状态：提案（待审阅）** · 面向 libmicyou 后端 · 设计目标：**插件能力范围最大、插件作者自由度最高**
> 本文为纯设计文档，不含实现承诺；§15 列出需要拍板的开放问题。

---

## 0. 目标、非目标与设计原则

### 0.1 目标

1. **能力上限最高**：插件可以触达后端的一切面——音频图（含旁路监听、注入、录制）、
   全部 RPC 方法、事件流、前端 UI、网络、文件系统、系统服务、跨设备通道，
   并且**能反向扩展宿主**（注册自己的 RPC 方法/事件/音频节点类型/输入源）。
2. **作者自由度最高**：
   - 语言自由：任何能读写 stdio/socket JSON 的语言都能写插件（进程外形态）；
   - 形态自由：沙箱 WASM / 原生动态库 / 独立进程三档自选，同一份逻辑可多档发布；
   - 结构自由：插件不局限于"效果器"或"工具"二分——可同时是音频源、汇、处理器、
     UI 面板、后台服务、RPC 服务提供者的任意组合；
   - 并发自由：插件可拥有自己的线程/任务，实时音频回调只是可选参与面之一；
   - 开发体验自由：无宿主也能开发测试（协议规范 + mock host + 录像回放夹具）。
3. **用户主权**：能力越大，授权体系越要清晰——一切特权皆经**显式、可见、可撤销**的
   能力授权；默认安全，升级自愿。

### 0.2 非目标

- 不追求移动端（Android 端）插件运行时同步落地（协议按可移植设计，实现桌面先行）；
- 不在本方案内定义应用市场/审核流程（只定义签名与分发的挂钩点）；
- 不做浏览器内插件宿主（Web 前端只承担 UI 委托与会话角色）。

### 0.3 设计原则

| # | 原则 | 含义 |
|---|---|---|
| P1 | **插件即客户端**（Plugin-as-Client） | 插件与前端走同一协议、同一路由、同一事件总线；插件能做的 ≈ 前端能做的 + 实时扩展面 |
| P2 | **一个协议，多种形态**（One Protocol, Many Shapes） | 逻辑 Host API 只有一份（MPP），WASM/native/external 只是绑定层差异 |
| P3 | **音频图优于处理链**（Graph over Chain） | 线性 DSP 链升级为可重连的有向图，插件是图中的节点/观察者/改写者 |
| P4 | **能力即授权**（Capability = Grant） | 架构上不设禁区，一切面皆可经 scope 授权开放；强制点在协议边界，全程审计 |
| P5 | **实时域自治**（RT Autonomy） | 实时回调契约极简且可绕过：插件可自带工作线程 + 无锁队列，宿主提供原语而非强加模型 |
| P6 | **失败局部化**（Blast Radius Control) | 进程外隔离、RT 违约自动旁路、崩溃重启退避——单插件失败不伤及音频主路 |

---

## 1. 总体架构

```
                        ┌────────────────────────── libmicyou 后端 ──────────────────────────┐
                        │                                                                    │
  前端会话 ──JSON-RPC──▶│  RpcService(路由/会话)  ◀──────────────┐                            │
                        │        │                              │ 注册 ext.* 方法/事件        │
                        │  Backend 服务门面                      │                            │
                        │        │                        ┌─────┴─────────────────────┐      │
                        │  ServerCore ── EventBus ───────▶│   Plugin Kernel (新 crate) │      │
                        │        │                        │  ┌──────────────────────┐  │      │
                        │  StreamGraph(音频图内核)          │  │ Permission Engine     │  │      │
                        │   ├─ 内建节点(DSP/源/汇)          │  │ (scope 授权/审计)     │  │      │
                        │   ├─ 插件节点(wasm/native)        │  ├──────────────────────┤  │      │
                        │   └─ 外部节点(shm 环)             │  │ Registry & Lifecycle  │  │      │
                        │        ▲   ▲                     │  │ (清单/装配/状态机)     │  │      │
                        │        │   │                     │  ├──────────────────────┤  │      │
                        │        │   └── MPP 绑定层 ────────│  │ HostAPI 服务注册表    │  │      │
                        │        │        (三档运行时)       │  │ core/rpc/audio/ui/   │  │      │
                        │  ┌─────┴────────────────────┐     │  │ net/fs/sys/bus/device│  │      │
                        │  │ 运行时:                   │     │  └──────────────────────┘  │      │
                        │  │  A. external(独立进程)    │     └───────────┬────────────────┘      │
                        │  │  B. wasm(组件沙箱)        │                 │ MPP (JSON-RPC 双向)   │
                        │  │  C. native(cdylib C-ABI)  │     ┌───────────▼────────────────┐      │
                        │  └──────────────────────────┘     │ 插件实例们（任意语言/形态）  │      │
                        └────────────────────────────────────┴────────────────────────────┴──────┘
```

关键决策：

- **Plugin Kernel 独立成 crate**（暂名 `micyou-plugin-kernel`），取代现有
  `micyou-plugin` + `core::plugins` 宿主接线；它对上只依赖 `micyou-api` 契约与
  core 暴露的稳定接缝（`HostRpc`、`EventBus`、`UiBridge`、`Backend`），对下管理三档运行时。
- **会话统一**：插件会话与前端会话进入同一 `SessionRegistry`（打 `kind: plugin|frontend`
  标记），共享订阅过滤、UI 委托与审计管道。`ext.*` 方法注册进同一张路由表。
- **HostAPI = 服务注册表**：不再是一个巨型 trait，而是一组按域划分的服务对象
  （§5），运行时绑定层把 MPP 调用映射到服务方法；强制授权发生在映射点。

---

## 2. 运行时形态（三档 + 多档发布）

| | A. External（独立进程） | B. WASM（组件沙箱） | C. Native（cdylib） |
|---|---|---|---|
| 语言 | **任意**（stdio/socket JSON） | Rust/C/TS/JS/Go/Python…→WASM | Rust/C/Zig/任何 C-ABI 语言 |
| 隔离 | **OS 进程级**（最强） | 沙箱线性内存（强） | 无（同进程） |
| 崩溃影响 | 无（可自动重启） | trap 捕获，实例销毁 | 拖垮宿主（如实告知用户） |
| 性能 | 控制面佳；音频经 shm 环佳 | 计算密集 DSP 佳（JIT） | 极致（零拷贝、SIMD、FFI 现成库） |
| 线程/并发 | **完全自由**（自己的进程） | 宿主代管 worker 池 | 完全自由（须守 RT 契约） |
| 系统访问 | 全部（经授权） | 经 WASI + 宿主代理 | 全部（经授权声明+审计） |
| 分发体积 | 大（带运行时） | 小 | 中（平台矩阵） |
| 适用 | 复杂应用、AI 推理、集成现有程序 | 效果器/分析器/工具、不可信来源 | 高性能 DSP、绑定既有 C++ 库 |

设计要点：

1. **同一插件可携带多档 artifact**，宿主按平台/信任级别择优装配（例：可信安装用
   native 获得性能，市场预览用 wasm 沙箱试跑）。
2. **External 是能力上限档**：它天然拥有自己的事件循环、线程、子进程、网络栈；
   宿主对它只做协议级授权与资源记账，不做进程内沙箱。这一档是"自由度最高"的
   最终答案——极端情况下，插件可以是一个完整的现有程序（只要说 MPP）。
3. **WASM 档升级技术栈**：wasmtime + 组件模型（WIT 接口）+ WASI preview2，
   可选 `wasi-nn` 特征（沙箱内跑 ML 推理，对标现有 ONNX 能力）；纯 Rust 无 JIT
   场景保留 wasmi 作为降级引擎（feature gate）。
4. **Native 档 C-ABI 重新设计**（不复用旧 `mpl_host_api_t`）：单一入口
   `mpp_create(host_vtable) -> plugin_vtable`，双向 vtable 版本化，宿主/插件各自
   声明支持的 MPP 版本区间，交集协商。

---

## 3. MPP —— MicYou Plugin Protocol

**一切形态的逻辑协议都是 MPP；JSON-RPC 2.0 双向；与前端契约同源。**

### 3.1 传输绑定

| 运行时 | 请求通道 | 音频通道 |
|---|---|---|
| external | stdio（宿主 spawn）或本地 socket（插件自连，支持"伴随应用"模式） | 共享内存环（§6.4），无环则退化为控制面插件 |
| wasm | 宿主函数导入（组件模型 WIT 定义，等价语义） | 线性内存块交接（拷贝 ≤ 数十 KiB/块，可接受） |
| native | vtable 直接调用 | 指针零拷贝 |

### 3.2 握手（类 LSP）

```jsonc
→ initialize {
    protocol: "mpp/1", pluginInfo: {id, version, runtimeFeatures:[...]},
    requestedScopes: [...],            // 运行时二次声明（须 ⊆ 清单，防偷渡）
  }
← initializeResult {
    hostInfo: {name:"libmicyou", version, apiVersion},
    grantedScopes: [...],              // 实际生效授权（Permission Engine 决议）
    hostFeatures: ["wasi-nn","shm-audio","offline-graph",...],
    limits: {rtBlockFrames:960, rtBudgetUs:2000, memMb:256, ...},
    sessionToken: "…",                 // 后续特权调用的持有凭据（external/socket 防串扰）
  }
→ initialized                          // 通知；随后宿主可下发 lifecycle 调用
```

### 3.3 消息面

- **plugin→host**：HostAPI 调用（§5 全部服务）、`ext.*` 方法响应、日志、心跳。
- **host→plugin**：生命周期（`start/stop/suspend/resume/configChanged/permissionChanged`）、
  事件投递（`host/event`）、总线消息（`bus/message`）、UI 通道数据（`ui/channel`）、
  音频块通知（external 档为 shm doorbell，非 JSON）、`ext.*` 方法调用。
- **二进制负载**：小负载 base64 内嵌；音频/大块数据一律走带外通道（shm / 内存区间句柄），
  协议保持纯文本可调试（`micyou-plugintool trace` 可直接旁观会话）。

### 3.4 与前端 RPC 的关系

MPP 复用 `micyou-api` 的方法目录与事件目录：插件调用 `rpc/call` 时，内核直接进
`RpcService::dispatch`（等价于一个带 scope 约束的会话）。**一份方法目录，三类调用者
（前端/插件/内嵌宿主），零漂移。**

---

## 4. 清单与打包（Manifest v2）

目录形态（zip 或目录直装）：

```
com.example.suite/
├── plugin.json          # 清单（JSON + JSON-Schema 校验；i18n 内联）
├── artifacts/
│   ├── suite.wasm               # B 档（组件模型）
│   ├── linux-x86_64/libsuite.so # C 档
│   ├── windows-x86_64/suite.exe # A 档（自包含二进制）
│   └── macos-aarch64/suite
├── ui/                  # 面板资源（html/js/css/wasm 皆可）
├── models/              # 私有数据（wasi-nn 模型等）
└── SIGNATURES           # 可选：minisign/ed25519 签名（对 manifest+artifacts 摘要）
```

清单骨架（完整 schema 另附 JSON-Schema 文件）：

```jsonc
{
  "format": "micyou-plugin/2",
  "id": "com.example.suite",
  "version": "1.0.0",
  "name": {"en": "Suite", "zh-CN": "套件"},
  "description": {"en": "...", "zh-CN": "..."},
  "authors": [{"name": "...", "url": "..."}],
  "license": "MIT", "homepage": "...", "repository": "...",

  "host": {"mpp": ">=1.0 <2.0", "features": ["shm-audio"]},

  "artifacts": [
    {"runtime": "wasm",     "entry": "artifacts/suite.wasm",
     "os": ["*"], "arch": ["*"], "engine": {"prefer": "wasmtime", "fallback": "wasmi"}},
    {"runtime": "native",   "entry": "artifacts/linux-x86_64/libsuite.so", "os": ["linux"], "arch": ["x86_64"]},
    {"runtime": "external", "entry": "artifacts/windows-x86_64/suite.exe", "os": ["windows"],
     "argv": ["--mpp-stdio"], "audio": "shm", "restart": {"maxRetries": 3, "backoffMs": [500,2000,10000]}}
  ],

  "permissions": {
    "requested": [
      {"scope": "audio:graph:node:comp",  "reason": {"zh-CN": "提供压缩器节点"}},
      {"scope": "audio:tap:processed",    "reason": {"en": " Loudness analysis"}, "optional": true},
      {"scope": "net:http:api.example.com/*", "reason": {"en": "Preset cloud sync"}},
      {"scope": "rpc:call:audio/*",       "reason": {"zh-CN": "面板控制静音/耳返"}},
      {"scope": "rpc:provide:ext.com.example.suite.*", "reason": {"en": "Expose analyze() to frontends"}}
    ]
  },

  "audio": {
    "nodes": [
      {"type": "comp", "label": {"en": "Compressor"}, "category": "dynamics",
       "ports": {"in": [{"id": "audio", "channels": "1-2", "formats": ["f32"]}],
                 "out": [{"id": "audio", "channels": "1-2"}],
                 "sidechain": [{"id": "sc", "optional": true}]},
       "latencyMs": 0, "rtBudgetUs": 300,
       "params": [
         {"id": "threshold", "label": {"en":"Threshold"}, "type": "float",
          "min": -60, "max": 0, "unit": "dB", "default": -24, "automation": true},
         {"id": "mode", "type": "enum", "options": ["feed-forward","feed-back"], "default": "feed-forward"}
       ]}
    ],
    "sources": [{"type": "pad", "label": {"en":"Soundpad"}, "channels": 2}],
    "sinks":   [{"type": "recorder", "label": {"en":"Recorder"}}]
  },

  "ui": {
    "panels": [{"id": "console", "label": {"en":"Console"}, "entry": "ui/console.html",
                "channel": "ui/com.example.suite.console"}],
    "windows": {"resizable": true, "minSize": [360, 480]}
  },

  "provides": {
    "methods": [{"name": "ext.com.example.suite.analyze",
                 "params": {"…JSON-Schema…"}, "result": {"…"}}],
    "events":  ["ext.com.example.suite.loudness"]
  },

  "config": {"schema": {"type": "object", "properties": {"…": {}}}},
  "resources": {"cpu": "15%", "memoryMb": 512, "threads": 4},
  "distribution": {"updateUrl": "https://…/suite.json", "channel": "stable"}
}
```

要点：

- **参数描述符**（`audio.nodes[].params`）驱动宿主**自动生成前端控件**——插件不做 UI
  也能被调参（自由度与省事兼得）。
- `provides` 使插件的扩展面**先验可发现**：前端可枚举 ext 方法生成集成 UI，权限系统
  可对其精确授权。
- `resources` 为限额与调度提示（external 档强制，wasm 档由引擎限额，native 档仅记账）。

---

## 5. HostAPI 服务面（能力全景）

服务按域组织；每域列出代表性 API（完整规范见实现期的 `mpp-spec.md`）。
所有调用在绑定层过 Permission Engine（§7），拒绝返回结构化错误并审计。

### core — 生命周期与自省
`core.info()`（宿主/版本/特征/限额）、`core.config.get|set|watch`（插件私有配置，
schema 校验）、`core.state.get|set`（自由 KV 状态，崩溃恢复用）、`core.log(level,msg)`、
`core.shutdown(reason)`（插件自请下线）、`core.setInstanceName`（同插件多实例）。

### rpc — 后端服务面（含反向扩展）
- `rpc.call(method, params)` —— 全部契约方法（受 `rpc:call:*` scope 分级）；
- `rpc.provide(name, handler)` —— **注册 `ext.<id>.<method>`**，前端与其他插件可调用；
- `rpc.retract(name)`；
- `events.subscribe(filter)` / `unsubscribe` —— 后端事件流（含 `ext.*` 事件）；
- `events.emit(name, payload)` —— 发布 `ext.<id>.<event>`（前端可订阅）。

### audio — 音频图（P3 的落点）
- `audio.graph.describe()` —— 当前图拓扑快照（节点/连接/格式/延迟）；
- `audio.graph.addNode(type, opts)` / `removeNode` / `link(from,to)` / `unlink`
  —— **插件可重连线**（含把任意节点插到任意两点之间、建立并联支路）；
- `audio.tap.attach(point, opts)` —— 只读探针：`raw`（传输原始）/`pre:<node>`/`post:<node>`/
  `monitor`（耳返后）/`loopback`（**扬声器参考信号**——第三方 AEC/对齐类插件的关键开放）；
- `audio.inject(sourceId, frames)` / `audio.source.create|push`（音效板、伴奏、TTS）；
- `audio.sink.create(format)` —— 消费处理后信号（录音、转推、转写）；
- `audio.param.set(nodeId, paramId, value)` / `getRange`（自动化：可被时间线/事件驱动）；
- `audio.device.list` / `audio.format.negotiate`；
- `audio.offline.render(graphSpec, input)` —— 离线图渲染（非实时批处理，如批量降噪导出）。

**节点实现面**（由装配时注入，非运行时调用）：`process(ctx)`（RT 契约见 §6）、
`onParamChange`、`onGraphChange`、`reset()`、`getState/setState`（工程保存/恢复）。

### ui — 前端委托
- `ui.panel.open|close|focus(panelId)`、`ui.window.open(spec)`；
- `ui.channel.send(channelId, payload)` —— 面板↔插件双向低延迟通道（宿主路由，
  前端 SDK 暴露 `postMessage` 对等物）；
- `ui.notify(title, body, actions[])`（动作回执为事件）；
- `ui.tray.propose(items)` / `ui.command.propose`（向前端**提议**托盘项/命令面板条目，
  前端决定是否渲染——UI 主权在前端，能力开放不打折）；
- `ui.indicator(kind)` —— 主动点亮"正在录音/正在监听"类隐私指示（配合 `audio:tap`）。

### net — 网络
- `net.http.request(...)`（异步，流式响应可选）；
- `net.ws.connect(url)`（全双工，事件驱动收发）；
- `net.tcp.connect|listen`、`net.udp.bind|send`（**裸套接字**，external/native 档可授，
  wasm 档经宿主代理转发；scope 精确到 host:port 通配）；
- `net.dns.resolve`。

### fs — 文件
- `fs.data.*`（插件私有数据目录，全权）；`fs.temp.*`；
- `fs.read|write(path)`（scope 按路径通配授权，越权即拒绝+审计）；
- `fs.watch(path)`（变更事件）；
- `fs.pick(kind)` —— 委托前端弹文件选择器（结果回传路径令牌，令牌即临时授权）。

### sys — 系统
- `sys.hotkey.register|unregister`（组合键→事件）；
- `sys.clipboard.read|write`；
- `sys.env.get(allowlist)`、`sys.time.*`、`sys.random`；
- `sys.spawn(cmd, argv, opts)` —— **子进程**（external/native 档、显式授权、记账审计；
  wasm 档不提供）；
- `sys.worker.spawn(fnRef|entry)` —— wasm 档的受管并发：宿主 worker 池回调
  （弥补无 wasi-threads 的并发自由）。

### bus — 插件间
- `bus.publish(topic, payload)` / `bus.subscribe(topicFilter)`；
- `bus.request(targetPlugin|topic, payload, timeoutMs)`（关联 ID 请求-响应）；
- `bus.blackboard.get|set(key)` —— 共享黑板（小型跨插件状态，带版本与变更事件）；
- `bus.discover(capability)` —— 按能力发现其他插件（例："谁提供 loudness 分析？"）。

### device — 跨设备（手机侧对称插件）
- `device.send(pluginId, payload)` / `device.request(...)`（关联响应）；
- `device.subscribe(filter)`；`device.info()`（对端插件清单/能力协商结果）；
- 手机侧运行时不在本期实现，但**协议与 scope 对称预留**。

---

## 6. 音频图内核（StreamGraph）与实时契约

### 6.1 图模型

```
transport-source ──▶ [AEC] ──▶ [NS] ──▶ [EQ] ──▶ [Plugin:comp] ──▶ [gain] ──▶ virtual-mic-sink
        │                                                    │
        └── tap:raw ─▶ [Plugin:analyzer(wasm)]               └── monitor-sink
loopback-source ─────────── sidechain ─▶ [Plugin:comp]
[Plugin:pad(source)] ─────────────────────────▶ mixer ─▶ virtual-mic-sink
[Plugin:recorder(sink)] ◀── post:NS
```

- **节点（Unit）**：source / processor / sink / mixer / tap-adapter / convert(采样率·声道·格式)。
- **端口**：音频（f32 交错或平面，声明声道区间与格式集）、参数（原子槽）、事件（无锁队列）。
- **拓扑**：任意 DAG（允许多源多汇、并联支路、sidechain 副输入）；**禁止环**，
  成环的 link 请求被拒绝并给出环路说明；图变更支持热重配（双缓冲交换，无咔哒声）。
- **兼容路径**：现有 `settings.json.processing_chain` 自动编译为一条线性子图
  （`AEC→…→VAD` 顺序语义不变）；用户/插件可在其上自由增删连线。配置序列化仍是
  链格式 + 附加图增量（overlay），旧前端不受影响。

### 6.2 执行模型

- 宿主 RT 线程按块驱动（默认 480×frames@48k = 10ms，可配 2.5–20ms）；
  拓扑序执行，缓冲池预分配，**RT 路径零分配、零锁、零系统调用**（external 节点除外，见 6.4）。
- 每节点 `rtBudgetUs`（清单声明）；宿主以移动平均监测超时：**连续违约 → 自动旁路**
  （交叉淡入干信号）+ `audio/nodeDegraded` 事件；恢复健康后自动回插（可配置为手动）。
- 参数写入为原子/三缓冲槽，任何线程可写，RT 读一致快照——插件 UI 与逻辑无需关心时序。

### 6.3 插件侧并发自由（P5）

SDK 提供而非强制：
- `RingBuffer<T>`（SPSC/MPSC 无锁环，跨线程音频/事件传递原语）；
- `WorkerPool` 抽象（wasm 档映射宿主 worker，external/native 档映射自有线程）；
- `Deadline` 计时器（RT 块预算感知）。
  插件可以完全在自有线程做重活（推理、网络），RT 回调只搬运结果——这是 AI 类
  插件（降噪/变声/转写）的标准形态。

### 6.4 External 档音频通道（shm 环）

```
宿主 RT 线程                     插件进程
   │  写入输入环(f32 块+序号)        │
   ├─────── eventfd/命名事件 ──────▶│  poll/忙等(自适应退避)
   │                               │  process()（自有线程池亦可）
   ◀────── 输出环+完成序号 ──────────┤
   │  读取；若滞后 > N 块 → 干信号直通(旁路)并记违约
```

- Linux memfd+eventfd / Windows file mapping+event / macOS shm+mach semaphore（或统一
  忙等+退避兜底）；环长 = 4 块，端到端增加 ≤ 1 块延迟；
- 图编译器尽量把 external 节点排在链尾/独立周期（10ms 网格）以掩蔽 IPC 延迟；
- 不支持 shm 的环境自动退化为"控制面插件"（其余能力不受影响）。

### 6.5 离线图（bonus 能力）

同一图内核可在离线模式跑（无 RT 约束、大块的、可并行分片），供 `audio.offline.render`
与插件批处理使用——录音后处理、批量转码、数据集标注类插件由此可能。

---

## 7. 能力与权限模型（Permission Engine）

### 7.1 Scope 语法

```
scope      := domain (":" segment)*          # 段支持 '*' 通配；末段 '**' 为深层通配
domain     := rpc | events | audio | ui | net | fs | sys | bus | device | data | ext

示例：
  rpc:call:server/status        rpc:call:audio/*          rpc:call:*
  rpc:provide:ext.com.x.*       events:sub:audio*         events:sub:*
  audio:node:comp               audio:graph:write         audio:tap:raw        audio:tap:*
  audio:inject                  audio:sink                audio:offline
  ui:panel                      ui:channel:*              ui:notify            ui:tray
  net:http:api.example.com/*    net:ws:*                  net:tcp:connect:127.0.0.1:*
  fs:data:**                    fs:read:/home/u/music/**  fs:pick
  sys:hotkey                    sys:clipboard:write       sys:spawn            sys:worker
  bus:pub:mix.*                 bus:sub:*                 bus:blackboard
  device:send:com.x.remote      device:*
```

### 7.2 决议流程

```
清单 requested(带 reason/i18n/optional)
      │
      ▼
策略解析: 档位默认策略(wasm=沙箱档 / external,native=开放档) × 用户授权记录 × 管理员策略(可选)
      │            ├─ 已授权 → 直接生效
      │            ├─ 未授权且非 optional → 前端授权对话框（scope+reason 可读呈现，支持逐项/通配）
      │            └─ optional 未授权 → 以缺失能力启动，运行时可申请临时提权
      ▼
grantedScopes（initialize 返回；`permissionChanged` 事件支持热撤销）
      ▼
运行时强制：MPP 绑定层逐调用匹配 scope → 拒绝 = 结构化错误 + 审计日志 + 可选前端提示
```

- **临时提权**：`core.requestScope(scope, reason)` → `uiRequest` 到前端 → 用户批准
  （可含时效）→ `permissionChanged`。避免"为偶发操作永久开洞"。
- **审计**：所有特权调用（含被拒）写审计环形日志；前端可开"插件活动"面板；
  `audio:tap:*` 授权期间**强制**隐私指示（`ui.indicator` 由内核代管点亮）。
- **信任档**：`sandboxed`（wasm 默认策略：无 sys:spawn / 无裸 socket / fs 限 data）
  与 `trusted`（external/native 可申请全部 scope）——但**任何 scope 都可被用户授予
  任何档**（架构无禁区，P4），差别只在默认策略与提醒强度。
- **签名与来源**：SIGNATURES 可选校验；未签名插件首次装载强提醒；市场策略在
  分发层（非内核）实现。

---

## 8. UI 扩展模型

1. **面板（panel）**：插件自带 HTML/JS/WASM 资源，前端在隔离 webview/iframe 渲染；
   资源经 `ui/panelAsset` 方法由内核代理（前端无需访问插件磁盘路径）；
   面板↔插件经 `ui.channel` 双向消息（宿主路由，带配额）。
2. **自动 UI**：`audio.nodes[].params` 与 `config.schema` 驱动前端生成原生控件——
   零 UI 代码的插件自动获得完整可调面板。
3. **窗口/托盘/命令面板**：插件只能**提议**（propose），渲染与主权在前端；
   无前端连接时相关调用返回结构化 `uiUnavailable`（插件可降级）。
4. **多实例**：同一插件可多实例（`core.setInstanceName`），面板与实例一一绑定。

---

## 9. 生命周期、稳定性与资源治理

```
installed ─enable→ resolved(授权决议) ─→ starting ─→ running ──┬─ degraded(RT违约/心跳丢失→自动旁路/重启)
    ▲                                                          └─ failed(退避重启 maxRetries)
    └─disable/uninstall─ stopping ←─ suspend/resume（会话级省电）
```

- **热插拔**：目录监视 + `plugins/*` RPC；开发模式 `plugintool dev` 直推热重载
  （状态经 `getState/setState` 迁移）。
- **崩溃隔离**：external 进程死亡 → 图内节点即时旁路（干信号）→ 按 backoff 重启 →
  `setState` 恢复；wasm trap → 实例销毁重建；native → 宿主级风险，文档与授权界面
  明示。
- **心跳与看门狗**：external 档协议级 ping（默认 2s/超时 6s）；RT 节点按块预算监测。
- **资源限额**：内存（wasm 引擎硬限 / external 软记账+告警）、CPU 配额、fd/线程数
  提示、事件投递速率限制（防总线洪泛）、`ui.channel` 字节配额。
- **确定性关停**：宿主退出前广播 `stop`（限时 flush 状态）→ 杀 external 进程树。

---

## 10. SDK、工具链与作者体验

| 交付物 | 内容 |
|---|---|
| **mpp-spec.md** | 协议规范（语言无关，含 JSON-Schema/WIT/C 头三份机器可读绑定） |
| `libmicyou-plugin-sdk`（Rust） | 一份实现编译到三档：`#[mpp::plugin]` 过程宏生成 external 协议循环 / native vtable / wasm 组件导出 |
| `@micyou/plugin-sdk`（TS/JS） | 组件模型（jco）打包 wasm 档 + Node/Deno external 档；面板前端框架适配（vanilla/Vue/React 模板） |
| `libmicyou-plugin-c` | C 头 + 微型运行时（native/external 通用） |
| `micyou-plugin-py` | external 档 asyncio SDK |
| **`micyou-plugintool`** | `new`(模板) / `build`(多档矩阵) / `lint`(清单+scope 合理性) / `package` / `sign` / `run`(拉起本地宿主) / `dev`(热重载+trace 旁观) / `test` |
| **Mock Host + 夹具** | 无后端开发：内存 mock host 实现全 MPP；音频夹具 WAV 录像回放；断言库（图拓扑/参数/输出 PCM 对比）——插件可进作者自己的 CI |
| 示例套件 | 每个扩展点一个官方示例：压缩器(图节点)/响度分析(tap+面板)/音效板(source)/录音机(sink)/云预设(http+config)/遥控器(device 跨端)/方法提供者(ext.rpc)/自定义输入源(external+net) |

---

## 11. 版本化与演进

- **MPP 语义化版本**：`initialize` 协商区间；宿主保留 N-1 兼容窗口；
  弃用项经 `hostFeatures.deprecations` 通告 + 审计日志标警。
- **特征探测**：`hostFeatures`（`shm-audio`、`wasi-nn`、`offline-graph`、`net.tcp`…）
  使插件优雅降级，而不是硬失败。
- **schema 演进**：清单 `format: micyou-plugin/2` 独立版本线；内核带 v1→v2 清单
  迁移器（若保留兼容，见 §13）。
- **ext.* 命名空间**：`ext.<plugin-id>.*` 天然防冲突；插件卸载即撤注册。

---

## 12. 安全分析（诚实的权衡）

| 威胁 | 缓解 | 残余风险 |
|---|---|---|
| 恶意插件窃取麦克风数据 | `audio:tap:*` 逐项授权 + 强制隐私指示 + 审计 | 用户误授权（与社会工程同域） |
| 权限偷渡（清单外请求） | initialize 二次声明必须 ⊆ 清单；绑定层白名单强制 | — |
| RT 拒绝服务（卡音频线程） | 预算监测 + 自动旁路；wasm 引擎燃料/epoch 中断；external 天然隔离 | native 档无法抢占（如实告知，trusted 档专属） |
| 资源耗尽 | 限额（内存/CPU/fd/速率）+ 看门狗 | native 档软限额 |
| 崩溃传染 | external/wasm 隔离；native 档明示风险 | native |
| 供应链（市场分发） | 签名 + 摘要锁定 + 来源标记 + 首装强提醒；市场审核在分发层 | 自装未签名插件=用户自决 |
| 跨插件攻击面 | bus 消息配额 + scope 化订阅；`rpc:call:ext.*` 需授权 | — |
| 隐私（fs/net/sys） | scope 精确到路径/域名/端口通配；临时提权代替永久授权 | — |

**立场声明**：本设计选择"能力全开 + 授权主权在用户"，而非"架构性阉割"。
不受信来源请引导至 wasm 沙箱档；external/native 档在授权 UI 中与"运行任何本地程序"
同级别明示。

---

## 13. 与现有系统的关系（决策点，非设计输入）

新内核独立成型后，与既有 `micyou-plugin`（v1–v3 ABI/清单/总线）的关系有两个选项：

- **选项 A（推荐）：薄兼容层**。`legacy-shim` crate 把旧清单/ABI/总线映射为新内核的
  native 档实例（旧 `HostApi` 方法一一映射到 HostAPI 服务；旧 `Plugin:<id>` 链节点
  映射为图节点）。保留一个大版本周期后移除。
- **选项 B：干净切割**。只认 `micyou-plugin/2` 清单；存量插件由作者用 SDK 重发布
  （Rust SDK 迁移成本约为改导入 + 属性宏替换）。

两选项下，**内核、协议、图、权限模型均按本方案全新实现**，不含旧代码路径。

---

## 14. 分阶段实施计划

| 阶段 | 交付 | 验收 |
|---|---|---|
| **C0 规范先行** | mpp-spec.md + JSON-Schema/WIT/C 三绑定草案 + mock host + plugintool 骨架 | 用 TS/Rust 各写一个 echo 插件跑通 initialize/lifecycle/rpc.call |
| **C1 内核与 external 档** | Plugin Kernel（Registry/状态机/Permission Engine/审计）+ external 运行时（stdio/socket）+ HostAPI 域：core/rpc/events/fs(data)/bus/ui(委托) + ext.* 方法注册进路由 | 示例：面板工具插件、RPC 方法提供插件；CI 全绿 |
| **C2 音频图内核** | StreamGraph（DAG/热重配/预算监测/自动旁路）+ 内建节点化（现有 DSP 拆为节点，链→图编译兼容）+ wasm 档（wasmtime+组件模型）+ audio 域：node/tap/param/inject | 示例：压缩器(wasm)、响度分析(tap+面板)；处理链回归测试与旧实现逐块一致 |
| **C3 全能力面** | shm 音频（external DSP/source/sink）+ net/sys/device 域 + 离线图 + 临时提权 + 签名校验 + native 档 v-next ABI | 示例：音效板(source)、录音机(sink)、云预设(http)、跨端遥控 |
| **C4 生态工具** | plugintool dev/test/sign/publish + SDK 全语言 + 文档站 + 示例套件齐套 + （若选 A）legacy-shim | 第三方作者按文档 30 分钟跑通首个插件 |

依赖关系：C0→C1→C2→C3 顺序强约束；C4 与 C2/C3 可并行。

---

## 15. 开放问题（请审阅拍板）

1. **WASM 引擎选型**：wasmtime（性能/WASI-p2/组件模型/wasi-nn，依赖较重、Apache-2.0
   含 LLVM 二进制体积）vs 保持 wasmi（轻、纯 Rust、解释执行慢一个量级）。
   *建议：wasmtime 为默认 feature，wasmi 保留为 `no-jit` 降级 feature。*
2. **音频图替换处理链**：是否接受"链格式继续作为配置的序列化形态 + 图作为运行时
   真相"的双层方案？（§6.1）还是配置也全面图形化（破坏旧配置文件兼容）？
   *建议：双层方案。*
3. **存量插件兼容**：§13 选项 A（薄 shim）还是 B（干净切割）？
   *建议：A，一个版本周期。*
4. **裸网络与子进程**（`net.tcp/udp`、`sys.spawn`）：对 external/native 档开放到
   "可授权即全开"（本方案立场），还是内核层硬性禁用某些域？
   *建议：全开可授权，默认拒绝，审计强制。*
5. **插件自连模式**（插件作为独立常驻程序连接宿主 socket，而非宿主 spawn）：
   纳入 C1 还是延后？（伴随应用形态，自由度加分项，鉴权要求更高）
   *建议：C1 做 stdio spawn，socket 自连放 C3 并附 token 鉴权。*
6. **手机侧对称运行时**的优先级：仅协议预留（本方案）还是纳入 C3 排期？
7. **清单格式**：JSON（本方案，i18n 内联 + JSON-Schema 校验成熟）vs TOML/YAML
   （作者手感好但 i18n 与 schema 工具链弱）。
8. **命名**：协议名 MPP、内核 crate 名 `micyou-plugin-kernel`、工具名
   `micyou-plugintool` 是否采纳？（影响仓库结构与文档域名）
9. **同插件多实例的上限策略**与图节点数上限（防 UI 滥用导致图爆炸）：
   硬限制（如 64 节点）还是资源记账软限制？

---

## 附录 A：扩展面对照（新设计 vs 现状，仅供审阅定位）

| 维度 | 现状（HostApi v3） | 本设计 |
|---|---|---|
| 语言 | Rust(native)/WASM(wasmi 解释) | 任意语言（external）+ WASM(JIT/组件) + native |
| 后端访问 | call_host 桥（分级白名单） | 全契约方法 + **反向注册 ext.\* 方法/事件** |
| 音频 | 线性链插入节点（宿主定序） | DAG 图：重连线/多点 tap（含 loopback）/源/汇/离线渲染 |
| UI | 面板 HTML + 开窗委托 | 面板 + 通道 + 自动参数 UI + 托盘/命令提议 + 隐私指示 |
| 并发 | 宿主代管定时器/HTTP 线程 | 自有进程/线程（external、native）+ 受管 worker（wasm）+ 无锁原语 SDK |
| 权限 | 固定能力清单（capability 字符串） | scope 语法 + 通配 + 临时提权 + 热撤销 + 审计 + 签名 |
| 稳定性 | native 崩溃拖垮宿主 | 进程隔离/自动旁路/退避重启/限额 |
| 开发体验 | 手工 ABI 镜像/宿主内调试 | 协议规范 + mock host + 录像夹具 + plugintool dev 热重载 + 多语言 SDK |
