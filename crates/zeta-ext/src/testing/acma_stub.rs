//! A stub of the register service (the MCP server the `acma` provider asks), served on
//! 127.0.0.1 by [`serve`]: `initialize` (giving a session id), `tools/call` for
//! `execute_sql` and `get_frequency_allocation`, and `GET /health`.
//!
//! `execute_sql` answers the rows given to [`serve`], each an array in [`COLUMNS`] order,
//! keeping those whose `FREQUENCY ± BANDWIDTH / 2` (a NULL bandwidth as 0 Hz) overlaps the
//! band the query names, as the register does, so a row on one band is never answered on
//! another. `get_frequency_allocation` answers one plan entry, 403-406 MHz, for a frequency
//! inside it and no entry elsewhere. Every row a test hands it is synthetic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

/// The columns of an `execute_sql` answer, in order; `CALL_SIGN` is the last.
pub const COLUMNS: [&str; 12] = [
    "FREQUENCY",
    "BANDWIDTH",
    "EMISSION",
    "DEVICE_TYPE",
    "TRANSMITTER_POWER",
    "TRANSMITTER_POWER_UNIT",
    "LICENCE_NO",
    "SITE_NAME",
    "LATITUDE",
    "LONGITUDE",
    "LICENCEE",
    "CALL_SIGN",
];

/// The plan entry's band: 403-406 MHz.
const PLAN_HZ: (u64, u64) = (403_000_000, 406_000_000);
/// How long [`Mode::Slow`] holds every tool call.
const SLOW: Duration = Duration::from_secs(3);

/// How the stub misbehaves, if at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    /// The plan entry is a satellite service.
    Satellite,
    /// Every tool call is a JSON-RPC error.
    RpcError,
    /// Every tool call answers HTML.
    Html,
    /// Every tool call answers after [`SLOW`].
    Slow,
    /// The first row without a `BANDWIDTH` (NULL in the register).
    NullBandwidth,
    /// A service that answers no `CALL_SIGN` column.
    NoCallSign,
    /// An older register whose `device_details` lacks `CALL_SIGN`: SQL naming
    /// `d.CALL_SIGN` is rejected with SQLite's error as a JSON-RPC error.
    NoCallSignColumnRpc,
    /// The same register answering the rejection as a tool error's text.
    NoCallSignColumnTool,
}

/// What the stub answers from, and what it saw.
#[derive(Debug, Default)]
pub struct Stub {
    pub mode: Mode,
    /// The rows `execute_sql` answers from, each in [`COLUMNS`] order.
    pub rows: Vec<Value>,
    /// Tool calls seen.
    pub calls: AtomicUsize,
    /// `get_frequency_allocation` calls seen.
    pub plans: AtomicUsize,
    /// The last `execute_sql` arguments.
    pub sql: Mutex<Option<Value>>,
    /// `execute_sql` calls seen.
    pub queries: AtomicUsize,
    /// `execute_sql` calls rejected for naming a missing `CALL_SIGN`.
    pub rejected: AtomicUsize,
    /// `execute_sql` calls whose SQL named no band the stub could read: answered with every
    /// row, unfiltered, and said on stderr. A test that relies on the cut asserts it is 0.
    pub unbanded: AtomicUsize,
}

fn sse(v: Value) -> Response {
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!("event: message\ndata: {v}\n\n"),
    )
        .into_response()
}

