"""Time-domain goldens for the micyou-audio native processors.

Replicates PureVoxProcessor::process / AecProcessor::process (crates/
micyou-audio/src/dsp.rs) frame-exactly in numpy — sqrt-Hann analysis,
model hop, Hermitian mirror, iFFT synthesis and the respective OLA
schemes — driving the numpy reference interpreter. The Rust `#[cfg(test)]`
goldens in dsp.rs feed the same time-domain inputs to the real processors
and compare, which pins the STFT/cache/OLA wiring on top of the model
semantics already covered by micyou-infer's fixtures.

Usage: python3 gen_td_golden.py [--check]
"""

from __future__ import annotations

import argparse
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import onnx  # noqa: E402

from onnxref.interpreter import Interpreter, OnnxGraph  # noqa: E402
from parity_ort import MODELS, REPO_ROOT  # noqa: E402

OUT_DIR = os.path.join(REPO_ROOT, "crates", "micyou-audio", "tests", "fixtures")
FRAMES = 24
HOP = 480
NFFT = 960
BINS = NFFT // 2 + 1


def sqrt_hann(n: int) -> np.ndarray:
    # mirrors dsp.rs sqrt_hann_window: f64 math, cast to f32
    i = np.arange(n, dtype=np.float64)
    val = 0.5 - 0.5 * np.cos(2.0 * np.pi * i / (n - 1))
    return np.sqrt(val).astype(np.float32)


def ola_gain_f32(w: np.ndarray, hop: int) -> np.float32:
    # mirrors dsp.rs overlap_add_gain: sequential f32 accumulation
    acc = np.float32(0.0)
    for i in range(hop):
        a = np.float32(w[i] * w[i])
        b = np.float32(w[i + hop] * w[i + hop])
        acc = np.float32(acc + np.float32(a + b))
    avg = np.float32(acc / np.float32(hop))
    if avg > np.float32(0.001):
        return np.float32(1.0 / np.sqrt(avg))
    return np.float32(1.0)


def interp_for(key: str) -> tuple[Interpreter, list, list, dict]:
    cfg = MODELS[key]
    model = onnx.load(cfg["path"])
    in_names = [i.name for i in model.graph.input]
    out_names = [o.name for o in model.graph.output]
    graph = OnnxGraph(model)
    return Interpreter(graph), in_names, out_names, cfg


def speechlike(rng, n_frames: int, hop: int, f0=160.0, sr=48000.0) -> np.ndarray:
    """Formant-ish AM tone mixture + noise, one contiguous f32 signal."""
    n = n_frames * hop
    t = np.arange(n, dtype=np.float64) / sr
    env = 0.5 + 0.5 * np.sin(2 * np.pi * 2.3 * t + 0.7)
    sig = (
        0.5 * np.sin(2 * np.pi * f0 * t)
        + 0.3 * np.sin(2 * np.pi * f0 * 2.7 * t)
        + 0.2 * np.sin(2 * np.pi * f0 * 5.1 * t + 1.3)
    ) * env
    sig += 0.05 * rng.standard_normal(n)
    return (0.3 * sig).astype(np.float32)


