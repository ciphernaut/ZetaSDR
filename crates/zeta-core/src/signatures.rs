//! The learned signatures: what the node (or the operator, by hand) proposes about a
//! signal no mode row knows, one [`SignatureRecord`] per file under
//! `state_dir/signatures/<ulid>.json`. A proposal waits for the operator, who accepts
//! or rejects it; an accepted one is later superseded when a mode row proves its band.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::classify::FeatureRecord;
use crate::knowledge::Allocation;
use crate::station::Station;
use crate::types::{Baud, Hz, JobId, SourceId};

/// The store's directory under the state directory.
pub const SIGNATURES_DIR: &str = "signatures";
/// A proposal's confidence from the features alone.
pub const CONFIDENCE_FEATURES: f32 = 0.3;
/// A proposal's confidence with a framing whose sync and period hold.
pub const CONFIDENCE_FRAMING: f32 = 0.6;
/// A proposal's confidence with a CRC that checks on [`PROPOSE_CRC_FRAMES`] frames.
pub const CONFIDENCE_CRC: f32 = 0.9;
/// The CRC-clean frames a framing needs for [`CONFIDENCE_CRC`] (and a bits or frames
/// job to propose).
pub const PROPOSE_CRC_FRAMES: u32 = 5;
/// The layer-1 label of a window with no signal in it: nothing to propose.
pub const NOISE_LABEL: &str = "noise";

/// Where a signature stands: `proposed` (waiting for the operator), `accepted`,
/// `rejected`, or `superseded` (a mode row proved its band).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignatureState {
    Proposed,
    Accepted,
    Rejected,
    Superseded,
}

impl SignatureState {
    /// Its name as serialised.
    pub fn name(self) -> &'static str {
        match self {
            SignatureState::Proposed => "proposed",
            SignatureState::Accepted => "accepted",
            SignatureState::Rejected => "rejected",
            SignatureState::Superseded => "superseded",
        }
    }
}

impl std::fmt::Display for SignatureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The burst shape of a signature, from its feature record (each `null` when layer 1
/// did not measure it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct Burst {
    /// Fraction of the window the signal is on.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<f32>")]
    pub duty: Option<f32>,
    /// Median burst length, ms.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<f32>")]
    pub burst_ms: Option<f32>,
    /// Median burst start-to-start interval, ms.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<f32>")]
    pub period_ms: Option<f32>,
}

/// Where a signature came from and what became of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct SignatureProvenance {
    pub source: SourceId,
    /// The station at the time, `null` when none was set.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<Station>")]
    pub station: Option<Station>,
    /// The captures it was learned from (ids in the burst store).
    pub captures: Vec<String>,
    /// The capture and bits jobs.
    pub jobs: Vec<JobId>,
    /// The nearest licensed assignment, `null` for none.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<Allocation>")]
    pub allocation: Option<Allocation>,
    pub proposed_ns: u64,
    /// `node`, or the transport of a hand proposal.
    pub proposed_by: String,
    /// When the operator answered the proposal (an accept, or for a rejected record
    /// the reject); `null` while proposed.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<u64>")]
    pub accepted_ns: Option<u64>,
    /// The transport that answer came through.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub accepted_by: Option<String>,
    /// The mode whose proof superseded the record.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub superseded_by: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<u64>")]
    pub superseded_ns: Option<u64>,
}

