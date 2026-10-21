//! Transcribe stage: audio in, `transcript` events out, through a fake backend.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::fixtures::speech_like;
use zeta_speech::testing::FakeBackend;
use zeta_speech::{SpeechBackend, TranscribeStage};

const RATE: u64 = 48_000;
/// Input frames are 100 ms long.
const FRAME: usize = 4_800;
const FRAME_NS: u64 = 100_000_000;
/// t0 of the first input frame.
const T_BASE: u64 = 1_000_000_000;
const SEC: u64 = 1_000_000_000;

fn prov(t0: u64) -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(t0),
            end_ns: Timestamp(t0 + FRAME_NS),
        },
    }
}

fn audio(samples: &[f32], channels: u16, t0: u64) -> Frame {
    audio_at(samples, RATE, channels, t0)
}

fn audio_at(samples: &[f32], rate: u64, channels: u16, t0: u64) -> Frame {
    Frame::new(
        StreamId::new(),
        prov(t0),
        Payload::Audio {
            sample_rate_hz: Hz(rate),
            channels,
            samples: samples.into(),
        },
    )
}

/// Send `samples` (mono or interleaved) as 100 ms frames from `t0`; returns the end time.
fn send(tx: &broadcast::Sender<Frame>, samples: &[f32], channels: u16, t0: u64) -> u64 {
    send_at(tx, samples, RATE, channels, t0)
}

/// [`send`] at `rate`, in 100 ms frames.
fn send_at(
    tx: &broadcast::Sender<Frame>,
    samples: &[f32],
    rate: u64,
    channels: u16,
    t0: u64,
) -> u64 {
    let mut t = t0;
    for c in samples.chunks(rate as usize / 10 * channels as usize) {
        tx.send(audio_at(c, rate, channels, t)).unwrap();
        t += (c.len() / channels as usize) as u64 * SEC / rate;
    }
    t
}

struct Rig {
    tx: broadcast::Sender<Frame>,
    events: broadcast::Receiver<Frame>,
    run: tokio::task::JoinHandle<Result<(), StageError>>,
    cancel: CancellationToken,
}

fn start_with(backend: Option<Arc<dyn SpeechBackend>>, timeout: Duration, params: Value) -> Rig {
    let bus = Arc::new(StreamBus::new(1_024));
    let out = bus.register(StreamKind::Event, prov(0));
    let events = bus.subscribe(&out).unwrap();
    let (tx, rx) = broadcast::channel(1_024);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    let run = tokio::spawn(async move {
        TranscribeStage::new(backend, timeout)
            .run(
                params,
                vec![rx],
                vec![out],
                bus,
                c,
                StageContext::new(SourceId("s".into())),
            )
            .await
    });
    Rig {
        tx,
        events,
        run,
        cancel,
    }
}

fn start(fake: &Arc<FakeBackend>) -> Rig {
    start_with(
        Some(fake.clone() as Arc<dyn SpeechBackend>),
        Duration::from_secs(30),
        serde_json::json!({}),
    )
}

/// Close the input, wait for the stage, and return its result and every event it sent.
async fn finish(rig: Rig) -> (Result<(), StageError>, Vec<Value>) {
    drop(rig.tx);
    wait(rig.run, rig.events).await
}

/// Wait for the stage (its input closed) and return its result and the events left.
async fn wait(
    run: tokio::task::JoinHandle<Result<(), StageError>>,
    mut events: broadcast::Receiver<Frame>,
) -> (Result<(), StageError>, Vec<Value>) {
    let res = tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .expect("stage ends")
        .unwrap();
    let mut out = Vec::new();
    while let Ok(f) = events.try_recv() {
        out.push(event(f));
    }
    (res, out)
}

fn event(f: Frame) -> Value {
    let Payload::Event(v) = f.payload else {
        panic!("expected an event")
    };
    assert_eq!(v["protocol"], "transcript", "{v}");
    v
}

