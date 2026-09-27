"""Generate golden fixtures for the Rust VM tests.

Two families:
  1. e2e streaming goldens per model — the numpy reference interpreter runs
     N frames with self-feedback (exactly like the Rust runtime), recording
     frame inputs, full enhanced outputs, strided cache samples per frame
     and the FULL final-frame state.
  2. per-op goldens — every distinct op instance of the compiled graphs is
     extracted into a single-op mini blob (real constant weights kept), fed
     random activations and executed by the numpy replay to produce the
     expected output. Plus synthetic edge cases (negative-step slices,
     expand, backward GRU, im2col conv, shrink resize, …).

Fixtures land in crates/micyou-infer/tests/fixtures/ and are consumed by
`cargo test` (micyou-infer op/e2e tests, micyou-audio integration tests).

Usage:
    python3 gen_golden.py            # regenerate
    python3 gen_golden.py --check    # regenerate into tmp and compare with
                                     # committed fixtures (tolerant for f32
                                     # noise, strict for structure)
"""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import sys
import tempfile

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import onnx  # noqa: E402

from onnxref.compiler import (  # noqa: E402
    NULL_SLOT, OPS, SLOT_ARENA, SLOT_CONST, SLOT_INPUT, CompiledGraph, OpRec, SlotRec,
)
from onnxref.interpreter import Interpreter, OnnxGraph  # noqa: E402
from onnxref.replay import BlobGraph, run_blob  # noqa: E402
from parity_ort import MODELS, REPO_ROOT, feedback_map  # noqa: E402

ASSET_DIR = os.path.join(REPO_ROOT, "crates", "micyou-audio", "src", "assets")
FIXTURE_DIR = os.path.join(REPO_ROOT, "crates", "micyou-infer", "tests", "fixtures")
BLOB_NAME = {"purevox6": "purevox6.mcy", "aec7": "aec7.mcy"}
E2E_FRAMES = 16
CACHE_SAMPLES = 512
OP_DATA_CAP = 8192  # floats per case (in+out) to keep the repo light
MAX_PER_OPCODE = 10  # distinct real instances kept per opcode

INV_OPS = {v: k for k, v in OPS.items()}


# ── helpers ────────────────────────────────────────────────────────────────

def sample_plan(numel: int, budget: int):
    stride = max(1, math.ceil(numel / budget))
    idxs = list(range(0, numel, stride))
    if numel > 0 and idxs[-1] != numel - 1:
        idxs.append(numel - 1)
    return stride, idxs


def random_for(op: str, shape, seed: int, pos: int = 0) -> np.ndarray:
    rng = np.random.default_rng(seed)
    if op in ("LOG",):
        return (rng.uniform(0.05, 5.0, shape)).astype(np.float32)
    if op in ("SQRT",):
        return (rng.uniform(0.0, 4.0, shape)).astype(np.float32)
    if op == "DIV" and pos > 0:
        return (rng.uniform(0.5, 2.0, shape)).astype(np.float32)
    return (rng.standard_normal(shape) * 0.5).astype(np.float32)


# ── e2e goldens ────────────────────────────────────────────────────────────