/// One learned signature: the waveform, the framing that checked (if any), the decoder
/// row generated from it (if any), the confidence and the provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct SignatureRecord {
    /// A ULID; the stem of its file.
    pub id: String,
    /// The node's `<modulation> <width> kHz at <MHz>`, or the operator's.
    pub name: String,
    pub state: SignatureState,
    /// The band: the measured centre less half the bandwidth.
    pub min_hz: Hz,
    /// The measured centre plus half the bandwidth.
    pub max_hz: Hz,
    pub bandwidth_hz: Hz,
    /// The layer-1 label (`fsk`, `psk`, `ook`, ...).
    pub modulation: String,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<u8>")]
    pub order: Option<u8>,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<Baud>")]
    pub symbol_rate_baud: Option<Baud>,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<Hz>")]
    pub tone_spacing_hz: Option<Hz>,
    pub burst: Burst,
    /// The `frame_decode` params that checked, `null` for none.
    #[serde(default)]
    pub framing: Option<Value>,
    /// The mode row generated from the signature, `null` for none.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub decoder: Option<String>,
    /// That row's file.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub decoder_file: Option<String>,
    /// Why no row was written, or that one was kept as it was.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub decoder_note: Option<String>,
    /// [`CONFIDENCE_FEATURES`], [`CONFIDENCE_FRAMING`] or [`CONFIDENCE_CRC`].
    pub confidence: f32,
    /// What the proposal rests on, in words.
    pub evidence: String,
    pub provenance: SignatureProvenance,
    /// Later encounters the layer-2 scorer counted.
    pub matches: u64,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<u64>")]
    pub last_matched_ns: Option<u64>,
}

impl SignatureRecord {
    /// Whether `center_hz` lies within the band widened by half the bandwidth on each
    /// side: `min_hz - bandwidth_hz/2 ..= max_hz + bandwidth_hz/2`.
    pub fn band_holds(&self, center_hz: Hz) -> bool {
        let half = self.bandwidth_hz.0 / 2;
        (self.min_hz.0.saturating_sub(half)..=self.max_hz.0.saturating_add(half))
            .contains(&center_hz.0)
    }
}

/// What a proposer knows of a signature before it is stored.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSignature {
    /// `None` for [`default_name`].
    pub name: Option<String>,
    pub features: FeatureRecord,
    pub modulation: String,
    pub framing: Option<Value>,
    /// The CRC-clean frames the framing decoded.
    pub crc_frames: u32,
    pub evidence: String,
    pub source: SourceId,
    pub station: Option<Station>,
    pub captures: Vec<String>,
    pub jobs: Vec<JobId>,
    pub allocation: Option<Allocation>,
    pub proposed_by: String,
}

/// A proposal's confidence: [`CONFIDENCE_FEATURES`] without a framing,
/// [`CONFIDENCE_FRAMING`] with one, [`CONFIDENCE_CRC`] with one whose `crc` is set and
/// that decoded at least [`PROPOSE_CRC_FRAMES`] CRC-clean frames.
pub fn confidence(framing: Option<&Value>, crc_frames: u32) -> f32 {
    match framing.filter(|f| !f.is_null()) {
        None => CONFIDENCE_FEATURES,
        Some(f) if !f["crc"].is_null() && crc_frames >= PROPOSE_CRC_FRAMES => CONFIDENCE_CRC,
        Some(_) => CONFIDENCE_FRAMING,
    }
}

/// The node's name for a signature: `<modulation> <kHz> kHz at <MHz> MHz`
/// (`fsk 16.0 kHz at 404.025 MHz`).
pub fn default_name(modulation: &str, bandwidth_hz: Hz, center_hz: Hz) -> String {
    format!(
        "{modulation} {:.1} kHz at {:.3} MHz",
        bandwidth_hz.0 as f64 / 1e3,
        center_hz.0 as f64 / 1e6
    )
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("unknown signature {0}")]
    Unknown(String),
    #[error("signature {id} is {state}")]
    State { id: String, state: SignatureState },
    #[error("io: {0}")]
    Io(String),
}

/// The store: the directory and its records, every change written through.
pub struct SignatureStore {
    dir: PathBuf,
    records: Mutex<Vec<SignatureRecord>>,
    load_errors: Vec<String>,
}

