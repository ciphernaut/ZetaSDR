# Plan 33: radio selection policy

Design spec, 2026-11-09. From the operator's request of the same day (section 1). Builds on the
agent loop (`crates/zeta-agent/src/agent.rs`), the page's `/ask` path (`crates/zeta-ui/web/api.js`
`ask()`, `app.js` `askAgent`, `crates/zeta-ws/src/ask.rs`), the source tabs and the Map tab
(`app.js` `select`, `syncMainTabs`, `renderTabs`; plan 32), the view policies (`view.js`
`POLICIES`), the waterfall's box menu (`menu.js`), the guide (`crates/zeta/src/agents_md.rs`,
`docs/agents/field-craft.md`) and the themes doc (`docs/themes.md`). The plan executes on `main`
after the digital-modes sprint branch (plans 19.3, 29, 30, 32) has merged into it, beside plans
28.1 and 28.2 already there; every name below is checked against both.

A reviewer read this document as the builder before hand-over; its questions are folded in
(section 9 lists them and where each went). The shape is the author's: each call is listed in
section 10 and is open to correction; the operator's decisions of 2026-11-09 are section 11.

## 1. Intent and why

The operator, verbatim:

> one thing that keeps tripping me up is that when i ask the internal agent to start a job it
> selects the hackrf even if the b210 is in focus. ideally, the default radio to work with is
> either the active tab, or if map or some other view is being used, the previously used radio.
> then the need arises to call out a background radio if a different one is required. may help
> to somehow mark the radio tab with the current 'default' in some way (maybe a '*' or something
> cooler)

and then:

> ok put it into a plan, and lets have a 'selection policy' as a user setting rather than making
> it concrete. it will also need to tie into permission/authorisation levels later particular
> where TX is concerned

