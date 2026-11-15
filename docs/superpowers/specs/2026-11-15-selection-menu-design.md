# The selection menu: decoders from the mode rows, six groups (plan 40)

2026-11-15. Small enough that this page is the spec and the plan. Not built yet.

## Intent

Two things the operator met on the field host with a second radio attached:

- **DVB-T has no entry.** The only way to start `dvbt_decode` from the page is "Identify
  (thorough)". The node accepts a direct `job_start dvbt_decode` (it checks the row's `min_hz`
  and nothing else); the page never offers it.
- **The menu is cumbersome.** A box on the waterfall opens one flat list of 27 entries, 25 of
  them visible on a typical box.

Both have one cause. The menu's decoder entries are a table written into
`crates/zeta-ui/web/menu.js` (`ENTRIES`, `JOBS`, and a branch per decoder in `jobTuning` with
its own width constant): APRS, POCSAG, RDS, ADS-B, ACARS and "Decode video". A decoder mode is
data everywhere else (a row in `modes/*.json`), and the rows already reach the page through
`modes_list`, but only to fill "Decode across this band…", which starts a `watch`. So a new row
gets no entry until someone edits the page, and every entry the page does have is always
shown, whatever the box.

## Goals and non-goals

- Every mode row has a direct Decode entry, with no page change per row. DVB-T is the first to
  gain one; operator rows from `--modes-dir` gain theirs the same way.
- The menu opens as six lines. What does not fit the box is one click away, never gone.
- The Decode list leads with what is plausible for this box: its frequency and its width.
- Transcription is one setting, not a second copy of each audio entry.
- Every entry keeps running on the shown radio (the tab's own source), as now.
- No node change: no operation, no field, no row schema. `modes_list` already carries all
  that is used (`id`, `name`, `state`, `reason`, `band`).
- Not in this plan: the job row, tag and outline menus offering next steps (a TODO.md gist);
  ranking modes by `knowledge_lookup` and a type-to-filter box (TODO.md, the flat menu of
  100+ modes, narrowed by this plan to those two); the agent starting a decoder on a
  frequency of its own invention, and the conflict line's retry applying it (both seen
  the same day, both their own fixes).

## Approaches

Three shapes were put to the operator:

1. **Grouped** (chosen): a few top-level lines, each opening a short group.
2. Flat but filtered: one list of what fits the box and a "more…" line. No groups, but the
   list's length and order change with every box, so nothing is where it was.
3. Search first: a filter box over every mode and action. The best answer at a hundred
   modes; it needs typing, which a touch screen makes awkward, and there are fifteen rows.

For the Decode list: plausible rows first (chosen), every ready row always (grows with each
row), or the width alone as "Decode across" filters today (offers ADS-B on any wide box
anywhere). For transcription: one toggle (chosen), the five entries kept in a group, or
attach only from the running job's own menu.

## Design

### The menu

```
What is this?
Identify (thorough)
Decode        ›  <the rows plausible for this box>
                 other modes…          (the rest; a row that is not ready is greyed)
Listen        ›  NBFM audio · FM broadcast audio · AM · SSB (USB) · SSB (LSB)
                 [ ] transcribe
Watch / scan  ›  Watch this band · Decode across this band… · Capture bursts · Scan this span
More          ›  measure · Start plan… · Start set… · Save selection as SigMF · Send to agent
```

- A group is a header `button.ctx-item.ctx-group[data-group]` followed by its
  `.ctx-sub[data-for]`, the indented form "Decode across" already uses. One group is open at
  a time. **A click opens a group; the pointer passing over a header does not**: closing the
  group above moves every line below it under the pointer, and a hover that opened groups
  would then open the wrong one.
- Decode is open when the menu appears if it has a plausible row, so the common case (a box
  round a signal, start its decoder) is still one click.
- An entry keeps its `data-action`. A row's entry has `data-action="<row id>"`, so
  `adsb_decode`, `acars_decode`, `pocsag_decode`, `rds_decode` and `aprs_decode` keep theirs;
  "Decode video" becomes `video_decode`, the row's id.
- "Decode across this band…" keeps its own nested list and its rule (ready rows no wider than
  the box), inside Watch / scan.
- Up and Down move over the visible entries as now. Enter or Right on a header opens its
  group and focuses its first entry; Left closes it and returns to the header.
- The menu is placed again whenever its height changes (a group opened, the rows arrived),
  so it stays inside the viewport.
- The selectors the themes style stay: `[role="menu"]`, `.ctx-item`, `.ctx-sub`.

### Decode entries from the rows

`modes_list` is read once per page, as now, but without the `state` filter. Per box:

- **The width a row's entry asks for** (`rowTuning(row, box)`): the box's centre; the box's
  width when it lies between the row's floor (`band.min_hz`, else `band.bandwidth_hz`) and its
  nominal `band.bandwidth_hz`, else the nominal width. This gives what the written branches
  give today for a box round a signal (ADS-B 2.4 MHz, RDS 200 kHz, POCSAG 12.5 kHz, ACARS
  10 kHz), DVB-T the box as drawn from 5 to 8 MHz and 8 MHz for a narrower one, and analog
  video the box as drawn from 6 to 24 MHz. The constants `ADSB_BANDWIDTH_HZ`,
  `POCSAG_MIN_BANDWIDTH_HZ` and `ACARS_MIN_BANDWIDTH_HZ` go: the row carries the number.