impl SignatureStore {
    /// Open (making it if missing) `state_dir/signatures`, reading every `*.json`; a
    /// file that does not read as a record, or whose id is not its stem, is skipped and
    /// listed by [`load_errors`](Self::load_errors). A leftover `*.json.tmp` is removed.
    pub fn open(state_dir: &Path) -> std::io::Result<Arc<Self>> {
        let dir = state_dir.join(SIGNATURES_DIR);
        std::fs::create_dir_all(&dir)?;
        let (mut records, mut load_errors) = (Vec::new(), Vec::new());
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.to_string_lossy().ends_with(".json.tmp") {
                // Best effort: a leftover that cannot be removed is harmless.
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let parsed = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| {
                    serde_json::from_slice::<SignatureRecord>(&b).map_err(|e| e.to_string())
                })
                .and_then(|r| {
                    if r.id == stem {
                        Ok(r)
                    } else {
                        Err(format!("id {} is not the file's name", r.id))
                    }
                });
            match parsed {
                Ok(r) => records.push(r),
                Err(e) => load_errors.push(format!("{SIGNATURES_DIR}/{name}: {e}")),
            }
        }
        load_errors.sort();
        Ok(Arc::new(Self {
            dir,
            records: Mutex::new(records),
            load_errors,
        }))
    }

    /// The files [`open`](Self::open) could not read, each `signatures/<file>: <reason>`.
    pub fn load_errors(&self) -> &[String] {
        &self.load_errors
    }

    /// The records in `state` (`None` for every one), newest first (by `proposed_ns`,
    /// then id).
    pub fn list(&self, state: Option<SignatureState>) -> Vec<SignatureRecord> {
        let mut v: Vec<SignatureRecord> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| state.is_none_or(|s| r.state == s))
            .cloned()
            .collect();
        v.sort_by(|a, b| (b.provenance.proposed_ns, &b.id).cmp(&(a.provenance.proposed_ns, &a.id)));
        v
    }

    /// The record `id`.
    pub fn get(&self, id: &str) -> Option<SignatureRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.id == id)
            .cloned()
    }

    /// Every accepted record, newest first.
    pub fn accepted(&self) -> Vec<SignatureRecord> {
        self.list(Some(SignatureState::Accepted))
    }

    /// Store a new proposal: its band is the features' centre less and plus half their
    /// bandwidth, its name `new.name` or [`default_name`], its confidence
    /// [`confidence`] of the framing and the CRC-clean frames.
    pub fn propose(&self, new: NewSignature, now_ns: u64) -> Result<SignatureRecord, StoreError> {
        let f = &new.features;
        let half = f.bandwidth_hz.0 / 2;
        let record = SignatureRecord {
            id: ulid::Ulid::generate().to_string(),
            name: new
                .name
                .unwrap_or_else(|| default_name(&new.modulation, f.bandwidth_hz, f.center_hz)),
            state: SignatureState::Proposed,
            min_hz: Hz(f.center_hz.0.saturating_sub(half)),
            max_hz: Hz(f.center_hz.0.saturating_add(half)),
            bandwidth_hz: f.bandwidth_hz,
            modulation: new.modulation,
            order: f.order,
            symbol_rate_baud: f.symbol_rate_baud,
            tone_spacing_hz: f.tone_spacing_hz,
            burst: Burst {
                duty: f.duty,
                burst_ms: f.burst_ms,
                period_ms: f.period_ms,
            },
            confidence: confidence(new.framing.as_ref(), new.crc_frames),
            framing: new.framing.filter(|v| !v.is_null()),
            decoder: None,
            decoder_file: None,
            decoder_note: None,
            evidence: new.evidence,
            provenance: SignatureProvenance {
                source: new.source,
                station: new.station,
                captures: new.captures,
                jobs: new.jobs,
                allocation: new.allocation,
                proposed_ns: now_ns,
                proposed_by: new.proposed_by,
                accepted_ns: None,
                accepted_by: None,
                superseded_by: None,
                superseded_ns: None,
            },
            matches: 0,
            last_matched_ns: None,
        };
        let mut records = self.records.lock().unwrap();
        self.persist(&record)?;
        records.push(record.clone());
        Ok(record)
    }

    /// Accept the proposal `id`, renamed to `name` when given: [`StoreError::State`]
    /// unless it is proposed.
    pub fn accept(
        &self,
        id: &str,
        name: Option<String>,
        by: &str,
        now_ns: u64,
    ) -> Result<SignatureRecord, StoreError> {
        self.change(id, |r| {
            expect_state(r, SignatureState::Proposed)?;
            r.state = SignatureState::Accepted;
            if let Some(n) = name {
                r.name = n;
            }
            r.provenance.accepted_ns = Some(now_ns);
            r.provenance.accepted_by = Some(by.to_string());
            Ok(())
        })
    }

    /// Reject the proposal `id`: [`StoreError::State`] unless it is proposed. Who
    /// answered, and when, go in `accepted_by` and `accepted_ns`.
    pub fn reject(&self, id: &str, by: &str, now_ns: u64) -> Result<SignatureRecord, StoreError> {
        self.change(id, |r| {
            expect_state(r, SignatureState::Proposed)?;
            r.state = SignatureState::Rejected;
            r.provenance.accepted_ns = Some(now_ns);
            r.provenance.accepted_by = Some(by.to_string());
            Ok(())
        })
    }

    /// Mark the accepted record `id` superseded by the mode `mode`:
    /// [`StoreError::State`] unless it is accepted.
    pub fn supersede(
        &self,
        id: &str,
        mode: &str,
        now_ns: u64,
    ) -> Result<SignatureRecord, StoreError> {
        self.change(id, |r| {
            expect_state(r, SignatureState::Accepted)?;
            r.state = SignatureState::Superseded;
            r.provenance.superseded_by = Some(mode.to_string());
            r.provenance.superseded_ns = Some(now_ns);
            Ok(())
        })
    }

    /// Set the record's generated row (`decoder`), its file and the note.
    pub fn set_decoder(
        &self,
        id: &str,
        mode: Option<String>,
        file: Option<String>,
        note: Option<String>,
    ) -> Result<SignatureRecord, StoreError> {
        self.change(id, |r| {
            r.decoder = mode;
            r.decoder_file = file;
            r.decoder_note = note;
            Ok(())
        })
    }

    /// Count one more match of the record `id` at `now_ns`.
    pub fn matched(&self, id: &str, now_ns: u64) -> Result<SignatureRecord, StoreError> {
        self.change(id, |r| {
            r.matches += 1;
            r.last_matched_ns = Some(now_ns);
            Ok(())
        })
    }

    /// Apply `f` to a copy of the record `id`, write it, then keep it.
    fn change(
        &self,
        id: &str,
        f: impl FnOnce(&mut SignatureRecord) -> Result<(), StoreError>,
    ) -> Result<SignatureRecord, StoreError> {
        let mut records = self.records.lock().unwrap();
        let slot = records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| StoreError::Unknown(id.into()))?;
        let mut next = slot.clone();
        f(&mut next)?;
        self.persist(&next)?;
        *slot = next.clone();
        Ok(next)
    }

    /// Write `<id>.json` atomically: a temp file, then a rename.
    fn persist(&self, r: &SignatureRecord) -> Result<(), StoreError> {
        let io = |e: std::io::Error| StoreError::Io(e.to_string());
        let json = serde_json::to_vec_pretty(r).map_err(|e| StoreError::Io(e.to_string()))?;
        let tmp = self.dir.join(format!("{}.json.tmp", r.id));
        std::fs::write(&tmp, json).map_err(io)?;
        std::fs::rename(&tmp, self.dir.join(format!("{}.json", r.id))).map_err(io)
    }
}