Why it happens today. The page sends the agent only the request text: `api.ask()` posts
`{request, max_turns, allow, page_tools}`, `AskRequest` (`deny_unknown_fields`) has no other
field, and `Agent::turns` builds `[system, user]` with no context. `job_start` and `plan_start`
require `source`, nothing supplies a default, and the only guidance is the rule "Ids are never
invented: `source` values come from `source list`". The model then calls `source_list` or
`source_free`, both of which list in id order, and picks one by whatever it reads into the lists
and the names: nothing ties the choice to the radio the operator is looking at.
The page, meanwhile, knows exactly which radio the operator is looking at: `state.selected`
(set by `select`, kept in `sessionStorage["zeta.source"]`), unchanged while the Map tab is shown
(the shown source's tab carries `kept`), and already the implicit source of the typed
`plan start` and `source pin` (`shownSource()`). It just never tells the agent.

So the fix is to carry the page's focus to the agent with each request, decide the default radio
from it under a policy the operator chooses, tell the model that default in plain words, hold the
model to it where the policy says so, show the operator which radio is the default, and say so
when a run used another one. The same record is the seam a later access-level design plugs into.

## 2. Goals, non-goals, anti-goals

### Goals

1. A request from the page that names no radio runs on the radio the operator is looking at: the
   shown tab, or while the Map (or any later non-radio main view) is shown, the radio last shown.
2. The rule is a named **selection policy**, a user setting, not a hard-coded behaviour: `focus`
   (default: the focused radio only, else stop and ask), `focus-fallback` (the opt-in fall-back
   to a free radio), `free-first` (today's behaviour), `ask`; plus an optional ordered
   preference list of radios used wherever a policy picks among several.
3. A radio named in the request always wins over the policy.
4. When a run works on a radio other than the default, the operator is told, in the agent's
   answer (the guide requires it, with the reason: busy, pinned, faulted, does not cover the
   band) and deterministically by the page (a transcript note computed from the calls, not from
   the model's prose).
5. The box menu's "Send to agent" names the radio its selection was drawn on.
6. The default radio's tab is marked (an accent dot with a hover title), in both footprints,
   while the RF view or the Map is shown, theme-able.
7. `zeta ask` and requests without a page keep working unchanged; `zeta ask` gains flags for a
   default radio and a policy.
8. The selection result is shaped so a later access-level design can restrict which radios a
   caller may select, require a stronger level for transmit, and record who selected what,
   without changing the policy's shape (section 5).

### Non-goals

- Transmit, in any form. `Capabilities.tx` already exists in `source_list` and stays as it is.
- Access levels, ACLs, per-caller identity. Section 5 defines the seam only.
- Trunking or any multi-radio orchestration beyond picking one default.
- Tracking a node-wide "last radio an op ran on" (section 4.2 says why).
- A node-stored policy default (`zeta serve --radio-policy`): no page-less `/ask` caller needs it
  today (section 4.6); it arrives with per-caller levels.
- Matching radios by driver or role (`driver=b200`): `SourceInfo` carries no driver; the
  preference list names source ids. A TODO gist keeps the idea.
- Changing the watch's own placement (`decode_on: free`, `prefer` on its `detect` node): that is
  the watch's policy, set per job, and stays as it is.

### Anti-goals

- Making `source` optional on any op. The ops stay strict for every transport; the default is
  resolved before the model writes the op, and the model writes it out.
- The agent stopping or replacing a job to make room on the default radio. Falling back means a
  free radio, never `replace` (the standing rule: jobs belong to the operator).
- The agent changing its own selection policy: `radio_policy` and `radio_prefer` are not agent
  view keys.
- One variant per policy in code paths. One resolver and one gate, the policy a parameter.

## 3. Constraints and findings

- **One run at a time.** `/ask` holds `ask_lock` for the run, so the context travels with each
  request and the run owns it; no shared mutable "current radio" on the node is needed.
- **Strict request body.** `AskRequest` is `deny_unknown_fields`. A new optional field keeps old
  bodies valid; a page newer than its node would be refused, but the node serves the page and the
  page reloads on a build change (`NODE_CHECK_MS`), so they move together.
- **The system prompt.** `zeta_client::system_prompt(agents_md)` keeps the intro, `## Rules` and
  `## Field craft` of the guide. Local models served through chat templates (Gemma among them)
  reject or fold a second system message, so per-request context is appended to the one system
  message, at its end, which keeps the long prefix identical across requests (prefix caches stay
  warm) and leaves the user message exactly the request (the e2e's request-keyed fake model
  matches on the last user message, `zeta-agent::fake` `Scripts`).
- **Prose without a tool call** is an answer only to a question or after tool calls
  (`is_question`, design 10 of the agent spec). A policy that ends in asking the operator must
  therefore make a call first; the guide has it call `source_free` for the band, which it needs
  anyway to know who could take it. The loop's rule is unchanged.
- **Refusals the page offers to unlock.** `askAgent` treats every call result with
  `error: "not_allowed"` as a refused stop, delete or signature and offers a re-run with the
  allowance. A policy refusal must not look like that; it uses its own error word (section 4.5).
- **The page's focus.** `state.selected` is set by `select` (a tab click, or `renderTabs`
  restoring `zeta.source`, else the first source). The view's `main` (`rf | map`, plan 32) says
  whether the shown tab or the Map is selected; with `map` the shown source's tab carries
  `kept`. Typed `plan start` and `source pin` already act on `shownSource()`.
- **The box menu's selection** carries `sourceId` (`this.sel = {...sel, sourceId:
  this.wf.sourceId}`); the `ask` action drops it from the text.
- **Sources change.** A source may be faulted, removed between restarts, or a stale id may sit in
  `sessionStorage`. The resolver checks the focus against a fresh `source_list`.
- **The e2e suite** runs two shards (`SHARD_STARTS` in `crates/zeta-ui/tests/e2e/ui.py`) under the
  690 s deadline (`crates/zeta/tests/ui.rs`). The LLM node (`ZETA_UI_URL_LLM`) has the sources
  `tone` and `cb` and a request-keyed fake model (`fake_llm_scripts`). New page behaviour is
  tested by extending existing ask scenarios where it fits and adding one scenario.
- **Public repository.** Source ids in docs and tests are device models or test names (`b210`,
  `hackrf`, `tone`, `cb`), never host names.

## 4. Design

### 4.1 The policies

A **selection policy** says which radio is the default for a request that names none, and what
the agent may do when the default cannot take the band. Four named policies (the operator chose stop-and-ask as the default, section 11):

| Policy | Default radio | When the default cannot take the band |
|---|---|---|
| `focus` (the default) | the focus: the shown radio, or the kept one while the Map is shown | use no other radio: say why and ask which to use, starting nothing; the gate refuses another radio the request did not name |
| `focus-fallback` (opt-in) | the focus | use the first free radio (`source_free`, in preference order) and say which and why |
| `free-first` | none | (today's behaviour) the first free radio in preference order |
| `ask` | none | when the request names no radio: call `source_free`, then ask which of the free radios to use; start nothing |

"Cannot take the band": it does not cover it (`tune_min_hz..=tune_max_hz`, `max_bandwidth_hz`,
a measured cap with `why` on `main`), a rigid holder or a pin holds it elsewhere (a `conflict`),
or it is faulted. When no free radio can take the band either, every policy ends the same way:
start nothing, report why (the conflict's holders, or that no radio covers the band) and never
`replace`. A request with a context but no usable focus (a stale id, nothing listed yet) has no
default radio: under `focus` it behaves as `ask` (list the free radios, ask which, start nothing)
and says "no radio in focus"; under `focus-fallback` it behaves as `free-first` and says so.

**The preference list**, `radio_prefer`, is an ordered list of source ids or `none`. It orders the
candidates wherever a policy picks among several (the `focus-fallback` fall-back, `free-first`, the
radios `ask` offers and the radios `focus` lists when it asks); ids not in `source_list` are skipped (radios come and go); radios not listed follow
in id order. It never picks the default under `focus` or `focus-fallback`. It is a list rather than a fifth policy so
it composes with each of the four.

**Precedence**, first that applies:

1. A radio the request names. "Names" is a source id appearing in the request text as a whole
   word, case-insensitive (`on b210`, `B210:`; not `b2100`). One named radio becomes the default
   with reason `named`; several named radios leave the policy's default in place and the model
   picks among the named ones. A named radio is never refused by the policy gate.
2. The policy, applied to the request's context.
3. Without context (a page-less `/ask`, `zeta ask` without any radio flag or variable): policy
   `free-first` in id order, today's behaviour, kept for compatibility. This is the one place the
   default policy does not apply: `focus` is the default for a request that carries a context
   (the page always does), and a context that omits `radio_policy` means `focus` when it gives a
   focus, else `free-first`.

### 4.2 What "the previously used radio" is

The page's shown radio is it. `state.selected` changes only when the operator selects a radio
tab (or the page restores it at load), and it stays put while the Map is shown; that is exactly
"the active tab, or if map or some other view is being used, the previously used radio". The
context says which of the two it is (`main`), so the reason reads `shown` or `kept`.

A node-wide "last radio an op ran on" is not tracked. With several callers (the page, `zeta`
one-shots, MCP clients, the agent) sharing one secret, a node-wide memory would carry one caller's
choice into another's default, which is the confusion this plan removes; a per-caller memory needs
caller identity, which arrives with access levels (section 5). `zeta ask` takes `--radio` instead,
and an MCP client is its own agent with its own memory.

### 4.3 Where the setting lives

**On the page**, two view policies in `POLICIES` (`view.js`), set with `view set`, shown by
`view show`, reset by `view reset`:

- `radio_policy`: `focus | focus-fallback | free-first | ask` (default `focus`);
- `radio_prefer`: `none` or a comma-separated list of source ids (`b210,hackrf`; each matching
  `SOURCE_ID`, no repeats; default `none`). The page does not check the ids against
  `source_list`: a radio that is absent today may be attached tomorrow.

**Persistence (operator's decision, section 11).** These two keys persist for the user across
tabs and browser restarts; every other view policy stays per tab in `sessionStorage` (`VIEW_KEY`),
and so does the shown radio (`state.selected`, `zeta.source`). The page has no cookie code and
every existing store is Web Storage wrapped in try/catch, so the choice is `localStorage`: one
key (`RADIO_PREFS_KEY = "zeta.radio_prefs"`, a JSON object holding only the keys set), read and
written beside `loadStored`/`saveStored` in `view.js` with the same try/catch. With storage
unavailable the defaults apply and a value set holds for the page load. A cookie would add
nothing (it would ride every request to the node), and the node has no users yet, so the
browser is the store. `view.get` reads these two keys from storage on each call (so another tab's
change shows at the next ask or repaint), `view set` writes it and `view reset` clears both keys.
When users exist the preference moves to the user's record on the node, keyed by the caller
(section 5).

Neither joins `AGENT_VIEW_KEYS` (page) or `VIEW_KEYS` (`crates/zeta-agent/src/page.rs`): the agent
never sets its own selection policy. Neither has a theme default: a theme is presentation, and a
policy is not.

**For `zeta ask`**, three flags on `AskArgs`, each with an environment variable so a shell or a
unit can make it standing:

- `--radio ID` (`ZETA_RADIO`): the focus, as the page's shown radio; reason `flag`;
- `--radio-policy P` (`ZETA_RADIO_POLICY`; no clap default: with none of the three given there is
  no context and precedence step 3 applies, and with any given an absent policy follows the rule
  in 4.1);
- `--radio-prefer IDS` (`ZETA_RADIO_PREFER`).

An unknown policy word is a usage error (clap's exit 2, naming the allowed words). An unknown `--radio` id is
not an error: the resolver reports it as `focus <id> is not a source` and continues with no
focus, as for the page.

**MCP** runs no agent of ours: its client reads the guide's rule (section 4.7) and has no
context, so it behaves as precedence step 3.

### 4.4 The context on the wire and in the run

`api.ask(text, maxTurns, allow, context)` posts a new optional field:

```json
{"request": "...", "max_turns": 32, "allow": {...}, "page_tools": true,
 "context": {"focus": "b210", "main": "map", "radio_policy": "focus", "radio_prefer": ["b210", "hackrf"]}}
```

- `focus`: `state.selected`, or `null` before any source is listed.
- `main`: the view's `main` (`rf | map`); later non-radio views add values here.
- `radio_policy`, `radio_prefer`: the view's values (`[]` for `none`).

On the node, `AskRequest` gains `context: Option<AskContext>` (`#[serde(default)]`), and
`AskContext` is `deny_unknown_fields` with every field optional (`radio_prefer` defaults to `[]`).
A bad policy word is a 422 naming the field, as a bad `max_turns` is.

In `zeta-agent`, a new module `radio.rs` owns the mechanism:

- `RadioPolicy` (`focus | focus-fallback | free-first | ask`, serde `kebab-case`, `Default` focus,
  `FromStr` and `Display` with the same words, so `zeta`'s clap flag parses it with
  `value_parser` and `zeta-agent` takes no clap dependency).
- `RadioContext { focus: Option<SourceId>, focus_from: FocusFrom, policy: RadioPolicy, prefer:
  Vec<SourceId> }`, `FocusFrom = shown | kept | flag | none`, built from `AskContext` by `zeta-ws`
  (`main: map` gives `kept`, else `shown`) and from the flags by `zeta ask`.
- `resolve(request: &str, ctx: &RadioContext, sources: &[SourceInfo], caller: Caller) ->
  Selection`: precedence 4.1 over a fresh `source_list`. A focus that is not listed is dropped with
  the note `focus <id> is not a source`; a faulted focus stays the default (the model will see the
  fault and fall back under `focus-fallback`, or report it and ask under `focus`) and the note says
  `faulted`. `RadioContext::default()` (no context at all) has policy `free-first` (precedence
  step 3).
- `Selection` (section 5) is recorded on the `Transcript` and rendered into the system prompt.

`Agent::run` applies one `source_list` through its executor before the first completion, resolves
the selection, and appends a section to the system message. The pre-read is hidden from the
model and from `Transcript.calls` (so the prose rule of section 3 still counts only the model's
own calls, and no scripted fake conversation shifts), but it goes through the executor like any
op, so it is written to `ops.log` and the `ops` stream with transport `agent`: one more record at
the start of every run, which tests counting records must expect. A node error on it aborts the
run as any node error does; an op error leaves `selection` empty and the step-3 paragraph.

```
## This request

Default radio: b210 (policy focus: the radio last shown on the page; the Map is shown).
Radios in preference order: b210, hackrf.
When the request names no radio, work on b210 only. If b210 cannot take the band (it does not
cover it, a job or a pin holds it, or it is faulted), start nothing: say why, name the free radios
`source_free` gives in the order above, and ask which to use. Never `replace`.
```

Under `focus-fallback` the last two sentences read: use the first free radio `source_free` gives in
the order above, never `replace`, and say in your answer which radio you used and why b210 was not.

with one fixed paragraph per policy and reason (the four policies, plus "no radio in focus" and
"the request names b210"); the texts are constants in `radio.rs` and pinned by unit tests. A run
built without a context (the default `AgentConfig`) gets the step-3 paragraph, so every run states
its default, and `zeta ask` against a remote node (`RemoteExecutor`) works the same.

### 4.5 The gate

`Agent::call` already passes each op through `gate(op, allow)`. A second check beside it,
`Selection::admit(&op) -> Option<Value>`, looks at the ops that take a radio for work:
`RADIO_OPS = job_start, plan_start, source_ensure, source_pin, source_set_gain` (their `source`
field). Ops that only read (`source_list`, `source_free`, `classify`, `iq_read`) or release
(`source_unpin`, `job_stop`) are never checked; `bits_start` and `graph_start` take no radio.

- Under `focus` with a default radio, an op on another radio that the request did not name
  is refused, not applied: `{"error": "policy", "field": "source", "policy": "focus",
  "default": "b210", "message": "radio policy focus: hackrf is not the default radio b210;
  the operator names another radio in the request"}`. The model sees it as the call's result, as
  it sees an op error, and reports it.
- Under every other policy (`focus-fallback`, `free-first`, `ask`) the gate admits the op and records it in `selection.off_default` when
  its radio is not the default (with no default, nothing is off it).

`error: "policy"` is a word only the agent gate produces (never `Node::apply`), so the page's
unlock handler, which keys on `not_allowed`, never offers a re-run for it. The same function is
where an access check plugs in later (section 5).

### 4.6 Who sends what

- **The page**: every `askAgent` call (typed text, the microphone's transcript, the unlock
  re-run, the box menu) sends the context.
- **The box menu's "Send to agent"** names its radio in the text: `Look at <c> Hz, <bw> Hz wide,
  on <source>, from <start> to <end>: what is it?`. The named-radio rule makes it the default
  whatever the policy, and the transcript line shows it. The context's `focus` stays the page's.
- **`zeta ask`**: the flags, local or `--node` (the agent runs in the CLI either way).
- **A page-less `POST /ask`** (curl, a script): no context, precedence step 3. No caller of that
  shape needs a stored default today, so no `zeta serve --radio-policy` is added.

### 4.7 The guide

- A new rule in `RULES` (`agents_md.rs`), after the ids rule: "Radios: when the request names a
  radio, use it. Otherwise use the default radio the request's context names (`## This request`
  in the system prompt); with none, the first free id `source_free` gives. When the default
  cannot take the band (it does not cover it, a job or a pin holds it, it is faulted), follow the
  policy paragraph: under the default `focus`, start nothing, say why and ask which radio to use;
  under `focus-fallback`, use the first free radio and say which and why. Never stop or replace a
  job to free a radio."
