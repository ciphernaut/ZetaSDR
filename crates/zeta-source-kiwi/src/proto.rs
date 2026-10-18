//! KiwiSDR SND-stream protocol codec: pure encode/decode, no networking.
//!
//! Strings and byte offsets follow the kiwiclient reference (`kiwi/client.py`,
//! `kiwirecorder.py`) and were confirmed against a KiwiSDR 2 running v1.837.
//! Every parser checks lengths explicitly: a malformed frame from the network is
//! a [`ProtoError`] naming what was wrong, never a panic.

use num_complex::Complex;

/// SND flag: the ADC overflowed while this frame was captured.
pub const SND_FLAG_ADC_OVFL: u8 = 0x02;
/// SND flag: the frame carries interleaved I/Q (IQ mode), preceded by GPS bytes.
pub const SND_FLAG_STEREO: u8 = 0x08;
/// SND flag: the frame is IMA-ADPCM compressed (never the case in IQ mode).
pub const SND_FLAG_COMPRESSED: u8 = 0x10;
/// SND flag: little-endian samples, used by the Kiwi only for camped streams;
/// this client never camps, so [`parse_snd`] ignores it.
pub const SND_FLAG_LITTLE_ENDIAN: u8 = 0x80;

pub const CMD_SQUELCH: &str = "SET squelch=0 max=0";
pub const CMD_GENATTN: &str = "SET genattn=0";
pub const CMD_GEN: &str = "SET gen=0 mix=-1";
pub const CMD_IDENT: &str = "SET ident_user=zetasdr";
pub const CMD_AGC: &str = "SET agc=1 hang=0 thresh=-100 slope=6 decay=1000 manGain=50";
pub const CMD_COMPRESSION_OFF: &str = "SET compression=0";
pub const CMD_KEEPALIVE: &str = "SET keepalive";

/// Bytes before the samples in an IQ-mode SND frame: tag 3, flags 1, seq 4,
/// S-meter 2, GPS 10.
const SND_IQ_HEADER: usize = 20;
/// Longest value echoed back inside a [`ProtoError::BadValue`], in characters.
const MAX_ERR_VALUE: usize = 64;
/// The widest IQ passband edge the Kiwi accepts, in Hz.
const MAX_HALF_BAND_HZ: u64 = 6000;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProtoError {
    #[error("kiwi frame too short: {len} bytes")]
    Short { len: usize },
    #[error("kiwi SND frame is not IQ: flags 0x{flags:02x} lack STEREO (0x08)")]
    NotIq { flags: u8 },
    #[error("kiwi SND frame is compressed; IQ mode must be uncompressed")]
    Compressed,
    #[error("kiwi SND frame has {bytes} sample bytes, not a multiple of 4")]
    OddSamples { bytes: usize },
    #[error("kiwi field {name} has bad value {value:?}")]
    BadValue { name: String, value: String },
    #[error("kiwi MSG is not UTF-8")]
    NotUtf8,
}

fn bad_value(name: &str, value: &str) -> ProtoError {
    let value = match value.char_indices().nth(MAX_ERR_VALUE) {
        Some((i, _)) => format!("{}...", &value[..i]),
        None => value.to_string(),
    };
    ProtoError::BadValue {
        name: name.to_string(),
        value,
    }
}

/// Websocket path of the audio/IQ stream: `/<ts>/SND`.
pub fn snd_path(ts: u32) -> String {
    format!("/{ts}/SND")
}

/// Full websocket URL of the audio/IQ stream: `ws://host:port/<ts>/SND`.
pub fn snd_url(host: &str, port: u16, ts: u32) -> String {
    format!("ws://{host}:{port}{}", snd_path(ts))
}

/// `SET auth t=kiwi p=<password>`; an absent password sends `p=` empty.
/// The caller validates the password (no space, `=` or control character).
pub fn cmd_auth(password: Option<&str>) -> String {
    format!("SET auth t=kiwi p={}", password.unwrap_or(""))
}

/// `SET mod=iq low_cut=-H high_cut=H freq=<kHz>` with `H = ceil(bandwidth/2)`
/// clamped to `1..=6000` and the centre in kHz with three decimals, formatted
/// from integers.
pub fn cmd_mod_iq(center_hz: u64, bandwidth_hz: u64) -> String {
    let half = bandwidth_hz.div_ceil(2).clamp(1, MAX_HALF_BAND_HZ);
    format!(
        "SET mod=iq low_cut=-{half} high_cut={half} freq={}.{:03}",
        center_hz / 1000,
        center_hz % 1000
    )
}