def gen_e2e(name: str, outdir: str):
    cfg = MODELS[name]
    model = onnx.load(cfg["path"])
    in_names = [i.name for i in model.graph.input]
    out_names = [o.name for o in model.graph.output]
    shapes = {
        i.name: tuple(d.dim_value for d in i.type.tensor_type.shape.dim)
        for i in model.graph.input
    }
    for o in model.graph.output:
        shapes[o.name] = tuple(d.dim_value for d in o.type.tensor_type.shape.dim)
    graph = OnnxGraph(model)
    interp = Interpreter(graph)
    fb = feedback_map(in_names, out_names)

    frame_inputs = cfg["frame_inputs"]
    in_lens = [int(np.prod(shapes[n])) for n in in_names]
    out_lens = [int(np.prod(shapes[n])) for n in out_names]

    rng = np.random.default_rng(20260927)
    state = {n: np.zeros(shapes[n], np.float32) for n in in_names}

    # per-frame recorded regions: frame inputs (full), non-cache outputs
    # (full), cache outputs (strided samples). Final block: all non-empty
    # outputs full.
    frame_in_idx = [in_names.index(n) for n in frame_inputs]
    state_outs = [i for i, n in enumerate(out_names) if n in fb]
    primary_outs = [
        i for i, n in enumerate(out_names)
        if n not in fb and out_lens[i] > 0
    ]
    sampled = []
    for i in state_outs:
        stride, idxs = sample_plan(out_lens[i], CACHE_SAMPLES)
        sampled.append({"idx": i, "stride": stride, "n": len(idxs), "offsets": idxs})
    final_idx = [i for i in range(len(out_names)) if out_lens[i] > 0]
    feedback = [[out_names.index(o), in_names.index(i)] for o, i in fb.items()]

    bin_parts = []
    last = None
    for _f in range(E2E_FRAMES):
        for fi in frame_inputs:
            state[fi] = (rng.standard_normal(shapes[fi]) * cfg["scale"]).astype(np.float32)
        for ii in frame_in_idx:
            bin_parts.append(state[in_names[ii]].astype("<f4").tobytes())
        out = interp.run({n: state[n] for n in in_names})
        for oi in primary_outs:
            bin_parts.append(np.asarray(out[out_names[oi]], np.float32).astype("<f4").tobytes())
        for s, oi in zip(sampled, state_outs):
            flat = np.asarray(out[out_names[oi]], np.float32).reshape(-1)
            bin_parts.append(flat[s["offsets"]].astype("<f4").tobytes())
        for oname, iname in fb.items():
            state[iname] = np.asarray(out[oname], np.float32).reshape(shapes[iname])
        last = out
    for oi in final_idx:
        bin_parts.append(np.asarray(last[out_names[oi]], np.float32).astype("<f4").tobytes())

    meta = {
        "model": name,
        "frames": E2E_FRAMES,
        "inputs": [{"name": n, "shape": list(shapes[n]), "len": in_lens[i],
                    "is_frame": i in frame_in_idx} for i, n in enumerate(in_names)],
        "outputs": [{"name": n, "shape": list(shapes[n]), "len": out_lens[i]}
                    for i, n in enumerate(out_names)],
        "feedback": feedback,
        "frame_input_idxs": frame_in_idx,
        "primary_out_idxs": primary_outs,
        "sampled_outputs": [{"idx": s["idx"], "stride": s["stride"], "n": s["n"]} for s in sampled],
        "sampled_offsets": [s["offsets"] for s in sampled],
        "final_output_idxs": final_idx,
    }
    os.makedirs(outdir, exist_ok=True)
    with open(os.path.join(outdir, f"e2e_{name}.bin"), "wb") as fh:
        fh.write(b"".join(bin_parts))
    with open(os.path.join(outdir, f"e2e_{name}.json"), "w") as fh:
        json.dump(meta, fh)
    total = sum(len(p) for p in bin_parts)
    print(f"[e2e {name}] {E2E_FRAMES} frames → {total/1e6:.2f} MB")


# ── per-op goldens ─────────────────────────────────────────────────────────

def mini_blob(op_code, i64, f32, in_specs, out_shape):
    """in_specs: list of (shape, const_data|None). None entries for NULL
    inputs are represented by shape=None."""
    cg = CompiledGraph("mini")
    const_parts = []
    const_len = 0
    in_ids = []
    input_ids = []
    for shape, cdata in in_specs:
        if shape is None:
            in_ids.append(NULL_SLOT)
            continue
        sid = len(cg.slots)
        numel = int(np.prod(shape)) if len(shape) else 1
        if cdata is not None:
            cg.slots.append(SlotRec(id=sid, kind=SLOT_CONST, shape=tuple(shape),
                                    off=const_len, numel=numel))
            const_parts.append(np.ascontiguousarray(cdata, dtype="<f4").reshape(-1))
            const_len += numel
        else:
            cg.slots.append(SlotRec(id=sid, kind=SLOT_INPUT, shape=tuple(shape),
                                    off=0, numel=numel))
            input_ids.append(sid)
        in_ids.append(sid)
    out_id = len(cg.slots)
    out_numel = int(np.prod(out_shape)) if len(out_shape) else 1
    cg.slots.append(SlotRec(id=out_id, kind=SLOT_ARENA, shape=tuple(out_shape),
                            off=0, numel=out_numel))
    cg.ops = [OpRec(code=op_code, name="mini", ins=in_ids, outs=[out_id],
                    i64=list(i64), f32=list(f32))]
    cg.inputs = input_ids
    cg.outputs = [out_id]
    cg.arena_len = out_numel
    cg.const_data = (
        np.concatenate(const_parts) if const_parts else np.zeros(0, np.float32)
    )
    return cg


