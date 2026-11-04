//! Vad stage: gated audio on output 0, speech transitions on output 1.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::fixtures::{awgn_real, speech_like};
use zeta_dsp::vad::VadStage;

/// t0 of the first input frame.
const T_BASE: u64 = 1_000;

fn prov(t0: u64) -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(t0),
            end_ns: Timestamp(t0 + 100_000_000),
        },
    }
}

fn audio(samples: Vec<f32>, rate_hz: u64, channels: u16, t0: u64) -> Frame {
    Frame::new(
        StreamId::new(),
        prov(t0),
        Payload::Audio {
            sample_rate_hz: Hz(rate_hz),
            channels,
            samples: samples.into(),
        },
    )
}

type Run = tokio::task::JoinHandle<Result<(), StageError>>;

fn spawn_with(
    params: serde_json::Value,
    bus: Arc<StreamBus>,
    rx: broadcast::Receiver<Frame>,
    outs: Vec<StreamId>,
) -> Run {
    tokio::spawn(async move {
        VadStage::new()
            .run(
                params,
                vec![rx],
                outs,
                bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
    })
}

fn spawn(bus: Arc<StreamBus>, rx: broadcast::Receiver<Frame>, outs: Vec<StreamId>) -> Run {
    spawn_with(serde_json::json!({}), bus, rx, outs)
}

#[tokio::test]
async fn gates_audio_and_emits_transitions() {
    let rate = 48_000u64;
    let bus = Arc::new(StreamBus::new(256));
    let a_out = bus.register(StreamKind::Audio, prov(0));
    let e_out = bus.register(StreamKind::Event, prov(0));
    let mut rx_a = bus.subscribe(&a_out).unwrap();
    let mut rx_e = bus.subscribe(&e_out).unwrap();
    let (tx, rx_in) = broadcast::channel(256);
    // A hangover shorter than the 300 ms gaps keeps the bursts apart.
    let h = spawn_with(
        serde_json::json!({"hangover_ms": 200}),
        bus,
        rx_in,
        vec![a_out, e_out],
    );

    // Noise-only lead-in, then 3.6 s of bursts; stereo with identical channels.
    let mut mono = vec![0.0; 4_800];
    mono.extend(speech_like(48_000, 3.6));
    awgn_real(&mut mono, 20.0, 3);
    let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
    for (i, c) in stereo.chunks(9_600).enumerate() {
        let t0 = T_BASE + i as u64 * 100_000_000;
        tx.send(audio(c.to_vec(), rate, 2, t0)).unwrap();
    }
    drop(tx);
    assert!(h.await.unwrap().is_ok());

    let mut events = Vec::new();
    while let Ok(f) = rx_e.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("expected event")
        };
        assert_eq!(v["protocol"], "vad");
        events.push((v["speech"].as_bool().unwrap(), v["t_ns"].as_u64().unwrap()));
    }
    // Six bursts, each one speech run: 12 transitions.
    assert_eq!(events.len(), 12, "{events:?}");
    for (i, (speech, _)) in events.iter().enumerate() {
        assert_eq!(*speech, i % 2 == 0);
    }
    // Burst k starts 100 ms (the lead-in) + k * 600 ms after the first frame's
    // t0; its run begins at its first loud frame, within one 20 ms frame, and
    // lasts the burst plus the 200 ms hangover.
    let frame_ns = 20_000_000u64;
    for k in 0..6usize {
        let burst = T_BASE + 100_000_000 + k as u64 * 600_000_000;
        let (_, on) = events[2 * k];
        let (_, off) = events[2 * k + 1];
        assert!(on.abs_diff(burst) <= frame_ns, "burst {k} on at {on}");
        assert!(
            (off - on).abs_diff(500_000_000) <= frame_ns,
            "burst {k} lasts {}",
            off - on
        );
    }

    // Forwarded audio is 48 kHz stereo; each frame's samples are the matching
    // slice of the original input, located from its start time, and its span
    // length is its sample count at the rate.
    let mut starts = Vec::new();
    let mut forwarded = 0;
    while let Ok(f) = rx_a.try_recv() {
        let Payload::Audio {
            sample_rate_hz,
            channels,
            samples,
        } = &f.payload
        else {
            panic!("expected audio")
        };
        assert_eq!((sample_rate_hz.0, *channels), (rate, 2));
        let n = samples.len() / 2;
        let span = f.provenance.span;
        assert_eq!(
            span.end_ns.0 - span.start_ns.0,
            n as u64 * 1_000_000_000 / rate
        );
        let idx = ((span.start_ns.0 - T_BASE) * rate + 500_000_000) / 1_000_000_000;
        let idx = idx as usize;
        assert_eq!(&stereo[idx * 2..(idx + n) * 2], &samples[..], "at {idx}");
        starts.push(span.start_ns.0);
        forwarded += n;
    }
    for (_, t) in events.iter().filter(|(s, _)| *s) {
        assert!(starts.contains(t), "no audio starts at {t}: {starts:?}");
    }
    // Six runs of about 500 ms.
    assert!(forwarded.abs_diff(6 * 24_000) <= 6 * 2 * 960, "{forwarded}");
}

