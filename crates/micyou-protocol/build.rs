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
    // Point prost-build at the vendored protoc binary so the build works
    // without a system protobuf installation (same approach as upstream).
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    prost_build::compile_protos(&["proto/network.proto"], &["proto/"])?;
    Ok(())
}
