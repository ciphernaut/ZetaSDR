//! A classic pcap stream (either byte order, microsecond or nanosecond magic):
//! a 24-byte file header, then records of a 16-byte header and `incl_len` bytes.
//! Every record's bytes become one packet of the header's link type.

use super::{PacketParser, Parsed, StreamError};
use std::collections::BTreeMap;
use zeta_core::stream::LinkType;

const FILE_HEADER: usize = 24;
const RECORD_HEADER: usize = 16;

/// The longest record read, whatever the stream's header says.
const MAX_RECORD: u32 = 262_144;

/// A pcap parser. A bad header or record is a sticky error: the push that meets a
/// bad record after good ones returns those packets, and every later push is the error.
pub struct PcapParser {
    failed: Option<StreamError>,
    link_types: BTreeMap<u16, LinkType>,
    buf: Vec<u8>,
    /// Set once the file header is read: (big-endian, snap length, link type).
    header: Option<(bool, u32, LinkType)>,
}

/// Whether `magic` (as four stream bytes) is big-endian, or an error when it is not pcap.
fn byte_order(magic: [u8; 4]) -> Result<bool, StreamError> {
    match u32::from_le_bytes(magic) {
        0xa1b2_c3d4 | 0xa1b2_3c4d => Ok(false),
        0xd4c3_b2a1 | 0x4d3c_b2a1 => Ok(true),
        _ => Err(StreamError(format!(
            "not a pcap stream (magic {})",
            magic.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ))),
    }
}

fn read_u32(b: &[u8], big: bool) -> u32 {
    let a = [b[0], b[1], b[2], b[3]];
    if big {
        u32::from_be_bytes(a)
    } else {
        u32::from_le_bytes(a)
    }
}

impl PcapParser {
    pub fn new(link_types: BTreeMap<u16, LinkType>) -> Self {
        Self {
            failed: None,
            link_types,
            buf: Vec::new(),
            header: None,
        }
    }

    /// Reads the file header once it is whole; the magic is judged as soon as
    /// its four bytes are here.
    fn read_header(&mut self) -> Result<(), StreamError> {
        if self.buf.len() < 4 {
            return Ok(());
        }
        let big = byte_order([self.buf[0], self.buf[1], self.buf[2], self.buf[3]])?;
        if self.buf.len() < FILE_HEADER {
            return Ok(());
        }
        let snap = read_u32(&self.buf[16..], big);
        // The top four bits of the field carry FCS flags, not the link type.
        let network = read_u32(&self.buf[20..], big) & 0x0fff_ffff;
        let n = u16::try_from(network)
            .map_err(|_| StreamError(format!("pcap link type {network} is out of range")))?;
        let link = self
            .link_types
            .get(&n)
            .copied()
            .unwrap_or_else(|| LinkType::from_number(n));
        self.buf.drain(..FILE_HEADER);
        self.header = Some((big, snap, link));
        Ok(())
    }
}

