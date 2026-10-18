//! HDLC deframing and AX.25 UI-frame parsing.

use crate::crc16_x25;

/// Smallest frame accepted, FCS included: 14 address bytes, control, 2 FCS
/// bytes (an AX.25 S or U frame without PID; a UI frame is at least 18).
const MIN_FRAME_LEN: usize = 17;
/// Largest frame collected (AX.25 allows 256 info bytes plus 10 addresses);
/// longer runs without a flag are discarded.
const MAX_FRAME_LEN: usize = 400;

/// Streaming HDLC deframer over a raw (stuffed) bit stream; state carries
/// across [`feed`](Self::feed) calls.
///
/// Flags are found on the stuffed stream: a 0 after five 1s is dropped, six 1s
/// then a 0 is a flag, seven 1s abort the frame in progress. Bytes are assembled
/// LSB-first. Between two flags, a byte-aligned run of at least 17 bytes is a
/// candidate frame: it is returned (FCS stripped) when its last two bytes,
/// little-endian, equal [`crc16_x25`] of the rest, and counted in
/// [`fcs_failures`](Self::fcs_failures) otherwise. Shorter or unaligned runs
/// (idle flags, noise) are dropped silently.
#[derive(Debug)]
pub struct HdlcDeframer {
    /// Consecutive 1s on the raw stream.
    ones: u32,
    /// True after a flag, until an abort or overflow.
    in_frame: bool,
    /// Unstuffed bits since the last flag, one per entry (includes the flag
    /// prefix `0 1 1 1 1 1` until the flag completes).
    bits: Vec<u8>,
    fcs_failures: u64,
}

impl HdlcDeframer {
    /// A deframer waiting for its first flag.
    pub fn new() -> Self {
        Self {
            ones: 0,
            in_frame: false,
            bits: Vec::with_capacity(MAX_FRAME_LEN * 8),
            fcs_failures: 0,
        }
    }

    /// Feed raw bits (0 or 1; any non-zero is 1); returns the frames, FCS
    /// stripped, completed by this call.
    pub fn feed(&mut self, bits: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for &b in bits {
            if b != 0 {
                self.ones += 1;
                if self.ones >= 7 {
                    self.in_frame = false;
                    self.bits.clear();
                } else if self.ones <= 5 {
                    self.push(1);
                }
                // A sixth 1 is held: it is either a flag or an abort.
            } else {
                match self.ones {
                    5 => {} // stuffed 0
                    6 => {
                        // Flag: its leading `0 1 1 1 1 1` were pushed as data.
                        let len = self.bits.len().saturating_sub(6);
                        self.bits.truncate(len);
                        if self.in_frame
                            && let Some(f) = self.finish()
                        {
                            frames.push(f);
                        }
                        self.bits.clear();
                        self.in_frame = true;
                    }
                    _ => self.push(0),
                }
                self.ones = 0;
            }
        }
        frames
    }

    /// Frames dropped for a bad FCS since construction.
    pub fn fcs_failures(&self) -> u64 {
        self.fcs_failures
    }

    fn push(&mut self, bit: u8) {
        if self.bits.len() >= MAX_FRAME_LEN * 8 + 8 {
            self.in_frame = false;
            self.bits.clear();
        }
        self.bits.push(bit);
    }

    /// Validate the bits between two flags as a frame.
    fn finish(&mut self) -> Option<Vec<u8>> {
        if !self.bits.len().is_multiple_of(8) || self.bits.len() < MIN_FRAME_LEN * 8 {
            return None;
        }
        let bytes: Vec<u8> = self
            .bits
            .chunks_exact(8)
            .map(|c| c.iter().rev().fold(0u8, |acc, &b| (acc << 1) | b))
            .collect();
        let (body, fcs) = bytes.split_at(bytes.len() - 2);
        if u16::from_le_bytes([fcs[0], fcs[1]]) == crc16_x25(body) {
            Some(body.to_vec())
        } else {
            self.fcs_failures += 1;
            None
        }
    }
}

impl Default for HdlcDeframer {
    fn default() -> Self {
        Self::new()
    }
}

/// A parsed AX.25 UI frame. Callsigns are rendered `CALL-SSID`, `CALL` when
/// the SSID is 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ax25Frame {
    pub src: String,
    pub dst: String,
    /// Digipeaters in order (has-been-repeated bit not rendered).
    pub path: Vec<String>,
    pub info: Vec<u8>,
}

/// Most address fields in a frame: destination, source and 8 digipeaters.
const MAX_ADDRS: usize = 10;