fn expect_state(r: &SignatureRecord, want: SignatureState) -> Result<(), StoreError> {
    if r.state == want {
        return Ok(());
    }
    Err(StoreError::State {
        id: r.id.clone(),
        state: r.state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features() -> FeatureRecord {
        FeatureRecord {
            bandwidth_hz: Hz(16_000),
            center_hz: Hz(404_025_000),
            symbol_rate_baud: Some(Baud(4_800)),
            snr_db: 20.0,
            burst: true,
            duty: Some(0.1),
            bursts: Some(2),
            burst_ms: Some(240.0),
            period_ms: Some(1_000.0),
            order: Some(2),
            tone_spacing_hz: None,
            symbol_rate_line: None,
        }
    }

    fn new(name: Option<&str>) -> NewSignature {
        NewSignature {
            name: name.map(str::to_string),
            features: features(),
            modulation: "fsk".into(),
            framing: None,
            crc_frames: 0,
            evidence: "features only".into(),
            source: SourceId("rx".into()),
            station: None,
            captures: vec!["01CAP".into()],
            jobs: vec![JobId("01JOB".into())],
            allocation: None,
            proposed_by: "node".into(),
        }
    }

    #[test]
    fn confidence_rises_with_a_framing_and_a_checked_crc() {
        let framed = serde_json::json!({"sync": {"hex": "2dd4", "bits": 16}, "crc": null});
        let crc = serde_json::json!({"sync": {"hex": "2dd4", "bits": 16}, "crc": "crc16-ccitt"});
        assert_eq!(confidence(None, 9), CONFIDENCE_FEATURES);
        assert_eq!(confidence(Some(&Value::Null), 9), CONFIDENCE_FEATURES);
        assert_eq!(confidence(Some(&framed), 9), CONFIDENCE_FRAMING);
        assert_eq!(
            confidence(Some(&crc), PROPOSE_CRC_FRAMES - 1),
            CONFIDENCE_FRAMING
        );
        assert_eq!(confidence(Some(&crc), PROPOSE_CRC_FRAMES), CONFIDENCE_CRC);
    }

    #[test]
    fn default_name_reads_the_width_and_centre() {
        assert_eq!(
            default_name("fsk", Hz(16_000), Hz(404_025_000)),
            "fsk 16.0 kHz at 404.025 MHz"
        );
    }

    #[test]
    fn the_store_walks_a_proposal_through_its_states() {
        let dir = tempfile::tempdir().unwrap();
        let s = SignatureStore::open(dir.path()).unwrap();
        let a = s.propose(new(None), 1).unwrap();
        assert_eq!(a.name, "fsk 16.0 kHz at 404.025 MHz");
        assert_eq!((a.min_hz, a.max_hz), (Hz(404_017_000), Hz(404_033_000)));
        assert!(a.band_holds(Hz(404_009_000)) && a.band_holds(Hz(404_041_000)));
        assert!(!a.band_holds(Hz(404_008_999)) && !a.band_holds(Hz(404_041_001)));
        let b = s.propose(new(Some("mine")), 2).unwrap();
        assert_eq!(b.name, "mine");
        let ids: Vec<String> = s.list(None).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [b.id.clone(), a.id.clone()]);
        assert_eq!(
            s.supersede(&a.id, "pocsag_decode", 3),
            Err(StoreError::State {
                id: a.id.clone(),
                state: SignatureState::Proposed
            })
        );
        let acc = s.accept(&a.id, None, "ui", 4).unwrap();
        assert_eq!(acc.name, a.name);
        assert_eq!(s.accepted(), std::slice::from_ref(&acc));
        let m = s.matched(&a.id, 5).unwrap();
        assert_eq!((m.matches, m.last_matched_ns), (1, Some(5)));
        let d = s
            .set_decoder(&a.id, Some("learned_x".into()), Some("f".into()), None)
            .unwrap();
        assert_eq!(d.decoder.as_deref(), Some("learned_x"));
        let sup = s.supersede(&a.id, "pocsag_decode", 6).unwrap();
        assert_eq!(sup.state, SignatureState::Superseded);
        assert_eq!(
            sup.provenance.superseded_by.as_deref(),
            Some("pocsag_decode")
        );
        assert_eq!(
            s.reject(&a.id, "ui", 7),
            Err(StoreError::State {
                id: a.id.clone(),
                state: SignatureState::Superseded
            })
        );
        assert_eq!(
            s.reject("nope", "ui", 7),
            Err(StoreError::Unknown("nope".into()))
        );
        let r = s.reject(&b.id, "ui", 8).unwrap();
        assert_eq!(r.state, SignatureState::Rejected);
        drop(s);
        let s = SignatureStore::open(dir.path()).unwrap();
        assert_eq!(s.get(&a.id), Some(sup));
        assert_eq!(s.get(&b.id), Some(r));
        assert!(s.load_errors().is_empty());
    }

    #[test]
    fn the_record_schema_requires_every_field() {
        let s = serde_json::to_value(schemars::schema_for!(SignatureRecord)).unwrap();
        let props = s["properties"].as_object().unwrap();
        let required = s["required"].as_array().unwrap();
        assert_eq!(required.len(), props.len());
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(
            s["properties"]["decoder"]["type"],
            serde_json::json!(["string", "null"])
        );
    }
}
