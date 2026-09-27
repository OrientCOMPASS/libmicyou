"""Generic numpy interpreter for the ONNX subset used by purevox6 / aec7.

This is the semantic reference for the pure-Rust port (methodology follows
the silero_v4 port in Qwen3-subtitle-assistant): every op is implemented
directly in numpy with float32 arithmetic, validated bit-approximately
against onnxruntime in CI, and later transcribed mechanically to Rust.

Supported models (all shapes static, batch=1):
  * purevox6.onnx      — opset 13, 5 in / 5 out (spec + 4 flat caches)
  * aec7_ep0185.onnx   — opset 17, 14 in / 14 out (mic/far frame + 12 caches)

Op coverage (union of both graphs):
  structural: Constant ConstantOfShape Identity Reshape Transpose Squeeze
              Unsqueeze Slice Concat Expand Shape Gather Cast Pad
  elementwise: Add Sub Mul Div Pow Sqrt Log Sigmoid Clip
  reduce:     ReduceMean ReduceL2
  linear:     MatMul Conv ConvTranspose BatchNormalization LayerNormalization
  sequence:   GRU (fwd/bwd/bidirectional, linear_before_reset)
  image:      Resize (linear, half_pixel, scales-driven)
"""

from __future__ import annotations

import math
from typing import Dict, Iterable, List, Optional, Sequence, Tuple

import numpy as np
import onnx
from onnx import numpy_helper

FLOAT32 = np.float32
INT64 = np.int64


def _as_f32(x: np.ndarray) -> np.ndarray:
    return x.astype(FLOAT32, copy=False)


def attr_dict(node: onnx.NodeProto) -> dict:
    """Decode node attributes into a plain dict (tensors as np arrays)."""
    out = {}
    for a in node.attribute:
        if a.type == onnx.AttributeProto.FLOAT:
            out[a.name] = float(a.f)
        elif a.type == onnx.AttributeProto.INT:
            out[a.name] = int(a.i)
        elif a.type == onnx.AttributeProto.STRING:
            out[a.name] = a.s.decode("utf-8")
        elif a.type == onnx.AttributeProto.FLOATS:
            out[a.name] = [float(v) for v in a.floats]
        elif a.type == onnx.AttributeProto.INTS:
            out[a.name] = [int(v) for v in a.ints]
        elif a.type == onnx.AttributeProto.STRINGS:
            out[a.name] = [s.decode("utf-8") for s in a.strings]
        elif a.type == onnx.AttributeProto.TENSOR:
            out[a.name] = numpy_helper.to_array(a.t)
        else:
            raise NotImplementedError(f"attribute type {a.type} on {node.name}")
    return out


class OnnxGraph:
    """Loaded graph: initializers, node list, input/output names."""

    def __init__(self, model: onnx.ModelProto):
        self.model = model
        g = model.graph
        self.initializers: Dict[str, np.ndarray] = {
            init.name: numpy_helper.to_array(init) for init in g.initializer
        }
        self.nodes = list(g.node)
        self.input_names = [i.name for i in g.input]
        self.output_names = [o.name for o in g.output]
        self.node_attrs = [attr_dict(n) for n in self.nodes]
        # producer map for quick lookups
        self.producer: Dict[str, int] = {}
        for idx, n in enumerate(self.nodes):
            for o in n.output:
                self.producer[o] = idx


