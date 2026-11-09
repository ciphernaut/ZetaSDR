//! Radio selection: which radio an agent request works on when it names none.
//!
//! A request carries an optional [`RadioContext`] (the page's shown radio, `zeta ask`'s
//! flags). [`resolve`] applies the precedence over a fresh `source_list`:
//!
//! 1. a source id the request names as a whole word (one becomes the default, reason
//!    `named`; several keep the policy's default);
//! 2. the [`RadioPolicy`] over the context;
//! 3. no context at all ([`RadioContext::default`]): `free-first` in id order.
//!
//! The [`Selection`] it returns is rendered into the system prompt
//! ([`Selection::prompt_section`]), checks each radio-taking op ([`Selection::admit`]) and
//! is recorded on the transcript. It is also the seam access levels and transmit plug
//! into later: `candidates` is what the caller may select, and `admit` is the one check
//! every radio-taking agent op passes.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeta_core::ops::SourceInfo;
use zeta_core::types::SourceId;

/// The ops that take a radio for work, read through their `source` field. Nothing else
/// is checked by the radio policy: reads (`source_list`, `source_free`, `classify`,
/// `iq_read`) and releases (`source_unpin`, `job_stop`) never are.
pub const RADIO_OPS: &[&str] = &[
    "job_start",
    "plan_start",
    "source_ensure",
    "source_pin",
    "source_set_gain",
];

/// The policy words, in [`RadioPolicy`] order.
pub const RADIO_POLICIES: &[&str] = &["focus", "focus-fallback", "free-first", "ask"];

/// Which radio is the default for a request that names none, and what the agent may
/// do when the default cannot take the band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RadioPolicy {
    /// The focus only; when it cannot take the band, say why and ask, starting nothing.
    #[default]
    Focus,
    /// The focus, else the first free radio, saying which and why.
    FocusFallback,
    /// No default: the first free radio in preference order.
    FreeFirst,
    /// No default: list the free radios and ask which, starting nothing.
    Ask,
}

impl RadioPolicy {
    fn word(self) -> &'static str {
        match self {
            RadioPolicy::Focus => "focus",
            RadioPolicy::FocusFallback => "focus-fallback",
            RadioPolicy::FreeFirst => "free-first",
            RadioPolicy::Ask => "ask",
        }
    }
}

impl fmt::Display for RadioPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// An unknown policy word.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown radio policy {0} (allowed: focus, focus-fallback, free-first, ask)")]
pub struct UnknownRadioPolicy(pub String);

impl FromStr for RadioPolicy {
    type Err = UnknownRadioPolicy;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "focus" => Ok(RadioPolicy::Focus),
            "focus-fallback" => Ok(RadioPolicy::FocusFallback),
            "free-first" => Ok(RadioPolicy::FreeFirst),
            "ask" => Ok(RadioPolicy::Ask),
            other => Err(UnknownRadioPolicy(other.to_string())),
        }
    }
}

/// Where the context's focus came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusFrom {
    /// The radio shown on the page.
    Shown,
    /// The radio last shown on the page, kept while another view (the Map) is shown.
    Kept,
    /// `zeta ask --radio`.
    Flag,
    #[default]
    None,
}

/// A request's radio context. [`RadioContext::default`] is "no context at all"
/// (precedence step 3): policy `free-first`, no focus, no preference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioContext {
    pub focus: Option<SourceId>,
    pub focus_from: FocusFrom,
    pub policy: RadioPolicy,
    pub prefer: Vec<SourceId>,
}

impl Default for RadioContext {
    fn default() -> Self {
        RadioContext {
            focus: None,
            focus_from: FocusFrom::None,
            policy: RadioPolicy::FreeFirst,
            prefer: Vec::new(),
        }
    }
}

impl RadioContext {
    /// A context from optional input: an omitted policy means `focus` when the context
    /// gives a focus, else `free-first`.
    pub fn from_parts(
        focus: Option<SourceId>,
        focus_from: FocusFrom,
        policy: Option<RadioPolicy>,
        prefer: Vec<SourceId>,
    ) -> RadioContext {
        let policy = policy.unwrap_or(if focus.is_some() {
            RadioPolicy::Focus
        } else {
            RadioPolicy::FreeFirst
        });
        RadioContext {
            focus,
            focus_from,
            policy,
            prefer,
        }
    }
}

