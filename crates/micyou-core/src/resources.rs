/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 * See LICENSE for details.
 */

//! Runtime resource discovery: the ONNX Runtime shared library and the
//! bundled model/ALSA resource tree (PureVox + AEC7 models).
//!
//! Search order covers packaged installs (`<prefix>/lib/micyou/{libs,resources}`),
//! executable-relative layouts (`cargo run`, daemon next to `resources/`), an
//! explicit host-provided resource dir (e.g. a GUI bundling the files) and the
//! development checkout.

use std::path::{Path, PathBuf};

/// Platform-specific ONNX Runtime shared library filename.
pub const fn ort_runtime_filename() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "onnxruntime.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libonnxruntime.dylib"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "libonnxruntime.so"
    }
}

/// Markers identifying a MicYou resource directory (the bundled ONNX models).
const RESOURCE_MARKERS: [&str; 2] = ["purevox6.onnx", "aec7_ep0185.onnx"];

/// Find the ONNX Runtime shared library. `resource_root` is an optional
/// host-provided directory (GUI resource dir, daemon `--resources`).
pub fn find_ort_runtime(resource_root: Option<&Path>) -> Option<PathBuf> {
    let filename = ort_runtime_filename();
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Host-provided resource root (production: lib copied next to resources)
    if let Some(root) = resource_root {
        candidates.push(root.join(filename));
        if let Some(parent) = root.parent() {
            candidates.push(parent.join("libs").join(filename));
            candidates.push(parent.join(filename));
        }
    }

    // Executable-relative
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        candidates.push(exe_dir.join(filename));
        candidates.push(exe_dir.join("resources").join(filename));
        candidates.push(exe_dir.join("libs").join(filename));
        if let Some(prefix) = exe_dir.parent() {
            candidates.push(
                prefix
                    .join("lib")
                    .join("micyou")
                    .join("libs")
                    .join(filename),
            );
            candidates.push(
                prefix
                    .join("lib")
                    .join("micyou")
                    .join("resources")
                    .join(filename),
            );
        }
    }

    // Environment override (packagers / power users)
    if let Some(dir) = std::env::var_os("MICYOU_RESOURCE_DIR") {
        candidates.push(PathBuf::from(dir).join(filename));
    }

    // Dev mode: crates/micyou-core/libs/
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("libs")
            .join(filename),
    );

    candidates.into_iter().find(|p| p.exists())
}

/// Locate the directory containing MicYou's bundled runtime resources (ONNX
/// models and the ALSA config). Linux packages use the standard
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
        // The workspace ships the models under crates/micyou-core/resources,
        // so the compile-time fallback must resolve during `cargo test`.
        let dir = find_resource_dir(None).expect("dev resource dir");
        assert!(dir.join("purevox6.onnx").exists());
        assert!(dir.join("aec7_ep0185.onnx").exists());
    }

    #[test]
    fn ort_filename_matches_platform_convention() {
        let name = ort_runtime_filename();
        #[cfg(target_os = "windows")]
        assert_eq!(name, "onnxruntime.dll");
        #[cfg(target_os = "macos")]
        assert_eq!(name, "libonnxruntime.dylib");
        #[cfg(target_os = "linux")]
        assert_eq!(name, "libonnxruntime.so");
    }
}
