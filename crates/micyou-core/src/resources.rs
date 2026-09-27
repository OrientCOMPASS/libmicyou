/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Runtime resource discovery for the bundled ALSA resource tree.
//!
//! The AI noise-suppression and AEC models are compiled into the binary as
//! pure-Rust inference blobs (see `crates/micyou-infer` and
//! `tools/onnx-port`), so the only runtime resources left to locate are the
//! PipeWire/ALSA configuration files. Legacy packaged trees that still ship
//! the `.onnx` files are recognized as well.
//!
//! Search order covers packaged installs (`<prefix>/lib/micyou/resources`),
//! executable-relative layouts (`cargo run`, daemon next to `resources/`),
//! an explicit host-provided resource dir (e.g. a GUI bundling the files)
//! and the development checkout.

use std::path::{Path, PathBuf};

/// Markers identifying a MicYou resource directory (ALSA config; the legacy
/// ONNX model files are still accepted so pre-port packaged trees resolve).
const RESOURCE_MARKERS: [&str; 3] = [
    "alsa/micyou-pipewire.conf",
    "purevox6.onnx",
    "aec7_ep0185.onnx",
];

/// Locate the directory containing MicYou's bundled runtime resources
/// (ALSA config). Linux packages use the standard
/// `/usr/bin + /usr/lib/micyou/resources` layout; AppImage exposes the same
/// tree below its temporary mount point.
pub fn find_resource_dir(resource_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();

    // Explicit host-provided dir (GUI resource dir, daemon flag)
    if let Some(dir) = resource_dir {
        candidates.push(dir.to_path_buf());
        candidates.push(dir.join("resources"));
    }

    // Environment override
    if let Some(dir) = std::env::var_os("MICYOU_RESOURCE_DIR") {
        let dir = PathBuf::from(dir);
        candidates.push(dir.clone());
        candidates.push(dir.join("resources"));
    }

    // Executable-relative (covers `cargo run` and installs where resources
    // live next to the binary)
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        candidates.push(exe_dir.clone());
        candidates.push(exe_dir.join("resources"));
        if let Some(prefix) = exe_dir.parent() {
            candidates.push(prefix.join("lib").join("micyou").join("resources"));
        }
    }

    // Compile-time fallback for `cargo run` from the workspace
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources"));

    candidates.into_iter().find(|dir| {
        RESOURCE_MARKERS
            .iter()
            .any(|model| dir.join(model).exists())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_dir_is_found_in_dev_checkout() {
        // The workspace ships the ALSA config (and the source ONNX models)
        // under crates/micyou-core/resources, so the compile-time fallback
        // must resolve during `cargo test`.
        let dir = find_resource_dir(None).expect("dev resource dir");
        assert!(dir.join("alsa/micyou-pipewire.conf").exists());
    }
}
