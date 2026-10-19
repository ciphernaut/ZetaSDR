//! `NodeBackend` against `zeta-ws` test servers.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;

use zeta_client::{ClientError, NodeClient};
use zeta_speech::testing::FakeBackend;
use zeta_speech::{NodeBackend, SpeechBackend, SpeechError, wav_s16_mono};
use zeta_ws::testing::{TestOpts, spawn_with};

const SECRET: &str = "s3cret";

fn with_speech(speech: Arc<dyn SpeechBackend>) -> TestOpts {
    TestOpts {
        speech: Some(speech),
        ..TestOpts::default()
    }
}

#[tokio::test]
async fn forwards_to_the_node_and_returns_its_text() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeBackend::new("over and out").with_languages(&["en", "de"]));
    let ts = spawn_with(dir.path(), with_speech(fake.clone())).await;

    let b = NodeBackend::new(&ts.url, SECRET).await.unwrap();
    assert_eq!(b.languages(), ["en", "de"]);
    assert!(b.name().contains(&ts.url), "{}", b.name());
    let wav = wav_s16_mono(&[1, 2, 3]);
    let t = b.transcribe(&wav, "en").await.unwrap();
    assert_eq!(
        (t.text.as_str(), t.language.as_str()),
        ("over and out", "en")
    );
    assert_eq!(fake.calls(), vec![(wav, "en".to_string())]);
    ts.stop().await;
}

#[tokio::test]
async fn an_unreachable_node_assumes_english() {
    let b = NodeBackend::new("http://127.0.0.1:9", SECRET)
        .await
        .unwrap();
    assert_eq!(b.languages(), ["en"]);
    let err = b.transcribe(&wav_s16_mono(&[0]), "en").await.unwrap_err();
    assert!(matches!(err, SpeechError::Unreachable(_)), "{err:?}");
}

#[tokio::test]
async fn an_unsupported_language_comes_back_as_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn_with(dir.path(), with_speech(Arc::new(FakeBackend::new("x")))).await;
    let b = NodeBackend::new(&ts.url, SECRET).await.unwrap();
    let err = b.transcribe(&wav_s16_mono(&[0]), "de").await.unwrap_err();
    assert_eq!(
        err,
        SpeechError::Unsupported {
            language: "de".to_string(),
            supported: vec!["en".to_string()],
        }
    );
    ts.stop().await;
}

/// A loopback address nothing listens on yet.
fn free_addr() -> SocketAddr {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
}

#[tokio::test]
async fn two_nodes_forwarding_to_each_other_refuse_the_loop() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let b_addr = free_addr();
    let to_b = NodeBackend::new(&format!("http://{b_addr}"), SECRET)
        .await
        .unwrap();
    let a = spawn_with(dir_a.path(), with_speech(Arc::new(to_b))).await;
    let to_a = NodeBackend::new(&a.url, SECRET).await.unwrap();
    let b = spawn_with(
        dir_b.path(),
        TestOpts {
            addr: Some(b_addr),
            ..with_speech(Arc::new(to_a))
        },
    )
    .await;

    // A request with no hop: A forwards it to B, B refuses the hop with 508, and A reports
    // that as its backend's error.
    assert_forwarding_loop(&a.url).await;
    b.stop().await;
    a.stop().await;
}

#[tokio::test]
async fn a_node_forwarding_to_itself_refuses_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let addr = free_addr();
    let to_self = NodeBackend::new(&format!("http://{addr}"), SECRET)
        .await
        .unwrap();
    let ts = spawn_with(
        dir.path(),
        TestOpts {
            addr: Some(addr),
            ..with_speech(Arc::new(to_self))
        },
    )
    .await;
    assert_forwarding_loop(&ts.url).await;
    ts.stop().await;
}

/// A first-hand `POST /stt` to `url` fails 502 with the forwarding loop the next node saw.
async fn assert_forwarding_loop(url: &str) {
    let client = NodeClient::new(url, SECRET).unwrap();
    let err = client
        .stt(wav_s16_mono(&[0]), "en", false)
        .await
        .unwrap_err();
    let ClientError::Http { status, body, .. } = &err else {
        panic!("want an HTTP error, got {err:?}");
    };
    assert_eq!(*status, 502, "{body}");
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["message"], "speech daemon: stt forwarding loop");
}