def gru_mini(op_code, i64, f32, in_specs, out_shapes):
    """Two-output mini blob for GRU (Y may be NULL)."""
    cg = CompiledGraph("mini")
    const_parts = []
    const_len = 0
    in_ids = []
    input_ids = []
    for shape, cdata in in_specs:
        if shape is None:
            in_ids.append(NULL_SLOT)
            continue
        sid = len(cg.slots)
        numel = int(np.prod(shape)) if len(shape) else 1
        if cdata is not None:
            cg.slots.append(SlotRec(id=sid, kind=SLOT_CONST, shape=tuple(shape),
                                    off=const_len, numel=numel))
            const_parts.append(np.ascontiguousarray(cdata, dtype="<f4").reshape(-1))
            const_len += numel
        else:
            cg.slots.append(SlotRec(id=sid, kind=SLOT_INPUT, shape=tuple(shape),
                                    off=0, numel=numel))
            input_ids.append(sid)
        in_ids.append(sid)
    outs = []
    arena = 0
    for oshape in out_shapes:
        if oshape is None:
            outs.append(NULL_SLOT)
            continue
        sid = len(cg.slots)
        numel = int(np.prod(oshape)) if len(oshape) else 1
        cg.slots.append(SlotRec(id=sid, kind=SLOT_ARENA, shape=tuple(oshape),
                                off=arena, numel=numel))
        arena += numel
        outs.append(sid)
    cg.ops = [OpRec(code=op_code, name="mini", ins=in_ids, outs=outs,
                    i64=list(i64), f32=list(f32))]
    cg.inputs = input_ids
    # expose every live output as a graph output (Y first, then Y_h)
    cg.outputs = [o for o in outs if o != NULL_SLOT]
    cg.arena_len = arena
    cg.const_data = (
        np.concatenate(const_parts) if const_parts else np.zeros(0, np.float32)
    )
    return cg


def extract_real_ops(outdir: str):
    os.makedirs(os.path.join(outdir, "ops"), exist_ok=True)
    index = []
    seen = set()
    per_op = {}
    n_skipped = 0
    for model_name in ("purevox6", "aec7"):
        blob = BlobGraph(open(os.path.join(ASSET_DIR, BLOB_NAME[model_name]), "rb").read())
        for k, (code, ins, outs, i64, f32) in enumerate(blob.ops):
            opname = INV_OPS[code]
            # dedupe on semantics-relevant signature
            in_sigs = []
            for s in ins:
                if s == NULL_SLOT:
                    in_sigs.append(None)
                else:
                    in_sigs.append((blob.slot_kind[s], tuple(blob.slot_shape[s])))
            key = (opname, tuple(i64), tuple(round(v, 6) for v in f32),
                   tuple(in_sigs), tuple(blob.slot_shape[outs[0]]) if outs and outs[0] != NULL_SLOT else None)
            if key in seen:
                continue
            seen.add(key)
            # build mini in_specs + feed data
            in_specs = []
            feeds = []
            total = 0
            skip = False
            seed = 5000 + k
            for pos, s in enumerate(ins):
                if s == NULL_SLOT:
                    in_specs.append((None, None))
                    continue
                shape = tuple(blob.slot_shape[s])
                numel = int(np.prod(shape)) if len(shape) else 1
                kind = blob.slot_kind[s]
                if kind == 1:  # CONST → keep real weights
                    off = blob.slot_off[s]
                    data = np.array(blob.const_data[off:off + numel]).reshape(shape)
                    in_specs.append((shape, data))
                    total += numel
                else:  # INPUT/ARENA/ALIAS → random activation
                    in_specs.append((shape, None))
                    feeds.append(random_for(opname, shape, seed + pos, pos))
                    total += numel
            out_shape = None
            for o in outs:
                if o != NULL_SLOT:
                    out_shape = tuple(blob.slot_shape[o])
                    total += int(np.prod(out_shape)) if len(out_shape) else 1
            if out_shape is None:
                continue  # op without live output (shouldn't happen post-DCE)
            if total > OP_DATA_CAP or per_op.get(opname, 0) >= MAX_PER_OPCODE:
                n_skipped += 1
                continue
            per_op[opname] = per_op.get(opname, 0) + 1
            if opname == "GRU":
                cg = gru_mini(code, i64, f32, in_specs,
                              [tuple(blob.slot_shape[o]) if o != NULL_SLOT else None for o in outs])
            else:
                cg = mini_blob(code, i64, f32, in_specs, out_shape)
            case = f"{model_name}_{k:04d}_{opname.lower()}"
            blob_bytes = cg.to_bytes()
            mini = BlobGraph(blob_bytes)
            results = run_blob(mini, feeds)
            # for ops with a NULL first output (GRU Y dropped), outs list may
            # contain NULL — mini_blob only supports single-output ops; GRU
            # instances always keep Y_h as outs[0] after NULL filtering below
            data = b"".join(f.astype("<f4").tobytes() for f in feeds) + \
                   b"".join(np.asarray(r, np.float32).astype("<f4").tobytes() for r in results)
            with open(os.path.join(outdir, "ops", case + ".mcy"), "wb") as fh:
                fh.write(blob_bytes)
            with open(os.path.join(outdir, "ops", case + ".bin"), "wb") as fh:
                fh.write(data)
            index.append({
                "name": case,
                "op": opname,
                "in_lens": [int(f.size) for f in feeds],
                "out_lens": [int(np.asarray(r).size) for r in results],
            })
    print(f"[ops] {len(index)} real cases, {n_skipped} skipped (>{OP_DATA_CAP} floats)")
    return index


