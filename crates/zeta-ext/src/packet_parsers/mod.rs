//! How an external program's binary output becomes packets: one incremental
//! parser per [`PacketParserKind`] a row's packet output names. Bytes are pushed
//! as they arrive, in any split; each call answers the packets that completed.

pub mod beast;
pub mod pcap;

use std::collections::BTreeMap;
use zeta_core::modes::PacketParserKind;
use zeta_core::stream::LinkType;

/// One packet cut from a program's binary output.
#[derive(Debug)]
pub struct Parsed {
    pub link_type: LinkType,
    pub data: Vec<u8>,
}

/// Why a stream stopped being readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError(pub String);

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An incremental parser: feed bytes as they arrive, take whole packets out.
pub trait PacketParser: Send {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Parsed>, StreamError>;
    /// How many pushed bytes are held, waiting for the rest of a packet.
    fn buffered(&self) -> usize;
}

/// The parser for `kind`. `link_types` maps the link-type numbers a pcap header
/// may carry to the link type the node uses for them.
pub fn parser(
    kind: PacketParserKind,
    link_types: &BTreeMap<u16, LinkType>,
) -> Box<dyn PacketParser> {
    match kind {
        PacketParserKind::Pcap => Box::new(pcap::PcapParser::new(link_types.clone())),
        PacketParserKind::Beast => Box::new(beast::BeastParser::default()),
    }
}
