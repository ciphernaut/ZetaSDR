//! `zeta registers check [PROVIDER] [--center-hz HZ] [--bandwidth-hz HZ]`: run every
//! register query (the built-in ones and the operator's `--registers-dir`) against the
//! configured register with sample values, then one whole answer, and print one line
//! each. Local only, and no node is opened: it needs only the register (`--knowledge-acma`)
//! and the station, read from the state directory when one is set there.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use zeta_core::knowledge::KnowledgeProvider;
use zeta_core::register::{ParamKind, ParamValue, RegisterQuery, RegisterState, Signal};
use zeta_ext::acma::{ACMA_PROVIDER, ACMA_REGION, Row};
use zeta_ext::mcp_client::McpClient;

use crate::node_setup;

/// The query whose first row gives the other queries their register ids.
const CANDIDATES: &str = "candidates";

/// How one query's run ended.
enum Outcome {
    Ok {
        rows: usize,
        ms: u128,
    },
    Fail(String),
    /// Not run, and nothing wrong: the band has no candidate to take a register id from.
    Quiet(String),
    /// Not run, and the check cannot say the query works: a defect to fix.
    Skipped(String),
}

/// Check the register's queries (only `only`'s, when given) at `signal` and print
/// `<provider>/<id>: ok <rows> rows <ms> ms`, `... fail: <reason>` or
/// `... skipped: <reason>` for each, then `<provider>: answer <ms> ms of <deadline> ms`
/// for one whole answer from the station (from nowhere without one), and, after the
/// queries, `<provider>: skipped <n> of <total> queries` when any was skipped (a skip
/// is no failure only when the band is quiet: no candidate to take a register id from; any
/// other skip, a query the check cannot run, fails). A register that does
/// not answer its health read is `<provider>: unreachable: <reason>` alone. Exit 0 iff
/// nothing failed; with no register configured, print `no register configured` and fail.
pub async fn run(
    state_dir: &Path,
    acma: Option<&crate::sources::AcmaFlag>,
    registers_dir: Option<&Path>,
    only: Option<&str>,
    signal: Signal,
) -> ExitCode {
    let Some(flag) = acma else {
        println!("no register configured");
        return ExitCode::FAILURE;
    };
    if let Some(p) = only.filter(|p| *p != ACMA_PROVIDER) {
        println!("no register {p} configured (configured: {ACMA_PROVIDER})");
        return ExitCode::FAILURE;
    }
    let queries = match node_setup::register_queries(registers_dir) {
        Ok(q) => Arc::new(q),
        Err(e) => {
            println!("registers: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (station, unreadable) = zeta_core::station::load(state_dir);
    if let Some(e) = unreadable {
        eprintln!("zeta: {e}; checking from no station");
    }
    let client = match McpClient::new(&flag.url, node_setup::ACMA_CLIENT_TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            println!("{ACMA_PROVIDER}: unreachable: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = client.health().await {
        println!("{ACMA_PROVIDER}: unreachable: {e}");
        return ExitCode::FAILURE;
    }
    let at = station.clone();
    let provider =
        node_setup::acma_provider(client, flag, Arc::new(move || at.clone()), queries.clone());
    let sample = provider.signal_values(&signal, station.as_ref());

    // The candidates first: the queries that take a register id take it from them.
    let mut rows = queries.for_provider(ACMA_PROVIDER);
    rows.sort_by_key(|q| q.id != CANDIDATES);
    let mut failed = false;
    let (mut skipped, mut quiet, mut total) = (0usize, 0usize, 0usize);
    let mut candidates: Option<Result<Vec<Row>, String>> = None;
    for q in rows {
        let outcome = match values_for(q, &sample, candidates.as_ref()) {
            Err(Skip::Quiet(why)) => Outcome::Quiet(why),
            Err(Skip::Defect(why)) => Outcome::Skipped(why),
            Ok(values) => {
                let t = Instant::now();
                let result = provider.run_query(&q.id, &values).await;
                let ms = t.elapsed().as_millis();
                let outcome = match &result {
                    Ok(r) => Outcome::Ok { rows: r.len(), ms },
                    Err(e) => Outcome::Fail(e.clone()),
                };
                if q.id == CANDIDATES {
                    candidates = Some(result);
                }
                outcome
            }
        };
        let name = format!("{ACMA_PROVIDER}/{}", q.id);
        total += 1;
        match outcome {
            Outcome::Ok { rows, ms } => println!("{name}: ok {rows} rows {ms} ms"),
            Outcome::Fail(why) => {
                failed = true;
                println!("{name}: fail: {why}");
            }
            Outcome::Quiet(why) => {
                quiet += 1;
                println!("{name}: skipped: {why}");
            }
            Outcome::Skipped(why) => {
                skipped += 1;
                failed = true;
                println!("{name}: skipped: {why}");
            }
        }
    }
    if skipped + quiet > 0 {
        println!(
            "{ACMA_PROVIDER}: skipped {} of {total} queries ({quiet} quiet band, {skipped} not runnable)",
            skipped + quiet
        );
    }

    let t = Instant::now();
    let answer = provider
        .describe(ACMA_REGION, &signal, station.as_ref())
        .await;
    let ms = t.elapsed().as_millis();
    let deadline = provider.timeout().as_millis();
    let status = match answer.as_ref().map(|a| (a.status.state, &a.status)) {
        Some((RegisterState::Ok, _)) => String::new(),
        Some((RegisterState::Partial, s)) => {
            let names: Vec<&str> = s.failed.iter().map(|f| f.query.as_str()).collect();
            format!("; partial: {}", names.join(", "))
        }
        Some((RegisterState::Unreachable, s)) => {
            format!("; unreachable: {}", s.reason.as_deref().unwrap_or_default())
        }
        None => format!("; no answer for region {ACMA_REGION}"),
    };
    if !status.is_empty() {
        failed = true;
    }
    println!("{ACMA_PROVIDER}: answer {ms} ms of {deadline} ms{status}");
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Why a query is skipped.
enum Skip {
    /// The band has no candidate row to take a register id from: no failure.
    Quiet(String),
    /// The check cannot run the query (a candidates row without the column, a
    /// parameter with no sample value, no candidates query, a failed one): a failure.
    Defect(String),
}

/// The values `q` runs with: the sample signal's, and for a query that takes a register
/// id, every parameter the first candidates row holding all its ids names (its
/// `licence_no`, `site_id` and `center_hz`). `Err` says why the query is skipped.
fn values_for<'a>(
    q: &'a RegisterQuery,
    sample: &BTreeMap<&'static str, ParamValue>,
    candidates: Option<&Result<Vec<Row>, String>>,
) -> Result<BTreeMap<&'a str, ParamValue>, Skip> {
    let mut values: BTreeMap<&str, ParamValue> =
        sample.iter().map(|(k, v)| (*k, v.clone())).collect();
    let ids: Vec<&str> = q
        .params
        .iter()
        .filter(|p| p.kind == ParamKind::Id)
        .map(|p| p.name.as_str())
        .collect();
    if !ids.is_empty() {
        let wanted = ids.join(", ");
        let rows = match candidates {
            None => {
                return Err(Skip::Defect(format!(
                    "no {CANDIDATES} run to take {wanted} from"
                )));
            }
            Some(Err(_)) => {
                return Err(Skip::Defect(format!(
                    "the {CANDIDATES} query failed: no {wanted}"
                )));
            }
            Some(Ok(rows)) if rows.is_empty() => {
                return Err(Skip::Quiet(format!(
                    "the {CANDIDATES} query returned no {wanted}"
                )));
            }
            Some(Ok(rows)) => rows,
        };
        let row = rows
            .iter()
            .find(|r| ids.iter().all(|id| id_of(r, id).is_some()))
            .ok_or_else(|| Skip::Defect(format!("no {CANDIDATES} row holds {wanted}")))?;
        for p in &q.params {
            let v = match p.kind {
                ParamKind::Id => id_of(row, &p.name).map(ParamValue::Id),
                ParamKind::Number => number_of(row, &p.name),
            };
            if let Some(v) = v {
                values.insert(p.name.as_str(), v);
            }
        }
    }
    match q
        .params
        .iter()
        .find(|p| !values.contains_key(p.name.as_str()))
    {
        Some(p) => Err(Skip::Defect(format!("no sample value for `{}`", p.name))),
        None => Ok(values),
    }
}

/// A cell as a register id's text: trimmed text, or a number's digits.
fn id_of(r: &Row, k: &str) -> Option<String> {
    match r.get(k)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A cell as a query number: an integer as is, else a finite number.
fn number_of(r: &Row, k: &str) -> Option<ParamValue> {
    match r.get(k)? {
        Value::Number(n) => n
            .as_i64()
            .map(ParamValue::Int)
            .or_else(|| n.as_f64().map(ParamValue::Number)),
        Value::String(s) => s.trim().parse().ok().map(ParamValue::Number),
        _ => None,
    }
    .filter(|v| !matches!(v, ParamValue::Number(x) if !x.is_finite()))
}