/// `SET AR OK in=<audio_rate> out=44100`, the reply to `MSG audio_rate=`.
pub fn cmd_ar_ok(audio_rate: u32) -> String {
    format!("SET AR OK in={audio_rate} out=44100")
}

/// One recognised `name[=value]` pair of a Kiwi `MSG`.
#[derive(Debug, Clone, PartialEq)]
pub enum KiwiMsg {
    /// `sample_rate=`: the true IQ rate, a decimal float.
    SampleRate(f64),
    /// `audio_rate=`: the nominal integer rate, answered with [`cmd_ar_ok`].
    AudioRate(u32),
    /// `badp=`: 0 is success, 1..=7 are refusals.
    BadPassword(u8),
    /// `too_busy=`: all N client slots are taken.
    TooBusy(u32),
    /// `down`: the server is down.
    Down,
    /// `redirect=`: the URL-decoded target.
    Redirect(String),
    /// `freq_offset=`: kHz, non-zero on transverter Kiwis.
    FreqOffsetKhz(f64),
    /// Any other pair, verbatim.
    Other(String),
}

/// Three-byte frame tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Msg,
    Snd,
    Other([u8; 3]),
}

/// The tag of a websocket payload, or `None` when it is shorter than 3 bytes.
pub fn tag(payload: &[u8]) -> Option<Tag> {
    let t: [u8; 3] = payload.get(..3)?.try_into().ok()?;
    Some(match &t {
        b"MSG" => Tag::Msg,
        b"SND" => Tag::Snd,
        _ => Tag::Other(t),
    })
}

fn expect_tag(payload: &[u8], want: Tag, min_len: usize) -> Result<(), ProtoError> {
    let short = ProtoError::Short { len: payload.len() };
    if payload.len() < min_len {
        return Err(short);
    }
    match tag(payload) {
        Some(got) if got == want => Ok(()),
        Some(_) => Err(bad_value("tag", &String::from_utf8_lossy(&payload[..3]))),
        None => Err(short),
    }
}

fn parse_num<T: std::str::FromStr>(name: &str, value: &str) -> Result<T, ProtoError> {
    value.parse().map_err(|_| bad_value(name, value))
}

fn parse_finite(name: &str, value: &str) -> Result<f64, ProtoError> {
    parse_num::<f64>(name, value)
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| bad_value(name, value))
}

/// Percent-decodes a URL-encoded value; an invalid escape or a non-UTF-8
/// result is an error naming `name`.
fn percent_decode(name: &str, value: &str) -> Result<String, ProtoError> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let byte = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| bad_value(name, value))?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| bad_value(name, value))
}

/// Parses a `MSG` payload: the tag, one space, then space-separated `name` or
/// `name=value` pairs. Empty pairs are skipped.
pub fn parse_msg(payload: &[u8]) -> Result<Vec<KiwiMsg>, ProtoError> {
    expect_tag(payload, Tag::Msg, 3)?;
    let body = std::str::from_utf8(&payload[3..]).map_err(|_| ProtoError::NotUtf8)?;
    let body = body.strip_prefix(' ').unwrap_or(body);
    let mut msgs = Vec::new();
    for pair in body.split(' ').filter(|p| !p.is_empty()) {
        let (name, value) = match pair.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (pair, None),
        };
        let msg = match (name, value) {
            ("sample_rate", Some(v)) => KiwiMsg::SampleRate(parse_finite(name, v)?),
            ("audio_rate", Some(v)) => KiwiMsg::AudioRate(parse_num(name, v)?),
            ("badp", Some(v)) => KiwiMsg::BadPassword(parse_num(name, v)?),
            ("too_busy", Some(v)) => KiwiMsg::TooBusy(parse_num(name, v)?),
            ("down", _) => KiwiMsg::Down,
            ("redirect", Some(v)) => KiwiMsg::Redirect(percent_decode(name, v)?),
            ("freq_offset", Some(v)) => KiwiMsg::FreqOffsetKhz(parse_finite(name, v)?),
            _ => KiwiMsg::Other(pair.to_string()),
        };
        msgs.push(msg);
    }
    Ok(msgs)
}

