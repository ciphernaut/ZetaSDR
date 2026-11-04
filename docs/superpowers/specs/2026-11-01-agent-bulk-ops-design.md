# Plan 21: requests too big for the voice console

Design spec, 2026-11-01. Brainstormed with the operator; a reviewer read it as the builder before
hand-over. Builds on the agent loop of plan 10 (`crates/zeta-agent`, `POST /ask` in
`crates/zeta-ws/src/ask.rs`), the lease manager (`crates/zeta-core/src/lease.rs`), the channel plans
(`crates/zeta-core/src/plans.rs`, plan "channel plans" of 2026-10-21) and the attached jobs of the
capture-and-transcripts design (`job_attach`). The TODO entry it answers is "Agent: requests too big
for the voice console" (commit 5f43af4).

## 1. Intent and why

The operator said to the voice console: "set up a narrowband FM decoder for every CB channel and
attach a transcribe session". That is 80 `fm_demod` starts and 80 `job_attach`es on the pinned
UHF CB capture. The run ended on the turn limit twice. The ops log of the live node on the field host
(transport `agent`, 2026-11-01) shows what happened:

- Run 1 (12:34) and run 2 (12:51): `job_list`, then seven `job_stop`s. The agent cleared the
  operator's hand-made jobs, unasked, twice, and spent its eight turns doing it.
- Run 3 (12:51:45): `channel_plans`, `source_list`, `job_start` ch1, `job_attach`, `job_start` ch2,
  `job_start` ch2 again, `job_attach`; turn limit.

Three facts from the code and the log decide the shape of the fix:

1. The loop already applies every tool call a completion carries (`Agent::turns` iterates
   `c.tool_calls`), but the model (gemma-4-12b behind llama-server) emits exactly one call per
   completion, and the completion cap is 1024 tokens (`LLM_MAX_TOKENS`), which would cut a long
   multi-call turn anyway. 160 operations are 160 turns at one to four seconds each.
2. The model doubled ch2 at the second channel. A 12B model writing 160 correct calls from a plan
   listing is not a reliable path, and a `batch` op that takes a list of ops would not change that.
3. The guide already says jobs are the operator's. The model stopped seven of them anyway, twice.
   Words did not hold; a mechanism must.

So the agent's job in this scenario is to understand the request, check who holds the radio, call
one operation that does the fan-out, read its result and tell the operator in plain words. The
fan-out, the clash check and the refusal to stop jobs are the node's and the executor's, where they
are testable.

What the operator wants, in their words: set up all 160 jobs, or block and report if the current
jobs (besides the spectrum job) would clash, offering the words that would let it proceed. Nothing
partial. The operator's own jobs survive any request that did not name them.

## 2. Goals, non-goals, anti-goals

### Goals

- One operation, `plan_start`, starts a kind on every channel of a plan (or the channels named),
  with an optional attached kind on each, on one source, and answers the ids it started and the
  channels it refused, with why. It is an ordinary op: in `schema`, in the guide (so the agent's
  tool list), on the CLI, typed on the page.
- All or nothing on the radio: `plan_start` decides the lease once, for the band bounding every
  requested channel, under the source's gate. A conflict names the holders and starts nothing. Only
  then are the channels started, inside that capture, where a rigid start never retunes.
- The agent cannot stop or replace a job unless the run was allowed to. `job_stop` and any
  non-empty `replace` are refused by the agent's executor, before the node sees them, until the
  `/ask` request (or the `zeta ask` command) carries the allowance. The refusal tells the model to
  report the clash and how the operator unlocks it.
- A duplicate start is refused: `job_start` on a source, kind and band that an identical running
  rigid job already holds answers an error naming that job. `plan_start` treats such a channel as
  `existing` and leaves it alone, so running it twice adds what is missing.
- The page sends the API's maximum of 32 turns.
- The model's reasoning, when the server returns it, is kept per turn in the `/ask` transcript and
  shown on the page, collapsed.
- The guide says all of this in words too: never stop or replace a job the request did not name;
  for a whole plan use `plan_start`; report a refusal as a refusal.

### Non-goals

- No change to how a single `job_start`, `job_attach` or `job_retune` leases, expands or conflicts.
  `plan_start` composes them under one gate; it does not re-implement them.
- No generic `batch` op. The fan-out over a plan is the generic mechanism the voice console needs
  (any plan, any kind, any attachable kind); a list-of-ops batch stays a slot (see 7).
