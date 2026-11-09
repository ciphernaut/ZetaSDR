//! The agent loop: model turns, tool calls applied through an executor, errors fed back.

use crate::exec::{Executor, to_json, trim_result};
use crate::llm::{AgentError, LlmClient, LlmConfig};
use crate::page::{PageTools, VIEW_SET, view_set, view_set_tool};
use crate::radio::{Caller, RadioContext, Selection, resolve};
use crate::wire::{ChatMessage, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use zeta_client::{ToolDef, op_from_call, op_tools, system_prompt};
use zeta_core::ops::{OpError, SourceInfo};

/// What a run may do beyond reading and starting: stopping jobs is the
/// operator's call, as are deleting captures and answering signature proposals, so
/// every switch is off unless the run is allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Allow {
    /// `job_stop` may be applied.
    pub stop_jobs: bool,
    /// A non-empty `replace` may be sent (it stops the named jobs).
    pub replace: bool,
    /// `capture_delete` may be applied.
    pub delete_captures: bool,
    /// `signature_accept` and `signature_reject` may be applied.
    pub accept_signatures: bool,
}

/// The operations that answer a signature proposal: the operator's decision.
pub const GATED_SIGNATURE_OPS: &[&str] = &["signature_accept", "signature_reject"];

/// The operations whose `replace` stops jobs to make room.
pub const GATED_REPLACE_OPS: &[&str] = &[
    "job_start",
    "job_retune",
    "source_ensure",
    "source_pin",
    "plan_start",
];

/// `Some(error)` when `op` would stop jobs and the run is not allowed to:
/// a `job_stop`, or a gated op with a non-empty `replace`; likewise a
/// `capture_delete` without `delete_captures`, and a `signature_accept` or
/// `signature_reject` without `accept_signatures`. The executor is
/// never reached; the error goes back to the model as the call's result.
pub fn gate(op: &Value, allow: Allow) -> Option<Value> {
    // `op_from_call` always inserts `"op"`, so a value without a name never comes from a call.
    let name = op["op"].as_str()?;
    if name == "capture_delete" {
        if allow.delete_captures {
            return None;
        }
        return Some(json!({
            "error": "not_allowed",
            "field": "op",
            "op": name,
            "message": "deleting captures is not allowed in this run: report it to the operator; they unlock it by allowing capture deletes for the run",
        }));
    }
    if GATED_SIGNATURE_OPS.contains(&name) {
        if allow.accept_signatures {
            return None;
        }
        return Some(json!({
            "error": "not_allowed",
            "field": "op",
            "op": name,
            "message": "accepting or rejecting a signature is the operator's decision: report the proposal to the operator; they unlock it by allowing signatures for the run",
        }));
    }
    let field = if name == "job_stop" || name == "jobs_stop" {
        if allow.stop_jobs {
            return None;
        }
        "op"
    } else if GATED_REPLACE_OPS.contains(&name)
        && op["replace"].as_array().is_some_and(|r| !r.is_empty())
    {
        if allow.replace {
            return None;
        }
        "replace"
    } else {
        return None;
    };
    Some(json!({
        "error": "not_allowed",
        "field": field,
        "op": name,
        "message": "stopping jobs is not allowed in this run: report the conflict and its holders to the operator; they unlock it with the retry button or by allowing stops for the run",
    }))
}

/// The jobs one run started itself, which it may stop without the run
/// being allowed stops: a `measure` it read, an `identify` it is done
/// with. Per run, never shared: a later run starts empty.
#[derive(Default)]
pub struct OwnJobs {
    /// Jobs a successful `job_start`, `job_attach`, `graph_start`, `bits_start` or
    /// `plan_start` answered, and the `verdict.job` of an own `identify` once its
    /// context was read.
    jobs: HashSet<String>,
    /// The own `identify` jobs, whose context names the job that proved.
    identifies: HashSet<String>,
}

impl OwnJobs {
    /// Whether `op` is a `job_stop` of one of this run's own jobs. `jobs_stop` never is,
    /// and a `job_stop` has no `replace`, which stays with [`gate`].
    pub fn allows_stop(&self, op: &Value) -> bool {
        op["op"] == "job_stop" && op["job"].as_str().is_some_and(|j| self.jobs.contains(j))
    }

