"""Compile the bundled ONNX models into MCYI blobs for the pure-Rust VM.

Usage (from the repository root or tools/onnx-port):
    python3 tools/onnx-port/compile_models.py [--check]

Writes:
    crates/micyou-audio/src/assets/purevox6.mcy   (+ .mcy.json debug sidecar)
    crates/micyou-audio/src/assets/aec7.mcy       (+ .mcy.json debug sidecar)

--check recompiles and fails if the committed blobs differ (CI freshness
gate). The blob format is deterministic, so byte-identical output is
expected for identical model + compiler versions.
"""

from __future__ import annotations

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from onnxref.compiler import compile_model  # noqa: E402

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
ASSET_DIR = os.path.join(REPO_ROOT, "crates", "micyou-audio", "src", "assets")

MODELS = [
    ("purevox6", os.path.join(REPO_ROOT, "crates/micyou-core/resources/purevox6.onnx"), "purevox6.mcy"),
    ("aec7", os.path.join(REPO_ROOT, "crates/micyou-core/resources/aec7_ep0185.onnx"), "aec7.mcy"),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="verify committed blobs are up to date")
    args = ap.parse_args()

    os.makedirs(ASSET_DIR, exist_ok=True)
    failed = False
    for name, src, out_name in MODELS:
        cg = compile_model(src, name)
        blob = cg.to_bytes()
        out_path = os.path.join(ASSET_DIR, out_name)
        json_path = out_path + ".json"
        report = cg.report
        print(f"[{name}] {report['onnx_nodes']} onnx nodes -> {report['runtime_nodes']} runtime "
              f"-> {report['emitted_ops']} emitted ops; slots={report['slots']} "
              f"arena={report['arena_bytes']/1e6:.2f}MB const={report['const_bytes']/1e6:.2f}MB "
              f"blob={report['blob_bytes']/1e6:.2f}MB")
        print(f"    op histogram: {report['op_hist']}")
        if args.check:
            with open(out_path, "rb") as f:
                committed = f.read()
            if committed != blob:
                print(f"    STALE: {out_path} differs from freshly compiled blob "
                      f"({len(committed)} vs {len(blob)} bytes)")
                failed = True
            else:
                print(f"    fresh: {out_path}")
        else:
            with open(out_path, "wb") as f:
                f.write(blob)
            with open(json_path, "w") as f:
                f.write(cg.debug_json())
            print(f"    wrote {out_path} (+ .json sidecar)")
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
