//! The main audio stream of a stage with two voices (design 38, 5.10), as `dmr_decode`'s
//! two time slots are: what the play button, the mix and a plain `job_attach` read, chosen
//! from the voices' own streams by the job's audio policy ([`AudioPolicy`]).
//!
//! Its own module beside the stage: the policy and the stream it writes are the same for
//! any stage with two voices, and the stage's file holds the protocol.
//!
//! **The policy.** The stage tells [`MainStream`] when a call's audio begins (its first frame
//! fed to a vocoder) and ends (its last). A call with no audio (encrypted, no vocoder, a
//! vocoder that failed, speech still held) is never begun, so it never holds the main
//! stream.
//! - `first_active`: a call that begins while no call holds the main stream holds it until
//!   its audio ends; one that begins while another holds it never joins it.
//! - `follow`: as `first_active`, and when the holder ends while another call's audio goes
//!   on, the one that began first of those holds it from that moment.
//! - `all`: every call's audio, mixed.
//!
//! **The stream.** A task reads both voices' streams from the bus, as their vocoders publish
//! them, and writes the main stream in time order: `first_active` and `follow` forward the
//! holder's frames as they are (spans and samples); `all` cuts the main stream on one 20 ms
//! grid ([`MIX_CELL_NS`], absolute time), each cell the sum of the voices' samples there,
//! saturated at full scale, so a lone call's samples pass through re-cut onto the grid. The
//! stage says how far its decisions are final ([`MainStream::known`]) and which frames it
//! fed ([`MainStream::fed`]): a span is written once it is before the first and every frame
//! fed before it has come back, or its vocoder is gone ([`MainStream::gone`]), or
//! [`GIVE_UP`] has passed since it was fed (the vocoder seat's watchdog kills a vocoder
//! that stops answering well before that). A frame that comes back after its span was
//! written is dropped. When the stage drops its [`MainStream`] the task writes what it
//! holds and ends.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use zeta_core::ops::AudioPolicy;
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Span, StreamId, Timestamp};

use crate::vocoder::{AUDIO_RATE_HZ, VOCODER_WATCHDOG_MS};

/// The main stream's grid under `all`, ns: one vocoder frame (20 ms).
pub const MIX_CELL_NS: u64 = 20_000_000;
/// Samples in a cell at [`AUDIO_RATE_HZ`].
const CELL_SAMPLES: usize = (AUDIO_RATE_HZ * MIX_CELL_NS / 1_000_000_000) as usize;
/// How long a frame fed to a vocoder is waited for before the main stream goes on without
/// it: the vocoder seat's watchdog and a second more.
pub const GIVE_UP: Duration = Duration::from_millis(VOCODER_WATCHDOG_MS + 1000);
/// The voices a [`MainStream`] chooses from.
pub const VOICES: usize = 2;

/// What the stage tells the task.
#[derive(Debug)]
enum Command {
    /// Call `id`'s frames on `voice` are on the main stream from `from`.
    On { id: u64, voice: usize, from: u64 },
    /// Call `id`'s are not from `at`.
    Off { id: u64, at: u64 },
    /// A frame with `span` was fed to `voice`'s vocoder.
    Fed { voice: usize, span: Span },
    /// The frames fed to `voice` before `before` that have not come back never will.
    Gone { voice: usize, before: u64 },
    /// The stage's decisions are final before `until`: no frame will be fed, and no call
    /// begin or end, before it.
    Known { until: u64 },
}

/// The audio policy over the calls, and the handle of the task that writes the main
/// stream. Dropping it ends the task once it has written what it holds.
pub struct MainStream {
    policy: AudioPolicy,
    tx: mpsc::UnboundedSender<Command>,
    /// The calls whose audio has begun and not ended: id, voice, in the order they began.
    live: Vec<(u64, usize)>,
    /// The call holding the main stream (`first_active`, `follow`).
    holder: Option<u64>,
    /// The live calls that have been on the main stream.
    been: Vec<u64>,
    next: u64,
    known: u64,
}

