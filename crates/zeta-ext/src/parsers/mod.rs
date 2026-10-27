//! How an external program's output lines become event fields: one parser per
//! [`Parser`] a row's `output.parser` names. Each takes one line (any trailing
//! `\r\n` ignored) and answers the event's own fields, or `None` for a line it
//! cannot read; the stage adds the envelope (`protocol`, `summary`, `t_ns`).

pub mod json_lines;
pub mod kiss;
pub mod multimon;
pub mod nmea;
pub mod sbs;
pub mod text;

use serde_json::Value;
use zeta_core::modes::Parser;

/// The fields of `line` read with `p`; `None` when `p` cannot read it.
pub fn parse(p: Parser, line: &str) -> Option<Value> {
    let line = line.trim_end_matches(['\r', '\n']);
    match p {
        Parser::JsonLines => json_lines::parse(line),
        Parser::Sbs => sbs::parse_line(line),
        Parser::Nmea => nmea::parse(line),
        Parser::Kiss => kiss::parse(line),
        Parser::Text => text::parse(line),
        Parser::Multimon => multimon::parse(line),
    }
}
