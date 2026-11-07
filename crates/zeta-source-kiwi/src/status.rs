//! The KiwiSDR status page: a text page of `key=value` lines served at
//! [`KIWI_STATUS_PATH`] on the receiver's web port. The keys read here are
//! `gps=(lat, lon)`, `asl=` (metres above sea level), `gps_good=` (satellites
//! in the fix) and `name=`; every other line is ignored.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// The status page's path on the receiver's web port.
pub const KIWI_STATUS_PATH: &str = "/status";
/// Largest status response read; a longer one is an error.
const MAX_STATUS_BYTES: u64 = 64 * 1024;
/// Port of an `http://` URL that names none.
const HTTP_DEFAULT_PORT: u16 = 80;

/// What a status page says about the receiver.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KiwiStatus {
    /// `(lat, lon)` in degrees; `None` when missing, malformed, out of range
    /// or `(0, 0)` (an unconfigured receiver).
    pub position: Option<(f64, f64)>,
    /// Metres above sea level.
    pub asl_m: Option<f64>,
    /// Satellites in the GPS fix.
    pub gps_good: Option<u32>,
    pub name: Option<String>,
}

/// Read a status page's text. Never fails: what cannot be read is `None`.
pub fn parse(text: &str) -> KiwiStatus {
    let mut s = KiwiStatus::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "gps" => s.position = parse_gps(value),
            "asl" => s.asl_m = value.parse::<f64>().ok().filter(|v| v.is_finite()),
            "gps_good" => s.gps_good = value.parse().ok(),
            "name" => s.name = Some(value.to_string()).filter(|n| !n.is_empty()),
            _ => {}
        }
    }
    s
}

/// `(lat, lon)`, in range and not the origin.
fn parse_gps(value: &str) -> Option<(f64, f64)> {
    let inner = value.strip_prefix('(')?.strip_suffix(')')?;
    let (lat, lon) = inner.split_once(',')?;
    let lat: f64 = lat.trim().parse().ok()?;
    let lon: f64 = lon.trim().parse().ok()?;
    let ok = (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
        && !(lat == 0.0 && lon == 0.0);
    ok.then_some((lat, lon))
}

/// `http://host[:port][/path]` as `(host, port, path)`; any other scheme is
/// an error.
fn split_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("status url {url:?}: only http:// is supported"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            let port = p
                .parse()
                .map_err(|_| format!("status url {url:?}: bad port"))?;
            (h, port)
        }
        None => (authority, HTTP_DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err(format!("status url {url:?}: no host"));
    }
    Ok((host.to_string(), port, path.to_string()))
}

/// One HTTP/1.0 `GET` of a status page, the whole exchange bounded by
/// `limit`; a status other than 200 is an error.
pub async fn fetch(url: &str, limit: Duration) -> Result<KiwiStatus, String> {
    let (host, port, path) = split_url(url)?;
    let get = async {
        let mut tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| format!("status: connect: {e}"))?;
        let req = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n");
        tcp.write_all(req.as_bytes())
            .await
            .map_err(|e| format!("status: send: {e}"))?;
        let mut raw = Vec::new();
        (&mut tcp)
            .take(MAX_STATUS_BYTES + 1)
            .read_to_end(&mut raw)
            .await
            .map_err(|e| format!("status: read: {e}"))?;
        if raw.len() as u64 > MAX_STATUS_BYTES {
            return Err(format!("status: response over {MAX_STATUS_BYTES} bytes"));
        }
        let text = String::from_utf8_lossy(&raw);
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| "status: no header end".to_string())?;
        let status_line = head.lines().next().unwrap_or_default();
        if status_line.split_whitespace().nth(1) != Some("200") {
            return Err(format!("status: {status_line:?}"));
        }
        Ok(parse(body))
    };
    match timeout(limit, get).await {
        Ok(r) => r,
        Err(_) => Err(format!("status: no answer in {} ms", limit.as_millis())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn parses_the_position_altitude_and_fixes() {
        let s = parse("status=active\ngps=(-27.40, 153.10)\nasl=12\ngps_good=9\nname=a kiwi\n");
        assert_eq!(s.position, Some((-27.40, 153.10)));
        assert_eq!(s.asl_m, Some(12.0));
        assert_eq!(s.gps_good, Some(9));
        assert_eq!(s.name.as_deref(), Some("a kiwi"));
        // CRLF line ends, spaces around the parts, unknown lines.
        let s = parse("users=0\r\n gps = ( -27.3842 ,153.1175 ) \r\nasl= 4\r\nfoo\r\n");
        assert_eq!(s.position, Some((-27.3842, 153.1175)));
        assert_eq!(s.asl_m, Some(4.0));
        assert_eq!(s.gps_good, None);
        assert_eq!(s.name, None);
    }

    #[test]
    fn an_origin_gps_is_no_position() {
        for text in [
            "gps=(0, 0)\n",
            "gps=(0.0, 0.000)\n",
            "gps=(95, 10)\n",
            "gps=(-27, 181)\n",
            "status=active\nasl=12\n",
            "gps=junk\n",
            "gps=(-27.4)\n",
            "gps=(-27.4, 153.1, 9)\n",
            "gps=(NaN, 153.1)\n",
            "",
        ] {
            assert_eq!(parse(text).position, None, "{text:?}");
        }
    }

    async fn stub(reply: Option<&'static str>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/status", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            match reply {
                Some(body) => {
                    let mut buf = [0u8; 1024];
                    let _ = tcp.read(&mut buf).await;
                    let page = format!("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n{body}");
                    tcp.write_all(page.as_bytes()).await.unwrap();
                }
                None => tokio::time::sleep(Duration::from_secs(30)).await,
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn fetch_reads_one_status_from_a_stub_and_times_out_on_silence() {
        let (url, _t) = stub(Some(
            "status=active\ngps=(-27.40, 153.10)\nasl=12\ngps_good=9\n",
        ))
        .await;
        let s = fetch(&url, Duration::from_secs(2)).await.unwrap();
        assert_eq!(s.position, Some((-27.40, 153.10)));
        assert_eq!(s.asl_m, Some(12.0));
        assert_eq!(s.gps_good, Some(9));

        let (url, t) = stub(None).await;
        let started = std::time::Instant::now();
        let err = fetch(&url, Duration::from_millis(200)).await.unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2), "{err}");
        assert!(err.contains("200 ms"), "{err}");
        t.abort();

        let quick = Duration::from_millis(200);
        assert!(fetch("https://127.0.0.1:1/status", quick).await.is_err());
        assert!(fetch("http://", quick).await.is_err());
    }

    #[test]
    fn urls_are_split_into_host_port_and_path() {
        assert_eq!(
            split_url("http://127.0.0.1:8073/status"),
            Ok(("127.0.0.1".to_string(), 8073, "/status".to_string()))
        );
        assert_eq!(
            split_url("http://kiwi-host"),
            Ok(("kiwi-host".to_string(), 80, "/".to_string()))
        );
        assert!(split_url("ftp://x/status").is_err());
        assert!(split_url("http://x:port/status").is_err());
    }
}
