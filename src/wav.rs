//! Minimal RIFF/WAVE reader and writer.
//!
//! Enough of the format to get recorded sound into a patch and rendered sound
//! back out: 16- and 24-bit PCM and 32-bit float, mono or stereo, including the
//! WAVE_FORMAT_EXTENSIBLE header that some tools emit.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// How samples are stored in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    Int16,
    Int24,
    Float32,
}

impl SampleFormat {
    pub const NAMES: &'static [&'static str] = &["int16", "int24", "float32"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "int16" | "i16" | "16" => Some(Self::Int16),
            "int24" | "i24" | "24" => Some(Self::Int24),
            "float32" | "f32" | "float" => Some(Self::Float32),
            _ => None,
        }
    }

    fn bits(self) -> u16 {
        match self {
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Float32 => 32,
        }
    }

    fn tag(self) -> u16 {
        match self {
            Self::Int16 | Self::Int24 => 1, // WAVE_FORMAT_PCM
            Self::Float32 => 3,             // WAVE_FORMAT_IEEE_FLOAT
        }
    }
}

/// Interleaved audio, nominally in `-1..1`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Audio {
    pub sample_rate: u32,
    pub channels: usize,
    /// Interleaved: frame 0 channel 0, frame 0 channel 1, frame 1 channel 0, …
    pub samples: Vec<f32>,
}

impl Audio {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            sample_rate,
            channels,
            samples: Vec::new(),
        }
    }

    pub fn mono(sample_rate: u32, samples: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels: 1,
            samples,
        }
    }

    pub fn stereo(sample_rate: u32, left: &[f32], right: &[f32]) -> Self {
        let frames = left.len().min(right.len());
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            samples.push(left[i]);
            samples.push(right[i]);
        }
        Self {
            sample_rate,
            channels: 2,
            samples,
        }
    }

    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels
        }
    }

    pub fn seconds(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f32 / self.sample_rate as f32
        }
    }

    pub fn sample(&self, frame: usize, channel: usize) -> f32 {
        if channel >= self.channels {
            return 0.0;
        }
        self.samples
            .get(frame * self.channels + channel)
            .copied()
            .unwrap_or(0.0)
    }

    /// Average of every channel at `frame`.
    pub fn mixdown(&self, frame: usize) -> f32 {
        if self.channels == 0 {
            return 0.0;
        }
        let sum: f32 = (0..self.channels).map(|c| self.sample(frame, c)).sum();
        sum / self.channels as f32
    }

    /// Largest absolute sample, for reporting clipping headroom.
    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn write(&self, path: impl AsRef<Path>, format: SampleFormat) -> io::Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut file = fs::File::create(path)?;
        file.write_all(&self.encode(format))
    }

    pub fn encode(&self, format: SampleFormat) -> Vec<u8> {
        let channels = self.channels.max(1) as u16;
        let bits = format.bits();
        let block_align = channels * bits / 8;
        let byte_rate = self.sample_rate * u32::from(block_align);
        let data_len = self.samples.len() * usize::from(bits / 8);

        let mut out = Vec::with_capacity(44 + data_len + 1);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&format.tag().to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());

        for sample in &self.samples {
            match format {
                SampleFormat::Int16 => {
                    let v = (clip(*sample) * 32767.0).round() as i16;
                    out.extend_from_slice(&v.to_le_bytes());
                }
                SampleFormat::Int24 => {
                    let v = (clip(*sample) * 8_388_607.0).round() as i32;
                    out.extend_from_slice(&v.to_le_bytes()[..3]);
                }
                SampleFormat::Float32 => out.extend_from_slice(&sample.to_le_bytes()),
            }
        }
        // RIFF chunks are word aligned.
        if data_len % 2 == 1 {
            out.push(0);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(bad("not a RIFF/WAVE file"));
        }
        let mut cursor = 12;
        let mut format = None;
        let mut channels = 0usize;
        let mut sample_rate = 0u32;
        let mut data: Option<&[u8]> = None;

        while cursor + 8 <= bytes.len() {
            let id = &bytes[cursor..cursor + 4];
            let size =
                u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            let body_start = cursor + 8;
            let body_end = body_start.saturating_add(size).min(bytes.len());
            let body = &bytes[body_start..body_end];

            if id == b"fmt " {
                if body.len() < 16 {
                    return Err(bad("fmt chunk is too short"));
                }
                let mut tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap()) as usize;
                sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                // WAVE_FORMAT_EXTENSIBLE hides the real format in its GUID.
                if tag == 0xFFFE {
                    if body.len() < 26 {
                        return Err(bad("extensible fmt chunk is too short"));
                    }
                    tag = u16::from_le_bytes(body[24..26].try_into().unwrap());
                }
                format = Some(match (tag, bits) {
                    (1, 16) => SampleFormat::Int16,
                    (1, 24) => SampleFormat::Int24,
                    (3, 32) => SampleFormat::Float32,
                    (tag, bits) => {
                        return Err(bad(format!(
                            "unsupported WAVE format {tag} at {bits} bits \
                             (supported: 16- and 24-bit PCM, 32-bit float)"
                        )))
                    }
                });
            } else if id == b"data" {
                data = Some(body);
            }

            // Chunk bodies are padded to an even length.
            cursor = body_start + size + (size % 2);
        }

        let format = format.ok_or_else(|| bad("no fmt chunk"))?;
        let data = data.ok_or_else(|| bad("no data chunk"))?;
        if channels == 0 {
            return Err(bad("file claims zero channels"));
        }

        let samples = match format {
            SampleFormat::Int16 => data
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect(),
            SampleFormat::Int24 => data
                .chunks_exact(3)
                .map(|b| {
                    // Sign-extend 24 bits into an i32.
                    let raw = i32::from_le_bytes([b[0], b[1], b[2], 0]) << 8;
                    (raw >> 8) as f32 / 8_388_608.0
                })
                .collect(),
            SampleFormat::Float32 => data
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
        };

        Ok(Self {
            sample_rate,
            channels,
            samples,
        })
    }
}