/// Who asked. Gains the caller's identity and access level once users exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Caller {
    pub transport: String,
}

impl Caller {
    pub fn page() -> Caller {
        Caller {
            transport: "page".into(),
        }
    }
    pub fn http() -> Caller {
        Caller {
            transport: "http".into(),
        }
    }
    pub fn cli() -> Caller {
        Caller {
            transport: "cli".into(),
        }
    }
}

/// Why the default radio is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The request names it.
    Named,
    /// The radio shown on the page.
    Shown,
    /// The radio last shown on the page (the Map is shown).
    Kept,
    /// `zeta ask --radio`.
    Flag,
    /// No default radio.
    #[default]
    None,
}

/// A radio op the gate admitted on a radio other than the default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OffDefault {
    pub tool: String,
    pub source: SourceId,
}

/// The radio selection for one request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Selection {
    /// The default radio for this request.
    pub radio: Option<SourceId>,
    pub reason: Reason,
    /// `focus <id> is not a source`, `focus <id> is faulted: <fault>`.
    pub note: Option<String>,
    /// The policy that chose it.
    pub policy: RadioPolicy,
    pub caller: Caller,
    /// The radios this caller may select, in preference order.
    pub candidates: Vec<SourceId>,
    /// The radios the request names, in id order.
    pub named: Vec<SourceId>,
    /// Radio ops admitted on another radio than the default, filled during the run.
    pub off_default: Vec<OffDefault>,
    /// The context's focus when it is listed, whether or not it is the default (under
    /// `free-first` and `ask` it never is). Picks the paragraph; not on the wire.
    #[serde(skip)]
    focus: Option<SourceId>,
}

/// A char that ends a source id word: anything but ASCII alphanumerics, `-` and `_`.
fn is_boundary(c: char) -> bool {
    !(c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The ids appearing in `request` as whole words, case-insensitive, in `ids` order: an
/// occurrence counts when the chars either side of it (or the start or end of the
/// request) are boundaries. Ids may themselves hold `.` and `:` (`kiwi.1`, `node:b210`).
pub fn named_in(request: &str, ids: &[SourceId]) -> Vec<SourceId> {
    // ASCII lowercasing keeps byte offsets, so slices of `text` line up with `request`.
    let text = request.to_ascii_lowercase();
    let whole = |at: usize, len: usize| {
        let before = text[..at].chars().next_back().is_none_or(is_boundary);
        let after = text[at + len..].chars().next().is_none_or(is_boundary);
        before && after
    };
    ids.iter()
        .filter(|id| {
            let needle = id.0.to_ascii_lowercase();
            !needle.is_empty()
                && text
                    .match_indices(needle.as_str())
                    .any(|(at, m)| whole(at, m.len()))
        })
        .cloned()
        .collect()
}

/// Resolve the request's selection over a fresh `source_list` (precedence in the
/// module doc).
pub fn resolve(
    request: &str,
    ctx: &RadioContext,
    sources: &[SourceInfo],
    caller: Caller,
) -> Selection {
    let mut ids: Vec<SourceId> = sources.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    ids.dedup();

    let mut candidates: Vec<SourceId> = Vec::with_capacity(ids.len());
    for p in &ctx.prefer {
        if ids.contains(p) && !candidates.contains(p) {
            candidates.push(p.clone());
        }
    }
    for id in &ids {
        if !candidates.contains(id) {
            candidates.push(id.clone());
        }
    }

    // The focus: dropped when it is not listed, kept with a note when faulted.
    let mut note = None;
    let focus = match &ctx.focus {
        None => None,
        Some(f) => match sources.iter().find(|s| &s.id == f) {
            None => {
                note = Some(format!("focus {} is not a source", f.0));
                None
            }
            Some(s) => {
                if let Some(fault) = &s.fault {
                    note = Some(format!("focus {} is faulted: {fault}", f.0));
                }
                Some(f.clone())
            }
        },
    };

    let named = named_in(request, &ids);
    let (radio, reason) = if named.len() == 1 {
        (Some(named[0].clone()), Reason::Named)
    } else {
        match (ctx.policy, focus.clone()) {
            (RadioPolicy::Focus | RadioPolicy::FocusFallback, Some(f)) => {
                let reason = match ctx.focus_from {
                    FocusFrom::Kept => Reason::Kept,
                    FocusFrom::Flag => Reason::Flag,
                    FocusFrom::Shown | FocusFrom::None => Reason::Shown,
                };
                (Some(f), reason)
            }
            _ => (None, Reason::None),
        }
    };

    Selection {
        radio,
        reason,
        note,
        policy: ctx.policy,
        caller,
        candidates,
        named,
        off_default: Vec::new(),
        focus,
    }
}

const HEADER: &str = "## This request";
const HEAD_FOCUS: &str = "Default radio: {radio} (policy {policy}: {why}).";
const WHY_SHOWN: &str = "the radio shown on the page";
const WHY_KEPT: &str = "the radio last shown on the page; the Map is shown";
const WHY_FLAG: &str = "the radio `--radio` names";
const HEAD_NAMED: &str = "Default radio: {radio} (the request names {radio}).";
const HEAD_FREE_FIRST: &str = "No default radio (policy free-first).";
const HEAD_ASK: &str = "No default radio (policy ask).";
const HEAD_NO_FOCUS: &str = "No default radio (no radio in focus). Policy {policy}.";
const CANDIDATES: &str = "Radios in preference order: {candidates}.";
const NAMED_SEVERAL: &str = "The request names {named}: choose among them.";
const RULE_FOCUS: &str = "When the request names no radio, work on {radio} only. If {radio} cannot take the band (it does not
cover it, a job or a pin holds it, or it is faulted), start nothing: say why, name the free radios
`source_free` gives in the order above, and ask which to use. Never `replace`.";
const RULE_FOCUS_FALLBACK: &str = "When the request names no radio, work on {radio}. If {radio} cannot take the band (it does not
cover it, a job or a pin holds it, or it is faulted), use the first free radio `source_free` gives in
the order above, never `replace`, and say in your answer which radio you used and why {radio} was not.";
const RULE_NAMED: &str =
    "Work on {radio}, which the request names. If {radio} cannot take the band (it does not cover
