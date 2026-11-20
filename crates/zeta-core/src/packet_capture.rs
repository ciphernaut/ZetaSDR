//! The `packet_capture` stage: the sink of a `packets` job, appending each packet its
//! input carries to a pcapng capture in the node's [`PacketStore`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::modes::Access;
use crate::packets::{NewPacketCapture, PacketStore, PacketWriter};
use crate::stage::{Backend, Stage, StageContext, StageError, StageNote, StageSpec};
use crate::stream::{Frame, LinkType, Payload, StreamBus};
use crate::types::{Hz, JobId, StreamId, StreamKind};

/// The stage kind, as graphs name it.
pub const PACKET_CAPTURE_KIND: &str = "packet_capture";
/// Packets queued for the writer thread before the stage waits for it.
const QUEUE: usize = 256;
/// How often the job's context is noted at most between a file's first packet and its
/// roll.
const NOTE_EVERY: Duration = Duration::from_secs(1);

/// `packet_capture` parameters: what its captures record of the decoder job they hold.
/// The node fills all four from the input job each time it launches the attached job.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PacketCaptureParams {
    /// The decoder job captured (the attached job's `input`).
    pub job: JobId,
    /// The decoder job's centre, Hz.
    pub center_hz: u64,
    /// The decoder job's mode row id; `null` for a job of no row.
    #[serde(deserialize_with = "crate::modes::nullable")]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub mode: Option<String>,
    /// The row's access; `null` for none.
    #[serde(deserialize_with = "crate::modes::nullable")]
    #[schemars(required, schema_with = "crate::modes::nullable_enum::<Access>")]
    pub access: Option<Access>,
}

/// Writes each packet of its one input to a capture in [`StageContext::packets`],
/// timestamped with the frame's `provenance.span.start_ns`. Its file is begun at the
/// first packet, with that packet's link type; a packet of another link type closes it
/// and begins a fresh one, as the store's size cap does (a roll). The job's context is
/// noted `{"capture": <id>, "packets": <n>, "bytes": <n>}` (the file being written) at a
/// file's first packet, at each roll, at most once a second between, and once more at
/// the end. The input closing ends the stage `Ok(())`, its capture marked no longer
/// being written; a cancel (the job stopped, or its `stream_in` ended first) still writes
/// every packet its input holds before it ends. A file that cannot be written ends it `Failed` naming the path and
/// the error; nothing upstream is touched. Without a store (no node) it fails at once.
///
/// The files are written on a blocking thread fed by a queue of [`QUEUE`] packets, so a
/// slow disk holds up neither the runtime nor the decoder: once the queue is full the
/// stage waits for it, and its input lags (logged and skipped) as any slow reader's.
pub struct PacketCaptureStage {
    spec: StageSpec,
}

impl PacketCaptureStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: PACKET_CAPTURE_KIND,
                inputs: vec![StreamKind::Packet],
                outputs: vec![],
                params_schema: schemars::schema_for!(PacketCaptureParams),
            },
        }
    }
}

impl Default for PacketCaptureStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for PacketCaptureStage {
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
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: PacketCaptureParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("packet_capture params: {e}")))?;
        let Some(store) = ctx.packets.clone() else {
            return Err(StageError::Failed(
                "packet_capture needs a node's state directory".into(),
            ));
        };
        let Some(mut rx) = inputs.into_iter().next() else {
            return Err(StageError::Failed("packet_capture needs one input".into()));
        };
        let base = NewPacketCapture {
            job: p.job,
            source: ctx.source.clone(),
            center_hz: Hz(p.center_hz),
            // Each file's own: its first packet's.
            link_type: LinkType::User0,
            mode: p.mode,
            access: p.access,
        };
        let (tx, items) = mpsc::channel::<Item>(QUEUE);
        let note = ctx.note.clone();
        let mut writer = tokio::task::spawn_blocking(move || write_all(&store, base, items, note));
        // A packet received and not yet queued when the stage was cancelled.
        let mut held = None;
        let end = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(StageError::Cancelled),
                // The writer ends on its own only on an error.
                r = &mut writer => return joined(r),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        let Some(item) = Item::of(frame) else {
                            continue;
                        };
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => {
                                held = Some(item);
                                break Err(StageError::Cancelled);
                            }
                            permit = tx.reserve() => match permit {
                                Ok(permit) => permit.send(item),
                                Err(_) => return joined(writer.await),
                            },
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("zeta: packet_capture lagged, {n} packets skipped");
                    }
                    Err(broadcast::error::RecvError::Closed) => break Ok(()),
                },
            }
        };
        // Cancelled (the supervisor stops the job once its `stream_in` ends, which may be
        // before this stage has read the input's last packets): what the input still
        // holds is queued too, so a stop drops nothing already decoded.
        if end.is_err() {
            let mut rest = held.into_iter().chain(std::iter::from_fn(|| {
                loop {
                    match rx.try_recv() {
                        Ok(frame) => {
                            if let Some(item) = Item::of(frame) {
                                return Some(item);
                            }
                        }
                        Err(broadcast::error::TryRecvError::Lagged(n)) => {
                            eprintln!("zeta: packet_capture lagged, {n} packets skipped");
                        }
                        Err(_) => return None,
                    }
                }
            }));
            for item in &mut rest {
                if tx.send(item).await.is_err() {
                    return joined(writer.await);
                }
            }
        }
        // The writer drains what is queued, closes its file and ends.
        drop(tx);
        joined(writer.await).and(end)
    }
}

