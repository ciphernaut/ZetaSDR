//! A small table of CRC definitions in the Rocksoft parameter model, with a
//! bitwise, table-free evaluator. Frames are searched against it by name.

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

/// Every spec, in search order.
pub const CRCS: &[CrcSpec] = &[
    CRC16_CCITT,
    CRC16_IBM,
    CRC16_DNP,
    CRC32,
    CRC16_X25,
    CRC16_KERMIT,
];

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
        assert!(by_name("crc8").is_none());
    }
}