impl MainStream {
    /// The main stream on `out`, from the two voices' streams `voices` on `bus`, under
    /// `policy`; and the task writing it. `None` when a voice's stream is not registered.
    pub fn spawn(
        policy: AudioPolicy,
        bus: &Arc<StreamBus>,
        out: StreamId,
        voices: [&StreamId; VOICES],
    ) -> Option<(Self, JoinHandle<()>)> {
        let rx = [bus.subscribe(voices[0])?, bus.subscribe(voices[1])?];
        let (tx, cmd) = mpsc::unbounded_channel();
        let writer = Writer {
            mix: policy == AudioPolicy::All,
            bus: bus.clone(),
            out,
            buffer: Default::default(),
            pending: Default::default(),
            segments: Vec::new(),
            known: 0,
            cursor: 0,
        };
        let task = tokio::spawn(writer.run(cmd, rx));
        let main = MainStream {
            policy,
            tx,
            live: Vec::new(),
            holder: None,
            been: Vec::new(),
            next: 0,
            known: 0,
        };
        Some((main, task))
    }

    fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    /// A call's audio begins on `voice` with a frame starting at `at`: its id.
    pub fn begin(&mut self, voice: usize, at: u64) -> u64 {
        let id = self.next;
        self.next += 1;
        self.live.push((id, voice));
        if self.policy == AudioPolicy::All || self.holder.is_none() {
            self.take(id, voice, at);
        }
        id
    }

    fn take(&mut self, id: u64, voice: usize, from: u64) {
        if self.policy != AudioPolicy::All {
            self.holder = Some(id);
        }
        self.been.push(id);
        self.send(Command::On { id, voice, from });
    }

    /// Whether call `id`'s audio is on the main stream now.
    pub fn on_main(&self, id: u64) -> bool {
        match self.policy {
            AudioPolicy::All => self.live.iter().any(|(l, _)| *l == id),
            _ => self.holder == Some(id),
        }
    }

    /// Call `id`'s audio ends with a frame ending at `at`: whether it was ever on the main
    /// stream. Under `follow` the call that began first of those still going holds it from
    /// `at`.
    pub fn end(&mut self, id: u64, at: u64) -> bool {
        let Some(k) = self.live.iter().position(|(l, _)| *l == id) else {
            return false;
        };
        self.live.remove(k);
        let been = self.been.contains(&id);
        self.been.retain(|b| *b != id);
        if been {
            self.send(Command::Off { id, at });
        }
        if self.holder == Some(id) {
            self.holder = None;
            if self.policy == AudioPolicy::Follow
                && let Some(&(next, voice)) = self.live.first()
            {
                self.take(next, voice, at);
            }
        }
        been
    }

    /// A frame with `span` is about to be fed to `voice`'s vocoder.
    pub fn fed(&self, voice: usize, span: Span) {
        self.send(Command::Fed { voice, span });
    }

    /// `voice`'s vocoder is gone: the frames fed to it before `before` that have not come
    /// back never will.
    pub fn gone(&self, voice: usize, before: u64) {
        self.send(Command::Gone { voice, before });
    }

    /// The stage's decisions are final before `until` (it only grows).
    pub fn known(&mut self, until: u64) {
        if until > self.known {
            self.known = until;
            self.send(Command::Known { until });
        }
    }
}

/// A frame of a voice waiting to be written, by its span's start.
struct Waiting {
    start: u64,
    frame: Frame,
}

/// The span during which a call's frames are on the main stream.
struct Segment {
    id: u64,
    voice: usize,
    from: u64,
    to: Option<u64>,
}

/// The task writing the main stream.
struct Writer {
    mix: bool,
    bus: Arc<StreamBus>,
    out: StreamId,
    /// Each voice's frames come back, by start.
    buffer: [VecDeque<Waiting>; VOICES],
    /// Each voice's frames fed and not yet back, with when to stop waiting for them.
    pending: [Vec<(Span, Instant)>; VOICES],
    segments: Vec<Segment>,
    known: u64,
    /// Written up to here: the last frame's start (forwarding), the next cell (mixing).
    cursor: u64,
}

