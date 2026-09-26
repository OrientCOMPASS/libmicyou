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

//! The audio pipeline: a dedicated thread that turns transport audio events
//! into virtual-microphone output.
//!
//! ```text
//! AudioStreamEvent ─▶ JitterBuffer(+FEC) ─▶ decode (Opus/PCM) ─▶ resample 48k
//!        ─▶ DSP chain (AEC→NS→Dereverb→EQ→Gain→AGC→VAD + plugin nodes)
//!        ─▶ AudioOutputHandle (cpal ring buffer) ─▶ virtual mic
//! ```
//!
//! Upstream kept this as a 400-line closure inside `start_server_inner`; here
//! it is an owned struct so the pieces (session resets, loopback/AEC sync,
//! decoding, metering) are readable and unit-testable, and the only external
//! coupling is the event bus it publishes to.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use micyou_audio::dsp::{AudioDspSettings, DspProcessor, ExternalDspHook};
use micyou_audio::{AecFailure, RubatoResampler};
use micyou_protocol::micyou::AudioPacketMessage;
use micyou_transport::stats::NetworkStats;
use micyou_transport::stream::{validate_audio_packet, AudioStreamEvent};
use micyou_transport::udp::{ActiveAudioSession, SharedActiveAudioSession};
use tokio::sync::{mpsc, oneshot};

use crate::audio_output::AudioOutputHandle;
use crate::events::{AecStatus, EventBus, ServerEvent};

/// Everything the pipeline thread needs, assembled by the server core.
pub struct PipelineParams {
    /// Live DSP settings (shared with the RPC surface; hot-swappable).
    pub dsp_settings: Arc<RwLock<AudioDspSettings>>,
    /// Persistent output device handle.
    pub output: Arc<AudioOutputHandle>,
    /// Plugin DSP stage (runs at `Plugin:<id>` chain nodes).
    pub dsp_hook: Option<ExternalDspHook>,
    /// Shared statistics (mute flag, levels, session format).
    pub stats: Arc<NetworkStats>,
    /// Event bus for level/spectrum/AEC notifications.
    pub bus: EventBus,
    /// Ear-return monitoring flag (watched on every packet).
    pub is_monitoring: Arc<AtomicBool>,
    /// Spectrum streaming flag (frontends opt in).
    pub spectrum_enabled: Arc<AtomicBool>,
    /// Transport session state — gates the AEC loopback capture.
    pub active_audio_session: SharedActiveAudioSession,
    /// Resolved resource root (ONNX models), if any.
    pub resource_root: Option<PathBuf>,
    /// Web mode skips the DSP chain (browser audio is already processed).
    pub skip_dsp: bool,
    /// Output device to (re-)open before streaming starts.
    pub output_device: Option<String>,
    /// Output ring-buffer headroom in milliseconds.
    pub output_buffer_ms: usize,
}

/// Spawn the dedicated audio thread.
///
/// Returns the thread handle (tracked by the lifecycle state machine) and a
/// ready-signal receiver that resolves once the output device open was
/// attempted. The thread exits when `rx` closes or the sender is dropped.
pub fn spawn(
    params: PipelineParams,
    mut rx: mpsc::Receiver<AudioStreamEvent>,
) -> (
    std::thread::JoinHandle<()>,
    oneshot::Receiver<Result<(), String>>,
) {
    let (ready_tx, ready_rx) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("micyou-audio-pipeline".into())
        .spawn(move || {
            run(params, &mut rx, ready_tx);
        })
        .expect("spawn audio pipeline thread");
    (thread, ready_rx)
}

