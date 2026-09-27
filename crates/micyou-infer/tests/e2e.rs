//! End-to-end streaming goldens: run the compiled PureVox6 / AEC7 graphs
//! frame-by-frame with cache feedback (exactly how the audio pipeline
//! drives them) and compare against the numpy reference trajectory
//! recorded by `tools/onnx-port/gen_golden.py`.
//!
//! Per-frame the fixture records: full frame inputs, full primary outputs
//! (enhanced spectrum), strided samples of every cache output; after the
//! last frame, every non-empty output in full.

use std::fs;
use std::path::PathBuf;

use micyou_infer::{Graph, Session};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn within(got: f32, exp: f32, atol: f32) -> bool {
    if got.is_nan() && exp.is_nan() {
        return true;
    }
    (got - exp).abs() <= atol + atol * exp.abs()
}

struct Meta {
    frames: usize,
    input_lens: Vec<usize>,
    frame_input_idxs: Vec<usize>,
    primary_out_idxs: Vec<usize>,
    output_lens: Vec<usize>,
    sampled: Vec<(usize, usize, usize)>, // (out idx, stride, n samples)
    sampled_offsets: Vec<Vec<usize>>,
    feedback: Vec<(usize, usize)>, // (out idx, in idx)
    final_output_idxs: Vec<usize>,
}

fn load_meta(model: &str) -> Meta {
    let raw = fs::read_to_string(fixture_dir().join(format!("e2e_{model}.json")))
        .unwrap_or_else(|e| panic!("{model}: meta read: {e}"));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("meta parses");
    let usize_arr = |key: &str| -> Vec<usize> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_u64().unwrap() as usize)
            .collect()
    };
    let input_lens: Vec<usize> = v["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["len"].as_u64().unwrap() as usize)
        .collect();
    let output_lens: Vec<usize> = v["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["len"].as_u64().unwrap() as usize)
        .collect();
    let sampled: Vec<(usize, usize, usize)> = v["sampled_outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["idx"].as_u64().unwrap() as usize,
                s["stride"].as_u64().unwrap() as usize,
                s["n"].as_u64().unwrap() as usize,
            )
        })
        .collect();
    let sampled_offsets: Vec<Vec<usize>> = v["sampled_offsets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as usize)
                .collect()
        })
        .collect();
    let feedback: Vec<(usize, usize)> = v["feedback"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let a = p.as_array().unwrap();
            (
                a[0].as_u64().unwrap() as usize,
                a[1].as_u64().unwrap() as usize,
            )
        })
        .collect();
    Meta {
        frames: v["frames"].as_u64().unwrap() as usize,
        input_lens,
        frame_input_idxs: usize_arr("frame_input_idxs"),
        primary_out_idxs: usize_arr("primary_out_idxs"),
        output_lens,
        sampled,
        sampled_offsets,
        feedback,
        final_output_idxs: usize_arr("final_output_idxs"),
    }
}

fn run_e2e(model: &str, blob: &[u8], atol: f32) {
    let meta = load_meta(model);
    let data = f32s(
        &fs::read(fixture_dir().join(format!("e2e_{model}.bin")))
            .unwrap_or_else(|e| panic!("{model}: bin read: {e}")),
    );

    let graph = Graph::parse(blob).unwrap_or_else(|e| panic!("{model}: parse: {e}"));
    assert_eq!(graph.num_inputs(), meta.input_lens.len());
    assert_eq!(graph.num_outputs(), meta.output_lens.len());
    let mut sess = Session::new(&graph);

    let mut state: Vec<Vec<f32>> = meta.input_lens.iter().map(|&l| vec![0.0f32; l]).collect();
    let mut cursor = 0usize;
    let mut worst = 0.0f32;
    let mut worst_where = String::new();

    let compare =
        |where_: &str, got: &[f32], exp: &[f32], worst: &mut f32, worst_where: &mut String| {
            assert_eq!(got.len(), exp.len(), "{model} {where_}: length");
            for (j, (&g, &e)) in got.iter().zip(exp.iter()).enumerate() {
                assert!(
                    within(g, e, atol),
                    "{model} {where_}[{j}]: got {g} expected {e} (diff {})",
                    (g - e).abs()
                );
                let d = (g - e).abs() / e.abs().max(1.0);
                if d > *worst {
                    *worst = d;
                    *worst_where = format!("{where_}[{j}]");
                }
            }
        };

    for frame in 0..meta.frames {
        for &ii in &meta.frame_input_idxs {
            let len = meta.input_lens[ii];
            state[ii].copy_from_slice(&data[cursor..cursor + len]);
            cursor += len;
        }
        sess.run(&state.iter().map(|v| v.as_slice()).collect::<Vec<_>>())
            .unwrap_or_else(|e| panic!("{model} frame {frame}: run: {e}"));

        for &oi in &meta.primary_out_idxs {
            let len = meta.output_lens[oi];
            let got: Vec<f32> = sess.output(oi).unwrap().to_vec();
            compare(
                &format!("f{frame}.out{oi}"),
                &got,
                &data[cursor..cursor + len],
                &mut worst,
                &mut worst_where,
            );
            cursor += len;
        }
        for ((oi, _stride, n), offsets) in meta.sampled.iter().zip(meta.sampled_offsets.iter()) {
            let out = sess.output(*oi).unwrap();
            let got: Vec<f32> = offsets.iter().map(|&k| out[k]).collect();
            let exp: Vec<f32> = data[cursor..cursor + n].to_vec();
            compare(
                &format!("f{frame}.cache{oi}"),
                &got,
                &exp,
                &mut worst,
                &mut worst_where,
            );
            cursor += n;
        }
        for &(oo, ii) in &meta.feedback {
            let out = sess.output(oo).unwrap();
            state[ii].copy_from_slice(out);
        }
    }
    for &oi in &meta.final_output_idxs {
        let len = meta.output_lens[oi];
        let got: Vec<f32> = sess.output(oi).unwrap().to_vec();
        compare(
            &format!("final.out{oi}"),
            &got,
            &data[cursor..cursor + len],
            &mut worst,
            &mut worst_where,
        );
        cursor += len;
    }
    assert_eq!(cursor, data.len(), "{model}: fixture fully consumed");
    println!(
        "{model}: {} frames OK; worst rel diff {worst:.3e} @ {worst_where}",
        meta.frames
    );
}

#[test]
fn e2e_purevox6_streaming() {
    run_e2e(
        "purevox6",
        include_bytes!("../../micyou-audio/src/assets/purevox6.mcy"),
        3e-4,
    );
}

#[test]
fn e2e_aec7_streaming() {
    run_e2e(
        "aec7",
        include_bytes!("../../micyou-audio/src/assets/aec7.mcy"),
        3e-4,
    );
}
