//! A stub of the register service (the MCP server the `acma` provider asks), served on
//! 127.0.0.1 by [`serve`] or [`serve_fixture`]: `initialize` (giving a session id),
//! `tools/call` for `execute_sql` and `get_frequency_allocation`, and `GET /health`.
//!
//! The register is a [`Fixture`]: a synthetic in-memory SQLite database with the service's
//! table and column names. `execute_sql` runs the statement it is given on it, for real
//! (`SELECT` and `WITH` only), and answers the service's envelope. `get_frequency_allocation`
//! answers from the fixture's `spectrum_allocations`. Every row a test hands it is synthetic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rusqlite::Connection;
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::{Value, json};

/// The columns of the rows [`serve`] and [`Fixture::from_rows`] take, in order; `CALL_SIGN`
/// is the last.
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
/// The `limit` of an `execute_sql` call that gives none.
const DEFAULT_LIMIT: usize = 1000;

/// The register's tables, with the service's table and column names.
const SCHEMA: &str = "
CREATE TABLE device_details (SDD_ID INTEGER PRIMARY KEY, LICENCE_NO TEXT, FREQUENCY INTEGER,
    BANDWIDTH INTEGER, EMISSION TEXT, DEVICE_TYPE TEXT, TRANSMITTER_POWER NUMERIC,
    TRANSMITTER_POWER_UNIT TEXT, EIRP REAL, EIRP_UNIT TEXT, SITE_ID TEXT, ANTENNA_ID TEXT,
    POLARISATION TEXT, AZIMUTH REAL, HEIGHT REAL, TILT REAL, CALL_SIGN TEXT,
    CLASS_OF_STATION_CODE TEXT, NATURE_OF_SERVICE_ID TEXT, HOURS_OF_OPERATION TEXT,
    AREA_DESCRIPTION TEXT, STATION_NAME TEXT);
CREATE TABLE site (SITE_ID TEXT PRIMARY KEY, LATITUDE NUMERIC, LONGITUDE NUMERIC, NAME TEXT,
    STATE TEXT, POSTCODE TEXT, SITE_PRECISION TEXT, ELEVATION REAL);
CREATE TABLE licence (LICENCE_NO TEXT PRIMARY KEY, CLIENT_NO INTEGER, SV_ID INTEGER,
    SS_ID INTEGER, LICENCE_TYPE_NAME TEXT, DATE_ISSUED TEXT, DATE_OF_EXPIRY TEXT,
    STATUS TEXT, STATUS_TEXT TEXT);
CREATE TABLE client (CLIENT_NO INTEGER PRIMARY KEY, LICENCEE TEXT, TRADING_NAME TEXT,
    CAT_ID INTEGER, CLIENT_TYPE_ID INTEGER);
CREATE TABLE client_type (TYPE_ID INTEGER PRIMARY KEY, NAME TEXT);
CREATE TABLE industry_cat (CAT_ID INTEGER PRIMARY KEY, DESCRIPTION TEXT, NAME TEXT);
CREATE TABLE licence_service (SV_ID INTEGER PRIMARY KEY, SV_NAME TEXT);
CREATE TABLE licence_subservice (SS_ID INTEGER PRIMARY KEY, SV_SV_ID INTEGER, SS_NAME TEXT);
CREATE TABLE antenna (ANTENNA_ID TEXT PRIMARY KEY, GAIN REAL, FRONT_TO_BACK REAL,
    H_BEAMWIDTH REAL, V_BEAMWIDTH REAL, ANTENNA_TYPE TEXT, MODEL TEXT);
CREATE TABLE class_of_station (CODE TEXT PRIMARY KEY, DESCRIPTION TEXT);
CREATE TABLE nature_of_service (CODE TEXT PRIMARY KEY, DESCRIPTION TEXT);
CREATE TABLE spectrum_allocations (freq_start_hz INTEGER, freq_end_hz INTEGER, unit TEXT,
    page INTEGER, services_json TEXT, footnotes_json TEXT, raw TEXT);
CREATE TABLE spectrum_australian_footnotes (footnote_ref TEXT, footnote_text TEXT, page INTEGER);
CREATE TABLE spectrum_international_footnotes (footnote_ref TEXT, footnote_text TEXT,
    page INTEGER);
CREATE TABLE link_pairs (id INTEGER, lat_a REAL, lon_a REAL, label_a TEXT, lat_b REAL,
    lon_b REAL, label_b TEXT, site_id_a TEXT, site_id_b TEXT, frequency_hz INTEGER,
    service TEXT, licensee TEXT);
CREATE VIEW v_links AS SELECT id, lat_a, lon_a, label_a, lat_b, lon_b, label_b, site_id_a,
    site_id_b, frequency_hz, service, licensee FROM link_pairs;
