//! The WAV a backend is handed.

/// The sample rate every backend expects.
const RATE: u32 = 16_000;

/// `samples_16k` as a 16 kHz mono 16-bit PCM WAV with the canonical 44-byte header.
pub fn wav_s16_mono(samples_16k: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples_16k.len() * 2).expect("WAV data under 4 GiB");
    let mut out = Vec::with_capacity(44 + samples_16k.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples_16k {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_canonical() {
        let wav = wav_s16_mono(&[1, -2, 3]);
        let u16_at = |at: usize| u16::from_le_bytes([wav[at], wav[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
        assert_eq!(wav.len(), 44 + 6);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32_at(4), 36 + 6);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let fmt = (u32_at(16), u16_at(20), u16_at(22), u32_at(24), u16_at(34));
        assert_eq!(fmt, (16, 1, 1, 16_000, 16));
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_at(40), 6);
        assert_eq!(i16::from_le_bytes([wav[46], wav[47]]), -2);
    }
}
