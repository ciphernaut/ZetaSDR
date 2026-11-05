//! Typed stream bus: every output stream, with provenance, between stages, jobs and clients.
//!
//! Each source may have a feed: an event stream that `publish` fans every other event frame of
//! that source into, provenance (and so the job) intact; `register_unfed` streams stay out of it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::port::IqBlock;
use crate::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

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
    /// One image. `frame` holds `width × height × 3` bytes for `rgb8`, `width × height` for
    /// `gray8`, or a PNG file (any non-zero length) for `png`; the bus refuses a frame whose
    /// body does not fit (see [`VideoFormat::check_body`]).
    Video {
        width: u32,
        height: u32,
        format: VideoFormat,
        frame: Arc<[u8]>,
    },
    Text(String),
    Event(serde_json::Value),
    Sink(IqBlock),
}

/// How a [`Payload::Video`] frame's bytes are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoFormat {
    /// Row-major, 3 bytes per pixel.
    Rgb8,
    /// Row-major, 1 byte per pixel.
    Gray8,
    /// A PNG file; the header's dimensions are the frame's.
    Png,
}

impl VideoFormat {
    /// The wire name: `rgb8`, `gray8` or `png`.
    pub fn name(self) -> &'static str {
        match self {
            VideoFormat::Rgb8 => "rgb8",
            VideoFormat::Gray8 => "gray8",
            VideoFormat::Png => "png",
        }
    }

    /// Check that a body of `len` bytes fits `width × height` in this format: exactly
    /// `width × height × 3` (rgb8) or `width × height` (gray8), any non-zero length for png.
    pub fn check_body(self, width: u32, height: u32, len: usize) -> Result<(), String> {
        let pixels = u64::from(width) * u64::from(height);
        let want = match self {
            VideoFormat::Rgb8 => pixels * 3,
            VideoFormat::Gray8 => pixels,
            VideoFormat::Png => {
                return if len > 0 {
                    Ok(())
                } else {
                    Err("png video frame has an empty body".into())
                };
            }
        };
        if len as u64 == want {
            Ok(())
        } else {
            Err(format!(
                "{} video frame of {width}x{height} needs a {want}-byte body, got {len}",
                self.name()
            ))
        }
    }
}

