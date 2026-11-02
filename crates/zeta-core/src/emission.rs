//! ITU emission designators (`16K0F1D`): bandwidth and the three class letters.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::Hz;

/// First symbol: the type of modulation of the main carrier.
const MODULATION: [(char, &str); 18] = [
    ('N', "Unmodulated carrier"),
    ('A', "Amplitude modulation, double sideband"),
    ('H', "Amplitude modulation, single sideband, full carrier"),
    (
        'R',
        "Amplitude modulation, single sideband, reduced or variable carrier",
    ),
    (
        'J',
        "Amplitude modulation, single sideband, suppressed carrier",
    ),
    ('B', "Amplitude modulation, independent sidebands"),
    ('C', "Amplitude modulation, vestigial sideband"),
    ('F', "Frequency modulation"),
    ('G', "Phase modulation"),
    (
        'D',
        "Amplitude and angle modulation, simultaneous or in sequence",
    ),
    ('P', "Unmodulated pulses"),
    ('K', "Pulses modulated in amplitude"),
    ('L', "Pulses modulated in width or duration"),
    ('M', "Pulses modulated in position or phase"),
    ('Q', "Pulses with angle modulation during the pulse"),
    ('V', "Pulses with a combination of modulations"),
    ('W', "Combinations of amplitude, angle and pulse modulation"),
    ('X', "Other cases"),
];

/// Second symbol: the nature of the signal modulating the main carrier.
const NATURE: [(char, &str); 8] = [
    ('0', "No modulating signal"),
    ('1', "Single channel, quantized/digital, no sub-carrier"),
    ('2', "Single channel, quantized/digital, with sub-carrier"),
    ('3', "Single channel, analogue"),
    ('7', "Two or more channels, quantized/digital"),
    ('8', "Two or more channels, analogue"),
    ('9', "Composite, quantized/digital with analogue"),
    ('X', "Other cases"),
];

/// Third symbol: the type of information transmitted.
const INFO: [(char, &str); 9] = [
    ('N', "No information transmitted"),
    ('A', "Telegraphy, for aural reception"),
    ('B', "Telegraphy, for automatic reception"),
    ('C', "Facsimile"),
    ('D', "Data transmission, telemetry, telecommand"),
    ('E', "Telephony, including sound broadcasting"),
    ('F', "Television, video"),
    ('W', "Combination of the above"),
    ('X', "Other cases"),
];

/// A decoded emission designator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Emission {
    pub bandwidth_hz: Hz,
    pub modulation: char,
    pub modulation_name: String,
    pub nature: char,
    pub nature_name: String,
    pub info: char,
    pub info_name: String,
}

fn lookup(table: &[(char, &str)], c: char) -> Option<String> {
    table
        .iter()
        .find(|(k, _)| *k == c)
        .map(|(_, n)| (*n).to_string())
}

/// The bandwidth prefix: three digits and one unit letter (`H K M G`) standing where the
/// decimal point falls (`16K0` = 16.0 kHz, `500H` = 500 Hz, `1M25` = 1.25 MHz).
fn bandwidth(prefix: &[char]) -> Option<u64> {
    if prefix.len() != 4 {
        return None;
    }
    let mut letters = prefix.iter().enumerate().filter(|(_, c)| c.is_alphabetic());
    let (at, unit) = letters.next()?;
    if letters.next().is_some() {
        return None;
    }
    let scale: u64 = match unit {
        'H' => 1,
        'K' => 1_000,
        'M' => 1_000_000,
        'G' => 1_000_000_000,
        _ => return None,
    };
    let mut n = 0u64;
    for (i, c) in prefix.iter().enumerate() {
        if i != at {
            n = n * 10 + u64::from(c.to_digit(10)?);
        }
    }
    let frac_digits = (prefix.len() - 1 - at) as u32;
    Some(n * scale / 10u64.pow(frac_digits))
}

/// Decode an emission designator such as `16K0F1D`; anything malformed is `None`.
pub fn parse_emission(code: &str) -> Option<Emission> {
    let chars: Vec<char> = code.trim().to_uppercase().chars().collect();
    if chars.len() != 7 {
        return None;
    }
    let bw = bandwidth(&chars[..4])?;
    let (modulation, nature, info) = (chars[4], chars[5], chars[6]);
    Some(Emission {
        bandwidth_hz: Hz(bw),
        modulation,
        modulation_name: lookup(&MODULATION, modulation)?,
        nature,
        nature_name: lookup(&NATURE, nature)?,
        info,
        info_name: lookup(&INFO, info)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_16k0f1d_like_the_register_service() {
        let e = parse_emission("16K0F1D  ").unwrap();
        assert_eq!(e.bandwidth_hz, Hz(16_000));
        assert_eq!((e.modulation, e.nature, e.info), ('F', '1', 'D'));
        assert_eq!(e.modulation_name, "Frequency modulation");
        assert_eq!(e.info_name, "Data transmission, telemetry, telecommand");
    }

    #[test]
    fn decodes_the_table_of_codes() {
        for (code, bw, m) in [
            ("16K0F2D", 16_000, 'F'),
            ("25K0G7W", 25_000, 'G'),
            ("2K80J3E", 2_800, 'J'),
            ("6K00A3E", 6_000, 'A'),
            ("1M25F9W", 1_250_000, 'F'),
            ("500HA1A", 500, 'A'),
        ] {
            let e = parse_emission(code).unwrap();
            assert_eq!((e.bandwidth_hz.0, e.modulation), (bw, m), "{code}");
        }
        assert!(parse_emission("").is_none());
        assert!(parse_emission("16K0Z9Z").is_none());
        assert!(parse_emission("16K0F1").is_none());
        assert!(parse_emission("1630F1D").is_none());
    }
}
