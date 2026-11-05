//! Sample formats apps hand to Android, converted to and from the `f32` streams use inside.

/// A PCM encoding of one sample, little-endian as on every Android ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Unsigned 8-bit, centered on 128 (`ENCODING_PCM_8BIT`).
    U8,
    /// Signed 16-bit (`ENCODING_PCM_16BIT`, `AAUDIO_FORMAT_PCM_I16`).
    I16,
    /// Signed 24-bit packed in three bytes (`ENCODING_PCM_24BIT_PACKED`).
    I24,
    /// Signed 32-bit (`ENCODING_PCM_32BIT`).
    I32,
    /// 32-bit float, nominally in [-1, 1] (`ENCODING_PCM_FLOAT`).
    F32,
}

impl Format {
    pub const fn bytes(self) -> usize {
        match self {
            Format::U8 => 1,
            Format::I16 => 2,
            Format::I24 => 3,
            Format::I32 | Format::F32 => 4,
        }
    }

    /// Converts whole samples of `bytes` into `out`, and returns how many.
    pub fn decode(self, bytes: &[u8], out: &mut [f32]) -> usize {
        let samples = bytes.chunks_exact(self.bytes()).zip(out.iter_mut());
        let mut count = 0;
        for (raw, sample) in samples {
            *sample = match self {
                Format::U8 => (f32::from(raw[0]) - 128.0) / 128.0,
                Format::I16 => f32::from(i16::from_le_bytes([raw[0], raw[1]])) / 32_768.0,
                Format::I24 => {
                    // Shifted into the top of an i32, so the sign extends.
                    (i32::from_le_bytes([0, raw[0], raw[1], raw[2]]) >> 8) as f32 / 8_388_608.0
                }
                Format::I32 => {
                    (f64::from(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
                        / 2_147_483_648.0) as f32
                }
                Format::F32 => f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            };
            count += 1;
        }
        count
    }

    /// Converts `samples` into whole samples of `out`, rounding and clipping integers, and returns
    /// how many. Floats pass unchanged, as Android passes them.
    pub fn encode(self, samples: &[f32], out: &mut [u8]) -> usize {
        let mut count = 0;
        for (sample, raw) in samples.iter().zip(out.chunks_exact_mut(self.bytes())) {
            let scaled = |full: f64| (f64::from(*sample) * full).round().clamp(-full, full - 1.0);
            match self {
                Format::U8 => raw[0] = (scaled(128.0) + 128.0) as u8,
                Format::I16 => raw.copy_from_slice(&(scaled(32_768.0) as i16).to_le_bytes()),
                Format::I24 => {
                    raw.copy_from_slice(&(scaled(8_388_608.0) as i32).to_le_bytes()[..3]);
                }
                Format::I32 => {
                    raw.copy_from_slice(&(scaled(2_147_483_648.0) as i32).to_le_bytes());
                }
                Format::F32 => raw.copy_from_slice(&sample.to_le_bytes()),
            }
            count += 1;
        }
        count
    }
}