- No change to the MCP transport (`zeta-mcp`): its model is a client under the operator's hand and
  takes no gate. The gate is the local agent loop's (`zeta ask`, `POST /ask`).
- No parsing of permission out of the operator's words. "Replace them" is not detected in the
  request text; the allowance is a flag the page or the command sets.
- No change to `cb_watch.py`, which keeps its own policy of demodulating only the active channels.
- No mixer for listening to several channels at once. That is the next design.

### Anti-goals

- An agent that quietly does less than asked. A refused channel is in the result, a refused run is
  an error, and the agent's answer repeats both.
- Hidden knowledge. The fan-out must be discoverable by the agent (its tool list), the operator
  (the guide, the page) and a script (`schema`).
- A gate the model can talk its way around. The executor refuses; the system prompt only explains.

## 3. Constraints

- Every field is required and unknown fields are rejected, as for every op (`deny_unknown_fields`
  on `Op`). `null` where a field has none.
- The op's docs are its doc comment on the `Op` variant plus a `note` in
  `crates/zeta/src/agents_md.rs`; `zeta agents-md --check` must pass, and `AGENTS.md` and
  `skills/zetasdr/SKILL.md` are regenerated with it. The agent's tool description for the op comes
  from that rendering (`zeta-client/src/guide.rs` `op_sections`).
- The agent's tool list and system prompt are derived from `schema` and `AGENTS.md`, so a new op
  reaches the model without agent code, but the executor's gate is agent code.
- The lease manager's rules stand: elastic jobs never make a source busy; a pin is never a holder; a
  scan time-shares the radio and every other request on it conflicts; `replace` names rigid holders
  only, each checked and stopped under the gate right before the request.
- The node's channel plans are static (`plans::PLANS`, today `uhf_cb_au`); a plan's channels and
  `bandwidth_hz` come from there, never from the request.
- Jobs are persisted one record each; `plan_start` creates ordinary records (`kind`, `tuning`,
  `input`), nothing that names the plan. A node restart resumes them as any other jobs.
- The speech backend is the node's (`--stt-socket`/`--stt-node`); `job_attach` already fails with a
  reason naming it when there is none.
- The web page's e2e test is `crates/zeta-ui/tests/e2e/ui.py`; agent tests use `FakeLlm`
  (`crates/zeta-agent/src/fake.rs`) and scripted completions; ws tests the same over HTTP.

## 4. The design

### 4.1 Shape

Four pieces, each independently testable and shippable in this order: the gate, the duplicate
refusal, `plan_start`, the transcript and page changes.

```
operator ──voice/text──> page ── POST /ask {request, max_turns: 32, allow: {stop_jobs, replace}} ──> serve
                                                                                                      │
                                                   zeta-agent: Agent::call ── gate(allow) ──x refused ─┤ (fed back to the model)
                                                                        └── exec.apply(op) ──> node ───┤
                                                                                                      │
   node: plan_start ── gate(source) ── lease bounding band (replace) ── start each channel ── attach each
```

### 4.2 The gate in the agent's executor path

`AgentConfig` gains an `Allow` policy:

```rust
pub struct Allow { pub stop_jobs: bool, pub replace: bool }   // both false by default
```

`Agent::call` checks the parsed op against it before `exec.apply`:

- `job_stop` with `stop_jobs: false` is refused.
- `job_start`, `job_retune`, `source_ensure`, `source_pin`, `plan_start` with a non-empty `replace`
  and `replace: false` is refused.
- `source_unpin` is not gated: a pin is not a job, and the guide already tells the model to unpin
  only when the operator gives the pinned channels up. (My call; see 8.)

A refused call becomes the call's result, as a bad argument does today (it never reaches the node,
so it is not on the `ops` stream), with this JSON:

```json
{"error": "not_allowed", "field": "replace" | "op", "op": "job_stop",
 "message": "stopping jobs is not allowed in this run: report the conflict and its holders to the operator; they unlock it with the retry button or by allowing stops for the run"}
```

`field` is `"op"` for `job_stop` and `"replace"` for the others. The `CallRecord` keeps `ok: false`
and this result, so the page can see what was refused and offer the unlock (4.7).

The allowance travels:

- `POST /ask` takes `allow: {"stop_jobs": bool, "replace": bool}`, optional, both false when
  absent (the one optional field of the request, like `max_turns`; unknown keys inside it are
  rejected).