/// Ensure the persistent output device is open, creating the PipeWire virtual
/// sink/source first on Linux when no explicit device was selected.
/// Idempotent: safe to call at startup and again on every server start.
pub fn ensure_audio_output_started(
    output: &Arc<AudioOutputHandle>,
    output_device: Option<String>,
    output_buffer_ms: usize,
    resource_dir: Option<&std::path::Path>,
) -> bool {
    #[cfg(target_os = "linux")]
    let resolved_resource_dir = crate::resources::find_resource_dir(resource_dir);

    #[cfg(target_os = "linux")]
    {
        if output_device.is_none()
            && crate::platform::pipewire::is_available()
            && !crate::platform::pipewire::is_setup()
        {
            if crate::platform::pipewire::setup(resolved_resource_dir.as_deref()) {
                log::info!("[PipeWire] Virtual device ready, ALSA will route to virtual sink");
            } else {
                log::warn!("[PipeWire] Setup failed, falling back to default device");
            }
        }
    }
    let _ = resource_dir;

    output.ensure_open(output_device, output_buffer_ms)
}

/// Tear down the persistent output device. Only called on process exit.
pub fn shutdown_audio_output(output: &Arc<AudioOutputHandle>) {
    output.shutdown();
    #[cfg(target_os = "linux")]
    if crate::platform::pipewire::is_setup() {
        crate::platform::pipewire::cleanup();
    }
}

fn publish_aec(bus: &EventBus, available: bool, enabled: bool, reason: Option<AecFailure>) {
    bus.publish(ServerEvent::AecStatusChanged {
        status: AecStatus {
            available,
            enabled,
            reason,
        },
    });
}

fn disable_aec_runtime(runtime_available: &mut bool, bus: &EventBus, reason: AecFailure) {
    if !std::mem::replace(runtime_available, false) {
        return;
    }
    publish_aec(bus, false, false, Some(reason));
}

fn restore_aec_runtime(
    runtime_available: &mut bool,
    settings: &Arc<RwLock<AudioDspSettings>>,
    bus: &EventBus,
) {
    *runtime_available = true;
    let enabled = settings
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .aec_enabled;
    publish_aec(bus, true, enabled, None);
}

fn should_capture_loopback(
    transport_active: bool,
    audio_received: bool,
    aec_enabled: bool,
    runtime_available: bool,
) -> bool {
    transport_active && audio_received && aec_enabled && runtime_available
}

/// Decode one transport packet payload into interleaved f32 samples.
/// Returns `None` when the packet is undecodable (unsupported format,
/// broken Opus frame); callers skip the packet.
fn decode_packet(
    audio_data: &AudioPacketMessage,
    opus_decoder: &mut Option<(u32, usize, micyou_audio::opus::Decoder)>,
    opus_float_buf: &mut Vec<f32>,
) -> Option<Vec<f32>> {
    if audio_data.codec_kind() == micyou_protocol::Codec::Opus {
        // Opus decode: recreate the (stateful) decoder whenever the format
        // changes or a new transport session starts.
        let channels = audio_data.channel_count as usize;
        let sample_rate = audio_data.sample_rate as u32;
        let needs_decoder = match opus_decoder {
            Some((sr, ch, _)) => *sr != sample_rate || *ch != channels,
            None => true,
        };
        if needs_decoder {
            let created = micyou_audio::opus::Channels::from_channel_count(channels)
                .and_then(|ch| micyou_audio::opus::Decoder::new(sample_rate, ch).ok());
            *opus_decoder = created.map(|dec| (sample_rate, channels, dec));
            if opus_decoder.is_none() {
                log::error!(
                    "[Audio] Failed to create Opus decoder for {}Hz/{}ch",
                    sample_rate,
                    channels
                );
                return None;
            }
        }
        let (_, _, decoder) = opus_decoder.as_mut()?;
        let target_frames = (sample_rate as usize / 50) * channels; // 20 ms
        if opus_float_buf.len() != target_frames {
            opus_float_buf.resize(target_frames, 0.0);
        }
        match decoder.decode_float(&audio_data.buffer, opus_float_buf) {
            Ok(frames) => Some(opus_float_buf[..frames * channels].to_vec()),
            Err(e) => {
                log::error!("[Audio] Opus decode error: {}", e);
                None
            }
        }
    } else {
        let Some(format) = audio_data.format() else {
            log::error!(
                "[Audio] Unsupported audio format: {}",
                audio_data.audio_format
            );
            return None;
        };
        Some(format.decode_buffer(&audio_data.buffer))
    }
}

