"""Round-trip check: compiled MCYI blob (numpy replay) == reference
interpreter == onnxruntime, over streaming frames with cache feedback.

Usage:
    python3 replay_check.py [--frames N] [--seed S] [--with-ort]
"""

from __future__ import annotations

import argparse
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from onnxref.interpreter import Interpreter, OnnxGraph  # noqa: E402
from onnxref.replay import BlobGraph, run_blob  # noqa: E402
from parity_ort import MODELS, feedback_map, REPO_ROOT  # noqa: E402

ASSET_DIR = os.path.join(REPO_ROOT, "crates", "micyou-audio", "src", "assets")
BLOB = {"purevox6": "purevox6.mcy", "aec7": "aec7.mcy"}


def check(name, frames, seed, with_ort):
    cfg = MODELS[name]
    import onnx
    model = onnx.load(cfg["path"])
    input_names = [i.name for i in model.graph.input]
    output_names = [o.name for o in model.graph.output]
    input_shapes = {
        i.name: tuple(d.dim_value for d in i.type.tensor_type.shape.dim)
        for i in model.graph.input
    }
    graph = OnnxGraph(model)
    interp = Interpreter(graph)
    blob = BlobGraph(open(os.path.join(ASSET_DIR, BLOB[name]), "rb").read())

    sess = None
    if with_ort:
        import onnxruntime as ort
        so = ort.SessionOptions()
        so.intra_op_num_threads = 1
        so.log_severity_level = 3
        sess = ort.InferenceSession(cfg["path"], sess_options=so,
                                    providers=["CPUExecutionProvider"])
    fb = feedback_map(input_names, output_names)
    rng = np.random.default_rng(seed)
    state = {n: np.zeros(input_shapes[n], np.float32) for n in input_names}
    worst_blob = {}
    worst_ort = {}
    for f in range(frames):
        for fi in cfg["frame_inputs"]:
            state[fi] = (rng.standard_normal(input_shapes[fi]) * cfg["scale"]).astype(np.float32)
        feeds = {n: state[n] for n in input_names}
        feed_list = [state[n] for n in input_names]

        out_interp = interp.run(feeds)
        out_blob = run_blob(blob, feed_list)
        for oname, bval in zip(output_names, out_blob):
            ival = np.asarray(out_interp[oname], np.float32)
            bval = np.asarray(bval, np.float32)
            assert ival.shape == bval.shape, f"{oname}: {ival.shape} vs {bval.shape}"
            if ival.size == 0:
                continue
            d = float(np.abs(ival - bval).max())
            if oname not in worst_blob or d > worst_blob[oname][0]:
                worst_blob[oname] = (d, f)
            if sess is not None:
                pass
        if sess is not None:
            out_ort = sess.run(None, feeds)
            for oname, oval in zip([o.name for o in sess.get_outputs()], out_ort):
                bval = None
                for nm, bv in zip(output_names, out_blob):
                    if nm == oname:
                        bval = np.asarray(bv, np.float32)
                if bval is None or bval.size == 0:
                    continue
                d = float(np.abs(bval - np.asarray(oval, np.float32)).max())
                if oname not in worst_ort or d > worst_ort[oname][0]:
                    worst_ort[oname] = (d, f)
        for oname, iname in fb.items():
            state[iname] = np.asarray(out_interp[oname], np.float32).reshape(input_shapes[iname])

    ok = True
    print(f"── {name}: {frames} frames")
    for k in sorted(worst_blob):
        d, f = worst_blob[k]
        flag = "OK " if d <= 2e-5 else "FAIL"
        ok &= d <= 2e-5
        print(f"  [blob-vs-interp {flag}] {k}: max_abs={d:.3e} (frame {f})")
    for k in sorted(worst_ort):
        d, f = worst_ort[k]
        flag = "OK " if d <= 1e-4 else "FAIL"
        ok &= d <= 1e-4
        print(f"  [blob-vs-ort    {flag}] {k}: max_abs={d:.3e} (frame {f})")
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", type=int, default=8)
    ap.add_argument("--seed", type=int, default=999)
    ap.add_argument("--with-ort", action="store_true")
    args = ap.parse_args()
    ok = True
    for name in MODELS:
        ok &= check(name, args.frames, args.seed, args.with_ort)
    print("REPLAY", "PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
