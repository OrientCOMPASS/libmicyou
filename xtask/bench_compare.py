#!/usr/bin/env python3
"""Render ort-vs-native benchmark JSONs into a markdown summary table.

Usage: bench_compare.py ort.json native.json [summary.md]
"""
import json
import sys


def main():
    ort = json.load(open(sys.argv[1]))
    nat = json.load(open(sys.argv[2]))
    summary_path = sys.argv[3] if len(sys.argv) > 3 else None

    ort_cfg = {c["name"]: c for c in ort["configs"]}
    nat_cfg = {c["name"]: c for c in nat["configs"]}
    frames = nat.get("frames", "?")

    lines = []
    lines.append(f"## Native VM vs ONNX Runtime — per-frame DSP latency ({frames} frames measured, 10 ms budget)")
    lines.append("")
    lines.append("| config | ort mean | native mean | ratio | ort p95 | native p95 | native RTF |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|")
    for name in [c["name"] for c in ort["configs"]]:
        o = ort_cfg.get(name)
        n = nat_cfg.get(name)
        if not o or not n:
            continue
        ratio = n["mean_us"] / o["mean_us"] if o["mean_us"] else float("nan")
        lines.append(
            f"| {name} | {o['mean_us']:.0f} µs | {n['mean_us']:.0f} µs | "
            f"{ratio:.2f}× | {o['p95_us']:.0f} µs | {n['p95_us']:.0f} µs | "
            f"{n['rtf']:.4f} |"
        )
    lines.append("")
    lines.append("_Same runner, same harness, single-threaded; RTF = mean frame time / 10 ms hop budget. "
                 "GitHub-hosted runners are shared — treat ratios as indicative, not absolute._")
    text = "\n".join(lines)
    print(text)
    if summary_path:
        with open(summary_path, "a") as fh:
            fh.write(text + "\n")


if __name__ == "__main__":
    main()