impl PacketParser for PcapParser {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Parsed>, StreamError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        self.buf.extend_from_slice(bytes);
        if self.header.is_none()
            && let Err(e) = self.read_header()
        {
            self.failed = Some(e.clone());
            return Err(e);
        }
        let Some((big, snap, link)) = self.header else {
            return Ok(Vec::new());
        };
        let limit = if snap == 0 {
            MAX_RECORD
        } else {
            snap.min(MAX_RECORD)
        };
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            let rest = &self.buf[at..];
            if rest.len() < RECORD_HEADER {
                break;
            }
            let incl = read_u32(&rest[8..], big);
            if incl > limit {
                let e = StreamError(format!(
                    "pcap record of {incl} bytes exceeds the limit {limit}"
                ));
                self.failed = Some(e.clone());
                if out.is_empty() {
                    return Err(e);
                }
                break;
            }
            let end = RECORD_HEADER + incl as usize;
            if rest.len() < end {
                break;
            }
            out.push(Parsed {
                link_type: link,
                data: rest[RECORD_HEADER..end].to_vec(),
            });
            at += end;
        }
        self.buf.drain(..at);
        Ok(out)
    }

    fn buffered(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(v: u32, big: bool) -> [u8; 4] {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    }

    fn header(magic: u32, big: bool, snap: u32, link: u32) -> Vec<u8> {
        let w16 = |v: u16| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut h = Vec::new();
        h.extend(w(magic, big));
        h.extend(w16(2));
        h.extend(w16(4));
        h.extend([0u8; 8]);
        h.extend(w(snap, big));
        h.extend(w(link, big));
        h
    }

    fn record(big: bool, data: &[u8]) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend(w(1, big));
        r.extend(w(2, big));
        r.extend(w(data.len() as u32, big));
        r.extend(w(data.len() as u32, big));
        r.extend(data);
        r
    }

    fn stream(magic: u32, big: bool, link: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let a: Vec<u8> = (0..14).collect();
        let b: Vec<u8> = (100..107).collect();
        let mut s = header(magic, big, 262144, link);
        s.extend(record(big, &a));
        s.extend(record(big, &b));
        (s, a, b)
    }

    fn feed_bytewise(p: &mut dyn PacketParser, s: &[u8]) -> Vec<Parsed> {
        s.iter().flat_map(|b| p.push(&[*b]).unwrap()).collect()
    }

    #[test]
    fn a_pcap_stream_fed_byte_by_byte_gives_each_record() {
        let (s, a, b) = stream(0xa1b2_c3d4, false, 147);
        let mut p = PcapParser::new(BTreeMap::new());
        let got = feed_bytewise(&mut p, &s);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, a);
        assert_eq!(got[1].data, b);
        assert!(got.iter().all(|g| g.link_type == LinkType::User0));
    }

    #[test]
    fn link_types_map_what_the_program_writes() {
        let (s, _, _) = stream(0xa1b2_c3d4, false, 147);
        let mut p = PcapParser::new(BTreeMap::from([(147, LinkType::ModeS)]));
        let got = feed_bytewise(&mut p, &s);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|g| g.link_type == LinkType::ModeS));
    }

    #[test]
    fn a_big_endian_and_a_nanosecond_header_are_read() {
        let (s, a, _) = stream(0xa1b2_c3d4, true, 147);
        let got = PcapParser::new(BTreeMap::new()).push(&s).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, a);
        let (s, a, _) = stream(0xa1b2_3c4d, false, 3);
        let got = PcapParser::new(BTreeMap::new()).push(&s).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, a);
        assert_eq!(got[0].link_type, LinkType::Ax25);
    }

    #[test]
    fn a_stream_that_is_not_pcap_is_an_error() {
        let mut p = PcapParser::new(BTreeMap::new());
        let e = p.push(b"OFF\n").unwrap_err();
        assert_eq!(e.0, "not a pcap stream (magic 4f46460a)");
    }

    #[test]
    fn a_header_with_a_zero_snap_length_still_reads_records() {
        let mut s = header(0xa1b2_c3d4, false, 0, 147);
        s.extend(record(false, &[7u8; 14]));
        let got = PcapParser::new(BTreeMap::new()).push(&s).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, [7u8; 14]);
    }

    #[test]
    fn a_record_over_the_fixed_cap_is_an_error_whatever_the_header_says() {
        let mut s = header(0xa1b2_c3d4, false, 0xffff_ffff, 147);
        let mut r = record(false, &[]);
        r[8..12].copy_from_slice(&300_000u32.to_le_bytes());
        s.extend(r);
        assert!(PcapParser::new(BTreeMap::new()).push(&s).is_err());
    }

    #[test]
    fn packets_before_a_bad_record_are_returned_and_the_next_push_is_the_error() {
        let mut s = header(0xa1b2_c3d4, false, 262144, 147);
        s.extend(record(false, &[1u8; 14]));
        let mut r = record(false, &[]);
        r[8..12].copy_from_slice(&300_000u32.to_le_bytes());
        s.extend(r);
        let mut p = PcapParser::new(BTreeMap::new());
        let got = p.push(&s).unwrap();
        assert_eq!(got.len(), 1);
        let e1 = p.push(&[0]).unwrap_err();
        let e2 = p.push(&[1, 2, 3]).unwrap_err();
        assert_eq!(e1, e2);
        assert!(e1.0.contains("300000"));
    }

    #[test]
    fn a_record_longer_than_the_snap_length_is_an_error() {
        let mut s = header(0xa1b2_c3d4, false, 262144, 147);
        let mut r = record(false, &[]);
        r[8..12].copy_from_slice(&300_000u32.to_le_bytes());
        s.extend(r);
        assert!(PcapParser::new(BTreeMap::new()).push(&s).is_err());
    }
}
