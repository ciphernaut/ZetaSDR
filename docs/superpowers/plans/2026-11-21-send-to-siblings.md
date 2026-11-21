# Send to and the Merged, Marked Subject (Plan 43) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From a running decoder, start another handler of the same mode on the same band without disturbing it, and show what both hear as one subject whose fields say which handler gave them.

**Architecture:** `job_start` gains `beside`; the node checks the request against the named job, starts an ordinary rigid job and writes one `group` key on both records. A group retunes as one lease move and relaunches member by member. The page's subject store keeps, beside each subject's latest values, what each source (`<mode>@<handler>`) said; one pure function turns that into per-field marks, drawn from theme tokens in the Subjects table, the map popup and a legend. Menus and the job list read the mode's `handlers`.

**Tech Stack:** Rust 1.97 (zeta-core, zeta, zeta-source-node), the plain-JS page (`crates/zeta-ui/web`), CSS themes, Python Playwright scenarios (`crates/zeta-ui/tests/e2e/ui.py`) started by `crates/zeta/tests/ui.rs`.

**Spec:** `docs/superpowers/specs/2026-11-21-send-to-siblings-design.md` (the calls, numbered 1 to 29; cited below as "call N"), on `docs/superpowers/specs/2026-11-21-alternative-handlers-design.md` sections "Send to: a sibling beside a running job" and "One merged subject, marked". Read both.

## Global Constraints

- One operation set: every new field is in `crates/zeta-core/src/ops.rs` and applied by `Node::apply`; transports only translate.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: prose goes in `crates/zeta/src/agents_md.rs` or `docs/agents/field-craft.md`, then regenerate; never hand-edit.
- A request without `beside` and a record without `group` behave exactly as before: every existing test passes unchanged except where a task says otherwise.
- The request field is `beside`; the record field is `group`. `plan_start`'s and `jobs_stop`'s `group` are a channel plan's and are not touched.
- `beside` is `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a start without it still works against an older node. `group` on the record is `#[serde(default)]` and always written (the record rule in `job.rs`).
- A sibling start makes no tune and stops nothing. Its failure leaves every other job as it was.
- A group never holds one handler twice: not at start, not after a relaunch.
- The source key of a subject field is `<mode id>@<handler id>`; an event without `handler` counts as the row's own handler.
- Marks: classes `mark-1` to `mark-4` and `mark-differs`; tokens `--c-handler-1..4`, `--handler-style-1..4`, `--c-differs`. A marked value's `title` always names its handler. Envelope fields (`protocol`, `handler`, `summary`, `t_ns`, `position`, `access`) are never marked.
- Numbers and booleans are never compared between sources; only strings can be `differs`.
- Public repository: roles not host names, no observed traffic, no real aircraft addresses; fixtures use synthesised lines. `scripts/public-check.sh` and `scripts/check-doc-links.sh` clean before the branch lands.
- Gates per `CLAUDE.md`: `cargo fmt -p <crate> -- --check` per crate (nested worktree), clippy `-D warnings`, a private `CARGO_TARGET_DIR`. Per task run only its tests and neighbours; crate suites once per batch; the workspace and UI suites once at the end.
- Tests that read state right after an operation poll for the end state (`common::until`, ui.py `wait_until`), never read once.
- Commit messages: `Send to: <what changed>`.

## Review Focus

- `beside` names a job that stops while the sibling starts, or has already stopped: refused (`job <id> is stopped`) with nothing started and no `group` written on any record. (Task 1)
- A group retune that conflicts with a holder outside the group: every member keeps its old lease, band and run; the error names only outside holders. (Task 3)
- A restart where the first member's chosen handler is gone and the only other ready handler is the one its sibling runs: that member is `failed` with a reason naming the sibling; the sibling runs. (Task 2)
- A subject one source heard for a minute before a second source joins, and a member that stops while its sibling runs: marks appear when the second source first supplies the subject; the stopped member's values and marks stay while the group lives. (Tasks 6, 7)
- Events from an older node (no `handler`), a theme with none of the new tokens, and a `theme.json` pinning a handler to slot 0 or 5: the first two draw with the row's own handler and the base defaults; the third is refused at load naming `handlers.<id>`. (Tasks 5, 6)

---

### Task 1: `job_start.beside` and the record's `group`

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`Op::JobStart`, `require_the_look`, the `JobStart` arm, a new `check_beside`), `crates/zeta-core/src/job.rs` (`JobRecord`, `require_format`), `crates/zeta-core/src/node/mod.rs` (`StartHandler`, `running_record`, `job_start_gated`, new group helpers), every `JobRecord { .. }` and `Op::JobStart { .. }` literal that lists all fields (src and tests of zeta-core, `crates/zeta/src/{cli.rs,main.rs,modes_check.rs}`, `crates/zeta-source-node/src/port.rs`, `crates/zeta/tests/resume.rs`)
- Test: `crates/zeta-core/tests/jobs.rs`, `crates/zeta-core/src/job.rs` unit test, `crates/zeta-core/tests/common/mod.rs` (`handlers` helpers)

