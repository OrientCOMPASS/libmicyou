"""numpy reference interpreter ↔ onnxruntime parity check (streaming).

Runs both engines over N streaming frames (state caches fed back each frame,
exactly like the Rust runtime does) and reports per-output max errors.

CI usage:
    python3 parity_ort.py [--frames N] [--seed S] [--all-nodes]

--all-nodes additionally exposes every node output as a graph output for the
ORT session and diffs all intermediates (the silero-port methodology:
layer-by-layer bit-approximate comparison).
"""

from __future__ import annotations

import argparse
import os
import sys

import numpy as np
import onnx
import onnxruntime as ort

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from onnxref.interpreter import Interpreter, OnnxGraph  # noqa: E402

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))

MODELS = {
    "purevox6": dict(
        path=os.path.join(REPO_ROOT, "crates/micyou-core/resources/purevox6.onnx"),
        # graph inputs that are fed fresh random data each frame
        frame_inputs=["spec"],
        scale=0.05,
    ),
    "aec7": dict(
        path=os.path.join(REPO_ROOT, "crates/micyou-core/resources/aec7_ep0185.onnx"),
        frame_inputs=["mic_frame", "far_frame"],
        scale=0.05,
    ),
}


def feedback_map(input_names, output_names):
    """output name → input name for state caches (suffix conventions)."""
    fb = {}
    for out in output_names:
        for suffix in ("_out", "_o"):
            if out.endswith(suffix):
                base = out[: -len(suffix)]
                if base in input_names:
                    fb[out] = base
                break
    return fb


def run_parity(name, cfg, frames, seed, all_nodes=False):
    path = cfg["path"]
    model = onnx.load(path)
    input_names = [i.name for i in model.graph.input]
    output_names = [o.name for o in model.graph.output]
    input_shapes = {
        i.name: [d.dim_value for d in i.type.tensor_type.shape.dim]
        for i in model.graph.input
    }

    # ORT session (optionally with all intermediates exposed)
    if all_nodes:
        model = onnx.shape_inference.infer_shapes(model)
        existing = set(output_names)
        vi_names = set()
        for n in model.graph.node:
            for o in n.output:
                if o and o not in existing and o not in vi_names:
                    vi_names.add(o)
        value_infos = {v.name: v for v in model.graph.value_info}
        for o in sorted(vi_names):
            if o in value_infos:
                model.graph.output.append(value_infos[o])
        output_names = [o.name for o in model.graph.output]

    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.inter_op_num_threads = 1
    so.log_severity_level = 3
    sess = ort.InferenceSession(path if not all_nodes else model.SerializeToString(),
                                sess_options=so, providers=["CPUExecutionProvider"])
    ort_out_names = [o.name for o in sess.get_outputs()]

    graph = OnnxGraph(model if all_nodes else onnx.load(path))
    interp = Interpreter(graph)
    fb = feedback_map(input_names, output_names)

    rng = np.random.default_rng(seed)
    state = {n: np.zeros(input_shapes[n], np.float32) for n in input_names}
    worst = {}
    for f in range(frames):
        for fi in cfg["frame_inputs"]:
            state[fi] = (rng.standard_normal(input_shapes[fi]) * cfg["scale"]).astype(np.float32)
        feeds = {n: state[n] for n in input_names}
        out_np = interp.run(feeds)
        out_ort = sess.run(None, feeds)
        for oname, oval in zip(ort_out_names, out_ort):
            nval = out_np.get(oname)
            if nval is None:
                continue
            nval = np.asarray(nval, dtype=np.float32)
            oval = np.asarray(oval, dtype=np.float32)
            if nval.shape != oval.shape:
                print(f"SHAPE MISMATCH {oname}: np={nval.shape} ort={oval.shape}")
                return False
            if nval.size == 0:
                continue
            adiff = float(np.abs(nval - oval).max())
            denom = np.maximum(np.abs(oval), 1e-6)
            rdiff = float((np.abs(nval - oval) / denom).max())
            key = oname
            prev = worst.get(key)
            if prev is None or adiff > prev[0]:
                worst[key] = (adiff, rdiff, f)
        # feed state back
        for oname, iname in fb.items():
            if oname in out_np:
                state[iname] = np.asarray(out_np[oname], np.float32).reshape(input_shapes[iname])

    ok = True
    print(f"── {name}: {frames} streaming frames, {len(worst)} tensors compared")
    for k in sorted(worst):
        adiff, rdiff, frame = worst[k]
        flag = "OK " if (adiff < 1e-4 or rdiff < 2e-5) else "FAIL"
        if flag == "FAIL":
            ok = False
        print(f"  [{flag}] {k}: max_abs={adiff:.3e} max_rel={rdiff:.3e} (frame {frame})")
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", type=int, default=12)
    ap.add_argument("--seed", type=int, default=1234)
    ap.add_argument("--all-nodes", action="store_true")
    ap.add_argument("--model", default=None, help="limit to one model key")
    args = ap.parse_args()

    all_ok = True
    for name, cfg in MODELS.items():
        if args.model and args.model != name:
            continue
        ok = run_parity(name, cfg, args.frames, args.seed, args.all_nodes)
        all_ok &= ok
    print("PARITY", "PASS" if all_ok else "FAIL")
    sys.exit(0 if all_ok else 1)


if __name__ == "__main__":
    main()