- `zeta ask` takes `--allow-stop` and `--allow-replace` (false by default) and passes them whether
  it runs the agent locally or against `--node`.
- `/ask` answers the transcript with the `allow` it ran under, so the page can show it.

The system prompt's Rules (`RULES` in `agents_md.rs`) gain two lines, which also render into
`AGENTS.md`:

- "Jobs belong to the operator: never `job_stop` a job or put one in `replace` unless the request
  names it or the run allows it. A `conflict` is reported with its holders; the operator decides."
- "For a kind on every channel of a plan use `plan_start`, never one `job_start` per channel."

The guide's `job_stop` section and the Field craft section "Taking the radio from another job"
(`docs/agents/field-craft.md`) say the same in their own terms, and a new Field craft section
"Starting a whole plan" (4.9) shows the fan-out.

### 4.3 The duplicate refusal in `job_start`

Under the source's gate, before the lease, `job_start` with `elastic: false` looks for a `running`
rigid job on the same `source` with the same `kind` and `tuning` (`center_hz`, `bandwidth_hz`) and,
for the kinds that take them, the same `deviation_hz` and `sideband`. A `JobRecord` does not store
the start parameters, so those two are read from the running job's graph: the `deviation_hz` of its
`nbfm_demod`/`wfm_demod` node, the `sideband` of its `ssb_demod` node, by a helper on `JobKind`
beside `reexpand` that knows which node carries them. The request's `null` is compared as the kind's
default. A job whose graph an operator `graph_put` replaced and that has no such node compares on
`kind` and `tuning` alone. A deviation edited live with a carried-params `graph_put` therefore
counts as it now runs: a fresh start with the original value is not a duplicate of it.

