//! DSP chain benchmark — ONNX Runtime baseline (ort).
//!
//! This harness is copied into a `main`-branch checkout by the CI `bench`
//! job and built against the ort-based `micyou-audio`. It mirrors
//! `bench-native` exactly (same signals, configs, timing) so the two
//! result sets are directly comparable.
//!
//! Env:
//!   MICYOU_ORT_LIB    path to libonnxruntime.so (required)
//!   MICYOU_MODEL_DIR  resource dir containing purevox6.onnx / aec7_ep0185.onnx (required)

use std::time::Instant;

use micyou_audio::{AudioDspSettings, DspProcessor};
use std::sync::{Arc, RwLock};

const HOP: usize = 480;
const WARMUP: usize = 300;

/// Deterministic LCG — no rand dependency, identical signals across trees.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// Speech-like deterministic signal: AM harmonic stack + light noise.
fn gen_signal(seed: u64, frames: usize, f0: f32) -> Vec<f32> {
    let mut rng = Lcg(seed);
    let n = frames * HOP;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / 48000.0;
        let env = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 2.1 * t + 0.4).sin();
        let s = 0.5 * (2.0 * std::f32::consts::PI * f0 * t).sin()
            + 0.3 * (2.0 * std::f32::consts::PI * f0 * 2.7 * t).sin()
            + 0.2 * (2.0 * std::f32::consts::PI * f0 * 5.1 * t + 1.3).sin();
        out.push(0.3 * s * env + 0.02 * rng.next_f32());
    }
    out
}

struct Config {
    name: &'static str,
    aec: bool,
    ns: bool,
}

fn run_config(
    cfg: &Config,
    mic: &[f32],
    far: &[f32],
    frames: usize,
    model_dir: Option<std::path::PathBuf>,
) -> serde_json::Value {
    let mut settings = AudioDspSettings::default();
    settings.aec_enabled = cfg.aec;
    settings.ns_enabled = cfg.ns;
    settings.ns_type = "PureVox".to_string();
    settings.ns_intensity = 100.0;
    let mut dsp = DspProcessor::new(Arc::new(RwLock::new(settings)), model_dir);

    let total = WARMUP + frames;
    let mut durations: Vec<f64> = Vec::with_capacity(frames);
    let mut chunk: Vec<f32> = Vec::with_capacity(HOP);
    let mut far_chunk: Vec<f32> = Vec::with_capacity(HOP);
    for f in 0..total {
        chunk.clear();
        chunk.extend_from_slice(&mic[f * HOP..(f + 1) * HOP]);
        far_chunk.clear();
        far_chunk.extend_from_slice(&far[f * HOP..(f + 1) * HOP]);
        dsp.set_far_end_audio(&far_chunk);
        let t0 = Instant::now();
        let (raw, processed) = dsp.process(&mut chunk, 1, 200.0);
        let dt = t0.elapsed();
        assert!(
            raw.is_finite() && processed.is_finite(),
            "non-finite meters"
        );
        assert!(chunk.iter().all(|v| v.is_finite()), "non-finite audio");
        if f >= WARMUP {
            durations.push(dt.as_secs_f64() * 1e6);
        }
    }
    durations.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = durations.len();
    let mean = durations.iter().sum::<f64>() / n as f64;
    let p50 = durations[n / 2];
    let p95 = durations[(n as f64 * 0.95) as usize];
    let max = durations[n - 1];
    serde_json::json!({
        "name": cfg.name,
        "frames": frames,
        "mean_us": mean,
        "p50_us": p50,
        "p95_us": p95,
        "max_us": max,
        // real-time factor against the 10 ms hop budget
        "rtf": mean / 10_000.0,
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut frames = 3000usize;
    let mut json_path: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--frames" => frames = args.next().expect("--frames value").parse().unwrap(),
            "--json" => json_path = Some(args.next().expect("--json value")),
            other => panic!("unknown arg {other}"),
        }
    }

    // ort baseline requires the dynamic runtime + model dir
    let ort_lib =
        std::env::var("MICYOU_ORT_LIB").expect("MICYOU_ORT_LIB must point at libonnxruntime.so");
    let model_dir = std::env::var("MICYOU_MODEL_DIR")
        .expect("MICYOU_MODEL_DIR must point at the resources dir");
    micyou_audio::init_ort_runtime(std::path::Path::new(&ort_lib))
        .unwrap_or_else(|e| panic!("init_ort_runtime({ort_lib}): {e}"));
    let model_dir = Some(std::path::PathBuf::from(model_dir));

    let total = WARMUP + frames;
    let mic = gen_signal(0xbeef_c0de_0000_0001, total, 160.0);
    let far = gen_signal(0xbeef_c0de_0000_0002, total, 220.0);

    let configs = [
        Config {
            name: "baseline (dsp chain, no models)",
            aec: false,
            ns: false,
        },
        Config {
            name: "aec7",
            aec: true,
            ns: false,
        },
        Config {
            name: "purevox6",
            aec: false,
            ns: true,
        },
        Config {
            name: "aec7+purevox6",
            aec: true,
            ns: true,
        },
    ];

    let mut results = Vec::new();
    println!(
        "bench-ort: {} frames (+{} warmup), 480-sample hops @48kHz mono",
        frames, WARMUP
    );
    for cfg in &configs {
        let r = run_config(cfg, &mic, &far, frames, model_dir.clone());
        println!(
            "  {:<32} mean {:>9.1} µs  p50 {:>9.1}  p95 {:>9.1}  max {:>9.1}  RTF {:.4}",
            cfg.name,
            r["mean_us"].as_f64().unwrap(),
            r["p50_us"].as_f64().unwrap(),
            r["p95_us"].as_f64().unwrap(),
            r["max_us"].as_f64().unwrap(),
            r["rtf"].as_f64().unwrap(),
        );
        results.push(r);
    }

    let out = serde_json::json!({
        "label": "ort",
        "frames": frames,
        "warmup": WARMUP,
        "configs": results,
    });
    if let Some(path) = json_path {
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
        println!("wrote {path}");
    }
}
