/* libmicyou — headless, frontend-decoupled backend for MicYou. */

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
