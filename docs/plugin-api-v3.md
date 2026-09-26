# 插件 API v3：call_host 与事件订阅（插件解放）

Host API v3 在 v2（控制面 get/set mute/monitoring/DSP）之上打通了插件与后端
**全部服务面**：插件通过 `call_host` 直接调用与前端完全相同的 RPC 方法目录，
通过 `subscribe_host_events` 订阅后端事件流。宿主实现与远程前端共用同一条
dispatch 代码路径（`RpcService::dispatch`），因此插件能做到的事与一个连接中的
前端一致——只受能力声明约束。

## 1. 新能力（plugin.json `capabilities`）

| 能力 | 授予范围 |
|---|---|
| `host.call` | 只读 + 控制面方法（见 §3 分级表 Call 列） |
| `host.admin` | 管理面方法（生命周期、持久化配置写入、插件管理、安装器）；隐含 `host.call` 范围 |
| `host.events` | 后端事件订阅（`host:event` 消息） |

清单需声明 `apiVersion: 3`（宿主 `HOST_API_VERSION = 3`，仍接受 1/2 的旧插件）。

## 2. HostApi 新方法

```rust
// micyou_plugin::HostApi（trait 默认实现返回 "not supported"，v2 宿主兼容）
fn call_host(&self, method: &str, params_json: &str) -> PluginResult<String>;
fn subscribe_host_events(&self, filter: &str) -> PluginResult<()>;
fn unsubscribe_host_events(&self, filter: &str) -> PluginResult<()>;
```

### call_host

- `method`：契约方法名（`"server/status"`、`"audio/mute/set"`…，全表见
  [rpc-api.md](./rpc-api.md)）；`params_json`：by-name 参数对象（`"{}"` 可省）。
- 返回 **JSON 信封字符串**：
  - 成功：`{"ok":true,"result":<方法返回值>}`
  - 业务失败：`{"ok":false,"error":{"code":-32000,"message":"...","data":null}}`
  - 基础设施失败（无桥/无运行时/超时 10s/能力不足）：`Err(PluginError)`
- **阻塞调用，非实时安全**：禁止在 `process_audio` 中调用；建议在工作线程/
  定时器回调中使用。

### subscribe_host_events

- `filter`：`"*"` 或事件 tag 前缀（`"audio"`、`"device"`、`"server"`、`"plugin"`…）。
- 匹配事件以总线消息投递：`source="host"`、`topic="host:event"`、
  payload = `ServerEvent` 的 JSON（`{"type":"...","data":{...}}`）。
- 插件在 `handle_message` 中处理；高频事件（audioLevel/audioMetrics/audioSpectrum）
  同样按前缀过滤，注意自行降频。
- 插件被禁用/卸载时订阅自动清除。

## 3. 方法访问分级（`micyou_api::methods::method_access`）

| 级别 | 方法 | 所需能力 |
|---|---|---|
| **Denied** | `session/hello` `session/subscribe` `session/unsubscribe` | 插件不是 RPC 会话 |
| **Admin** | `server/start` `server/stop` `server/prefs/save` `audio/settings/update` `network/firewall/allow` `usb/enable` `devices/vbcable/install` `devices/blackhole/setInput` `devices/blackhole/restore` `config/ui/save` `config/theme/save` `plugins/setEnabled` `plugins/uninstall` `plugins/config/set` `plugins/preview/*` `plugins/install/*` `plugins/import` `plugins/update/*` `mode/releaseLock` `system/log/export` | `host.admin` |
| **Call** | 其余全部（状态查询、静音/耳返/频谱控制、网络信息、插件列表/触发/面板、日志读取…） | `host.call`（或 `host.admin`） |

分级表与 dispatch 实现在同一仓库共同演进：新增方法必须同步归类。

## 4. 运行时支持

| 运行时 | 绑定 |
|---|---|
| **native (cdylib)** | `mpl_host_api_t` 末尾追加 `call_host` / `subscribe_host_events` / `unsubscribe_host_events` 三个函数指针（append-only ABI：旧插件不受影响；**新插件必须先检查 `mpl_host_info_t.api_version >= 3`** 再解引用新字段）。签名见 `include/micyou_plugin_abi.h`。 |
| **WASM (wasmi)** | 宿主导入 `"call_host"(method_ptr, params_ptr) -> envelope_ptr`、`"subscribe_host_events"(filter_ptr) -> code`、`"unsubscribe_host_events"(filter_ptr) -> code`；字符串经线性内存 NUL 结尾传递，返回值指针为宿主分配（与 `get_config` 相同约定）。 |

## 5. 示例

WASM 插件（面板按钮切换全局静音并显示服务器状态）：

```wat
;; 伪代码示意（实际见 plugins/examples）
(call $call_host (i32.const $method_server_status) (i32.const $empty_params))
;; → {"ok":true,"result":{"phase":"running","isServerRunning":true,...}}
(call $call_host (i32.const $method_mute_set) (i32.const $params_muted_true))
;; → {"ok":true,"result":{"ok":true}}
```

Native 插件（C）：

```c
if (host_info->api_version >= 3) {
    char buf[4096]; uint32_t len = sizeof buf;
    mpl_result_t r = host->call_host(ctx, "server/status", "{}", buf, &len);
    if (r == MPL_OK) { /* parse {"ok":true,"result":{...}} */ }

    host->subscribe_host_events(ctx, "device");   /* host:event 消息 */
}
```

## 6. 设计说明

- **同一 dispatch 路径**：`call_host` 不是并行实现，而是复用
  `RpcService::dispatch`（含参数反序列化与错误映射），保证插件、stdio、
  WebSocket 三种客户端行为一致，契约漂移无处发生。
- **执行模型**：桥接到捕获的 tokio 运行时上执行，调用线程以 10s 超时同步等待
  ——对 native/WASM/RPC worker 线程都安全（守护进程运行时为多线程）。
- **默认拒绝**：未声明能力的插件调用 `call_host` 得到
  `MPL_ERR_PERMISSION` / `Err(PermissionDenied)`；未知方法由路由器统一返回
  method-not-found（信封内 `-32601`）。
