//! SBS ("BaseStation") output of dump1090: one comma-separated line per
//! decoded message, 22 fields, of which `MSG` lines carry aircraft data.
//!
//! Fields, numbered from 1 as in the format's description: 1 message type
//! (`MSG`), 2 transmission type 1-8, 5 ICAO address (hex), 11 callsign,
//! 12 altitude (ft), 13 ground speed (kt), 14 track (deg), 15 latitude,
//! 16 longitude, 17 vertical rate (ft/min), 18 squawk, 22 is-on-ground
//! (`-1` or `1` true, `0` false). A field a message does not carry is empty.

use serde_json::{Value, json};

/// The event for one SBS line: `{"protocol":"adsb","icao","msg","callsign",
/// "altitude_ft","ground_speed_kt","track_deg","lat","lon",
/// "vertical_rate_fpm","squawk","on_ground"}`, every field the message does
/// not carry `null`. `None` for anything but a well-formed `MSG` line (any
/// trailing `\r\n` is ignored).
pub fn parse_line(line: &str) -> Option<Value> {
    let fields: Vec<&str> = line.trim_end_matches(['\r', '\n']).split(',').collect();
    if fields.first().map(|f| f.trim()) != Some("MSG") {
        return None;
    }
    // 1-based field `n`, trimmed, or `None` when missing or empty.
    let field = |n: usize| {
        fields
            .get(n - 1)
            .map(|f| f.trim())
            .filter(|f| !f.is_empty())
    };
    let msg: u8 = field(2)?.parse().ok().filter(|t| (1..=8).contains(t))?;
    let icao = field(5)?.to_ascii_uppercase();
    let int = |n: usize| {
        field(n).and_then(|f| {
            f.parse::<i64>()
                .ok()
                .or_else(|| f.parse::<f64>().ok().map(|x| x.round() as i64))
        })
    };
    let float = |n: usize| field(n).and_then(|f| f.parse::<f64>().ok());
    let on_ground = field(22).and_then(|f| match f {
        "-1" | "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });
    Some(json!({
        "protocol": "adsb",
        "icao": icao,
        "msg": msg,
        "callsign": field(11),
        "altitude_ft": int(12),
        "ground_speed_kt": float(13),
        "track_deg": float(14),
        "lat": float(15),
        "lon": float(16),
        "vertical_rate_fpm": int(17),
        "squawk": field(18),
        "on_ground": on_ground,
    }))
}
