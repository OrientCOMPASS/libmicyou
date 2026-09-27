/* MicYou — Turns your Android device into a high-quality PC microphone. */

pub mod aec;
#[cfg(feature = "dsp")]
pub mod dsp;
pub mod engine;
pub mod loopback;
pub mod mixer;
pub mod opus;

pub use aec::AecFailure;
#[cfg(feature = "dsp")]
pub use dsp::{AudioDspSettings, DspProcessor, EqualizerConfig};
pub use engine::{device_init_lock, AudioOutputManager, RubatoResampler};
pub use loopback::LoopbackCapture;
pub use mixer::{SoundEffect, SoundMixer};
