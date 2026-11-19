//! A pcapng writer for packet captures (Wireshark's file format).
//!
//! Pure functions that append blocks to a `Vec<u8>`; no I/O. A file is one
//! Section Header Block, one Interface Description Block and any number of
//! Enhanced Packet Blocks, all little-endian, every block laid out as
//! `type, total_length, body, total_length` with the total a multiple of 4.

use crate::stream::LinkType;

const SHB: u32 = 0x0A0D_0D0A;
const IDB: u32 = 1;
const EPB: u32 = 6;

fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

fn put_option(out: &mut Vec<u8>, code: u16, value: &[u8]) {
    out.extend_from_slice(&code.to_le_bytes());
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
    out.resize(out.len() + pad4(value.len()) - value.len(), 0);
}

fn end_options(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0, 0, 0, 0]);
}

/// Append one block of `kind` whose body is built by `body`; the framing
/// (type, both total lengths) is filled in here.
fn block(out: &mut Vec<u8>, kind: u32, body: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    body(out);
    let total = (out.len() - start + 4) as u32;
    out[start + 4..start + 8].copy_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
}

/// Append the file header: a Section Header Block (little-endian, version
/// 1.0, unknown section length, application `zeta`) and one Interface
/// Description Block for `link_type` with nanosecond timestamps.
pub fn header(out: &mut Vec<u8>, link_type: LinkType) {
    block(out, SHB, |o| {
        o.extend_from_slice(&0x1A2B_3C4Du32.to_le_bytes());
        o.extend_from_slice(&1u16.to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes());
        o.extend_from_slice(&(-1i64).to_le_bytes());
        put_option(o, 4, b"zeta");
        end_options(o);
    });
    block(out, IDB, |o| {
        o.extend_from_slice(&link_type.number().to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes());
        o.extend_from_slice(&0u32.to_le_bytes());
        put_option(o, 9, &[9]);
        end_options(o);
    });
}

/// Append one Enhanced Packet Block on interface 0, stamped `t_ns`
/// nanoseconds since the Unix epoch, carrying `data` and, when not empty,
/// `comment` as an `opt_comment`.
pub fn packet(out: &mut Vec<u8>, t_ns: u64, data: &[u8], comment: &str) {
    block(out, EPB, |o| {
        o.extend_from_slice(&0u32.to_le_bytes());
        o.extend_from_slice(&((t_ns >> 32) as u32).to_le_bytes());
        o.extend_from_slice(&(t_ns as u32).to_le_bytes());
        o.extend_from_slice(&(data.len() as u32).to_le_bytes());
        o.extend_from_slice(&(data.len() as u32).to_le_bytes());
        o.extend_from_slice(data);
        o.resize(o.len() + pad4(data.len()) - data.len(), 0);
        if !comment.is_empty() {
            put_option(o, 1, comment.as_bytes());
        }
        end_options(o);
    });
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// The total length of the whole block at the start of `b`, if there is one.
fn whole_block_len(b: &[u8]) -> Option<usize> {
    if b.len() < 12 {
        return None;
    }
    let len = u32_at(b, 4) as usize;
    if len < 12 || !len.is_multiple_of(4) || len > b.len() || u32_at(b, len - 4) as usize != len {
        return None;
    }
    Some(len)
}

/// The length of the longest prefix of `bytes` made of whole blocks (each
/// block's leading and trailing total length agree and fit). It is 0 when
/// the first block is not a whole Section Header Block.
pub fn whole_blocks(bytes: &[u8]) -> usize {
    if bytes.len() < 4 || u32_at(bytes, 0) != SHB {
        return 0;
    }
    let mut at = 0;
    while let Some(len) = whole_block_len(&bytes[at..]) {
        at += len;
    }
    at
}

/// One packet as [`read`] returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Nanoseconds since the Unix epoch.
    pub t_ns: u64,
    /// The packet bytes.
    pub data: Vec<u8>,
    /// The packet's `opt_comment`, if it has one.
    pub comment: Option<String>,
}

