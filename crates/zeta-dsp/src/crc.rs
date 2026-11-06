//! A small table of CRC definitions in the Rocksoft parameter model, with a
//! bitwise, table-free evaluator. Frames are searched against it by name.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One CRC algorithm: the Rocksoft model parameters plus its published check
/// value (the CRC of the ASCII bytes `123456789`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrcSpec {
    pub name: &'static str,
    /// Register width in bits, 8..=64.
    pub width: u8,
    pub poly: u64,
    pub init: u64,
    /// Reflect each input byte.
    pub refin: bool,
    /// Reflect the register before the final XOR.
    pub refout: bool,
    pub xorout: u64,
    pub check: u64,
}

/// Bits in a byte.
const BYTE_BITS: u32 = 8;

/// CRC-16/CCITT-FALSE: the RS41's.
pub const CRC16_CCITT: CrcSpec = CrcSpec {
    name: "crc16-ccitt",
    width: 16,
    poly: 0x1021,
    init: 0xFFFF,
    refin: false,
    refout: false,
    xorout: 0,
    check: 0x29B1,
};

/// CRC-16/ARC (IBM).
pub const CRC16_IBM: CrcSpec = CrcSpec {
    name: "crc16-ibm",
    width: 16,
    poly: 0x8005,
    init: 0,
    refin: true,
    refout: true,
    xorout: 0,
    check: 0xBB3D,
};

/// CRC-16/DNP.
pub const CRC16_DNP: CrcSpec = CrcSpec {
    name: "crc16-dnp",
    width: 16,
    poly: 0x3D65,
    init: 0,
    refin: true,
    refout: true,
    xorout: 0xFFFF,
    check: 0xEA82,
};

/// CRC-32 (ISO-HDLC, as in Ethernet and zip).
pub const CRC32: CrcSpec = CrcSpec {
    name: "crc32",
    width: 32,
    poly: 0x04C1_1DB7,
    init: 0xFFFF_FFFF,
    refin: true,
    refout: true,
    xorout: 0xFFFF_FFFF,
    check: 0xCBF4_3926,
};

/// CRC-16/X-25: the AX.25 FCS.
pub const CRC16_X25: CrcSpec = CrcSpec {
    name: "crc16-x25",
    width: 16,
    poly: 0x1021,
    init: 0xFFFF,
    refin: true,
    refout: true,
    xorout: 0xFFFF,
    check: 0x906E,
};

/// CRC-16/KERMIT: ACARS.
pub const CRC16_KERMIT: CrcSpec = CrcSpec {
    name: "crc16-kermit",
    width: 16,
    poly: 0x1021,
    init: 0,
    refin: true,
    refout: true,
    xorout: 0,
    check: 0x2189,
};

/// CRC-16/MODBUS.
pub const CRC16_MODBUS: CrcSpec = CrcSpec {
    name: "crc16-modbus",
    width: 16,
    poly: 0x8005,
    init: 0xFFFF,
    refin: true,
    refout: true,
    xorout: 0,
    check: 0x4B37,
};

/// CRC-8/SMBUS.
pub const CRC8: CrcSpec = CrcSpec {
    name: "crc8",
    width: 8,
    poly: 0x07,
    init: 0,
    refin: false,
    refout: false,
    xorout: 0,
    check: 0xF4,
};

/// Every spec, in search order. The 8-bit one is last: it checks by chance most often.
pub const CRCS: &[CrcSpec] = &[
    CRC16_CCITT,
    CRC16_IBM,
    CRC16_DNP,
    CRC16_MODBUS,
    CRC32,
    CRC16_X25,
    CRC16_KERMIT,
    CRC8,
];

/// A CRC in the Rocksoft model without a name or check value: the general form a
/// stage takes when the CRC is not in the table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CrcDef {
    /// Register width in bits, 8..=64.
    pub width: u8,
    pub poly: u64,
    pub init: u64,
    /// Reflect each input byte.
    pub refin: bool,
    /// Reflect the register before the final XOR.
    pub refout: bool,
    pub xorout: u64,
}

impl From<&CrcSpec> for CrcDef {
    fn from(s: &CrcSpec) -> Self {
        CrcDef {
            width: s.width,
            poly: s.poly,
            init: s.init,
            refin: s.refin,
            refout: s.refout,
            xorout: s.xorout,
        }
    }
}

/// A CRC given by table name or by definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum CrcParam {
    Name(String),
    Def(CrcDef),
}

