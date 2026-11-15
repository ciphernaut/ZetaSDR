//! The vocoder seat (design 38, 5.1): one call's speech through an external program.
//!
//! One call's vocoder is one child process, spawned with an empty environment and killed
//! when dropped: fixed-size feeds on its stdin, 16-bit little-endian mono speech at the
//! spec's rate on its stdout, resampled to 48 kHz and published on the stage's audio
//! output, one frame of audio per feed with that feed's span. What the program takes and
//! gives is the stage's [`VocoderSpec`]; the seat assumes none of it.
//!
//! Its exit fails nothing but that call's audio: the job's context says why once, under
//! the spec's context key (`{<context>: {"vocoder": <why>}}`), and the stage spawns it
//! again at its next call. A program that has taken feeds and given nothing back for
//! [`VOCODER_WATCHDOG_MS`] is killed and noted the same way.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use zeta_core::stage::{StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, Span, StreamId, Timestamp};

use crate::resample::Resampler;

/// How long a vocoder with feeds outstanding may give nothing back before it is killed,
/// ms.
pub const VOCODER_WATCHDOG_MS: u64 = 2000;
/// The rate of the speech the seat publishes, Hz.
pub const AUDIO_RATE_HZ: u64 = 48_000;
/// How long the vocoders of a finished input may take to hand back their speech.
pub const VOCODER_DRAIN: Duration = Duration::from_secs(2);

/// What a stage states about the vocoder for one call.
#[derive(Clone, Debug)]
pub struct VocoderSpec {
    /// The program's name in notes: the handler's name, not where it was found.
    pub name: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Bytes handed over per feed.
    pub feed_bytes: usize,
    /// Samples expected back per feed, at `rate_hz`.
    pub feed_samples: usize,
    /// The rate of the program's speech, Hz.
    pub rate_hz: u64,
    /// The job context key the vocoder's notes go under (`"m17"`, `"p25"`, `"dmr"`).
    pub context: &'static str,
}