/// One packet for the writer thread.
struct Item {
    t_ns: u64,
    link_type: LinkType,
    data: Arc<[u8]>,
}

impl Item {
    /// The packet `frame` carries, at its start; `None` for any other payload.
    fn of(frame: Frame) -> Option<Self> {
        let Payload::Packet { link_type, data } = frame.payload else {
            return None;
        };
        Some(Self {
            t_ns: frame.provenance.span.start_ns.0,
            link_type,
            data,
        })
    }
}

/// The writer thread's end as the stage's.
fn joined(r: Result<Result<(), String>, tokio::task::JoinError>) -> Result<(), StageError> {
    match r {
        Ok(r) => r.map_err(StageError::Failed),
        Err(e) => Err(StageError::Failed(format!("packet_capture: writer: {e}"))),
    }
}

/// Write every packet `items` brings until it closes (see [`PacketCaptureStage`]); the
/// first file error is the end, naming the path.
fn write_all(
    store: &Arc<PacketStore>,
    base: NewPacketCapture,
    mut items: mpsc::Receiver<Item>,
    note: Option<StageNote>,
) -> Result<(), String> {
    let mut writer: Option<PacketWriter> = None;
    let mut link = base.link_type;
    let mut noted: Option<Instant> = None;
    // A packet was written since the last note.
    let mut unnoted = false;
    while let Some(item) = items.blocking_recv() {
        let mut fresh = false;
        if writer.is_none() || link != item.link_type {
            // The old file is closed (its record no longer being written) first.
            drop(writer.take());
            let new = NewPacketCapture {
                link_type: item.link_type,
                ..base.clone()
            };
            let w = store
                .begin(new)
                .map_err(|e| format!("packet_capture: {}: {e}", store.dir().display()))?;
            link = item.link_type;
            fresh = true;
            writer = Some(w);
        }
        let w = writer.as_mut().expect("begun above");
        let rolled = w
            .write(item.t_ns, &item.data)
            .map_err(|e| format!("packet_capture: {}: {e}", file_of(store, w)))?;
        unnoted = true;
        if fresh || rolled.is_some() || noted.is_none_or(|t| t.elapsed() >= NOTE_EVERY) {
            note_capture(store, w, note.as_ref());
            noted = Some(Instant::now());
            unnoted = false;
        }
    }
    if unnoted && let Some(w) = &writer {
        note_capture(store, w, note.as_ref());
    }
    Ok(())
}

/// The path of the file `w` writes, else the store's directory.
fn file_of(store: &PacketStore, w: &PacketWriter) -> String {
    store
        .get(w.id())
        .map_or_else(|| store.dir().to_path_buf(), |r| r.path)
        .display()
        .to_string()
}