/// Read a one-section, one-interface file as [`header`] and [`packet`] make
/// it: the link type number and the packets. For tests; nothing reads
/// captures at run time.
pub fn read(bytes: &[u8]) -> Result<(u16, Vec<Packet>), String> {
    let mut at = 0;
    let mut link = None;
    let mut packets = Vec::new();
    while at < bytes.len() {
        let len = whole_block_len(&bytes[at..]).ok_or_else(|| format!("bad block at {at}"))?;
        let b = &bytes[at..at + len];
        match u32_at(b, 0) {
            SHB => {
                if at != 0 || u32_at(b, 8) != 0x1A2B_3C4D {
                    return Err("bad section header".into());
                }
            }
            IDB => {
                if link.is_some() || len < 20 {
                    return Err("bad interface block".into());
                }
                link = Some(u16::from_le_bytes([b[8], b[9]]));
            }
            EPB => {
                if link.is_none() || len < 32 {
                    return Err("packet before interface".into());
                }
                let t_ns = (u64::from(u32_at(b, 12)) << 32) | u64::from(u32_at(b, 16));
                let cap = u32_at(b, 20) as usize;
                let end = 28 + cap;
                if end > len - 4 {
                    return Err("packet data overruns its block".into());
                }
                let data = b[28..end].to_vec();
                let mut comment = None;
                let mut o = 28 + pad4(cap);
                while o + 4 <= len - 4 {
                    let code = u16::from_le_bytes([b[o], b[o + 1]]);
                    let n = u16::from_le_bytes([b[o + 2], b[o + 3]]) as usize;
                    if code == 0 {
                        break;
                    }
                    if o + 4 + n > len - 4 {
                        return Err("option overruns its block".into());
                    }
                    if code == 1 {
                        let text =
                            std::str::from_utf8(&b[o + 4..o + 4 + n]).map_err(|e| e.to_string())?;
                        comment = Some(text.to_string());
                    }
                    o += 4 + pad4(n);
                }
                packets.push(Packet {
                    t_ns,
                    data,
                    comment,
                });
            }
            other => return Err(format!("unexpected block type {other}")),
        }
        at += len;
    }
    link.map(|l| (l, packets))
        .ok_or_else(|| "no interface block".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_reads_back_what_was_written() {
        let mut f = Vec::new();
        header(&mut f, LinkType::Ax25Kiss);
        packet(
            &mut f,
            1_700_000_000_123_456_789,
            &[0, 1, 2, 3, 4],
            "job=J source=S freq_hz=144800000",
        );
        packet(&mut f, 1_700_000_001_000_000_000, &[9], "");
        let (lt, p) = read(&f).unwrap();
        assert_eq!(lt, 202);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].t_ns, 1_700_000_000_123_456_789);
        assert_eq!(p[0].data, [0, 1, 2, 3, 4]);
        assert_eq!(
            p[0].comment.as_deref(),
            Some("job=J source=S freq_hz=144800000")
        );
        assert_eq!(p[1].data, [9]);
        assert_eq!(p[1].comment, None);
    }

    #[test]
    fn the_section_header_is_byte_exact() {
        let mut f = Vec::new();
        header(&mut f, LinkType::User0);
        assert_eq!(
            &f[..12],
            [
                0x0a, 0x0d, 0x0d, 0x0a, 0x28, 0, 0, 0, 0x4d, 0x3c, 0x2b, 0x1a
            ]
        );
        assert_eq!(f.len() % 4, 0);
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }

    #[test]
    fn every_block_is_padded_and_its_two_lengths_agree() {
        let mut f = Vec::new();
        header(&mut f, LinkType::RawIp);
        for n in 1..=3usize {
            packet(&mut f, n as u64, &vec![7; n], "c");
        }
        let mut at = 0;
        let mut types = Vec::new();
        while at < f.len() {
            let len = u32_at(&f, at + 4) as usize;
            assert_eq!(len % 4, 0);
            assert_eq!(u32_at(&f, at + len - 4) as usize, len);
            types.push(u32_at(&f, at));
            at += len;
        }
        assert_eq!(at, f.len());
        assert_eq!(types, [0x0A0D0D0A, 1, 6, 6, 6]);
    }

    #[test]
    fn whole_blocks_stops_before_a_cut_block() {
        let mut f = Vec::new();
        header(&mut f, LinkType::Ax25Kiss);
        packet(&mut f, 1, &[1, 2, 3], "a");
        let second = f.len();
        packet(&mut f, 2, &[4, 5, 6, 7, 8], "b");
        assert_eq!(whole_blocks(&f), f.len());
        for cut in second..f.len() {
            assert_eq!(whole_blocks(&f[..cut]), second, "cut at {cut}");
        }
        assert_eq!(whole_blocks(&f[..5]), 0);
        assert_eq!(whole_blocks(&[]), 0);
        assert_eq!(whole_blocks(&f[second..]), 0);
    }
}