Today a `job_start` without `replace` takes the source's gate only inside `tune_if_unheld`; the
check needs the gate, or a racing double-click slips through, which is the case it exists for. So
`job_start` takes the gate for the check and the lease always (a small change to the common path;
the gate is per source and the check is a scan of that source's running jobs). One found is:

```json
{"error": "duplicate", "job": "<existing id>", "message": "an identical fm_demod already runs on <source> at <center_hz>/<bandwidth_hz> Hz as job <id>"}
```

A new `OpError` variant, `Duplicate { job, message }`, listed in `schema` and the guide with the
other errors. It is refused before anything is leased or stopped, so a `replace` on the same
request stops nothing (the duplicate check runs before the replacements, under the same gate).

Elastic starts (the page's spectrum) are not checked: the page relies on starting its own. An
attached job (`job_attach`) is not checked either: two transcribers on one demodulator are
pointless but harmless, and `plan_start` avoids them on its own (4.4).

Why in `job_start` and not only in `plan_start`: the agent doubled ch2 with plain starts, and the
page can double-click too. A refusal here is one rule in one place.

### 4.4 `plan_start`

```json
{"op": "plan_start", "plan": "uhf_cb_au", "channels": null, "kind": "fm_demod", "source": "<id>",
 "deviation_hz": null, "sideband": null, "attach": "transcribe", "language": null, "replace": []}
```

CLI: `zeta plan start --plan uhf_cb_au --kind fm_demod --source S [--channels 1,2,5-8]
[--attach transcribe] [--language de] [--replace ID,ID]`. (`zeta plans` stays the listing; the new
verb lives under a `plan` group, as `zeta modes list` and `zeta modes show` do.)

Fields:

- `plan`: an id from `channel_plans`; unknown is `unknown` with `field: "plan"` and `known`.
- `channels`: `null` for every channel of the plan, else an array of channel numbers; a number the
  plan lacks is `unknown` with `field: "channels"`; a repeated number is refused (`other`).
- `kind`: a job kind that runs on the radio (not `transcribe`, not `spectrum`, `scan`, `watch` or
  `identify`: those make no sense per channel and are refused as `other` naming the kind). The
  demodulators, the decoders and the mode rows are accepted as `job_start` accepts them.
- `source`: an id from `source list`.
- `deviation_hz`, `sideband`: passed to every start as `job_start` takes them (`null` is the kind's
  default). For an `fm_demod` on a 12.5 kHz plan channel the guide says 2500 Hz; `null` keeps
  `job_start`'s 5000 Hz default, as today. (My call: `plan_start` does not pick a deviation from the
  plan's width; see 8.)
- `attach`: `null`, or an attachable kind (`transcribe`) to attach to every started job.
- `language`: `job_attach`'s language, `null` for `en`; must be `null` when `attach` is.
- `replace`: as on `job_start`.

Every started job's band is the channel's `center_hz` with the plan's `bandwidth_hz`.

Under the source's gate (`job_start_gated`'s guard), in this order:

1. **Validate** everything above. Nothing has happened yet.
2. **Duplicates.** For each channel, an identical running rigid job (4.3) makes the channel
   `existing`; it is skipped in every step below.
3. **Preflight the attached stage.** With `attach` set, the stage the attached kind runs must be
   ready (4.8: `transcribe` is not when the node has no speech backend); otherwise the whole op is
   refused with the stage's reason and the advice to start without `attach`. Nothing has happened
   yet.
4. **Replacements.** The `replace` ids are checked and stopped as `job_start` does, under the gate.
5. **One lease decision** for the band that bounds every channel to start (lowest channel edge to
   highest; one channel is just that channel). It is checked as a rigid `job_start` of that band
   would be: inside the current capture, granted without a tune; outside it, the source is tuned
   when only elastic jobs (or nothing) hold it, and the elastic jobs are relaunched after the gate
   as today; a scan or a rigid holder elsewhere is a `conflict` naming the holders, with the same
   `options` a `job_start` conflict offers, and nothing is started. A band wider than the source's
   `max_bandwidth_hz` is `out_of_range` on `bandwidth_hz` with the bounding width as `value`.
   The lease itself is not kept: the capture it produced is, and each channel takes its own rigid
   lease inside it in step 6 (a rigid request inside the capture is granted without a tune and
   never conflicts). The builder chooses how the lease manager exposes this (a check-and-tune for a
   band without a holder, or a transient holder released once the channels hold); the observable
   rule is one decision, no partial tune.
6. **Start each channel** in channel-number order with `job_start`'s gated path, inside the capture.
   A start that fails (an expansion error, a source fault mid-way) is recorded under `refused` with
   its error and the op continues: the preflight has already ruled out the clash, so what fails
   here is that channel's own problem and the others are not punished for it.
7. **Attach** to each started job (`job_attach`'s path; `language` as given). A failure is recorded
   on that channel's entry as `attach_error`, the demodulator kept.

Steps 6 and 7 may run after the gate is released (the gate protects the lease decision; starts
inside a capture do not need it); the builder decides, with the constraint that another caller's
tune cannot slip between step 5 and the last start. Holding the gate for the whole op is the simple
answer and what the design assumes: 80 starts take well under a second.

The result:

```json
{"plan": "uhf_cb_au", "source": "b210", "capture": {"center_hz": 476918750, "bandwidth_hz": 1024000},
 "started": [{"channel": 1, "center_hz": 476425000, "job": "01…", "attached": "01…", "attach_error": null}, …],
 "existing": [{"channel": 2, "center_hz": 476450000, "job": "01…"}],
 "refused": [{"channel": 61, "center_hz": 476912500, "error": {…}}],
 "warnings": ["80 transcribe jobs share the speech backend's 4 slots: with the squelch on most channels are silent, so the slots are rarely contested; a busy daemon drops utterances with reason busy"]}
```

`warnings` is a list of strings, empty when there is nothing to say. The load warning appears when
the number of running jobs of the attached kind after the op exceeds the stage's `capacity` (4.8;
4 for `transcribe` today, the speech daemon's slots). The agent is told in the guide to repeat
warnings to the operator.

`job_list` shows the started jobs as ordinary jobs (`fm_demod`, `transcribe` with `input`); nothing
marks them as a plan's. `source_list.holders` lists each demodulator. `job_stop` on each is how
they end; there is no `plan_stop` in this plan (see 7).

An `existing` channel with `attach` set and no running attached job of that kind on it gets one
attached (so a second `plan_start` completes a first that ran without `attach`); one that already
has one is left alone and listed under `existing` with that attached id. This is what makes the op
safe to run twice.

### 4.5 The agent's turn budget

`api.ask` on the page sends `max_turns: 32` (`MAX_TURNS` in `ask.rs`; the request is refused above
it today, so the server does not change). `zeta ask --max-turns` keeps its default of 8 (one-shot
use; the operator raises it). The agent's own default stays 8 (`AgentConfig::default`,
`max_turns_defaults_to_eight` pins it).

### 4.6 Reasoning and content in the transcript

`Completion.reasoning` (`reasoning_content`, parsed in `llm.rs` and dropped) and the `content` a
completion carries beside its tool calls (dropped too, unless it is the final answer) are kept per
turn:

```json
"turn_log": [{"turn": 1, "reasoning": "…" | null, "content": "…" | null, "calls": 2}, …]
```

One entry per completion, in order; `calls` is how many tool calls it carried (0 on the answering
turn). The entries are not sent back to the model (today's rule stands: `reasoning_content` never
goes back). The transcript's existing `calls`, `answer`, `turns` and `request` are unchanged, so
the CLI's and the page's readers keep working; `zeta ask` prints the transcript as it does today,
`turn_log` included, since it prints the whole JSON.

Reasoning arrives only when the server produces it, which with llama-server means `--llm-thinking`
(the request sets `enable_thinking: false` otherwise). The page shows an entry's `reasoning` and
`content` under the assistant's answer, collapsed, one `<details>` per turn, labelled
`turn N (K calls)`; a turn with neither is not shown.

### 4.7 The page

- `api.ask(text, maxTurns = 32, allow = {stop_jobs: false, replace: false})`.
- A gated call in the returned transcript (`calls[i].result.error === "not_allowed"`) produces a
  transcript note after the assistant's answer, with one action: `allow stops and ask again`, which
  re-sends the same request with both flags true. The page shows the transcript's `allow` on the
  assistant line when either flag is on (`[may stop jobs]`), so a run that could stop jobs is
  visible as such.
- A per-session switch in the console's command line, `allow stops on|off` (a typed command beside
  `view …` and `source …`; off at page load), sets the flags for every following `/ask`. Voice
  reaches it the same way as any command the transcript puts in the command line. The operator says
  "allow stops on", then repeats the request. This is the "keywords" of the TODO: two words, fixed,
  never guessed from the request's wording.
