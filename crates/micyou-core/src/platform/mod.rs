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

//! Platform virtual-audio-device helpers.
//!
//! All modules drive external tools (installers, `pw-cli`,
//! `SwitchAudioSource`) through process spawning — no GUI toolkit involved.
//! `blackhole` and `vbcable` compile on every platform with inert fallbacks;
//! `pipewire` is Linux-only.

pub mod blackhole;
pub mod vbcable;

#[cfg(target_os = "linux")]
pub mod pipewire;
