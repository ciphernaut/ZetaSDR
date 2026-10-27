//! `adsb_decode` as a stage kind: an alias that runs the `adsb_decode` mode row's
//! program through [`crate::external`], so persisted `job.json` graphs and `graph_put`
//! edits that name the kind still load and run. New jobs expand from the row to
//! `channel` -> `external_decode {"mode": "adsb_decode"}` instead.
//!
//! The row (`modes/adsb_decode.json`) runs dump1090 with its input as SC16 on stdin at
//! [`ADSB_RATE_HZ`] and reads its SBS output from a loopback port
//! ([`crate::parsers::sbs`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::modes::{Decoder, ModeRow, ModeState};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

use crate::external::ModesHandle;

/// The only sample rate dump1090 takes.
pub const ADSB_RATE_HZ: u64 = 2_400_000;
/// The mode row the alias runs.
pub const ADSB_MODE: &str = "adsb_decode";

/// Why a stage given no program fails.
const NO_PROGRAM: &str = "adsb_decode: no dump1090 program (install dump1090 or set ZETA_HANDLER_DUMP1090 / ZETA_DUMP1090)";

/// The dump1090 to run: `ZETA_HANDLER_DUMP1090` (or `ZETA_DUMP1090`) when set, else the
/// first of the `adsb_decode` row's programs found in `PATH`.
pub fn find_dump1090() -> Option<PathBuf> {
    let row = embedded_row();
    match &row.decoder {
        Decoder::Program(b) => crate::proc::find_handler(&b.programs),
        Decoder::Graph { .. } => None,
    }
}

/// The embedded `adsb_decode` row.
fn embedded_row() -> ModeRow {
    zeta_core::modes::parse(include_str!("../../../modes/adsb_decode.json"))
        .expect("the embedded adsb_decode row parses")
}

/// `adsb_decode` takes no parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AdsbParams {}

/// Where the alias gets its row and program.
enum Runs {
    /// The embedded row with this program (`None`: every run fails at once).
    Fixed(Option<PathBuf>),
    /// The loaded registry's `adsb_decode` row and its probed program.
    Registry(ModesHandle),
}

/// IQ in, one event per decoded Mode S message out, via the `adsb_decode` row.
pub struct AdsbDecodeStage {
    spec: StageSpec,
    runs: Runs,
    env: Vec<(OsString, OsString)>,
}

impl AdsbDecodeStage {
    fn with(runs: Runs) -> Self {
        Self {
            spec: StageSpec {
                kind: ADSB_MODE,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(AdsbParams),
            },
            runs,
            env: Vec::new(),
        }
    }

    /// The embedded row run with `program`; with `None` every run fails at once.
    pub fn new(program: Option<PathBuf>) -> Self {
        Self::with(Runs::Fixed(program))
    }

    /// The registry's `adsb_decode` row, from the registry `handle` will hold.
    pub fn deferred(handle: ModesHandle) -> Self {
        Self::with(Runs::Registry(handle))
    }

    /// The program a stage made with [`AdsbDecodeStage::new`] runs.
    pub fn program(&self) -> Option<&Path> {
        match &self.runs {
            Runs::Fixed(p) => p.as_deref(),
            Runs::Registry(_) => None,
        }
    }

    /// Extra variables for the child, on top of the cleared environment (tests).
    pub fn with_env<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.env
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    fn resolve(&self) -> Result<(ModeRow, PathBuf), StageError> {
        let failed = |m: String| StageError::Failed(m);
        match &self.runs {
            Runs::Fixed(None) => Err(failed(NO_PROGRAM.into())),
            Runs::Fixed(Some(p)) => Ok((embedded_row(), p.clone())),
            Runs::Registry(handle) => {
                let reg = handle
                    .get()
                    .ok_or_else(|| failed("adsb_decode: no mode registry".into()))?;
                let m = reg
                    .get(ADSB_MODE)
                    .ok_or_else(|| failed("adsb_decode: unknown mode adsb_decode".into()))?;
                if m.state != ModeState::Ready {
                    return Err(failed(format!(
                        "adsb_decode: {}",
                        m.reason.as_deref().unwrap_or("not ready")
                    )));
                }
                let Decoder::Program(block) = &m.row.decoder else {
                    return Err(failed("adsb_decode: the row is not a program row".into()));
                };
                let program = m
                    .program
                    .clone()
                    .or_else(|| crate::proc::find_handler(&block.programs))
                    .ok_or_else(|| failed(NO_PROGRAM.into()))?;
                Ok((m.row.clone(), program))
            }
        }
    }
}

#[async_trait]
impl Stage for AdsbDecodeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let (row, program) = self.resolve()?;
        let _: AdsbParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("adsb_decode params: {e}")))?;
        crate::external::run_row(&row, &program, &self.env, inputs, outputs, bus, cancel, ctx).await
    }
}