def gen_purevox(rng) -> tuple[list, np.ndarray, np.ndarray]:
    interp, in_names, out_names, cfg = interp_for("purevox6")
    window = sqrt_hann(NFFT)
    gain = ola_gain_f32(window, HOP)
    prev = np.zeros(HOP, np.float32)
    acc = np.zeros(NFFT, np.float32)
    caches = {n: np.zeros((1, s), np.float32) for n, s in
              [("enc_c", 7368), ("dec_c", 1440), ("tfa_c", 800), ("inter_c", 4608)]}
    inputs = speechlike(rng, FRAMES, HOP).reshape(FRAMES, HOP)
    outputs = np.zeros((FRAMES, HOP), np.float32)
    for f in range(FRAMES):
        cur = inputs[f]
        buf = np.concatenate([prev, cur]).astype(np.float32)
        prev = cur.copy()
        buf = (buf * window).astype(np.float32)
        spec = np.fft.fft(buf)  # complex64
        spec_flat = np.zeros((1, BINS, 1, 2), np.float32)
        spec_flat[0, :, 0, 0] = spec[:BINS].real
        spec_flat[0, :, 0, 1] = spec[:BINS].imag
        feeds = {"spec": spec_flat, **caches}
        out = interp.run(feeds)
        enh = np.asarray(out["enhanced_spec"], np.float32).reshape(BINS, 2)
        cb = np.zeros(NFFT, np.complex64)
        cb[:BINS] = enh[:, 0] + 1j * enh[:, 1]
        for i in range(BINS, NFFT):
            cb[i] = np.conj(cb[NFFT - i])
        y = np.fft.ifft(cb)  # normalized (matches rust inverse * 1/N)
        samples = (y.real.astype(np.float32) * window).astype(np.float32)
        acc = (acc + samples).astype(np.float32)
        outputs[f] = (acc[:HOP] * gain).astype(np.float32)
        acc[: NFFT - HOP] = acc[HOP:]
        acc[NFFT - HOP:] = 0.0
        caches = {
            "enc_c": np.asarray(out["enc_c_out"], np.float32).reshape(1, 7368),
            "dec_c": np.asarray(out["dec_c_out"], np.float32).reshape(1, 1440),
            "tfa_c": np.asarray(out["tfa_c_out"], np.float32).reshape(1, 800),
            "inter_c": np.asarray(out["inter_c_out"], np.float32).reshape(1, 4608),
        }
    return [], inputs, outputs


