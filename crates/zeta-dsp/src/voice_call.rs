//! What the digital voice stages (`p25_decode`, `dmr_decode`) say about a call (design 38,
//! 5.5): the shared fields of a call event and the channel event, under the same names in
//! every voice row. A stage adds its own fields (P25's `nac`, DMR's `color_code`) to the
//! map these give.
//!
//! A call's events say whether its speech is heard (`audio`) and, when it is not, why
//! (`silent`, design 38, 5.3).

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Map, Value};
use zeta_core::stage::StageContext;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Provenance, Span, StreamId, Timestamp};

/// The vocoder program the voice rows name as their handler 0
/// (<https://github.com/ciphernaut/zeta-mbe>).
pub const ZETA_MBE: &str = "zeta-mbe";
/// Speech samples the vocoder gives per frame (20 ms at [`MBE_RATE_HZ`]).
pub const MBE_FRAME_SAMPLES: usize = 160;
/// The rate of the vocoder's speech, Hz.
pub const MBE_RATE_HZ: u64 = 8000;
/// The script that builds and installs [`ZETA_MBE`], and the library it links, for a
/// stage run without a row to say so.
const INSTALL_SCRIPT: &str = "tools/install-vocoders.sh";
const INSTALL_URL: &str = "https://github.com/ciphernaut/zeta-mbe";

/// The job-context note for calls heard without their vocoder: the program and how to
/// get it, from the row's handler 0 when the stage runs one.
pub fn no_vocoder_note(ctx: &StageContext) -> String {
    let install = ctx
        .mode
        .as_ref()
        .and_then(|row| row.handler_blocks().first().map(|b| b.install.clone()));
    let (how, url) = match &install {
        Some(i) => (i.hint(), i.url.as_str()),
        None => (Some(format!("bash {INSTALL_SCRIPT}")), INSTALL_URL),
    };
    let how = match how {
        Some(h) => format!("install it with `{h}` from a source checkout ({url})"),
        None => format!("install it from {url}"),
    };
    let var = zeta_core::modes::handlers::handler_vars(&[ZETA_MBE.to_string()]).remove(0);
    format!(
        "{ZETA_MBE} not found: calls are reported with no audio (silent: no_vocoder); \
         {how}; the node finds it on PATH or by {var}, from the next call on"
    )
}

/// The vocoder looked up at each call (so a program installed while the job runs is used
/// from the next call), its absence noted once in the job's context until it is found.
#[derive(Default)]
pub struct ProgramLookup {
    /// Whether the missing vocoder has been noted since it was last found.
    said_missing: bool,
}

impl ProgramLookup {
    /// The row's handler 0 ([`ZETA_MBE`] without a row), looked up now; when it is absent
    /// and not noted yet, [`no_vocoder_note`] under `{<protocol>: {"vocoder": …}}`.
    pub fn find(&mut self, ctx: &StageContext, protocol: &str) -> Option<PathBuf> {
        let found = crate::vocoder::program(ctx, 0, ZETA_MBE, false)
            .ok()
            .flatten();
        match &found {
            Some(_) => self.said_missing = false,
            None if !self.said_missing => {
                self.said_missing = true;
                let note = no_vocoder_note(ctx);
                ctx.note(serde_json::json!({protocol: {"vocoder": note}}));
            }
            None => {}
        }
        found
    }
}

/// A stage's events: each `(t_ns, fields)`.
pub type Events = Vec<(u64, Value)>;

/// The events as frames on `out`, each in the row's envelope with `prov` spanning
/// `span_ns` from its time; none before the stage has a provenance.
pub fn publish(
    ctx: &StageContext,
    out: &StreamId,
    prov: Option<&Provenance>,
    protocol: &str,
    events: Events,
    span_ns: u64,
) -> Vec<Frame> {
    let Some(prov) = prov else {
        return Vec::new();
    };
    events
        .into_iter()
        .map(|(t, fields)| {
            let mut p = prov.clone();
            p.span = Span {
                start_ns: Timestamp(t),
                end_ns: Timestamp(t + span_ns),
            };
            Frame::new(
                out.clone(),
                p,
                Payload::Event(crate::util::event(ctx, t, protocol, fields)),
            )
        })
        .collect()
}

/// Whether a group or a unit-to-unit call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Group,
    Private,
}

impl Call {
    /// The event's `call` value: `group` or `private`.
    pub fn word(self) -> &'static str {
        match self {
            Call::Group => "group",
            Call::Private => "private",
        }
    }
}

/// Why a call's speech is not heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Silent {
    /// The vocoder program is not on the host.
    NoVocoder,
    /// The call is encrypted; nothing here decrypts.
    Encrypted,
    /// The vocoder failed to start, exited or stopped answering during the call.
    VocoderFailed,
}

/// The shared fields of a call event (spec 5.5); a stage adds its own to the map.
pub struct CallFields {
    pub protocol: &'static str,
    pub src: u32,
    pub dst: u32,
    /// `group` or `private` ([`Call::word`]).
    pub call: &'static str,
    pub encrypted: bool,
    pub emergency: bool,
    /// `None` means audio is true.
    pub silent: Option<Silent>,
    /// The role of the stream carrying its speech; `None` in a one-voice stage.
    pub stream: Option<&'static str>,
    /// Whether the call's speech is on the job's main stream: now on the opening event,
    /// at any time during the call on the closing one.
    pub program: bool,
    /// Voice frames received so far.
    pub frames: u32,
    /// Bit errors the protocol's own codes corrected so far.
    pub errors: u32,
    /// `true` on the closing event.
    pub eot: bool,
}

