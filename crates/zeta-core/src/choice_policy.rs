//! The choice policy (design 5.4): how far a `choose` may go for each caller, whether
//! its trial may widen a held capture, and what a trial may spend. Kept in
//! `choice-policy.json` in the state directory; the operator sets it (`choice_policy_set`).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Who asked: the step it may reach is the surface's.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// The page's box menu.
    Menu,
    /// A watch's `on_signal: choose`.
    Watch,
    /// The LLM agent.
    Agent,
    /// Any other caller: the CLI, HTTP, MCP.
    Client,
}

impl Surface {
    /// Its name as serialised.
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Menu => "menu",
            Surface::Watch => "watch",
            Surface::Agent => "agent",
            Surface::Client => "client",
        }
    }
}

/// The surface of the transport an operation arrived by (`ops::apply`'s `transport`):
/// the page is `ui`, the agent `agent`, every other caller a client. A watch sets
/// [`Surface::Watch`] itself.
pub fn surface_of(transport: &str) -> Surface {
    match transport {
        "ui" => Surface::Menu,
        "agent" => Surface::Agent,
        _ => Surface::Client,
    }
}

/// How deep a choice goes for a surface: `default` when the caller names no step,
/// never past `cap`. Both 1 to 3, `default` at most `cap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::modes::require_all)]
pub struct Ceiling {
    /// The step taken when none is asked for, 1 to 3.
    pub default: u8,
    /// The deepest step the surface may ask for, 1 to 3.
    pub cap: u8,
}

/// Whether a step-3 trial may grow a held capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeaseSetting {
    /// Only a running capture that already covers the band.
    Share,
    /// Also grow a held capture, without moving any holder's band.
    Widen,
}

/// What trials may spend.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::modes::require_all)]
pub struct Budget {
    /// Seconds a choice's trials may take in all, at least 1.
    pub trial_s: u32,
    /// Trials run at once, at least 1.
    pub concurrent: u32,
    /// Seconds of the ring one trial replays, above 0.
    pub slice_s: f32,
}

/// The whole policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::modes::require_all)]
pub struct ChoicePolicy {
    /// Each surface's ceiling, by surface name; a surface left out takes its built-in one.
    pub surfaces: BTreeMap<Surface, Ceiling>,
    /// Whether step 3 may widen a held capture.
    pub lease: LeaseSetting,
    /// What trials may spend.
    pub budget: Budget,
    /// A confidence at which the choice stops going deeper; `null` for none, and when
    /// absent.
    #[serde(default)]
    pub confidence_stop: Option<f32>,
}

/// The built-in ceiling of a surface.
fn builtin(s: Surface) -> Ceiling {
    let (default, cap) = match s {
        Surface::Menu => (1, 2),
        Surface::Watch => (3, 3),
        Surface::Agent | Surface::Client => (1, 3),
    };
    Ceiling { default, cap }
}

impl Default for ChoicePolicy {
    fn default() -> Self {
        ChoicePolicy {
            surfaces: [
                Surface::Menu,
                Surface::Watch,
                Surface::Agent,
                Surface::Client,
            ]
            .into_iter()
            .map(|s| (s, builtin(s)))
            .collect(),
            lease: LeaseSetting::Share,
            budget: Budget {
                trial_s: 30,
                concurrent: 3,
                slice_s: 2.0,
            },
            confidence_stop: None,
        }
    }
}

impl ChoicePolicy {
    /// Refuse a policy with a ceiling outside 1 to 3 or a default above its cap, or a
    /// budget that allows nothing; the message names the surface or field.
    pub fn validate(&self) -> Result<(), String> {
        for (s, c) in &self.surfaces {
            let name = s.as_str();
            if !(1..=3).contains(&c.default) || !(1..=3).contains(&c.cap) {
                return Err(format!(
                    "surfaces.{name}: default and cap are steps 1 to 3 (default {}, cap {})",
                    c.default, c.cap
                ));
            }
            if c.default > c.cap {
                return Err(format!(
                    "surfaces.{name}: default {} is above its cap {}",
                    c.default, c.cap
                ));
            }
        }
        let b = &self.budget;
        if b.trial_s < 1 {
            return Err("budget.trial_s must be at least 1".into());
        }
        if b.concurrent < 1 {
            return Err("budget.concurrent must be at least 1".into());
        }
        if !(b.slice_s > 0.0 && b.slice_s.is_finite()) {
            return Err("budget.slice_s must be above 0".into());
        }
        Ok(())
    }

    /// The step a choice from `s` goes to: `asked` (or the surface's default), never past
    /// the surface's cap, and the note `not tried: capped at step <n> for <surface>` when
    /// the ask was cut.
    pub fn steps(&self, s: Surface, asked: Option<u8>) -> (u8, Option<String>) {
        let c = self.surfaces.get(&s).copied().unwrap_or_else(|| builtin(s));
        match asked {
            None => (c.default, None),
            Some(n) if n > c.cap => (
                c.cap,
                Some(format!(
                    "not tried: capped at step {} for {}",
                    c.cap,
                    s.as_str()
                )),
            ),
            Some(n) => (n.max(1), None),
        }
    }
}