fn run(
    params: PipelineParams,
    rx: &mut mpsc::Receiver<AudioStreamEvent>,
    ready_tx: oneshot::Sender<Result<(), String>>,
) {
    let PipelineParams {
        dsp_settings,
        output,
        dsp_hook,
        stats,
        bus,
        is_monitoring,
        spectrum_enabled,
        active_audio_session,
        resource_root,
        skip_dsp,
        output_device,
        output_buffer_ms,
    } = params;

    // Ensure the virtual device is open. Normally a no-op (opened at daemon
    // startup); covers first-run and retry-after-failure cases.
    if !ensure_audio_output_started(
        &output,
        output_device,
        output_buffer_ms,
        resource_root.as_deref(),
    ) {
        log::error!("[Audio] Output device unavailable; audio will be silent");
    }
    let _ = ready_tx.send(Ok(()));

    let mut dsp_processor = DspProcessor::new(dsp_settings.clone(), resource_root);
    if let Some(hook) = dsp_hook {
        dsp_processor.set_external_hook(Some(hook));
    }
    let mut jb = micyou_transport::JitterBuffer::new(12);
    let mut frame_counter: u32 = 0;
    let mut input_resampler: Option<RubatoResampler> = None;
    let mut current_input_sample_rate: u32 = 0;
    let mut resample_out_buf = Vec::new();
    let mut pcm_f32 = Vec::new();
    // Opus decoder keyed by (sample_rate, channels); recreated on format or
    // session change (stateful codec).
    let mut opus_decoder: Option<(u32, usize, micyou_audio::opus::Decoder)> = None;
    let mut opus_float_buf: Vec<f32> = Vec::new();

    // Speaker loopback capture for the AEC far-end reference. Windows uses
    // WASAPI loopback; Linux records the default physical playback sink.
    // Both start lazily only after an AEC-enabled session sends audio.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let loopback: Option<micyou_audio::LoopbackCapture> =
        Some(micyou_audio::LoopbackCapture::new());
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    let loopback: Option<micyou_audio::LoopbackCapture> = None;

    let mut audio_received_for_session = false;
    let mut aec_runtime_available = true;
    if loopback.is_some() {
        restore_aec_runtime(&mut aec_runtime_available, &dsp_settings, &bus);
    }

    // Sync the AEC far-end capture with actual audio flow: a control session
    // alone is not enough — without mic packets there is no echo to cancel.
    let mut sync_loopback = |audio_received: &mut bool, runtime_available: &mut bool| {
        let transport_active = !matches!(
            *active_audio_session
                .read()
                .unwrap_or_else(|p| p.into_inner()),
            ActiveAudioSession::Inactive
        );
        if !transport_active {
            *audio_received = false;
        }

        let Some(lb) = &loopback else {
            return transport_active;
        };
        let aec_enabled = dsp_settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .aec_enabled;
        let should_capture = should_capture_loopback(
            transport_active,
            *audio_received,
            aec_enabled,
            *runtime_available,
        );

        if !should_capture {
            if lb.is_active() {
                lb.stop();
            }
            return transport_active;
        }
        if lb.is_active() {
            return transport_active;
        }

        let failure = lb.take_failure_reason().or_else(|| lb.start().err());
        if let Some(reason) = failure {
            disable_aec_runtime(runtime_available, &bus, reason);
        } else {
            log::info!("[Audio] Starting speaker loopback capture for AEC");
        }
        transport_active
    };

    loop {
        // Idle heartbeat every 500 ms keeps the loopback stream stopped while
        // no session is active (biggest idle CPU win).
        match rx.try_recv() {
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => {
                // Poll fast (10 ms) while a session is active: packets can
                // arrive after a silence gap and must not sit in the channel
                // for up to 500 ms (audible dropout at each utterance start).
                let session_active =
                    sync_loopback(&mut audio_received_for_session, &mut aec_runtime_available);
                std::thread::sleep(std::time::Duration::from_millis(if session_active {
                    10
                } else {
                    500
                }));
            }
            Ok(event) => {
                output.set_monitoring(is_monitoring.load(Ordering::Relaxed));
                match event {
                    AudioStreamEvent::SessionStarting { expected, epoch } => {
                        audio_received_for_session = false;
                        if let Some(lb) = &loopback {
                            lb.reset_session();
                        }
                        dsp_processor.reset_aec_session();
                        opus_decoder = None;
                        if loopback.is_some() {
                            restore_aec_runtime(&mut aec_runtime_available, &dsp_settings, &bus);
                        }
                        jb.prepare_transport_session_epoch(expected, epoch);
                        continue;
                    }
                    AudioStreamEvent::Packet { packet, epoch } => {
                        audio_received_for_session = true;
                        sync_loopback(&mut audio_received_for_session, &mut aec_runtime_available);
                        jb.push_epoch(packet, epoch);
                    }
                }
                let packets: Vec<_> = std::iter::from_fn(|| jb.pop()).collect();

                for ordered_packet in packets {
                    let Some(audio_data) = ordered_packet.audio_packet else {
                        continue;
                    };
                    let Some(decoded) =
                        decode_packet(&audio_data, &mut opus_decoder, &mut opus_float_buf)
                    else {
                        continue;
                    };
                    pcm_f32 = decoded;
                    if pcm_f32.is_empty() {
                        continue;
                    }

                    let channels = audio_data.channel_count.max(1) as usize;
                    let sample_rate = audio_data.sample_rate as u32;

                    if sample_rate > 0 && sample_rate != 48000 {
                        if current_input_sample_rate != sample_rate {
                            match RubatoResampler::new(sample_rate, 48000, channels) {
                                Ok(res) => {
                                    input_resampler = Some(res);
                                    current_input_sample_rate = sample_rate;
                                }
                                Err(e) => {
                                    log::error!("Failed to create resampler: {}", e);
                                    input_resampler = None;
                                    current_input_sample_rate = 48000;
                                }
                            }
                        }
                        if let Some(resampler) = input_resampler.as_mut() {
                            resampler.resample(&pcm_f32, channels, &mut resample_out_buf);
                            pcm_f32.clear();
                            pcm_f32.extend_from_slice(&resample_out_buf);
                        }
                    } else {
                        input_resampler = None;
                        current_input_sample_rate = 48000;
                    }

                    let queued_samples = output.queued_samples();
                    let queued_ms = (queued_samples as f64 / channels as f64) / 48.0;

                    // Web mode: browser audio arrives processed; skip the DSP
                    // chain and output directly.
                    let (input_rms, processed_rms) = if skip_dsp {
                        let sum: f32 = pcm_f32.iter().map(|x| x * x).sum();
                        let rms = (sum / pcm_f32.len() as f32).sqrt();
                        (rms, rms)
                    } else {
                        // Feed one mono far-end reference sample per near-end
                        // frame; matching counts prevents drift when packet
                        // sizes or input sample rates vary.
                        let near_frames = pcm_f32.len() / channels;
                        if let Some(far_data) = loopback
                            .as_ref()
                            .filter(|capture| capture.is_active())
                            .map(|capture| capture.read(near_frames))
                        {
                            dsp_processor.set_far_end_audio(&far_data);
                        }
                        let (raw, processed) =
                            dsp_processor.process(&mut pcm_f32, channels, queued_ms);
                        if let Some(reason) = dsp_processor.take_aec_failure() {
                            disable_aec_runtime(&mut aec_runtime_available, &bus, reason);
                        }
                        (raw, processed)
                    };

                    // Local hard-mute: the output engine drops audio itself;
                    // additionally report silence so UI meters and plugin
                    // snapshots read zero while muted.
                    let muted_now = stats.is_muted();
                    let (input_rms, processed_rms) = if muted_now {
                        (0.0, 0.0)
                    } else {
                        (input_rms, processed_rms)
                    };

                    // Precise RMS values for the plugin HostApi snapshot.
                    stats.set_levels(input_rms, processed_rms);

                    output.push(pcm_f32.clone(), channels);

                    frame_counter = frame_counter.wrapping_add(1);
                    if frame_counter.is_multiple_of(6) {
                        let level = (processed_rms * 500.0).min(100.0) as u32;
                        bus.publish(ServerEvent::AudioLevel { level });

                        if spectrum_enabled.load(Ordering::Acquire) {
                            let (mut raw_spec, mut proc_spec) = dsp_processor.get_spectrums();
                            if muted_now {
                                raw_spec.iter_mut().for_each(|v| *v = 0.0);
                                proc_spec.iter_mut().for_each(|v| *v = 0.0);
                            }
                            bus.publish(ServerEvent::AudioSpectrum {
                                raw: raw_spec,
                                processed: proc_spec,
                            });
                        }
                    }
                }
            }
        }
    }

    if let Some(lb) = &loopback {
        let was_active = lb.is_active();
        lb.stop();
        if was_active {
            log::info!("[Audio] Speaker loopback stopped");
        }
    }
}

