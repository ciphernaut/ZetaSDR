# Send to and the merged, marked subject: what plan 42's build settles

Design note for plan 43. The design itself is the operator's, approved 2026-11-21:
`2026-11-21-alternative-handlers-design.md`, sections "Send to: a sibling beside a running
job" and "One merged subject, marked" (delivery item 2). This note does not restate it. It
records what a survey of the code as plan 42 left it (main `990f6c6a`) found that the
design did not say, and the call made for each. Where this note and the parent spec differ,
this note is the later word; every such place is marked **differs**.

## Intent, unchanged

From a running decoder, start another handler for the same mode on the same band, disturb
nothing, and show what both hear as one subject whose fields say which handler gave them.
The first use is the comparison plan 42's acceptance run could not make: dump1090 and OnAir
on the same aircraft over the same seconds.

## What the code is today

- `job_start` already takes `handler` and `needs`, and a second job of a mode on the same
  band with another handler is already not a duplicate (plan 42, decision 4). So two
  handlers can run side by side now; nothing ties them.
- `JobRecord` has no `group`. A record is always written with every field (the record rule,
  `job.rs` `require_format`).
- `LeaseManager::retune_locked` sets aside one holder's leases. A retune of one sibling to a
  band outside the capture is therefore a `conflict` naming the other sibling.
- Restart, recovery and retune settle each record's handler alone (`rehandle`), so two
  records may be re-chosen onto the same handler.
- The page's subject store keeps one flat `state` per subject: the last value of each field,
  whoever sent it. Groups come only from a watch's kept decoders. The cross-job merged view
  is built in the Subjects pane from the members' states.
- The page counts events per job only for the selected source's feed, with no rate.
- A submenu element (`sub`) cannot be shown disabled with a reason; only whole entries can.
- Themes: tokens are an allow-list (`themes/tokens.rs`) pinned to `style.css`'s `:root` and
  to `docs/themes.md` by tests; `theme.json`'s top-level keys are a fixed list.
- There is no event-feed pane. Events are drawn as ribbons on the waterfall.
- The UI suite's one two-handler mode (`aprs_decode`: `builtin` and `alt`) never has both
  handlers hear the same subject.

## Calls made

### The node

1. **`beside` names the band; the request still carries it.** `job_start`'s `kind`,
   `source`, `center_hz` and `bandwidth_hz` stay required fields of the op (they are not
   optional types, and every client of the schema reads them as required). With `beside`
   they must equal the named job's, and `deviation_hz`, `sideband`, `elastic` must be
   `null`, `null`, `false`; a difference is refused naming the field and both values.
   **Differs** from "or omit them": the CLI gives the omission instead (`zeta job start
   --beside <id> --handler <h>` reads the job and fills the rest), and the page copies the
   job's record.
2. **`beside` is refused** for a job that is not running, is attached, is owned by a watch
   or an identify, is elastic, or whose kind is no mode row; `replace` must be empty with
   it. Each refusal names the reason.
3. **The record field is `group`**, as the spec says: `null` on a job never sent from, else
   the id of the job the first Send to named. `plan_start` and `jobs_stop` have a request
   field also called `group` (a channel plan's group). They are different operations and the
   record has no such field, so the name stands; the field's doc says which is meant.
4. **A sibling's handler is always named** (the spec requires `handler` with `beside`), so
   its record has `handler_named: true` and a relaunch that finds it gone fails that member
   and never re-chooses. Only the job first sent from can have a chosen handler. When it is
   re-chosen, handlers that running members of its group run are passed over ("runs beside
   it as job <id>"), so a group never holds one handler twice.
5. **A group retune is one lease move.** A new lease call sets aside every member's lease
   on the source, grants the new band once as the rigid request `retune_locked` makes, and
   leases it to each member; on a conflict every lease is put back. Members relaunch in
   turn under the one gate, as `move_capture` relaunches holders; a member whose relaunch
   fails is `failed` and the rest stand. `job_retune`'s result stays the job that was
   named. `replace` may not name a member.
6. **`job_stop` is unchanged**: it stops the one job. Stopping a group is the operator
   stopping each member.
7. **History keeps one terminal record per handler.** The history prune treats a finished
   record as a re-run of an earlier one by kind, source and band; the key gains the handler,
   or a stopped OnAir job would drop the stopped dump1090 job's record beside it.
8. **Rollback.** `group` is written always (the record rule), so a node rolled back past
   this build resumes no jobs, as past plan 42's. The CHANGELOG says so.

### The page

9. **Send to is its own first-level group in a job's menu**, between Next and Stop, shown
   when the job's mode lists more than one handler. Each element is a handler the group
   does not run: a `ready` one starts the sibling, an `unavailable` one is drawn disabled
   with its reason as the title. `sub` elements gain `disabled` and `title` for this.
10. **Capture packets via <name>.** When the job's own handler gives no packets, the entry
    names the handler it will use: the mode's `handler` (the policy's pick) when that one
    gives packets and the group does not run it, else the first such handler in the mode's
    list. It sends `beside`, that `handler` and `needs: ["packet"]`, then attaches the
    capture to the new job. If a member of the group already gives packets, the capture is
    attached to that member and nothing starts.
11. **Decode ›.** A decoder entry whose mode has more than one `ready` handler becomes a
    submenu: `<policy's pick's name> (policy)` first, sending `handler: null`, then every
    ready handler by name, sending it named.
12. **The group in the job list** reuses the watch's fold (`details.job-group`,
    `data-kind="siblings"`): the oldest running member's row is the summary and the others
    sit under it. Every member's row shows its handler's name always (not only when two are
    ready), `N/min` and `N subjects`.
    - `N/min` is the events the page received for that job in the last 60 s. The page
      learns of events from the feed of the source it shows, so a member on another source
      shows no rate, as it shows no count today.
    - `N subjects` is the subjects of the group that this member's handler has supplied at
      least one field for.