";

/// The services of the plan segment: the satellite variant, or the terrestrial one.
fn plan_services(satellite: bool) -> Value {
    if satellite {
        json!([{"name": "MOBILE-SATELLITE", "primary": true}])
    } else {
        json!([
            {"name": "FIXED", "primary": true, "inline_footnotes": []},
            {"name": "MOBILE", "primary": true, "qualifier": "except aeronautical mobile (R)"},
            {"name": "METEOROLOGICAL AIDS", "primary": false},
            {"name": "RADIOLOCATION", "primary": false, "inline_footnotes": ["AUS29"]}
        ])
    }
}

/// A JSON value as an SQLite parameter.
fn param(v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

/// An SQLite value as JSON: NULL is null, integers and reals numbers, text strings.
fn json_of(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(r) => json!(r),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!(format!("<{} bytes>", b.len())),
    }
}

/// A synthetic register in an in-memory SQLite database with the service's table and
/// column names.
pub struct Fixture {
    db: Mutex<Connection>,
}

impl std::fmt::Debug for Fixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fixture")
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::empty()
    }
}

impl Fixture {
    /// Every table, no rows but one plan segment (403-406 MHz) with one footnote.
    pub fn empty() -> Fixture {
        let db = Connection::open_in_memory().expect("an in-memory register");
        db.execute_batch(SCHEMA).expect("the register schema");
        db.execute(
            "INSERT INTO spectrum_allocations VALUES (?1, ?2, 'MHz', 50, ?3, '[]', '')",
            rusqlite::params![
                PLAN_HZ.0 as i64,
                PLAN_HZ.1 as i64,
                plan_services(false).to_string()
            ],
        )
        .expect("the plan segment");
        db.execute(
            "INSERT INTO spectrum_australian_footnotes VALUES ('AUS29', \
             'An example footnote for the plan segment.', 50)",
            [],
        )
        .expect("the plan footnote");
        Fixture { db: Mutex::new(db) }
    }