/// 100 ms of 48 kHz mono: one input frame.
const CHUNK: usize = 4_800;
const FRAME_NS: u64 = 20_000_000;

/// Speech transitions (speech, t_ns) and forwarded audio spans (start, end).
type Outputs = (Vec<(bool, u64)>, Vec<(u64, u64)>);

fn drain(rx_a: &mut broadcast::Receiver<Frame>, rx_e: &mut broadcast::Receiver<Frame>) -> Outputs {
    let mut events = Vec::new();
    while let Ok(f) = rx_e.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("expected event")
        };
        events.push((v["speech"].as_bool().unwrap(), v["t_ns"].as_u64().unwrap()));
    }
    let mut audio = Vec::new();
    while let Ok(f) = rx_a.try_recv() {
        let span = f.provenance.span;
        audio.push((span.start_ns.0, span.end_ns.0));
    }
    (events, audio)
}

/// About -50 dB of white noise.
fn noise(n: usize, seed: u64) -> Vec<f32> {
    let mut x = vec![0.0; n];
    awgn_real(&mut x, 50.0, seed);
    x
}

/// `x` plus -50 dB noise.
fn noisy(mut x: Vec<f32>, seed: u64) -> Vec<f32> {
    let n = noise(x.len(), seed);
    for (s, n) in x.iter_mut().zip(n) {
        *s += n;
    }
    x
}

/// Send `x` as 100 ms mono frames, numbering them from `*i`.
fn send(tx: &broadcast::Sender<Frame>, x: &[f32], i: &mut u64) {
    for c in x.chunks(CHUNK) {
        tx.send(audio(c.to_vec(), 48_000, 1, T_BASE + *i * 100_000_000))
            .unwrap();
        *i += 1;
    }
}

/// Wait until the stage has taken every queued frame.
async fn drained(tx: &broadcast::Sender<Frame>) {
    while !tx.is_empty() {
        tokio::task::yield_now().await;
    }
}

/// Input queue depth of the lag runs, in frames.
const LAG_CAP: usize = 16;

/// Run the stage (200 ms hangover) on `before`, then two frames it never sees
/// (it lags), then `after` (exactly the queue depth, so only the two are lost),
/// then `tail`. Also returns the t0 of `after`'s first frame.
async fn run_with_lag(before: &[f32], after: &[f32], tail: &[f32]) -> (Outputs, u64) {
    assert!(before.len() <= LAG_CAP * CHUNK && tail.len() <= LAG_CAP * CHUNK);
    assert_eq!(after.len(), LAG_CAP * CHUNK);
    let bus = Arc::new(StreamBus::new(256));
    let a_out = bus.register(StreamKind::Audio, prov(0));
    let e_out = bus.register(StreamKind::Event, prov(0));
    let mut rx_a = bus.subscribe(&a_out).unwrap();
    let mut rx_e = bus.subscribe(&e_out).unwrap();
    let (tx, rx_in) = broadcast::channel(LAG_CAP);
    let h = spawn_with(
        serde_json::json!({"hangover_ms": 200}),
        bus,
        rx_in,
        vec![a_out, e_out],
    );
    let mut i = 0;
    send(&tx, before, &mut i);
    drained(&tx).await;
    send(&tx, &[0.0; 2 * CHUNK], &mut i);
    let after_t0 = T_BASE + i * 100_000_000;
    send(&tx, after, &mut i);
    drained(&tx).await;
    send(&tx, tail, &mut i);
    drop(tx);
    assert!(h.await.unwrap().is_ok());
    (drain(&mut rx_a, &mut rx_e), after_t0)
}