# ── synthetic edge cases ───────────────────────────────────────────────────

def synthetic_cases(outdir: str):
    os.makedirs(os.path.join(outdir, "ops"), exist_ok=True)
    index = []
    rng = np.random.default_rng(777)

    def emit_case(name, cg, feeds, out_lens):
        blob_bytes = cg.to_bytes()
        mini = BlobGraph(blob_bytes)
        results = run_blob(mini, feeds)
        data = b"".join(f.astype("<f4").tobytes() for f in feeds) + \
               b"".join(np.asarray(r, np.float32).astype("<f4").tobytes() for r in results)
        with open(os.path.join(outdir, "ops", name + ".mcy"), "wb") as fh:
            fh.write(blob_bytes)
        with open(os.path.join(outdir, "ops", name + ".bin"), "wb") as fh:
            fh.write(data)
        index.append({
            "name": name,
            "op": INV_OPS[cg.ops[0].code],
            "in_lens": [int(f.size) for f in feeds],
            "out_lens": [int(np.asarray(r).size) for r in results],
        })

    def f32arr(x):
        return np.ascontiguousarray(x, dtype=np.float32)

    # 1. negative-step full reverse + partial + empty slices
    x = f32arr(np.arange(2 * 5 * 7, dtype=np.float32).reshape(2, 5, 7))
    # reverse axis1: starts=[4], ends=[-1] (through 0), steps=[-1]
    cg = mini_blob(OPS["SLICE"], [1, 1, 4, -1, -1], [], [(x.shape, None)], (2, 5, 7))
    emit_case("syn_slice_reverse", cg, [x], [x.size])
    # partial negative step: axis2 start 6 end 1 step -2 → idx 6,4,2
    cg = mini_blob(OPS["SLICE"], [1, 2, 6, 1, -2], [], [(x.shape, None)], (2, 5, 3))
    emit_case("syn_slice_neg_partial", cg, [x], [2 * 5 * 3])
    # empty selection sentinel (0,0)
    cg = mini_blob(OPS["SLICE"], [1, 0, 0, 0, -1], [], [(x.shape, None)], (0, 5, 7))
    emit_case("syn_slice_empty", cg, [x], [0])
    # positive multi-axis slice: axis0 0:2, axis1 1:4 → (2,3,7)
    cg = mini_blob(OPS["SLICE"], [2, 0, 0, 2, 1, 1, 1, 4, 1], [], [(x.shape, None)], (2, 3, 7))
    emit_case("syn_slice_pos_multi", cg, [x], [2 * 3 * 7])

    # 2. expand
    a = f32arr(rng.standard_normal((1, 3, 1)))
    cg = mini_blob(OPS["EXPAND"], [], [], [(a.shape, None)], (4, 3, 5))
    emit_case("syn_expand", cg, [a], [4 * 3 * 5])
    a2 = f32arr(rng.standard_normal((1,)))
    cg = mini_blob(OPS["EXPAND"], [], [], [((1,), None)], (2, 2))
    emit_case("syn_expand_scalar", cg, [a2], [4])

    # 3. gather axis 0 with negative index; axis 1
    g = f32arr(rng.standard_normal((4, 3, 2)))
    cg = mini_blob(OPS["GATHER"], [0, -1, 2], [], [(g.shape, None)], (2, 3, 2))
    emit_case("syn_gather_neg", cg, [g], [2 * 3 * 2])
    cg = mini_blob(OPS["GATHER"], [1, 0], [], [(g.shape, None)], (4, 1, 2))
    emit_case("syn_gather_axis1_scalar_idx", cg, [g], [4 * 1 * 2])

    # 4. matmul 2D×2D and 4D batch
    A = f32arr(rng.standard_normal((5, 4)))
    B = f32arr(rng.standard_normal((4, 3)))
    cg = mini_blob(OPS["MATMUL"], [], [], [(A.shape, None), (B.shape, None)], (5, 3))
    emit_case("syn_matmul_2d", cg, [A, B], [15])
    A4 = f32arr(rng.standard_normal((1, 1, 1, 6)))
    B4 = f32arr(rng.standard_normal((6, 2)))
    cg = mini_blob(OPS["MATMUL"], [], [], [(A4.shape, None), (B4.shape, None)], (1, 1, 1, 2))
    emit_case("syn_matmul_4d", cg, [A4, B4], [2])
    A3 = f32arr(rng.standard_normal((3, 2, 4)))
    cg = mini_blob(OPS["MATMUL"], [], [], [(A3.shape, None), (B.shape, None)], (3, 2, 3))
    emit_case("syn_matmul_batch", cg, [A3, B], [3 * 2 * 3])

    # 5. GRU variants: backward-only, no bias, no h0, bidirectional
    seq, batch, ind, h = 3, 1, 4, 5
    X = f32arr(rng.standard_normal((seq, batch, ind)))
    Wf = f32arr(rng.standard_normal((1, 3 * h, ind)) * 0.3)
    Rf = f32arr(rng.standard_normal((1, 3 * h, h)) * 0.3)
    Bf = f32arr(rng.standard_normal((1, 6 * h)) * 0.1)
    H0 = f32arr(rng.standard_normal((1, batch, h)) * 0.2)
    # backward-only (direction=1), lbr=1
    cg = mini_blob(OPS["GRU"], [h, 1, 1], [],
                   [(X.shape, None), (Wf.shape, Wf), (Rf.shape, Rf), (Bf.shape, Bf),
                    (None, None), (H0.shape, None)], (1, batch, h))
    # note: mini_blob single output; GRU outs in blob are [Y, Yh] — build manually:
    cg2 = CompiledGraph("mini")
    ids = []
    for shape, cdata in [(X.shape, None), (Wf.shape, Wf), (Rf.shape, Rf), (Bf.shape, Bf), (H0.shape, None)]:
        if shape is None:
            ids.append(NULL_SLOT)
            continue
        sid = len(cg2.slots)
        numel = int(np.prod(shape))
        if cdata is not None:
            cg2.slots.append(SlotRec(id=sid, kind=SLOT_CONST, shape=tuple(shape), off=0, numel=numel))
        else:
            cg2.slots.append(SlotRec(id=sid, kind=SLOT_INPUT, shape=tuple(shape), off=0, numel=numel))
        ids.append(sid)
    # const data concat for W, R, B
    cg2.const_data = np.concatenate([Wf.reshape(-1), Rf.reshape(-1), Bf.reshape(-1)])
    offs = [0, Wf.size, Wf.size + Rf.size]
    ci = 0
    for sid in ids:
        if sid != NULL_SLOT and cg2.slots[sid].kind == SLOT_CONST:
            cg2.slots[sid].off = offs[ci]
            ci += 1
    y_id = len(cg2.slots)
    cg2.slots.append(SlotRec(id=y_id, kind=SLOT_ARENA, shape=(seq, 1, batch, h), off=0, numel=seq * batch * h))
    yh_id = len(cg2.slots)
    cg2.slots.append(SlotRec(id=yh_id, kind=SLOT_ARENA, shape=(1, batch, h), off=seq * batch * h, numel=batch * h))
    cg2.ops = [OpRec(code=OPS["GRU"], name="gru", ins=ids, outs=[y_id, yh_id], i64=[h, 1, 1], f32=[])]
    cg2.inputs = [ids[0], ids[4]]
    cg2.outputs = [yh_id]
    cg2.arena_len = seq * batch * h + batch * h
    emit_case("syn_gru_backward_bias_h0", cg2, [X, H0], [batch * h])

    # bidirectional with bias, no h0
    Wb = f32arr(rng.standard_normal((2, 3 * h, ind)) * 0.3)
    Rb = f32arr(rng.standard_normal((2, 3 * h, h)) * 0.3)
    Bb = f32arr(rng.standard_normal((2, 6 * h)) * 0.1)
    cg3 = CompiledGraph("mini")
    ids = []
    for shape, cdata in [(X.shape, None), (Wb.shape, Wb), (Rb.shape, Rb), (Bb.shape, Bb), (None, None)]:
        if shape is None:
            ids.append(NULL_SLOT)
            continue
        sid = len(cg3.slots)
        numel = int(np.prod(shape))
        kind = SLOT_CONST if cdata is not None else SLOT_INPUT
        cg3.slots.append(SlotRec(id=sid, kind=kind, shape=tuple(shape), off=0, numel=numel))
        ids.append(sid)
    cg3.const_data = np.concatenate([Wb.reshape(-1), Rb.reshape(-1), Bb.reshape(-1)])
    offs = [0, Wb.size, Wb.size + Rb.size]
    ci = 0
    for sid in ids:
        if sid != NULL_SLOT and cg3.slots[sid].kind == SLOT_CONST:
            cg3.slots[sid].off = offs[ci]
            ci += 1
    yh_id = len(cg3.slots)
    cg3.slots.append(SlotRec(id=yh_id, kind=SLOT_ARENA, shape=(2, batch, h), off=0, numel=2 * batch * h))
    cg3.ops = [OpRec(code=OPS["GRU"], name="gru", ins=ids, outs=[NULL_SLOT, yh_id], i64=[h, 2, 1], f32=[])]
    cg3.inputs = [ids[0]]
    cg3.outputs = [yh_id]
    cg3.arena_len = 2 * batch * h
    emit_case("syn_gru_bidi_noh0", cg3, [X], [2 * batch * h])

    # forward, no bias, no h0
    cg4 = CompiledGraph("mini")
    ids = []
    for shape, cdata in [(X.shape, None), (Wf.shape, Wf), (Rf.shape, Rf), (None, None), (None, None)]:
        if shape is None:
            ids.append(NULL_SLOT)
            continue
        sid = len(cg4.slots)
        numel = int(np.prod(shape))
        kind = SLOT_CONST if cdata is not None else SLOT_INPUT
        cg4.slots.append(SlotRec(id=sid, kind=kind, shape=tuple(shape), off=0, numel=numel))
        ids.append(sid)
    cg4.const_data = np.concatenate([Wf.reshape(-1), Rf.reshape(-1)])
    offs = [0, Wf.size]
    ci = 0
    for sid in ids:
        if sid != NULL_SLOT and cg4.slots[sid].kind == SLOT_CONST:
            cg4.slots[sid].off = offs[ci]
            ci += 1
    y_id = len(cg4.slots)
    cg4.slots.append(SlotRec(id=y_id, kind=SLOT_ARENA, shape=(seq, 1, batch, h), off=0, numel=seq * batch * h))
    cg4.ops = [OpRec(code=OPS["GRU"], name="gru", ins=ids, outs=[y_id, NULL_SLOT], i64=[h, 0, 1], f32=[])]
    cg4.inputs = [ids[0]]
    cg4.outputs = [y_id]
    cg4.arena_len = seq * batch * h
    emit_case("syn_gru_forward_nobias", cg4, [X], [seq * batch * h])

    # 6. binary broadcast patterns
    A = f32arr(rng.standard_normal((1, 4, 1, 6)))
    Bc = f32arr(rng.standard_normal((6,)))
    cg = mini_blob(OPS["MUL"], [], [], [(A.shape, None), (Bc.shape, None)], (1, 4, 1, 6))
    emit_case("syn_mul_chan_broadcast", cg, [A, Bc], [24])
    S = f32arr(np.array([2.0], np.float32))
    cg = mini_blob(OPS["DIV"], [], [], [(A.shape, None), (S.reshape(()).shape, None)], (1, 4, 1, 6))
    emit_case("syn_div_scalar", cg, [A, S.reshape(())], [24])

    # 7. pad rank2 with cv
    P = f32arr(rng.standard_normal((2, 3)))
    cg = mini_blob(OPS["PAD"], [0, 1, 1, 1], [-3.5], [(P.shape, None)], (3, 5))
    emit_case("syn_pad_cv", cg, [P], [15])

    # 8. resize shrink + grow multi-dim
    R = f32arr(rng.standard_normal((1, 1, 4, 6)))
    cg = mini_blob(OPS["RESIZE"], [], [], [(R.shape, None)], (1, 1, 2, 12))
    emit_case("syn_resize_shrink_grow", cg, [R], [24])

    # 9. grouped conv (im2col path): g=2, k=3x3, s=1, pad=1
    XC = f32arr(rng.standard_normal((1, 4, 5, 5)))
    WC = f32arr(rng.standard_normal((6, 2, 3, 3)) * 0.3)
    BC = f32arr(rng.standard_normal((6,)) * 0.1)
    cg = mini_blob(OPS["CONV"], [3, 3, 1, 1, 1, 1, 1, 1, 1, 1, 2, 1], [],
                   [(XC.shape, None), (WC.shape, WC), (BC.shape, BC)], (1, 6, 5, 5))
    emit_case("syn_conv_grouped_im2col", cg, [XC], [6 * 25])
    # strided conv g1 k2x2 s2
    XC2 = f32arr(rng.standard_normal((1, 2, 6, 7)))
    WC2 = f32arr(rng.standard_normal((3, 2, 2, 2)) * 0.3)
    cg = mini_blob(OPS["CONV"], [2, 2, 2, 2, 1, 1, 0, 0, 0, 0, 1, 0], [],
                   [(XC2.shape, None), (WC2.shape, WC2)], (1, 3, 3, 3))
    emit_case("syn_conv_stride_nobias", cg, [XC2], [3 * 9])
    # dilated depthwise
    XD = f32arr(rng.standard_normal((1, 3, 8, 8)))
    WD = f32arr(rng.standard_normal((3, 1, 3, 3)) * 0.3)
    cg = mini_blob(OPS["CONV"], [3, 3, 1, 1, 2, 2, 2, 2, 2, 2, 3, 0], [],
                   [(XD.shape, None), (WD.shape, WD)], (1, 3, 8, 8))
    emit_case("syn_conv_dw_dilated", cg, [XD], [3 * 64])

    # 10. convtranspose with output_padding
    XT = f32arr(rng.standard_normal((1, 4, 2, 3)))
    WT = f32arr(rng.standard_normal((4, 2, 2, 3)) * 0.3)  # [C, M/g, kh, kw], g=4 depthwise-like
    BT = f32arr(rng.standard_normal((8,)) * 0.1)
    # g=4, M=8, k=(2,3), s=(1,2), pads=(0,1,0,1), output_padding=(0,1)
    # h_out = (2-1)*1 - 0 - 0 + 2 + 0 = 3; w_out = (3-1)*2 -1 -1 + 3 + 1 = 6
    cg = mini_blob(OPS["CONV_T"], [2, 3, 1, 2, 1, 1, 0, 1, 0, 1, 4, 1, 0, 1], [],
                   [(XT.shape, None), (WT.shape, WT), (BT.shape, BT)], (1, 8, 3, 6))
    emit_case("syn_convt_outpad", cg, [XT], [8 * 18])

    # 11. reduce variants
    RM = f32arr(rng.standard_normal((2, 3, 4)))
    cg = mini_blob(OPS["REDUCE_MEAN"], [0, 2, 0, 2], [], [(RM.shape, None)], (3,))
    emit_case("syn_reduce_mean_axes02_nk", cg, [RM], [3])
    cg = mini_blob(OPS["REDUCE_MEAN"], [1, 1, 1], [], [(RM.shape, None)], (2, 1, 4))
    emit_case("syn_reduce_mean_axis1_kd", cg, [RM], [8])
    cg = mini_blob(OPS["REDUCE_L2"], [1, 1, 2], [], [(RM.shape, None)], (2, 3, 1))
    emit_case("syn_reduce_l2", cg, [RM], [6])

    # 12. layer norm axis -2 (2-dim normalize)
    LN = f32arr(rng.standard_normal((2, 3, 4)))
    SC = f32arr(rng.standard_normal((3, 4)) * 0.5 + 1.0)
    BI = f32arr(rng.standard_normal((3, 4)) * 0.1)
    cg = mini_blob(OPS["LAYER_NORM"], [1], [1e-5],
                   [(LN.shape, None), (SC.shape, SC), (BI.shape, BI)], (2, 3, 4))
    emit_case("syn_layernorm_axis1", cg, [LN], [24])

    # 13. batchnorm
    BN = f32arr(rng.standard_normal((1, 3, 2, 2)))
    sc = f32arr(rng.standard_normal((3,)) * 0.3 + 1)
    bi = f32arr(rng.standard_normal((3,)) * 0.1)
    mn = f32arr(rng.standard_normal((3,)) * 0.2)
    vr = f32arr(rng.uniform(0.1, 2.0, (3,)))
    cg = mini_blob(OPS["BATCH_NORM"], [], [1e-5],
                   [(BN.shape, None), (sc.shape, sc), (bi.shape, bi), (mn.shape, mn), (vr.shape, vr)],
                   (1, 3, 2, 2))
    emit_case("syn_batchnorm", cg, [BN], [12])

    # 14. transpose 4D
    TR = f32arr(rng.standard_normal((2, 3, 4, 5)))
    cg = mini_blob(OPS["TRANSPOSE"], [0, 2, 3, 1], [], [(TR.shape, None)], (2, 4, 5, 3))
    emit_case("syn_transpose_4d", cg, [TR], [120])

    # 15. copy / clip / pow3 / sigmoid edges / log-sqrt edges
    CP = f32arr(rng.standard_normal((3, 3)))
    cg = mini_blob(OPS["COPY"], [], [], [(CP.shape, None)], (3, 3))
    emit_case("syn_copy", cg, [CP], [9])
    cg = mini_blob(OPS["CLIP"], [1, 0], [-0.5, 0.0], [(CP.shape, None)], (3, 3))
    emit_case("syn_clip_lo", cg, [CP], [9])
    cg = mini_blob(OPS["CLIP"], [1, 1], [0.1, 2.0], [(CP.shape, None)], (3, 3))
    emit_case("syn_clip_both", cg, [CP], [9])
    PW = f32arr(rng.standard_normal((4, 4)))
    E3 = f32arr(np.array([3.0], np.float32).reshape(()))
    cg = mini_blob(OPS["POW"], [], [], [(PW.shape, None), (E3.shape, None)], (4, 4))
    emit_case("syn_pow3", cg, [PW, E3], [16])
    SG = f32arr(np.array([-90.0, -1.0, 0.0, 1.0, 88.0, 12.0], np.float32))
    cg = mini_blob(OPS["SIGMOID"], [], [], [(SG.shape, None)], (6,))
    emit_case("syn_sigmoid_extremes", cg, [SG], [6])
    LG = f32arr(np.array([1e-30, 0.5, 1.0, 3.0, 1e6], np.float32))
    cg = mini_blob(OPS["LOG"], [], [], [(LG.shape, None)], (5,))
    emit_case("syn_log_edge", cg, [LG], [5])
    SQ = f32arr(np.array([0.0, 1e-8, 2.0, 9.0], np.float32))
    cg = mini_blob(OPS["SQRT"], [], [], [(SQ.shape, None)], (4,))
    emit_case("syn_sqrt_edge", cg, [SQ], [4])

    # 16. concat 3 inputs axis=-1
    C1 = f32arr(rng.standard_normal((2, 2)))
    C2 = f32arr(rng.standard_normal((2, 3)))
    C3 = f32arr(rng.standard_normal((2, 1)))
    cg = mini_blob(OPS["CONCAT"], [1], [], [(C1.shape, None), (C2.shape, None), (C3.shape, None)], (2, 6))
    emit_case("syn_concat3", cg, [C1, C2, C3], [12])

    # 17. add [C] ⊗ [1,B,C]
    AA = f32arr(rng.standard_normal((5,)))
    AB = f32arr(rng.standard_normal((1, 3, 5)))
    cg = mini_blob(OPS["ADD"], [], [], [(AA.shape, None), (AB.shape, None)], (1, 3, 5))
    emit_case("syn_add_rowvec", cg, [AA, AB], [15])

    print(f"[ops] {len(index)} synthetic cases")
    return index


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    outdir = args.out or FIXTURE_DIR
    if args.check:
        tmp = tempfile.mkdtemp(prefix="golden-check-")
        outdir = tmp

    os.makedirs(os.path.join(outdir, "ops"), exist_ok=True)
    for name in MODELS:
        gen_e2e(name, outdir)
    index = extract_real_ops(outdir)
    index += synthetic_cases(outdir)
    index.sort(key=lambda e: e["name"])
    with open(os.path.join(outdir, "ops_index.json"), "w") as fh:
        json.dump(index, fh, indent=1)
    total_bytes = 0
    for root, _dirs, files in os.walk(outdir):
        for f in files:
            total_bytes += os.path.getsize(os.path.join(root, f))
    print(f"[gen] {len(index)} op cases; fixtures total {total_bytes/1e6:.2f} MB → {outdir}")

    if args.check:
        # compare structure + tolerant data
        ok = True
        committed_idx = json.load(open(os.path.join(FIXTURE_DIR, "ops_index.json")))
        if [e["name"] for e in committed_idx] != [e["name"] for e in index]:
            print("index mismatch")
            ok = False
        else:
            for e in index:
                for ext in (".mcy", ".bin"):
                    a = os.path.join(FIXTURE_DIR, "ops", e["name"] + ext)
                    b = os.path.join(outdir, "ops", e["name"] + ext)
                    if open(a, "rb").read() != open(b, "rb").read():
                        if ext == ".mcy":
                            print(f"blob mismatch: {e['name']}")
                            ok = False
                        else:
                            # data may differ by f32 noise across BLAS builds
                            da = np.fromfile(a, np.float32)
                            db = np.fromfile(b, np.float32)
                            if da.shape != db.shape or np.abs(da - db).max() > 1e-4:
                                print(f"data mismatch: {e['name']} max={np.abs(da-db).max():.3e}")
                                ok = False
        for name in MODELS:
            for suffix in (".json",):
                a = json.load(open(os.path.join(FIXTURE_DIR, f"e2e_{name}{suffix}")))
                b = json.load(open(os.path.join(outdir, f"e2e_{name}{suffix}")))
                if a != b:
                    print(f"e2e meta mismatch {name}")
                    ok = False
            da = np.fromfile(os.path.join(FIXTURE_DIR, f"e2e_{name}.bin"), np.float32)
            db = np.fromfile(os.path.join(outdir, f"e2e_{name}.bin"), np.float32)
            if da.shape != db.shape or np.abs(da - db).max() > 5e-4:
                print(f"e2e data mismatch {name}: max={np.abs(da-db).max():.3e}")
                ok = False
        shutil.rmtree(outdir, ignore_errors=True)
        print("GOLDEN CHECK", "PASS" if ok else "FAIL")
        sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