    /// A register holding `rows`, each an array in [`COLUMNS`] order. Each row gets its own
    /// site (`S<n>`), client and antenna (`A<n>`), and the first row of a licence number
    /// gets the licence.
    pub fn from_rows(rows: &[Value]) -> Fixture {
        let f = Fixture::empty();
        {
            let db = f.db.lock().unwrap();
            let run = |sql: &str, p: &[SqlValue]| {
                db.execute(sql, rusqlite::params_from_iter(p.iter()))
                    .expect("a fixture row");
            };
            for (n, row) in rows.iter().enumerate() {
                let c = |i: usize| param(&row[i]);
                let site = SqlValue::Text(format!("S{n}"));
                let antenna = SqlValue::Text(format!("A{n}"));
                let client = SqlValue::Integer(n as i64 + 1);
                run(
                    "INSERT INTO site (SITE_ID, LATITUDE, LONGITUDE, NAME) VALUES (?1, ?2, ?3, ?4)",
                    &[site.clone(), c(8), c(9), c(7)],
                );
                run(
                    "INSERT INTO client (CLIENT_NO, LICENCEE) VALUES (?1, ?2)",
                    &[client.clone(), c(10)],
                );
                run(
                    "INSERT OR IGNORE INTO licence (LICENCE_NO, CLIENT_NO) VALUES (?1, ?2)",
                    &[c(6), client],
                );
                run(
                    "INSERT INTO antenna (ANTENNA_ID) VALUES (?1)",
                    std::slice::from_ref(&antenna),
                );
                run(
                    "INSERT INTO device_details (FREQUENCY, BANDWIDTH, EMISSION, DEVICE_TYPE, \
                     TRANSMITTER_POWER, TRANSMITTER_POWER_UNIT, LICENCE_NO, SITE_ID, ANTENNA_ID, \
                     CALL_SIGN) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    &[
                        c(0),
                        c(1),
                        c(2),
                        c(3),
                        c(4),
                        c(5),
                        c(6),
                        site,
                        antenna,
                        c(11),
                    ],
                );
            }
        }
        f
    }

    /// Run `statement` (any SQL, several statements allowed) on the register: a test's own
    /// rows.
    pub fn sql(&self, statement: &str) {
        let db = self.db.lock().unwrap();
        db.execute_batch(statement)
            .unwrap_or_else(|e| panic!("fixture SQL {statement}: {e}"));
    }

    /// Drop `column` of `table`, as an older register lacks it.
    pub fn drop_column(&self, table: &str, column: &str) {
        self.sql(&format!("ALTER TABLE {table} DROP COLUMN {column}"));
    }

    /// Read at most `limit` rows of a `SELECT` or `WITH` statement: the columns, the rows,
    /// and whether more were available. A refusal or SQLite's error is the `Err` text.
    fn query(&self, sql: &str, limit: usize) -> Result<(Vec<String>, Vec<Value>, bool), String> {
        let head = sql.trim_start().to_ascii_uppercase();
        if !(head.starts_with("SELECT") || head.starts_with("WITH")) {
            return Err("only SELECT statements are allowed".to_string());
        }
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(sql).map_err(|e| e.to_string())?;
        if !stmt.readonly() {
            return Err("only SELECT statements are allowed".to_string());
        }
        let columns: Vec<String> = stmt.column_names().map(|c| c.to_string()).collect();
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            if out.len() >= limit {
                truncated = true;
                break;
            }
            out.push(Value::Array(
                (0..columns.len())
                    .map(|i| json_of(row.get_ref(i).expect("a column")))
                    .collect(),
            ));
        }
        Ok((columns, out, truncated))
    }

    /// The plan answer for `freq_hz`: the narrowest `spectrum_allocations` segment holding
    /// it, or no entry.
    fn plan(&self, freq_hz: Option<u64>, satellite: bool) -> Value {
        let none = json!({"match_count": 0, "allocation": null, "regions": {}});
        let Some(freq_hz) = freq_hz else {
            return none;
        };
        let db = self.db.lock().unwrap();
        let found = db.query_row(
            "SELECT freq_start_hz, freq_end_hz, unit, page, services_json, footnotes_json \
             FROM spectrum_allocations WHERE freq_start_hz <= ?1 AND freq_end_hz >= ?1 \
             ORDER BY freq_end_hz - freq_start_hz LIMIT 1",
            [freq_hz as i64],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        );
        let Ok((start, end, unit, page, services, footnotes)) = found else {
            return none;
        };
        let services = if satellite {
            plan_services(true)
        } else {
            serde_json::from_str(&services).unwrap_or(Value::Null)
        };
        let footnotes: Value = serde_json::from_str(&footnotes).unwrap_or(json!([]));
        json!({"match_count": 1, "allocation": {"freq_start_hz": start, "freq_end_hz": end,
            "unit": unit, "page": page, "services": services, "footnotes": footnotes,
            "raw": ""}, "regions": {}})
    }
}

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
    /// The same register wrapping the rejection as a JSON-RPC error that names no
    /// column (`query failed`).
    NoCallSignColumnGeneric,
    /// The same register answering the rejection as a JSON error object in the tool's
    /// text (`{"error":"query failed"}`).
    NoCallSignColumnJson,
    /// Every `execute_sql` call is a JSON-RPC error (`query failed`), with or without
    /// `CALL_SIGN`; the plan still answers.
    SqlFails,
}

/// What the stub answers from, and what it saw.
#[derive(Debug)]
pub struct Stub {
    pub mode: Mode,
    /// The register `execute_sql` runs on.
    pub fixture: Fixture,
    /// Tool calls seen.
    pub calls: AtomicUsize,
    /// `get_frequency_allocation` calls seen.
    pub plans: AtomicUsize,
    /// The last `execute_sql` arguments.
    pub sql: Mutex<Option<Value>>,
    /// `execute_sql` calls seen.
    pub queries: AtomicUsize,
    /// `execute_sql` calls rejected for naming a missing `CALL_SIGN` (every call under
    /// [`Mode::SqlFails`]).
    pub rejected: AtomicUsize,
    /// Kept for tests that assert the stub never had to guess a band: the stub runs the
    /// SQL for real now, so it stays 0.
    pub unbanded: AtomicUsize,
    /// The body of `GET /health`.
    pub health: Mutex<Value>,
    /// An `execute_sql` whose text contains this answers an error.
    pub fail_matching: Mutex<Option<String>>,
}

fn sse(v: Value) -> Response {
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!("event: message\ndata: {v}\n\n"),
    )
        .into_response()
}

/// A tool error answering `error` as text.
fn tool_error(id: &Value, error: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,
        "result":{"isError":true,"content":[{"type":"text","text":format!("SQL error: {error}")}]}})
}