**Interfaces:**
- Produces:
  - `Op::JobStart { .., beside: Option<JobId> }`, last field, schema-required with no default through `require_the_look` (extend `HANDLER_FIELDS` to `["handler", "needs", "beside"]`).
  - `JobRecord.group: Option<JobId>`, `#[serde(default)]`, nullable schema, added to `require_format`'s list.
  - `StartHandler.group: Option<JobId>`; `job_start_gated` writes it to the record.
  - `Node::group_members(&self, group: &JobId) -> Vec<JobRecord>`: the running records whose `group` is it, oldest first.
  - `Inner::set_group(&self, id: &JobId, group: &JobId) -> Result<(), NodeError>`: sets the running entry's record's `group` and persists it.
  - `common::handlers::start_beside(node, beside: &JobId, handler: Option<&str>, needs: &[&str]) -> Result<OpResult, OpError>`, which copies kind, source and band from the job's record.

The rules, all checked under the source's gate after the handler is resolved and before the duplicate check; each refusal is `OpError::Other` unless said:

| Case | Answer |
|---|---|
| `beside` unknown | `unknown_job` |
| the job is not `Running` | `job <id> is <status>` |
| attached, owned, elastic, or its kind is no mode row | `job <id> cannot be sent from: <it is attached to job X / it is run by <kind> X / it is elastic / <kind> has no handlers>` |
| `replace` not empty | `beside: replace must be empty (Send to stops nothing)` |
| `kind`, `source`, `center_hz`, `bandwidth_hz` differ, or `deviation_hz`/`sideband` not `null`, or `elastic` | `beside job <id>: <field> is <asked>, the job's is <its>` (first differing field in that order) |
| `handler` is `null` | `beside needs a handler: one of <ready ids the group does not run, comma separated>` (`none is free` when empty) |
| the resolved handler is run by a running member of the group (the named job when it has no group yet) | `OpError::Duplicate { job: <that member>, message: "handler <h> already runs beside job <beside> as job <member>" }` |

A record with `handler: None` runs the row's own (`LoadedMode::own_handler()`), as `duplicate_of_as` reads it.

The band compared is the named job's record's `tuning` (call 21). After the group's duplicate check the ordinary duplicate check still runs, so a job outside the group running that handler on the band is a `duplicate` naming it (call 28).

On success: the group id is the named job's `group`, or its own id when it has none. The new record carries it from creation. When the named job had none, `set_group` writes it on the named job **after** the sibling has launched, under the same gate (call 22): a failed start writes nothing. The group id is a bare key and is never resolved to a record (call 23). The start goes through the existing rigid path; because the band equals a held lease's, `tune_if_unheld` must report no tune (assert it in the test rather than adding a shortcut; if it would tune, lease the band with `ensure_locked` and skip the tune).

- [ ] **Step 1: Write the failing tests** in `tests/jobs.rs`, using `common::handlers` (`MODE`, `OWN`, `ALT`, `RADIO`, `start`, `cfg(dir, false)`), plus one unit test in `job.rs`:

```rust
// tests/jobs.rs
#[tokio::test] async fn a_sibling_starts_beside_a_running_job_and_both_carry_the_group()
//   first = start(&node, None, &[]); sib = start_beside(&node, &first, Some(ALT), &[]) -> Job(id)
//   until both records Running; record(first).group == Some(first); record(sib).group == Some(first)
//   record(sib).handler == Some(ALT), handler_named == true; radio.tunes() unchanged by the second start
//   on_disk(first).group == Some(first)
#[tokio::test] async fn send_to_from_a_sibling_joins_its_group()
//   a third handler (write_handler(dir, "alt2", false)) started beside `sib`: group == Some(first)
#[tokio::test] async fn beside_refuses_a_handler_the_group_already_runs()
//   start_beside(first, Some(OWN)) == Err(Duplicate { job: first, message contains "already runs beside job" })
//   and after ALT runs: start_beside(first, Some(ALT)) == Err(Duplicate { job: sib, .. })
#[tokio::test] async fn beside_without_a_handler_names_the_free_ones()
//   Err(Other) message == format!("beside needs a handler: one of {ALT}")
#[tokio::test] async fn beside_refuses_a_band_that_is_not_the_jobs()
//   same op with center_hz + 1000: message == "beside job <id>: center_hz is 1090001000, the job's is 1090000000"
#[tokio::test] async fn beside_refuses_a_stopped_job_and_writes_no_group()
//   stop first; start_beside -> Err "job <id> is stopped"; record(first).group == None; job_list has one record
#[tokio::test] async fn stopping_the_first_member_leaves_its_sibling_running_in_the_group()
//   stop first; until first Stopped; record(sib) Running with group Some(first); node.group_members(&first) == [sib]
#[tokio::test] async fn a_failed_sibling_start_leaves_the_first_job_as_it_was()
//   start_beside(first, Some(ALT_MISSING)) -> Err (unavailable); record(first) Running, group None, same tuning
// job.rs
#[test] fn a_record_without_group_loads_and_writes_null()   // extend a_record_without_input_loads_as_on_the_radio: written["group"] == Null
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p zeta-core --test jobs -- beside sibling send_to` — FAIL (no field `beside`).
- [ ] **Step 3: Add the fields** (`ops.rs`, `job.rs`, `StartHandler`, `running_record` with `group: None`) and fix every literal the compiler names. Doc comments: `beside` says what it does, each refusal's wording and "the record field is `group`"; `group` says "the id of the job the first Send to named; not `plan_start`'s group".
- [ ] **Step 4: Implement `check_beside`, `group_members`, `set_group` and the `JobStart` arm's use of them.**
- [ ] **Step 5: Run.** The Step 2 command, then `cargo test -p zeta-core --test jobs --test replace --test resume` and `cargo test -p zeta-core job::` — PASS.
- [ ] **Step 6: Commit.** `Send to: job_start takes beside, and siblings share a group`

