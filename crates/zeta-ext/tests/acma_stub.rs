//! The register stub runs real SQL on a synthetic in-memory SQLite register.

use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use zeta_ext::testing::acma_stub::{self, Fixture, Mode};

/// One MCP `tools/call` against the stub: the JSON-RPC message answered.
async fn call(url: &str, tool: &str, args: Value) -> Value {
    let http = reqwest::Client::new();
    let init = http
        .post(format!("{url}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .send()
        .await
        .unwrap();
    let session = init.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_string();
    let text = http
        .post(format!("{url}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", session)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":tool,"arguments":args}}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let data = text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
    serde_json::from_str(data).unwrap()
}

/// The tool's text content, parsed as JSON.
fn content(message: &Value) -> Value {
    serde_json::from_str(message["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn two_licences() -> Fixture {
    let f = Fixture::empty();
    f.sql(
        "INSERT INTO client (CLIENT_NO, LICENCEE) VALUES (1, 'Example Water Authority'), \
         (2, 'Example Rail Operator'); \
         INSERT INTO licence (LICENCE_NO, CLIENT_NO) VALUES ('9000001/1', 1), ('9000002/1', 2), \
         ('9000003/1', 2)",
    );
    f
}

#[tokio::test]
async fn execute_sql_runs_a_select_on_the_fixture_and_answers_columns_and_rows() {
    let (url, stub) = acma_stub::serve_fixture(two_licences(), Mode::Normal).await;
    let all = call(
        &url,
        "execute_sql",
        json!({"sql": "SELECT LICENCE_NO, CLIENT_NO FROM licence ORDER BY LICENCE_NO",
            "limit": 10}),
    )
    .await;
    let all = content(&all);
    assert_eq!(
        json!({"columns": all["columns"], "rows": all["rows"], "rowCount": all["rowCount"],
            "truncated": all["truncated"]}),
        json!({"columns": ["LICENCE_NO", "CLIENT_NO"],
            "rows": [["9000001/1", 1], ["9000002/1", 2], ["9000003/1", 2]],
            "rowCount": 3, "truncated": false})
    );
    let cut = content(
        &call(
            &url,
            "execute_sql",
            json!({"sql": "SELECT LICENCE_NO, NULL AS EMPTY_COLUMN FROM licence ORDER BY LICENCE_NO",
                "limit": 2}),
        )
        .await,
    );
    assert_eq!(cut["rowCount"], 2);
    assert_eq!(cut["truncated"], true);
    assert_eq!(cut["rows"][1], json!(["9000002/1", null]));
    assert_eq!(stub.queries.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_write_statement_is_refused() {
    let (url, _) = acma_stub::serve_fixture(two_licences(), Mode::Normal).await;
    let w = call(
        &url,
        "execute_sql",
        json!({"sql": "DELETE FROM licence", "limit": 10}),
    )
    .await;
    assert_eq!(w["result"]["isError"], true);
    let n = content(
        &call(
            &url,
            "execute_sql",
            json!({"sql": "SELECT COUNT(*) AS N FROM licence", "limit": 10}),
        )
        .await,
    );
    assert_eq!(n["rows"], json!([[3]]));
}

#[tokio::test]
async fn a_dropped_column_makes_the_query_an_error_naming_it() {
    let f = two_licences();
    f.drop_column("device_details", "CALL_SIGN");
    let (url, _) = acma_stub::serve_fixture(f, Mode::Normal).await;
    let m = call(
        &url,
        "execute_sql",
        json!({"sql": "SELECT d.CALL_SIGN FROM device_details d", "limit": 10}),
    )
    .await;
    assert_eq!(m["result"]["isError"], true);
    assert!(
        m["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("CALL_SIGN")
    );
}

#[tokio::test]
async fn health_reports_the_data_date() {
    let (url, stub) = acma_stub::serve_fixture(Fixture::empty(), Mode::Normal).await;
    let get = || async {
        reqwest::get(format!("{url}/health"))
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    };
    let h = get().await;
    assert_eq!(h["status"], "ok");
    assert_eq!(h["dataAsOf"], "2026-01-01T00:00:00.000Z");
    *stub.health.lock().unwrap() = json!({"status": "ok", "dataAsOf": "2026-02-03T00:00:00.000Z"});
    assert_eq!(get().await["dataAsOf"], "2026-02-03T00:00:00.000Z");
}

#[tokio::test]
async fn fail_matching_answers_an_error_for_a_query_containing_it() {
    let (url, stub) = acma_stub::serve_fixture(two_licences(), Mode::Normal).await;
    *stub.fail_matching.lock().unwrap() = Some("FROM client".to_string());
    let m = call(
        &url,
        "execute_sql",
        json!({"sql": "SELECT * FROM client", "limit": 10}),
    )
    .await;
    assert_eq!(m["result"]["isError"], true);
}

#[tokio::test]
async fn from_rows_gives_each_row_a_site_a_licence_and_a_client() {
    let rows = vec![
        json!([
            404025000,
            25000,
            "16K0F2D",
            "T",
            1,
            "W",
            "9000001/1",
            "Example Site A",
            -27.95,
            153.2,
            "Example Water Authority",
            "VK9XAA"
        ]),
        json!([
            404050000,
            null,
            "16K0F2D",
            "T",
            2,
            "W",
            "9000002/1",
            "Example Site B",
            -27.9,
            153.1,
            "Example Rail Operator",
            null
        ]),
    ];
    let (url, _) = acma_stub::serve_fixture(Fixture::from_rows(&rows), Mode::Normal).await;
    let m = content(
        &call(
            &url,
            "execute_sql",
            json!({"sql": "SELECT d.FREQUENCY, d.BANDWIDTH, d.EMISSION, d.DEVICE_TYPE, \
                d.TRANSMITTER_POWER, d.TRANSMITTER_POWER_UNIT, d.LICENCE_NO, s.NAME, \
                s.LATITUDE, s.LONGITUDE, c.LICENCEE, d.CALL_SIGN \
                FROM device_details d JOIN site s ON s.SITE_ID = d.SITE_ID \
                LEFT JOIN licence l ON l.LICENCE_NO = d.LICENCE_NO \
                LEFT JOIN client c ON c.CLIENT_NO = l.CLIENT_NO \
                JOIN antenna a ON a.ANTENNA_ID = d.ANTENNA_ID ORDER BY d.FREQUENCY",
                "limit": 10}),
        )
        .await,
    );
    assert_eq!(m["rows"], json!(rows));
}
