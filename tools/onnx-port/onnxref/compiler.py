"""Static compiler: ONNX model → flat MCYI blob for the pure-Rust VM.

Methodology (mirrors the silero_v4 pure-Rust port):
  1. trace the graph once with the numpy reference interpreter using dummy
     inputs — every shape in these graphs is static, so this yields exact
     shapes for every tensor and exact values for every constant tensor;
  2. constant-fold: initializers, Constant nodes, and every node whose
     inputs are all constant (Shape counts as constant because input shapes
     are static) collapse into compile-time values; all structural op
     parameters (Reshape targets, Slice ranges, Pad widths, Resize sizes,
     Gather indices, …) become immediate attributes — the VM never sees
     int64 tensors;
  3. alias away zero-cost metadata ops (Reshape/Squeeze/Unsqueeze/Identity
     become slot aliases sharing storage);
  4. drop dead ops, pack live ranges into a single f32 arena;
  5. serialize: header + slot table + op table + f32 constant blob.

The Rust VM (crates/micyou-infer) is a mechanical transcription of the
op kernels validated here; replay.py re-executes the blob in numpy to
prove the compiled artifact is faithful to the interpreter.
"""

from __future__ import annotations

import hashlib
import json
import struct
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

import numpy as np
import onnx

from .interpreter import Interpreter, OnnxGraph, attr_dict

MAGIC = b"MCYI"
VERSION = 1

# Slot kinds
SLOT_INPUT = 0
SLOT_CONST = 1
SLOT_ARENA = 2
SLOT_ALIAS = 3

NULL_SLOT = 0xFFFFFFFF

# Opcode table — keep in sync with crates/micyou-infer/src/lib.rs
OPS = {
    "ADD": 1,
    "SUB": 2,
    "MUL": 3,
    "DIV": 4,
    "POW": 5,
    "SIGMOID": 6,
    "SQRT": 7,
    "LOG": 8,
    "CLIP": 9,
    "MATMUL": 10,
    "CONV": 11,
    "CONV_T": 12,
    "GRU": 13,
    "BATCH_NORM": 14,
    "LAYER_NORM": 15,
    "TRANSPOSE": 16,
    "CONCAT": 17,
    "SLICE": 18,
    "PAD": 19,
    "EXPAND": 20,
    "GATHER": 21,
    "REDUCE_MEAN": 22,
    "REDUCE_L2": 23,
    "RESIZE": 24,
    "COPY": 25,
}

DIRECTION_ENUM = {"forward": 0, "backward": 1, "bidirectional": 2}


@dataclass
class SlotRec:
    id: int
    kind: int
    shape: Tuple[int, ...]
    off: int = 0            # const: f32 element offset; alias: target slot id
    name: str = ""
    numel: int = 0


@dataclass
class OpRec:
    code: int
    name: str
    ins: List[int] = field(default_factory=list)
    outs: List[int] = field(default_factory=list)
    i64: List[int] = field(default_factory=list)
    f32: List[float] = field(default_factory=list)


