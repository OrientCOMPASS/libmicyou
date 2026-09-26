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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use the vendored protoc binary directly (no PROTOC env mutation), so the
    // build works in sandboxes and CI images without a system protobuf install.
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    prost_build::Config::new()
        .protoc_path(&protoc)
        .compile_protos(&["proto/network.proto"], &["proto/"])?;
    Ok(())
}
