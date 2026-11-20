//! The handler policy: which of a mode's handlers a start gets, and the operator's
//! rankings it reads (`handler_policy_set`, kept in `handler-policy.json` in the state
//! directory).

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{LoadedMode, ModeHandler, ModeState};
use crate::types::StreamKind;

/// The operator's rankings of handlers: per mode, and node-wide. An id a ranking names
/// goes before every id it does not name, in the ranking's order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = super::require_all)]
pub struct HandlerPolicy {
    /// Each mode's ranking, by mode id; a mode with none is left out. Read as `{}` when
    /// absent (a hand-written file); always written (the schema requires it).
    #[serde(default)]
    pub modes: BTreeMap<String, Vec<String>>,
    /// The node-wide ranking, read after the mode's own; `[]` for none, and when absent.
    #[serde(default)]
    pub all: Vec<String>,
}

/// A handler the policy passed over, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Passed {
    /// The handler's id.
    pub id: String,
    /// Why it was passed over: its `reason` when it is not ready (`broken` or
    /// `unavailable` when it has none), `outputs no <kind>` when it lacks a kind the
    /// start needs.
    pub why: String,
}

/// The handler a start of `mode` gets: among `handlers`, those ready and giving every
/// kind of `needs`, ordered by the mode's ranking, the node-wide ranking, in-process
/// first, then `handlers`' own order.
///
/// With none left, every handler and why it was passed over, in `handlers`' order.
pub fn pick<'a>(
    mode: &str,
    handlers: &'a [ModeHandler],
    needs: &[StreamKind],
    policy: &HandlerPolicy,
) -> Result<&'a ModeHandler, Vec<Passed>> {
    let none: &[String] = &[];
    let ranking = policy.modes.get(mode).map_or(none, Vec::as_slice);
    let rank = |list: &[String], id: &str| list.iter().position(|r| r == id).unwrap_or(usize::MAX);
    handlers
        .iter()
        .enumerate()
        .filter(|(_, h)| passed_over(h, needs).is_none())
        .min_by_key(|(i, h)| {
            (
                rank(ranking, &h.id),
                rank(&policy.all, &h.id),
                !h.in_process,
                *i,
            )
        })
        .map(|(_, h)| h)
        .ok_or_else(|| {
            handlers
                .iter()
                .filter_map(|h| {
                    passed_over(h, needs).map(|why| Passed {
                        id: h.id.clone(),
                        why,
                    })
                })
                .collect()
        })
}

/// Why [`choose`] found no handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The named handler is none of the mode's; its handlers' ids.
    Unknown(Vec<String>),
    /// The named handler cannot run: [`unready`]'s words.
    Unready(String),
    /// The named handler lacks an output the start needs: `handler <id> outputs no <kind>`.
    Lacks(String),
    /// No handler is left for the policy: each one and why it was passed over.
    NoneLeft(Vec<Passed>),
}

/// The handler a start of `m` runs: `named` when given (refused when the mode has no
/// handler of that id, it is not ready, or it lacks an output of `needs`; the policy is
/// not consulted), else [`pick`]'s under `policy`.
pub fn choose<'a>(
    m: &'a LoadedMode,
    named: Option<&str>,
    needs: &[StreamKind],
    policy: &HandlerPolicy,
) -> Result<&'a ModeHandler, Refusal> {
    let Some(id) = named else {
        return pick(&m.row.id.0, &m.handlers, needs, policy).map_err(Refusal::NoneLeft);
    };
    let Some(h) = m.handlers.iter().find(|h| h.id == id) else {
        return Err(Refusal::Unknown(
            m.handlers.iter().map(|h| h.id.clone()).collect(),
        ));
    };
    if let Some(why) = unready(h) {
        return Err(Refusal::Unready(why));
    }
    match needs.iter().find(|k| !h.outputs.contains(k)) {
        Some(k) => Err(Refusal::Lacks(format!(
            "handler {id} outputs no {}",
            kind_name(*k)
        ))),
        None => Ok(h),
    }
}

