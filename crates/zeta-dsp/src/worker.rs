//! Running a heavy stage's DSP on its own thread, behind a drop-on-full queue, so a
//! slow block holds neither a runtime worker nor its input's broadcast buffer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use zeta_core::stage::StageError;
use zeta_core::stream::{Frame, StreamBus};

use crate::{Input, note_lag};

/// Inputs a video stage's queue holds before it drops the newest.
pub const VIDEO_QUEUE_DEPTH: usize = 8;

/// [`crate::run_map`] with `f` on a thread of its own (named `zeta-<kind>`): the async
/// side receives frames from `inputs[0]`, counts broadcast lag through [`note_lag`], and
/// queues each input for the thread in a queue of `depth`. A full queue drops the input,
/// counts it in `dropped` and passes the next input that fits as [`Input::Lagged`] with
/// the inputs missed (broadcast lag and drops together) before it. The thread publishes
/// every frame `f` returns. Cancellation stops the thread before its next input and
/// returns `Cancelled`; a closed input lets the thread finish what is queued and returns
/// `Ok(())`; a thread that panics fails the stage.
pub async fn run_map_threaded<F>(
    kind: &'static str,
    mut inputs: Vec<broadcast::Receiver<Frame>>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
    depth: usize,
    dropped: Arc<AtomicU64>,
    mut f: F,
) -> Result<(), StageError>
where
    F: FnMut(Input) -> Vec<Frame> + Send + 'static,
{
    if inputs.len() != 1 {
        return Err(StageError::Failed("expected 1 input".into()));
    }
    let mut rx = inputs.remove(0);
    let (tx, queue) = sync_channel::<Input>(depth.max(1));
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name(format!("zeta-{kind}"))
        .spawn(move || {
            // Said when the thread ends, however it ends.
            let _done = DoneGuard(Some(done_tx));
            while let Ok(input) = queue.recv() {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                for out in f(input) {
                    bus.publish(out);
                }
            }
        })
        .map_err(|e| StageError::Failed(format!("{kind}: spawning its thread: {e}")))?;
    let mut missed = 0u64;
    let ended = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Err(StageError::Cancelled),
            r = rx.recv() => match r {
                Ok(frame) => {
                    if !offer(&tx, &mut missed, &dropped, frame) {
                        break Ok(());
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    note_lag(kind, n);
                    missed += n;
                }
                Err(broadcast::error::RecvError::Closed) => break Ok(()),
            },
        }
    };
    if ended.is_err() {
        stop.store(true, Ordering::Relaxed);
    }
    drop(tx);
    let _ = done_rx.await;
    match thread.join() {
        Ok(()) => ended,
        Err(_) => Err(StageError::Failed(format!("{kind}: its thread panicked"))),
    }
}

/// Sends `()` on drop, so the async side learns the thread ended without blocking on
/// `join`.
struct DoneGuard(Option<oneshot::Sender<()>>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

/// Queue `frame` for the thread, preceded by [`Input::Lagged`] when `missed` inputs were
/// lost since the last one queued. A full queue drops the frame (and the marker), counts
/// it in `dropped` and in `missed`. False when the thread has gone.
fn offer(tx: &SyncSender<Input>, missed: &mut u64, dropped: &AtomicU64, frame: Frame) -> bool {
    if *missed > 0 {
        match tx.try_send(Input::Lagged { missed: *missed }) {
            Ok(()) => *missed = 0,
            Err(TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
                *missed += 1;
                return true;
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    match tx.try_send(Input::Frame(frame)) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            *missed += 1;
            true
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use zeta_core::stream::Payload;
    use zeta_core::types::{Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

    use super::*;

    fn prov() -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        }
    }

    fn event(i: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Event(serde_json::json!(i)),
        )
    }

    #[tokio::test]
    async fn a_full_queue_drops_and_marks_the_next_input_lagged() {
        let (tx, rx) = broadcast::channel(64);
        let cancel = CancellationToken::new();
        let dropped = Arc::new(AtomicU64::new(0));
        let lagged = Arc::new(AtomicU64::new(0));
        let seen = lagged.clone();
        let task = tokio::spawn(run_map_threaded(
            "worker_test",
            vec![rx],
            Arc::new(StreamBus::new(4)),
            cancel.clone(),
            2,
            dropped.clone(),
            move |input| {
                match input {
                    Input::Lagged { .. } => {
                        seen.fetch_add(1, Ordering::Relaxed);
                    }
                    Input::Frame(_) => std::thread::sleep(Duration::from_millis(50)),
                }
                Vec::new()
            },
        ));
        for i in 0..40 {
            tx.send(event(i)).unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for _ in 0..200 {
            if lagged.load(Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(lagged.load(Ordering::Relaxed) > 0, "no Lagged input seen");
        assert!(dropped.load(Ordering::Relaxed) > 0, "nothing dropped");
        cancel.cancel();
        let r = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("stage ended within 1 s")
            .unwrap();
        assert!(matches!(r, Err(StageError::Cancelled)), "{r:?}");
    }

    #[tokio::test]
    async fn the_thread_publishes_what_the_closure_returns() {
        let bus = Arc::new(StreamBus::new(16));
        let out = bus.register(StreamKind::Event, prov());
        let mut sub = bus.subscribe(&out).unwrap();
        let (tx, rx) = broadcast::channel(16);
        let names = Arc::new(Mutex::new(Vec::new()));
        let thread_names = names.clone();
        let out_id = out.clone();
        let task = tokio::spawn(run_map_threaded(
            "worker_test",
            vec![rx],
            bus.clone(),
            CancellationToken::new(),
            4,
            Arc::new(AtomicU64::new(0)),
            move |input| {
                thread_names
                    .lock()
                    .unwrap()
                    .push(std::thread::current().name().map(str::to_owned));
                let Input::Frame(f) = input else {
                    return Vec::new();
                };
                let Payload::Event(v) = f.payload else {
                    return Vec::new();
                };
                let n = v.as_u64().unwrap();
                vec![Frame::new(
                    out_id.clone(),
                    prov(),
                    Payload::Event(serde_json::json!(n * 10)),
                )]
            },
        ));
        for i in 0..3 {
            tx.send(event(i)).unwrap();
        }
        drop(tx);
        let r = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("stage ended")
            .unwrap();
        assert!(r.is_ok(), "{r:?}");
        let mut got = Vec::new();
        while let Ok(f) = sub.try_recv() {
            if let Payload::Event(v) = f.payload {
                got.push(v.as_u64().unwrap());
            }
        }
        assert_eq!(got, vec![0, 10, 20]);
        let names = names.lock().unwrap();
        assert!(
            names
                .iter()
                .all(|n| n.as_deref() == Some("zeta-worker_test")),
            "{names:?}"
        );
    }
}
