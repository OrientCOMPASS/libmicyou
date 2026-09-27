"""Replay a compiled MCYI blob in numpy — proves the compiled artifact is
faithful to the reference interpreter, independent of the Rust VM.

Kernel semantics mirror crates/micyou-infer exactly (same formulas, same
f32 rounding points), so this replayer doubles as the executable spec for
the Rust port.
"""

from __future__ import annotations

import struct
from typing import Dict, List, Tuple

import numpy as np

from .compiler import (  # opcode / slot-kind constants
    NULL_SLOT,
    OPS,
    SLOT_ALIAS,
    SLOT_ARENA,
    SLOT_CONST,
    SLOT_INPUT,
    MAGIC,
    VERSION,
)

F32 = np.float32


def _as_f32(x):
    return x.astype(F32, copy=False)


class BlobGraph:
    """Parsed MCYI blob."""

    def __init__(self, data: bytes):
        assert data[:4] == MAGIC, "bad magic"
        (ver, n_slots, n_ops, n_in, n_out) = struct.unpack_from("<IIIII", data, 4)
        assert ver == VERSION
        (arena_len, const_len) = struct.unpack_from("<II", data, 24)
        pos = 32
        self.inputs: List[int] = list(struct.unpack_from(f"<{n_in}I", data, pos))
        pos += 4 * n_in
        self.outputs: List[int] = list(struct.unpack_from(f"<{n_out}I", data, pos))
        pos += 4 * n_out
        (slot_bytes_len,) = struct.unpack_from("<I", data, pos)
        pos += 4
        self.slot_kind: List[int] = []
        self.slot_shape: List[Tuple[int, ...]] = []
        self.slot_off: List[int] = []
        sp = pos
        for _ in range(n_slots):
            kind, ndim, _pad, off = struct.unpack_from("<BBHi", data, sp)
            sp += 8
            dims = struct.unpack_from(f"<{ndim}i", data, sp)
            sp += 4 * ndim
            self.slot_kind.append(kind)
            self.slot_shape.append(tuple(dims))
            self.slot_off.append(off)
        pos += slot_bytes_len
        (op_bytes_len,) = struct.unpack_from("<I", data, pos)
        pos += 4
        self.ops = []
        op_end = pos + op_bytes_len
        while pos < op_end:
            code, n_i64, n_ins, n_outs, n_f32 = struct.unpack_from("<HHHHH", data, pos)
            pos += 10
            ins = list(struct.unpack_from(f"<{n_ins}I", data, pos))
            pos += 4 * n_ins
            outs = list(struct.unpack_from(f"<{n_outs}I", data, pos))
            pos += 4 * n_outs
            i64 = list(struct.unpack_from(f"<{n_i64}q", data, pos))
            pos += 8 * n_i64
            f32 = list(struct.unpack_from(f"<{n_f32}f", data, pos))
            pos += 4 * n_f32
            self.ops.append((code, ins, outs, i64, f32))
        self.arena_len = arena_len
        self.const_data = np.frombuffer(data[pos:pos + 4 * const_len], dtype="<f4")
        # resolve aliases transitively
        self.slot_root = []
        for sid in range(n_slots):
            root = sid
            while self.slot_kind[root] == SLOT_ALIAS:
                root = self.slot_off[root]
            self.slot_root.append(root)


def _sigmoid(x):
    with np.errstate(over="ignore"):
        return _as_f32(1.0 / (1.0 + np.exp(-x, dtype=F32)))


_DUMP_FH = None
_DUMP_IDX = [0]