class CompiledGraph:
    def __init__(self, name: str):
        self.name = name
        self.slots: List[SlotRec] = []
        self.ops: List[OpRec] = []
        self.inputs: List[int] = []
        self.outputs: List[int] = []
        self.const_data = np.zeros(0, np.float32)
        self.arena_len = 0
        self.report: Dict[str, object] = {}

    # ── serialization ───────────────────────────────────────────────────

    def to_bytes(self) -> bytes:
        out = bytearray()
        out += MAGIC
        out += struct.pack("<IIIII", VERSION, len(self.slots), len(self.ops),
                           len(self.inputs), len(self.outputs))
        out += struct.pack("<II", self.arena_len, self.const_data.size)
        for s in self.inputs:
            out += struct.pack("<I", s)
        for s in self.outputs:
            out += struct.pack("<I", s)
        # slot table: kind u8, ndim u8, pad u16, off u32, dims…
        slot_bytes = bytearray()
        for s in self.slots:
            slot_bytes += struct.pack("<BBHi", s.kind, len(s.shape), 0, s.off)
            for d in s.shape:
                slot_bytes += struct.pack("<i", d)
        out += struct.pack("<I", len(slot_bytes)) + slot_bytes
        op_bytes = bytearray()
        for op in self.ops:
            op_bytes += struct.pack("<HHHHH", op.code, len(op.i64), len(op.ins),
                                    len(op.outs), len(op.f32))
            for i in op.ins:
                op_bytes += struct.pack("<I", i)
            for i in op.outs:
                op_bytes += struct.pack("<I", i)
            for v in op.i64:
                op_bytes += struct.pack("<q", v)
            for v in op.f32:
                op_bytes += struct.pack("<f", v)
        out += struct.pack("<I", len(op_bytes)) + op_bytes
        out += self.const_data.astype("<f4").tobytes()
        return bytes(out)

    def debug_json(self) -> str:
        return json.dumps(
            {
                "name": self.name,
                "inputs": self.inputs,
                "outputs": self.outputs,
                "arena_len": self.arena_len,
                "const_len": int(self.const_data.size),
                "slots": [
                    {
                        "id": s.id, "kind": s.kind, "shape": list(s.shape),
                        "off": s.off, "name": s.name,
                    }
                    for s in self.slots
                ],
                "ops": [
                    {
                        "code": op.code, "name": op.name, "ins": op.ins,
                        "outs": op.outs, "i64": op.i64, "f32": op.f32,
                    }
                    for op in self.ops
                ],
            },
            indent=1,
        )


class CompileError(RuntimeError):
    pass