- **Plausible here** (`plausible(row, box)`): a row with `range_hz` when the box overlaps it;
  else a row with `centers_hz` when one of them lies in the box widened by half the row's
  nominal width; else (a row that names no frequency: RDS, M17, analog video) when the box is
  no wider than twice its nominal width. `centers_hz` are the channels a row knows, not the
  only ones there are, so a row that fails this is listed under "other modes…", never
  dropped.
- A ready, plausible row is listed in Decode by its `name`. Every other row is under "other
  modes…", which opens in place.
- **Greyed** (`aria-disabled`, the reason as the entry's title, a click refused): a row whose
  `state` is not `ready`, with the row's `reason` (the missing program, the broken field);
  and a row whose width the shown radio cannot carry, in the lease's words (`insufficient
  bandwidth: <id> delivers at most <cap> Hz`), which is "Decode video"'s rule today
  (`videoRefusal`) applied to every row.
- While the pointer or the focus is on a row's entry the footer shows the tuning it will
  send, as it does for the SSB entries' dial, so the operator sees 2 400 000 Hz before
  starting ADS-B on a narrow box.
- A row's entry sends `job_start` with `kind` the row's id, that tuning, `deviation_hz` and
  `sideband` null, never fitted first (as the decoder entries are today).

### Transcribe

A view setting `transcribe` (`off`, `on`; default `off`), a checkbox line in Listen and `view
set transcribe on` at the command line. When on, an audio entry attaches a transcriber to the
demodulator it started, as the "Transcribe › …" entries did; those five entries and their
table go. It is kept with the tab's other view settings. It is not one of the keys an agent
may set: it starts a job.

### What changes

| File | Change |
|---|---|
| `crates/zeta-ui/web/menu.js` | the entry tables become two top entries and four groups; `rowTuning`, `plausible` (exported); the group helper; the Decode list filled per box; `videoRefusal` generalised; the transcribe tick; the header comment, which lists entries the menu no longer has and omits "Capture bursts" |
| `crates/zeta-ui/web/view.js` | the `transcribe` key beside `scanner` |
| `crates/zeta-ui/web/app.js` | hands the menu the setting and its setter |
| `crates/zeta-ui/web/style.css` | `.ctx-group`, the checkbox line; a tall menu scrolls rather than leaving the viewport |
| `docs/guide.md` | the menu table (it lists six entries) and the view-settings table |
| `docs/agents/field-craft.md` | the view-settings sentence, if it names the keys; then `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerated |
| `TODO.md` | the flat-menu gist narrowed; "the job on the page" struck from the DVB-T items |

## Proof

In `crates/zeta-ui/tests/e2e/ui.py`:

- A helper opens a group by its header; the scenarios that click an entry now inside one
  (`scan`, `watch`, `capture`, `iq_read`, `ask`, `measure`, `set`, the audio entries) use it.
  `classify` and `identify` stay top-level.
- `decode_menu_lists_new_kinds` becomes: the Decode group lists the plausible rows for the
  box by name, the rest under "other modes…".
- `transcript_ribbon_appears`: tick transcribe, click the audio entry, a transcriber is
  attached; with the tick off, none is.
- `decode_video_greys_when_the_box_is_too_wide` follows the entry to `video_decode`.
- New: two test rows beside the suite's `fake_subjects`, one whose `range_hz` holds the test
  tone and one whose range is elsewhere. The first is listed in Decode and starts with
  `rowTuning`'s width; the second is only under "other modes…".
- New: a row that is not ready is greyed and its title is the row's reason.
- New: `rowTuning` and `plausible` called directly on the built-in ADS-B, RDS, ACARS and
  DVB-T rows' bands, for the numbers above.

The gates of `CLAUDE.md`, the UI suite in a private target directory. On a loaded host the
wide-view scenarios fail now and then on `main` too: compare against `main` before blaming
the change.

On the field host, by the operator on the page: a box about 7 MHz wide round a television
multiplex opens six lines with Decode open, listing DVB-T and analog video and neither ADS-B
nor APRS; DVB-T starts on the box as drawn and locks. A box at the ADS-B frequency lists
ADS-B, and its footer reads 2 400 000 Hz before it is started. Listen, transcribe ticked,
NBFM on a voice channel starts the demodulator with a transcriber.

## Calls made here that the operator did not make

- A click, not a hover, opens a group (the reason is above). "Decode across this band…" is
  the one list that still opens under the pointer, as it does today, since nothing closes
  when it opens.
- A row that names no frequency is plausible only on a box up to twice its nominal width. The
  factor is a guess at "a box drawn round one such signal"; it decides only which of two
  lists a row is in.
- An entry wider than its row's nominal width is cut to it. Today a wide box makes RDS and
  ACARS ask for the whole box; a decoder gains nothing from more than its channel.
- Rows that are not ready are shown, greyed, rather than hidden: the title says what to
  install, which is how the operator learns a decoder exists.
- `transcribe` is a view setting and not a tick that resets with each menu, so a session of
  voice channels ticks it once.
