//! Typed stream bus: every output stream, with provenance, between stages, jobs and clients.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::port::IqBlock;
use crate::types::{Hz, Provenance, StreamId, StreamKind};

#[derive(Clone, Debug)]
pub enum Payload {
    Iq(IqBlock),
    Audio { sample_rate_hz: Hz, samples: Arc<[f32]> },
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamInfo {
    pub id: StreamId,
    pub kind: StreamKind,
    pub provenance: Provenance,
}

pub struct StreamBus {
    capacity: usize,
    streams: RwLock<HashMap<StreamId, (StreamInfo, broadcast::Sender<Frame>)>>,
}

impl StreamBus {
    pub fn new(capacity: usize) -> Self {
        Self { capacity, streams: RwLock::new(HashMap::new()) }
    }

    pub fn register(&self, kind: StreamKind, provenance: Provenance) -> StreamId {
        let id = StreamId::new();
        let (tx, _) = broadcast::channel(self.capacity);
        let info = StreamInfo { id: id.clone(), kind, provenance };
        self.streams.write().unwrap().insert(id.clone(), (info, tx));
        id
    }

    /// Publish to subscribers. Frames for unregistered streams and sends with
    /// no subscribers are dropped silently.
    pub fn publish(&self, frame: Frame) {
        if let Some((_, tx)) = self.streams.read().unwrap().get(&frame.stream) {
            let _ = tx.send(frame);
        }
    }

    pub fn subscribe(&self, id: &StreamId) -> Option<broadcast::Receiver<Frame>> {
        self.streams.read().unwrap().get(id).map(|(_, tx)| tx.subscribe())
    }

    pub fn list(&self) -> Vec<StreamInfo> {
        self.streams.read().unwrap().values().map(|(i, _)| i.clone()).collect()
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
            span: Span { start: Timestamp(0), end: Timestamp(1) },
        }
    }

    #[tokio::test]
    async fn subscriber_receives_published_frame() {
        let bus = StreamBus::new(8);
        let p = prov("s");
        let id = bus.register(StreamKind::Text, p.clone());
        let mut rx = bus.subscribe(&id).unwrap();
        bus.publish(Frame {
            stream: id.clone(),
            kind: StreamKind::Text,
            provenance: p,
            payload: Payload::Text("hi".into()),
        });
        let f = rx.recv().await.unwrap();
        assert_eq!(f.stream, id);
        assert_eq!(f.kind, StreamKind::Text);
        assert!(matches!(f.payload, Payload::Text(ref s) if s == "hi"));
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

    #[test]
    fn subscribe_unknown_is_none() {
        assert!(StreamBus::new(1).subscribe(&StreamId::new()).is_none());
    }
}