fn failure(res: Result<(), StageError>) -> String {
    match res {
        Err(StageError::Failed(m)) => m,
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// The fields of a 16-bit PCM WAV: (rate, channels, bits, data samples).
fn parse_wav(wav: &[u8]) -> (u32, u16, u16, Vec<i16>) {
    let u16_at = |at: usize| u16::from_le_bytes([wav[at], wav[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(u32_at(40) as usize, wav.len() - 44);
    let samples = wav[44..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    (u32_at(24), u16_at(22), u16_at(34), samples)
}

fn u(v: &Value, key: &str) -> u64 {
    v[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} missing in {v}"))
}

#[tokio::test]
async fn utterance_closes_on_gap() {
    let fake = Arc::new(FakeBackend::new("hello"));
    let rig = start(&fake);
    let burst = speech_like(RATE as u32, 1.0);
    let end1 = send(&rig.tx, &burst, 1, T_BASE);
    let t2 = end1 + 500_000_000;
    let end2 = send(&rig.tx, &burst, 1, t2);
    let (res, events) = finish(rig).await;
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(events.len(), 2, "{events:?}");
    for (v, (start, end)) in events.iter().zip([(T_BASE, end1), (t2, end2)]) {
        assert_eq!(v["text"], "hello");
        assert_eq!(v["language"], "en");
        assert_eq!(v["backend"], "fake");
        assert_eq!((u(v, "start_ns"), u(v, "end_ns")), (start, end), "{v}");
        assert!(u(v, "duration_ms").abs_diff(1_000) <= 5, "{v}");
    }
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn utterance_closes_at_max_length() {
    let fake = Arc::new(FakeBackend::new("long"));
    let rig = start(&fake);
    let end = send(&rig.tx, &speech_like(RATE as u32, 20.0), 1, T_BASE);
    let (res, events) = finish(rig).await;
    assert!(res.is_ok(), "{res:?}");
    let spans: Vec<(u64, u64)> = events
        .iter()
        .map(|v| (u(v, "start_ns"), u(v, "end_ns")))
        .collect();
    let cut = T_BASE + 15 * SEC;
    assert_eq!(spans, vec![(T_BASE, cut), (cut, end)]);
    let lens: Vec<usize> = fake
        .calls()
        .iter()
        .map(|(wav, _)| parse_wav(wav).3.len())
        .collect();
    assert_eq!(lens, vec![15 * 16_000, 5 * 16_000]);
}

#[tokio::test]
async fn short_utterance_discarded() {
    let fake = Arc::new(FakeBackend::new("x"));
    let rig = start(&fake);
    // 400 ms, a gap, then 1 s: only the second reaches the backend.
    let end = send(&rig.tx, &speech_like(RATE as u32, 0.4), 1, T_BASE);
    let t2 = end + SEC;
    send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, t2);
    let (res, events) = finish(rig).await;
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(u(&events[0], "start_ns"), t2);
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn wav_sent_is_16k_mono_s16() {
    let fake = Arc::new(FakeBackend::new("x"));
    let rig = start(&fake);
    let x = speech_like(RATE as u32, 1.5);
    send(&rig.tx, &x, 1, T_BASE);
    let (res, _) = finish(rig).await;
    assert!(res.is_ok(), "{res:?}");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let (wav, language) = &calls[0];
    assert_eq!(language, "en");
    let (rate, channels, bits, samples) = parse_wav(wav);
    assert_eq!((rate, channels, bits), (16_000, 1, 16));
    assert_eq!(samples.len(), 24_000);
    // The fixture peaks under 0.5: the WAV carries it under half scale.
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!((10_000..=16_500).contains(&peak), "peak {peak}");
}

#[tokio::test]
async fn stereo_input_is_downmixed() {
    let mono = speech_like(RATE as u32, 1.0);
    let wav_of = |samples: Vec<f32>, channels: u16| async move {
        let fake = Arc::new(FakeBackend::new("x"));
        let rig = start(&fake);
        send(&rig.tx, &samples, channels, T_BASE);
        let (res, _) = finish(rig).await;
        assert!(res.is_ok(), "{res:?}");
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        parse_wav(&calls[0].0)
    };
    let reference = wav_of(mono.clone(), 1).await;
    let same: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
    assert_eq!(wav_of(same, 2).await, reference);
    // Opposite-phase channels average to silence.
    let anti: Vec<f32> = mono.iter().flat_map(|&s| [s, -s]).collect();
    let (rate, channels, _, samples) = wav_of(anti, 2).await;
    assert_eq!((rate, channels), (16_000, 1));
    assert_eq!(samples.len(), 16_000);
    assert!(samples.iter().all(|&s| s == 0));
}

#[tokio::test]
async fn unsupported_language_fails_at_start() {
    let fake = Arc::new(FakeBackend::new("x").with_languages(&["en", "fr"]));
    let rig = start_with(
        Some(fake.clone() as Arc<dyn SpeechBackend>),
        Duration::from_secs(30),
        serde_json::json!({"language": "de"}),
    );
    // The input stays open: the stage must fail without waiting for audio.
    let res = tokio::time::timeout(Duration::from_secs(1), rig.run)
        .await
        .expect("fails at once")
        .unwrap();
    assert_eq!(
        failure(res),
        "transcribe: backend fake supports en, fr, not de"
    );
    drop(rig.tx);
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn no_backend_fails_at_start() {
    let rig = start_with(None, Duration::from_secs(30), serde_json::json!({}));
    let res = tokio::time::timeout(Duration::from_secs(1), rig.run)
        .await
        .expect("fails at once")
        .unwrap();
    assert_eq!(
        failure(res),
        "transcribe: this node has no speech backend (--stt-socket or --stt-node)"
    );
    drop(rig.tx);
}

#[tokio::test]
async fn one_error_is_an_event_two_fail_the_job() {
    // One failure, then a success: an error event, a transcript, a clean end.
    let fake = Arc::new(FakeBackend::new("ok"));
    fake.fail_next(1);
    let rig = start(&fake);
    let end = send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, T_BASE);
    send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, end + SEC);
    let (res, events) = finish(rig).await;
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["error"], "speech daemon: fake failure");
    assert_eq!(
        (u(&events[0], "start_ns"), u(&events[0], "end_ns")),
        (T_BASE, end)
    );
    assert!(events[0].get("text").is_none());
    assert_eq!(events[1]["text"], "ok");

    // Two consecutive failures end the job, naming the backend.
    let fake = Arc::new(FakeBackend::new("ok"));
    fake.fail_next(2);
    let rig = start(&fake);
    let end = send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, T_BASE);
    send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, end + SEC);
    let (res, events) = finish(rig).await;
    assert_eq!(
        failure(res),
        "transcribe: backend fake: speech daemon: fake failure"
    );
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events.iter().all(|v| v["error"].is_string()), "{events:?}");
}

#[tokio::test]
async fn slow_backend_does_not_stall_input_and_overflow_is_reported() {
    let fake = Arc::new(FakeBackend::new("slow"));
    fake.set_delay(Duration::from_secs(2));
    let Rig {
        tx,
        mut events,
        run,
        cancel: _cancel,
    } = start(&fake);
    // Five 600 ms utterances, 1 s apart, all at once. At 16 kHz the stage does not
    // resample, so the 100 ms bound measures the queueing, not an unoptimised build's FIR.
    let burst = speech_like(16_000, 0.6);
    let mut starts = Vec::new();
    let mut t = T_BASE;
    for _ in 0..5 {
        starts.push(t);
        t = send_at(&tx, &burst, 16_000, 1, t) + SEC;
    }
    let sent = Instant::now();
    // The last utterance closes when the input does.
    drop(tx);
    // The two that overflow the queue are reported at once.
    let mut dropped = Vec::new();
    while dropped.len() < 2 {
        let left = Duration::from_millis(500).saturating_sub(sent.elapsed());
        let f = tokio::time::timeout(left, events.recv())
            .await
            .expect("overflow reported within 500 ms")
            .unwrap();
        dropped.push(event(f));
    }
    assert!(sent.elapsed() < Duration::from_millis(500));
    for (v, start) in dropped.iter().zip(&starts[3..]) {
        assert_eq!(v["dropped"], true, "{v}");
        assert_eq!(u(v, "start_ns"), *start, "the newest are dropped: {v}");
        assert!(u(v, "end_ns") > *start);
    }
    // Only the first has reached the backend so far.
    assert_eq!(fake.calls().len(), 1);
    let (res, events) = wait(run, events).await;
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(events.len(), 3, "{events:?}");
    for (v, start) in events.iter().zip(&starts) {
        assert_eq!(v["text"], "slow", "{v}");
        assert_eq!(u(v, "start_ns"), *start);
    }
    assert_eq!(fake.calls().len(), 3);
}

#[tokio::test]
async fn timeout_is_reported() {
    let fake = Arc::new(FakeBackend::new("late"));
    fake.set_delay(Duration::from_secs(5));
    let rig = start_with(
        Some(fake.clone() as Arc<dyn SpeechBackend>),
        Duration::from_millis(200),
        serde_json::json!({}),
    );
    let end = send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, T_BASE);
    let t = Instant::now();
    let (res, events) = finish(rig).await;
    assert!(res.is_ok(), "{res:?}");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0]["error"],
        "the speech daemon did not answer within 0.2 s"
    );
    assert_eq!(
        (u(&events[0], "start_ns"), u(&events[0], "end_ns")),
        (T_BASE, end)
    );
}