#[tokio::test]
async fn lag_in_noise_keeps_the_floor() {
    // Noise, a lag, the same noise: still no speech.
    let ((events, audio), _) =
        run_with_lag(&noise(10 * CHUNK, 21), &noise(LAG_CAP * CHUNK, 22), &[]).await;
    assert!(events.is_empty(), "{events:?}");
    assert!(audio.is_empty(), "{audio:?}");
}

#[tokio::test]
async fn burst_right_after_a_lag_is_detected() {
    // The floor learnt from the noise before the lag survives it, so a burst in
    // the first frame after the lag is speech. Bursts at 0, 600 and 1200 ms.
    let after = noisy(speech_like(48_000, 1.6), 23);
    let ((events, audio), after_t0) =
        run_with_lag(&noise(10 * CHUNK, 21), &after, &noise(5 * CHUNK, 24)).await;
    assert_eq!(events.len(), 6, "{events:?}");
    for (k, w) in events.chunks(2).enumerate() {
        let burst = after_t0 + k as u64 * 600_000_000;
        assert!(w[0].0 && !w[1].0, "{events:?}");
        assert!(w[0].1.abs_diff(burst) <= FRAME_NS, "burst {k}: {events:?}");
    }
    assert!(audio.iter().all(|&(s, _)| s >= after_t0), "{audio:?}");
}

#[tokio::test]
async fn lag_closes_an_open_run_once() {
    // 500 ms of noise, then a burst still open at the lag; noise after it.
    let mut before = noise(5 * CHUNK, 21);
    before.extend(noisy(speech_like(48_000, 0.3), 25));
    let ((events, audio), after_t0) = run_with_lag(&before, &noise(LAG_CAP * CHUNK, 22), &[]).await;
    // One run: opened at the burst, closed at the lag where its forwarded audio
    // stops (the look-back it still held is dropped), and nothing after.
    assert_eq!(events.len(), 2, "{events:?}");
    let ((on, t_on), (off, t_off)) = (events[0], events[1]);
    assert!(on && !off, "{events:?}");
    assert!(
        t_on.abs_diff(T_BASE + 500_000_000) <= FRAME_NS,
        "{events:?}"
    );
    let end = audio.iter().map(|&(_, e)| e).max().expect("no audio");
    assert!(
        t_off.abs_diff(end) <= 1_000,
        "closed at {t_off}, audio ends {end}"
    );
    assert!(t_off < after_t0, "{events:?}");
}

#[tokio::test]
async fn first_frame_mid_burst_is_heard_from_the_second_burst() {
    let rate = 48_000u64;
    let bus = Arc::new(StreamBus::new(256));
    let a_out = bus.register(StreamKind::Audio, prov(0));
    let e_out = bus.register(StreamKind::Event, prov(0));
    let mut rx_a = bus.subscribe(&a_out).unwrap();
    let mut rx_e = bus.subscribe(&e_out).unwrap();
    let (tx, rx_in) = broadcast::channel(256);
    let h = spawn_with(
        serde_json::json!({"hangover_ms": 200}),
        bus,
        rx_in,
        vec![a_out, e_out],
    );
    // The clean fixture (exact-zero gaps) from 150 ms into its first burst:
    // the first frame seeds the floor at speech level. Whole bursts follow at
    // 450 ms + k * 600 ms, k = 0..6.
    let mut x = speech_like(48_000, 4.2);
    let x = x.split_off(7_200);
    let mut i = 0;
    send(&tx, &x, &mut i);
    drop(tx);
    assert!(h.await.unwrap().is_ok());
    let (events, audio) = drain(&mut rx_a, &mut rx_e);
    // The partial first burst is missed; the first zero gap lowers the floor,
    // and every whole burst is one run.
    assert_eq!(events.len(), 12, "{events:?}");
    for (k, w) in events.chunks(2).enumerate() {
        let burst = T_BASE + 450_000_000 + k as u64 * 600_000_000;
        assert!(w[0].0 && !w[1].0, "{events:?}");
        assert!(w[0].1.abs_diff(burst) <= FRAME_NS, "burst {k}: {events:?}");
    }
    let first = events[0].1;
    assert!(audio.iter().all(|&(s, _)| s >= first), "{audio:?}");
    let forwarded: u64 = audio.iter().map(|&(s, e)| e - s).sum();
    let want = 6 * 500_000_000u64;
    assert!(
        forwarded.abs_diff(want) <= 6 * 2 * FRAME_NS,
        "{forwarded} ns forwarded at {rate} Hz"
    );
}