### Task 2: A relaunch never doubles a handler, and history keeps one record per handler

**Files:**
- Modify: `crates/zeta-core/src/node/recover.rs` (`resolve_for_relaunch`), `crates/zeta-core/src/node/mod.rs` (the history prune's key, near line 948)
- Test: `crates/zeta-core/tests/resume.rs`, `crates/zeta-core/tests/jobs.rs`

**Interfaces:**
- Consumes: `JobRecord.group`, `Node::group_members` (Task 1).
- Produces: `resolve_for_relaunch` passes to `pick` only the handlers no other member of `rec.group` holds. "Holds" means a record with that `group`, another id, state `Running` or about to be resumed in this pass, whose handler (or the row's own) is that id. A passed-over handler's reason is `runs beside it as job <id>`, carried in the `no_handler` text that becomes the failure reason.

Members resume in `created_ns` order within their sort class, so the first member settles first; a sibling's handler is always named and is never re-chosen (call 4). The check must read the other members' *records*, not only running entries, because at a restart the sibling may not have launched yet.

The prune key becomes `(kind, source, center_hz, bandwidth_hz, handler-or-own)`: a record with no `handler` takes the row's own (`LoadedMode::own_handler()`, the mode registry read where the prune runs), so an old record and a new one of the row's own handler share a key (call 27). The same filter serves a restart, `recover_source` and a retune (call 24).

- [ ] **Step 1: Write the failing tests.**

```rust
// tests/resume.rs, beside a_chosen_handler_gone_at_restart_is_chosen_again_and_noted
#[tokio::test] async fn a_rechosen_member_never_takes_its_siblings_handler()
//   node with OWN and ALT; first chosen (OWN), sibling named ALT; close; OWN goes missing (alt_goes_missing's
//   counterpart for OWN: add `own_goes_missing` to common::handlers if absent); reopen with resume
//   until first is Failed; reason contains "runs beside it as job"; sibling Running on ALT
#[tokio::test] async fn a_group_resumes_member_by_member()
//   both handlers present at reopen: both Running, both group == Some(first)
// tests/jobs.rs
#[tokio::test] async fn a_stopped_job_of_another_handler_keeps_its_record()
//   start OWN, start ALT on the same band, stop both: job_list holds both terminal records
```

- [ ] **Step 2: Run to see them fail.** `cargo test -p zeta-core --test resume -- member group_resumes` and `--test jobs -- keeps_its_record`.
- [ ] **Step 3: Implement** the filter in `resolve_for_relaunch` and the prune key.
- [ ] **Step 4: Run** the same, then `cargo test -p zeta-core --test resume --test recover` — PASS.
- [ ] **Step 5: Commit.** `Send to: a relaunch never puts two members on one handler`

### Task 3: A group retunes as one

**Files:**
- Modify: `crates/zeta-core/src/lease.rs` (new `retune_group_locked` and unit tests), `crates/zeta-core/src/node/retune.rs` (`Node::job_retune`), `crates/zeta-core/src/ops.rs` (`job_retune`'s refusal and doc)
- Test: `crates/zeta-core/tests/retune.rs` (add the `common::handlers` rig or a local two-handler one)

**Interfaces:**
- Consumes: `Node::group_members` (Task 1); `restarts_then_gate`, `hold_attached`, `relaunch_in_place`, `relaunch_attached`, `after_gate` as `move_capture` uses them (`retune.rs:174-316`).
- Produces:

```rust
/// Move every lease `holders` hold on `port` to `want` under the gate the caller holds:
/// their leases are set aside together, `want` is granted once as `retune_locked` grants
/// it, then leased to each holder. On an error every set-aside lease is put back.
pub(crate) async fn retune_group_locked(&self, port: &dyn Port, holders: &[JobId],
    want: Tuning, guard: &OwnedMutexGuard<()>) -> Result<(Tuning, bool), LeaseError>
```

`Node::job_retune` on a record whose `group` has more than one running member takes the group path, in call 25's order: the members' restart locks then the gate (a group-only variant of `restarts_then_gate`, covering exactly the members running once the gate is held, retrying until that set is covered; other holders of the source are not locked); `rehandle` each, oldest first, before any lease is touched (a member that cannot be settled, named handler gone or no free handler, refuses the whole retune naming that member, nothing moved); `hold_attached` for every member (call 26); `retune_group_locked` (one tune at most); re-expand each; `relaunch_in_place` each under the one guard, named job first, collecting `AfterGate`s; after the guard drops, `relaunch_attached` for all. A member whose re-expansion or relaunch fails is `Failed` with its reason and the others stand. If the port was tuned, `relaunch_elastic` once. If every member fails, `idle_if_unheld`. The op answers `Job(<the named id>)`.

`job_retune` with `replace` naming a member is refused before the gate: `replace names job <id>, a member of the retuned job's group`.

- [ ] **Step 1: Write the failing tests.**

```rust
// lease.rs unit tests
#[tokio::test] async fn retune_group_locked_moves_every_holder_and_tunes_once()
//   two holders on band A, nobody else: move to B outside the capture -> Ok((_, true)); holders() == [(a,B),(b,B)]; port tuned once
#[tokio::test] async fn retune_group_locked_restores_every_lease_on_a_conflict()
//   a third rigid holder on A: move to B outside -> Err(Conflict { holders == [third] }); holders() unchanged
// tests/retune.rs
#[tokio::test] async fn retuning_one_member_moves_the_whole_group()
//   first + sibling; retune(sibling, new band outside the capture, &[]) -> Ok; until both Running at the new tuning; ids unchanged
#[tokio::test] async fn a_group_retune_conflicts_only_with_holders_outside_it()
//   plus an fm_demod elsewhere in the capture: retune outside -> Err(Conflict) whose holders == [fm id];
//   both members still Running at the old tuning
#[tokio::test] async fn a_member_that_fails_its_relaunch_leaves_the_others_standing()
//   make ALT fail at relaunch (the rig's stage factory refusing the second launch of ALT): first Running at new band, sibling Failed with a reason
#[tokio::test] async fn a_packet_capture_follows_its_member_through_a_group_retune()
//   packets attached to the sibling: after the retune its record's tuning equals the sibling's and it is Running
#[tokio::test] async fn replace_may_not_name_a_member_of_the_group()
```

- [ ] **Step 2: Run to see them fail.** `cargo test -p zeta-core lease::tests::retune_group` and `cargo test -p zeta-core --test retune -- group member`.
- [ ] **Step 3: Implement `retune_group_locked`.**
- [ ] **Step 4: Implement the group path in `Node::job_retune`** and the refusal; extend `job_retune`'s op doc with one paragraph on groups.
- [ ] **Step 5: Run** the Step 2 commands, then `cargo test -p zeta-core --test retune --test attach --test widen` — PASS.
- [ ] **Step 6: Commit.** `Send to: a retune moves the whole group under one gate`

### Task 4: The CLI, the agent's prose, the changelog

**Files:**
- Modify: `crates/zeta/src/cli.rs` (`JobCmd::Start`), `crates/zeta/src/main.rs` (filling a `--beside` start from the job), `crates/zeta/src/agents_md.rs` (`note("job_start")`, `note("job_retune")`, `hint`), `docs/agents/field-craft.md`, `CHANGELOG.md`, regenerated `AGENTS.md` and `skills/zetasdr/SKILL.md`
- Test: `crates/zeta/tests/cli.rs`

**Interfaces:**
- Consumes: `Op::JobStart.beside` (Task 1).
- Produces: `zeta job start --beside <JOB> --handler <ID> [--needs packet]`. With `--beside`, the kind, `--source`, `--center` and `--bandwidth` arguments become optional (clap `required_unless_present = "beside"`); the CLI reads the job (`job_list` through whichever transport the command uses, local state or `--node`) and fills what was left out. A given value is sent as given, so the node's own refusal answers a mismatch. An unknown job is the CLI's error `no job <id>`.

Prose (one paragraph each, in the house style of the neighbouring notes):
- `job_start` note: what `beside` does, that it needs `handler`, that nothing is tuned or stopped, the `group` on both records, the CLI form.
- `job_retune` note: a member moves its group.
- `hint("job_start", "beside")`.
- `field-craft.md`, under the handlers section: comparing two decoders (start one beside the other, read each job's events, a silent decoder beside a busy one is the decoder, not the band); a capture from a handler that gives packets when the running one gives none.
- `CHANGELOG.md`: the feature, and "a node rolled back past this build resumes no jobs (records carry `group`)".

- [ ] **Step 1: Write the failing tests** in `tests/cli.rs`, beside `job_start_handler_and_needs_reach_the_op`:

```rust
#[test] fn job_start_beside_fills_the_band_from_the_job()
//   start a job; `job start --beside <id> --handler alt`; last_logged_op: op["beside"] == id, kind/source/center_hz/bandwidth_hz equal the job's
#[test] fn job_start_beside_of_an_unknown_job_is_an_error_naming_it()
```

- [ ] **Step 2: Run to see them fail.** `cargo test -p zeta --test cli -- beside`.
- [ ] **Step 3: Implement the flag and the fill-in.**
- [ ] **Step 4: Write the prose, then regenerate:** `cargo run -q -p zeta -- agents-md > AGENTS.md` and `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`.
- [ ] **Step 5: Run.** The Step 2 command; `cargo run -q -p zeta -- agents-md --check`; `cargo run -q -p zeta -- agents-md --skill --check`; `cargo test -p zeta --test agents_md` — PASS.
- [ ] **Step 6: Commit.** `Send to: the CLI's --beside, and the agent's notes`

### Task 5: Theme tokens for marks, and `theme.json`'s `handlers`

**Files:**
- Modify: `crates/zeta-core/src/themes/tokens.rs`, `crates/zeta-core/src/themes/theme_json.rs` (`ThemeJson`, `FIELDS`, validation), `crates/zeta-ui/web/style.css` (`:root`, the mark rules), `themes/{console,cyberpunk,terminal,psytrance,vibecoder}/theme.json` or `style.css` (wherever each sets its tokens today), `docs/themes.md`
- Test: `crates/zeta-core/tests/themes.rs`

**Interfaces:**
- Produces:
  - Tokens `--c-handler-1`, `-2`, `-3`, `-4`, `--handler-style-1`, `-2`, `-3`, `-4`, `--c-differs` in `TOKENS`, `:root` and the docs. Base defaults: styles `normal`, `italic`, `normal`, `italic`; colours four that differ from each other and from `--text` on `--bg`; `--c-differs` distinct from all four.
  - `ThemeJson.handlers: BTreeMap<String, u8>` (`#[serde(default)]`), key a handler id (`^[a-z0-9][a-z0-9_-]*$`), value 1 to 4; else `ThemeError::new("handlers.<id>", "a slot is 1 to 4")` / `"not a handler id"`.
  - CSS, in `style.css`:

```css
.mark-1 { color: var(--c-handler-1); font-style: var(--handler-style-1); }
/* ... -2, -3, -4 ... */
.mark-differs { color: var(--c-differs); text-decoration: underline dotted; }
```

- Each built-in theme sets the five colours to fit its palette (`plain` keeps the base defaults). The one light theme (`vibecoder`) must keep them readable on its `--bg`.

- [ ] **Step 1: Write the failing tests** in `tests/themes.rs`:

```rust
#[test] fn theme_json_takes_handler_slots_and_refuses_a_slot_out_of_range()
//   {"handlers": {"onair": 2}} parses, .handlers["onair"] == 2; {"handlers": {"onair": 5}} -> error field "handlers.onair";
//   {"handlers": {"On Air": 1}} -> error field "handlers.On Air"
#[test] fn builtin_mark_colours_meet_3_to_1_on_the_background()
//   for every built-in theme, each of --c-handler-1..4 and --c-differs (resolved through the base when unset)
//   has contrast >= 3.0 against --bg, using the file's existing contrast helper
```

`tokens_match_the_core_stylesheet` and `docs_list_every_token` already fail until the tokens are in all three places.

- [ ] **Step 2: Run to see them fail.** `cargo test -p zeta-core --test themes`.
- [ ] **Step 3: Add the tokens, the field, the CSS, the themes' colours and the docs** (`docs/themes.md`: the nine tokens in the token list, a "Handler marks" paragraph, the `handlers` key with the `{"onair": 2}` example, `.mark-N`, `.mark-differs`, `.subject-legend` among the documented hooks).
- [ ] **Step 4: Run.** `cargo test -p zeta-core --test themes` — PASS.
- [ ] **Step 5: Commit.** `Send to: theme tokens for handler marks, and theme.json's handlers`

### Task 6: The subject store keeps what each source said

**Files:**
- Modify: `crates/zeta-ui/web/subjects.js` (the store, new exports, the header doc), `crates/zeta/tests/ui.rs` (the `fake_pair` fixture: a row in `fake_modes()` and an entry in `fake_handlers()`)
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Consumes: the job record's `group`, `handler`; the event's `handler`; the row's `handlers` (already on the store's rows).
- Produces, in `subjects.js`:

```js
// "<mode>@<handler>"; the row's own handler when the event names none.
export function sourceKey(job, p, row)
// What each drawn field of subject `s` should look like. `slots`: source key -> 1..4.
// Returns Map<field, { mark: null | 1 | 2 | 3 | 4 | "differs", title: string | null }>.
export function marksOf(s, slots)
```

  - A subject gains `by`: `Map<sourceKey, { mode, handler, fields: { [field]: { v, t } } }>`, filled in `event` beside the existing `state` update (which does not change).
  - `store.groupId(jobId)`: the watch's group when there is one, else the record's `group`, else the job's id. `sync` keeps a sibling group's subjects while any member runs, and moves subjects heard under a job's own id into the group when its record first shows one (the move a watch's decoder already gets).
  - `store.sources(groupId)`: `[{ key, mode, handler, name, slot }]` in the order the sources first supplied a field; `name` is the handler's `name` from the row's `handlers` (the id when unknown); `slot` is the theme's pin for that handler id, else the next free of 1 to 4, the fifth and later sharing 4.
  - `store.setHandlerSlots(map)`: the theme's `handlers` object; calling it re-slots and fires `onChange`.
  - `store.heardBy(jobId)`: how many subjects of the job's group have a `by` entry for that job's source key.
  - The merged-group builder (`groupOf`, `subjects.js:628-659`) gives its merged subject a `by` that is the union of its members' `by` maps, so `marksOf` works on it unchanged.

`marksOf`, exactly (call 15): let `S` be the sources in `s.by`. With fewer than two, every field is `{mark: null, title: null}`. Otherwise, for a field: `G` = the sources holding it. A string field held by two or more sources whose current values differ (trimmed, case kept) is `differs`, title `"<name A>: <value> · <name B>: <value>"` in slot order. Else `|G| == |S|` is plain with no title; `|G| == 1` is that source's slot, title `only <name>`; anything between is plain with title `from <name>, <name>`. Envelope fields and the subject's key are always plain.

The fixture: mode `fake_pair` (subject key `id`, name `pairs`, columns `name`, `own_only`, `alt_only`, `level`; band and channel like `fake_subjects`), its own program `fake_lines` printing `{"id":"PAIR1","name":"Alpha","own_only":"x","level":1}`, and in `alt.json` a `fake_pair` entry printing `{"id":"PAIR1","name":"Alfa","alt_only":"y","level":2}`. Shape the row's event schema so both lines are valid (the fields one program lacks are optional). Both repeat (`$FAKE_REPEAT_BYTES`) so either can be heard first.

- [ ] **Step 1: Write the failing scenario** `scenario_subject_store_keeps_each_sources_fields`, an in-page unit test in the style of `scenario_subjects_store_merge_units_and_fill` (fake `api`/`view`, `import('./subjects.js')`). It asserts:
  - `sourceKey({kind:"m"}, {handler:"alt"}, row) === "m@alt"`; with no `handler` and a row whose first handler is `builtin`, `"m@builtin"`.
  - Two jobs with `group: "G"`, events for key `K` from each (`name` differing, one field each, `level` 1 then 2): `store.list("G").length === 1`; `s.state.level === 2`; `s.by.size === 2`.
  - `marksOf` on it: `name` → `differs` with both names in the title; `own_only` → `1`, title `only <own name>`; `alt_only` → `2`; `level` → plain; the key → plain.
  - Before the second source's event: every mark `null`.
  - `store.sources("G")` slots `[1, 2]`; after `setHandlerSlots({alt: 4})`, the alt source's slot is `4` and the change callback fired.
  - `store.heardBy(job1.id) === 1`.
  - `sync` with the first job gone and the sibling running: the subject is still listed with both `by` entries; with both gone: dropped.
  - A job whose record gains `group` after its first event: its subject is listed under the group, once.
- [ ] **Step 2: Run to see it fail.** `ZETA_UI_ONLY=subject_store_keeps_each_sources_fields cargo test -p zeta --test ui -- --nocapture` (check it did not print `ui e2e skipped`).
- [ ] **Step 3: Implement** the store changes and the two exports; update the header doc's export list.
- [ ] **Step 4: Add the `fake_pair` fixture** to `ui.rs` and assert in a short scenario `scenario_fake_pair_has_two_ready_handlers` that `modes_list` gives it handlers `fake_lines` and `alt`, both `ready`.
- [ ] **Step 5: Run** both scenarios plus the neighbours: `ZETA_UI_ONLY=subject_store_keeps_each_sources_fields,fake_pair_has_two_ready_handlers,subjects_store_merge_units_and_fill,subjects_merge_across_rows,handler_named_on_job_row` — PASS.
- [ ] **Step 6: Commit.** `Send to: the subject store keeps each source's fields`

### Task 7: Marks drawn: the table, the legend, the map popup

**Files:**
- Modify: `crates/zeta-ui/web/subjects.js` (`subjectValues`, `drawSection`, `section`), `crates/zeta-ui/web/map.js` (`subjectPopup`), `crates/zeta-ui/web/app.js` (`applyTheme` → `setHandlerSlots`), `crates/zeta-ui/web/style.css` (`.subject-legend`, `.chip`)
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Consumes: `marksOf`, `store.sources`, `store.setHandlerSlots` (Task 6); the mark classes and tokens (Task 5).
- Produces:
  - `subjectValues(s, slots = null)` entries gain `mark` and `markTitle`; without `slots` both are `null`.
  - A table cell `td[data-col]` carries class `mark-N` or `mark-differs` and, when marked or titled, `title = "<text> (<markTitle>)"`; otherwise its title stays the text. Classes are removed when the mark goes.
  - `p.subject-legend` directly after a section's heading, present only when the section's sources number two or more: one `span.chip.mark-N[data-source=<key>]` per source with the handler's name as text and `title = "<mode> by <name>"`, then `span.chip.mark-differs` reading `differs`. In the cross-row merged section the chip's text is `<protocol> <name>`.
  - `subjectPopup` wraps each value in `span.mark-N` / `span.mark-differs` with the same title.
  - `applyTheme` calls `state.subjects.setHandlerSlots(theme.handlers ?? {})`.

- [ ] **Step 1: Write the failing scenarios.**
  - `scenario_sibling_fields_are_marked_in_the_table`: on source `tone`, start `fake_pair` (own), then a sibling by HTTP (`job_start` with `beside`, `handler: "alt"`). Open Subjects. Expect one row for `PAIR1`; `td[data-col="own_only"]` has class `mark-1` and a title containing `only`; `td[data-col="alt_only"]` has `mark-2`; `td[data-col="name"]` has `mark-differs` and a title containing both `Alpha` and `Alfa`; `td[data-col="level"]` has no mark class. `.subject-legend .chip` count 3, the first two reading the handlers' names. The computed `color` of the `mark-1` cell differs from an unmarked cell's.
  - `scenario_marks_stay_when_a_member_stops`: stop the first job; the row stays, its marks stay, the legend keeps both chips; stop the sibling; the row goes.
  - `scenario_a_lone_decoder_draws_no_marks`: `fake_pair` alone: no `[class*="mark-"]` in the section and no `.subject-legend`.
  - `scenario_map_popup_marks_its_values`: only if `fake_pair`'s lines can carry a position cheaply (add `lat`/`lon` to both lines at the demo station); the popup holds a `span.mark-1` and a `span.mark-differs`.
  - `scenario_theme_pins_a_handler_to_a_slot`: an operator theme (the rig `operator_theme` uses) with `"handlers": {"alt": 4}`: the `alt_only` cell has `mark-4`.
  - Extend `scenario_theme_hooks`' documented-selector list with `.mark-1`, `.mark-differs`, `.subject-legend`.
- [ ] **Step 2: Run to see them fail** with `ZETA_UI_ONLY=<the five names>`.
- [ ] **Step 3: Implement** `subjectValues`, the cell classes and titles, the legend, the popup, `applyTheme`.
- [ ] **Step 4: Run** the five plus `subjects_merge_across_rows,subjects_table_merges_three_aircraft,map_draws_one_marker_per_subject,theme_hooks,operator_theme` — PASS.
- [ ] **Step 5: Commit.** `Send to: fields only one handler gave are drawn in its mark`

### Task 8: The menus: Send to, Capture packets via, Decode's handlers

**Files:**
- Modify: `crates/zeta-ui/web/actions.js` (`GROUPS.job`, the `send_to` entry, the `packets` entry, `decoder`), `crates/zeta-ui/web/menu.js` (`entryNodes` for `sub` elements with `disabled`/`title`, `startJob`, new `sendTo`, `capturePackets`, `jobCtx`)
- Test: `crates/zeta-ui/tests/e2e/ui.py` (`MENU_ACTION_IDS` gains `send_to`)

**Interfaces:**
- Consumes: `jobHandler`, `jobOutputs`, `rowOf` (actions.js); the record's `group` and `handler`; the mode's `handlers` and `handler` from `modes_list`.
- Produces:
  - `GROUPS.job`: `{ id: "sendto", label: "Send to" }` between `next` and `stop`.
  - A `sub` element may carry `disabled: true` and `title`; `menu.js` draws it `aria-disabled="true"` with that title and does not run it.
  - Exported from actions.js: `groupHandlers(job, jobs)` → the handler ids run by the job's group and by any other running, unowned job of the same kind on the same source and tuning (call 28; a record with no `handler` counts as the row's first handler), and `packetVia(job, row, jobs)` → `{ member }` (a running group member whose handler gives packets), `{ handler }` (the handler to start, per call 10), or `null`.
  - `host.sendTo(job, handlerId, needs = [])` → sends `job_start` with the job's `kind`, `source`, `tuning.center_hz`, `tuning.bandwidth_hz`, `deviation_hz: null`, `sideband: null`, `elastic: false`, `replace: []`, `handler`, `needs`, `beside: job.id`; resolves to the new job's id; an error is shown the way a failed `startJob` is.
  - `host.startJob(id, sel, handler = null)` sends `handler` only when not `null`.
  - `jobCtx` gains `jobs` (the last `job_list`).

Entries:
- `send_to` (`group: "sendto"`, `on: ["job"]`): `applies` when running, not attached, not owned, and the row has more than one handler. `sub`: every handler not in `groupHandlers`, in the row's order: ready → `{ key: h.id, label: h.name, run: (k) => k.host.sendTo(k.job, h.id) }`; else `{ key, label: h.name, disabled: true, title: h.reason }`. With nothing to list the entry is not offered.
- `packets`: label `Stop packet capture` when a capture runs on this job; `Capture packets` when the job's own handler gives packets; `Capture packets via <name>` when `packetVia` gives a handler or a member (the member's handler's name). Not offered otherwise. `run`: own → as today; `{member}` → attach to that member; `{handler}` → `sendTo(job, handler, ["packet"])` then attach to the returned job.
- `decoder(id, …)`: when the row has more than one `ready` handler it has `sub`: `{ key: "policy", label: "<name of row.handler> (policy)", run: start(id) }`, then one per ready handler `{ key: h.id, label: h.name, run: (c) => c.host.startJob(id, c.sel, h.id) }`. With one ready handler it is the plain entry it is today.

- [ ] **Step 1: Write the failing scenarios.**
  - `scenario_send_to_starts_a_sibling_from_the_job_menu`: `aprs_decode` (builtin) on `aprs`; open the row's menu; the first level has `group:sendto` between `group:next` and `stop`; its panel lists `Alt lines (UI tests)` and not `built-in`; clicking it sends a `job_start` whose body has `beside` = the job and `handler` = `alt` (`page.expect_request(is_op("job_start"))`); two `aprs_decode` jobs then run. Reopen the first job's menu: no `sendto` group (nothing left to send to). Open the sibling's menu: also none.
  - `scenario_send_to_shows_an_unavailable_handler_greyed`: on a mode whose second handler is not ready (`adsb_decode`: `onair` is unavailable on the test host): the element is `aria-disabled="true"`, its title holds the handler's `reason`, and a click sends nothing.
  - `scenario_capture_packets_via_another_handler`: start `aprs_decode` with `handler: "alt"` (no packets); its menu's `packets` item reads `Capture packets via built-in`; clicking it produces a `job_start` with `beside`, `handler: "builtin"`, `needs: ["packet"]`, then a `job_attach` of `packets` whose `job` is the new job. Reopen the alt job's menu: `Capture packets via built-in` still (the member case) and clicking attaches nothing new while the capture runs on the sibling (the item reads as the member's capture state: assert no second `job_start`).
  - `scenario_decode_menu_lists_a_modes_handlers`: drag a box that fits `aprs_decode` on `aprs`; the APRS decode entry opens a panel with `built-in (policy)`, `built-in`, `Alt lines (UI tests)`; choosing the third sends `handler: "alt"`; choosing the first sends no `handler` key.
  - Extend `scenario_menu_entry_table_is_whole` through `MENU_ACTION_IDS`, and fix `job_menu_offers_next_steps_from_outline_and_row`'s expected first level only if `fm_demod` is affected (it has one handler: it should not be).
- [ ] **Step 2: Run to see them fail** with `ZETA_UI_ONLY=<the four names>`.
- [ ] **Step 3: Implement** `sub` elements' `disabled`/`title` in `menu.js`, then the entries and host methods.
- [ ] **Step 4: Run** the four plus `menu_entry_table_is_whole,job_menu_offers_next_steps_from_outline_and_row,packet_capture,handler_named_on_job_row,decode_menu_lists_new_kinds,box_menu_offers_only_what_fits,submenus_stay_inside_the_viewport` — PASS.
- [ ] **Step 5: Commit.** `Send to: the job menu's Send to, capture via another handler, Decode's handlers`

### Task 9: The group in the job list, and the handler on ribbons

**Files:**
- Modify: `crates/zeta-ui/web/agent.js` (`groupOf` or a new `siblingsOf`, `fold`, `jobRow`, the header doc), `crates/zeta-ui/web/overlays.js` (the per-job event times, `jobRow`'s cells, `subjectRibbon` and the plain ribbon's text), `crates/zeta-ui/web/style.css` (`.job-group[data-kind="siblings"]`, `.job .rate`, `.job .heard`), `docs/themes.md` (the new hooks)
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Consumes: the record's `group`; `store.heardBy` (Task 6); `jobHandler`.
- Produces:
  - A fold `details.job-group[data-kind="siblings"][data-plan=<group id>]` for every group with two or more running members, built like the watch's: the oldest running member's row in the `<summary>`, the others under it in `created_ns` order. A group of one running member is an ordinary row. A sibling group inside a set's or a watch's group nests one level in, as a watch nests in a set.
  - A job's row shows `.tag.handler` whenever its record has a `group` or its mode has more than one ready handler (today: only the second).
  - A member's row has `span.rate` reading `<n>/min` (title `events in the last 60 s`; empty when the page has no feed for the job's source) and `span.heard` reading `<n> subjects` using the row's `subject.name` in place of "subjects" when the row has one (`3 pairs`); empty when the mode has no subject.
  - `overlays.js`: `rateOf(jobId)` → the count of events received for the job in the last 60 000 ms (a ring of event times per job, pruned on read and on a 5 s tick so a decoder that falls silent counts down).
  - A ribbon of a job with a `group`: the plain ribbon's text ends ` · <handler name>`; the keyed subject ribbon's `title` ends ` · <handler name>`.

- [ ] **Step 1: Write the failing scenarios.**
  - `scenario_siblings_fold_as_one_group_in_the_job_list`: `fake_pair` own + `alt` beside it on `tone`: one `details.job-group[data-kind="siblings"]`; its summary holds the first job's row; the sibling's row is inside the fold and not in the summary; both rows have `.tag.handler` with their names; within 15 s each `.rate` matches `^\d+/min$` with n ≥ 1 and each `.heard` reads `1 pairs`. Stop the first: the fold is gone, the sibling is an ordinary row still showing its handler tag.
  - `scenario_a_silent_sibling_shows_zero`: a sibling whose handler prints nothing (give `alt` a `fake_subjects` entry with an empty lines file, or reuse a mode where `alt` prints once and wait out the 60 s window with `page.clock` if the suite uses it; otherwise assert `0/min` on an empty-lines handler): its `.rate` reads `0/min` and `.heard` reads `0 <name>` while the first shows ≥ 1.
  - `scenario_a_sibling_ribbon_names_its_handler`: with `aprs_decode` builtin + alt on `aprs`, a ribbon's title or text for the alt job contains `Alt lines (UI tests)`; a lone `aprs_decode` job's ribbon contains no handler name.
  - Extend `scenario_theme_hooks` with `.job-group[data-kind="siblings"]`, `.job .rate`, `.job .heard`.
- [ ] **Step 2: Run to see them fail** with `ZETA_UI_ONLY=<the three names>`.
- [ ] **Step 3: Implement** the fold, the row cells, `rateOf` and the ribbon text.
- [ ] **Step 4: Run** the three plus `handler_named_on_job_row,subject_jobs_show_with_no_subjects_yet,theme_hooks` and the scenarios that cover watch and set folds (find them by `job-group` in ui.py) — PASS.
- [ ] **Step 5: Commit.** `Send to: siblings fold as one group, each row with its rate and subjects`

### Task 10: The acceptance checks, the recipe, the open work

**Files:**
- Modify: `UAT.md` (a "Plan 43" section), `RECIPES.md` (a "Compare two decoders" recipe), `TODO.md` (the alternative-handlers section: plan 43 built, what it left)
- Test: `bash scripts/check-doc-links.sh`, `bash scripts/public-check.sh`

`UAT.md` checks, against the field host's radio on 1090 MHz:
- **43.1** From the running `adsb_decode` job's menu, Send to › OnAir: a second job starts, the first job's id, band and event count carry on, the radio is not retuned (the page's spectrum does not jump), and both records carry `group`.
- **43.2** In Subjects, an aircraft both hear is one row and one marker. A field only OnAir gave (`df`, `tc`, `level_dbfs` when shown) is in OnAir's mark with a title naming it; the legend names both. Switch theme: the marks follow.
- **43.3** Stop the first job: the rows and marks stay; the sibling's row leaves the fold.
- **43.4** On a job started with a handler that gives no packets, Capture packets via <name> starts that sibling and the capture downloads and opens in Wireshark.
- **43.5** Retune one member from its menu: both move; their ids are kept.
- **43.6** Restart the node: both members resume in the group.
- **43.7** The comparison plan 42 left open: both members running from the same moment for ten minutes; record each row's `N/min` and `N subjects`, and how many aircraft only one heard.

- [ ] **Step 1: Write the three documents.** Roles, not host names; no observed traffic.
- [ ] **Step 2: Run** `bash scripts/check-doc-links.sh` and `bash scripts/public-check.sh` — both clean.
- [ ] **Step 3: Commit.** `Send to: the acceptance checks, a recipe, and what is left`

---

## After the last task

Once, on the head: `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`; `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped` line); both `agents-md --check` forms; `public-check`; `check-doc-links`. Then the whole-branch review. Landing, deploying and UAT 43 wait for the operator's word.