/// The `execute_sql` answer to `args`, or the whole JSON-RPC message that is an error.
fn execute_sql(s: &Stub, id: &Value, args: &Value) -> Result<Value, Value> {
    *s.sql.lock().unwrap() = Some(args.clone());
    s.queries.fetch_add(1, Ordering::SeqCst);
    let sql = args["sql"].as_str().unwrap_or_default();
    let limit = args["limit"].as_u64().map_or(DEFAULT_LIMIT, |n| n as usize);
    let older = matches!(
        s.mode,
        Mode::NoCallSignColumnRpc
            | Mode::NoCallSignColumnTool
            | Mode::NoCallSignColumnGeneric
            | Mode::NoCallSignColumnJson
    );
    if s.fail_matching
        .lock()
        .unwrap()
        .as_deref()
        .is_some_and(|m| sql.contains(m))
    {
        return Err(tool_error(
            id,
            "query failed (the stub was told to fail it)",
        ));
    }
    let answer = if s.mode == Mode::SqlFails {
        Err("no such column: d.CALL_SIGN".to_string())
    } else {
        s.fixture.query(sql, limit)
    };
    let (mut columns, mut rows, truncated) = match answer {
        Ok(a) => a,
        Err(error) if s.mode == Mode::SqlFails || (older && error.contains("CALL_SIGN")) => {
            s.rejected.fetch_add(1, Ordering::SeqCst);
            return Err(match s.mode {
                Mode::NoCallSignColumnRpc => json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32603,"message":error}}),
                Mode::NoCallSignColumnGeneric | Mode::SqlFails => json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32603,"message":"query failed"}}),
                Mode::NoCallSignColumnJson => json!({"jsonrpc":"2.0","id":id,
                    "result":{"content":[{"type":"text",
                        "text":json!({"error":"query failed"}).to_string()}]}}),
                _ => tool_error(id, &error),
            });
        }
        Err(error) => return Err(tool_error(id, &error)),
    };
    if s.mode == Mode::NoCallSign
        && let Some(i) = columns.iter().position(|c| c == "CALL_SIGN")
    {
        columns.remove(i);
        for row in &mut rows {
            if let Some(r) = row.as_array_mut() {
                r.remove(i);
            }
        }
    }
    let n = rows.len();
    Ok(
        json!({"columns": columns, "rows": rows, "truncated": truncated, "rowCount": n,
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
                "get_frequency_allocation" => s
                    .fixture
                    .plan(args["freq_hz"].as_u64(), s.mode == Mode::Satellite),
                t => panic!("unexpected tool {t}"),
            };
            sse(json!({"jsonrpc":"2.0","id":id,
                "result":{"content":[{"type":"text","text":out.to_string()}]}}))
        }
        m => panic!("unexpected method {m}"),
    }
}

/// Serve the stub on 127.0.0.1 (a task on the current tokio runtime, serving as long as
/// that runs) running its queries on `fixture`; its `http://HOST:PORT` and what it sees.
/// The modes that model an older register (`NoCallSign*`) drop `CALL_SIGN` from the
/// fixture's `device_details` first, and `NullBandwidth` blanks the first row's bandwidth.
pub async fn serve_fixture(fixture: Fixture, mode: Mode) -> (String, Arc<Stub>) {
    if matches!(
        mode,
        Mode::NoCallSignColumnRpc
            | Mode::NoCallSignColumnTool
            | Mode::NoCallSignColumnGeneric
            | Mode::NoCallSignColumnJson
    ) {
        fixture.drop_column("device_details", "CALL_SIGN");
    }
    if mode == Mode::NullBandwidth {
        fixture.sql(
            "UPDATE device_details SET BANDWIDTH = NULL \
             WHERE SDD_ID = (SELECT MIN(SDD_ID) FROM device_details)",
        );
    }
    let stub = Arc::new(Stub {
        mode,
        fixture,
        calls: AtomicUsize::default(),
        plans: AtomicUsize::default(),
        sql: Mutex::default(),
        queries: AtomicUsize::default(),
        rejected: AtomicUsize::default(),
        unbanded: AtomicUsize::default(),
        health: Mutex::new(json!({"status": "ok", "dataAsOf": "2026-01-01T00:00:00.000Z"})),
        fail_matching: Mutex::default(),
    });
    let app = Router::new()
        .route("/mcp", post(mcp))
        .route(
            "/health",
            get(|State(s): State<Arc<Stub>>| async move {
                let body = s.health.lock().unwrap().to_string();
                ([(header::CONTENT_TYPE, "application/json")], body)
            }),
        )
        .with_state(stub.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, stub)
}

/// [`serve_fixture`] on the register [`Fixture::from_rows`] builds from `rows`, each an
/// array in [`COLUMNS`] order.
pub async fn serve(rows: Vec<Value>, mode: Mode) -> (String, Arc<Stub>) {
    serve_fixture(Fixture::from_rows(&rows), mode).await
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
    fn the_plan_has_an_entry_only_inside_its_band() {
        let f = Fixture::empty();
        assert_eq!(f.plan(Some(404_025_000), false)["match_count"], 1);
        assert!(f.plan(Some(100_000_000), false)["allocation"].is_null());
        assert!(f.plan(None, false)["allocation"].is_null());
    }
}
