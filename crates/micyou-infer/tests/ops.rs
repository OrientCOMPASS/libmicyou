//! Per-op golden tests: every fixture is a single-op MCYI blob extracted
//! from the compiled PureVox6/AEC7 graphs (real weights, random
//! activations) or synthesized for edge coverage. Expected outputs come
//! from the numpy reference replay (`tools/onnx-port/gen_golden.py`),
//! which is itself validated against onnxruntime in CI.

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

fn close(got: f32, exp: f32) -> bool {
    if got.is_nan() && exp.is_nan() {
        return true;
    }
    if got.is_infinite() || exp.is_infinite() {
        return got == exp;
    }
    (got - exp).abs() <= 2e-4 + 2e-4 * exp.abs()
}

#[test]
fn op_goldens() {
    let dir = fixture_dir();
    let index_raw = fs::read_to_string(dir.join("ops_index.json")).expect("ops_index.json");
    let index: serde_json::Value = serde_json::from_str(&index_raw).expect("index parses");
    let cases = index.as_array().expect("index is an array");
    assert!(cases.len() > 100, "expected a rich fixture set");

    let mut worst = 0.0f32;
    let mut worst_case = String::new();
    let mut checked = 0usize;

    for case in cases {
        let name = case["name"].as_str().unwrap();
        let in_lens: Vec<usize> = case["in_lens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as usize)
            .collect();
        let out_lens: Vec<usize> = case["out_lens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as usize)
            .collect();

        let blob = fs::read(dir.join("ops").join(format!("{name}.mcy")))
            .unwrap_or_else(|e| panic!("{name}: blob read: {e}"));
        let data = f32s(
            &fs::read(dir.join("ops").join(format!("{name}.bin")))
                .unwrap_or_else(|e| panic!("{name}: data read: {e}")),
        );

        let graph = Graph::parse(&blob).unwrap_or_else(|e| panic!("{name}: parse: {e}"));
        assert_eq!(graph.num_inputs(), in_lens.len(), "{name}: input count");

        let mut pos = 0usize;
        let mut feeds: Vec<Vec<f32>> = Vec::with_capacity(in_lens.len());
        for &len in &in_lens {
            feeds.push(data[pos..pos + len].to_vec());
            pos += len;
        }
        let expected = &data[pos..];

        let mut sess = Session::new(&graph);
        sess.run(&feeds.iter().map(|v| v.as_slice()).collect::<Vec<_>>())
            .unwrap_or_else(|e| panic!("{name}: run: {e}"));

        let mut epos = 0usize;
        for (oi, &olen) in out_lens.iter().enumerate() {
            let got = sess
                .output(oi)
                .unwrap_or_else(|e| panic!("{name}: output {oi}: {e}"));
            assert_eq!(got.len(), olen, "{name}: output {oi} length");
            for (j, (&g, &e)) in got
                .iter()
                .zip(expected[epos..epos + olen].iter())
                .enumerate()
            {
                assert!(
                    close(g, e),
                    "{name}: output {oi}[{j}] got {g} expected {e} (diff {})",
                    (g - e).abs()
                );
                let rel = (g - e).abs() / e.abs().max(1.0);
                if rel > worst {
                    worst = rel;
                    worst_case = format!("{name}[{oi}][{j}]");
                }
            }
            epos += olen;
        }
        assert_eq!(epos, expected.len(), "{name}: expected data consumed");
        checked += 1;
    }
    println!("{checked} op goldens passed; worst rel err {worst:.3e} @ {worst_case}");
}

#[test]
fn determinism_same_inputs_bit_identical() {
    // Running the same session twice with identical inputs must produce
    // bit-identical outputs (no hidden state beyond the declared caches).
    let dir = fixture_dir();
    let index_raw = fs::read_to_string(dir.join("ops_index.json")).unwrap();
    let index: serde_json::Value = serde_json::from_str(&index_raw).unwrap();
    let case = &index.as_array().unwrap()[0];
    let name = case["name"].as_str().unwrap();
    let in_lens: Vec<usize> = case["in_lens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as usize)
        .collect();
    let blob = fs::read(dir.join("ops").join(format!("{name}.mcy"))).unwrap();
    let data = f32s(&fs::read(dir.join("ops").join(format!("{name}.bin"))).unwrap());
    let graph = Graph::parse(&blob).unwrap();
    let mut pos = 0usize;
    let mut feeds: Vec<Vec<f32>> = Vec::new();
    for &len in &in_lens {
        feeds.push(data[pos..pos + len].to_vec());
        pos += len;
    }
    let mut sess = Session::new(&graph);
    sess.run(&feeds.iter().map(|v| v.as_slice()).collect::<Vec<_>>())
        .unwrap();
    let first: Vec<f32> = sess.output(0).unwrap().to_vec();
    sess.run(&feeds.iter().map(|v| v.as_slice()).collect::<Vec<_>>())
        .unwrap();
    let second: Vec<f32> = sess.output(0).unwrap().to_vec();
    assert_eq!(first, second, "VM must be deterministic for identical runs");
}