def run_blob(blob: BlobGraph, feeds: List[np.ndarray], dump_path: str | None = None) -> List[np.ndarray]:
    global _DUMP_FH
    if dump_path:
        _DUMP_FH = open(dump_path, "w")
        _DUMP_FH.write("# op dumps\n")
        _DUMP_IDX[0] = 0
    n_slots = len(blob.slot_kind)
    store: Dict[int, np.ndarray] = {}
    for i, sid in enumerate(blob.inputs):
        store[blob.slot_root[sid]] = _as_f32(feeds[i])

    def slot_view(sid: int) -> np.ndarray:
        root = blob.slot_root[sid]
        arr = store.get(root)
        if arr is None:
            kind = blob.slot_kind[sid]
            if kind == SLOT_CONST:
                off = blob.slot_off[sid]
                numel = int(np.prod(blob.slot_shape[sid], dtype=np.int64)) if blob.slot_shape[sid] else 1
                arr = blob.const_data[off:off + numel]
                store[root] = arr
            else:
                raise KeyError(f"slot {sid} not produced")
        shape = blob.slot_shape[sid]
        if arr.shape != shape:
            arr = arr.reshape(shape)
        return arr

    INV = {v: k for k, v in OPS.items()}
    for code, ins, outs, i64, f32 in blob.ops:
        op = INV[code]
        A = [slot_view(s) if s != NULL_SLOT else None for s in ins]
        if op in ("ADD", "SUB", "MUL", "DIV", "POW"):
            fn = {"ADD": np.add, "SUB": np.subtract, "MUL": np.multiply,
                  "DIV": np.divide, "POW": np.power}[op]
            res = [_as_f32(fn(A[0], A[1]))]
        elif op == "SIGMOID":
            res = [_sigmoid(A[0])]
        elif op == "SQRT":
            res = [_as_f32(np.sqrt(A[0]))]
        elif op == "LOG":
            with np.errstate(divide="ignore", invalid="ignore"):
                res = [_as_f32(np.log(A[0]))]
        elif op == "CLIP":
            has_lo, has_hi = i64[0], i64[1]
            lo = F32(f32[0]) if has_lo else None
            hi = F32(f32[1]) if has_hi else None
            res = [_as_f32(np.clip(A[0], lo, hi))]
        elif op == "MATMUL":
            res = [_as_f32(np.matmul(A[0], A[1]))]
        elif op in ("CONV", "CONV_T"):
            kh, kw, sh, sw, dh, dw, pt, pl, pb, pr, group, has_bias = i64[:12]
            opt = i64[12:14] if op == "CONV_T" else [0, 0]
            res = [_conv(A[0], A[1], A[2] if has_bias else None,
                         (kh, kw), (sh, sw), (dh, dw), (pt, pl, pb, pr), group,
                         tuple(opt), transpose=(op == "CONV_T"))]
        elif op == "GRU":
            hidden, direction, lbr = i64
            res = _gru(A[0], A[1], A[2], A[3], A[4], hidden, direction, lbr)
        elif op == "BATCH_NORM":
            x, scale, bias, mean, var = A
            eps = F32(f32[0])
            shp = [1, -1] + [1] * (x.ndim - 2)
            scale = scale.reshape(shp); bias = bias.reshape(shp)
            mean = mean.reshape(shp); var = var.reshape(shp)
            inv = _as_f32(1.0 / np.sqrt(var + eps))
            res = [_as_f32((x - mean) * inv * scale + bias)]
        elif op == "LAYER_NORM":
            axis, = i64
            eps = F32(f32[0])
            x, scale, bias = A
            rax = tuple(range(axis, x.ndim))
            mean = x.mean(axis=rax, keepdims=True, dtype=F32)
            centered = _as_f32(x - mean)
            var = np.square(centered, dtype=F32).mean(axis=rax, keepdims=True, dtype=F32)
            inv = _as_f32(1.0 / np.sqrt(var + eps))
            out = _as_f32(centered * inv * scale)
            if bias is not None:
                out = _as_f32(out + bias)
            res = [out]
        elif op == "TRANSPOSE":
            res = [np.ascontiguousarray(np.transpose(A[0], axes=tuple(i64)))]
        elif op == "CONCAT":
            axis = i64[0] % A[0].ndim
            res = [np.ascontiguousarray(np.concatenate(A, axis=axis))]
        elif op == "SLICE":
            cnt = i64[0]
            data = A[0]
            idx = [slice(None)] * data.ndim
            for k in range(cnt):
                a, s, e, st = i64[1 + 4 * k: 5 + 4 * k]
                if st > 0:
                    idx[a] = slice(s, e, st)
                else:
                    if s == 0 and e == 0:
                        idx[a] = slice(0, 0, st)
                    else:
                        idx[a] = slice(s, None if e == -1 else e, st)
            res = [np.ascontiguousarray(data[tuple(idx)])]
        elif op == "PAD":
            nd = len(A[0].shape)
            pads = i64[:2 * nd]
            cv = f32[0]
            width = [(pads[i], pads[i + nd]) for i in range(nd)]
            res = [_as_f32(np.pad(A[0], width, mode="constant", constant_values=cv))]
        elif op == "EXPAND":
            shape = blob.slot_shape[blob.slot_root[outs[0]]]
            res = [_as_f32(np.broadcast_to(A[0], shape).copy())]
        elif op == "GATHER":
            axis = i64[0]
            indices = np.array(i64[1:], dtype=np.int64)
            res = [np.ascontiguousarray(np.take(A[0], indices, axis=axis))]
        elif op in ("REDUCE_MEAN", "REDUCE_L2"):
            keepdims, nax = i64[0], i64[1]
            axes = tuple(i64[2:2 + nax])
            if op == "REDUCE_MEAN":
                res = [_as_f32(A[0].mean(axis=axes, keepdims=bool(keepdims), dtype=F32))]
            else:
                sq = np.square(A[0], dtype=F32).sum(axis=axes, keepdims=bool(keepdims), dtype=F32)
                res = [_as_f32(np.sqrt(sq))]
        elif op == "RESIZE":
            res = [_resize_linear_half_pixel(A[0], blob.slot_shape[blob.slot_root[outs[0]]])]
        elif op == "COPY":
            res = [np.array(A[0], dtype=F32)]
        else:
            raise NotImplementedError(op)

        for o, v in zip(outs, res):
            if o != NULL_SLOT:
                store[blob.slot_root[o]] = np.ascontiguousarray(_as_f32(v))
        if _DUMP_FH is not None:
            parts = [str(_DUMP_IDX[0]), str(code)]
            for o, v in zip(outs, res):
                if o == NULL_SLOT:
                    parts.append("null")
                    continue
                arr = np.asarray(v, np.float32).reshape(-1)
                s = float(np.float64(arr.sum())) if arr.size else 0.0
                v0 = float(arr[0]) if arr.size else float("nan")
                v1 = float(arr[1]) if arr.size > 1 else float("nan")
                parts.append(f"{s:.9e} {v0:.9e} {v1:.9e}")
            _DUMP_FH.write(" ".join(parts) + "\n")
            _DUMP_IDX[0] += 1

    results = []
    for sid in blob.outputs:
        arr = slot_view(sid)
        results.append(np.array(arr, dtype=F32))
    if _DUMP_FH is not None:
        _DUMP_FH.close()
        _DUMP_FH = None
    return results