/// Parse a UI frame whose FCS has already been stripped (as
/// [`HdlcDeframer::feed`] returns it). Address fields run until one has bit 0
/// of its SSID byte set; SSID bits 7..5 (C/H and reserved) are ignored. Returns
/// `None` unless there are 2 to 10 addresses with printable callsigns, then
/// control 0x03 and PID 0xF0.
pub fn parse_ax25_ui(frame: &[u8]) -> Option<Ax25Frame> {
    let mut addrs = Vec::with_capacity(MAX_ADDRS);
    let mut rest = frame;
    loop {
        if addrs.len() == MAX_ADDRS || rest.len() < 7 {
            return None;
        }
        let (field, tail) = rest.split_at(7);
        addrs.push(decode_addr(field)?);
        rest = tail;
        if field[6] & 1 != 0 {
            break;
        }
    }
    if addrs.len() < 2 {
        return None;
    }
    match rest {
        [0x03, 0xF0, info @ ..] => {
            let mut it = addrs.into_iter();
            let dst = it.next()?;
            let src = it.next()?;
            Some(Ax25Frame {
                src,
                dst,
                path: it.collect(),
                info: info.to_vec(),
            })
        }
        _ => None,
    }
}

/// Decode one 7-byte address field to `CALL` or `CALL-SSID`.
fn decode_addr(field: &[u8]) -> Option<String> {
    let mut call = String::with_capacity(9);
    for &b in &field[..6] {
        let c = b >> 1;
        if !(c.is_ascii_graphic() || c == b' ') {
            return None;
        }
        call.push(c as char);
    }
    let len = call.trim_end_matches(' ').len();
    call.truncate(len);
    if call.is_empty() || call.contains(' ') {
        return None;
    }
    let ssid = (field[6] >> 1) & 0x0F;
    if ssid != 0 {
        call.push('-');
        call.push_str(&ssid.to_string());
    }
    Some(call)
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{ax25_ui_frame, hdlc_frame_bits};

    const INFO: &[u8] = b"!3748.12S/14458.33E>";

    fn frame() -> Vec<u8> {
        ax25_ui_frame("VK3ABC-9", "APRS", &["WIDE1-1", "WIDE2-1"], INFO)
    }

    #[test]
    fn hdlc_deframer_recovers_frame_and_checks_fcs() {
        let f = frame();
        let bits = hdlc_frame_bits(&f, 8, 2);
        let mut d = HdlcDeframer::new();
        // Split mid-frame so state must carry across calls.
        let mut frames = d.feed(&bits[..150]);
        frames.extend(d.feed(&bits[150..]));
        assert_eq!(frames, vec![f[..f.len() - 2].to_vec()]);
        assert_eq!(d.fcs_failures(), 0);
    }

    #[test]
    fn flipped_bit_yields_no_frame() {
        let mut bits = hdlc_frame_bits(&frame(), 8, 2);
        bits[100] ^= 1;
        let mut d = HdlcDeframer::new();
        assert!(d.feed(&bits).is_empty());
        assert_eq!(d.fcs_failures(), 1);
    }

    #[test]
    fn parse_ax25_ui_fields() {
        let f = frame();
        let ax = parse_ax25_ui(&f[..f.len() - 2]).expect("parse");
        assert_eq!(ax.src, "VK3ABC-9");
        assert_eq!(ax.dst, "APRS");
        assert_eq!(ax.path, vec!["WIDE1-1".to_string(), "WIDE2-1".to_string()]);
        assert_eq!(ax.info, INFO);
    }

    #[test]
    fn parse_ax25_ui_ignores_c_bit_in_ssid() {
        // Hand-built: dst "APRS" with SSID byte 0xE0 (C bit set, SSID 0), src
        // "N0CALL-1" with 0x62 | 1 (last address), control, PID, info.
        let mut f: Vec<u8> = b"APRS  ".iter().map(|c| c << 1).collect();
        f.push(0xE0);
        f.extend(b"N0CALL".iter().map(|c| c << 1));
        f.push(0x60 | (1 << 1) | 1);
        f.extend_from_slice(&[0x03, 0xF0]);
        f.extend_from_slice(b">hi");
        let ax = parse_ax25_ui(&f).expect("parse");
        assert_eq!(ax.dst, "APRS");
        assert_eq!(ax.src, "N0CALL-1");
        assert!(ax.path.is_empty());
        assert_eq!(ax.info, b">hi");
    }
}