/// Wrap a bare web-mode audio packet into a validated ordered packet with a
/// synthetic session/epoch (used by the web→pipeline bridge in the server).
pub fn wrap_web_packet(
    packet: AudioPacketMessage,
    sequence_number: i32,
) -> Option<micyou_protocol::micyou::AudioPacketMessageOrdered> {
    let ordered = micyou_protocol::micyou::AudioPacketMessageOrdered {
        sequence_number,
        audio_packet: Some(packet),
        timestamp: 0,
        fec_buffer: Vec::new(),
        fec_sequence_number: -1,
        session_id: 0,
        fec_packet_lengths: Vec::new(),
    };
    validate_audio_packet(&ordered).then_some(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_capture_gating_requires_all_conditions() {
        assert!(should_capture_loopback(true, true, true, true));
        assert!(!should_capture_loopback(false, true, true, true));
        assert!(!should_capture_loopback(true, false, true, true));
        assert!(!should_capture_loopback(true, true, false, true));
        assert!(!should_capture_loopback(true, true, true, false));
    }

    #[test]
    fn decodes_pcm16_packets() {
        let packet = AudioPacketMessage {
            buffer: vec![0x00, 0x80, 0x00, 0x40],
            sample_rate: 48000,
            channel_count: 1,
            audio_format: 2,
            codec: micyou_protocol::CODEC_PCM,
        };
        let mut decoder = None;
        let mut buf = Vec::new();
        let samples = decode_packet(&packet, &mut decoder, &mut buf).expect("decoded");
        assert_eq!(samples.len(), 2);
        assert!((samples[0] - -1.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_unknown_pcm_format() {
        let packet = AudioPacketMessage {
            buffer: vec![0u8; 8],
            sample_rate: 48000,
            channel_count: 1,
            audio_format: 99,
            codec: micyou_protocol::CODEC_PCM,
        };
        let mut decoder = None;
        let mut buf = Vec::new();
        assert!(decode_packet(&packet, &mut decoder, &mut buf).is_none());
    }

    #[test]
    fn web_packet_wrapper_validates_payloads() {
        let good = AudioPacketMessage {
            buffer: vec![0u8; 64],
            sample_rate: 48000,
            channel_count: 1,
            audio_format: 2,
            codec: micyou_protocol::CODEC_PCM,
        };
        assert!(wrap_web_packet(good.clone(), 0).is_some());

        let bad = AudioPacketMessage {
            channel_count: 9,
            ..good
        };
        assert!(wrap_web_packet(bad, 1).is_none());
    }
}
