//! A frame row's quick attempt: its chain (`resample → <demod> → clock_recover → slicer →
//! frame_decode`) run offline over a channel `classify` extracted, proving the row on a
//! frame that passes the framing's CRC.

use num_complex::Complex;
use serde_json::{Value, json};
use zeta_core::bits::{chain_rate_hz, decision_nodes};
use zeta_core::modes::FrameBlock;

use crate::framing::Framing;
use crate::offline::run_chain;

/// The evidence of `block`'s chain over `chan` (IQ at `rate_hz`):
/// `frame <n> B, <crc name> ok (<ok>/<frames>)` when at least one cut frame passes the
/// CRC (`n` the first such frame's length), else `None`. A framing without a `crc`
/// never proves, nor does a chain that does not run.
pub async fn attempt(chan: Vec<Complex<f32>>, rate_hz: u64, block: &FrameBlock) -> Option<String> {
    let framing: Framing = serde_json::from_value(block.framing.clone()).ok()?;
    let (_, crc) = framing.crc.as_ref()?.resolve().ok()?;
    let d = &block.demod;
    let chain = chain_rate_hz(rate_hz, d.symbol_rate_baud, d.modulation, d.order);
    let mut stages: Vec<(&str, Value)> = Vec::new();
    if chain != rate_hz {
        stages.push(("resample", json!({"out_rate_hz": chain})));
    }
    let decision = decision_nodes(d.modulation, d.symbol_rate_baud, d.order, d.tone_spacing_hz);
    stages.extend(decision.into_iter().map(|(_, kind, params)| (kind, params)));
    stages.push(("frame_decode", block.framing.clone()));
    let events = run_chain(&stages, &chan, rate_hz).await.ok()?;
    let frames: Vec<&Value> = events.iter().filter(|e| e["protocol"] == "frame").collect();
    let ok: Vec<&&Value> = frames.iter().filter(|e| e["crc_ok"] == true).collect();
    let first = ok.first()?;
    Some(format!(
        "frame {} B, {crc} ok ({}/{})",
        first["length_bytes"],
        ok.len(),
        frames.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{parse_params, synthesise};

    /// A frame block over Task 3's framing: fsk at 4800 Bd, `crc` given or not.
    fn block(crc: Option<&str>) -> FrameBlock {
        let mut framing =
            json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}});
        if let Some(c) = crc {
            framing["crc"] = c.into();
        }
        serde_json::from_value(json!({
            "demod": {"modulation": "fsk", "symbol_rate_baud": 4800},
            "framing": framing,
        }))
        .unwrap()
    }

    /// About a second of the `framing` family's frames at 48 kHz and 20 dB, framed by
    /// `block`: 16 frames of 272 bits, a 32-byte body and its CRC-16, or 34 bytes of body
    /// without a CRC.
    fn frames(block: &FrameBlock) -> Vec<Complex<f32>> {
        let body_bytes = if block.framing["crc"].is_null() {
            34
        } else {
            32
        };
        let params = json!({"family": "framing", "modulation": "fsk", "symbol_rate_baud": 4800,
            "deviation_hz": 2400.0, "frames": 16, "body_bytes": body_bytes, "seed": 3,
            "framing": block.framing, "repeat": 1, "snr_db": 20.0});
        let (spec, imp) = parse_params(params.as_object().unwrap().clone()).unwrap();
        synthesise(&spec, &imp, 48_000, 0)
    }

    #[tokio::test]
    async fn a_crc_clean_frame_proves_the_row() {
        let b = block(Some("crc16-ccitt"));
        let got = attempt(frames(&b), 48_000, &b).await.expect("a proof");
        assert!(got.starts_with("frame 34 B, crc16-ccitt ok ("), "{got}");
    }

    #[tokio::test]
    async fn a_framing_without_a_crc_never_proves() {
        let b = block(None);
        assert_eq!(attempt(frames(&b), 48_000, &b).await, None);
    }

    #[tokio::test]
    async fn noise_does_not_prove() {
        let b = block(Some("crc16-ccitt"));
        let mut noise = vec![Complex::new(0.0, 0.0); 48_000];
        crate::synth::awgn(&mut noise, 0.0, 9);
        assert_eq!(attempt(noise, 48_000, &b).await, None);
    }
}