impl Writer {
    async fn run(
        mut self,
        mut cmd: mpsc::UnboundedReceiver<Command>,
        mut rx: [broadcast::Receiver<Frame>; VOICES],
    ) {
        let mut open = [true; VOICES];
        loop {
            let deadline = self.pending.iter().flatten().map(|(_, d)| *d).min();
            let [a, b] = &mut rx;
            tokio::select! {
                c = cmd.recv() => match c {
                    Some(c) => self.command(c, &mut rx),
                    None => break,
                },
                r = a.recv(), if open[0] => self.receive(0, r, &mut open),
                r = b.recv(), if open[1] => self.receive(1, r, &mut open),
                _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)),
                    if deadline.is_some() => self.give_up(Instant::now()),
            }
            self.write(self.bound());
        }
        // The stage is done, and its vocoders with it: what they published is all in.
        for (v, r) in rx.iter_mut().enumerate() {
            self.drain(v, r);
        }
        self.pending.iter_mut().for_each(Vec::clear);
        self.write(u64::MAX);
    }

    fn command(&mut self, c: Command, rx: &mut [broadcast::Receiver<Frame>; VOICES]) {
        match c {
            Command::On { id, voice, from } => self.segments.push(Segment {
                id,
                voice,
                from,
                to: None,
            }),
            Command::Off { id, at } => {
                if let Some(s) = self.segments.iter_mut().find(|s| s.id == id) {
                    s.to = Some(at);
                }
            }
            Command::Fed { voice, span } => {
                let back = self.buffer[voice]
                    .iter()
                    .any(|w| w.frame.provenance.span == span);
                if !back && span.start_ns.0 >= self.cursor {
                    self.pending[voice].push((span, Instant::now() + GIVE_UP));
                }
            }
            Command::Gone { voice, before } => {
                // All it published is on the bus by now.
                self.drain(voice, &mut rx[voice]);
                self.pending[voice].retain(|(s, _)| s.start_ns.0 >= before);
            }
            Command::Known { until } => self.known = self.known.max(until),
        }
    }

    fn receive(
        &mut self,
        voice: usize,
        r: Result<Frame, broadcast::error::RecvError>,
        open: &mut [bool; VOICES],
    ) {
        match r {
            Ok(f) => self.arrive(voice, f),
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => open[voice] = false,
        }
    }

    /// Every frame `voice`'s stream holds now.
    fn drain(&mut self, voice: usize, rx: &mut broadcast::Receiver<Frame>) {
        loop {
            match rx.try_recv() {
                Ok(f) => self.arrive(voice, f),
                Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    }

    /// A frame of `voice` came back.
    fn arrive(&mut self, voice: usize, frame: Frame) {
        let span = frame.provenance.span.clone();
        if let Some(k) = self.pending[voice].iter().position(|(s, _)| *s == span) {
            self.pending[voice].remove(k);
        }
        let start = span.start_ns.0;
        if start < self.cursor {
            return;
        }
        let b = &mut self.buffer[voice];
        let at = b.partition_point(|w| w.start <= start);
        b.insert(at, Waiting { start, frame });
    }

    fn give_up(&mut self, now: Instant) {
        for p in &mut self.pending {
            p.retain(|(_, d)| *d > now);
        }
    }

    /// How far the main stream can be written: before what the stage has made final, and
    /// before every frame still to come back.
    fn bound(&self) -> u64 {
        self.pending
            .iter()
            .flatten()
            .map(|(s, _)| s.start_ns.0)
            .fold(self.known, u64::min)
    }

    /// Whether `voice`'s frame starting at `t` is on the main stream.
    fn on(&self, voice: usize, t: u64) -> bool {
        self.segments
            .iter()
            .any(|s| s.voice == voice && s.from <= t && s.to.is_none_or(|e| t < e))
    }

    fn write(&mut self, bound: u64) {
        if self.mix {
            self.write_mixed(bound);
        } else {
            self.write_forwarded(bound);
        }
        let cursor = self.cursor;
        self.segments.retain(|s| s.to.is_none_or(|e| e > cursor));
    }

    /// The holder's frames before `bound`, as they are, in time order.
    fn write_forwarded(&mut self, bound: u64) {
        loop {
            let next = (0..VOICES)
                .filter_map(|v| self.buffer[v].front().map(|w| (w.start, v)))
                .min();
            let Some((start, v)) = next.filter(|(s, _)| *s < bound) else {
                return;
            };
            let w = self.buffer[v].pop_front().expect("a front");
            if self.on(v, start) {
                self.cursor = start;
                let f = w.frame;
                self.bus
                    .publish(Frame::new(self.out.clone(), f.provenance, f.payload));
            }
        }
    }

    /// The cells that end before `bound`, each the sum of the voices' frames on the main
    /// stream there.
    fn write_mixed(&mut self, bound: u64) {
        loop {
            let first = (0..VOICES)
                .filter_map(|v| self.buffer[v].front().map(|w| w.start))
                .min();
            let Some(first) = first else {
                return;
            };
            let cell = self.cursor.max(first / MIX_CELL_NS * MIX_CELL_NS);
            if cell.saturating_add(MIX_CELL_NS) > bound {
                return;
            }
            self.write_cell(cell);
            self.cursor = cell + MIX_CELL_NS;
            let cursor = self.cursor;
            for b in &mut self.buffer {
                while b
                    .front()
                    .is_some_and(|w| w.frame.provenance.span.end_ns.0 <= cursor)
                {
                    b.pop_front();
                }
            }
        }
    }

    /// The cell starting at `cell`: published when a frame on the main stream reaches it.
    fn write_cell(&mut self, cell: u64) {
        let base = sample_index(cell);
        let end = cell + MIX_CELL_NS;
        let mut sum = vec![0f32; CELL_SAMPLES];
        let mut prov = None;
        for v in 0..VOICES {
            let mut voice = vec![0f32; CELL_SAMPLES];
            let mut any = false;
            for w in &self.buffer[v] {
                let span = &w.frame.provenance.span;
                if span.end_ns.0 <= cell || w.start >= end || !self.on(v, w.start) {
                    continue;
                }
                let Payload::Audio { samples, .. } = &w.frame.payload else {
                    continue;
                };
                let first = sample_index(w.start);
                for (i, s) in samples.iter().enumerate() {
                    let at = first + i as u128;
                    if at >= base && at < base + CELL_SAMPLES as u128 {
                        voice[(at - base) as usize] = *s;
                    }
                }
                any = true;
                prov.get_or_insert_with(|| w.frame.provenance.clone());
            }
            if any {
                sum.iter_mut().zip(&voice).for_each(|(s, v)| *s += v);
            }
        }
        let Some(mut p) = prov else {
            return;
        };
        p.span = Span {
            start_ns: Timestamp(cell),
            end_ns: Timestamp(end),
        };
        sum.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
        self.bus.publish(Frame::new(
            self.out.clone(),
            p,
            Payload::Audio {
                sample_rate_hz: Hz(AUDIO_RATE_HZ),
                channels: 1,
                samples: sum.into(),
            },
        ));
    }
}

/// The sample at [`AUDIO_RATE_HZ`] nearest time `t_ns`, counted from time 0.
pub fn sample_index(t_ns: u64) -> u128 {
    (u128::from(t_ns) * u128::from(AUDIO_RATE_HZ) + 500_000_000) / 1_000_000_000
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use zeta_core::types::{Provenance, SourceId, StreamKind};

    use super::*;

    const T0: u64 = 1_000_000_000;
    const FRAME_NS: u64 = 20_000_000;

    fn prov(span: Span) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span,
        }
    }

    fn span(start: u64) -> Span {
        Span {
            start_ns: Timestamp(start),
            end_ns: Timestamp(start + FRAME_NS),
        }
    }

    /// A 20 ms frame at `start` on `stream`, every sample `level`.
    fn frame(stream: &StreamId, start: u64, level: f32) -> Frame {
        Frame::new(
            stream.clone(),
            prov(span(start)),
            Payload::Audio {
                sample_rate_hz: Hz(AUDIO_RATE_HZ),
                channels: 1,
                samples: vec![level; CELL_SAMPLES].into(),
            },
        )
    }

    struct Rig {
        bus: Arc<StreamBus>,
        voices: [StreamId; VOICES],
        main: Option<MainStream>,
        task: Option<JoinHandle<()>>,
        out: broadcast::Receiver<Frame>,
        got: Vec<Frame>,
    }

    impl Rig {
        fn new(policy: AudioPolicy) -> Self {
            let bus = Arc::new(StreamBus::new(1024));
            let p = prov(span(0));
            let voices = [
                bus.register(StreamKind::Audio, p.clone()),
                bus.register(StreamKind::Audio, p.clone()),
            ];
            let main_out = bus.register(StreamKind::Audio, p);
            let out = bus.subscribe(&main_out).unwrap();
            let (main, task) =
                MainStream::spawn(policy, &bus, main_out, [&voices[0], &voices[1]]).unwrap();
            Rig {
                bus,
                voices,
                main: Some(main),
                task: Some(task),
                out,
                got: Vec::new(),
            }
        }

        fn main(&mut self) -> &mut MainStream {
            self.main.as_mut().unwrap()
        }

        /// `n` frames from `from` on `voice`, fed and published, every sample `level`.
        fn speak(&mut self, voice: usize, from: u64, n: u64, level: f32) {
            for k in 0..n {
                let start = from + k * FRAME_NS;
                self.main().fed(voice, span(start));
                self.bus.publish(frame(&self.voices[voice], start, level));
            }
        }

        fn collect(&mut self) {
            while let Ok(f) = self.out.try_recv() {
                self.got.push(f);
            }
        }

        async fn until(&mut self, f: impl Fn(&[Frame]) -> bool) -> bool {
            for _ in 0..500 {
                self.collect();
                if f(&self.got) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }

        async fn end(&mut self) {
            self.main = None;
            self.task.take().unwrap().await.unwrap();
            self.collect();
        }
    }

    fn starts(frames: &[Frame]) -> Vec<u64> {
        frames
            .iter()
            .map(|f| f.provenance.span.start_ns.0)
            .collect()
    }

    fn samples(f: &Frame) -> Vec<f32> {
        let Payload::Audio { samples, .. } = &f.payload else {
            panic!("audio")
        };
        samples.to_vec()
    }

    #[tokio::test]
    async fn the_policies_pick_the_holder() {
        // Call a on voice 0, b on voice 1 begun after it; a ends first.
        for (policy, b_on, b_after) in [
            (AudioPolicy::FirstActive, false, false),
            (AudioPolicy::Follow, false, true),
            (AudioPolicy::All, true, true),
        ] {
            let mut r = Rig::new(policy);
            let a = r.main().begin(0, T0);
            let b = r.main().begin(1, T0 + 100_000_000);
            assert!(r.main().on_main(a), "{policy:?}");
            assert_eq!(r.main().on_main(b), b_on, "{policy:?}");
            assert!(r.main().end(a, T0 + 200_000_000), "{policy:?}");
            assert!(!r.main().on_main(a), "{policy:?}");
            assert_eq!(r.main().on_main(b), b_after, "{policy:?}");
            assert_eq!(r.main().end(b, T0 + 300_000_000), b_after, "{policy:?}");
            // A call that begins with none holding takes it.
            let c = r.main().begin(1, T0 + 400_000_000);
            assert!(r.main().on_main(c), "{policy:?}");
            r.end().await;
        }
    }

    #[tokio::test]
    async fn follow_forwards_the_holder_then_the_next_in_time_order() {
        let mut r = Rig::new(AudioPolicy::Follow);
        let a = r.main().begin(0, T0);
        r.speak(0, T0, 5, 0.1);
        let b = r.main().begin(1, T0 + 30_000_000);
        r.speak(1, T0 + 30_000_000, 8, 0.2);
        let a_end = T0 + 5 * FRAME_NS;
        r.main().end(a, a_end);
        r.main().end(b, T0 + 30_000_000 + 8 * FRAME_NS);
        // Nothing is written before the stage says its decisions are final.
        tokio::time::sleep(Duration::from_millis(50)).await;
        r.collect();
        assert!(r.got.is_empty(), "{:?}", starts(&r.got));
        r.main().known(T0 + 1_000_000_000);
        assert!(r.until(|g| g.len() >= 9).await, "{:?}", starts(&r.got));
        r.end().await;
        let s = starts(&r.got);
        // a's five frames, then b's four from where a ended, never backwards.
        let a_frames: Vec<u64> = (0..5).map(|k| T0 + k * FRAME_NS).collect();
        assert_eq!(s[..5], a_frames);
        assert_eq!(s.len(), 9, "{s:?}");
        assert!(s[5..].iter().all(|&t| t >= a_end), "{s:?}");
        assert!(s.windows(2).all(|w| w[1] > w[0]), "{s:?}");
        assert!(r.got[5..].iter().all(|f| samples(f) == [0.2; CELL_SAMPLES]));
    }

    #[tokio::test]
    async fn all_sums_the_voices_on_one_grid() {
        let mut r = Rig::new(AudioPolicy::All);
        let a = r.main().begin(0, T0);
        let b = r.main().begin(1, T0 + 10_000_000);
        r.speak(0, T0, 4, 0.75);
        r.speak(1, T0 + 10_000_000, 2, 0.5);
        r.main().end(a, T0 + 4 * FRAME_NS);
        r.main().end(b, T0 + 50_000_000);
        r.main().known(T0 + 1_000_000_000);
        assert!(r.until(|g| g.len() >= 4).await, "{:?}", starts(&r.got));
        r.end().await;
        let cells: Vec<u64> = (0..4).map(|k| T0 + k * FRAME_NS).collect();
        assert_eq!(starts(&r.got), cells);
        let l: Vec<Vec<f32>> = r.got.iter().map(samples).collect();
        // b covers 10 to 50 ms: half the first cell, the second, half the third; the sum
        // saturates at full scale.
        let half = CELL_SAMPLES / 2;
        assert!(l[0][..half].iter().all(|&s| s == 0.75));
        assert!(l[0][half..].iter().all(|&s| s == 1.0));
        assert!(l[1].iter().all(|&s| s == 1.0));
        assert!(l[2][..half].iter().all(|&s| s == 1.0));
        assert!(l[2][half..].iter().all(|&s| s == 0.75));
        assert!(l[3].iter().all(|&s| s == 0.75));
    }

    #[tokio::test]
    async fn a_frame_fed_is_waited_for_until_its_vocoder_is_gone() {
        let mut r = Rig::new(AudioPolicy::FirstActive);
        r.main().begin(0, T0);
        r.speak(0, T0, 2, 0.1);
        // Fed, never published: the main stream waits for it.
        r.main().fed(0, span(T0 + 2 * FRAME_NS));
        r.speak(0, T0 + 3 * FRAME_NS, 1, 0.1);
        r.main().known(T0 + 1_000_000_000);
        assert!(r.until(|g| g.len() >= 2).await);
        tokio::time::sleep(Duration::from_millis(50)).await;
        r.collect();
        assert_eq!(r.got.len(), 2, "{:?}", starts(&r.got));
        r.main().gone(0, T0 + 3 * FRAME_NS);
        assert!(r.until(|g| g.len() >= 3).await, "{:?}", starts(&r.got));
        assert_eq!(starts(&r.got)[2], T0 + 3 * FRAME_NS);
        r.end().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_frame_that_never_comes_is_given_up() {
        let mut r = Rig::new(AudioPolicy::FirstActive);
        r.main().begin(0, T0);
        r.main().fed(0, span(T0));
        r.speak(0, T0 + FRAME_NS, 1, 0.1);
        r.main().known(T0 + 1_000_000_000);
        tokio::time::sleep(GIVE_UP - Duration::from_millis(100)).await;
        r.collect();
        assert!(r.got.is_empty(), "{:?}", starts(&r.got));
        tokio::time::sleep(Duration::from_millis(200)).await;
        r.collect();
        assert_eq!(starts(&r.got), [T0 + FRAME_NS]);
        r.end().await;
    }

    #[tokio::test]
    async fn the_end_writes_what_is_held() {
        let mut r = Rig::new(AudioPolicy::FirstActive);
        r.main().begin(0, T0);
        r.speak(0, T0, 3, 0.1);
        r.end().await;
        assert_eq!(starts(&r.got), [T0, T0 + FRAME_NS, T0 + 2 * FRAME_NS]);
    }
}