- `plan start uhf_cb_au fm_demod [transcribe] [ch 1,2,5-8]` typed on the command line applies
  `plan_start` on the shown source (`state.selected`, as `source pin` does) without the agent, and
  reports the result as the transcript renders a `plan_start` outcome: `plan_start uhf_cb_au
  fm_demod+transcribe → 78 started, 2 existing, 0 refused` with the refusals listed, and the
  warnings as notes. The source's outline menu offers `Start plan…` which fills the command line
  with that text for the operator to edit and submit; the menu's placement (the pinned band's
  outline, or the source row) is the builder's.
- `plan_start` joins `REPLACEABLE`, so its `conflict` gets the same retry buttons.
- `formatOp` renders a `plan_start` record on the `ops` stream as above, and a `not_allowed` error
  as `→ not allowed: …`, and a `duplicate` error as `→ duplicate of job <short id>`.

### 4.8 Stage readiness

`zeta-core` knows nothing of speech: `TranscribeStage` lives in `crates/zeta-speech`, is always
registered (`node_setup.rs`), and a `job_attach` on a node without a backend answers the new job's
id, the job then failing at launch with `transcribe: this node has no speech backend`. The daemon's
slot count (`STT_CONCURRENCY`, 4) is the speech crate's too. `plan_start` needs both at op time, so
the stage contract gains one generic hook rather than a speech-specific flag:

```rust
pub struct Readiness { pub ready: Result<(), String>, pub capacity: Option<u32> }
trait Stage { … fn readiness(&self) -> Readiness { Readiness::always() } … }
```

`TranscribeStage` answers `Err("this node has no speech backend (--stt-socket or --stt-node)")`
without a backend and `capacity: Some(4)` with one (the daemon's slots; a `--stt-node` backend
answers the same number, it forwards to a daemon with that many). Every other stage keeps the
default: ready, no capacity.

Who reads it:

- `plan_start`, step 3 of 4.4, for the attached kind's stage, and for the load warning.
- `job_attach`, which now refuses at op time with the stage's reason (`other`) instead of answering
  an id whose job fails a moment later. The launch-time failure stays for a backend that goes away
  after the attach.
- `schema`, which lists each stage's readiness beside its `params_schema`, so a client can see what
  the node cannot run before it tries. (`modes_list` already does this for mode rows with `state`
  and `reason`; this is the same idea for stages.)

This is the one-mechanism form: the hook is for any stage that depends on something outside the
node (a program, a daemon, a device), `transcribe` is only the first to answer it.