def _resize_linear_half_pixel(x: np.ndarray, out_shape) -> np.ndarray:
    out = x
    for dim in range(x.ndim):
        in_d = out.shape[dim]
        out_d = int(out_shape[dim])
        if in_d == out_d:
            continue
        ratio = F32(in_d) / F32(out_d)
        coords = _as_f32((np.arange(out_d, dtype=F32) + F32(0.5)) * ratio - F32(0.5))
        lo = np.floor(coords).astype(np.int64)
        frac = _as_f32(coords - lo.astype(F32))
        lo_c = np.clip(lo, 0, in_d - 1)
        hi_c = np.clip(lo + 1, 0, in_d - 1)
        out = np.moveaxis(out, dim, 0)
        low, high = out[lo_c], out[hi_c]
        shape = [out_d] + [1] * (out.ndim - 1)
        out = _as_f32(low + _as_f32((high - low) * frac.reshape(shape)))
        out = np.moveaxis(out, 0, dim)
    return np.ascontiguousarray(out)


def _conv(x, w, b, k, s, d, pads, group, opt, transpose):
    x = _as_f32(x)
    n, c_in, h_in, w_in = x.shape
    kh, kw = k
    sh, sw = s
    dh, dw = d
    pt, pl, pb_, pr = pads
    eff_kh, eff_kw = dh * (kh - 1) + 1, dw * (kw - 1) + 1
    if not transpose:
        m = w.shape[0]
        cpg = c_in // group
        mpg = m // group
        h_out = (h_in + pt + pb_ - eff_kh) // sh + 1
        w_out = (w_in + pl + pr - eff_kw) // sw + 1
        xp = np.pad(x, ((0, 0), (0, 0), (pt, pb_), (pl, pr))) if (pt or pl or pb_ or pr) else x
        cols = np.empty((n, group, cpg * kh * kw, h_out * w_out), F32)
        for gi in range(group):
            xg = xp[:, gi * cpg:(gi + 1) * cpg]
            col = np.empty((n, cpg, kh, kw, h_out, w_out), F32)
            for i in range(kh):
                for j in range(kw):
                    col[:, :, i, j] = xg[:, :, i * dh:i * dh + sh * h_out:sh,
                                         j * dw:j * dw + sw * w_out:sw]
            cols[:, gi] = col.reshape(n, cpg * kh * kw, h_out * w_out)
        out = np.empty((n, m, h_out, w_out), F32)
        wg = w.reshape(group, mpg, cpg * kh * kw)
        for gi in range(group):
            r = np.matmul(wg[gi][None], cols[:, gi:gi + 1])
            out[:, gi * mpg:(gi + 1) * mpg] = r.reshape(n, mpg, h_out, w_out)
    else:
        mpg = w.shape[1]
        m = mpg * group
        cpg = c_in // group
        opt_h, opt_w = opt
        h_out = (h_in - 1) * sh - pt - pb_ + eff_kh + opt_h
        w_out = (w_in - 1) * sw - pl - pr + eff_kw + opt_w
        canvas_h = (h_in - 1) * sh + eff_kh
        canvas_w = (w_in - 1) * sw + eff_kw
        canvas = np.zeros((n, m, canvas_h, canvas_w), F32)
        xcols = x.reshape(n, group, cpg, h_in * w_in)
        wg = w.reshape(group, cpg, mpg * kh * kw)
        for gi in range(group):
            r = _as_f32(np.matmul(np.transpose(wg[gi])[None], xcols[:, gi:gi + 1]))
            r = r.reshape(n, mpg, kh, kw, h_in, w_in)
            for mi in range(mpg):
                ch = gi * mpg + mi
                for i in range(kh):
                    for j in range(kw):
                        canvas[:, ch,
                               i * dh:i * dh + sh * h_in:sh,
                               j * dw:j * dw + sw * w_in:sw] += r[:, mi, i, j]
        out = canvas[:, :, pt:pt + h_out, pl:pl + w_out]
    if b is not None:
        out = _as_f32(out + b.reshape(1, -1, 1, 1))
    return _as_f32(out)