- `source_free`'s note: its `free` list is in id order, which is no preference; the request's
  default and preference order come first.
- `docs/agents/field-craft.md`, the page paragraph: the default radio's mark, `view set
  radio_policy` and `radio_prefer`, that the box menu names its radio, and that the page notes a
  run on another radio.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerated.

### 4.8 The answer and the page's note

`Transcript` gains `selection` (section 5), so `/ask`'s answer and `zeta ask`'s output carry it.
After the answer line the page adds one note when `selection.off_default` is not empty:
`ran on hackrf (job_start), not the default radio b210 (focus-fallback)`; and, when the run's `selection`
note says the focus was dropped, `focus b210 is not a source; no default radio`. A `policy`
refusal shows as the call's result inside its turn, as other call results do, with no button.

### 4.9 The default radio's mark

The page computes the default it would send: under `focus` and `focus-fallback`, `state.selected`;
under `free-first` and `ask`, none. The tab of that radio gets class `default` and, first in the
tab, a `span.default-mark` (a small dot, `aria-hidden`), with the tab's title extended by
`default radio for the agent (policy focus)`. It is redrawn by `select`, by `syncMainTabs`, by a
change of `radio_policy`, and survives `renderTabs`.

- On the RF view the default is the selected tab, so the dot sits beside the selection. On the
  Map, the `kept` tab carries the dot: the operator sees where a request will run.
- Narrow footprint: the dot shows before the compact centre.
- CSS: `.src-tab .default-mark { width: var(--default-mark-size); height:
  var(--default-mark-size); border-radius: 50%; background: var(--default-mark); }` with tokens
  `--default-mark: var(--accent)` and `--default-mark-size: 6px`; hidden unless `.src-tab.default`.
  The page reads no colour from the theme beyond the tokens.
- `docs/themes.md`: `.src-tab.default` and `.src-tab .default-mark` in Regions, the two tokens in
  the token list; both selectors join the e2e's `THEME_HOOKS`.
- The page's own ask path is unchanged otherwise: typed `plan start` and `source pin` act on the
  shown radio as they do now (they are the operator's commands, not the agent's).

## 5. The permission seam

Access levels are a later design (TODO "Access: ACLs on restricted rows": an access level per
transport caller, restricted rows' events gated, the level stated in `GET /health`; plan 29
marks `pocsag_decode` and `flex_decode` events `access: "restricted"`). Transmit is later still
(TODO's VNA-style sweep: "an `allow`-class gate for TX"). This plan builds neither; it fixes the
shape they plug into.

```rust
pub struct Selection {
    pub radio: Option<SourceId>,        // the default for this request
    pub reason: Reason,                 // named | shown | kept | flag | none
    pub note: Option<String>,           // "focus b210 is not a source", "faulted", ...
    pub policy: RadioPolicy,            // the policy that chose it
    pub caller: Caller,                 // {transport: "page" | "http" | "cli"}
    pub candidates: Vec<SourceId>,      // the radios this caller may select, in preference order
    pub named: Vec<SourceId>,           // radios the request names
    pub off_default: Vec<OffDefault>,   // {tool, source}, filled during the run
}
```

- **(a) Which radios a caller may select.** `candidates` is every listed source today. An ACL
  filters it in `resolve` before anything else, and `admit` refuses an op on a radio outside it
  whatever the policy or the request names. The policy only orders and defaults within what the
  caller may use; it never widens it. The context is client-supplied, so it is a preference,
  never a permission: the ACL is applied on the node side of it.
- **(b) Transmit.** `Capabilities.tx` is already in `source_list` (all sources report `false`
  today). A later TX op joins `RADIO_OPS` marked as transmit, and `admit` then requires the
  caller's transmit level for it and for any radio whose `tx` is true when the op transmits.
  `focus` never makes a transmit op's radio implicit: the TX design decides whether a transmit
  request must always name its radio (the likely answer, noted in the TODO gist).
- **(d) Whose preference.** `radio_policy` and `radio_prefer` live in the browser today (4.3,
  decision 2). Once users exist they move to the user's record on the node, keyed by the caller,
  and the page's context becomes an override of it; the node's value is applied on the node side
  like the ACL.
- **(c) Who selected what.** The `Selection` is on every transcript today. With caller identity,
  `Caller` gains the identity and the level, and the selection is appended to `ops.log` beside the
  ops it led to (`ops.log` already tags `transport: "agent"`).
- `admit` is the one function every radio-taking agent op passes; it is where the ACL check goes.
  A direct op from the page, the CLI or MCP never meets it today; the ACL design puts the same
  check in `Node::apply_from` for those, keyed by the transport caller.

## 6. Testing

### 6.1 `zeta-agent` unit (`src/radio.rs` tests)

- `resolve` table: a named radio wins under every policy; two named keep the policy's default;
  a whole-word match only (`b2100` does not name `b210`), case-insensitive; `focus` with `main`
  rf gives `shown`, with map `kept`; a focus not listed is dropped with the note and behaves as
  `free-first` (under `focus`: as `ask`); a faulted focus stays the default with the note; `free-first` and `ask` have no
  default; `candidates` follow `prefer` then id order, skipping unknown ids; no context
  gives policy `free-first` (`RadioContext::default()`); a context that omits the policy means
  `focus` with a focus and `free-first` without.
- `admit`: `focus` refuses `job_start`, `plan_start`, `source_ensure`, `source_pin` and
  `source_set_gain` on another radio not named (the error shape above), admits the default and a
  named radio; never checks `source_list`, `source_free`, `source_unpin`, `classify`;
  `focus-fallback` admits and records `off_default`; no default records nothing.
- The prompt paragraphs: one per policy and reason, each naming the radio and the fallback rule.

### 6.2 `zeta-agent` with the fake model (`tests/agent.rs`, feature `test-fake`)

On a node with two file sources given tune ranges (the existing `tone_node` helper extended):

- the first chat request's system message ends with `## This request` naming the default and the
  user message is exactly the request;
