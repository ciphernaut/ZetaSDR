//! `bits_start`'s plan and chain: from a capture set's feature record to
//! `capture_in → resample → <demod> → clock_recover → slicer → bit_view`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bursts::CaptureRecord;
use crate::classify::FeatureRecord;
use crate::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId};
use crate::types::{Baud, Hz, SourceId, StreamKind};

/// Samples per symbol the chain runs at (the resampler's target).
pub const SAMPLES_PER_SYMBOL: u64 = 8;

/// Fewest samples per symbol the capture must hold for the chain to start.
pub const MIN_SAMPLES_PER_SYMBOL: u64 = 4;

/// How far the chain rate may stray from `SAMPLES_PER_SYMBOL × baud`, as a fraction,
/// for a ratio to the capture rate the resampler serves exactly.
pub const CHAIN_RATE_TOLERANCE: f64 = 0.0625;

/// The largest reduced `max(L, M)` the resampler takes without snapping: a copy of
/// `zeta_dsp::resample::SNAP_FACTOR`, which `zeta-core` cannot reach.
pub const CHAIN_RATE_FACTOR_MAX: u64 = 2048;

/// The carrier offset, Hz either side, the PSK chain's rate leaves room for: the
/// demodulator's coarse estimate raises the signal to the `order`th power, so the
/// rate must hold `2 × order × PSK_OFFSET_RANGE_HZ`.
pub const PSK_OFFSET_RANGE_HZ: u64 = 2000;

/// `psk_demod`'s grid: its input rate is a whole multiple of this many samples per
/// symbol (`zeta_dsp::psk::OUT_SAMPLES_PER_SYMBOL`).
const PSK_GRID_SAMPLES_PER_SYMBOL: u64 = 4;

/// How many steps of the PSK grid the chain rate search tries above the least rate.
const PSK_RATE_SEARCH_STEPS: u64 = 64;

/// The pause `capture_in` takes after each pass of the set, ms.
pub const LOOP_PAUSE_MS: u64 = 500;

/// The demodulator the chain runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Modulation {
    Fsk,
    Psk,
    Ook,
}

impl Modulation {
    fn name(self) -> &'static str {
        match self {
            Modulation::Fsk => "fsk",
            Modulation::Psk => "psk",
            Modulation::Ook => "ook",
        }
    }

    /// The orders the demodulator takes.
    fn orders(self) -> &'static [u8] {
        match self {
            Modulation::Fsk => &[2, 4],
            Modulation::Psk => &[2, 4, 8],
            Modulation::Ook => &[2],
        }
    }

    /// The modulation a layer-1 label names: `fsk` and `fm` (FSK heard through a
    /// discriminator) are FSK.
    fn from_label(label: &str) -> Option<Modulation> {
        match label {
            "fsk" | "fm" => Some(Modulation::Fsk),
            "psk" => Some(Modulation::Psk),
            "ook" => Some(Modulation::Ook),
            _ => None,
        }
    }
}

/// What `bits_start` runs: the job's `context.bits`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BitsPlan {
    /// The set's capture ids, in time order.
    pub captures: Vec<String>,
    pub source: SourceId,
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
    pub capture_rate_hz: Hz,
    pub modulation: Modulation,
    pub symbol_rate_baud: Baud,
    pub order: u8,
    /// The feature record's measured tone spacing, handed to `fsk_demod`; `None` when
    /// the record has none (the demodulator then tracks the tones itself) or the chain
    /// is not FSK.
    pub tone_spacing_hz: Option<Hz>,
    /// The rate the capture is resampled to.
    pub rate_hz: Hz,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `max(L, M)` of `rate / capture_rate` reduced.
fn factor(rate: u64, capture_rate: u64) -> u64 {
    let g = gcd(rate, capture_rate).max(1);
    (rate / g).max(capture_rate / g)
}