impl Payload {
    pub fn kind(&self) -> StreamKind {
        match self {
            Payload::Iq(_) => StreamKind::Iq,
            Payload::Audio { .. } => StreamKind::Audio,
            Payload::Video { .. } => StreamKind::Video,
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
    feeds: RwLock<HashMap<SourceId, StreamId>>,
    unfed: RwLock<HashSet<StreamId>>,
}

impl StreamBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            dropped: AtomicU64::new(0),
            streams: RwLock::new(HashMap::new()),
            feeds: RwLock::new(HashMap::new()),
            unfed: RwLock::new(HashSet::new()),
        }
    }

    /// The source's event feed, registered on first call; later calls answer the same id.
    pub fn register_feed(&self, source: &SourceId) -> StreamId {
        let mut feeds = self.feeds.write().unwrap();
        if let Some(id) = feeds.get(source) {
            return id.clone();
        }
        let id = self.register(
            StreamKind::Event,
            Provenance {
                source: source.clone(),
                job: None,
                channel: None,
                span: Span {
                    start_ns: Timestamp(0),
                    end_ns: Timestamp(0),
                },
            },
        );
        feeds.insert(source.clone(), id.clone());
        id
    }

    pub fn feed_of(&self, source: &SourceId) -> Option<StreamId> {
        self.feeds.read().unwrap().get(source).cloned()
    }

    /// As [`StreamBus::register`], but the stream's frames stay out of its source's feed.
    pub fn register_unfed(&self, kind: StreamKind, provenance: Provenance) -> StreamId {
        let id = self.register(kind, provenance);
        self.unfed.write().unwrap().insert(id.clone());
        id
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
        // Lock order is feeds, then streams (as in `register_feed`).
        let feeds = self.feeds.read().unwrap();
        let streams = self.streams.read().unwrap();
        if let Some((info, tx)) = streams.get(&frame.stream) {
            if info.kind != frame.kind {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if matches!(frame.payload, Payload::Audio { channels: 0, .. }) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if let Payload::Video {
                width,
                height,
                format,
                frame: body,
            } = &frame.payload
                && format.check_body(*width, *height, body.len()).is_err()
            {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if frame.kind == StreamKind::Event {
                if feeds.values().any(|f| *f == frame.stream) {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                let source = &frame.provenance.source;
                if source.0 != "node"
                    && let Some(feed) = feeds.get(source)
                    && !self.unfed.read().unwrap().contains(&frame.stream)
                    && let Some((_, ftx)) = streams.get(feed)
                {
                    let _ = ftx.send(Frame {
                        stream: feed.clone(),
                        ..frame.clone()
                    });
                }
            }
            let _ = tx.send(frame);
        }
    }

    /// Number of frames dropped for carrying the wrong kind for their stream, or an audio
    /// frame with 0 channels, or a video frame whose body does not fit its format.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Remove a stream; its sender is dropped, so subscribers see `Closed`
    /// once they have drained what was already sent.
    pub fn unregister(&self, id: &StreamId) {
        self.streams.write().unwrap().remove(id);
        self.unfed.write().unwrap().remove(id);
        self.feeds.write().unwrap().retain(|_, f| f != id);
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

    fn job_prov(src: &str, job: &str) -> Provenance {
        Provenance {
            job: Some(crate::types::JobId(job.into())),
            ..prov(src)
        }
    }

    #[tokio::test]
    async fn feed_carries_every_event_of_a_source_with_its_job() {
        let bus = StreamBus::new(8);
        let feed = bus.register_feed(&SourceId("a".into()));
        let mut rx = bus.subscribe(&feed).unwrap();
        let s1 = bus.register(StreamKind::Event, job_prov("a", "j1"));
        let s2 = bus.register(StreamKind::Event, job_prov("a", "j2"));
        let mut own = bus.subscribe(&s1).unwrap();
        bus.publish(Frame::new(
            s1.clone(),
            job_prov("a", "j1"),
            Payload::Event(serde_json::json!({"n": 1})),
        ));
        bus.publish(Frame::new(
            s2.clone(),
            job_prov("a", "j2"),
            Payload::Event(serde_json::json!({"n": 2})),
        ));
        let f1 = rx.recv().await.unwrap();
        let f2 = rx.recv().await.unwrap();
        assert_eq!(
            (
                f1.stream.clone(),
                f1.provenance.job.clone().unwrap().0.as_str()
            ),
            (feed.clone(), "j1")
        );
        assert_eq!(
            (
                f2.stream.clone(),
                f2.provenance.job.clone().unwrap().0.as_str()
            ),
            (feed.clone(), "j2")
        );
        assert!(matches!(f1.payload, Payload::Event(ref v) if *v == serde_json::json!({"n": 1})));
        // The job's own stream still carries the frame, under its own id.
        assert_eq!(own.recv().await.unwrap().stream, s1);
    }

    #[tokio::test]
    async fn feed_skips_unfed_streams_other_sources_audio_and_node() {
        let bus = StreamBus::new(8);
        let feed = bus.register_feed(&SourceId("a".into()));
        let mut rx = bus.subscribe(&feed).unwrap();
        let unfed = bus.register_unfed(StreamKind::Event, job_prov("a", "fft"));
        let other = bus.register(StreamKind::Event, job_prov("b", "j"));
        let audio = bus.register(StreamKind::Audio, job_prov("a", "j"));
        let node = bus.register(StreamKind::Event, job_prov("node", "mix"));
        let fed = bus.register(StreamKind::Event, job_prov("a", "j"));
        let ev = |n| Payload::Event(serde_json::json!({"n": n}));
        bus.publish(Frame::new(
            unfed,
            job_prov("a", "fft"),
            Payload::Event(serde_json::json!({"q": [1]})),
        ));
        bus.publish(Frame::new(other, job_prov("b", "j"), ev(1)));
        bus.publish(Frame::new(
            audio,
            job_prov("a", "j"),
            Payload::Audio {
                sample_rate_hz: Hz(48_000),
                channels: 1,
                samples: Arc::from([0.0f32]),
            },
        ));
        bus.publish(Frame::new(node, job_prov("node", "mix"), ev(1)));
        bus.publish(Frame::new(fed, job_prov("a", "j"), ev(2)));
        let f = rx.recv().await.unwrap();
        assert!(matches!(f.payload, Payload::Event(ref v) if *v == serde_json::json!({"n": 2})));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn feed_is_registered_once_and_never_written_directly() {
        let bus = StreamBus::new(8);
        let feed = bus.register_feed(&SourceId("a".into()));
        assert_eq!(bus.register_feed(&SourceId("a".into())), feed);
        assert_eq!(bus.feed_of(&SourceId("a".into())), Some(feed.clone()));
        assert_eq!(bus.feed_of(&SourceId("b".into())), None);
        let mut rx = bus.subscribe(&feed).unwrap();
        bus.publish(Frame::new(
            feed.clone(),
            prov("a"),
            Payload::Event(serde_json::json!({"n": 1})),
        ));
        assert_eq!(bus.dropped(), 1);
        assert!(rx.try_recv().is_err());
        let info = bus.list().into_iter().find(|i| i.id == feed).unwrap();
        assert_eq!(
            (
                info.kind,
                info.provenance.job,
                info.provenance.source.0.as_str()
            ),
            (StreamKind::Event, None, "a")
        );
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
    async fn publish_refuses_a_video_body_that_does_not_fit_its_format() {
        let bus = StreamBus::new(8);
        let p = prov("s");
        let id = bus.register(StreamKind::Video, p.clone());
        let mut rx = bus.subscribe(&id).unwrap();
        let video = |format, len| {
            Frame::new(
                id.clone(),
                p.clone(),
                Payload::Video {
                    width: 4,
                    height: 2,
                    format,
                    frame: Arc::from(vec![0u8; len]),
                },
            )
        };
        bus.publish(video(VideoFormat::Rgb8, 23));
        bus.publish(video(VideoFormat::Gray8, 24));
        bus.publish(video(VideoFormat::Png, 0));
        let got = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(
            got.is_err(),
            "a mismatched video frame must not be delivered"
        );
        assert_eq!(bus.dropped(), 3);
        bus.publish(video(VideoFormat::Rgb8, 24));
        bus.publish(video(VideoFormat::Gray8, 8));
        bus.publish(video(VideoFormat::Png, 10));
        for _ in 0..3 {
            assert!(rx.recv().await.is_ok());
        }
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