    /// Note what a call that succeeded gave this run; `result` is `{"result", "value"}`
    /// as the executor answers. A plan's `existing` jobs are not the run's.
    pub fn record(&mut self, tool: &str, arguments: &Value, result: &Value) {
        let value = &result["value"];
        match tool {
            "job_start" | "job_attach" | "graph_start" | "bits_start" => {
                if let Some(id) = value.as_str() {
                    self.jobs.insert(id.to_string());
                    if tool == "job_start" && arguments["kind"] == "identify" {
                        self.identifies.insert(id.to_string());
                    }
                }
            }
            "plan_start" => {
                for s in value["started"].as_array().into_iter().flatten() {
                    for key in ["job", "attached"] {
                        if let Some(id) = s[key].as_str() {
                            self.jobs.insert(id.to_string());
                        }
                    }
                }
            }
            "job_context" => {
                let own = arguments["job"]
                    .as_str()
                    .is_some_and(|j| self.identifies.contains(j));
                if let (true, Some(id)) = (own, value["identify"]["verdict"]["job"].as_str()) {
                    self.jobs.insert(id.to_string());
                }
            }
            _ => {}
        }
    }
}

pub struct AgentConfig {
    pub system_prompt: String,
    pub tools: Vec<ToolDef>,
    pub max_turns: u32,
    /// What the run may stop; nothing by default.
    pub allow: Allow,
    /// The page-only tools (`view_set`), on only for a run the page asked for.
    pub page: Option<PageTools>,
    /// The request's radio context; [`RadioContext::default`] is none at all.
    pub radio: RadioContext,
    /// Who asked.
    pub caller: Caller,
}