/// The number after `marker` in `sql`.
fn number_after(sql: &str, marker: &str) -> Option<u64> {
    let rest = &sql[sql.find(marker)? + marker.len()..];
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The band `sql` asks for, `(lo, hi)`: the `>= lo` and `<= hi` of its overlap test.
fn band_of(sql: &str) -> Option<(u64, u64)> {
    let hi = number_after(sql, "COALESCE(d.BANDWIDTH, 0) / 2 <=")?;
    let lo = number_after(sql, "COALESCE(d.BANDWIDTH, 0) / 2 >=")?;
    Some((lo, hi))
}

/// Whether `row` (in [`COLUMNS`] order) overlaps `lo..=hi`.
fn overlaps(row: &Value, (lo, hi): (u64, u64)) -> bool {
    let Some(f) = row[0].as_u64() else {
        return false;
    };
    let half = row[1].as_u64().unwrap_or(0) / 2;
    f.saturating_sub(half) <= hi && f + half >= lo
}

fn plan(freq_hz: Option<u64>, satellite: bool) -> Value {
    let inside = freq_hz.is_some_and(|f| (PLAN_HZ.0..=PLAN_HZ.1).contains(&f));
    if !inside {
        return json!({"match_count": 0, "allocation": null, "regions": {}});
    }
    let services = if satellite {
        json!([{"name": "MOBILE-SATELLITE", "primary": true}])
    } else {
        json!([
            {"name": "FIXED", "primary": true, "inline_footnotes": []},
            {"name": "MOBILE", "primary": true, "qualifier": "except aeronautical mobile (R)"},
            {"name": "METEOROLOGICAL AIDS", "primary": false},
            {"name": "RADIOLOCATION", "primary": false, "inline_footnotes": ["AUS29"]}
        ])
    };
    json!({"match_count": 1, "allocation": {"freq_start_hz": PLAN_HZ.0,
        "freq_end_hz": PLAN_HZ.1, "unit": "MHz", "page": 50, "services": services,
        "footnotes": [], "raw": ""}, "regions": {}})
}

/// The `execute_sql` answer to `args`, or the whole JSON-RPC message rejecting a missing
/// `CALL_SIGN`.
fn execute_sql(s: &Stub, id: &Value, args: &Value) -> Result<Value, Value> {
    *s.sql.lock().unwrap() = Some(args.clone());
    s.queries.fetch_add(1, Ordering::SeqCst);
    let sql = args["sql"].as_str().unwrap_or_default();
    let older = matches!(
        s.mode,
        Mode::NoCallSignColumnRpc | Mode::NoCallSignColumnTool
    );
    if older && sql.contains("d.CALL_SIGN") {
        s.rejected.fetch_add(1, Ordering::SeqCst);
        let error = "no such column: d.CALL_SIGN";
        if s.mode == Mode::NoCallSignColumnRpc {
            return Err(json!({"jsonrpc":"2.0","id":id,
                "error":{"code":-32603,"message":error}}));
        }
        return Err(json!({"jsonrpc":"2.0","id":id,
            "result":{"isError":true,"content":[{"type":"text",
                "text":format!("SQL error: {error}")}]}}));
    }
    let mut rows = s.rows.clone();
    if s.mode == Mode::NullBandwidth
        && let Some(first) = rows.first_mut()
    {
        first[1] = Value::Null;
    }
    match band_of(sql) {
        Some(band) => rows.retain(|r| overlaps(r, band)),
        None => {
            s.unbanded.fetch_add(1, Ordering::SeqCst);
            eprintln!("acma_stub: no band in the SQL, every row answered: {sql}");
        }
    }
    let mut columns: Vec<&str> = COLUMNS.to_vec();
    if older {
        // `NULL AS CALL_SIGN`: the column, empty.
        for row in &mut rows {
            row[COLUMNS.len() - 1] = Value::Null;
        }
    }
    if s.mode == Mode::NoCallSign {
        columns.pop();
        for row in &mut rows {
            if let Some(r) = row.as_array_mut() {
                r.truncate(COLUMNS.len() - 1);
            }
        }
    }
    let n = rows.len();
    Ok(
        json!({"columns": columns, "rows": rows, "truncated": false, "rowCount": n,
        "result_id": "r1", "_hints": []}),
    )
}

async fn mcp(State(s): State<Arc<Stub>>, headers: HeaderMap, body: String) -> Response {
    let req: Value = serde_json::from_str(&body).unwrap();
    let id = req["id"].clone();
    match req["method"].as_str().unwrap() {
        "initialize" => {
            let mut r =
                sse(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26"}}));
            r.headers_mut()
                .insert("mcp-session-id", "session-0".parse().unwrap());
            r
        }
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/call" => {
            assert_eq!(headers.get("mcp-session-id").unwrap(), "session-0");
            s.calls.fetch_add(1, Ordering::SeqCst);
            if req["params"]["name"] == "get_frequency_allocation" {
                s.plans.fetch_add(1, Ordering::SeqCst);
            }
            match s.mode {
                Mode::RpcError => {
                    return sse(json!({"jsonrpc":"2.0","id":id,
                        "error":{"code":-32603,"message":"database is locked"}}));
                }
                Mode::Html => return "<html>".into_response(),
                Mode::Slow => tokio::time::sleep(SLOW).await,
                _ => {}
            }
            let args = &req["params"]["arguments"];
            let out = match req["params"]["name"].as_str().unwrap() {
                "execute_sql" => match execute_sql(&s, &id, args) {
                    Ok(v) => v,
                    Err(rejection) => return sse(rejection),
                },
                "get_frequency_allocation" => {
                    plan(args["freq_hz"].as_u64(), s.mode == Mode::Satellite)
                }
                t => panic!("unexpected tool {t}"),
            };
            sse(json!({"jsonrpc":"2.0","id":id,
                "result":{"content":[{"type":"text","text":out.to_string()}]}}))
        }
        m => panic!("unexpected method {m}"),
    }
}

/// Serve the stub on 127.0.0.1 (a task on the current tokio runtime, serving as long as
/// that runs) answering `rows`, each an array in [`COLUMNS`] order; its `http://HOST:PORT`
/// and what it sees.
pub async fn serve(rows: Vec<Value>, mode: Mode) -> (String, Arc<Stub>) {
    let stub = Arc::new(Stub {
        mode,
        rows,
        ..Stub::default()
    });
    let app = Router::new()
        .route("/mcp", post(mcp))
        .route(
            "/health",
            get(|| async { r#"{"status":"ok","version":"1.11.0","isSyncing":false}"# }),
        )
        .with_state(stub.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, stub)
}

/// [`serve`] on a thread with a runtime of its own, for a test that has none; the URL. It
/// serves as long as the test process lives.
pub fn serve_on_thread(rows: Vec<Value>, mode: Mode) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("register stub runtime");
        rt.block_on(async move {
            let (url, _stub) = serve(rows, mode).await;
            tx.send(url).expect("hand the stub's url over");
            std::future::pending::<()>().await;
        });
    });
    rx.recv().expect("the register stub starts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_is_read_from_the_query_and_cuts_the_rows() {
        let sql = "SELECT 1 WHERE d.FREQUENCY - COALESCE(d.BANDWIDTH, 0) / 2 <= 100006250 \
                   AND d.FREQUENCY + COALESCE(d.BANDWIDTH, 0) / 2 >= 99993750 ORDER BY 1";
        let band = band_of(sql).unwrap();
        assert_eq!(band, (99_993_750, 100_006_250));
        assert!(overlaps(&json!([100_000_000, 16000]), band));
        assert!(overlaps(&json!([100_010_000, 16000]), band));
        assert!(!overlaps(&json!([100_020_000, 16000]), band));
        assert!(!overlaps(&json!([404_025_000, null]), band));
    }

    #[test]
    fn the_plan_has_an_entry_only_inside_its_band() {
        assert_eq!(plan(Some(404_025_000), false)["match_count"], 1);
        assert!(plan(Some(100_000_000), false)["allocation"].is_null());
        assert!(plan(None, false)["allocation"].is_null());
    }
}