/// Note the capture `w` writes, with its size so far, in the job's context.
fn note_capture(store: &PacketStore, w: &PacketWriter, note: Option<&StageNote>) {
    let (Some(note), Some(r)) = (note, store.get(w.id())) else {
        return;
    };
    note(serde_json::json!({"capture": r.id, "packets": r.packets, "bytes": r.bytes}));
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::bursts::BurstPolicy;
    use crate::packets::{PACKET_CAPTURE_MAX_BYTES, PACKETS_DIR, PacketCaptureRecord, PacketStore};
    use crate::pcapng;
    use crate::stream::{LinkType, Payload};
    use crate::types::{Provenance, SourceId, Span, Timestamp};

    const T0: u64 = 1_700_000_000_000_000_000;

    fn prov(t: u64) -> Provenance {
        Provenance {
            source: SourceId("radio".into()),
            job: Some(JobId("ATTACHED".into())),
            channel: None,
            span: Span {
                start_ns: Timestamp(t),
                end_ns: Timestamp(t + 10),
            },
        }
    }

    fn params() -> serde_json::Value {
        serde_json::json!({
            "job": "DECODER", "center_hz": 144_800_000u64, "mode": "aprs_decode", "access": null
        })
    }

    fn packet(link_type: LinkType, data: &[u8]) -> Payload {
        Payload::Packet {
            link_type,
            data: Arc::from(data.to_vec()),
        }
    }

    /// The bus with the stage's input stream, and the store in `dir` with `cap`.
    struct Rig {
        bus: Arc<StreamBus>,
        input: StreamId,
        store: Arc<PacketStore>,
        notes: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl Rig {
        fn new(dir: &Path, cap: u64) -> Self {
            let bus = Arc::new(StreamBus::new(64));
            let input = bus.register(StreamKind::Packet, prov(0));
            let store = PacketStore::open_with_cap(dir, BurstPolicy::default(), cap).unwrap();
            Self {
                bus,
                input,
                store,
                notes: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn ctx(&self) -> StageContext {
            let notes = self.notes.clone();
            StageContext {
                packets: Some(self.store.clone()),
                ..StageContext::new(SourceId("radio".into()))
            }
            .with_note(Arc::new(move |v| notes.lock().unwrap().push(v)))
        }

        /// Run the stage on the input with `ctx`.
        fn spawn(
            &self,
            params: serde_json::Value,
            ctx: StageContext,
        ) -> tokio::task::JoinHandle<Result<(), StageError>> {
            self.spawn_with(params, ctx, CancellationToken::new())
        }

        /// [`Rig::spawn`] under `cancel`.
        fn spawn_with(
            &self,
            params: serde_json::Value,
            ctx: StageContext,
            cancel: CancellationToken,
        ) -> tokio::task::JoinHandle<Result<(), StageError>> {
            let rx = self.bus.subscribe(&self.input).unwrap();
            let bus = self.bus.clone();
            tokio::spawn(async move {
                PacketCaptureStage::new()
                    .run(params, vec![rx], vec![], bus, cancel, ctx)
                    .await
            })
        }

        fn publish(&self, t: u64, payload: Payload) {
            self.bus
                .publish(Frame::new(self.input.clone(), prov(t), payload));
        }
    }

    /// Poll `f` until it holds, for at most 10 s.
    async fn until(mut f: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(tokio::time::Instant::now() < deadline, "timed out");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn first(store: &PacketStore) -> Option<PacketCaptureRecord> {
        store.list().into_iter().next()
    }

    #[tokio::test]
    async fn packets_are_written_with_their_own_times_and_the_job_comment() {
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let h = rig.spawn(params(), rig.ctx());
        rig.publish(T0, packet(LinkType::Ax25Kiss, &[0, 0x82, 0xa0]));
        rig.publish(
            T0 + 1_000_000_000,
            packet(LinkType::Ax25Kiss, &[0, 0x96, 0x68]),
        );
        until(|| first(&rig.store).is_some_and(|r| r.packets == 2)).await;
        let r = first(&rig.store).unwrap();
        let (link, packets) = pcapng::read(&std::fs::read(&r.path).unwrap()).unwrap();
        assert_eq!(link, 202);
        assert_eq!(
            packets.iter().map(|p| p.t_ns).collect::<Vec<_>>(),
            [T0, T0 + 1_000_000_000]
        );
        assert_eq!(packets[0].data, [0, 0x82, 0xa0]);
        assert_eq!(packets[1].data, [0, 0x96, 0x68]);
        for p in &packets {
            assert_eq!(
                p.comment.as_deref(),
                Some("job=DECODER source=radio freq_hz=144800000")
            );
        }
        assert_eq!(r.job, JobId("DECODER".into()));
        assert_eq!(r.mode.as_deref(), Some("aprs_decode"));
        assert_eq!(r.link_type, LinkType::Ax25Kiss);
        assert!(r.writing);
        h.abort();
    }

    #[tokio::test]
    async fn the_capture_closes_when_the_input_closes() {
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let h = rig.spawn(params(), rig.ctx());
        rig.publish(T0, packet(LinkType::Ax25Kiss, &[1, 2, 3]));
        until(|| first(&rig.store).is_some_and(|r| r.packets == 1)).await;
        rig.bus.unregister(&rig.input);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let r = first(&rig.store).unwrap();
        assert!(!r.writing);
        assert_eq!(r.packets, 1);
    }

    /// The supervisor cancels the stage once its `stream_in` ends, which can be before the
    /// stage has read its input's last packets: they are written all the same. On the
    /// test's one-thread runtime the stage first runs after the packets are published,
    /// the input closed and the cancel fired, so none has been read when it sees the
    /// cancel.
    #[tokio::test]
    async fn a_cancel_writes_the_packets_its_input_still_holds() {
        const N: usize = 20;
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let cancel = CancellationToken::new();
        let h = rig.spawn_with(params(), rig.ctx(), cancel.clone());
        for i in 0..N {
            rig.publish(T0 + i as u64, packet(LinkType::Ax25Kiss, &[i as u8; 8]));
        }
        rig.bus.unregister(&rig.input);
        cancel.cancel();
        assert!(matches!(h.await.unwrap(), Err(StageError::Cancelled)));
        let r = first(&rig.store).expect("a capture");
        assert!(!r.writing);
        assert_eq!(r.packets, N as u64);
        let (_, packets) = pcapng::read(&std::fs::read(&r.path).unwrap()).unwrap();
        assert_eq!(packets.len(), N);
        assert_eq!(packets[N - 1].t_ns, T0 + N as u64 - 1);
    }

    #[tokio::test]
    async fn no_file_is_made_before_the_first_packet() {
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let h = rig.spawn(params(), rig.ctx());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rig.store.list().is_empty());
        rig.bus.unregister(&rig.input);
        assert!(matches!(h.await.unwrap(), Ok(())));
        assert!(rig.store.list().is_empty());
        let files: Vec<_> = std::fs::read_dir(tmp.path().join(PACKETS_DIR))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "pcapng"))
            .collect();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn the_job_context_names_the_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let h = rig.spawn(params(), rig.ctx());
        rig.publish(T0, packet(LinkType::Ax25Kiss, &[1, 2, 3]));
        until(|| !rig.notes.lock().unwrap().is_empty()).await;
        let id = first(&rig.store).unwrap().id;
        let note = rig.notes.lock().unwrap()[0].clone();
        assert_eq!(note["capture"], serde_json::json!(id));
        assert_eq!(note["packets"], serde_json::json!(1));
        assert!(note["bytes"].as_u64().is_some_and(|b| b > 0));
        h.abort();
    }

    #[tokio::test]
    async fn a_new_link_type_begins_a_fresh_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let h = rig.spawn(params(), rig.ctx());
        rig.publish(T0, packet(LinkType::Ax25Kiss, &[1, 2, 3]));
        rig.publish(T0 + 1, packet(LinkType::User0, &[4, 5, 6]));
        until(|| rig.store.list().len() == 2 && rig.store.list().iter().all(|r| r.packets == 1))
            .await;
        rig.bus.unregister(&rig.input);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut links: Vec<u16> = rig
            .store
            .list()
            .iter()
            .map(|r| {
                assert!(!r.writing);
                pcapng::read(&std::fs::read(&r.path).unwrap()).unwrap().0
            })
            .collect();
        links.sort();
        assert_eq!(links, [147, 202]);
    }

    #[tokio::test]
    async fn a_capture_of_another_link_type_opens_in_its_own_file() {
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let h = rig.spawn(params(), rig.ctx());
        rig.publish(T0, packet(LinkType::Other(195), &[1, 2]));
        rig.publish(T0 + 1, packet(LinkType::Other(195), &[3, 4]));
        rig.publish(T0 + 2, packet(LinkType::ModeS, &[5, 6]));
        until(|| rig.store.list().iter().map(|r| r.packets).sum::<u64>() == 3).await;
        rig.bus.unregister(&rig.input);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut files: Vec<(u16, LinkType, u64)> = rig
            .store
            .list()
            .iter()
            .map(|r| {
                let n = pcapng::read(&std::fs::read(&r.path).unwrap()).unwrap().0;
                (n, r.link_type, r.packets)
            })
            .collect();
        files.sort_by_key(|f| f.0);
        assert_eq!(
            files,
            [(148, LinkType::ModeS, 1), (195, LinkType::Other(195), 2)]
        );
    }

    #[tokio::test]
    async fn without_a_store_it_fails_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let rig = Rig::new(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let h = rig.spawn(params(), StageContext::new(SourceId("radio".into())));
        let r = h.await.unwrap();
        assert!(
            matches!(&r, Err(StageError::Failed(m)) if m == "packet_capture needs a node's state directory"),
            "{r:?}"
        );
    }

    #[tokio::test]
    async fn a_write_error_fails_the_capture_and_not_its_input() {
        let tmp = tempfile::tempdir().unwrap();
        // A cap so small every packet after a file's first rolls to a fresh file.
        let rig = Rig::new(tmp.path(), 1);
        let h = rig.spawn(params(), rig.ctx());
        rig.publish(T0, packet(LinkType::Ax25Kiss, &[1, 2, 3]));
        until(|| first(&rig.store).is_some_and(|r| r.packets == 1)).await;
        // The store's directory gone and a file in its place: the roll cannot make a file.
        let dir = tmp.path().join(PACKETS_DIR);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        rig.publish(T0 + 1, packet(LinkType::Ax25Kiss, &[4, 5, 6]));
        let r = tokio::time::timeout(Duration::from_secs(10), h)
            .await
            .expect("the stage ends")
            .unwrap();
        let shown = dir.display().to_string();
        assert!(
            matches!(&r, Err(StageError::Failed(m)) if m.contains(&shown)),
            "{r:?}"
        );
        assert!(rig.bus.list().iter().any(|s| s.id == rig.input));
        rig.publish(T0 + 2, packet(LinkType::Ax25Kiss, &[7, 8, 9]));
    }
}