/// What a vocoder needs from its stage.
#[derive(Clone)]
pub struct VocoderEnv {
    pub bus: Arc<StreamBus>,
    /// The stage's audio output.
    pub out: StreamId,
    pub ctx: StageContext,
    /// The reader tasks of finished vocoders, awaited when the input ends.
    pub drains: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl VocoderEnv {
    /// End `v`'s call: close its input and keep its reader, still publishing the speech
    /// in flight, for [`drain_all`](Self::drain_all).
    pub fn park(&self, mut v: Vocoder) {
        if let Some(h) = v.finish() {
            let mut d = self.drains.lock().unwrap();
            d.retain(|h| !h.is_finished());
            d.push(h);
        }
    }

    /// At the stage's end: after a clean end (`result` ok) every parked reader gets the one
    /// [`VOCODER_DRAIN`] deadline; any still running after it, or every one after a failed
    /// or cancelled run, is aborted (its child is killed with it).
    pub async fn drain_all(&self, result: &Result<(), StageError>) {
        let mut pending: Vec<JoinHandle<()>> = std::mem::take(&mut *self.drains.lock().unwrap());
        if result.is_ok() {
            let _ = tokio::time::timeout(VOCODER_DRAIN, async {
                for h in pending.iter_mut() {
                    let _ = h.await;
                }
            })
            .await;
        }
        pending.iter().for_each(JoinHandle::abort);
    }
}

/// One call's vocoder: feeds in on its stdin, speech out on its stdout to the audio
/// output.
pub struct Vocoder {
    feeds: Option<mpsc::UnboundedSender<Vec<u8>>>,
    spans: Option<mpsc::UnboundedSender<Span>>,
    feed_bytes: usize,
    closing: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
}

impl Vocoder {
    /// `spec`'s program for one call, publishing on `env.out` with `prov`.
    pub fn spawn(env: &VocoderEnv, spec: &VocoderSpec, prov: Provenance) -> Result<Self, String> {
        let name = spec.name;
        let mut resampler = Resampler::new(spec.rate_hz, AUDIO_RATE_HZ)
            .map_err(|e| format!("{name}: speech at {} Hz: {e}", spec.rate_hz))?;
        let mut child = tokio::process::Command::new(&spec.program)
            .args(&spec.args)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("{name} failed to start: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (feeds, mut feeds_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (spans, mut spans_rx) = mpsc::unbounded_channel::<Span>();
        let closing = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let writer = tokio::spawn(async move {
            while let Some(b) = feeds_rx.recv().await {
                if stdin.write_all(&b).await.is_err() {
                    break;
                }
            }
        });
        let (bus, out, ctx, done, live) = (
            env.bus.clone(),
            env.out.clone(),
            env.ctx.clone(),
            closing.clone(),
            alive.clone(),
        );
        let (feed_samples, context) = (spec.feed_samples, spec.context);
        let feed_ns = feed_samples as u64 * 1_000_000_000 / spec.rate_hz;
        let watchdog = Duration::from_millis(VOCODER_WATCHDOG_MS);
        let reader = tokio::spawn(async move {
            let mut raw = Vec::new();
            let mut speech: Vec<f32> = Vec::new();
            let mut buf = [0u8; 4096];
            let mut last_end = 0u64;
            // The spans of the feeds whose speech has not come back yet.
            let mut outstanding: VecDeque<Span> = VecDeque::new();
            let mut feeding = true;
            // Armed while feeds are outstanding; reset by each read.
            let mut deadline: Option<Instant> = None;
            let stalled = loop {
                tokio::select! {
                    r = stdout.read(&mut buf) => {
                        match r {
                            Ok(0) | Err(_) => break false,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                        while let Ok(s) = spans_rx.try_recv() {
                            outstanding.push_back(s);
                        }
                        let whole = raw.len() / 2 * 2;
                        speech.extend(
                            raw.drain(..whole)
                                .collect::<Vec<u8>>()
                                .chunks(2)
                                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0),
                        );
                        while speech.len() >= feed_samples {
                            let chunk: Vec<f32> = speech.drain(..feed_samples).collect();
                            let span = outstanding.pop_front().unwrap_or(Span {
                                start_ns: Timestamp(last_end),
                                end_ns: Timestamp(last_end + feed_ns),
                            });
                            last_end = span.end_ns.0;
                            let samples = resampler.process(&chunk);
                            if samples.is_empty() {
                                continue;
                            }
                            let mut p = prov.clone();
                            p.span = span;
                            bus.publish(Frame::new(
                                out.clone(),
                                p,
                                Payload::Audio {
                                    sample_rate_hz: Hz(AUDIO_RATE_HZ),
                                    channels: 1,
                                    samples: samples.into(),
                                },
                            ));
                        }
                        deadline = (!outstanding.is_empty()).then(|| Instant::now() + watchdog);
                    }
                    s = spans_rx.recv(), if feeding => match s {
                        Some(s) => {
                            if outstanding.is_empty() {
                                deadline = Some(Instant::now() + watchdog);
                            }
                            outstanding.push_back(s);
                        }
                        None => feeding = false,
                    },
                    _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)),
                        if deadline.is_some() => break true,
                }
            };
            let why = if stalled {
                let _ = child.kill().await;
                format!("{name} stopped answering: no speech for {VOCODER_WATCHDOG_MS} ms, killed")
            } else {
                match child.wait().await {
                    Ok(s) if s.success() && done.load(Ordering::SeqCst) => {
                        live.store(false, Ordering::SeqCst);
                        return;
                    }
                    Ok(s) => format!("{name} exited: {s}"),
                    Err(e) => format!("{name} exited: {e}"),
                }
            };
            // Not alive before the note: a stage that has seen the note sees the exit.
            live.store(false, Ordering::SeqCst);
            ctx.note(serde_json::json!({ context: {"vocoder": why} }));
        });
        Ok(Self {
            feeds: Some(feeds),
            spans: Some(spans),
            feed_bytes: spec.feed_bytes,
            closing,
            alive,
            writer: Some(writer),
            reader: Some(reader),
        })
    }