impl AgentConfig {
    /// `max_turns` defaults to 8, `radio` to no context and `caller` to the CLI.
    pub fn new(system_prompt: impl Into<String>, tools: Vec<ToolDef>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            tools,
            max_turns: 8,
            allow: Allow::default(),
            page: None,
            radio: RadioContext::default(),
            caller: Caller::cli(),
        }
    }

    /// Offer the page-only tools (`view_set`), `themes` being the node's ready theme ids.
    pub fn enable_page_tools(&mut self, themes: Vec<String>) {
        if self.page.is_none() {
            self.tools.push(view_set_tool());
        }
        self.page = Some(PageTools { themes });
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CallRecord {
    pub tool: String,
    pub arguments: Value,
    pub ok: bool,
    pub result: Value,
}

/// One model completion: what it thought and said, and how many calls it made.
#[derive(Clone, Debug, Serialize)]
pub struct TurnEntry {
    /// 1-based completion number.
    pub turn: u32,
    /// The model's reasoning, shown to the operator and never sent back.
    pub reasoning: Option<String>,
    /// The content as the model sent it; `None` when empty after trimming.
    pub content: Option<String>,
    /// Tool calls in the completion.
    pub calls: usize,
}

#[derive(Debug, Serialize)]
pub struct Transcript {
    pub request: String,
    pub answer: Option<String>,
    pub calls: Vec<CallRecord>,
    /// How many completions were requested.
    pub turns: u32,
    /// One entry per completion, in order.
    pub turn_log: Vec<TurnEntry>,
    /// What the run was allowed to stop.
    pub allow: Allow,
    /// The run's radio selection: `Some` once the pre-read `source_list` succeeded.
    pub selection: Option<Selection>,
}

/// Why a run stopped, with everything done up to that point.
#[derive(Debug, Serialize, thiserror::Error)]
#[error("{error}")]
pub struct AgentFailure {
    #[serde(flatten)]
    pub error: AgentError,
    pub transcript: Transcript,
}

const QUESTION_WORDS: &[&str] = &[
    "what", "which", "where", "when", "who", "whom", "whose", "why", "how", "is", "are", "was",
    "were", "does", "did", "can", "could", "will", "would", "should", "has", "have", "any",
];

/// Whether `request` asks something rather than commands something: it ends
/// with `?`, or its first word is a question word. `can`, `could`, `will` and
/// `would` followed by `you` make a polite request, not a question.
pub fn is_question(request: &str) -> bool {
    let text = request.trim();
    if text.ends_with('?') {
        return true;
    }
    let mut words = text.split_whitespace().map(|w| {
        w.trim_matches(|c: char| c.is_ascii_punctuation())
            .to_lowercase()
    });
    let word = words.next().unwrap_or_default();
    if matches!(word.as_str(), "can" | "could" | "will" | "would")
        && words.next().as_deref() == Some("you")
    {
        return false;
    }
    QUESTION_WORDS.contains(&word.as_str())
}

/// The agent `zeta ask` and the node's `POST /ask` both run: tools from the
/// node's `schema`, the system prompt from its guide `agents_md`. `page`
/// (the node's ready theme ids) adds the page-only tools, for a run the web
/// page asked for; `zeta ask` passes `None`. `radio` and `caller` pick the run's
/// default radio (see [`crate::radio`]).
#[allow(clippy::too_many_arguments)]
pub fn build_agent<E: Executor>(
    llm: LlmConfig,
    exec: E,
    schema: &Value,
    agents_md: &str,
    max_turns: u32,
    allow: Allow,
    page: Option<PageTools>,
    radio: RadioContext,
    caller: Caller,
) -> Result<Agent<E>, AgentError> {
    let llm = LlmClient::new(llm)?;
    let mut cfg = AgentConfig::new(system_prompt(agents_md), op_tools(schema, agents_md));
    cfg.max_turns = max_turns;
    cfg.allow = allow;
    cfg.radio = radio;
    cfg.caller = caller;
    if let Some(page) = page {
        cfg.enable_page_tools(page.themes);
    }
    Ok(Agent::new(llm, exec, cfg))
}

pub struct Agent<E: Executor> {
    llm: LlmClient,
    exec: E,
    cfg: AgentConfig,
}

impl<E: Executor> Agent<E> {
    pub fn new(llm: LlmClient, exec: E, cfg: AgentConfig) -> Self {
        Self { llm, exec, cfg }
    }

    /// Run `request` to an answer or a failure.
    pub async fn run(&self, request: &str) -> Result<Transcript, AgentFailure> {
        let mut transcript = Transcript {
            request: request.to_string(),
            answer: None,
            calls: Vec::new(),
            turns: 0,
            turn_log: Vec::new(),
            allow: self.cfg.allow,
            selection: None,
        };
        match self.turns(&mut transcript).await {
            Ok(()) => Ok(transcript),
            Err(error) => Err(AgentFailure { error, transcript }),
        }
    }

    async fn turns(&self, t: &mut Transcript) -> Result<(), AgentError> {
        self.llm.wait_ready().await?;
        let section = self.select(t).await?;
        let mut messages = vec![
            ChatMessage::system(format!("{}\n{section}", self.cfg.system_prompt)),
            ChatMessage::user(t.request.clone()),
        ];
        let mut own = OwnJobs::default();
        while t.turns < self.cfg.max_turns {
            t.turns += 1;
            let c = self.llm.complete(&messages, &self.cfg.tools).await?;
            t.turn_log.push(TurnEntry {
                turn: t.turns,
                reasoning: c.reasoning.clone(),
                content: c
                    .content
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string),
                calls: c.tool_calls.len(),
            });
            if c.tool_calls.is_empty() {
                // Design 10: prose is an answer only to a question (see
                // `is_question`: a polite request is not one) or after
                // tool calls; it is never parsed for operations.
                let content = c.content.unwrap_or_default();
                let text = content.trim();
                if !text.is_empty() && (!t.calls.is_empty() || is_question(&t.request)) {
                    t.answer = Some(text.to_string());
                    return Ok(());
                }
                return Err(AgentError::NoToolCall { content });
            }
            // `reasoning_content` is never sent back.
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: c.content,
                tool_calls: c.tool_calls.clone(),
                tool_call_id: None,
            });
            for call in &c.tool_calls {
                let record = self.call(call, &mut own, t.selection.as_mut()).await?;
                // Compact JSON, as `serde_json::to_string` writes it.
                let content = record.result.to_string();
                messages.push(ChatMessage::tool(call.id.clone(), content));
                t.calls.push(record);
            }
        }
        Err(AgentError::TurnLimit {
            turns: self.cfg.max_turns,
        })
    }

    /// The pre-read: one `source_list` through the executor, hidden from the model and
    /// from `calls`, resolved into `t.selection`. Returns the `## This request` section.
    /// A node error aborts the run; an op error (or a value that is not a source list)
    /// leaves `selection` `None` and the no-context paragraph.
    async fn select(&self, t: &mut Transcript) -> Result<String, AgentError> {
        let sources = match self.exec.apply(json!({"op": "source_list"})).await? {
            Ok(result) => serde_json::from_value::<Vec<SourceInfo>>(result["value"].clone()).ok(),
            Err(_) => None,
        };
        let Some(sources) = sources else {
            let fallback = resolve(
                &t.request,
                &RadioContext::default(),
                &[],
                self.cfg.caller.clone(),
            );
            return Ok(fallback.prompt_section());
        };
        let selection = resolve(
            &t.request,
            &self.cfg.radio,
            &sources,
            self.cfg.caller.clone(),
        );
        let section = selection.prompt_section();
        t.selection = Some(selection);
        Ok(section)
    }

    /// Apply one tool call. Bad arguments and unknown tools are the model's
    /// fault: they become the call's result and never reach the node. A
    /// page-only tool is answered here, never gated and never applied.
    /// A radio op the selection refuses is recorded `ok: false` and never applied.
    async fn call(
        &self,
        call: &ToolCall,
        own: &mut OwnJobs,
        selection: Option<&mut Selection>,
    ) -> Result<CallRecord, AgentError> {
        let f = &call.function;
        if let (Some(page), VIEW_SET) = (&self.cfg.page, f.name.as_str()) {
            let (arguments, result) = match f.args() {
                Err(message) => (
                    f.arguments.clone(),
                    Err(to_json(&OpError::Other { message })),
                ),
                Ok(args) => {
                    let result = view_set(page, &args);
                    (args, result)
                }
            };
            let ok = result.is_ok();
            return Ok(CallRecord {
                tool: f.name.clone(),
                arguments,
                ok,
                result: result.unwrap_or_else(|e| e),
            });
        }
        let (arguments, outcome) = match f.args() {
            Err(message) => (f.arguments.clone(), Err(OpError::Other { message })),
            Ok(args) => {
                let op = op_from_call(&f.name, args.clone(), &self.cfg.tools);
                (args, op)
            }
        };
        let result = match outcome {
            Ok(op) => match gate(&op, self.cfg.allow) {
                Some(refusal) if !own.allows_stop(&op) => Err(refusal),
                _ => match selection.and_then(|s| s.admit(&f.name, &arguments)) {
                    Some(refusal) => Err(refusal),
                    None => self.exec.apply(op).await?,
                },
            },
            Err(e) => Err(to_json(&e)),
        };
        if let Ok(value) = &result {
            own.record(&f.name, &arguments, value);
        }
        let ok = result.is_ok();
        let mut result = result.unwrap_or_else(|e| e);
        if ok {
            trim_result(&f.name, &mut result);
        }
        Ok(CallRecord {
            tool: f.name.clone(),
            arguments,
            ok,
            result,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_accept_and_reject_are_gated() {
        let accept = json!({"op": "signature_accept", "id": "01SIG", "name": null});
        let reject = json!({"op": "signature_reject", "id": "01SIG"});
        for op in [&accept, &reject] {
            let refused = gate(op, Allow::default()).expect("refused");
            assert_eq!(
                refused,
                json!({
                    "error": "not_allowed",
                    "field": "op",
                    "op": op["op"],
                    "message": "accepting or rejecting a signature is the operator's decision: report the proposal to the operator; they unlock it by allowing signatures for the run",
                })
            );
            // Allowing stops and deletes does not allow signatures.
            let others = Allow {
                stop_jobs: true,
                replace: true,
                delete_captures: true,
                accept_signatures: false,
            };
            assert!(gate(op, others).is_some());
            let allowed = Allow {
                accept_signatures: true,
                ..Allow::default()
            };
            assert!(gate(op, allowed).is_none());
        }
        let propose =
            json!({"op": "signature_propose", "capture": "01CAP", "name": null, "framing": null});
        assert!(gate(&propose, Allow::default()).is_none());
        let list = json!({"op": "signature_list", "state": null});
        assert!(gate(&list, Allow::default()).is_none());
    }

    #[test]
    fn own_jobs_count_bits_start_results() {
        let mut own = OwnJobs::default();
        let job = |id: &str| json!({"result": "job", "value": id});
        own.record("bits_start", &json!({"capture": "01CAP"}), &job("01BITS"));
        own.record("graph_start", &json!({}), &job("01GRAPH"));
        assert!(own.allows_stop(&json!({"op": "job_stop", "job": "01BITS"})));
        assert!(own.allows_stop(&json!({"op": "job_stop", "job": "01GRAPH"})));
        assert!(!own.allows_stop(&json!({"op": "job_stop", "job": "01OTHER"})));
    }
}
