//! Typed stream bus: every output stream, with provenance, between stages, jobs and clients.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::port::IqBlock;
use crate::types::{Hz, Provenance, StreamId, StreamKind};

#[derive(Clone, Debug)]
pub enum Payload {
    Iq(IqBlock),
    /// PCM audio. `samples` is interleaved (`frames × channels` values); `channels` is at
    /// least 1, and the bus refuses a frame that says 0.
    Audio {
        sample_rate_hz: Hz,
        channels: u16,
        samples: Arc<[f32]>,
    },
    /// Deferred: no video payload is carried yet.
    Video,
    Text(String),
    Event(serde_json::Value),
    Sink(IqBlock),
}

impl Payload {
    pub fn kind(&self) -> StreamKind {
        match self {
            Payload::Iq(_) => StreamKind::Iq,
            Payload::Audio { .. } => StreamKind::Audio,
            Payload::Video => StreamKind::Video,
            Payload::Text(_) => StreamKind::Text,
            Payload::Event(_) => StreamKind::Event,
            Payload::Sink(_) => StreamKind::Sink,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub stream: StreamId,
    pub kind: StreamKind,
    pub provenance: Provenance,
    pub payload: Payload,
}

impl Frame {
    /// Build a frame whose `kind` is derived from its payload.
    pub fn new(stream: StreamId, provenance: Provenance, payload: Payload) -> Self {
        Self {
            stream,
            kind: payload.kind(),
            provenance,
            payload,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct StreamInfo {
    pub id: StreamId,
    pub kind: StreamKind,
    pub provenance: Provenance,
}

pub struct StreamBus {
    capacity: usize,
    dropped: AtomicU64,
    streams: RwLock<HashMap<StreamId, (StreamInfo, broadcast::Sender<Frame>)>>,
}

impl StreamBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            dropped: AtomicU64::new(0),
            streams: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, kind: StreamKind, provenance: Provenance) -> StreamId {
        let id = StreamId::new();
        let (tx, _) = broadcast::channel(self.capacity);
        let info = StreamInfo {
            id: id.clone(),
            kind,
            provenance,
        };
        self.streams.write().unwrap().insert(id.clone(), (info, tx));
        id
    }

    /// Publish to subscribers. Frames for unregistered streams and sends with
    /// no subscribers are dropped silently; frames whose kind differs from the
    /// registered stream's are dropped and counted in [`StreamBus::dropped`].
    pub fn publish(&self, frame: Frame) {
        if let Some((info, tx)) = self.streams.read().unwrap().get(&frame.stream) {
            if info.kind != frame.kind {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if matches!(frame.payload, Payload::Audio { channels: 0, .. }) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let _ = tx.send(frame);
        }
    }

    /// Number of frames dropped for carrying the wrong kind for their stream, or an audio
    /// frame with 0 channels.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Remove a stream; its sender is dropped, so subscribers see `Closed`
    /// once they have drained what was already sent.
    pub fn unregister(&self, id: &StreamId) {
        self.streams.write().unwrap().remove(id);
    }

    pub fn subscribe(&self, id: &StreamId) -> Option<broadcast::Receiver<Frame>> {
        self.streams
            .read()
            .unwrap()
            .get(id)
            .map(|(_, tx)| tx.subscribe())
    }

    /// Live subscribers of a registered stream; `None` if it is not registered.
    pub fn receiver_count(&self, id: &StreamId) -> Option<usize> {
        self.streams
            .read()
            .unwrap()
            .get(id)
            .map(|(_, tx)| tx.receiver_count())
    }

    pub fn list(&self) -> Vec<StreamInfo> {
        self.streams
            .read()
            .unwrap()
            .values()
            .map(|(i, _)| i.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SourceId, Span, Timestamp};

    fn prov(src: &str) -> Provenance {
        Provenance {
            source: SourceId(src.into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        }
    }

    #[tokio::test]
    async fn subscriber_receives_published_frame() {
        let bus = StreamBus::new(8);
        let p = prov("s");
        let id = bus.register(StreamKind::Text, p.clone());
        let mut rx = bus.subscribe(&id).unwrap();
        bus.publish(Frame::new(id.clone(), p, Payload::Text("hi".into())));
        let f = rx.recv().await.unwrap();
        assert_eq!(f.stream, id);
        assert_eq!(f.kind, StreamKind::Text);
        assert!(matches!(f.payload, Payload::Text(ref s) if s == "hi"));
    }

    #[tokio::test]
    async fn publish_with_mismatched_kind_is_dropped_and_counted() {
        let bus = StreamBus::new(8);
        let p = prov("s");
        let id = bus.register(StreamKind::Text, p.clone());
        let mut rx = bus.subscribe(&id).unwrap();
        bus.publish(Frame::new(id, p, Payload::Event(serde_json::json!({}))));
        let got = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(got.is_err(), "mismatched frame must not be delivered");
        assert_eq!(bus.dropped(), 1);
    }

    #[tokio::test]
    async fn publish_refuses_audio_with_zero_channels() {
        let bus = StreamBus::new(8);
        let p = prov("s");
        let id = bus.register(StreamKind::Audio, p.clone());
        let mut rx = bus.subscribe(&id).unwrap();
        let audio = |channels| {
            Frame::new(
                id.clone(),
                p.clone(),
                Payload::Audio {
                    sample_rate_hz: Hz(8_000),
                    channels,
                    samples: Arc::from(vec![0.0f32; 4]),
                },
            )
        };
        bus.publish(audio(0));
        let got = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(got.is_err(), "a 0-channel frame must not be delivered");
        assert_eq!(bus.dropped(), 1);
        bus.publish(audio(2));
        assert!(rx.recv().await.is_ok());
    }

    #[tokio::test]
    async fn list_reports_registered_streams_with_provenance() {
        let bus = StreamBus::new(8);
        bus.register(StreamKind::Text, prov("a"));
        bus.register(StreamKind::Event, prov("b"));
        let mut l = bus.list();
        l.sort_by(|x, y| x.provenance.source.0.cmp(&y.provenance.source.0));
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].provenance.source, SourceId("a".into()));
        assert_eq!(l[1].kind, StreamKind::Event);
    }

    #[tokio::test]
    async fn unregister_closes_subscribers_and_drops_from_list() {
        let bus = StreamBus::new(8);
        let id = bus.register(StreamKind::Text, prov("s"));
        let mut rx = bus.subscribe(&id).unwrap();
        bus.unregister(&id);
        assert!(bus.list().is_empty());
        assert!(bus.subscribe(&id).is_none());
        assert!(matches!(
            rx.recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
    }

    #[test]
    fn receiver_count_tracks_subscribers() {
        let bus = StreamBus::new(1);
        let id = bus.register(StreamKind::Text, prov("s"));
        assert_eq!(bus.receiver_count(&id), Some(0));
        let _rx = bus.subscribe(&id).unwrap();
        assert_eq!(bus.receiver_count(&id), Some(1));
        assert_eq!(bus.receiver_count(&StreamId::new()), None);
    }

    #[test]
    fn subscribe_unknown_is_none() {
        assert!(StreamBus::new(1).subscribe(&StreamId::new()).is_none());
    }
}