#[tokio::test]
async fn cancel_ends_the_stage_promptly() {
    let fake = Arc::new(FakeBackend::new("x"));
    fake.set_delay(Duration::from_secs(30));
    let rig = start(&fake);
    let end = send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, T_BASE);
    // A frame after a gap closes the utterance, so the backend call is in flight.
    send(&rig.tx, &[0.0; FRAME], 1, end + SEC);
    let deadline = Instant::now() + Duration::from_secs(2);
    while fake.calls().is_empty() && Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert_eq!(fake.calls().len(), 1);
    rig.cancel.cancel();
    let res = tokio::time::timeout(Duration::from_millis(500), rig.run)
        .await
        .expect("cancel is prompt")
        .unwrap();
    assert!(matches!(res, Err(StageError::Cancelled)), "{res:?}");
}

#[tokio::test]
async fn rate_change_fails() {
    let fake = Arc::new(FakeBackend::new("x"));
    let rig = start(&fake);
    rig.tx.send(audio(&[0.0; FRAME], 1, T_BASE)).unwrap();
    rig.tx
        .send(Frame::new(
            StreamId::new(),
            prov(T_BASE + FRAME_NS),
            Payload::Audio {
                sample_rate_hz: Hz(16_000),
                channels: 1,
                samples: vec![0.0; 1_600].into(),
            },
        ))
        .unwrap();
    let (res, _) = finish(rig).await;
    assert_eq!(failure(res), "sample_rate_hz changed from 48000 to 16000");
}