- one `source_list` is applied before the first completion (the executor's log);
- under `focus` (the default) a scripted `job_start` on the other source comes back as the `policy`
  error in the next request's tool message, and nothing started (`job_list` empty);
- under `focus-fallback` the same call starts the job and the transcript's `selection.off_default`
  names it;
- `ask` with a scripted `source_free` then prose ends with an answer (the loop's rule holds);
- a run without a context states the step-3 paragraph (policy `free-first`);
- under `focus-fallback`, with the other source held and the default not covering the band, a scripted
  `source_free` (empty `free`) then prose ends with an answer and nothing started.

### 6.3 `zeta-ws` (`tests/ask.rs`) and `zeta` (`tests/cli.rs`)

- `/ask` with `context` runs and the answer carries `selection`; with an unknown field inside
  `context` it is a 422 naming it; with `radio_policy: "nearest"` a 422 naming `radio_policy`;
  without `context` it runs as before.
- `zeta ask --radio-policy nearest` exits 2 naming the allowed words; `--radio`, `--radio-policy`
  and `--radio-prefer` parse, and their environment variables are read.

### 6.4 The page (`crates/zeta-ui/tests/e2e/ui.py`)

Sparingly, on the LLM node (`tone`, `cb`):

- extend `scenario_ask_sends_32_turns_and_the_allowance`: the posted body's `context` is
  `{"focus": "cb", "main": "rf", "radio_policy": "focus", "radio_prefer": []}` (the defaults, with
  storage empty);
- one new scenario, `scenario_radio_default_mark_and_policy`:
  - with `cb` selected, `.src-tab.default` is the `cb` tab with the hover title; after the Map
    tab, the `kept` `cb` tab still carries it and the next ask's `context.main` is `map`,
    `focus` `cb`;
  - with the default `focus`, a request scripted to `job_start` on `tone` ("radio fallback
    probe") comes back as the `policy` refusal in the turn, no unlock button shows, no job started
    on `tone`;
  - `view set radio_policy focus-fallback` and the same request: the note `ran on tone
    (job_start), not the default radio cb (focus-fallback)` shows;
  - persistence: after `view set radio_policy focus-fallback` and `view set radio_prefer
    tone,cb`, a second page in the same browser context (a new tab) reads both in `view show`
    and its next ask posts them, while its shown radio is its own (`zeta.source` is per tab); a
    reload keeps them; `view reset` clears both. With `localStorage` made to throw
    (`add_init_script` replacing `Storage.prototype.getItem` and `setItem` for that page) the page
    boots with the defaults and `view set` still answers ok;
  - `view set radio_policy free-first`: no tab carries `.default`;
  - `view reset` and `localStorage.removeItem("zeta.radio_prefs")` in `finally`.
- the box menu's "Send to agent" text names the radio: one assertion added to the existing
  menu-ask coverage (the scenario that sends a selection to the agent), else to the new scenario.
- `THEME_HOOKS` gains `.src-tab.default` and `.src-tab .default-mark`; `scenario_theme_hooks`
  needs a page with a default radio, which it has (`focus` with a shown source).
- The new scenario goes in shard 2's half, before `fm_demod_does_not_poll_streams`, and stays
  under 20 s; `SHARD_STARTS` is unchanged unless the timings say otherwise.
- The fake model's scripts (`fake_llm_scripts` in `crates/zeta/tests/ui.rs`) gain "radio fallback
  probe": `job_start` `measure` on `tone`, then `Started it on tone.`.

### 6.5 UAT (the field host, B210 and HackRF both attached)

1. With the B210 tab selected, ask "start APRS on 144.8": the job runs on the B210; the B210 tab
   carries the dot.
2. Switch to the Map tab and ask the same for 145.175: it runs on the B210 (the `kept` tab with
   the dot).
3. With the B210 held by a watch, ask for a band both radios cover: under the default `focus` the
   agent starts nothing, says the B210 is held and asks which radio to use; after `view set
   radio_policy focus-fallback` it uses the HackRF and says why, and the page notes it. Then ask
   for a band only the B210 covers: under either it starts nothing and names the watch.
4. "start APRS on 144.8 on the hackrf" runs on the HackRF under every policy.
5. `view set radio_policy ask`: "start APRS on 144.8" lists the free radios and asks.
6. Box menu "Send to agent" on the HackRF's waterfall: the request text names `hackrf`.
   Persistence: set `radio_policy focus-fallback`, open a second tab and restart the browser; both
   keep it, and each tab keeps its own shown radio.
7. A theme with its own accent shows the dot in that accent.

## 7. Docs, TODO and UAT

- `docs/guide.md`: the view-policy table rows `radio_policy` and `radio_prefer`; the `zeta ask`
  flags.
- `docs/themes.md`: section 4.9's hooks and tokens.
- `docs/agents/field-craft.md`, `agents_md.rs`, `AGENTS.md`, `SKILL.md`: section 4.7.
- `TODO.md`: extend "Access: ACLs on restricted rows" with the radio seam (candidates filtered,
  `admit` the check, transmit requires a level and probably a named radio, selections in
  `ops.log`); a gist for matching radios by driver or role in `radio_prefer` and a node-stored
  per-caller policy (the browser's stored `radio_policy` and `radio_prefer` move to the user's
  record on the node when users exist).
- `UAT.md`: "Plan 33: radio selection policy" with 6.5's checks.
- The plan series: row 33 and a status section.

## 8. Left to the plan

Task boundaries, the order (the resolver and gate first, then the wire, then the page, then the
guide), exact constant names, and which existing menu scenario takes the "names its radio"
assertion.

## 9. Questions from the builder check (folded in)

A reviewer read the first draft as the builder. Its questions, most important first, and where
each went:

1. *Are the operator questions answered, the storage one above all?* Yes, both, on 2026-11-09
   (section 11): stop and ask by default (`focus`), and the two keys persist in the browser
   (`localStorage`, 4.3). The page task builds the persist branch; the node side is unchanged.
2. *Is the pre-read `source_list` visible to the model, in `calls`, in `ops.log`?* Answered in
   4.4: hidden from the model and from `calls`, applied through the executor and so logged.
3. *Section 9 was empty in the draft.* This list.
4. *UAT 3 asked for a fallback onto a radio that cannot cover the band.* Corrected: the fallback
   check uses a band both radios cover, and a second step checks the no-free-radio case, now
   stated in 4.1 (start nothing, report why, never `replace`) and tested in the agent tests.

Decided by the builder without asking: how the context reaches `Agent::run`, telling a `page`
caller from an `http` one (`page_tools`), whether named radios count as off the default (they do
when they are not the default; the note is information, not blame), a failing pre-read (4.4),
which context the unlock re-run sends (the one at the time of the click), clearing the probe job
between e2e steps. The e2e fixture's "every CB channel" now names the `cb` source under the
whole-word rule; that request already runs on `cb`, so nothing changes.

## 10. Calls made in this document (check these while reading)

1. Four policies plus an orthogonal preference list, not a fifth `prefer` policy: the list
   composes with each policy.
2. `focus` uses the focused radio only, else stops and asks, and the gate refuses another radio;
   `focus-fallback` is the opt-in fall-back to a free radio, said aloud. The default is `focus`
   (decision 1 in section 11). No context at all stays `free-first`.
3. A named radio is a whole-word, case-insensitive source id in the request text; one named
   radio becomes the default, several leave the default and are all admitted.
4. No node-wide "last used radio" (4.2); `zeta ask --radio` stands in for the CLI.
5. The policy is page view state, persisted in the browser's `localStorage` (decision 2) with no
   theme default and no agent key; `zeta ask` has flags
   with environment variables; no `zeta serve` default.
6. The context is appended to the one system message as `## This request`, not a second system
   message and not a prefix to the user message.
7. The default is resolved by code before the first completion, from one `source_list`; the band
   fallback under `focus-fallback` is the model's, made visible by `off_default` and the page's note.
8. The gate refuses only under `focus`, with the agent-only error word `policy`, so the
   page's unlock flow never mistakes it for an allowance.
9. The box menu names its radio in the request text rather than in a separate context field: the
   named-radio rule already covers it, and the transcript shows it.
10. The mark is an accent dot (`span.default-mark`) with a hover title, two theme tokens.
11. A faulted focus stays the default (the model sees the fault and falls back under
    `focus-fallback`, or under `focus` reports it and asks); an unlisted focus is dropped.
12. `radio_prefer` ids are not checked against `source_list` on the page; unknown ids are skipped
    by the resolver.

## 11. The operator's decisions (2026-11-09)

1. *When the focused radio cannot take the band, fall back or stop and ask?* **Stop and ask.** The
   default policy `focus` is therefore the strict behaviour: the focused radio only; when it
   cannot take the band the agent says why, names the free radios and asks which to use, starting
   nothing, and the agent-side gate (error word `policy`) refuses an op on an unnamed other radio.
   The fall-back (the focus, else the first free radio, saying which and why) stays available as
   the opt-in policy `focus-fallback`. A request with no context at all (a page-less `/ask`,
   `zeta ask` without radio flags) keeps today's `free-first`, for compatibility (4.1).
2. *Per tab, or persist?* **Persist for the user or session; there are no users yet, so a browser
   cookie for now.** `radio_policy` and `radio_prefer` persist across tabs and browser restarts in
   the browser. The page's conventions are Web Storage with try/catch rather than cookies, so the
   store is `localStorage` (4.3); the effect the operator asked for is the same. The shown radio
   stays per tab. Once users exist the preference moves to the user's record on the node, keyed by
   the caller (section 5).