### 4.9 Docs and the guide

- `Op::PlanStart`'s doc comment and a `note("plan_start")` carry the behaviour of 4.4 in the
  guide's voice. `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerated.
- Field craft gains "Starting a whole plan": check `source_list` for holders other than the elastic
  spectrum job; call `plan_start`; read `started`, `existing`, `refused`, `warnings`; report them;
  on a `conflict`, report the holders and stop, the operator decides. It also says what a
  `not_allowed` result means and what to tell the operator.
- "Taking the radio from another job" gets the sentence the TODO asks for: stopping or replacing
  a job is the operator's explicit decision; hand-made jobs survive any request that did not name
  them.
- `README.md`'s operation list and the `zeta plan start` CLI form.

## 5. Testing

Agent (`crates/zeta-agent/tests/agent.rs`, `FakeLlm`):

- A scripted `job_stop` with `allow.stop_jobs: false` never reaches the executor, the model
  receives the `not_allowed` JSON, the transcript's record is `ok: false` with it; with `true` it is
  applied. The same for `job_start` with a non-empty `replace` and the `replace` flag, and a
  `plan_start` with `replace`. An empty `replace` passes under `replace: false`.
- `turn_log` has one entry per completion with the fake's `reasoning_content` and `content`, and
  the request bodies sent to the fake never contain `reasoning_content`.

Node (`crates/zeta-core`, over a file source with a tune range, the existing fixtures):

- `job_start` twice with the same kind and band answers `duplicate` naming the first; a different
  `deviation_hz` is not a duplicate; an elastic start is never a duplicate.
- `plan_start` on `uhf_cb_au`, all channels, `attach: null`, on an unheld source tunes it to the
  bounding band and starts 80 rigid jobs; `source_list.holders` has 80 ids; the result's `started`
  has 80 entries in channel order.
- The same with a rigid holder outside the band on the source: `conflict` naming it, zero jobs
  started, the source untuned; with that id in `replace`: it is stopped and the 80 start.
- With a scan on the source: `conflict` naming the scan.
- Inside a pin that holds the band: no tune, 80 started. A pin elsewhere: `conflict` offering
  `unpin source <id> (source_unpin)`.
- `channels: [1, 2, 41]` starts three; `[1, 1]` is refused; `[81]` is `unknown`.
- Run twice: the second answers 80 `existing`, nothing started, nothing stopped.
- `attach` of a stand-in attachable kind whose stage answers not ready: refused before any start,
  zero jobs. With a stand-in stage that is ready with `capacity: Some(4)`: 80 demodulators and 80
  attached jobs, each attached job's `input` its demodulator; the load warning present. Run again
  with `attach` after a run without: 80 `existing`, each now with an `attached` id. (Core's attach
  tests, `crates/zeta-core/tests/attach.rs`, already use inline stand-in stages; these follow them.)
- `job_attach` with `kind: transcribe` on a node without a backend is refused at op time with the
  stage's reason; with the fake backend it is accepted. These, and one `plan_start` with
  `attach: transcribe` over the fake daemon (`zeta_speech::testing`, `FakeDaemon`/`FakeBackend`),
  live in `crates/zeta/tests` beside `speech_chain.rs` and `attach.rs`, since `zeta-speech` depends
  on `zeta-core` and core cannot host them.
- `schema` lists `transcribe`'s readiness as not ready without a backend and ready with
  `capacity: 4` with one.