/// One decoded IQ-mode SND frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Snd {
    pub flags: u8,
    pub seq: u32,
    /// `0.1 * smeter - 127`.
    pub rssi_dbm: f32,
    pub gps_sec: u32,
    pub gps_nsec: u32,
    /// Each component is the `i16` sample divided by 32768.
    pub samples: Vec<Complex<f32>>,
}

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Parses an IQ-mode SND payload. Offsets from the payload start: `[0..3]`
/// `SND`; `[3]` flags; `[4..8]` seq u32 LE; `[8..10]` S-meter u16 BE; `[10]`
/// last GPS solution; `[11]` dummy; `[12..16]` GPS seconds u32 LE; `[16..20]`
/// GPS nanoseconds u32 LE; `[20..]` interleaved I,Q `i16`, big-endian whatever
/// [`SND_FLAG_LITTLE_ENDIAN`] says (it applies only to camped streams).
pub fn parse_snd(payload: &[u8]) -> Result<Snd, ProtoError> {
    expect_tag(payload, Tag::Snd, SND_IQ_HEADER)?;
    let flags = payload[3];
    if flags & SND_FLAG_STEREO == 0 {
        return Err(ProtoError::NotIq { flags });
    }
    if flags & SND_FLAG_COMPRESSED != 0 {
        return Err(ProtoError::Compressed);
    }
    let data = &payload[SND_IQ_HEADER..];
    if !data.len().is_multiple_of(4) {
        return Err(ProtoError::OddSamples { bytes: data.len() });
    }
    let smeter = u16::from_be_bytes([payload[8], payload[9]]);
    // Big-endian always: the reference honours SND_FLAG_LITTLE_ENDIAN only for
    // camped streams, and this client never camps.
    let scale = |b: &[u8]| f32::from(i16::from_be_bytes([b[0], b[1]])) / 32768.0;
    let samples = data
        .chunks_exact(4)
        .map(|s| Complex::new(scale(&s[..2]), scale(&s[2..])))
        .collect();
    Ok(Snd {
        flags,
        seq: le_u32(&payload[4..8]),
        rssi_dbm: (0.1 * f64::from(smeter) - 127.0) as f32,
        gps_sec: le_u32(&payload[12..16]),
        gps_nsec: le_u32(&payload[16..20]),
        samples,
    })
}

/// Builds an IQ-mode SND payload with big-endian samples and zero GPS bytes.
#[cfg(feature = "test-fake")]
pub fn encode_snd(flags: u8, seq: u32, smeter: u16, samples: &[(i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SND_IQ_HEADER + samples.len() * 4);
    out.extend_from_slice(b"SND");
    out.push(flags);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&smeter.to_be_bytes());
    out.extend_from_slice(&[0; 10]);
    for (i, q) in samples {
        out.extend_from_slice(&i.to_be_bytes());
        out.extend_from_slice(&q.to_be_bytes());
    }
    out
}