/// A mode's own fields as the node reports them (`modes_list`, `modes_show`) once it
/// may have more than one handler.
#[derive(Clone, Debug, PartialEq)]
pub struct Reported {
    /// `ready` when any handler is; else the row's own (a broken row stays broken).
    pub state: ModeState,
    /// `reason`, `program` and `missing` of the handler the policy picks now, or the
    /// row's own when none is ready.
    pub reason: Option<String>,
    pub program: Option<PathBuf>,
    pub missing: Vec<String>,
    /// The union of the ready handlers' outputs, in handler order; the row's own when
    /// none is ready.
    pub outputs: Vec<StreamKind>,
    /// The handler the policy picks now for a start that needs nothing; `None` when none
    /// is ready.
    pub handler: Option<String>,
}

/// `m`'s fields as reported under `policy`. With only the row's own handler they are the
/// row's own (`m.state`, `m.reason`, ...). `checked` is always the row's own.
pub fn reported(m: &LoadedMode, policy: &HandlerPolicy) -> Reported {
    let own = || Reported {
        state: m.state,
        reason: m.reason.clone(),
        program: m.program.clone(),
        missing: m.missing.clone(),
        outputs: m.outputs.clone(),
        handler: None,
    };
    if m.state == ModeState::Broken {
        return own();
    }
    let Ok(picked) = pick(&m.row.id.0, &m.handlers, &[], policy) else {
        return own();
    };
    let mut outputs: Vec<StreamKind> = Vec::new();
    for (i, h) in m.handlers.iter().enumerate() {
        if h.state != ModeState::Ready {
            continue;
        }
        // The row's own handler's outputs are the row's.
        for k in if i == 0 { &m.outputs } else { &h.outputs } {
            if !outputs.contains(k) {
                outputs.push(*k);
            }
        }
    }
    // The row's own handler is described by the row's fields.
    let (reason, program, missing) = match std::ptr::eq(&m.handlers[0], picked) {
        true => (&m.reason, &m.program, &m.missing),
        false => (&picked.reason, &picked.program, &picked.missing),
    };
    Reported {
        state: ModeState::Ready,
        reason: reason.clone(),
        program: program.clone(),
        missing: missing.clone(),
        outputs,
        handler: Some(picked.id.clone()),
    }
}

/// Why `h` cannot run at all: `handler <id> is <state>: <reason>` (`broken` or
/// `unavailable`, the reason its own or the state's word); `None` when it is ready.
pub fn unready(h: &ModeHandler) -> Option<String> {
    let state = match h.state {
        ModeState::Ready => return None,
        ModeState::Broken => "broken",
        ModeState::Unavailable => "unavailable",
    };
    let reason = h.reason.as_deref().unwrap_or(state);
    Some(format!("handler {} is {state}: {reason}", h.id))
}

/// Why `h` cannot serve a start that needs `needs`; `None` when it can.
fn passed_over(h: &ModeHandler, needs: &[StreamKind]) -> Option<String> {
    if h.state != ModeState::Ready {
        let state = match h.state {
            ModeState::Broken => "broken",
            _ => "unavailable",
        };
        return Some(h.reason.clone().unwrap_or_else(|| state.into()));
    }
    needs
        .iter()
        .find(|k| !h.outputs.contains(k))
        .map(|k| format!("outputs no {}", kind_name(*k)))
}

