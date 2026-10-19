//! The typed envelope for non-IQ binary frames, the plugin wire's data-frame framing reused:
//! `u32LE header_len | FrameSummary JSON | u32LE body_len | body`. The body is `f32` LE PCM
//! for audio, UTF-8 for text, and empty for event, sink and video (an event's payload is in
//! the header). IQ goes as VRT packets, never in an envelope.

use zeta_core::ops::FrameSummary;
use zeta_core::stream::{Frame, Payload};

/// Longest envelope header, in bytes.
pub const MAX_HEADER: usize = 65_536;
/// Longest envelope body, in bytes.
pub const MAX_BODY: usize = 16_777_216;

/// Wrap a non-IQ frame. An IQ frame, or a header or body over its limit, is an error.
pub fn envelope(f: &Frame) -> Result<Vec<u8>, String> {
    let body: Vec<u8> = match &f.payload {
        Payload::Iq(_) => return Err("iq frames are sent as vrt packets".to_string()),
        Payload::Audio { samples, .. } => samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
        Payload::Text(t) => t.as_bytes().to_vec(),
        Payload::Event(_) | Payload::Sink(_) | Payload::Video => Vec::new(),
    };
    let header = serde_json::to_vec(&FrameSummary::from_frame(f)).map_err(|e| e.to_string())?;
    if header.len() > MAX_HEADER {
        return Err(format!(
            "header of {} bytes exceeds the {MAX_HEADER}-byte limit",
            header.len()
        ));
    }
    if body.len() > MAX_BODY {
        return Err(format!(
            "body of {} bytes exceeds the {MAX_BODY}-byte limit",
            body.len()
        ));
    }
    let mut out = Vec::with_capacity(8 + header.len() + body.len());
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Split `len | bytes` off the front of `bytes`, checking `len` against `max` first.
fn take<'a>(bytes: &'a [u8], what: &str, max: usize) -> Result<(&'a [u8], &'a [u8]), String> {
    let Some((len, rest)) = bytes.split_first_chunk::<4>() else {
        return Err(format!("envelope ended inside the {what} length"));
    };
    let len = u32::from_le_bytes(*len) as usize;
    if len > max {
        return Err(format!(
            "{what} of {len} bytes exceeds the {max}-byte limit"
        ));
    }
    if rest.len() < len {
        return Err(format!(
            "envelope ended inside the {what}: {} of {len} bytes",
            rest.len()
        ));
    }
    Ok(rest.split_at(len))
}

/// Parse an envelope into its header and body. Lengths are checked before anything is
/// allocated; trailing bytes are an error.
pub fn parse_envelope(bytes: &[u8]) -> Result<(FrameSummary, Vec<u8>), String> {
    let (header, rest) = take(bytes, "header", MAX_HEADER)?;
    let (body, rest) = take(rest, "body", MAX_BODY)?;
    if !rest.is_empty() {
        return Err(format!("{} trailing bytes after the body", rest.len()));
    }
    let summary: FrameSummary =
        serde_json::from_slice(header).map_err(|e| format!("header: {e}"))?;
    Ok((summary, body.to_vec()))
}