/// The rate the chain resamples a `capture_rate_hz` capture to. FSK and OOK: the rate
/// strictly within `SAMPLES_PER_SYMBOL × baud × (1 ± CHAIN_RATE_TOLERANCE)` whose ratio
/// to the capture rate reduces to the smallest `max(L, M)` (at most
/// [`CHAIN_RATE_FACTOR_MAX`]), ties to the nearest to `SAMPLES_PER_SYMBOL × baud`;
/// that rate itself when none reduces that far. PSK: the smallest multiple of
/// `psk_demod`'s grid (`4 × baud`) at least `max(SAMPLES_PER_SYMBOL × baud, 2 × order ×`
/// [`PSK_OFFSET_RANGE_HZ`]`)` whose ratio reduces to at most [`CHAIN_RATE_FACTOR_MAX`],
/// stepping up the grid; the least such multiple when none does within the search.
pub fn chain_rate_hz(
    capture_rate_hz: u64,
    symbol_rate_baud: u32,
    modulation: Modulation,
    order: u8,
) -> u64 {
    let baud = symbol_rate_baud as u64;
    let target = SAMPLES_PER_SYMBOL * baud;
    if capture_rate_hz == 0 || baud == 0 {
        return target;
    }
    if modulation == Modulation::Psk {
        let grid = PSK_GRID_SAMPLES_PER_SYMBOL * baud;
        let least = target.max(2 * order as u64 * PSK_OFFSET_RANGE_HZ);
        let first = least.div_ceil(grid);
        return (first..first + PSK_RATE_SEARCH_STEPS)
            .map(|k| k * grid)
            .find(|&r| factor(r, capture_rate_hz) <= CHAIN_RATE_FACTOR_MAX)
            .unwrap_or(first * grid);
    }
    let span = target as f64 * CHAIN_RATE_TOLERANCE;
    let (lo, hi) = (target as f64 - span, target as f64 + span);
    // Every rate whose reduced ratio has a denominator M <= the limit is
    // `capture_rate × L / M`: walk M, then the L that land strictly inside the window.
    let mut best: Option<(u64, u64, u64)> = None;
    for m in 1..=CHAIN_RATE_FACTOR_MAX {
        let l_lo = (lo * m as f64 / capture_rate_hz as f64).floor() as u64;
        let l_hi = (hi * m as f64 / capture_rate_hz as f64).ceil() as u64;
        for l in l_lo.max(1)..=l_hi {
            let num = capture_rate_hz * l;
            if !num.is_multiple_of(m) {
                continue;
            }
            let r = num / m;
            if !(lo < r as f64 && (r as f64) < hi) {
                continue;
            }
            let f = factor(r, capture_rate_hz);
            if f > CHAIN_RATE_FACTOR_MAX {
                continue;
            }
            let key = (f, r.abs_diff(target), r);
            if best.is_none_or(|b| key < b) {
                best = Some(key);
            }
        }
    }
    best.map_or(target, |(_, _, r)| r)
}

/// The chain `bits_start` runs over `set` (the named capture's set, in time order),
/// each `None` taken from `features` (the named capture's record) or, for the
/// modulation, the layer-1 `label`.
pub fn bits_plan(
    set: &[CaptureRecord],
    features: &FeatureRecord,
    label: Option<&str>,
    modulation: Option<Modulation>,
    symbol_rate_baud: Option<Baud>,
    order: Option<u8>,
) -> Result<BitsPlan, String> {
    let first = set.first().ok_or("no captures in the set")?;
    for r in &set[1..] {
        if r.sample_rate_hz != first.sample_rate_hz {
            return Err(format!(
                "capture {}: {} Hz differs from {}'s {} Hz",
                r.id, r.sample_rate_hz.0, first.id, first.sample_rate_hz.0
            ));
        }
        if r.center_hz != first.center_hz {
            return Err(format!(
                "capture {}: {} Hz differs from {}'s {} Hz",
                r.id, r.center_hz.0, first.id, first.center_hz.0
            ));
        }
    }
    let rate = symbol_rate_baud
        .or(features.symbol_rate_baud)
        .ok_or("no symbol rate measured; give symbol_rate_baud")?;
    if rate.0 == 0 {
        return Err("symbol_rate_baud: at least 1".into());
    }
    let modulation = match modulation {
        Some(m) => m,
        None => label.and_then(Modulation::from_label).ok_or_else(|| {
            format!(
                "no modulation measured (label {}); give modulation",
                label.unwrap_or("none")
            )
        })?,
    };
    let orders = modulation.orders();
    let order = match order {
        Some(n) if orders.contains(&n) => n,
        Some(n) => {
            let names: Vec<String> = orders.iter().map(u8::to_string).collect();
            let list = match names.split_last() {
                Some((last, [])) => last.clone(),
                Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
                None => String::new(),
            };
            return Err(format!(
                "order: {n} is not {list} for {}",
                modulation.name()
            ));
        }
        None => features
            .order
            .filter(|n| modulation != Modulation::Ook && orders.contains(n))
            .unwrap_or(2),
    };
    let capture_rate = first.sample_rate_hz.0;
    let need = MIN_SAMPLES_PER_SYMBOL * rate.0 as u64;
    if need > capture_rate {
        return Err(format!(
            "symbol_rate_baud {} needs {need} Hz; the capture holds {capture_rate} Hz",
            rate.0
        ));
    }
    Ok(BitsPlan {
        captures: set.iter().map(|r| r.id.clone()).collect(),
        source: first.source.clone(),
        center_hz: first.center_hz,
        bandwidth_hz: first.bandwidth_hz,
        capture_rate_hz: first.sample_rate_hz,
        modulation,
        symbol_rate_baud: rate,
        order,
        tone_spacing_hz: features
            .tone_spacing_hz
            .filter(|s| modulation == Modulation::Fsk && s.0 > 0),
        rate_hz: Hz(chain_rate_hz(capture_rate, rate.0, modulation, order)),
    })
}