impl CallFields {
    /// The fields as an event's map, with `"type": "call"` and `audio` (true when nothing
    /// silences the call).
    pub fn to_json(&self) -> Map<String, Value> {
        let v = serde_json::json!({
            "type": "call",
            "protocol": self.protocol,
            "src": self.src,
            "dst": self.dst,
            "call": self.call,
            "encrypted": self.encrypted,
            "emergency": self.emergency,
            "audio": self.silent.is_none(),
            "silent": self.silent,
            "stream": self.stream,
            "program": self.program,
            "frames": self.frames,
            "errors": self.errors,
            "eot": self.eot,
        });
        match v {
            Value::Object(m) => m,
            _ => unreachable!("an object literal"),
        }
    }
}

/// A channel event (`"type": "channel"`): the network decoded with no call in progress;
/// `control` once trunking signalling has been seen on it. The stage adds its network
/// field.
pub fn channel_event(protocol: &'static str, control: bool) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), "channel".into());
    m.insert("protocol".into(), protocol.into());
    m.insert("control".into(), control.into());
    m
}

/// The voice stages' tests' vocoder: the fake `zeta-mbe` (`tests/fake_zeta_mbe`) as a row's
/// handler, through a link in a temporary directory, absent until installed, so no test
/// sets the process's environment and none sees another's program.
#[cfg(test)]
pub(crate) mod fake_mbe {
    use std::marker::PhantomData;
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::ZETA_MBE;

    /// The mode row a stage's tests run it under.
    pub(crate) trait Row {
        /// The row's JSON text.
        const JSON: &'static str;
    }

    pub(crate) struct Mbe<R: Row> {
        _dir: tempfile::TempDir,
        link: PathBuf,
        _row: PhantomData<R>,
    }

    impl<R: Row> Mbe<R> {
        pub(crate) fn absent() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join(ZETA_MBE);
            Self {
                _dir: dir,
                link,
                _row: PhantomData,
            }
        }

        pub(crate) fn installed() -> Self {
            let m = Self::absent();
            m.install();
            m
        }

        pub(crate) fn install(&self) {
            let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_zeta_mbe");
            std::os::unix::fs::symlink(fake, &self.link).unwrap();
        }

        /// The file the fake keeps beside its link.
        pub(crate) fn beside(&self, suffix: &str) -> PathBuf {
            PathBuf::from(format!("{}.{suffix}", self.link.display()))
        }

        /// The fake's next run exits 1 after 10 frames.
        pub(crate) fn die_once(&self) {
            std::fs::write(self.beside("die"), "").unwrap();
        }

        /// What the fake's runs logged: `start` per run, `eof` per input closed.
        pub(crate) fn log(&self) -> String {
            std::fs::read_to_string(self.beside("log")).unwrap_or_default()
        }

        /// The row with its handler at the link.
        pub(crate) fn row(&self) -> Arc<zeta_core::modes::ModeRow> {
            let mut v: Value = serde_json::from_str(R::JSON).unwrap();
            v["decoder"]["handlers"][0]["programs"] = json!([self.link.to_str().unwrap()]);
            Arc::new(zeta_core::modes::parse(&v.to_string()).unwrap())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(silent: Option<Silent>) -> CallFields {
        CallFields {
            protocol: "p25",
            src: 9_990_001,
            dst: 65_000,
            call: Call::Group.word(),
            encrypted: false,
            emergency: false,
            silent,
            stream: None,
            program: true,
            frames: 9,
            errors: 1,
            eot: false,
        }
    }

    #[test]
    fn call_fields_serialise_as_the_spec_names_them() {
        let m = fields(None).to_json();
        let mut keys: Vec<&str> = m.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut want = [
            "protocol",
            "src",
            "dst",
            "call",
            "encrypted",
            "emergency",
            "type",
            "audio",
            "silent",
            "stream",
            "program",
            "frames",
            "errors",
            "eot",
        ];
        want.sort_unstable();
        assert_eq!(keys, want);
        assert_eq!(m["type"], "call");
        assert_eq!(m["call"], "group");
        assert_eq!(m["audio"], true);
        assert_eq!(m["silent"], Value::Null);
        assert_eq!(m["stream"], Value::Null);
        assert_eq!(m["src"], 9_990_001);

        for (silent, word) in [
            (Silent::NoVocoder, "no_vocoder"),
            (Silent::Encrypted, "encrypted"),
            (Silent::VocoderFailed, "vocoder_failed"),
        ] {
            let m = fields(Some(silent)).to_json();
            assert_eq!((&m["audio"], &m["silent"]), (&false.into(), &word.into()));
        }
        assert_eq!(Call::Private.word(), "private");

        let c = channel_event("p25", true);
        assert_eq!(
            serde_json::Value::Object(c),
            serde_json::json!({"type": "channel", "protocol": "p25", "control": true})
        );
    }
}
