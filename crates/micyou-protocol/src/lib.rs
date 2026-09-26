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

//! MicYou wire protocol: protobuf messages, magics and port constants.
//!
//! The wire format is **byte-compatible with the stock MicYou Android client**
//! and must not change without a protocol version bump:
//! - TCP control channel: length-prefixed protobuf [`micyou::MessageWrapper`],
//!   stream magic [`PACKET_MAGIC`] (`"MicY"`), handshake strings
//!   [`HANDSHAKE_CLIENT_STR`] / [`HANDSHAKE_SERVER_STR`].
//! - UDP audio channel (TCP port + 1): datagram magic [`UDP_PACKET_MAGIC`]
//!   (`"MicU"`), payload is an encoded [`micyou::AudioPacketMessageOrdered`].

/// Protobuf messages generated from `proto/network.proto`.
pub mod micyou {
    include!(concat!(env!("OUT_DIR"), "/micyou.rs"));
}

/// TCP stream magic, ASCII `"MicY"` as a big-endian i32.
pub const PACKET_MAGIC: i32 = 0x4D696359;
/// UDP datagram magic, ASCII `"MicU"` as a big-endian i32.
pub const UDP_PACKET_MAGIC: i32 = 0x4D696355;

/// Audio buffer codecs carried in [`micyou::AudioPacketMessage::codec`].
pub const CODEC_PCM: i32 = 0;
/// Opus-compressed audio buffer.
pub const CODEC_OPUS: i32 = 1;

/// Default TCP control port.
pub const PORT: u16 = 9123;
/// Default UDP audio port (control port + 1).
pub const UDP_PORT: u16 = 9124;
/// mDNS service type advertised by the desktop server.
pub const MDNS_SERVICE_TYPE: &str = "_micyou._tcp.local.";
/// mDNS service type advertised in web (browser) mode.
pub const MDNS_WEB_SERVICE_TYPE: &str = "_micyou-web._tcp.local.";
/// Handshake probe sent by the mobile client after TCP connect.
pub const HANDSHAKE_CLIENT_STR: &[u8] = b"MicYouCheck1";
/// Handshake reply sent by the desktop server.
pub const HANDSHAKE_SERVER_STR: &[u8] = b"MicYouCheck2";

/// Sample encoding of [`micyou::AudioPacketMessage::buffer`] when the codec is
/// [`CODEC_PCM`]. Values mirror `android.media.AudioFormat.ENCODING_*` so the
/// mobile client can forward its capture format verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum AudioFormat {
    /// 16-bit signed little-endian PCM (`ENCODING_PCM_16BIT`).
    Pcm16Bit = 2,
    /// 8-bit unsigned PCM (`ENCODING_PCM_8BIT`).
    Pcm8Bit = 3,
    /// 32-bit little-endian float PCM (`ENCODING_PCM_FLOAT`).
    PcmFloat = 4,
    /// Packed 24-bit signed little-endian PCM (`ENCODING_PCM_24BIT_PACKED`).
    Pcm24BitPacked = 6,
}

impl AudioFormat {
    /// Map the raw wire value; unknown encodings yield `None`.
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            2 => Some(Self::Pcm16Bit),
            3 => Some(Self::Pcm8Bit),
            4 => Some(Self::PcmFloat),
            6 => Some(Self::Pcm24BitPacked),
            _ => None,
        }
    }

    /// Number of whole bytes per sample (8-bit counts 1 byte per sample).
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::Pcm16Bit => 2,
            Self::Pcm8Bit => 1,
            Self::PcmFloat => 4,
            Self::Pcm24BitPacked => 3,
        }
    }

    /// Decode one sample from the front of `bytes` into normalized `f32`
    /// (`-1.0..=1.0`). Returns `None` when the slice is too short.
    pub fn decode_sample(self, bytes: &[u8]) -> Option<f32> {
        match self {
            Self::Pcm16Bit => {
                let b: [u8; 2] = bytes.get(..2)?.try_into().ok()?;
                Some(i16::from_le_bytes(b) as f32 / 32768.0)
            }
            Self::Pcm8Bit => {
                let b = *bytes.first()?;
                Some((b as f32 - 128.0) / 128.0)
            }
            Self::PcmFloat => {
                let b: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
                Some(f32::from_le_bytes(b))
            }
            Self::Pcm24BitPacked => {
                let b: [u8; 3] = bytes.get(..3)?.try_into().ok()?;
                let sample = (b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i8 as i32) << 16);
                Some(sample as f32 / 8388608.0)
            }
        }
    }

    /// Decode a whole interleaved buffer into `f32` samples.
    pub fn decode_buffer(self, bytes: &[u8]) -> Vec<f32> {
        let step = self.bytes_per_sample();
        let mut out = Vec::with_capacity(bytes.len() / step.max(1));
        let mut chunks = bytes.chunks_exact(step);
        for chunk in &mut chunks {
            if let Some(sample) = self.decode_sample(chunk) {
                out.push(sample);
            }
        }
        out
    }
}