impl CrcParam {
    /// The definition and its name: the table name, or `crc<width>/<poly in hex>` for
    /// a definition.
    pub fn resolve(&self) -> Result<(CrcDef, String), String> {
        match self {
            CrcParam::Name(n) => by_name(n)
                .map(|s| (CrcDef::from(s), s.name.to_string()))
                .ok_or_else(|| {
                    let names: Vec<&str> = CRCS.iter().map(|s| s.name).collect();
                    format!("crc: unknown name \"{n}\" (one of {})", names.join(", "))
                }),
            CrcParam::Def(d) => {
                if !(8..=64).contains(&d.width) {
                    return Err("crc.width: 8..=64".to_string());
                }
                Ok((d.clone(), format!("crc{}/{:x}", d.width, d.poly)))
            }
        }
    }
}

/// The spec called `name`.
pub fn by_name(name: &str) -> Option<&'static CrcSpec> {
    CRCS.iter().find(|s| s.name == name)
}

/// Reflect the low `bits` bits of `v`.
fn reflect(v: u64, bits: u32) -> u64 {
    v.reverse_bits() >> (u64::BITS - bits)
}

/// The CRC of `data` under `spec`.
pub fn crc(spec: &CrcSpec, data: &[u8]) -> u64 {
    crc_def(&spec.into(), data)
}

/// The CRC of `data` under `spec` (its width must be 8..=64).
pub fn crc_def(spec: &CrcDef, data: &[u8]) -> u64 {
    let width = u32::from(spec.width);
    let mask = u64::MAX >> (u64::BITS - width);
    let top = 1u64 << (width - 1);
    let mut reg = spec.init & mask;
    for &byte in data {
        let b = if spec.refin {
            reflect(u64::from(byte), BYTE_BITS)
        } else {
            u64::from(byte)
        };
        reg ^= b << (width - BYTE_BITS);
        for _ in 0..BYTE_BITS {
            reg = if reg & top != 0 {
                (reg << 1) ^ spec.poly
            } else {
                reg << 1
            } & mask;
        }
    }
    if spec.refout {
        reg = reflect(reg, width);
    }
    (reg ^ spec.xorout) & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_matches_its_check_value() {
        for s in CRCS {
            assert_eq!(crc(s, b"123456789"), s.check, "{}", s.name);
        }
    }

    #[test]
    fn check_values_are_the_published_ones() {
        assert_eq!(by_name("crc16-ccitt").unwrap().check, 0x29B1);
        assert_eq!(by_name("crc16-ibm").unwrap().check, 0xBB3D);
        assert_eq!(by_name("crc16-dnp").unwrap().check, 0xEA82);
        assert_eq!(by_name("crc32").unwrap().check, 0xCBF4_3926);
        assert_eq!(by_name("crc16-x25").unwrap().check, 0x906E);
        assert_eq!(by_name("crc16-kermit").unwrap().check, 0x2189);
        assert!(by_name("crc99").is_none());
    }

    #[test]
    fn the_two_new_specs_check() {
        assert_eq!(by_name("crc8").unwrap().check, 0xF4);
        assert_eq!(by_name("crc16-modbus").unwrap().check, 0x4B37);
        assert_eq!(CRCS.len(), 8);
    }

    #[test]
    fn a_def_evaluates_like_its_spec() {
        for s in CRCS {
            assert_eq!(
                crc_def(&CrcDef::from(s), b"123456789"),
                s.check,
                "{}",
                s.name
            );
        }
    }

    #[test]
    fn crc_param_resolves_names_and_defs() {
        let p: CrcParam = serde_json::from_str("\"crc16-dnp\"").unwrap();
        assert_eq!(
            p.resolve().unwrap(),
            (CrcDef::from(&CRC16_DNP), "crc16-dnp".to_string())
        );
        let p: CrcParam = serde_json::from_str(
            r#"{"width":16,"poly":4129,"init":65535,"refin":false,"refout":false,"xorout":0}"#,
        )
        .unwrap();
        let (def, name) = p.resolve().unwrap();
        assert_eq!(name, "crc16/1021");
        assert_eq!(crc_def(&def, b"123456789"), 0x29B1);
        let bad: CrcParam = serde_json::from_str("\"crc99\"").unwrap();
        assert!(bad.resolve().unwrap_err().starts_with("crc: unknown name"));
        let narrow = CrcParam::Def(CrcDef {
            width: 4,
            ..CrcDef::from(&CRC8)
        });
        assert_eq!(narrow.resolve().unwrap_err(), "crc.width: 8..=64");
    }
}