13. **The store keeps what each source said.** A subject gains `by`: per source key, the
    latest value of each field and when. The source key is `<mode>@<handler>`; an event
    with no `handler` (an older node) counts as the row's own. `state` stays as it is (the
    latest value of each field), so every existing reader is untouched. Envelope fields
    (`protocol`, `handler`, `summary`, `t_ns`, `position`, `access`) are never marked.
14. **Sibling groups in the store.** A job's group is its watch's when it has one, else its
    record's `group`, else itself. A sibling group lives while any member runs.
15. **What is marked.** Within one subject, for each drawn field:
    - every source that has supplied the subject anything has supplied this field: plain;
    - one source alone has supplied it: that source's mark;
    - some but not all (three or more sources): plain, the title naming who gave it;
    - a string field two sources hold different current values for: the `differs` mark,
      newest shown, each source's value in the title. Numbers and booleans are never
      compared.
    A subject only one source has heard is drawn plain throughout: a mark says "only this
    one gave it" and needs a second source to mean anything.
16. **Slots and the legend.** A group's sources take slots 1 to 4 in the order they first
    supplied a field; a fifth and later share slot 4. `theme.json` gains `handlers`, a map
    of handler id to slot (1 to 4), which pins that handler's sources. The legend is a line
    under the section's heading in the Subjects pane, one chip per source in its mark with
    the handler's name, plus a `differs` chip; it is drawn only for a section with two or
    more sources.
17. **Theme tokens.** `--c-handler-1..4` (colours), `--handler-style-1..4` (values of
    `font-style`: `normal` or `italic`), `--c-differs`. Base defaults alternate the style
    (`normal`, `italic`, `normal`, `italic`), so slots 1 and 2 differ without colour; the
    name is always in the title and the legend. Every built-in theme sets the colours.
18. **"The event feed tags each line"** is read as the ribbons, there being no feed pane: a
    ribbon of a job in a sibling group ends with its handler's name, and a keyed ribbon's
    title names it.
19. **The map popup** draws each value in its mark with the same titles.

### Tests

20. The UI suite gains one fixture mode, `fake_pair`, with a subject, whose own program and
    whose `alt` handler entry print the same key with one shared text field (different
    values), one field each that the other lacks, and a changing number. Every mark has a
    scenario on it.

### Settled after a builder's read

21. **The band compared** (call 1) is the job record's `tuning` (its lease band), not the
    capture.
22. **`group` is written on the first job only after the sibling has launched**, under the
    same gate. A sibling start that fails leaves the first job's record untouched: no group
    of one is ever made by a failure.
23. **The group id is a bare key.** The first job may stop and its record be pruned while
    members still carry its id; nothing on the node or the page resolves the key to a
    record.
24. **"Holds a handler"** (call 4) reads records, not runs: a record of the same group,
    another id, that is `Running` or will be resumed in the same pass. So a restart, a
    source recovery and a retune give the same answer.
25. **The order of a group retune** (call 5): the restart locks of exactly the members that
    are running when the gate is taken (not of every holder of the source), then the gate;
    then each member's handler is settled, oldest first, with call 4's rule, before any
    lease is touched, and a member that cannot be settled refuses the whole retune with
    nothing moved; then the one lease move and at most one tune; then the relaunches. The
    page's elastic jobs relaunch once, after, as for one job. A member that has stopped by
    the time the gate is taken is left out. A member failed by its relaunch is not
    recovered on its own, as one job failed by a retune is not today.
26. **Attached jobs stay with their member.** Every member's attached jobs (a capture, a
    transcriber) are held and relaunched on that member, as for one job.
27. **History** (call 7): a record with no `handler` counts as the row's own handler, the
    way the duplicate rule reads it.
28. **A job outside the group already running the handler on that band** makes the start a
    `duplicate` naming it (the existing rule, checked after the group's). The page's Send
    to and Capture-via lists leave out handlers run by any running job of that mode on that
    source and band, in the group or not.
29. **A sibling group's subject in the cross-row merged view** is one member of the merged
    row, like any other subject; the merged row's `by` is the union of its members'.

## Not in this plan

- `capture_decode` and Explore's Send to (plan 44).
- OnAir's other modes and rows without a decoder (plan 45).
- A handler ranking by measured results; stopping or replacing a group in one operation.
- Merging on the node.

## Anti-goals, restated for the builder

- Send to never stops, retunes or starves the job it was sent from: a sibling start makes
  no tune, and its failure leaves the first job running.
- A mark that only colour carries.
- A subject field, an event or a capture whose handler cannot be told.
- A relaunch that leaves two members of a group on one handler.