    /// Hand over one feed; `bytes.len()` must equal the spec's `feed_bytes`, and a feed of
    /// any other length is dropped.
    pub fn feed(&self, span: Span, bytes: &[u8]) {
        if bytes.len() != self.feed_bytes {
            return;
        }
        if let (Some(s), Some(f)) = (&self.spans, &self.feeds) {
            let _ = s.send(span);
            let _ = f.send(bytes.to_vec());
        }
    }

    /// False once the child has exited or the watchdog killed it.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Close the input; the reader, still publishing, is returned to be awaited.
    pub fn finish(&mut self) -> Option<JoinHandle<()>> {
        self.closing.store(true, Ordering::SeqCst);
        self.feeds = None;
        self.spans = None;
        self.writer = None;
        self.reader.take()
    }
}

impl Drop for Vocoder {
    fn drop(&mut self) {
        // Not finished: the stage failed or was cancelled, so the child goes too.
        for h in [self.writer.take(), self.reader.take()]
            .into_iter()
            .flatten()
        {
            h.abort();
        }
    }
}

/// The row's handler `index`, looked up now (so a program installed while the job runs is
/// found at the next call); for a stage run without a row, `fallback` found the same way.
/// `Ok(None)` when it is absent and the row marks it optional, or the stage runs without
/// a row and `fallback` is not on the host and `required` is false.
pub fn program(
    ctx: &StageContext,
    index: usize,
    fallback: &str,
    required: bool,
) -> Result<Option<PathBuf>, String> {
    use zeta_core::modes::handlers::{find_handler, resolve};
    match &ctx.mode {
        Some(row) => match resolve(row, index) {
            Ok(p) => Ok(Some(p)),
            Err(_)
                if row
                    .decoder
                    .handler_blocks()
                    .get(index)
                    .is_some_and(|b| b.optional) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        },
        None => match find_handler(&[fallback.to_string()]) {
            Some(p) => Ok(Some(p)),
            None if required => Err(format!(
                "vocoder: handler {fallback} not found (tried {fallback})"
            )),
            None => Ok(None),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::broadcast;
    use zeta_core::stage::StageContext;
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Provenance, SourceId, Span, StreamKind, Timestamp};

    use super::*;

    /// The fakes' name in notes.
    const NAME: &str = "fake-vocoder";
    /// One feed's span, ns (160 samples at 8 kHz).
    const FEED_NS: u64 = 20_000_000;

    fn prov() -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(0),
            },
        }
    }

    fn span(n: u64) -> Span {
        Span {
            start_ns: Timestamp(1_000_000_000 + n * FEED_NS),
            end_ns: Timestamp(1_000_000_000 + (n + 1) * FEED_NS),
        }
    }

    /// A fake program: `body` as a shell script in `dir`. It is run through `/bin/sh`
    /// rather than executed, so a fork elsewhere in the test process that still holds the
    /// script open for writing cannot make the exec fail.
    fn fake(dir: &Path, body: &str) -> PathBuf {
        let script = dir.join("fake-vocoder.sh");
        std::fs::write(&script, format!("PATH=/usr/bin:/bin\n{body}\n")).unwrap();
        script
    }

    /// 96 bytes in, 160 silent samples (320 bytes) out, per feed; exit 0 at the end of
    /// the input.
    const ECHO: &str = r#"while :; do
    n=$(dd bs=96 count=1 iflag=fullblock 2>/dev/null | wc -c)
    [ "$n" -gt 0 ] || exit 0
    head -c 320 /dev/zero