/// Codec carried in [`micyou::AudioPacketMessage::codec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Raw PCM described by [`AudioFormat`].
    Pcm,
    /// Opus frames (20 ms packets from the mobile encoder).
    Opus,
}

impl Codec {
    /// Map the raw wire value; unknown codecs fall back to [`Codec::Pcm`]
    /// (legacy senders predate the codec field, whose proto3 default is 0).
    pub fn from_i32(value: i32) -> Self {
        match value {
            CODEC_OPUS => Self::Opus,
            _ => Self::Pcm,
        }
    }
}

impl micyou::AudioPacketMessage {
    /// Typed view of the `codec` field.
    pub fn codec_kind(&self) -> Codec {
        Codec::from_i32(self.codec)
    }

    /// Typed view of the `audio_format` field (capture format; for Opus it is
    /// telemetry only and not used for decoding).
    pub fn format(&self) -> Option<AudioFormat> {
        AudioFormat::from_i32(self.audio_format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magics_match_ascii_tags() {
        assert_eq!(PACKET_MAGIC.to_be_bytes(), *b"MicY");
        assert_eq!(UDP_PACKET_MAGIC.to_be_bytes(), *b"MicU");
    }

    #[test]
    fn audio_format_roundtrips_known_encodings() {
        for (value, expected) in [
            (2, AudioFormat::Pcm16Bit),
            (3, AudioFormat::Pcm8Bit),
            (4, AudioFormat::PcmFloat),
            (6, AudioFormat::Pcm24BitPacked),
        ] {
            assert_eq!(AudioFormat::from_i32(value), Some(expected));
            assert_eq!(expected as i32, value);
        }
        assert_eq!(AudioFormat::from_i32(0), None);
        assert_eq!(AudioFormat::from_i32(5), None);
    }

    #[test]
    fn decodes_pcm16_buffer() {
        let bytes = [0x00, 0x80, 0x00, 0x00, 0x00, 0x40]; // -32768, 0, 16384
        let decoded = AudioFormat::Pcm16Bit.decode_buffer(&bytes);
        assert_eq!(decoded.len(), 3);
        assert!((decoded[0] - -1.0).abs() < 1e-6);
        assert!(decoded[1].abs() < 1e-6);
        assert!((decoded[2] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn decodes_pcm24_packed_buffer() {
        // 8388607 (max positive 24-bit) little-endian packed
        let bytes = [0xff, 0xff, 0x7f];
        let decoded = AudioFormat::Pcm24BitPacked.decode_buffer(&bytes);
        assert_eq!(decoded.len(), 1);
        assert!((decoded[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decodes_pcm8_and_float() {
        let decoded = AudioFormat::Pcm8Bit.decode_buffer(&[0x00, 0x80, 0xff]);
        assert!((decoded[0] - -1.0).abs() < 1e-6);
        assert!(decoded[1].abs() < 1e-6);
        assert!((decoded[2] - (127.0 / 128.0)).abs() < 1e-6);

        let f = 0.25f32.to_le_bytes();
        let decoded = AudioFormat::PcmFloat.decode_buffer(&f);
        assert!((decoded[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn codec_defaults_to_pcm_for_legacy_senders() {
        assert_eq!(Codec::from_i32(0), Codec::Pcm);
        assert_eq!(Codec::from_i32(1), Codec::Opus);
        assert_eq!(Codec::from_i32(42), Codec::Pcm);
    }

    #[test]
    fn packet_accessors_are_typed() {
        let packet = micyou::AudioPacketMessage {
            buffer: Vec::new(),
            sample_rate: 48000,
            channel_count: 1,
            audio_format: 2,
            codec: CODEC_OPUS,
        };
        assert_eq!(packet.codec_kind(), Codec::Opus);
        assert_eq!(packet.format(), Some(AudioFormat::Pcm16Bit));
    }

    #[test]
    fn message_wrapper_serializes_empty_default() {
        use prost::Message;
        let wrapper = micyou::MessageWrapper::default();
        assert_eq!(wrapper.encode_to_vec(), Vec::new());
        assert!(wrapper.audio_packet.is_none());
        assert!(wrapper.plugin_message.is_none());
    }
}