#[tokio::test]
async fn utterance_closes_after_gap_of_wall_clock_silence() {
    // vad forwards only speech, so after a transmission no frame follows: the open
    // utterance must close on wall-clock silence, its span still taken from the audio.
    let fake = Arc::new(FakeBackend::new("over"));
    let mut rig = start(&fake);
    let end = send(&rig.tx, &speech_like(RATE as u32, 1.0), 1, T_BASE);
    let sent = Instant::now();
    let f = tokio::time::timeout(Duration::from_millis(1_500), rig.events.recv())
        .await
        .expect("closed within 1.5 s of the last frame")
        .unwrap();
    let v = event(f);
    assert!(
        sent.elapsed() >= Duration::from_millis(300),
        "{:?}",
        sent.elapsed()
    );
    assert_eq!(v["text"], "over");
    assert_eq!((u(&v, "start_ns"), u(&v, "end_ns")), (T_BASE, end));
    // The input is still open; closing it sends nothing more.
    let (res, rest) = finish(rig).await;
    assert!(res.is_ok(), "{res:?}");
    assert!(rest.is_empty(), "{rest:?}");
    assert_eq!(fake.calls().len(), 1);
}

/// Skipped input is counted under `transcribe` in `zeta_dsp`'s per-kind lag counter.
#[tokio::test]
async fn lagged_input_is_counted_by_kind() {
    let fake = Arc::new(FakeBackend::new("hello"));
    let bus = Arc::new(StreamBus::new(64));
    let out = bus.register(StreamKind::Event, prov(0));
    let events = bus.subscribe(&out).unwrap();
    // A 4-frame input sent 10 frames before the stage runs: it starts 6 behind.
    let (tx, rx) = broadcast::channel(4);
    send(&tx, &speech_like(RATE as u32, 1.0), 1, T_BASE);
    drop(tx);
    let run = tokio::spawn(async move {
        TranscribeStage::new(
            Some(fake as Arc<dyn SpeechBackend>),
            Duration::from_secs(30),
        )
        .run(
            serde_json::json!({}),
            vec![rx],
            vec![out],
            bus,
            CancellationToken::new(),
            StageContext::new(SourceId("s".into())),
        )
        .await
    });
    let (res, _) = wait(run, events).await;
    assert!(res.is_ok(), "{res:?}");
    let by_kind = zeta_dsp::lagged_frames_by_kind();
    assert!(
        by_kind.iter().any(|&(k, n)| k == "transcribe" && n >= 6),
        "{by_kind:?}"
    );
}