def _gru(x, w, r, b, init_h, hidden, direction, lbr):
    x = _as_f32(x); w = _as_f32(w); r = _as_f32(r)
    h = hidden
    num_dir = w.shape[0]
    seq, batch, _ = x.shape
    if init_h is None:
        init_h = np.zeros((num_dir, batch, h), F32)
    dirs = [0, 1] if direction == 2 else [0]
    ys = np.zeros((seq, num_dir, batch, h), F32)
    yh = np.zeros((num_dir, batch, h), F32)
    for d_idx, w_idx in enumerate(dirs):
        wd, rd = w[w_idx], r[w_idx]
        if b is not None:
            wb, rb = b[w_idx][:3 * h], b[w_idx][3 * h:]
        else:
            wb = np.zeros(3 * h, F32); rb = np.zeros(3 * h, F32)
        state = _as_f32(init_h[w_idx])
        reverse = direction == 1 or (direction == 2 and w_idx == 1)
        order = range(seq - 1, -1, -1) if reverse else range(seq)
        xproj = _as_f32(np.matmul(x, np.transpose(wd)))
        for t in order:
            xt = xproj[t]
            rproj = _as_f32(np.matmul(state, np.transpose(rd)))
            zt = _sigmoid(_as_f32(_as_f32(xt[:, :h] + rproj[:, :h]) + _as_f32(wb[:h] + rb[:h])))
            rt = _sigmoid(_as_f32(_as_f32(xt[:, h:2 * h] + rproj[:, h:2 * h]) + _as_f32(wb[h:2 * h] + rb[h:2 * h])))
            if lbr:
                ht = _as_f32(np.tanh(_as_f32(_as_f32(xt[:, 2 * h:] + wb[2 * h:]) +
                                             _as_f32(rt * _as_f32(rproj[:, 2 * h:] + rb[2 * h:])))))
            else:
                ht = _as_f32(np.tanh(_as_f32(_as_f32(xt[:, 2 * h:] + _as_f32(rt * rproj[:, 2 * h:])) +
                                             _as_f32(wb[2 * h:] + rb[2 * h:]))))
            state = _as_f32(_as_f32(_as_f32(1.0 - zt) * ht) + _as_f32(zt * state))
            ys[t, d_idx] = state
        yh[d_idx] = state
    return [ys, yh]