def compile_model(path: str, name: Optional[str] = None) -> CompiledGraph:
    model = onnx.load(path)
    graph = OnnxGraph(model)
    name = name or path.rsplit("/", 1)[-1]
    cg = CompiledGraph(name)

    input_names = graph.input_names
    output_names = graph.output_names
    input_shapes = {}
    for vi in model.graph.input:
        dims = [d.dim_value for d in vi.type.tensor_type.shape.dim]
        if any(d <= 0 for d in dims):
            raise CompileError(f"dynamic input shape on {vi.name}")
        input_shapes[vi.name] = tuple(dims)

    # ── 1. trace: shapes for every tensor, values for constant tensors ──
    interp = Interpreter(graph)
    rng = np.random.default_rng(20260927)
    feeds = {
        n: (rng.standard_normal(input_shapes[n]) * 0.05).astype(np.float32)
        for n in input_names
    }
    interp.run(feeds)
    traced: Dict[str, np.ndarray] = interp.values  # every tensor name → value

    shapes: Dict[str, Tuple[int, ...]] = {
        k: tuple(np.asarray(v).shape) for k, v in traced.items()
    }

    # ── 2. constant classification ──────────────────────────────────────
    is_const: Dict[str, bool] = {name: True for name in graph.initializers}
    for n in input_names:
        is_const[n] = False
    runtime_nodes: List[Tuple[int, onnx.NodeProto]] = []
    for idx, n in enumerate(graph.nodes):
        if n.op_type in ("Constant", "Shape"):
            for o in n.output:
                is_const[o] = True
            continue
        ins = [i for i in n.input if i != ""]
        if all(is_const.get(i, False) for i in ins):
            for o in n.output:
                is_const[o] = True
            continue
        for o in n.output:
            is_const[o] = False
        runtime_nodes.append((idx, n))

    # sanity: every runtime tensor must be float32
    for tname, is_c in is_const.items():
        arr = np.asarray(traced[tname])
        if not is_c and arr.dtype != np.float32:
            raise CompileError(f"runtime tensor {tname} has dtype {arr.dtype}")

    # ── 3. slot assignment ──────────────────────────────────────────────
    slot_of: Dict[str, int] = {}

    def new_slot(kind: int, shape: Tuple[int, ...], tname: str) -> int:
        numel = int(np.prod(shape, dtype=np.int64)) if len(shape) else 1
        rec = SlotRec(id=len(cg.slots), kind=kind, shape=shape, name=tname, numel=numel)
        cg.slots.append(rec)
        return rec.id

    const_blob: List[np.ndarray] = []
    const_index: Dict[str, int] = {}   # content hash → offset
    blob_len = 0

    def add_const(arr: np.ndarray) -> int:
        nonlocal blob_len
        arr = np.ascontiguousarray(arr.astype(np.float32)).reshape(-1)
        key = hashlib.sha256(arr.tobytes()).hexdigest()
        if key in const_index:
            return const_index[key]
        off = blob_len
        const_blob.append(arr)
        blob_len += arr.size
        const_index[key] = off
        return off

    def ensure_slot(tname: str) -> int:
        """Materialize a slot for a tensor name referenced by a runtime op."""
        if tname in slot_of:
            return slot_of[tname]
        shape = shapes[tname]
        if tname in input_shapes:
            sid = new_slot(SLOT_INPUT, shape, tname)
        elif is_const.get(tname):
            arr = np.asarray(traced[tname])
            if arr.dtype != np.float32:
                raise CompileError(
                    f"const tensor {tname} dtype {arr.dtype} consumed at runtime"
                )
            off = add_const(arr)
            sid = new_slot(SLOT_CONST, shape, tname)
            cg.slots[sid].off = off
        else:
            sid = new_slot(SLOT_ARENA, shape, tname)
        slot_of[tname] = sid
        return sid

    for n in input_names:
        ensure_slot(n)
    cg.inputs = [slot_of[n] for n in input_names]

    ALIAS_OPS = {"Reshape", "Squeeze", "Unsqueeze", "Identity", "Flatten"}

    def alias_root(sid: int) -> int:
        while cg.slots[sid].kind == SLOT_ALIAS:
            sid = cg.slots[sid].off
        return sid

    def make_alias(tname: str, src_name: str) -> int:
        if tname in slot_of:
            return slot_of[tname]
        src = ensure_slot(src_name)
        shape = shapes[tname]
        src_numel = cg.slots[alias_root(src)].numel
        numel = int(np.prod(shape, dtype=np.int64)) if len(shape) else 1
        if src_numel != numel:
            raise CompileError(f"alias numel mismatch {tname}: {src_numel} vs {numel}")
        sid = new_slot(SLOT_ALIAS, shape, tname)
        cg.slots[sid].off = src
        slot_of[tname] = sid
        return sid

    # structural input names that must be constant-folded into attributes
    def const_i64(tname: str) -> List[int]:
        if not is_const.get(tname, False):
            raise CompileError(f"structural input {tname} is not constant")
        return [int(v) for v in np.asarray(traced[tname]).reshape(-1)]

    def const_f32_scalar(tname: str) -> Optional[float]:
        if tname == "" or tname is None:
            return None
        if not is_const.get(tname, False):
            raise CompileError(f"scalar arg {tname} is not constant")
        return float(np.asarray(traced[tname]).reshape(-1)[0])

    # ── 4. emit ops ─────────────────────────────────────────────────────
    emitted: List[OpRec] = []

    def emit(op_type: str, node: onnx.NodeProto, ins: List[int], outs: List[int],
             i64: Optional[List[int]] = None, f32: Optional[List[float]] = None):
        emitted.append(OpRec(code=OPS[op_type], name=node.name or node.op_type,
                             ins=ins, outs=outs, i64=i64 or [], f32=f32 or []))

    for idx, n in runtime_nodes:
        attrs = graph.node_attrs[idx]
        t = n.op_type
        out0 = n.output[0]

        if t in ALIAS_OPS:
            src = [i for i in n.input if i != ""][0]
            make_alias(out0, src)
            continue

        def data_slot(pos: int) -> int:
            """Slot id for a DATA input at `pos` ('' → NULL_SLOT).

            Structural (int64) inputs are never materialized as slots; they
            are folded into op attributes by the per-op branches below.
            """
            nm = n.input[pos] if pos < len(n.input) else ""
            return ensure_slot(nm) if nm != "" else NULL_SLOT

        # structural args must be const — extract before creating out slots
        if t == "Slice":
            nd = len(shapes[n.input[0]])
            starts = const_i64(n.input[1])
            ends = const_i64(n.input[2])
            axes = const_i64(n.input[3]) if len(n.input) > 3 and n.input[3] else list(range(len(starts)))
            steps = const_i64(n.input[4]) if len(n.input) > 4 and n.input[4] else [1] * len(starts)
            norm = []
            for s, e, a, st in zip(starts, ends, axes, steps):
                a = a % nd
                dim = shapes[n.input[0]][a]
                if s < 0:
                    s += dim
                if e < 0:
                    e += dim
                if st > 0:
                    s = min(max(s, 0), dim)
                    e = min(max(e, 0), dim)
                else:
                    # ONNX negative-step semantics: clamped s==-1 → empty;
                    # clamped e==-1 → run through index 0 (inclusive). The VM
                    # applies the same rule; keep the sentinels as-is.
                    s = min(max(s, -1), dim - 1)
                    e = min(max(e, -1), dim - 1)
                    if s == -1:
                        s, e = 0, 0
                norm += [a, s, e, st]
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("SLICE", n, [data_slot(0)], [out], i64=[len(starts)] + norm)
            continue
        if t == "Pad":
            pads = const_i64(n.input[1])
            cv = const_f32_scalar(n.input[2]) if len(n.input) > 2 else None
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("PAD", n, [data_slot(0)], [out], i64=pads, f32=[cv if cv is not None else 0.0])
            continue
        if t == "Expand":
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("EXPAND", n, [data_slot(0)], [out])
            continue
        if t == "Gather":
            gi = const_i64(n.input[1])
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("GATHER", n, [data_slot(0)], [out], i64=[int(attrs.get("axis", 0))] + gi)
            continue
        if t == "Concat":
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            nd_in = len(shapes[n.input[0]])
            axis = int(attrs.get("axis", 0)) % nd_in
            emit("CONCAT", n, [data_slot(i) for i in range(len(n.input))], [out], i64=[axis])
            continue
        if t == "Transpose":
            perm = attrs.get("perm") or list(reversed(range(len(shapes[n.input[0]]))))
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("TRANSPOSE", n, [data_slot(0)], [out], i64=list(perm))
            continue
        if t in ("ReduceMean", "ReduceL2"):
            axes = attrs.get("axes")
            nd = len(shapes[n.input[0]])
            axes = [a % nd for a in axes] if axes else list(range(nd))
            keepdims = int(attrs.get("keepdims", 1))
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            code = "REDUCE_MEAN" if t == "ReduceMean" else "REDUCE_L2"
            emit(code, n, [data_slot(0)], [out], i64=[keepdims, len(axes)] + axes)
            continue
        if t == "Clip":
            lo = const_f32_scalar(n.input[1]) if len(n.input) > 1 else None
            hi = const_f32_scalar(n.input[2]) if len(n.input) > 2 else None
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("CLIP", n, [data_slot(0)], [out],
                 i64=[1 if lo is not None else 0, 1 if hi is not None else 0],
                 f32=[lo if lo is not None else 0.0, hi if hi is not None else 0.0])
            continue
        if t in ("Add", "Sub", "Mul", "Div", "Pow"):
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit({"Add": "ADD", "Sub": "SUB", "Mul": "MUL", "Div": "DIV", "Pow": "POW"}[t],
                 n, [data_slot(0), data_slot(1)], [out])
            continue
        if t in ("Sigmoid", "Sqrt", "Log"):
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit(t.upper(), n, [data_slot(0)], [out])
            continue
        if t == "MatMul":
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("MATMUL", n, [data_slot(0), data_slot(1)], [out])
            continue
        if t in ("Conv", "ConvTranspose"):
            k = attrs.get("kernel_shape")
            wshape = shapes[n.input[1]]
            k = [int(k[0]), int(k[1])] if k else [wshape[2], wshape[3]]
            st = [int(v) for v in attrs.get("strides", [1, 1])]
            dl = [int(v) for v in attrs.get("dilations", [1, 1])]
            pd = [int(v) for v in attrs.get("pads", [0, 0, 0, 0])]
            gp = int(attrs.get("group", 1))
            op_ = [int(v) for v in attrs.get("output_padding", [0, 0])]
            has_bias = 1 if len(n.input) > 2 and n.input[2] != "" else 0
            real_ins = [data_slot(0), data_slot(1), data_slot(2) if has_bias else NULL_SLOT]
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            i64 = k + st + dl + pd + [gp, has_bias] + (op_ if t == "ConvTranspose" else [])
            if attrs.get("auto_pad", "NOTSET") != "NOTSET":
                raise CompileError("auto_pad unsupported")
            emit("CONV" if t == "Conv" else "CONV_T", n, real_ins, [out], i64=i64)
            continue
        if t == "BatchNormalization":
            eps = float(attrs.get("epsilon", 1e-5))
            if int(attrs.get("training_mode", 0)):
                raise CompileError("BN training mode")
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("BATCH_NORM", n, [data_slot(i) for i in range(5)], [out], f32=[eps])
            continue
        if t == "LayerNormalization":
            axis = int(attrs.get("axis", -1))
            eps = float(attrs.get("epsilon", 1e-5))
            nd = len(shapes[n.input[0]])
            axis = axis % nd
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("LAYER_NORM", n, [data_slot(0), data_slot(1), data_slot(2)], [out],
                 i64=[axis], f32=[eps])
            continue
        if t == "GRU":
            hidden = int(attrs["hidden_size"])
            direction = DIRECTION_ENUM[attrs.get("direction", "forward")]
            lbr = int(attrs.get("linear_before_reset", 0))
            if len(n.input) > 4 and n.input[4] != "":
                raise CompileError("GRU sequence_lens unsupported")
            acts = attrs.get("activations")
            if acts:
                raise CompileError("custom GRU activations unsupported")
            b_in = data_slot(3)
            h_in = data_slot(5)
            outs = []
            for o in n.output:
                if o == "":
                    outs.append(NULL_SLOT)
                else:
                    sid = new_slot(SLOT_ARENA, shapes[o], o)
                    slot_of[o] = sid
                    outs.append(sid)
            while len(outs) < 2:
                outs.append(NULL_SLOT)
            emit("GRU", n, [data_slot(0), data_slot(1), data_slot(2), b_in, h_in], outs,
                 i64=[hidden, direction, lbr])
            continue
        if t == "Resize":
            mode = attrs.get("mode", "nearest")
            ctm = attrs.get("coordinate_transformation_mode", "half_pixel")
            if mode != "linear" or ctm != "half_pixel":
                raise CompileError(f"Resize {mode}/{ctm} unsupported")
            if len(n.input) > 2 and n.input[2] != "":
                raise CompileError("scales-driven Resize unsupported (use sizes)")
            out = new_slot(SLOT_ARENA, shapes[out0], out0)
            slot_of[out0] = out
            emit("RESIZE", n, [data_slot(0)], [out])
            continue
        raise CompileError(f"runtime op not emittable: {t} ({n.name})")

    cg.ops = emitted

    # ── 5. model outputs; materialize const-output slots ───────────────
    for o in output_names:
        ensure_slot(o)
    cg.outputs = [slot_of[o] for o in output_names]

    # ── 6. dead-op elimination ──────────────────────────────────────────
    # Alias slots share storage with their root, so liveness is tracked on
    # root ids only.
    live_slots = {alias_root(s) for s in cg.outputs}
    changed = True
    while changed:
        changed = False
        keep: List[OpRec] = []
        for op in reversed(cg.ops):
            real_outs = [alias_root(o) for o in op.outs if o != NULL_SLOT]
            if real_outs and not any(o in live_slots for o in real_outs):
                changed = True
                continue  # drop
            keep.append(op)
            for i in op.ins:
                if i != NULL_SLOT:
                    live_slots.add(alias_root(i))
        cg.ops = list(reversed(keep))

    # ── 6b. materialize pass-through outputs ────────────────────────────
    # A graph output may alias a model INPUT (e.g. aec7 prev2_o = Identity
    # of prev1). The VM must not hand back caller-owned memory as an output,
    # so copy such outputs into dedicated arena slots at the very end.
    for oi, sid in enumerate(cg.outputs):
        root = alias_root(sid)
        if cg.slots[root].kind == SLOT_INPUT:
            shape = cg.slots[sid].shape
            new_sid = new_slot(SLOT_ARENA, shape, cg.slots[sid].name + "_copy")
            emitted_copy = OpRec(code=OPS["COPY"], name=f"__copy_out_{oi}",
                                 ins=[sid], outs=[new_sid])
            cg.ops.append(emitted_copy)
            cg.outputs[oi] = new_sid

    # ── 7. arena packing with live-range reuse ─────────────────────────
    n_ops = len(cg.ops)
    first_def: Dict[int, int] = {}
    last_use: Dict[int, int] = {}
    for oi, op in enumerate(cg.ops):
        for i in op.ins:
            if i != NULL_SLOT:
                r = alias_root(i)
                last_use[r] = max(last_use.get(r, -1), oi)
                first_def.setdefault(r, oi)
        for o in op.outs:
            if o != NULL_SLOT:
                r = alias_root(o)
                first_def.setdefault(r, oi)
                last_use[r] = max(last_use.get(r, -1), oi)
    for s in cg.inputs:
        r = alias_root(s)
        first_def[r] = 0
        last_use[r] = max(last_use.get(r, -1), n_ops)
    for s in cg.outputs:
        r = alias_root(s)
        if r in first_def:
            last_use[r] = n_ops

    units = []
    for sid, s in enumerate(cg.slots):
        if s.kind != SLOT_ARENA:
            continue
        root = alias_root(sid)
        if root != sid:
            continue
        if sid not in first_def:
            continue  # never written (dead)
        units.append((s.numel, sid, first_def[sid], last_use.get(sid, n_ops)))

    # merge alias groups' intervals into root
    for sid, s in enumerate(cg.slots):
        if s.kind == SLOT_ALIAS:
            root = alias_root(sid)
            # root interval already covers via slot refs above (alias_root used)
            pass

    units.sort(key=lambda u: (-u[0], u[1]))
    placed: List[Tuple[int, int, int, int]] = []  # off, size, start, end
    ALIGN = 8  # f32 elements (32 bytes)

    def fits(off: int, size: int, start: int, end: int) -> bool:
        for po, ps, pstart, pend in placed:
            if start <= pend and pstart <= end:
                if off < po + ps and po < off + size:
                    return False
        return True

    arena_len = 0
    for size, sid, start, end in units:
        off = 0
        while not fits(off, size, start, end):
            # jump past the conflicting block ends
            next_off = None
            for po, ps, pstart, pend in placed:
                if start <= pend and pstart <= end:
                    cand = po + ps
                    cand = (cand + ALIGN - 1) // ALIGN * ALIGN
                    if cand > off and (next_off is None or cand < next_off):
                        next_off = cand
            off = next_off if next_off is not None else off + ALIGN
            off = (off + ALIGN - 1) // ALIGN * ALIGN
        placed.append((off, size, start, end))
        cg.slots[sid].off = off
        arena_len = max(arena_len, off + size)
    cg.arena_len = arena_len

    cg.const_data = (
        np.concatenate(const_blob) if const_blob else np.zeros(0, np.float32)
    )

    # ── 8. report ───────────────────────────────────────────────────────
    op_hist: Dict[str, int] = {}
    inv = {v: k for k, v in OPS.items()}
    for op in cg.ops:
        op_hist[inv[op.code]] = op_hist.get(inv[op.code], 0) + 1
    cg.report = {
        "model": name,
        "onnx_nodes": len(graph.nodes),
        "runtime_nodes": len(runtime_nodes),
        "emitted_ops": len(cg.ops),
        "slots": len(cg.slots),
        "arena_f32": arena_len,
        "arena_bytes": arena_len * 4,
        "const_f32": int(cg.const_data.size),
        "const_bytes": int(cg.const_data.size * 4),
        "blob_bytes": len(cg.to_bytes()),
        "op_hist": dict(sorted(op_hist.items(), key=lambda x: -x[1])),
    }
    return cg