done"#;

    fn spec(script: &Path) -> VocoderSpec {
        VocoderSpec {
            name: NAME,
            program: "/bin/sh".into(),
            args: vec![script.to_str().unwrap().into()],
            feed_bytes: 96,
            feed_samples: 160,
            rate_hz: 8_000,
            context: "p25",
        }
    }

    struct Seat {
        env: VocoderEnv,
        audio: broadcast::Receiver<Frame>,
        notes: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    fn seat() -> Seat {
        let bus = Arc::new(StreamBus::new(1024));
        let out = bus.register(StreamKind::Audio, prov());
        let audio = bus.subscribe(&out).unwrap();
        let notes = Arc::new(Mutex::new(Vec::new()));
        let n = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_note(Arc::new(move |v| n.lock().unwrap().push(v)));
        Seat {
            env: VocoderEnv {
                bus,
                out,
                ctx,
                drains: Arc::new(Mutex::new(Vec::new())),
            },
            audio,
            notes,
        }
    }

    impl Seat {
        /// The audio frames published so far.
        fn take(&mut self, into: &mut Vec<Frame>) {
            while let Ok(f) = self.audio.try_recv() {
                into.push(f);
            }
        }

        /// The vocoder notes under `p25`.
        fn vocoder_notes(&self) -> Vec<String> {
            self.notes
                .lock()
                .unwrap()
                .iter()
                .filter_map(|n| n["p25"]["vocoder"].as_str().map(String::from))
                .collect()
        }
    }

    /// Poll `f` every 10 ms for up to `limit` until it holds.
    async fn until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
        let end = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < end {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        f()
    }

    #[tokio::test]
    async fn feeds_come_back_as_48k_audio_with_their_spans() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = seat();
        let v = Vocoder::spawn(&s.env, &spec(&fake(dir.path(), ECHO)), prov()).unwrap();
        for n in 0..5 {
            v.feed(span(n), &[0; 96]);
        }
        let mut got = Vec::new();
        assert!(
            until(Duration::from_secs(5), || {
                s.take(&mut got);
                got.len() >= 5
            })
            .await,
            "{} frames",
            got.len()
        );
        assert_eq!(got.len(), 5);
        for (n, f) in got.iter().enumerate() {
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &f.payload
            else {
                panic!("expected audio")
            };
            assert_eq!((sample_rate_hz.0, *channels), (AUDIO_RATE_HZ, 1));
            assert_eq!(samples.len(), 960);
            assert_eq!(f.provenance.span, span(n as u64));
        }
        assert!(v.alive());
        assert!(s.vocoder_notes().is_empty(), "{:?}", s.vocoder_notes());
    }

    #[tokio::test]
    async fn an_exit_mid_call_notes_once_and_is_not_alive() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake(
            dir.path(),
            "for i in 1 2; do dd bs=96 count=1 iflag=fullblock 2>/dev/null >/dev/null; \
             head -c 320 /dev/zero; done; exit 3",
        );
        let s = seat();
        let mut v = Vocoder::spawn(&s.env, &spec(&script), prov()).unwrap();
        for n in 0..5 {
            v.feed(span(n), &[0; 96]);
        }
        assert!(until(Duration::from_secs(5), || !v.alive()).await);
        // The reader is done once it is awaited: nothing can note after it.
        if let Some(h) = v.finish() {
            h.await.unwrap();
        }
        let notes = s.vocoder_notes();
        assert_eq!(notes.len(), 1, "once: {notes:?}");
        assert!(notes[0].starts_with(&format!("{NAME} exited")), "{notes:?}");
    }

    #[tokio::test]
    async fn a_hang_is_killed_by_the_watchdog() {
        let dir = tempfile::tempdir().unwrap();
        // It takes the feed and keeps its stdout open, writing nothing.
        let script = fake(
            dir.path(),
            "dd bs=96 count=1 iflag=fullblock 2>/dev/null >/dev/null; exec sleep 60",
        );
        let s = seat();
        let v = Vocoder::spawn(&s.env, &spec(&script), prov()).unwrap();
        let fed = tokio::time::Instant::now();
        v.feed(span(0), &[0; 96]);
        let limit = Duration::from_millis(VOCODER_WATCHDOG_MS + 1_000);
        assert!(until(limit, || !v.alive()).await, "alive after {limit:?}");
        assert!(fed.elapsed() >= Duration::from_millis(VOCODER_WATCHDOG_MS));
        // The note follows the exit.
        assert!(until(Duration::from_secs(5), || !s.vocoder_notes().is_empty()).await);
        let notes = s.vocoder_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("stopped answering"), "{notes:?}");
        assert!(notes[0].contains(NAME), "{notes:?}");
    }

    #[tokio::test]
    async fn finish_publishes_the_speech_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = seat();
        let mut v = Vocoder::spawn(&s.env, &spec(&fake(dir.path(), ECHO)), prov()).unwrap();
        for n in 0..3 {
            v.feed(span(n), &[0; 96]);
        }
        let h = v.finish().expect("a reader");
        h.await.unwrap();
        let mut got = Vec::new();
        s.take(&mut got);
        let spans: Vec<Span> = got.iter().map(|f| f.provenance.span.clone()).collect();
        assert_eq!(spans, [span(0), span(1), span(2)]);
        assert!(!v.alive());
        assert!(s.vocoder_notes().is_empty(), "{:?}", s.vocoder_notes());
    }

    /// The processes whose command line names `script`.
    fn running(script: &Path) -> usize {
        let want = script.to_str().unwrap().as_bytes();
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
            .filter(|c| c.windows(want.len()).any(|w| w == want))
            .count()
    }

    #[tokio::test]
    async fn twenty_calls_leave_no_child() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake(dir.path(), ECHO);
        let s = seat();
        for n in 0..20 {
            let mut v = Vocoder::spawn(&s.env, &spec(&script), prov()).unwrap();
            v.feed(span(n), &[0; 96]);
            if let Some(h) = v.finish() {
                s.env.drains.lock().unwrap().push(h);
            }
        }
        let drains: Vec<_> = std::mem::take(&mut *s.env.drains.lock().unwrap());
        for h in drains {
            h.await.unwrap();
        }
        assert!(
            until(Duration::from_secs(5), || running(&script) == 0).await,
            "{} left",
            running(&script)
        );
        assert!(s.vocoder_notes().is_empty(), "{:?}", s.vocoder_notes());
    }

    #[tokio::test]
    async fn a_wrong_sized_feed_is_dropped_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = seat();
        let mut v = Vocoder::spawn(&s.env, &spec(&fake(dir.path(), ECHO)), prov()).unwrap();
        // Sent, the 95 bytes would make one feed with the next one's first byte, and a
        // second from the rest at the end of the input: two frames, the first with
        // this span.
        v.feed(span(0), &[0; 95]);
        v.feed(span(1), &[0; 96]);
        v.finish().expect("a reader").await.unwrap();
        let mut got = Vec::new();
        s.take(&mut got);
        let spans: Vec<Span> = got.iter().map(|f| f.provenance.span.clone()).collect();
        assert_eq!(spans, [span(1)]);
    }

    #[test]
    fn a_program_absent_without_a_row_is_an_error_only_when_required() {
        let ctx = StageContext::new(SourceId("s".into()));
        let e = program(&ctx, 0, "zeta-no-such-vocoder", true).unwrap_err();
        assert!(e.contains("handler zeta-no-such-vocoder not found"), "{e}");
        assert_eq!(program(&ctx, 0, "zeta-no-such-vocoder", false), Ok(None));
    }

    /// The M17 row with its handler made optional and named so no host has it.
    fn optional_row(optional: bool) -> Arc<zeta_core::modes::ModeRow> {
        let mut v: serde_json::Value =
            serde_json::from_str(include_str!("../../../modes/m17_decode.json")).unwrap();
        v["decoder"]["handlers"] = serde_json::json!([{
            "programs": ["zeta-no-such-handler"],
            "optional": optional,
            "install": {"url": "https://example.invalid"}
        }]);
        Arc::new(zeta_core::modes::parse(&v.to_string()).unwrap())
    }

    #[test]
    fn an_optional_handler_absent_is_none() {
        let ctx = StageContext::new(SourceId("s".into())).with_mode(optional_row(true));
        assert_eq!(program(&ctx, 0, "x", false), Ok(None));
        // Required, the same handler absent is the row's error.
        let ctx = StageContext::new(SourceId("s".into())).with_mode(optional_row(false));
        let e = program(&ctx, 0, "x", false).unwrap_err();
        assert!(e.contains("handler zeta-no-such-handler not found"), "{e}");
    }
}