- A bounding band wider than the source's `max_bandwidth_hz`: `out_of_range`.
- A kind of `transcribe`, `spectrum` or `scan`: refused.
- A node restart resumes the 160 as ordinary jobs (the persistence tests' pattern).

Serve (`crates/zeta-ws/tests/ask.rs`): `allow` is parsed, defaults to false, an unknown key inside
it is 422; the answered transcript carries `allow` and `turn_log`.

CLI: `zeta plan start` maps to the op (the `cli_form` rendering in `agents_md --check` covers the
guide; a unit test covers the channel list parsing `1,2,5-8`); `zeta ask --allow-stop` reaches
`AgentConfig`.

Page (`ui.py` e2e over `crates/zeta/tests/ui.rs`): the harness serves no model today, and its two
ask scenarios assert the 503. The `/ask` scenarios here need one, so the harness gains a third
`zeta serve` with `--llm-url` at a `FakeLlm` that answers by request: `fake.rs` gets a
`by_request(Vec<(substring, Vec<Value>)>)` form that picks the completion queue whose key the
user message contains (the existing ordered queue stays for the agent and ws tests). Scenarios are
then independent of the order Playwright runs them. Against it: `allow stops on` then an ask sends
both flags (the fake records the request bodies, the scenario reads them through the harness); a
scripted `job_stop` under stops-off shows the `not_allowed` note and its button, and the button
re-sends with both flags; the collapsed turn log appears when the fake returns
`reasoning_content`; `plan start …` typed applies the op against the pinned file source and renders
the counts. The two 503 scenarios keep running against the model-less server.

Guide: `zeta agents-md --check` passes with the regenerated files.

## 6. Acceptance on the field host

On the `sdr` node with the B210 pinned at 476 918 750 / 1 000 000 Hz:

1. From the page, "set up a narrowband FM decoder for every CB channel and attach a transcribe
   session" ends in one `plan_start` by the agent, 80 demodulators and 80 transcribers in
   `job list`, and an answer that names the counts and the speech-slot warning, within three or
   four turns.
2. The same request with a hand-made `aprs_decode` holding the radio at 144.8 MHz (unpinned) ends
   with a conflict naming it, nothing started, nothing stopped, and the answer saying so with the
   way to proceed; `allow stops on` and the request again stops it and starts the 80.
3. "Stop the APRS job" with stops not allowed ends with a refusal reported, the job running.
4. The same request a second time answers 80 `existing` and starts nothing.

## 7. Slots left open

- A list-of-ops `batch` with back-references, if a request ever needs a fan-out that is not a
  plan. The `plan_start` result shape (`started`/`existing`/`refused`/`warnings`) is the shape it
  would answer with.
- `plan_stop` (every job on a plan's channels of a kind) and a plan-aware `job_list` view. Stopping
  stays per job for now, which is also what the gate protects.
- The mixer for listening to many channels at once (the next design).
- A deviation taken from the plan's channel width (2500 Hz for 12.5 kHz channels) when `null`.

## 8. Calls made in this document (check these while reading)

- The allowance is two flags, not one: `stop_jobs` and `replace`, because a run may be allowed to
  make room for a request (`replace`) without being allowed to stop things in general. The page's
  switch and button set both.
- `source_unpin` is not gated. A pin is held by the operator, the guide already restricts it, and
  gating it would leave the agent unable to answer "unpin the radio".
- `plan_start` refuses the whole op when `attach: transcribe` is asked of a node without a speech
  backend, rather than starting 80 demodulators with 80 identical attach errors. The agent can call
  again without `attach`.
- The node learns a stage's availability through a generic `Stage::readiness` hook (4.8), not a
  speech flag on `NodeConfig`: it is the one-mechanism form, it lets `job_attach` refuse at op time
  too, and `schema` can show it. The reviewer offered both; this is the larger of the two by a
  little (the stage trait plus `zeta-speech`), and it is where a future program-backed or
  device-backed stage will answer the same question.
- The duplicate check reads `deviation_hz`/`sideband` from the running graph, so it reflects a live
  `graph_put`, and `job_start` takes the source's gate for the check always, so a racing
  double-start cannot slip through.
- The page's `/ask` e2e scenarios get a request-keyed `FakeLlm` behind a third harness server rather
  than being cut to the ws level: the not-allowed note, its button and the turn log are page
  behaviour and should be seen on the page. If the harness work turns out larger than a task, the
  builder may land the ws-level checks first and leave the page scenarios to the field host's
  acceptance, saying so in the plan.
- A per-channel failure after the lease decision (step 6) does not roll the others back: the clash
  the operator cares about is ruled out in step 5; a channel's own expansion error is that
  channel's.
- `plan_start` takes `deviation_hz` as `job_start` does and does not infer 2500 Hz from a 12.5 kHz
  plan; the agent is told the guide's advice applies.
- The permission words are fixed (`allow stops on|off`), not parsed from the request, and the
  switch is per page session, off at load.
- The turn log is a separate `turn_log` array rather than a field on each call, so the transcript's
  existing readers are untouched and a turn without calls (the answer) still has its reasoning.
- The duplicate check compares `kind`, `tuning`, `deviation_hz` and `sideband` on running rigid
  jobs only; a stopped identical job is history, not a duplicate.