#[tokio::test]
async fn single_output_skips_events() {
    let bus = Arc::new(StreamBus::new(8));
    let a_out = bus.register(StreamKind::Audio, prov(0));
    let (tx, rx_in) = broadcast::channel(8);
    let h = spawn(bus, rx_in, vec![a_out]);
    tx.send(audio(vec![0.0; 4_800], 48_000, 1, 0)).unwrap();
    drop(tx);
    assert!(h.await.unwrap().is_ok());
}

#[tokio::test]
async fn rate_and_channel_changes_fail() {
    for (rate, ch, msg) in [
        (16_000, 1, "sample_rate_hz changed from 48000 to 16000"),
        (48_000, 2, "channels changed from 1 to 2"),
    ] {
        let bus = Arc::new(StreamBus::new(8));
        let a_out = bus.register(StreamKind::Audio, prov(0));
        let e_out = bus.register(StreamKind::Event, prov(0));
        let (tx, rx_in) = broadcast::channel(8);
        let h = spawn(bus, rx_in, vec![a_out, e_out]);
        tx.send(audio(vec![0.0; 4_800], 48_000, 1, 0)).unwrap();
        tx.send(audio(vec![0.0; 4_800], rate, ch, 0)).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert_eq!(m, msg),
            other => panic!("{other:?}"),
        }
    }
}

/// Send `x` as 100 ms mono frames, the first starting at `t0`.
fn send_at(tx: &broadcast::Sender<Frame>, x: &[f32], t0: u64) {
    for (k, c) in x.chunks(CHUNK).enumerate() {
        tx.send(audio(c.to_vec(), 48_000, 1, t0 + k as u64 * 100_000_000))
            .unwrap();
    }
}

#[tokio::test]
async fn a_gap_in_the_input_restarts_the_clock() {
    // A squelched demodulator publishes nothing while closed: two transmissions
    // arrive as frames 5 s apart with nothing between them. The second must be
    // stamped at its own frames' time, not straight after the first's samples, and
    // the first run must close where its input stopped.
    let bus = Arc::new(StreamBus::new(256));
    let a_out = bus.register(StreamKind::Audio, prov(0));
    let e_out = bus.register(StreamKind::Event, prov(0));
    let mut rx_a = bus.subscribe(&a_out).unwrap();
    let mut rx_e = bus.subscribe(&e_out).unwrap();
    let (tx, rx_in) = broadcast::channel(256);
    let h = spawn_with(
        serde_json::json!({"hangover_ms": 200}),
        bus,
        rx_in,
        vec![a_out, e_out],
    );
    // First transmission: 500 ms of noise, a 300 ms burst, then a squelch tail
    // that leaves a part-frame (and the onset look-back) held in the stage.
    let mut first = noise(5 * CHUNK, 31);
    first.extend(noisy(speech_like(48_000, 0.3), 32));
    first.extend(noise(1_000, 33));
    send_at(&tx, &first, T_BASE);
    let first_end = T_BASE + first.len() as u64 * 1_000_000_000 / 48_000;
    // Second transmission 5 s later: a burst at its first frame, then noise.
    let second_t0 = first_end + 5_000_000_000;
    let mut second = noisy(speech_like(48_000, 0.3), 34);
    second.extend(noise(5 * CHUNK, 35));
    send_at(&tx, &second, second_t0);
    drop(tx);
    assert!(h.await.unwrap().is_ok());
    let (events, audio) = drain(&mut rx_a, &mut rx_e);

    assert_eq!(events.len(), 4, "{events:?}");
    let speech: Vec<bool> = events.iter().map(|e| e.0).collect();
    assert_eq!(speech, [true, false, true, false], "{events:?}");
    assert!(
        events[0].1.abs_diff(T_BASE + 500_000_000) <= FRAME_NS,
        "{events:?}"
    );
    assert!(
        events[1].1 <= first_end,
        "first run closes by the end of its input: {events:?}"
    );
    assert!(
        events[2].1.abs_diff(second_t0) <= FRAME_NS,
        "second run stamped at {}, its frames start at {second_t0}",
        events[2].1
    );
    // No forwarded audio falls in the gap.
    for &(s, e) in &audio {
        assert!(
            e <= first_end || s >= second_t0,
            "audio {s}..{e} in the gap"
        );
    }
    assert!(audio.iter().any(|&(s, _)| s >= second_t0), "{audio:?}");
}