def gen_aec(rng) -> tuple[list, np.ndarray, np.ndarray, np.ndarray]:
    interp, in_names, out_names, cfg = interp_for("aec7")
    window = sqrt_hann(NFFT)
    prev_mic = np.zeros(HOP, np.float32)
    prev_far = np.zeros(HOP, np.float32)
    ola = np.zeros(NFFT, np.float32)
    wsum = np.zeros(NFFT, np.float32)
    cache_spec = [
        ("res_enc_conv", 135680), ("res_enc_tfa", 248), ("mic_enc_conv", 135680),
        ("mic_enc_tfa", 248), ("deep_enc_tfa", 336), ("dec_conv", 13440),
        ("dec_tfa", 496), ("inter", 7680), ("res_prev1", 320), ("res_prev2", 320),
        ("mic_prev1", 320), ("mic_prev2", 320),
    ]
    caches = {n: np.zeros((1,) + ((1, 1, s) if s == 320 else (s,)), np.float32)
              for n, s in cache_spec}
    # prev caches have shape (1,1,1,320)
    for n in ("res_prev1", "res_prev2", "mic_prev1", "mic_prev2"):
        caches[n] = np.zeros((1, 1, 1, 320), np.float32)

    far = speechlike(np.random.default_rng(99), FRAMES, HOP, f0=220.0).reshape(FRAMES, HOP)
    mic = (0.6 * far + 0.02 * rng.standard_normal((FRAMES, HOP))).astype(np.float32)
    outputs = np.zeros((FRAMES, HOP), np.float32)

    def planar(sig_frame: np.ndarray) -> np.ndarray:
        buf = (sig_frame * window).astype(np.float32)
        spec = np.fft.fft(buf)
        out = np.zeros((1, 2, BINS), np.float32)
        out[0, 0] = spec[:BINS].real
        out[0, 1] = spec[:BINS].imag
        return out

    fb_out = {
        "res_enc_conv": "res_enc_conv_o", "res_enc_tfa": "res_enc_tfa_o",
        "mic_enc_conv": "mic_enc_conv_o", "mic_enc_tfa": "mic_enc_tfa_o",
        "deep_enc_tfa": "deep_enc_tfa_o", "dec_conv": "dec_conv_o",
        "dec_tfa": "dec_tfa_o", "inter": "inter_o",
        "res_prev1": "res_prev1_o", "res_prev2": "res_prev2_o",
        "mic_prev1": "mic_prev1_o", "mic_prev2": "mic_prev2_o",
    }
    for f in range(FRAMES):
        mbuf = np.concatenate([prev_mic, mic[f]]).astype(np.float32)
        prev_mic = mic[f].copy()
        fbuf = np.concatenate([prev_far, far[f]]).astype(np.float32)
        prev_far = far[f].copy()
        mic_frame = planar(mbuf)
        far_frame = planar(fbuf)
        feeds = {"mic_frame": mic_frame, "far_frame": far_frame, **caches}
        out = interp.run(feeds)
        enh = np.asarray(out["enhanced_frame"], np.float32).reshape(2, BINS)
        cb = np.zeros(NFFT, np.complex64)
        for bidx in range(NFFT):
            if bidx < BINS:
                cb[bidx] = enh[0, bidx] + 1j * enh[1, bidx]
            else:
                m = NFFT - bidx
                cb[bidx] = enh[0, m] - 1j * enh[1, m]
        y = np.fft.ifft(cb)
        samples = (y.real.astype(np.float32) * window).astype(np.float32)
        ola = (ola + samples).astype(np.float32)
        wsum = (wsum + (window * window).astype(np.float32)).astype(np.float32)
        out_hop = np.zeros(HOP, np.float32)
        for i in range(HOP):
            out_hop[i] = ola[i] / wsum[i] if wsum[i] > np.float32(1e-6) else ola[i]
        outputs[f] = out_hop
        ola[: NFFT - HOP] = ola[HOP:]
        ola[NFFT - HOP:] = 0.0
        wsum[: NFFT - HOP] = wsum[HOP:]
        wsum[NFFT - HOP:] = 0.0
        for n, oname in fb_out.items():
            caches[n] = np.asarray(out[oname], np.float32).reshape(caches[n].shape)
    return [], mic, far, outputs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args()

    os.makedirs(OUT_DIR, exist_ok=True)
    rng = np.random.default_rng(20260927)
    _, pv_in, pv_out = gen_purevox(rng)
    rng2 = np.random.default_rng(4242)
    _, aec_mic, aec_far, aec_out = gen_aec(rng2)

    metas = {
        "td_purevox6": {"frames": FRAMES, "hop": HOP, "layout": ["in", "out"]},
        "td_aec7": {"frames": FRAMES, "hop": HOP, "layout": ["mic", "far", "out"]},
    }
    blobs = {
        "td_purevox6": b"".join(
            (pv_in[f].astype("<f4").tobytes() + pv_out[f].astype("<f4").tobytes())
            for f in range(FRAMES)
        ),
        "td_aec7": b"".join(
            (aec_mic[f].astype("<f4").tobytes() + aec_far[f].astype("<f4").tobytes()
             + aec_out[f].astype("<f4").tobytes())
            for f in range(FRAMES)
        ),
    }
    for name in metas:
        bin_path = os.path.join(OUT_DIR, name + ".bin")
        json_path = os.path.join(OUT_DIR, name + ".json")
        if args.check:
            ok = True
            with open(bin_path, "rb") as fh:
                committed = fh.read()
            fresh = blobs[name]
            a = np.frombuffer(committed, "<f4")
            b = np.frombuffer(fresh, "<f4")
            if a.shape != b.shape or np.abs(a - b).max() > 1e-4:
                print(f"TD GOLDEN DRIFT {name}: max={np.abs(a-b).max() if a.shape==b.shape else 'shape'}")
                ok = False
            with open(json_path) as fh:
                if json.load(fh) != metas[name]:
                    print(f"TD META DRIFT {name}")
                    ok = False
            if not ok:
                sys.exit(1)
            print(f"[td {name}] fresh")
        else:
            with open(bin_path, "wb") as fh:
                fh.write(blobs[name])
            with open(json_path, "w") as fh:
                json.dump(metas[name], fh)
            print(f"[td {name}] wrote {len(blobs[name])/1e3:.0f} KB")


if __name__ == "__main__":
    main()
