//! The Beast binary format (dump1090, readsb): `0x1a`, a type byte (`0x31` Mode
//! A/C with 2 data bytes, `0x32` Mode S short with 7, `0x33` Mode S long with 14),
//! a 6-byte timestamp, a signal byte, then the data; a `0x1a` anywhere after the
//! type byte is doubled. Mode S frames become [`LinkType::ModeS`] packets; Mode
//! A/C frames are consumed. Bytes that are not a frame start are skipped.

use super::{PacketParser, Parsed, StreamError};
use zeta_core::stream::LinkType;

const ESC: u8 = 0x1a;

#[derive(Default)]
pub struct BeastParser {
    buf: Vec<u8>,
}

impl PacketParser for BeastParser {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Parsed>, StreamError> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut at = 0;
        'frames: loop {
            let b = &self.buf[at..];
            // Resynchronise on an `1a` followed by a handled type byte.
            let Some(i) = b.iter().position(|&x| x == ESC) else {
                at = self.buf.len();
                break;
            };
            at += i;
            let b = &self.buf[at..];
            let Some(&t) = b.get(1) else { break };
            let data_len = match t {
                0x31 => 2,
                0x32 => 7,
                0x33 => 14,
                _ => {
                    at += 1;
                    continue;
                }
            };
            // timestamp (6) + signal (1) + data, unescaped.
            let want = 7 + data_len;
            let mut body = Vec::with_capacity(want);
            let mut j = 2;
            while body.len() < want {
                let Some(&x) = b.get(j) else { break 'frames };
                if x == ESC {
                    match b.get(j + 1) {
                        None => break 'frames,
                        Some(&ESC) => {
                            body.push(ESC);
                            j += 2;
                        }
                        // A lone `1a` inside a frame: it was a false start.
                        Some(_) => {
                            at += 1;
                            continue 'frames;
                        }
                    }
                } else {
                    body.push(x);
                    j += 1;
                }
            }
            if data_len > 2 {
                out.push(Parsed {
                    link_type: LinkType::ModeS,
                    data: body[7..].to_vec(),
                });
            }
            at += j;
        }
        self.buf.drain(..at);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of `kind` with synthesised timestamp and signal, `1a` doubled.
    fn frame(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut f = vec![ESC, kind];
        for &b in [1u8, 2, 3, 4, 5, 6, 0x40].iter().chain(data) {
            f.push(b);
            if b == ESC {
                f.push(ESC);
            }
        }
        f
    }

    #[test]
    fn beast_frames_become_mode_s_packets() {
        let long: Vec<u8> = (0x80..0x8e).collect();
        let short: Vec<u8> = (0x20..0x27).collect();
        let mut s = frame(0x33, &long);
        s.extend(frame(0x31, &[0x12, 0x34]));
        s.extend(frame(0x32, &short));
        let mut p = BeastParser::default();
        let got: Vec<Parsed> = s.iter().flat_map(|b| p.push(&[*b]).unwrap()).collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, long);
        assert_eq!(got[1].data, short);
        assert!(got.iter().all(|g| g.link_type == LinkType::ModeS));
    }

    #[test]
    fn an_escaped_1a_in_a_frame_is_unescaped() {
        let mut data: Vec<u8> = (0x80..0x87).collect();
        data[3] = ESC;
        let got = BeastParser::default().push(&frame(0x32, &data)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, data);
    }

    #[test]
    fn garbage_before_a_frame_is_skipped() {
        let data: Vec<u8> = (0x50..0x57).collect();
        let mut s = vec![0x00, 0x99, ESC, 0x34, 0x07, ESC];
        s.extend(frame(0x32, &data));
        let got = BeastParser::default().push(&s).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, data);
    }
}