/// A stream kind's wire name (`packet`).
pub(crate) fn kind_name(k: StreamKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handler `id`; one not `ready` is `unavailable` with the reason `<id>: missing`.
    fn h(id: &str, in_process: bool, ready: bool, outputs: &[StreamKind]) -> ModeHandler {
        ModeHandler {
            id: id.into(),
            name: id.into(),
            in_process,
            outputs: outputs.to_vec(),
            state: match ready {
                true => ModeState::Ready,
                false => ModeState::Unavailable,
            },
            reason: (!ready).then(|| format!("{id}: missing")),
            program: None,
            missing: vec![],
            checked: None,
            file: format!("{id}.json"),
        }
    }

    const MODE: &str = "adsb_decode";
    const EV: &[StreamKind] = &[StreamKind::Event];
    const EV_PKT: &[StreamKind] = &[StreamKind::Event, StreamKind::Packet];

    fn picked(handlers: &[ModeHandler], needs: &[StreamKind], policy: &HandlerPolicy) -> String {
        match pick(MODE, handlers, needs, policy) {
            Ok(h) => h.id.clone(),
            Err(passed) => panic!("nothing picked: {passed:?}"),
        }
    }

    fn ranked(mode: &[&str], all: &[&str]) -> HandlerPolicy {
        let list = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        HandlerPolicy {
            modes: match mode.is_empty() {
                true => BTreeMap::new(),
                false => BTreeMap::from([(MODE.to_string(), list(mode))]),
            },
            all: list(all),
        }
    }

    #[test]
    fn the_rows_own_handler_wins_with_no_ranking() {
        let hs = [h("dump1090", false, true, EV), h("onair", false, true, EV)];
        assert_eq!(picked(&hs, &[], &HandlerPolicy::default()), "dump1090");
    }

    #[test]
    fn an_in_process_handler_beats_a_program() {
        let hs = [h("extprog", false, true, EV), h("builtin", true, true, EV)];
        assert_eq!(picked(&hs, &[], &HandlerPolicy::default()), "builtin");
    }

    #[test]
    fn the_modes_ranking_leads_then_the_node_wide_one() {
        let hs = [h("dump1090", false, true, EV), h("onair", false, true, EV)];
        assert_eq!(picked(&hs, &[], &ranked(&["onair"], &[])), "onair");
        assert_eq!(picked(&hs, &[], &ranked(&[], &["onair"])), "onair");
        assert_eq!(
            picked(&hs, &[], &ranked(&["dump1090"], &["onair"])),
            "dump1090"
        );
        // Another mode's ranking is not this one's.
        let mut other = ranked(&[], &[]);
        other
            .modes
            .insert("aprs_decode".into(), vec!["onair".into()]);
        assert_eq!(picked(&hs, &[], &other), "dump1090");
    }

    #[test]
    fn an_unavailable_ranked_handler_is_passed_over() {
        let hs = [h("dump1090", false, true, EV), h("onair", false, false, EV)];
        assert_eq!(
            picked(&hs, &[], &ranked(&["onair"], &["onair"])),
            "dump1090"
        );
    }

    #[test]
    fn needs_filters_before_the_ranking() {
        let hs = [
            h("dump1090", false, true, EV),
            h("onair", false, true, EV_PKT),
        ];
        let policy = ranked(&["dump1090"], &[]);
        assert_eq!(picked(&hs, &[StreamKind::Packet], &policy), "onair");
        assert_eq!(picked(&hs, &[StreamKind::Event], &policy), "dump1090");
    }

    #[test]
    fn nothing_left_names_each_handler_and_why() {
        let hs = [
            h("dump1090", false, true, EV),
            h("onair", false, false, EV_PKT),
        ];
        let passed = pick(MODE, &hs, &[StreamKind::Packet], &ranked(&["onair"], &[])).unwrap_err();
        assert_eq!(
            passed,
            [
                Passed {
                    id: "dump1090".into(),
                    why: "outputs no packet".into(),
                },
                Passed {
                    id: "onair".into(),
                    why: "onair: missing".into(),
                },
            ]
        );
    }

    #[test]
    fn a_policy_file_with_only_mode_rankings_loads_them() {
        let p: HandlerPolicy =
            serde_json::from_str(r#"{"modes": {"adsb_decode": ["onair"]}}"#).unwrap();
        assert_eq!(p, ranked(&["onair"], &[]));
        let p: HandlerPolicy = serde_json::from_str(r#"{"all": ["onair"]}"#).unwrap();
        assert_eq!(p, ranked(&[], &["onair"]));
        // Unknown keys are still refused.
        assert!(serde_json::from_str::<HandlerPolicy>(r#"{"mode": {}}"#).is_err());
    }
}
