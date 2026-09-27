# onnx-port — PureVox6 / AEC7 纯 Rust 移植工具链

把 `crates/micyou-core/resources/{purevox6,aec7_ep0185}.onnx` 静态编译为
`crates/micyou-audio/src/assets/*.mcy`(MCYI 平面二进制),供零依赖 VM
[`micyou-infer`](../../crates/micyou-infer) 执行,从而在后端移除
`ort`/ONNX Runtime。设计与验证矩阵见
[docs/pure-rust-inference.md](../../docs/pure-rust-inference.md)。

## 依赖

```bash
pip install numpy onnx            # 编译/golden 生成
pip install onnxruntime           # 仅 parity_ort.py 需要(CI 执行)
```

## 常用命令(仓库根目录)

```bash
# 重新编译两个模型 → crates/micyou-audio/src/assets/*.mcy (+ .json sidecar)
python3 tools/onnx-port/compile_models.py
# CI 新鲜度门禁:重编译并与已提交 blob 字节级比对
python3 tools/onnx-port/compile_models.py --check

# numpy 解释器 ↔ onnxruntime 流式对拍(32 帧,全输出)
python3 tools/onnx-port/parity_ort.py --frames 32
# 逐中间层对拍(定位语义偏差用,较慢)
python3 tools/onnx-port/parity_ort.py --frames 2 --all-nodes

# 编译产物(blob)↔ 解释器重放对拍
python3 tools/onnx-port/replay_check.py --frames 16

# 重新生成 Rust golden fixtures(crates/micyou-infer/tests/fixtures)
python3 tools/onnx-port/gen_golden.py
python3 tools/onnx-port/gen_td_golden.py     # dsp.rs 处理器时域 golden
# CI 漂移校验(容差内比对已提交 fixtures)
python3 tools/onnx-port/gen_golden.py --check
python3 tools/onnx-port/gen_td_golden.py --check
```

## 模块

| 文件 | 职责 |
|---|---|
| `onnxref/interpreter.py` | 通用 numpy ONNX 解释器(两模型算子全集,语义蓝本) |
| `onnxref/compiler.py` | 静态编译:trace → 常量折叠 → 别名化 → DCE → arena 打包 → MCYI 序列化 |
| `onnxref/replay.py` | MCYI blob 的 numpy 重放器 = Rust VM 的可执行规格 |
| `parity_ort.py` | 解释器/编译产物 ↔ onnxruntime 对拍 |
| `replay_check.py` | blob ↔ 解释器 三方对拍(可选 `--with-ort`) |
| `gen_golden.py` | Rust 单算子 + 流式端到端 golden fixtures |
| `gen_td_golden.py` | 时域 golden(numpy 复刻 dsp.rs 的 STFT/OLA 布线) |

## 换模型 / 改图后

1. 放入新 onnx → 改 `parity_ort.py` 的 `MODELS` 表(输入名/每帧随机输入);
2. `compile_models.py`(新算子会在编译期显式报错,补 `interpreter.py` +
   `compiler.py` + `replay.py` + Rust 内核四处);
3. `parity_ort.py` / `replay_check.py` 通过后 `gen_golden.py`、`gen_td_golden.py`
   重新生成 fixtures;
4. 提交 blob + sidecar + fixtures,CI 全链验证。