fn clip(v: f32) -> f32 {
    v.clamp(-1.0, 1.0)
}

fn bad(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize, channels: usize) -> Audio {
        let samples = (0..frames * channels)
            .map(|i| (i as f32 / (frames * channels) as f32) * 1.6 - 0.8)
            .collect();
        Audio {
            sample_rate: 44100,
            channels,
            samples,
        }
    }

    #[test]
    fn float32_round_trips_exactly() {
        let audio = ramp(64, 2);
        let decoded = Audio::decode(&audio.encode(SampleFormat::Float32)).unwrap();
        assert_eq!(decoded, audio);
    }

    #[test]
    fn integer_formats_round_trip_within_their_resolution() {
        // Two LSB: one for rounding, one for the standard 32767/32768
        // asymmetry between encoding and decoding.
        for (format, tolerance) in [
            (SampleFormat::Int16, 2.0 / 32768.0),
            (SampleFormat::Int24, 2.0 / 8_388_608.0),
        ] {
            let audio = ramp(100, 1);
            let decoded = Audio::decode(&audio.encode(format)).unwrap();
            assert_eq!(decoded.sample_rate, audio.sample_rate);
            assert_eq!(decoded.channels, 1);
            assert_eq!(decoded.samples.len(), audio.samples.len());
            for (a, b) in audio.samples.iter().zip(&decoded.samples) {
                assert!((a - b).abs() < tolerance, "{format:?}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn writing_clips_rather_than_wrapping() {
        let audio = Audio::mono(8000, vec![2.0, -2.0]);
        let decoded = Audio::decode(&audio.encode(SampleFormat::Int16)).unwrap();
        assert!((decoded.samples[0] - 1.0).abs() < 1e-3);
        assert!((decoded.samples[1] + 1.0).abs() < 1e-3);
    }

    #[test]
    fn odd_length_data_stays_word_aligned() {
        let audio = Audio::mono(8000, vec![0.5, -0.5, 0.25]);
        let bytes = audio.encode(SampleFormat::Int24);
        assert_eq!(bytes.len() % 2, 0, "RIFF chunks must be word aligned");
        assert_eq!(Audio::decode(&bytes).unwrap().frames(), 3);
    }

    #[test]
    fn unknown_chunks_are_skipped() {
        let audio = ramp(8, 1);
        let mut bytes = audio.encode(SampleFormat::Int16);
        // Splice a LIST chunk in between fmt and data.
        let mut spliced = bytes[..36].to_vec();
        spliced.extend_from_slice(b"LIST");
        spliced.extend_from_slice(&4u32.to_le_bytes());
        spliced.extend_from_slice(b"INFO");
        spliced.extend_from_slice(&bytes[36..]);
        let riff_size = (spliced.len() - 8) as u32;
        spliced[4..8].copy_from_slice(&riff_size.to_le_bytes());
        bytes = spliced;
        assert_eq!(Audio::decode(&bytes).unwrap().frames(), 8);
    }

    #[test]
    fn extensible_headers_are_understood() {
        let audio = ramp(4, 1);
        let plain = audio.encode(SampleFormat::Int16);
        // Rebuild with a 40-byte extensible fmt chunk.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&0xFFFEu16.to_le_bytes());
        bytes.extend_from_slice(&plain[22..36]);
        bytes.extend_from_slice(&22u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM sub-format
        bytes.extend_from_slice(&[0u8; 14]);
        bytes.extend_from_slice(&plain[36..]);
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());

        let decoded = Audio::decode(&bytes).unwrap();
        assert_eq!(decoded.frames(), 4);
        assert_eq!(decoded.sample_rate, 44100);
    }

    #[test]
    fn junk_is_rejected_with_a_readable_reason() {
        assert!(Audio::decode(b"not audio at all").is_err());
        let mut bytes = ramp(4, 1).encode(SampleFormat::Int16);
        // Claim 8-bit PCM, which is not supported.
        bytes[34..36].copy_from_slice(&8u16.to_le_bytes());
        let err = Audio::decode(&bytes).unwrap_err();
        assert!(err.to_string().contains("unsupported"), "{err}");
    }

    #[test]
    fn helpers_describe_the_buffer() {
        let audio = Audio::stereo(48000, &[1.0, 0.0], &[-1.0, 0.5]);
        assert_eq!(audio.frames(), 2);
        assert!((audio.seconds() - 2.0 / 48000.0).abs() < 1e-9);
        assert_eq!(audio.sample(0, 1), -1.0);
        assert_eq!(audio.mixdown(0), 0.0);
        assert_eq!(audio.peak(), 1.0);
    }
}
