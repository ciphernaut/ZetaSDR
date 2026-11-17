//! `UrlBackend` against a local in-process HTTP server.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeta_speech::{SpeechBackend, SpeechError, UrlBackend, wav_s16_mono};

const KEY: &str = "k3y-of-the-server";

/// One part of a multipart body: name, filename, content type, bytes.
type Part = (String, Option<String>, Option<String>, Vec<u8>);

/// One request as the server read it.
#[derive(Clone, Debug)]
struct Seen {
    headers: String,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<String> {
        self.headers.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }

    /// The parts of the multipart body.
    fn parts(&self) -> Vec<Part> {
        let ct = self.header("content-type").unwrap();
        let boundary = ct.split("boundary=").nth(1).unwrap().to_string();
        let delim = format!("--{boundary}").into_bytes();
        let mut parts = Vec::new();
        let mut rest = &self.body[..];
        while let Some(at) = find(rest, &delim) {
            rest = &rest[at + delim.len()..];
            if rest.starts_with(b"--") {
                break;
            }
            let rest_nl = &rest[2..];
            let split = find(rest_nl, b"\r\n\r\n").unwrap();
            let head = String::from_utf8_lossy(&rest_nl[..split]).to_string();
            let data = &rest_nl[split + 4..];
            let end = find(data, &delim).unwrap();
            let data = data[..end - 2].to_vec();
            let attr = |key: &str| {
                head.split(key)
                    .nth(1)
                    .map(|s| s.split('"').next().unwrap().to_string())
            };
            let name = attr("name=\"").unwrap();
            let filename = attr("filename=\"");
            let ctype = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Type: "))
                .map(str::to_string);
            parts.push((name, filename, ctype, data));
        }
        parts
    }

    fn field(&self, name: &str) -> Option<String> {
        self.parts()
            .into_iter()
            .find(|p| p.0 == name)
            .map(|p| String::from_utf8(p.3).unwrap())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A server answering every request `status` with `body`; `None` never answers.
async fn serve(answer: Option<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/audio/transcriptions",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let (head_end, len) = loop {
                    let n = conn.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(at) = find(&buf, b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..at]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        break (at + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = conn.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                log.lock().unwrap().push(Seen {
                    headers: String::from_utf8_lossy(&buf[..head_end]).to_string(),
                    body: buf[head_end..].to_vec(),
                });
                let Some((status, body)) = answer else {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    return;
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(reply.as_bytes()).await;
            });
        }
    });
    (url, seen)
}

fn backend(url: &str, key: Option<&str>, model: Option<&str>) -> UrlBackend {
    UrlBackend::new(
        url,
        key.map(str::to_string),
        model.map(str::to_string),
        vec![],
    )
    .unwrap()
}

#[tokio::test]
async fn a_transcribe_posts_the_wav_as_a_multipart_file_and_returns_the_text() {
    let (url, seen) = serve(Some((200, r#"{"text": " over and out "}"#))).await;
    let b = UrlBackend::new(
        &url,
        Some(KEY.into()),
        Some("whisper-1".into()),
        vec!["EN".into(), "de".into()],
    )
    .unwrap();
    assert_eq!(b.languages(), ["en", "de"]);
    assert_eq!(b.name(), format!("url:{}", url.split('/').nth(2).unwrap()));
    let wav = wav_s16_mono(&[1, 2, 3, -4]);
    let t = b.transcribe(&wav, "de").await.unwrap();
    assert_eq!(
        (t.text.as_str(), t.language.as_str()),
        ("over and out", "de")
    );

    let seen = seen.lock().unwrap();
    let req = &seen[0];
    assert!(
        req.headers.starts_with("POST /v1/audio/transcriptions "),
        "{}",
        req.headers
    );
    assert_eq!(
        req.header("authorization").unwrap(),
        format!("Bearer {KEY}")
    );
    let file = req.parts().into_iter().find(|p| p.0 == "file").unwrap();
    assert_eq!(file.1.as_deref(), Some("utterance.wav"));
    assert_eq!(file.2.as_deref(), Some("audio/wav"));
    assert_eq!(file.3, wav);
    assert_eq!(req.field("response_format").unwrap(), "json");
    assert_eq!(req.field("model").unwrap(), "whisper-1");
    assert_eq!(req.field("language").unwrap(), "de");
}

#[tokio::test]
async fn without_a_key_or_model_neither_is_sent() {
    let (url, seen) = serve(Some((200, r#"{"text": "x"}"#))).await;
    backend(&url, None, None)
        .transcribe(&wav_s16_mono(&[0]), "en")
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].header("authorization"), None);
    assert_eq!(seen[0].field("model"), None);
}

#[tokio::test]
async fn a_500_and_a_200_without_text_are_errors_that_leave_the_key_out() {
    for (status, body) in [(500, "boom"), (200, r#"{"detail": "nope"}"#), (500, KEY)] {
        let (url, _) = serve(Some((status, body))).await;
        let err = backend(&url, Some(KEY), None)
            .transcribe(&wav_s16_mono(&[0]), "en")
            .await
            .unwrap_err();
        let SpeechError::Daemon(msg) = &err else {
            panic!("{err:?}");
        };
        assert!(msg.contains(&status.to_string()), "{msg}");
        assert!(!msg.contains(KEY), "{msg}");
        assert!(!err.to_string().contains(KEY));
    }
}

#[tokio::test]
async fn a_server_that_never_answers_frees_its_slot_at_the_timeout() {
    let (url, _) = serve(None).await;
    let b = UrlBackend::with_limits(
        &url,
        None,
        None,
        vec![],
        Duration::from_millis(300),
        1,
        Duration::from_secs(5),
    )
    .unwrap();
    let wav = wav_s16_mono(&[0]);
    for _ in 0..2 {
        // The second call only gets its turn because the first gave its slot back.
        let started = Instant::now();
        let err = b.transcribe(&wav, "en").await.unwrap_err();
        assert!(matches!(err, SpeechError::Timeout(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

#[tokio::test]
async fn a_language_the_backend_lacks_is_unsupported_and_not_sent() {
    let (url, seen) = serve(Some((200, r#"{"text": "x"}"#))).await;
    let err = backend(&url, None, None)
        .transcribe(&wav_s16_mono(&[0]), "de")
        .await
        .unwrap_err();
    assert!(matches!(err, SpeechError::Unsupported { .. }), "{err:?}");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_busy_server_drops_the_call_after_the_queue_wait() {
    let (url, _) = serve(None).await;
    let b = Arc::new(
        UrlBackend::with_limits(
            &url,
            None,
            None,
            vec![],
            Duration::from_secs(5),
            1,
            Duration::from_millis(200),
        )
        .unwrap(),
    );
    let first = {
        let b = b.clone();
        tokio::spawn(async move { b.transcribe(&wav_s16_mono(&[0]), "en").await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let err = b.transcribe(&wav_s16_mono(&[0]), "en").await.unwrap_err();
    assert!(matches!(err, SpeechError::Busy(_)), "{err:?}");
    first.abort();
}

#[test]
fn a_url_that_is_not_http_is_refused() {
    for bad in ["not a url", "ftp://host/x", "file:///x"] {
        assert!(UrlBackend::new(bad, None, None, vec![]).is_err(), "{bad}");
    }
}

#[test]
fn the_name_is_the_host_and_never_the_path_or_a_token_in_it() {
    let b = backend("https://user:pw@gpu-host:8443/v1/x?token=abc", None, None);
    assert_eq!(b.name(), "url:gpu-host:8443");
}