class Interpreter:
    """Value-semantic executor. `run(feeds)` returns graph outputs.

    Pass `capture=set(names)` to also record intermediate tensors (used for
    per-node ORT diffing in CI).
    """

    def __init__(self, graph: OnnxGraph):
        self.g = graph
        self.values: Dict[str, np.ndarray] = {}
        self.captured: Dict[str, np.ndarray] = {}

    # ── public API ──────────────────────────────────────────────────────

    def run(
        self,
        feeds: Dict[str, np.ndarray],
        capture: Optional[Iterable[str]] = None,
    ) -> Dict[str, np.ndarray]:
        self.values = dict(self.g.initializers)
        self.captured = {}
        capture_set = set(capture or ())
        for name, val in feeds.items():
            if name not in self.g.input_names:
                raise KeyError(f"unknown graph input {name}")
            self.values[name] = val
        for idx, node in enumerate(self.g.nodes):
            inputs = []
            for in_name in node.input:
                if in_name == "":
                    inputs.append(None)
                else:
                    if in_name not in self.values:
                        raise KeyError(
                            f"node {idx} ({node.op_type} {node.name}): "
                            f"missing input {in_name}"
                        )
                    inputs.append(self.values[in_name])
            fn = getattr(self, f"_op_{node.op_type}", None)
            if fn is None:
                raise NotImplementedError(
                    f"op {node.op_type} (node {idx} {node.name})"
                )
            results = fn(inputs, self.g.node_attrs[idx], node)
            if len(node.output) == 1:
                results = [results]
            for out_name, value in zip(node.output, results):
                if out_name == "":
                    continue
                self.values[out_name] = value
                if out_name in capture_set:
                    self.captured[out_name] = value
        return {name: self.values[name] for name in self.g.output_names}

    # ── structural ops ──────────────────────────────────────────────────

    def _op_Constant(self, inputs, attrs, node):
        return attrs["value"]

    def _op_ConstantOfShape(self, inputs, attrs, node):
        shape = tuple(int(v) for v in inputs[0].reshape(-1))
        value = attrs.get("value")
        if value is None:
            fill = np.zeros((), FLOAT32)
        else:
            fill = np.asarray(value).reshape(())
        return np.full(shape, fill, dtype=fill.dtype)

    def _op_Identity(self, inputs, attrs, node):
        return inputs[0]

    def _op_Reshape(self, inputs, attrs, node):
        data, shape_in = inputs
        allowzero = attrs.get("allowzero", 0)
        target: List[int] = [int(v) for v in np.asarray(shape_in).reshape(-1)]
        # resolve 0 dims (copy from input) when allowzero == 0
        if allowzero == 0:
            for i, d in enumerate(target):
                if d == 0 and i < len(data.shape):
                    target[i] = data.shape[i]
        return data.reshape(tuple(target))

    def _op_Transpose(self, inputs, attrs, node):
        perm = attrs.get("perm")
        return np.transpose(inputs[0], axes=tuple(perm) if perm else None)

    def _op_Squeeze(self, inputs, attrs, node):
        data = inputs[0]
        axes = inputs[1] if len(inputs) > 1 else None
        if axes is None:
            return np.squeeze(data)
        ax = tuple(sorted((int(a) % data.ndim for a in np.asarray(axes).reshape(-1)), reverse=True))
        out = data
        for a in ax:
            out = np.squeeze(out, axis=a)
        return out

    def _op_Unsqueeze(self, inputs, attrs, node):
        data, axes = inputs
        ax = sorted(int(a) % (data.ndim + len(np.asarray(axes).reshape(-1))) for a in np.asarray(axes).reshape(-1))
        out = data
        for a in ax:
            out = np.expand_dims(out, axis=a)
        return out

    def _op_Slice(self, inputs, attrs, node):
        data = inputs[0]
        starts = [int(v) for v in inputs[1].reshape(-1)]
        ends = [int(v) for v in inputs[2].reshape(-1)]
        axes = [int(v) for v in inputs[3].reshape(-1)] if inputs[3] is not None else list(range(len(starts)))
        steps = [int(v) for v in inputs[4].reshape(-1)] if len(inputs) > 4 and inputs[4] is not None else [1] * len(starts)
        idx: List[slice] = [slice(None)] * data.ndim
        for s, e, a, st in zip(starts, ends, axes, steps):
            a = a % data.ndim
            dim = data.shape[a]
            if s < 0:
                s += dim
            if e < 0:
                e += dim
            if st > 0:
                s = min(max(s, 0), dim)
                e = min(max(e, 0), dim)
                idx[a] = slice(s, e, st)
            else:
                # ONNX negative-step semantics differ from numpy: a clamped
                # end of -1 means "run through index 0" (numpy would read -1
                # as dim-1), and a clamped start of -1 selects nothing.
                s = min(max(s, -1), dim - 1)
                e = min(max(e, -1), dim - 1)
                if s == -1:
                    idx[a] = slice(0, 0, st)  # empty
                else:
                    idx[a] = slice(s, None if e == -1 else e, st)
        return data[tuple(idx)]

    def _op_Concat(self, inputs, attrs, node):
        axis = attrs.get("axis", 0)
        return np.concatenate([np.asarray(x) for x in inputs], axis=axis)

    def _op_Expand(self, inputs, attrs, node):
        data, shape_in = inputs
        target = tuple(int(v) for v in np.asarray(shape_in).reshape(-1))
        return np.broadcast_to(data, target).astype(data.dtype, copy=True)

    def _op_Shape(self, inputs, attrs, node):
        return np.array(inputs[0].shape, dtype=INT64)

    def _op_Gather(self, inputs, attrs, node):
        axis = attrs.get("axis", 0)
        return np.take(inputs[0], inputs[1], axis=axis)

    def _op_Cast(self, inputs, attrs, node):
        to = attrs["to"]
        np_dtype = {1: FLOAT32, 7: INT64, 6: np.int32, 11: np.float64, 9: bool}[to]
        return inputs[0].astype(np_dtype)

    def _op_Pad(self, inputs, attrs, node):
        mode = attrs.get("mode", "constant")
        if mode != "constant":
            raise NotImplementedError(f"Pad mode {mode}")
        data = inputs[0]
        pads = [int(v) for v in inputs[1].reshape(-1)]
        const = 0
        if len(inputs) > 2 and inputs[2] is not None:
            const = np.asarray(inputs[2]).reshape(-1)[0]
        nd = data.ndim
        width = [(pads[i], pads[i + nd]) for i in range(nd)]
        return np.pad(data, width, mode="constant", constant_values=const)

    # ── elementwise ─────────────────────────────────────────────────────

    @staticmethod
    def _both_int(a, b) -> bool:
        return np.issubdtype(np.asarray(a).dtype, np.integer) and np.issubdtype(
            np.asarray(b).dtype, np.integer
        )

    def _op_Add(self, inputs, attrs, node):
        # int64 structural chains (Shape→Gather→Add→Div→Mul→Slice/Pad/…)
        # must keep integer semantics end to end.
        if self._both_int(*inputs):
            return np.add(inputs[0], inputs[1]).astype(np.int64)
        return _as_f32(np.add(inputs[0], inputs[1]))

    def _op_Sub(self, inputs, attrs, node):
        if self._both_int(*inputs):
            return np.subtract(inputs[0], inputs[1]).astype(np.int64)
        return _as_f32(np.subtract(inputs[0], inputs[1]))

    def _op_Mul(self, inputs, attrs, node):
        if self._both_int(*inputs):
            return np.multiply(inputs[0], inputs[1]).astype(np.int64)
        return _as_f32(np.multiply(inputs[0], inputs[1]))

    def _op_Div(self, inputs, attrs, node):
        a, b = inputs
        if self._both_int(a, b):
            # ONNX integer Div truncates toward zero (C semantics); numpy's
            # `/` would promote to true division. Shape-computation chains
            # (Shape→Gather→Add→Div→…) rely on this.
            return np.trunc(np.divide(a, b)).astype(np.int64)
        return _as_f32(np.divide(a, b))

    def _op_Pow(self, inputs, attrs, node):
        if self._both_int(*inputs):
            return np.power(inputs[0], inputs[1]).astype(np.int64)
        return _as_f32(np.power(inputs[0], inputs[1]))

    def _op_Sqrt(self, inputs, attrs, node):
        return _as_f32(np.sqrt(inputs[0]))

    def _op_Log(self, inputs, attrs, node):
        with np.errstate(divide="ignore", invalid="ignore"):
            return _as_f32(np.log(inputs[0]))

    def _op_Sigmoid(self, inputs, attrs, node):
        x = inputs[0]
        with np.errstate(over="ignore"):
            return _as_f32(1.0 / (1.0 + np.exp(-x)))

    def _op_Clip(self, inputs, attrs, node):
        x = inputs[0]
        lo = None if len(inputs) < 2 or inputs[1] is None else np.asarray(inputs[1]).reshape(-1)[0]
        hi = None if len(inputs) < 3 or inputs[2] is None else np.asarray(inputs[2]).reshape(-1)[0]
        return _as_f32(np.clip(x, lo, hi))

    # ── reductions ──────────────────────────────────────────────────────

    def _op_ReduceMean(self, inputs, attrs, node):
        axes = attrs.get("axes")
        keepdims = bool(attrs.get("keepdims", 1))
        x = inputs[0]
        if axes is None:
            axes_t = tuple(range(x.ndim))
        else:
            axes_t = tuple(a % x.ndim for a in axes)
        return _as_f32(x.mean(axis=axes_t, keepdims=keepdims, dtype=FLOAT32))

    def _op_ReduceL2(self, inputs, attrs, node):
        axes = attrs.get("axes")
        keepdims = bool(attrs.get("keepdims", 1))
        x = inputs[0]
        axes_t = tuple(a % x.ndim for a in axes) if axes else tuple(range(x.ndim))
        sq = np.square(x, dtype=FLOAT32).sum(axis=axes_t, keepdims=keepdims, dtype=FLOAT32)
        return _as_f32(np.sqrt(sq))

    # ── linear algebra ──────────────────────────────────────────────────

    def _op_MatMul(self, inputs, attrs, node):
        return _as_f32(np.matmul(inputs[0], inputs[1]))

    def _op_BatchNormalization(self, inputs, attrs, node):
        x, scale, bias, mean, var = inputs
        eps = FLOAT32(attrs.get("epsilon", 1e-5))
        # channel axis = 1 for NCHW
        shape = [1, -1] + [1] * (x.ndim - 2)
        scale = np.asarray(scale, FLOAT32).reshape(shape)
        bias = np.asarray(bias, FLOAT32).reshape(shape)
        mean = np.asarray(mean, FLOAT32).reshape(shape)
        var = np.asarray(var, FLOAT32).reshape(shape)
        inv = _as_f32(1.0 / np.sqrt(var + eps))
        return _as_f32((x - mean) * inv * scale + bias)

    def _op_LayerNormalization(self, inputs, attrs, node):
        x = inputs[0]
        scale = np.asarray(inputs[1], FLOAT32)
        bias = np.asarray(inputs[2], FLOAT32) if len(inputs) > 2 and inputs[2] is not None else None
        axis = int(attrs.get("axis", -1))
        eps = FLOAT32(attrs.get("epsilon", 1e-5))
        if axis < 0:
            axis += x.ndim
        reduce_axes = tuple(range(axis, x.ndim))
        mean = x.mean(axis=reduce_axes, keepdims=True, dtype=FLOAT32)
        centered = _as_f32(x - mean)
        var = np.square(centered, dtype=FLOAT32).mean(axis=reduce_axes, keepdims=True, dtype=FLOAT32)
        inv = _as_f32(1.0 / np.sqrt(var + eps))
        out = _as_f32(centered * inv * scale)
        if bias is not None:
            out = _as_f32(out + bias)
        return out

    def _op_Conv(self, inputs, attrs, node):
        x = inputs[0]
        w = np.asarray(inputs[1], FLOAT32)
        b = inputs[2] if len(inputs) > 2 else None
        return self._conv_nd(x, w, b, attrs, transpose=False)

    def _op_ConvTranspose(self, inputs, attrs, node):
        x = inputs[0]
        w = np.asarray(inputs[1], FLOAT32)
        b = inputs[2] if len(inputs) > 2 else None
        return self._conv_nd(x, w, b, attrs, transpose=True)

    @staticmethod
    def _conv_nd(x, w, b, attrs, transpose: bool) -> np.ndarray:
        """2-D Conv / ConvTranspose, NCHW, float32, im2col / col2im."""
        x = _as_f32(x)
        if x.ndim != 4:
            raise NotImplementedError(f"Conv on {x.ndim}-D input")
        n, c_in, h_in, w_in = x.shape
        k = attrs.get("kernel_shape") or list(w.shape[2:])
        kh, kw = int(k[0]), int(k[1])
        strides = attrs.get("strides", [1, 1])
        sh, sw = int(strides[0]), int(strides[1])
        dilations = attrs.get("dilations", [1, 1])
        dh, dw = int(dilations[0]), int(dilations[1])
        pads = attrs.get("pads", [0, 0, 0, 0])
        pt, pl, pb, pr = (int(pads[0]), int(pads[1]), int(pads[2]), int(pads[3]))
        group = int(attrs.get("group", 1))
        auto_pad = attrs.get("auto_pad", "NOTSET")
        if auto_pad != "NOTSET":
            raise NotImplementedError(f"auto_pad {auto_pad}")
        opad = attrs.get("output_padding", [0, 0])
        opt_h, opt_w = int(opad[0]), int(opad[1])

        if not transpose:
            m = w.shape[0]
            c_per_g = c_in // group
            m_per_g = m // group
            eff_kh, eff_kw = dh * (kh - 1) + 1, dw * (kw - 1) + 1
            h_out = (h_in + pt + pb - eff_kh) // sh + 1
            w_out = (w_in + pl + pr - eff_kw) // sw + 1
            if pt or pl or pb or pr:
                xp = np.pad(x, ((0, 0), (0, 0), (pt, pb), (pl, pr)), mode="constant")
            else:
                xp = x
            # im2col: [n, group, c/g*kh*kw, h_out*w_out]
            cols = np.empty((n, group, c_per_g * kh * kw, h_out * w_out), FLOAT32)
            for gi in range(group):
                xg = xp[:, gi * c_per_g:(gi + 1) * c_per_g, :, :]
                col = np.empty((n, c_per_g, kh, kw, h_out, w_out), FLOAT32)
                for i in range(kh):
                    i0 = i * dh
                    for j in range(kw):
                        j0 = j * dw
                        col[:, :, i, j, :, :] = xg[
                            :, :, i0:i0 + sh * h_out:sh, j0:j0 + sw * w_out:sw
                        ]
                cols[:, gi] = col.reshape(n, c_per_g * kh * kw, h_out * w_out)
            out = np.empty((n, m, h_out, w_out), FLOAT32)
            wg = w.reshape(group, m_per_g, c_per_g * kh * kw)
            for gi in range(group):
                res = np.matmul(wg[gi][None, :, :], cols[:, gi:gi + 1, :, :])
                out[:, gi * m_per_g:(gi + 1) * m_per_g] = res.reshape(n, m_per_g, h_out, w_out)
            if b is not None:
                out = _as_f32(out + np.asarray(b, FLOAT32).reshape(1, m, 1, 1))
            return out

        # ConvTranspose: w layout [C_in, M/group, kh, kw]
        m_per_g = w.shape[1]
        m = m_per_g * group
        c_per_g = c_in // group
        eff_kh, eff_kw = dh * (kh - 1) + 1, dw * (kw - 1) + 1
        h_out = (h_in - 1) * sh - pt - pb + eff_kh + opt_h
        w_out = (w_in - 1) * sw - pl - pr + eff_kw + opt_w
        # scatter into a padded canvas then crop
        canvas_h = (h_in - 1) * sh + eff_kh
        canvas_w = (w_in - 1) * sw + eff_kw
        canvas = np.zeros((n, m, canvas_h, canvas_w), FLOAT32)
        xcols = x.reshape(n, group, c_per_g, h_in * w_in)
        wg = w.reshape(group, c_per_g, m_per_g * kh * kw)
        for gi in range(group):
            # [n, m_per_g*kh*kw, h_in*w_in]
            res = np.matmul(np.transpose(wg[gi], (1, 0))[None, :, :], xcols[:, gi:gi + 1, :, :])
            res = _as_f32(res).reshape(n, m_per_g, kh, kw, h_in, w_in)
            for mi in range(m_per_g):
                ch = gi * m_per_g + mi
                for i in range(kh):
                    i0 = i * dh
                    for j in range(kw):
                        j0 = j * dw
                        # input position (a,b) scatters to canvas (a*sh + i0, b*sw + j0)
                        canvas[
                            :, ch,
                            i0:i0 + sh * h_in:sh,
                            j0:j0 + sw * w_in:sw,
                        ] += res[:, mi, i, j, :, :]
        # crop: pads remove rows/cols from the canvas (scatter formulation)
        assert opt_h <= pb and opt_w <= pr, "output_padding exceeds end pads"
        out = canvas[:, :, pt:pt + h_out, pl:pl + w_out]
        if b is not None:
            out = _as_f32(out + np.asarray(b, FLOAT32).reshape(1, m, 1, 1))
        return np.ascontiguousarray(out)

    def _op_GRU(self, inputs, attrs, node):
        """ONNX GRU. X [seq, batch, in]; W/R [dir, 3h, *]; B [dir, 6h].

        Gate order (z, r, h); with linear_before_reset=1:
            z = sigmoid(x·Wzᵀ + h·Rzᵀ + Wbz + Rbz)
            r = sigmoid(x·Wrᵀ + h·Rrᵀ + Wbr + Rbr)
            n = tanh(x·Whᵀ + Whb + r ⊙ (h·Rhᵀ + Rhb))
            h' = (1 - z) ⊙ n + z ⊙ h
        """
        x = _as_f32(inputs[0])
        w = _as_f32(inputs[1])
        r = _as_f32(inputs[2])
        b = _as_f32(inputs[3]) if len(inputs) > 3 and inputs[3] is not None else None
        h = int(attrs["hidden_size"])
        num_dir = w.shape[0]
        init_h = (
            _as_f32(inputs[5])
            if len(inputs) > 5 and inputs[5] is not None
            else np.zeros((num_dir, x.shape[1], h), FLOAT32)
        )
        direction = attrs.get("direction", "forward")
        seq, batch, _ = x.shape
        dirs = []
        if direction in ("forward", "bidirectional"):
            dirs.append(0)
        if direction in ("backward", "bidirectional"):
            dirs.append(1 if direction == "bidirectional" else 0)
        ys = np.zeros((seq, num_dir, batch, h), FLOAT32)
        yh = np.zeros((num_dir, batch, h), FLOAT32)
        for d_idx, w_idx in enumerate(dirs):
            wd = w[w_idx]  # [3h, in]
            rd = r[w_idx]  # [3h, h]
            if b is not None:
                wb, rb = _as_f32(b[w_idx][:3 * h]), _as_f32(b[w_idx][3 * h:])
            else:
                wb = np.zeros(3 * h, FLOAT32)
                rb = np.zeros(3 * h, FLOAT32)
            bz, br_, bh = wb[:h], wb[h:2 * h], wb[2 * h:]
            rbz, rbr, rbh = rb[:h], rb[h:2 * h], rb[2 * h:]
            state = init_h[w_idx].copy()
            reverse = direction == "backward" or (direction == "bidirectional" and w_idx == 1)
            order = range(seq - 1, -1, -1) if reverse else range(seq)
            # input projection (constant over timesteps' state): [seq, batch, 3h]
            xproj = _as_f32(np.matmul(x, np.transpose(wd)))
            xz_all, xr_all, xh_all = xproj[:, :, :h], xproj[:, :, h:2 * h], xproj[:, :, 2 * h:]
            for t in order:
                rproj = _as_f32(np.matmul(state, np.transpose(rd)))  # [batch, 3h]
                rz_t, rr_t, rh_t = rproj[:, :h], rproj[:, h:2 * h], rproj[:, 2 * h:]
                zt = _sigmoid_f32(_as_f32(_as_f32(xz_all[t] + rz_t) + _as_f32(bz + rbz)))
                rt = _sigmoid_f32(_as_f32(_as_f32(xr_all[t] + rr_t) + _as_f32(br_ + rbr)))
                if int(attrs.get("linear_before_reset", 0)):
                    ht = _as_f32(np.tanh(_as_f32(_as_f32(xh_all[t] + bh) + _as_f32(rt * _as_f32(rh_t + rbh)))))
                else:
                    ht = _as_f32(np.tanh(_as_f32(_as_f32(xh_all[t] + _as_f32(rt * rh_t)) + _as_f32(bh + rbh))))
                state = _as_f32(_as_f32(_as_f32(1.0 - zt) * ht) + _as_f32(zt * state))
                ys[t, d_idx] = state
            yh[d_idx] = state
        return [ys, yh]

    # ── resize ──────────────────────────────────────────────────────────

    def _op_Resize(self, inputs, attrs, node):
        mode = attrs.get("mode", "nearest")
        ctm = attrs.get("coordinate_transformation_mode", "half_pixel")
        x = _as_f32(inputs[0])
        # ONNX Resize inputs: X, roi, scales, sizes (positions 2 and 3!)
        scales = inputs[2] if len(inputs) > 2 and inputs[2] is not None else None
        sizes = inputs[3] if len(inputs) > 3 and inputs[3] is not None else None
        if mode != "linear":
            raise NotImplementedError(f"Resize mode {mode}")
        if ctm != "half_pixel":
            raise NotImplementedError(f"Resize ctm {ctm}")
        if sizes is not None and np.asarray(sizes).size:
            out_shape = tuple(int(v) for v in np.asarray(sizes).reshape(-1))
            # half_pixel with sizes: x_orig = (x_out + 0.5) * (in/out) - 0.5
            ratios = [np.float32(i) / np.float32(o) for i, o in zip(x.shape, out_shape)]
        elif scales is not None:
            scales_f = [np.float32(s) for s in np.asarray(scales).reshape(-1)]
            out_shape = tuple(int(math.floor(d * float(s))) for d, s in zip(x.shape, scales_f))
            ratios = [np.float32(1.0) / s for s in scales_f]
        else:
            raise NotImplementedError("Resize without scales or sizes")
        out = x
        # separable per-dimension linear interpolation
        for dim in range(x.ndim):
            in_d = out.shape[dim]
            out_d = out_shape[dim]
            if in_d == out_d:
                continue
            ratio = ratios[dim]
            coords = _as_f32((np.arange(out_d, dtype=np.float32) + np.float32(0.5)) * ratio - np.float32(0.5))
            lo = np.floor(coords).astype(np.int64)
            frac = _as_f32(coords - lo.astype(np.float32))
            lo_c = np.clip(lo, 0, in_d - 1)
            hi_c = np.clip(lo + 1, 0, in_d - 1)
            # move dim to front
            out = np.moveaxis(out, dim, 0)
            low = out[lo_c]
            high = out[hi_c]
            shape = [out_d] + [1] * (out.ndim - 1)
            frac_b = frac.reshape(shape)
            out = _as_f32(low + _as_f32((high - low) * frac_b))
            out = np.moveaxis(out, 0, dim)
        return np.ascontiguousarray(_as_f32(out))


def _sigmoid_f32(x: np.ndarray) -> np.ndarray:
    with np.errstate(over="ignore"):
        return _as_f32(1.0 / (1.0 + np.exp(-x, dtype=FLOAT32)))


def load(path: str) -> Tuple[OnnxGraph, Interpreter]:
    model = onnx.load(path)
    graph = OnnxGraph(model)
    return graph, Interpreter(graph)