/// Builds a `MSG` payload: `MSG ` followed by `pairs`.
#[cfg(feature = "test-fake")]
pub fn encode_msg(pairs: &str) -> Vec<u8> {
    let mut out = b"MSG ".to_vec();
    out.extend_from_slice(pairs.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_exact() {
        assert_eq!(cmd_auth(None), "SET auth t=kiwi p=");
        assert_eq!(cmd_auth(Some("pw")), "SET auth t=kiwi p=pw");
        assert_eq!(
            cmd_mod_iq(7_100_000, 10_000),
            "SET mod=iq low_cut=-5000 high_cut=5000 freq=7100.000"
        );
        assert_eq!(
            cmd_mod_iq(7_100_250, 20_000),
            "SET mod=iq low_cut=-6000 high_cut=6000 freq=7100.250"
        );
        assert_eq!(
            cmd_mod_iq(10_000_000, 1),
            "SET mod=iq low_cut=-1 high_cut=1 freq=10000.000"
        );
        assert_eq!(
            cmd_mod_iq(10_000, 0),
            "SET mod=iq low_cut=-1 high_cut=1 freq=10.000"
        );
        assert_eq!(
            cmd_mod_iq(7_100_000, 3),
            "SET mod=iq low_cut=-2 high_cut=2 freq=7100.000"
        );
        assert_eq!(cmd_ar_ok(12000), "SET AR OK in=12000 out=44100");
        assert_eq!(snd_path(1_760_000_000), "/1760000000/SND");
        assert_eq!(
            snd_url("kiwisdr.local", 8073, 1_760_000_000),
            "ws://kiwisdr.local:8073/1760000000/SND"
        );
        assert_eq!(CMD_SQUELCH, "SET squelch=0 max=0");
        assert_eq!(CMD_GENATTN, "SET genattn=0");
        assert_eq!(CMD_GEN, "SET gen=0 mix=-1");
        assert_eq!(CMD_IDENT, "SET ident_user=zetasdr");
        assert_eq!(
            CMD_AGC,
            "SET agc=1 hang=0 thresh=-100 slope=6 decay=1000 manGain=50"
        );
        assert_eq!(CMD_COMPRESSION_OFF, "SET compression=0");
        assert_eq!(CMD_KEEPALIVE, "SET keepalive");
    }

    #[test]
    fn parses_live_msgs() {
        assert_eq!(
            parse_msg(b"MSG sample_rate=11998.902088").unwrap(),
            vec![KiwiMsg::SampleRate(11998.902088)]
        );
        assert_eq!(
            parse_msg(b"MSG audio_init=1 audio_rate=12000").unwrap(),
            vec![
                KiwiMsg::Other("audio_init=1".into()),
                KiwiMsg::AudioRate(12000)
            ]
        );
        assert_eq!(
            parse_msg(b"MSG badp=0").unwrap(),
            vec![KiwiMsg::BadPassword(0)]
        );
        assert_eq!(
            parse_msg(b"MSG badp=1").unwrap(),
            vec![KiwiMsg::BadPassword(1)]
        );
        assert_eq!(
            parse_msg(b"MSG too_busy=8").unwrap(),
            vec![KiwiMsg::TooBusy(8)]
        );
        assert_eq!(parse_msg(b"MSG down").unwrap(), vec![KiwiMsg::Down]);
        assert_eq!(
            parse_msg(b"MSG redirect=http%3A%2F%2Fkiwi2.local%3A8073%2F").unwrap(),
            vec![KiwiMsg::Redirect("http://kiwi2.local:8073/".into())]
        );
        let version = b"MSG version_maj=1 version_min=837 debian_ver=11 model=2 platform=0 hw=1 \
ext_clk=0 freq_offset=0.000 abyy=A26 dx_db_name=dx has_attn=1";
        let msgs = parse_msg(version).unwrap();
        assert!(msgs.contains(&KiwiMsg::FreqOffsetKhz(0.0)));
        assert!(msgs.contains(&KiwiMsg::Other("version_maj=1".into())));
        assert_eq!(msgs.len(), 11);
        assert_eq!(
            parse_msg(b"MSG freq_offset=116000.000").unwrap(),
            vec![KiwiMsg::FreqOffsetKhz(116000.0)]
        );
        assert_eq!(parse_msg(b"MSG").unwrap(), vec![]);
        assert_eq!(
            parse_msg(b"MSG cfg_loaded").unwrap(),
            vec![KiwiMsg::Other("cfg_loaded".into())]
        );
    }

    #[test]
    fn malformed_msgs_are_errors_not_panics() {
        assert!(matches!(
            parse_msg(b"MSG audio_rate=fast"),
            Err(ProtoError::BadValue { name, .. }) if name == "audio_rate"
        ));
        assert!(matches!(
            parse_msg(b"MSG badp=300"),
            Err(ProtoError::BadValue { name, .. }) if name == "badp"
        ));
        assert!(matches!(
            parse_msg(b"MSG sample_rate=NaN"),
            Err(ProtoError::BadValue { name, .. }) if name == "sample_rate"
        ));
        assert!(matches!(
            parse_msg(b"MSG redirect=%zz"),
            Err(ProtoError::BadValue { name, .. }) if name == "redirect"
        ));
        assert!(matches!(parse_msg(b"MSG \xff"), Err(ProtoError::NotUtf8)));
        assert!(matches!(
            parse_msg(b"MS"),
            Err(ProtoError::Short { len: 2 })
        ));
        assert!(matches!(
            parse_msg(b"SND x"),
            Err(ProtoError::BadValue { name, .. }) if name == "tag"
        ));
        let long = format!("MSG audio_rate={}", "9".repeat(10_000));
        let Err(ProtoError::BadValue { value, .. }) = parse_msg(long.as_bytes()) else {
            panic!("expected BadValue");
        };
        assert!(value.len() < 100, "error value is bounded");
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    const LIVE_FRAME_2: &str = "534e440a020000000234ff00c37102008b13561f017604b9060afb72";

    #[test]
    fn parses_live_snd_frame_bytes() {
        let snd = parse_snd(&hex(LIVE_FRAME_2)).unwrap();
        assert_eq!(snd.flags, 0x0a);
        assert_eq!(snd.flags & SND_FLAG_ADC_OVFL, SND_FLAG_ADC_OVFL);
        assert_eq!(snd.seq, 2);
        assert!((snd.rssi_dbm - -70.6).abs() < 0.01, "{}", snd.rssi_dbm);
        assert_eq!(snd.gps_sec, 160_195);
        assert_eq!(snd.gps_nsec, 525_734_795);
        assert_eq!(
            snd.samples,
            vec![
                Complex::new(374.0 / 32768.0, 1209.0 / 32768.0),
                Complex::new(1546.0 / 32768.0, -1166.0 / 32768.0),
            ]
        );
    }

    #[test]
    fn full_live_size_frame_is_512_samples() {
        let frame = encode_snd(0x08, 7, 564, &[(0, 0); 512]);
        assert_eq!(frame.len(), 2068);
        let snd = parse_snd(&frame).unwrap();
        assert_eq!(snd.samples.len(), 512);
        assert_eq!(snd.seq, 7);
        assert_eq!((snd.gps_sec, snd.gps_nsec), (0, 0));
    }

    #[test]
    fn encode_snd_round_trips_the_live_frame() {
        let frame = encode_snd(0x0a, 2, 564, &[(374, 1209), (1546, -1166)]);
        let live = hex(LIVE_FRAME_2);
        assert_eq!(frame[..10], live[..10]);
        assert_eq!(frame[20..], live[20..]);
        assert_eq!(
            encode_msg("badp=0"),
            b"MSG badp=0".to_vec(),
            "encode_msg prefixes the tag and one space"
        );
        assert_eq!(
            parse_msg(&encode_msg("too_busy=4")).unwrap(),
            vec![KiwiMsg::TooBusy(4)]
        );
        assert_eq!(
            parse_snd(&encode_snd(0x08, 1, 0, &[(i16::MIN, i16::MAX)]))
                .unwrap()
                .samples,
            vec![Complex::new(-1.0, 32767.0 / 32768.0)]
        );
    }

    #[test]
    fn little_endian_flag_is_ignored() {
        // The reference only honours 0x80 for camped streams; this port never
        // camps, so samples are big-endian whatever the flag says.
        let mut frame = hex(LIVE_FRAME_2);
        frame[3] = 0x88;
        let snd = parse_snd(&frame).unwrap();
        assert_eq!(snd.flags, 0x88);
        assert_eq!(snd.samples, parse_snd(&hex(LIVE_FRAME_2)).unwrap().samples);
    }

    #[test]
    fn tags_are_recognised() {
        assert_eq!(tag(b"MSG x"), Some(Tag::Msg));
        assert_eq!(tag(b"SND"), Some(Tag::Snd));
        assert_eq!(tag(b"W/F..."), Some(Tag::Other(*b"W/F")));
        assert_eq!(tag(b"SN"), None);
    }

    #[test]
    fn malformed_frames_are_errors_not_panics() {
        let live = hex(LIVE_FRAME_2);
        for len in 0..20 {
            assert!(
                matches!(parse_snd(&live[..len]), Err(ProtoError::Short { len: l }) if l == len),
                "len {len}"
            );
        }
        let mut f = live.clone();
        f[3] = 0x00;
        assert!(matches!(
            parse_snd(&f),
            Err(ProtoError::NotIq { flags: 0x00 })
        ));
        f[3] = 0x18;
        assert!(matches!(parse_snd(&f), Err(ProtoError::Compressed)));
        let mut odd = live[..20].to_vec();
        odd.extend_from_slice(&[1, 2, 3]);
        assert!(matches!(
            parse_snd(&odd),
            Err(ProtoError::OddSamples { bytes: 3 })
        ));
        let mut bad_tag = live.clone();
        bad_tag[..3].copy_from_slice(b"MSG");
        assert!(matches!(
            parse_snd(&bad_tag),
            Err(ProtoError::BadValue { name, .. }) if name == "tag"
        ));
        for len in 0..=live.len() {
            let _ = parse_snd(&live[..len]);
            let _ = parse_msg(&live[..len]);
            let _ = tag(&live[..len]);
        }
        let msg = b"MSG version_maj=1 freq_offset=0.000 badp=0 redirect=a%2 too_busy=8";
        for len in 0..=msg.len() {
            let _ = parse_msg(&msg[..len]);
            let _ = parse_snd(&msg[..len]);
        }
    }
}