/// The plan's graph: `capture_in` (looping, paced, [`LOOP_PAUSE_MS`] between passes) →
/// `resample` → `demod` (`fsk_demod` given the plan's `tone_spacing_hz`, `null` when it
/// has none) → `clock` → `slicer` → `bit_view`.
pub fn bits_graph(plan: &BitsPlan) -> Graph {
    use serde_json::{Value, json};
    let baud = plan.symbol_rate_baud.0;
    let demod = match plan.modulation {
        Modulation::Fsk => (
            "fsk_demod",
            json!({
                "symbol_rate_baud": baud,
                "order": plan.order,
                "tone_spacing_hz": plan.tone_spacing_hz.map(|s| s.0),
            }),
        ),
        Modulation::Psk => (
            "psk_demod",
            json!({"symbol_rate_baud": baud, "order": plan.order}),
        ),
        Modulation::Ook => ("ook_demod", json!({"symbol_rate_baud": baud})),
    };
    // The default Mueller and Müller detector mistimes 4-level input.
    let algorithm = match (plan.modulation, plan.order) {
        (Modulation::Fsk, 4) => json!("early_late"),
        _ => Value::Null,
    };
    let (levels, constellation) = match plan.modulation {
        Modulation::Fsk => (plan.order, Value::Null),
        Modulation::Psk => (
            2,
            json!(match plan.order {
                2 => "bpsk",
                4 => "qpsk",
                _ => "8psk",
            }),
        ),
        Modulation::Ook => (2, Value::Null),
    };
    let nodes = [
        (
            "capture_in",
            crate::ops::CAPTURE_IN_STAGE,
            json!({
                "captures": plan.captures,
                "loop": true,
                "realtime": true,
                "loop_pause_ms": LOOP_PAUSE_MS,
            }),
        ),
        (
            "resample",
            "resample",
            json!({"out_rate_hz": plan.rate_hz.0}),
        ),
        ("demod", demod.0, demod.1),
        (
            "clock",
            "clock_recover",
            json!({"symbol_rate_baud": baud, "algorithm": algorithm}),
        ),
        (
            "slicer",
            crate::ops::SLICER_STAGE,
            json!({"levels": levels, "constellation": constellation}),
        ),
        ("bit_view", "bit_view", json!({})),
    ];
    let kinds = [
        StreamKind::Iq,
        StreamKind::Iq,
        StreamKind::Audio,
        StreamKind::Audio,
        StreamKind::Event,
    ];
    let edges = nodes
        .windows(2)
        .zip(kinds)
        .map(|(w, kind)| Edge {
            from: NodeId(w[0].0.into()),
            to: NodeId(w[1].0.into()),
            kind,
        })
        .collect();
    Graph {
        id: GraphId::new(),
        nodes: nodes
            .into_iter()
            .map(|(id, kind, params)| GraphNode {
                id: NodeId(id.into()),
                kind: kind.into(),
                params,
            })
            .collect(),
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JobId;

    fn features(rate: Option<u32>, order: Option<u8>) -> FeatureRecord {
        FeatureRecord {
            bandwidth_hz: Hz(12_000),
            center_hz: Hz(433_920_000),
            symbol_rate_baud: rate.map(Baud),
            snr_db: 20.0,
            burst: true,
            duty: Some(0.5),
            bursts: Some(2),
            burst_ms: Some(50.0),
            period_ms: Some(100.0),
            order,
            tone_spacing_hz: Some(Hz(4800)),
        }
    }

    fn record(id: &str, rate: u64, start_ns: u64) -> CaptureRecord {
        CaptureRecord {
            id: id.into(),
            meta_path: format!("/tmp/{id}.sigmf-meta").into(),
            data_path: format!("/tmp/{id}.sigmf-data").into(),
            source: SourceId("rx".into()),
            set: JobId("set".into()),
            center_hz: Hz(433_920_000),
            bandwidth_hz: Hz(20_000),
            sample_rate_hz: Hz(rate),
            start_ns,
            end_ns: start_ns + 200_000_000,
            bytes: 0,
            features: serde_json::json!({}),
            signal: None,
            job: JobId("cap".into()),
            watch: None,
            allocations: serde_json::json!([]),
            pinned: false,
            written_ns: 0,
        }
    }

    fn reduced(rate: u64, capture: u64) -> u64 {
        factor(rate, capture)
    }

    #[test]
    fn chain_rate_is_eight_samples_per_symbol_when_exact() {
        assert_eq!(chain_rate_hz(48_000, 4800, Modulation::Fsk, 2), 38_400);
    }

    #[test]
    fn chain_rate_picks_a_rate_the_resampler_serves() {
        let r = chain_rate_hz(48_000, 4789, Modulation::Fsk, 2);
        let target = 8.0 * 4789.0;
        assert!(
            (r as f64 - target).abs() < target * CHAIN_RATE_TOLERANCE,
            "{r}"
        );
        assert!(reduced(r, 48_000) <= CHAIN_RATE_FACTOR_MAX, "{r}");
    }

    #[test]
    fn chain_rate_for_psk_lands_on_the_demodulators_grid() {
        let r = chain_rate_hz(48_000, 4800, Modulation::Psk, 2);
        assert_eq!(r % 19_200, 0, "{r}");
        assert!(r >= 38_400, "{r}");
        assert!(reduced(r, 48_000) <= CHAIN_RATE_FACTOR_MAX, "{r}");
        let r = chain_rate_hz(48_000, 1200, Modulation::Psk, 4);
        assert_eq!(r % 4_800, 0, "{r}");
        assert!(r >= 16_000, "{r}");
        assert!(reduced(r, 48_000) <= CHAIN_RATE_FACTOR_MAX, "{r}");
    }

    #[test]
    fn the_plan_fills_nulls_from_the_features_and_builds_the_chain() {
        let set = [record("01A", 48_000, 1), record("01B", 48_000, 2)];
        let plan = bits_plan(
            &set,
            &features(Some(4800), Some(2)),
            Some("fsk"),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(plan.modulation, Modulation::Fsk);
        assert_eq!(plan.symbol_rate_baud, Baud(4800));
        assert_eq!(plan.order, 2);
        assert_eq!(plan.rate_hz, Hz(38_400));
        assert_eq!(plan.captures, ["01A", "01B"]);
        let g = bits_graph(&plan);
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.0.as_str()).collect();
        assert_eq!(
            ids,
            [
                "capture_in",
                "resample",
                "demod",
                "clock",
                "slicer",
                "bit_view"
            ]
        );
        let slicer = &g.nodes[4];
        assert_eq!(slicer.params["levels"], 2);
        assert!(slicer.params["constellation"].is_null());
        assert_eq!(
            g.nodes[0].params["captures"],
            serde_json::json!(["01A", "01B"])
        );
        assert_eq!(g.nodes[0].params["loop"], true);
        assert_eq!(g.nodes[0].params["realtime"], true);
        assert_eq!(g.nodes[0].params["loop_pause_ms"], LOOP_PAUSE_MS);
        assert_eq!(g.nodes[1].params["out_rate_hz"], 38_400);
        assert_eq!(g.nodes[2].kind, "fsk_demod");
        assert!(g.nodes[3].params["algorithm"].is_null());
        let kinds: Vec<StreamKind> = g.edges.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                StreamKind::Iq,
                StreamKind::Iq,
                StreamKind::Audio,
                StreamKind::Audio,
                StreamKind::Event
            ]
        );
    }

    #[test]
    fn the_fsk_demod_takes_the_measured_tone_spacing() {
        let set = [record("01A", 48_000, 1)];
        let mut f = features(Some(4800), Some(2));
        let plan = bits_plan(&set, &f, Some("fsk"), None, None, None).unwrap();
        assert_eq!(plan.tone_spacing_hz, Some(Hz(4800)));
        assert_eq!(bits_graph(&plan).nodes[2].params["tone_spacing_hz"], 4800);
        f.tone_spacing_hz = None;
        let plan = bits_plan(&set, &f, Some("fsk"), None, None, None).unwrap();
        assert!(bits_graph(&plan).nodes[2].params["tone_spacing_hz"].is_null());
        // Only FSK takes it.
        f.tone_spacing_hz = Some(Hz(4800));
        let plan = bits_plan(&set, &f, None, Some(Modulation::Ook), None, None).unwrap();
        assert_eq!(plan.tone_spacing_hz, None);
    }

    #[test]
    fn a_symbol_rate_of_zero_is_refused() {
        let set = [record("01A", 48_000, 1)];
        let given = bits_plan(
            &set,
            &features(Some(4800), None),
            Some("fsk"),
            None,
            Some(Baud(0)),
            None,
        );
        assert_eq!(given.unwrap_err(), "symbol_rate_baud: at least 1");
        let recorded = bits_plan(
            &set,
            &features(Some(0), None),
            Some("fsk"),
            None,
            None,
            None,
        );
        assert_eq!(recorded.unwrap_err(), "symbol_rate_baud: at least 1");
    }

    #[test]
    fn the_plan_builds_the_psk_and_4fsk_chains() {
        let set = [record("01A", 48_000, 1)];
        let plan = bits_plan(
            &set,
            &features(Some(2400), None),
            None,
            Some(Modulation::Psk),
            None,
            Some(4),
        )
        .unwrap();
        let g = bits_graph(&plan);
        assert_eq!(g.nodes[2].kind, "psk_demod");
        assert_eq!(g.nodes[4].params["levels"], 2);
        assert_eq!(g.nodes[4].params["constellation"], "qpsk");
        let plan = bits_plan(
            &set,
            &features(Some(2400), Some(4)),
            Some("fsk"),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(plan.order, 4);
        let g = bits_graph(&plan);
        assert_eq!(g.nodes[3].params["algorithm"], "early_late");
        assert_eq!(g.nodes[4].params["levels"], 4);
        let e = bits_plan(
            &set,
            &features(Some(2400), None),
            None,
            Some(Modulation::Fsk),
            None,
            Some(8),
        )
        .unwrap_err();
        assert_eq!(e, "order: 8 is not 2 or 4 for fsk");
    }

    #[test]
    fn the_plan_refuses_without_a_rate_or_a_modulation() {
        let set = [record("01A", 48_000, 1)];
        let e = bits_plan(
            &set,
            &features(None, None),
            Some("fsk"),
            Some(Modulation::Fsk),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(e, "no symbol rate measured; give symbol_rate_baud");
        let e = bits_plan(
            &set,
            &features(Some(4800), None),
            Some("am"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(e, "no modulation measured (label am); give modulation");
    }

    #[test]
    fn the_plan_refuses_a_rate_the_capture_cannot_hold() {
        let set = [record("01A", 48_000, 1)];
        let e = bits_plan(
            &set,
            &features(None, None),
            None,
            Some(Modulation::Fsk),
            Some(Baud(20_000)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            e,
            "symbol_rate_baud 20000 needs 80000 Hz; the capture holds 48000 Hz"
        );
    }

    #[test]
    fn the_plan_refuses_a_set_of_mixed_rates() {
        let set = [record("01A", 48_000, 1), record("01B", 96_000, 2)];
        let e = bits_plan(
            &set,
            &features(Some(4800), None),
            Some("fsk"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(e, "capture 01B: 96000 Hz differs from 01A's 48000 Hz");
    }
}