it, a job or a pin holds it, or it is faulted), start nothing: say why, name the free radios
`source_free` gives in the order above, and ask which to use. Never `replace`.";
const RULE_FREE: &str = "When the request names no radio, use the first free radio `source_free` gives in the order above
that can take the band, and say in your answer which radio you used. When none can, start nothing
and say why. Never `replace`.";
const RULE_ASK: &str =
    "When the request names no radio, call `source_free` for the band, then ask which of the free
radios to use, in the order above; start nothing. Never `replace`.";
const RULE_NO_FOCUS_ASK: &str =
    "When the request names no radio, call `source_free` for the band, name the free radios in the
order above, and ask which to use; start nothing. Never `replace`.";

fn join(ids: &[SourceId]) -> String {
    if ids.is_empty() {
        return "none".into();
    }
    ids.iter()
        .map(|s| s.0.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Selection {
    /// Check one tool call. Returns the `policy` refusal for a radio op on another radio
    /// than the default under `focus` that the request did not name; otherwise admits it
    /// (`None`). Records nothing: [`Selection::record_applied`] does, once the op applied.
    pub fn admit(&self, tool: &str, args: &Value) -> Option<Value> {
        if !RADIO_OPS.contains(&tool) {
            return None;
        }
        let src = args.get("source")?.as_str()?;
        let radio = self.radio.as_ref()?;
        if src == radio.0 {
            return None;
        }
        if self.policy == RadioPolicy::Focus && !self.named.iter().any(|n| n.0 == src) {
            return Some(json!({
                "error": "policy",
                "field": "source",
                "policy": "focus",
                "default": radio.0,
                "message": format!(
                    "radio policy focus: {src} is not the default radio {}; \
                     the operator names another radio in the request",
                    radio.0
                ),
            }));
        }
        None
    }

    /// Note a radio op that was admitted and applied successfully: recorded in
    /// `off_default` when its radio is not the default (with no default, nothing is off
    /// it). A call that failed never reaches here, so it is never reported as run.
    pub fn record_applied(&mut self, tool: &str, args: &Value) {
        if !RADIO_OPS.contains(&tool) {
            return;
        }
        let (Some(src), Some(radio)) = (args.get("source").and_then(Value::as_str), &self.radio)
        else {
            return;
        };
        if src != radio.0 {
            self.off_default.push(OffDefault {
                tool: tool.to_string(),
                source: SourceId(src.to_string()),
            });
        }
    }

    /// The `## This request` section appended to the system message: the default radio,
    /// the preference order and the rule for when the default cannot take the band.
    pub fn prompt_section(&self) -> String {
        let (head, rule) = match (&self.radio, self.reason, self.policy) {
            (Some(_), Reason::Named, _) => (HEAD_NAMED, RULE_NAMED),
            (Some(_), _, RadioPolicy::FocusFallback) => (HEAD_FOCUS, RULE_FOCUS_FALLBACK),
            (Some(_), _, _) => (HEAD_FOCUS, RULE_FOCUS),
            (None, _, RadioPolicy::Ask) => (HEAD_ASK, RULE_ASK),
            (None, _, RadioPolicy::FreeFirst) if self.focus.is_some() => {
                (HEAD_FREE_FIRST, RULE_FREE)
            }
            (None, _, RadioPolicy::Focus) => (HEAD_NO_FOCUS, RULE_NO_FOCUS_ASK),
            (None, _, _) => (HEAD_NO_FOCUS, RULE_FREE),
        };
        let why = match self.reason {
            Reason::Kept => WHY_KEPT,
            Reason::Flag => WHY_FLAG,
            _ => WHY_SHOWN,
        };
        let radio = self.radio.as_ref().map(|r| r.0.as_str()).unwrap_or("none");
        let fill = |t: &str| {
            t.replace("{radio}", radio)
                .replace("{policy}", self.policy.word())
                .replace("{why}", why)
                .replace("{candidates}", &join(&self.candidates))
                .replace("{named}", &join(&self.named))
        };

        let mut out = format!("{HEADER}\n\n{}\n{}\n", fill(head), fill(CANDIDATES));
        if self.named.len() > 1 {
            out.push_str(&fill(NAMED_SEVERAL));
            out.push('\n');
        }
        out.push_str(&fill(rule));
        out.push('\n');
        if let Some(note) = &self.note {
            out.push_str(&format!("Note: {}.\n", note.trim_end_matches('.')));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat};

    fn id(s: &str) -> SourceId {
        SourceId(s.into())
    }

    fn src(id_: &str, fault: Option<&str>) -> SourceInfo {
        SourceInfo {
            id: id(id_),
            capabilities: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(100_000_000)),
                tune_max_hz: Claim::Asserted(Hz(200_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(2_048_000)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
            },
            current: None,
            holders: vec![],
            elastic: vec![],
            pin: None,
            fault: fault.map(str::to_string),
            bank: None,
            gain: None,
            stats: None,
            worker: None,
            ring: None,
            position: None,
        }
    }

    fn three() -> Vec<SourceInfo> {
        vec![src("b210", None), src("cb", None), src("hackrf", None)]
    }

    fn ctx(focus: Option<&str>, from: FocusFrom, policy: RadioPolicy) -> RadioContext {
        RadioContext {
            focus: focus.map(id),
            focus_from: from,
            policy,
            prefer: vec![],
        }
    }

    const ALL: [RadioPolicy; 4] = [
        RadioPolicy::Focus,
        RadioPolicy::FocusFallback,
        RadioPolicy::FreeFirst,
        RadioPolicy::Ask,
    ];

    /// No "replace" anywhere except inside "never `replace`".
    fn replace_only_as_never(text: &str) -> bool {
        !text
            .to_lowercase()
            .replace("never `replace`", "")
            .contains("replace")
    }

    #[test]
    fn policy_words_round_trip() {
        assert_eq!(
            RADIO_POLICIES,
            ["focus", "focus-fallback", "free-first", "ask"]
        );
        for w in RADIO_POLICIES {
            let p: RadioPolicy = w.parse().unwrap();
            assert_eq!(p.to_string(), *w);
            assert_eq!(serde_json::to_value(p).unwrap(), json!(w));
        }
        let p: RadioPolicy = serde_json::from_value(json!("focus-fallback")).unwrap();
        assert_eq!(p, RadioPolicy::FocusFallback);
        assert_eq!(RadioPolicy::default(), RadioPolicy::Focus);
        let err = "nearest".parse::<RadioPolicy>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown radio policy nearest (allowed: focus, focus-fallback, free-first, ask)"
        );
    }

    #[test]
    fn naming_is_whole_word_and_case_insensitive() {
        let ids = [id("b210"), id("cb"), id("hackrf")];
        assert_eq!(named_in("start APRS on B210", &ids), vec![id("b210")]);
        assert_eq!(named_in("on b2100", &ids), Vec::<SourceId>::new());
        assert_eq!(named_in("cbx and cb-2", &ids), Vec::<SourceId>::new());
        assert_eq!(
            named_in("use CB, then hackrf", &ids),
            vec![id("cb"), id("hackrf")]
        );
        assert_eq!(named_in("144.8 b210:", &ids), vec![id("b210")]);
        assert_eq!(named_in("b210", &ids), vec![id("b210")]);
    }

    #[test]
    fn ids_with_dots_and_colons_can_be_named() {
        let ids = [id("kiwi.1"), id("b210")];
        assert_eq!(named_in("use KIWI.1 now", &ids), vec![id("kiwi.1")]);
        assert_eq!(named_in("kiwi.10", &ids), Vec::<SourceId>::new());
        assert_eq!(named_in("xkiwi.1", &ids), Vec::<SourceId>::new());
        assert_eq!(named_in("on kiwi.1.", &ids), vec![id("kiwi.1")]);

        let ids = [id("node:hackrf"), id("cb")];
        assert_eq!(
            named_in("listen on Node:HackRF, please", &ids),
            vec![id("node:hackrf")]
        );
        assert_eq!(named_in("node:hackrf2", &ids), Vec::<SourceId>::new());
        assert_eq!(named_in("node:hackrf_b", &ids), Vec::<SourceId>::new());

        // Under focus, a named dotted radio is admitted, not refused.
        let sources = vec![src("cb", None), src("kiwi.1", None)];
        let s = resolve(
            "start a measure on kiwi.1",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &sources,
            Caller::page(),
        );
        assert_eq!(s.radio, Some(id("kiwi.1")));
        assert_eq!(s.admit("job_start", &json!({"source": "kiwi.1"})), None);
    }

    #[test]
    fn a_named_radio_wins_under_every_policy() {
        for p in ALL {
            let s = resolve(
                "listen on hackrf",
                &ctx(Some("cb"), FocusFrom::Shown, p),
                &three(),
                Caller::page(),
            );
            assert_eq!(s.radio, Some(id("hackrf")), "{p}");
            assert_eq!(s.reason, Reason::Named, "{p}");
            assert_eq!(s.policy, p);
        }
    }

    #[test]
    fn two_named_radios_keep_the_policy_default() {
        let s = resolve(
            "compare b210 and hackrf",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &three(),
            Caller::page(),
        );
        assert_eq!(s.radio, Some(id("cb")));
        assert_eq!(s.reason, Reason::Shown);
        assert_eq!(s.named, vec![id("b210"), id("hackrf")]);
    }

    #[test]
    fn focus_reads_shown_or_kept() {
        for (from, reason) in [
            (FocusFrom::Shown, Reason::Shown),
            (FocusFrom::Kept, Reason::Kept),
            (FocusFrom::Flag, Reason::Flag),
        ] {
            let s = resolve(
                "start a measure",
                &ctx(Some("cb"), from, RadioPolicy::Focus),
                &three(),
                Caller::page(),
            );
            assert_eq!(s.radio, Some(id("cb")));
            assert_eq!(s.reason, reason);
            assert_eq!(s.note, None);
        }
    }

    #[test]
    fn an_unlisted_focus_is_dropped_with_a_note() {
        let s = resolve(
            "start a measure",
            &ctx(Some("gone"), FocusFrom::Shown, RadioPolicy::Focus),
            &three(),
            Caller::page(),
        );
        assert_eq!(s.radio, None);
        assert_eq!(s.reason, Reason::None);
        assert_eq!(s.note.as_deref(), Some("focus gone is not a source"));
        assert!(
            s.prompt_section()
                .contains("Note: focus gone is not a source")
        );
    }

    #[test]
    fn a_faulted_focus_stays_the_default_with_a_note() {
        let sources = vec![src("b210", None), src("cb", Some("usb gone"))];
        let s = resolve(
            "start a measure",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &sources,
            Caller::page(),
        );
        assert_eq!(s.radio, Some(id("cb")));
        assert_eq!(s.reason, Reason::Shown);
        assert_eq!(s.note.as_deref(), Some("focus cb is faulted: usb gone"));
        assert!(
            s.prompt_section()
                .contains("\nNote: focus cb is faulted: usb gone.\n")
        );

        // A fault that ends in a period does not double it.
        let sources = vec![src("cb", Some("usb gone."))];
        let s = resolve(
            "start a measure",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &sources,
            Caller::page(),
        );
        let p = s.prompt_section();
        assert!(
            p.contains("\nNote: focus cb is faulted: usb gone.\n"),
            "{p}"
        );
        assert!(!p.contains(".."), "{p}");
    }

    #[test]
    fn free_first_and_ask_have_no_default() {
        for p in [RadioPolicy::FreeFirst, RadioPolicy::Ask] {
            let s = resolve(
                "start a measure",
                &ctx(Some("cb"), FocusFrom::Shown, p),
                &three(),
                Caller::http(),
            );
            assert_eq!(s.radio, None, "{p}");
            assert_eq!(s.reason, Reason::None, "{p}");
        }
    }

    #[test]
    fn candidates_follow_prefer_then_id_order() {
        let sources = vec![
            src("tone", None),
            src("cb", None),
            src("hackrf", None),
            src("b210", None),
        ];
        let mut c = ctx(None, FocusFrom::None, RadioPolicy::FreeFirst);
        c.prefer = vec![id("hackrf"), id("gone"), id("cb")];
        let s = resolve("start a measure", &c, &sources, Caller::cli());
        assert_eq!(
            s.candidates,
            vec![id("hackrf"), id("cb"), id("b210"), id("tone")]
        );
        assert!(
            s.prompt_section()
                .contains("Radios in preference order: hackrf, cb, b210, tone.")
        );
    }

    #[test]
    fn no_context_is_free_first() {
        let d = RadioContext::default();
        assert_eq!(d.policy, RadioPolicy::FreeFirst);
        assert_eq!(d.focus, None);
        assert_eq!(d.focus_from, FocusFrom::None);
        let s = resolve("start a measure", &d, &three(), Caller::cli());
        assert_eq!(s.policy, RadioPolicy::FreeFirst);
        assert_eq!(s.radio, None);
        assert_eq!(s.reason, Reason::None);
        assert_eq!(s.note, None);
        assert_eq!(s.caller.transport, "cli");

        let with = RadioContext::from_parts(Some(id("cb")), FocusFrom::Shown, None, vec![]);
        assert_eq!(with.policy, RadioPolicy::Focus);
        assert_eq!(with.focus, Some(id("cb")));
        let without = RadioContext::from_parts(None, FocusFrom::None, None, vec![id("cb")]);
        assert_eq!(without.policy, RadioPolicy::FreeFirst);
        assert_eq!(without.prefer, vec![id("cb")]);
        let given = RadioContext::from_parts(None, FocusFrom::None, Some(RadioPolicy::Ask), vec![]);
        assert_eq!(given.policy, RadioPolicy::Ask);
    }

    #[test]
    fn focus_without_a_usable_focus_asks() {
        for focus in [None, Some("gone")] {
            let s = resolve(
                "start a measure",
                &ctx(focus, FocusFrom::Shown, RadioPolicy::Focus),
                &three(),
                Caller::page(),
            );
            assert_eq!(s.radio, None);
            let p = s.prompt_section();
            assert!(p.contains("No default radio (no radio in focus)"), "{p}");
            assert!(p.contains("ask which to use"), "{p}");
            assert!(p.contains("start nothing"), "{p}");

            let s = resolve(
                "start a measure",
                &ctx(focus, FocusFrom::Shown, RadioPolicy::FocusFallback),
                &three(),
                Caller::page(),
            );
            assert_eq!(s.radio, None);
            let p = s.prompt_section();
            assert!(p.contains("No default radio (no radio in focus)"), "{p}");
            assert!(
                p.contains("use the first free radio `source_free` gives"),
                "{p}"
            );
            assert!(!p.contains("ask which"), "{p}");
        }
    }

    #[test]
    fn strict_refuses_another_radio_on_every_radio_op() {
        let sources = vec![src("cb", None), src("tone", None)];
        let s = resolve(
            "start a measure",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &sources,
            Caller::page(),
        );
        for tool in RADIO_OPS {
            let refused = s.admit(tool, &json!({"source": "tone"}));
            assert_eq!(
                refused,
                Some(json!({
                    "error": "policy",
                    "field": "source",
                    "policy": "focus",
                    "default": "cb",
                    "message": "radio policy focus: tone is not the default radio cb; the operator names another radio in the request"
                })),
                "{tool}"
            );
            assert_eq!(s.admit(tool, &json!({"source": "cb"})), None, "{tool}");
        }
        assert!(s.off_default.is_empty());
    }

    #[test]
    fn strict_admits_a_named_radio() {
        let sources = vec![src("cb", None), src("tone", None), src("hackrf", None)];
        // One named radio becomes the default.
        let s = resolve(
            "start a measure on tone",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &sources,
            Caller::page(),
        );
        assert_eq!(s.admit("job_start", &json!({"source": "tone"})), None);
        // Two named radios: the focus stays the default, and both named are admitted.
        let s = resolve(
            "compare tone and hackrf",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &sources,
            Caller::page(),
        );
        assert_eq!(s.admit("job_start", &json!({"source": "tone"})), None);
        assert_eq!(s.admit("job_start", &json!({"source": "hackrf"})), None);
    }

    #[test]
    fn reads_and_releases_are_never_checked() {
        let sources = vec![src("cb", None), src("tone", None)];
        let mut s = resolve(
            "look around",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
            &sources,
            Caller::page(),
        );
        for tool in [
            "source_list",
            "source_free",
            "source_unpin",
            "classify",
            "job_stop",
        ] {
            assert_eq!(s.admit(tool, &json!({"source": "tone"})), None, "{tool}");
            assert_eq!(
                s.admit(tool, &json!({"selection": {"source": "tone"}})),
                None,
                "{tool}"
            );
            s.record_applied(tool, &json!({"source": "tone"}));
        }
        assert!(s.off_default.is_empty());
    }

    #[test]
    fn focus_fallback_admits_and_records_off_default() {
        let sources = vec![src("cb", None), src("tone", None)];
        let mut s = resolve(
            "start a measure",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::FocusFallback),
            &sources,
            Caller::page(),
        );
        // Admitting records nothing: only an applied op is recorded.
        assert_eq!(s.admit("job_start", &json!({"source": "tone"})), None);
        assert!(s.off_default.is_empty());
        s.record_applied("job_start", &json!({"source": "tone"}));
        assert_eq!(
            s.off_default,
            vec![OffDefault {
                tool: "job_start".into(),
                source: id("tone")
            }]
        );
        assert_eq!(s.admit("job_start", &json!({"source": "cb"})), None);
        s.record_applied("job_start", &json!({"source": "cb"}));
        // Reads are never recorded, applied or not.
        s.record_applied("source_free", &json!({"source": "tone"}));
        assert_eq!(s.off_default.len(), 1);
        assert_eq!(
            serde_json::to_value(&s.off_default).unwrap(),
            json!([{"tool": "job_start", "source": "tone"}])
        );
    }

    #[test]
    fn no_default_records_nothing() {
        let sources = vec![src("cb", None), src("tone", None)];
        let mut s = resolve(
            "start a measure",
            &ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::FreeFirst),
            &sources,
            Caller::http(),
        );
        assert_eq!(s.admit("job_start", &json!({"source": "tone"})), None);
        s.record_applied("job_start", &json!({"source": "tone"}));
        assert!(s.off_default.is_empty());
    }

    #[test]
    fn the_selection_serializes_field_for_field() {
        let s = resolve(
            "start a measure",
            &ctx(Some("cb"), FocusFrom::Kept, RadioPolicy::Focus),
            &three(),
            Caller::page(),
        );
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({
                "radio": "cb",
                "reason": "kept",
                "note": null,
                "policy": "focus",
                "caller": {"transport": "page"},
                "candidates": ["b210", "cb", "hackrf"],
                "named": [],
                "off_default": []
            })
        );
    }

    #[test]
    fn the_paragraph_names_the_default_and_the_rule() {
        let sources = vec![src("b210", None), src("cb", None), src("hackrf", None)];
        let section = |request: &str, c: RadioContext| {
            resolve(request, &c, &sources, Caller::page()).prompt_section()
        };
        let mut all = Vec::new();

        let p = section(
            "start a measure",
            ctx(Some("cb"), FocusFrom::Kept, RadioPolicy::Focus),
        );
        assert!(p.starts_with("## This request"), "{p}");
        assert!(
            p.contains(
                "Default radio: cb (policy focus: the radio last shown on the page; the Map is shown)"
            ),
            "{p}"
        );
        assert!(p.contains("work on cb only"), "{p}");
        assert!(p.contains("ask which to use"), "{p}");
        all.push(p);

        // Spec 4.4's example, verbatim (with b210 in focus and prefer b210, hackrf).
        let mut c = ctx(Some("b210"), FocusFrom::Kept, RadioPolicy::Focus);
        c.prefer = vec![id("b210"), id("hackrf")];
        let p = resolve(
            "start a measure",
            &c,
            &[src("b210", None), src("hackrf", None)],
            Caller::page(),
        )
        .prompt_section();
        assert_eq!(
            p,
            "## This request\n\n\
             Default radio: b210 (policy focus: the radio last shown on the page; the Map is shown).\n\
             Radios in preference order: b210, hackrf.\n\
             When the request names no radio, work on b210 only. If b210 cannot take the band (it does not\n\
             cover it, a job or a pin holds it, or it is faulted), start nothing: say why, name the free radios\n\
             `source_free` gives in the order above, and ask which to use. Never `replace`.\n"
        );

        let p = section(
            "start a measure",
            ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
        );
        assert!(p.contains("Default radio: cb (policy focus: the radio shown on the page)"));
        all.push(p);

        let p = section(
            "start a measure",
            ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::FocusFallback),
        );
        assert!(
            p.contains("Default radio: cb (policy focus-fallback"),
            "{p}"
        );
        assert!(
            p.contains("use the first free radio `source_free` gives"),
            "{p}"
        );
        assert!(p.contains("say in your answer which radio you used"), "{p}");
        assert!(p.contains("why cb was not"), "{p}");
        all.push(p);

        let p = section(
            "start a measure",
            ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::FreeFirst),
        );
        assert!(p.contains("No default radio (policy free-first)"), "{p}");
        all.push(p);

        let p = section(
            "start a measure",
            ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Ask),
        );
        assert!(p.contains("No default radio (policy ask)"), "{p}");
        assert!(
            p.contains("call `source_free` for the band, then ask which"),
            "{p}"
        );
        all.push(p);

        let p = section(
            "start a measure on hackrf",
            ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
        );
        assert!(
            p.contains("Default radio: hackrf (the request names hackrf)"),
            "{p}"
        );
        all.push(p);

        let p = section(
            "compare b210 and hackrf",
            ctx(Some("cb"), FocusFrom::Shown, RadioPolicy::Focus),
        );
        assert!(p.contains("The request names b210, hackrf"), "{p}");
        all.push(p);

        let p = section("start a measure", RadioContext::default());
        assert!(p.contains("No default radio (no radio in focus)"), "{p}");
        assert!(
            p.contains("use the first free radio `source_free` gives"),
            "{p}"
        );
        all.push(p);

        for p in &all {
            assert!(p.starts_with("## This request\n\n"), "{p}");
            assert!(
                p.contains("Radios in preference order: b210, cb, hackrf."),
                "{p}"
            );
            assert!(replace_only_as_never(p), "{p}");
            assert!(p.to_lowercase().contains("never `replace`"), "{p}");
        }
    }
}
