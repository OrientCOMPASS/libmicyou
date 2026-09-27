# 纯 Rust 推理移植:PureVox6 / AEC7 去 ONNX Runtime 化

> 状态:已落地于 `feat/pure-rust-inference`。方法论对齐
> [silero_v4 纯 Rust 移植](https://github.com/OrientCOMPASS/Qwen3-subtitle-assistant/blob/master/finetune/silero_v4_port.md)
> (numpy 参照实现 → 位精确对拍 → 权重平面化 → Rust 机械转写 → golden 测试),
> 并针对这两个**大得多、且以匿名算子图导出**的模型做了工程化推广:
> 参照实现是通用的 numpy ONNX 解释器,Rust 侧是静态编译产物 + 微型 VM。

## 1. 为什么可行(评估结论)

对 `purevox6.onnx`(2.1 MB,1105 节点)与 `aec7_ep0185.onnx`(4.8 MB,1581 节点)的
图分析显示:

| 维度 | purevox6 | aec7 | 结论 |
|---|---|---|---|
| 算子种类 | 28 | 30 | 全部可用 numpy/Rust 机械实现(Conv/ConvT/GRU/LN/BN/Resize/…) |
| 动态 shape | 无(batch=1,全部静态) | 无 | 可全静态编译,VM 无需 shape 推理 |
| 结构性算子 | Slice/Reshape/Pad/Expand 的索引全部来自 Constant/Shape 链 | 同左 | 编译期常量折叠,运行时零 int64 张量 |
| 每帧计算量 | **2.75 MMAC**(GRU 1.71M 为主) | **8.4 MMAC**(GRU 4.25M 为主) | 10 ms 帧预算内:即使标量 1 MAC/cycle 也只要 0.9/2.8 ms |
| 折叠后运行时节点 | 584 → 440 条指令 | 802 → 616 条指令 | 别名化(Reshape/Squeeze 零成本)+ 死代码消除 |
| 运行时内存 | arena 0.14 MB + 权重 2.06 MB | arena 1.85 MB + 权重 4.73 MB | 活跃区间打包后极小 |

关键风险点均已验证消除:GRU(含双向、linear_before_reset)、ConvTranspose
(output_padding)、Resize(sizes 驱动、half_pixel 线性)、ONNX Slice 负步长语义
(`end=-1` = 越过 0 而非 numpy 的末元素)——numpy 参照实现与 onnxruntime 的
流式对拍最大绝对误差 ~1e-6(f32 求和序噪声级)。

## 2. 架构

```
purevox6.onnx / aec7_ep0185.onnx        (仓库内保留,作为权重来源与 CI 对拍基准)
        │  tools/onnx-port/compile_models.py
        │    ① numpy 解释器全图 trace(所有 shape/常量值)
        │    ② 常量折叠(Constant/Shape/结构性链全部编译期求值)
        │    ③ Reshape/Squeeze/Unsqueeze/Identity → slot 别名(零指令)
        │    ④ 死代码消除 + 活跃区间打包 f32 arena
        ▼
crates/micyou-audio/src/assets/{purevox6,aec7}.mcy   (MCYI 平面二进制:
        │                                             指令流 + slot 表 + f32 权重)
        │  include_bytes! 嵌入二进制
        ▼
crates/micyou-infer                     (零依赖纯 Rust VM:~26 个定形算子内核,
        │                                预打包 arena,每帧零分配、零图解析)
        ▼
micyou-audio dsp.rs                     (PureVoxProcessor / AecProcessor:
                                          STFT/OLA 布线不变,ort Session → VM Session)
```

`ort` crate、`libonnxruntime.{so,dll,dylib}` 动态加载、模型文件发现逻辑全部移除;
资源目录只剩 PipeWire/ALSA 配置。模型不可再"缺失":编译进二进制,降噪/AEC 永远可用。

## 3. MCYI blob 格式(v1,小端)

```
magic "MCYI" | version u32 | n_slots n_ops n_in n_out u32 | arena_len const_len u32
input slot ids | output slot ids
slot 表(长度前缀): kind u8 (0=input 1=const 2=arena 3=alias) | ndim u8 | pad u16
                    | off i32(const:f32 偏移;alias:目标 slot;input:绑定序号)
                    | dims i32×ndim
op 表(长度前缀): code u16 | n_i64 n_in n_out n_f32 u16 | in/out slot ids u32
                  | i64 attrs | f32 attrs
const 数据段: f32 × const_len
```

算子属性布局(与 `onnxref/compiler.py` 一一对应):

| opcode | i64 attrs | 备注 |
|---|---|---|
| SLICE(18) | `[n, (axis,start,end,step)×n]` | 负步长 `end=-1`=到 0 为止;`(0,0)`=空选择 |
| PAD(19) | `[begin×nd, end×nd]` + f32 `[cv]` | |
| CONV(11)/CONV_T(12) | `[kh,kw,sh,sw,dh,dw,pt,pl,pb,pr,group,has_bias(,opt_h,opt_w)]` | NCHW;1×1→GEMM、depthwise→直算、其余→im2col |
| GRU(13) | `[hidden, direction(0f/1b/2bi), lbr]` | 输入 X,W,R,B?,H0?(NULL_SLOT=缺省);输出 [Y, Y_h] 恒两位 |
| LAYER_NORM(15) | `[axis]` + f32 `[eps]` | opset-17 语义(axis 起全部归一) |
| BATCH_NORM(14) | f32 `[eps]` | 推理模式 |
| REDUCE_MEAN/L2(22/23) | `[keepdims, n_axes, axes…]` | |
| GATHER(21) | `[axis, idx…]` | 常量索引 |
| CLIP(9) | `[has_lo, has_hi]` + f32 `[lo, hi]` | |
| RESIZE(24) | 无 | linear + half_pixel,目标尺寸=输出 slot shape |
| 其余 | 见 compiler.py | ADD/SUB/MUL/DIV/POW/SIGMOID/SQRT/LOG/MATMUL/TRANSPOSE/CONCAT/EXPAND/COPY |

VM 的唯一 unsafe 不变量:**同一 op 的输出区间与任何输入区间不重叠**
(编译器活跃区间打包保证;debug 构建逐 op 校验 `validate_layout`)。

## 4. 验证矩阵(全部在 CI 执行,`native-infer.yml`)

| 层 | 对拍双方 | 工具 | 容差/结果 |
|---|---|---|---|
| ① 算子语义 | numpy 解释器 ↔ onnxruntime | `parity_ort.py --frames 32` | 流式 32 帧全输出,max_abs ≤ ~2e-5 |
| ①′ 逐层 | 同上,所有 1105/1581 个中间张量 | `parity_ort.py --all-nodes` | 层级别定位任何语义偏差 |
| ② 编译忠实性 | MCYI blob(numpy 重放)↔ 解释器 | `replay_check.py --frames 16` | 位精确(绝大多数 0.0) |
| ③ blob 新鲜度 | 重编译 ↔ 已提交产物 | `compile_models.py --check` | 字节级一致(格式确定性) |
| ④ Rust 内核 | VM ↔ numpy golden(182 个单算子用例) | `cargo test -p micyou-infer --test ops` | abs ≤ 2e-4 |
| ⑤ Rust 端到端 | VM 流式自反馈 16 帧 ↔ 解释器轨迹 | `cargo test -p micyou-infer --test e2e` | 实测 worst rel 4.8e-6 (pv) / 1.2e-5 (aec) |
| ⑥ DSP 布线 | 处理器时域输出 ↔ numpy 时域复刻 | `cargo test -p micyou-audio`(dsp.rs `native_tests`) | STFT/cache/OLA 接线 golden + 降噪/消回声属性测试 |
| ⑦ 性能 | native VM ↔ ort(main 基线) | `bench` job(同一 runner 顺序执行) | 见 §5 |

golden fixtures 由 `gen_golden.py` / `gen_td_golden.py` 生成并提交
(`--check` 在 CI 校验漂移,f32 噪声容差内)。

## 5. 性能(与 ort 版对比)

CI `bench` job 在同一 GitHub runner 上分别对 `main`(ort + onnxruntime 1.19.2,
`intra/inter_threads=1`)与本分支(纯 Rust VM)运行同一 harness
(`xtask/bench-{ort,native}`,相同的确定性信号、4 种 DSP 配置、480 样本/帧),
结果写入 job summary。本地开发机没有可比性,以 CI 数字为准:

> (首跑后由 CI 结果回填 — 见 PR 的 bench job summary)

预算参照:实时约束为每帧 10 ms(48 kHz、480 样本 hop)。模型计算量
2.75/8.4 MMAC/帧,VM 内核以标量 f32 为主、依赖自动向量化。

## 6. 目录导航

```
tools/onnx-port/
  onnxref/interpreter.py   # 通用 numpy ONNX 解释器(语义蓝本)
  onnxref/compiler.py      # ONNX → MCYI 静态编译器(折叠/别名/DCE/arena 打包)
  onnxref/replay.py        # MCYI blob 的 numpy 重放器(= Rust VM 的可执行规格)
  parity_ort.py            # ①①′ 解释器 ↔ onnxruntime 对拍
  replay_check.py          # ② blob ↔ 解释器
  compile_models.py        # ③ 编译/新鲜度校验 CLI
  gen_golden.py            # ④⑤ Rust golden fixtures(单算子 + 流式)
  gen_td_golden.py         # ⑥ 时域 golden(dsp.rs 处理器级)
crates/micyou-infer/       # 纯 Rust VM(零依赖)
crates/micyou-audio/src/assets/*.mcy{,.json}   # 编译产物(+可读 sidecar)
xtask/bench-{native,ort}/  # 性能对比 harness(独立 workspace)
```

## 7. 已知边界

- VM 只支持这两个模型编译出的算子子集与静态 shape(编译器对其他图会显式报错,
  而非静默降级);
- Resize 仅 linear+half_pixel+sizes 驱动;GRU 仅默认激活(Sigmoid/Tanh);
- 若上游更换模型(如 PureVox 202609 三件套契约),重跑 `compile_models.py` 即可,
  新算子需求会在编译期暴露;
- f32 求和序与 ORT 存在 ~1e-6 级差异(与 silero 移植同量级),听感无影响,
  流式反馈下不发散(16 帧自反馈漂移实测 ≤1.2e-5,系统收缩性良好)。
