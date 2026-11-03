# Plan 24: the console theme (a frame around the spectrum, the rail, two-line jobs, riding ribbons)

Design spec, 2026-11-03. The operator brainstormed this with three rounds of mockups
(`.superpowers/brainstorm/*/content/layout*.html`, kept locally, not committed); answers marked
"operator" are theirs, the rest are the author's calls, listed in section 10. It answers the
`TODO.md` entries "UI: console restyle" (the 2026-10-27 gist, renamed by this plan), "UI: staircase transcript ribbons on the
waterfall" and "UI: the job panel hides the id behind a horizontal scroll"; "UI: the page watches at
most 4 jobs, so a tag loses its squelch dot" is untouched (see non-goals). UI only: no node change, no
new op.

## 1. Intent and why

The page grew up as a developer's console: a monospace dark theme from a mockup, a job row that is one
long line scrolling sideways, job tags stacked on the waterfall's outlines, transcript ribbons pushed
down lane by lane until nothing reads. The operator wants the spectrum to be the thing the eye lands
on, inside a frame that is "familiar yet futuristic": an homage to the bridge displays of the
1990s science-fiction television series the operator grew up with, in the idiom its production designer
gave them (rounded elbow
frames, bar segments with end caps, a warm palette on black, condensed uppercase labels, right-aligned
numerals), without anything that is the franchise's by name or mark, because this software will be
open source.

The restyle is also the moment to fix three things the operator has asked for separately: the
identifiers leave the waterfall for a rail beside the frequency axis; the job row gets two lines and
stops scrolling sideways; decoded text rides its channel's line on the waterfall, drifting left with
the RF, tilted so a burst of lines reads as a staircase rather than a pile.

## 2. Goals, non-goals, anti-goals

### Goals

- **The spectrum first** (operator). The waterfall takes the full height between the top and bottom
  bars and the full width between the rail and the side column; collapsing the side column's panes
  gives it the whole width.
- **A theme beside the current one** (operator: keep the old look). `view set theme console|plain`,
  persisted like the other view policies; the console is the default (author).
- **The frame** (operator): an elbow at top-left and bottom-left, bar segments with end caps across the
  top (brand, the selected source carrying its live numbers, the other sources, map, listen, drawer) and
  bottom (ops marker, the command line, mic, status).
- **The rail** (operator): channel labels only, left of the frequency axis, each at the height of its
  band, colour-matched to its outline; the waterfall's outlines carry no tag any more.
- **Riding ribbons** (operator): a decoded chunk sits centred on its band's line, drifts left with the
  columns and is tilted anticlockwise; a collision steps the older chunk up a lane (author, section 4.5).
- **Two-line jobs** (operator): identifier and state on line one, centre, live numbers and the id's
  tail on line two; no decoded text in the row; no horizontal scroll.
- **Collapsible side panes** (operator): the agent chat log above, the jobs below, each folding on its
  header bar.
- **The graph modal skinned** (operator): nodes as rounded bars coloured by stage family, the params'
  headline on the node; no builder.
- **Every number live** (operator: "as per current data"). The frame shows node facts the page already
  polls; nothing decorative.
- **Direction-ready** (operator): logical CSS properties and `dir="auto"` on every text chunk from day
  one, so a right-to-left locale mirrors the chrome later with a switch, not a rewrite.
- The UI e2e suite stays green on the console theme. Its selectors keep working with four known
  edits (section 3: three tag lookups that go through the outline, and the `view show` line count);
  one scenario proves the theme switch.

### Non-goals

- A ComfyUI-style graph builder (operator: "no need to add that now").
- Mirroring the time axis or the chrome for right-to-left locales now (operator: "at a later stage");
  `view set dir` and `view set time_flow` go to `TODO.md` as gists with the mechanics in section 4.9.
- Node-side anything: no op, no persisted theme on the node, no change to the event streams.
- A phone-first layout. The narrow footprint keeps working (section 4.10) but is not redesigned.
- Raising `MAX_WATCHED`, re-ordering the panes strip, the Subjects or Explore panes' content.
- Sound effects, animations beyond the ribbon step and the pane fold.

### Anti-goals

- Anything that is the franchise's: the name of its computer interface, its fleet, its designer's
  eponym for the panels, or any ship, series or character name, in code, docs, settings, the page or
  the theme id (the operator keeps the word list outside the repository; a redaction pass before
  publishing checks for it); the delta insignia; stardates;
  verbatim screen layouts copied from frames; the fan wikis' exact palette codes; the show's
  typeface or a clone sold as one. The theme id is `console`.
- Decorative numbers. If a readout cannot be fed from a value the page has, it is not drawn.
- A second DOM for the second theme. One DOM, two skins: the layout changes (rail, two-line rows,
  collapsible panes, centred ribbons) apply to both themes; the theme changes colours, radii, font,
  the elbow chrome and the ribbon tilt.
- Breaking a selector the e2e suite uses (section 3).

## 3. Constraints

- Assets are embedded by hand: `crates/zeta-ui/src/lib.rs` `ASSETS` is a table of
  `(path, mime, include_bytes!)`; a new file needs an entry, a MIME type and a line in the test
  `every_web_file_is_embedded_with_its_mime`. No build.rs.
- View policies live in `view.js` `POLICIES` (`{key, values|parse, default_, describe, then}`),
  persisted in sessionStorage under `zeta.view` as the values that differ from defaults;
  `view.onChange(cb(key, value))` is how modules follow a change.
- The frequency axis is DOM (`div.wf-axis`, left of the canvas, `span.tick` with MHz text and
  `data-edge`, `span.tick.ch` for plan channels); `yForHz`/`hzForY` map it. The e2e reads tick text
  and the axis's bounding box.
- The e2e suite (`crates/zeta-ui/tests/e2e/ui.py`, ~218 scenarios, run by `crates/zeta/tests/ui.rs`
  under a 600 s deadline, measured 352-440 s) selects by id, class, data attribute and text, never by
  colour or computed style. Selectors that must keep working, with what they expect:
  - `#jobs .job[data-id][data-state]`, `.job.attached`, and inside a row `.kind`, `.band` (text
    `x.xxxx MHz, 10.0 kHz`), `.tag.squelch`, the `hot` class.
  - `#waterfall .job-tag[data-job]` with children `.kind` (the kind), `.channel` (the plan channel,
    `ch20`), `.short`, `.dot`, `.dot.closed`, `.snippet` (the last transcript line, asserted at
    `ui.py:2774`); a tag is hovered (puts `hot` on tag, outline and row) and its bounding box is
    clicked; `.outline[data-key="pin"] .job-tag` must count 0; outline bounding-box y positions are
    compared. Three lookups go through the outline (`outline.locator(".job-tag …")` at `ui.py:1734`,
    `:2565`, `:2112`): these are re-pointed to `#waterfall .job-tag[data-job=<id>]`, the only
    scenario edits this plan makes besides the `view show` count (`ui.py:1128` asserts 12 policies;
    `theme` and `side` make 14).
  - `.ov-ribbon` with its protocol class (`.transcript`, `.candidate`, `.pocsag`, `.aprs`, `.agent`,
    `.kr .count`, the `fake_*` ones), found by text.
  - `#transcript .line` with `.op`, `.note`, `.note.error`, `.error`, `.assistant`, `.user`,
    `details.turn`; `#side` visible or hidden by footprint and drawer.
  - `#graph`, `#graph svg .graph-node[data-kind]`, `#graph-params`, `#graph-apply`.
  - `#waterfall .wf-axis`, `.wf-axis .tick`, `.tick.ch`, `canvas.wf-canvas` (bounding box for Hz per
    pixel), `#wf-badge`.
- Ribbons: `overlays.js` `ribbon(hz, text, cls)` creates `div.ov-ribbon <cls>` anchored by
  `style.right`, `place(r)` sets `top` from `yFor(hz)` and today pushes an overlapping ribbon *down*
  past the one it hits (`RIBBON_GAP_PX`, `MAX_LANES` 8); `scroll(shift)` moves them left with
  `wf.onColumn`; `expire()` drops them after `RIBBON_TTL_MS`.
- Tags: `tag(o, key)` creates `div.job-tag[data-job]` inside the outline; `layoutTags()` stacks
  overlapping tags down by `TAG_GAP_PX`; `setHot`/`setSpeaking` toggle classes on outline, tag and row.
- Job rows: `agent.js` `jobRow()` builds `div.job` with spans `.kind`, `.source`, `.state`, `.id`,
  `.band`, `.mix-now`, `.events`, tags `.tag.elastic|ch|auto|squelch`, `.pos`, `.reason`, and
  `.actions` (`.play`, `.transcribe`, `.mute`, `.graph`, `.stop`).
- The transcript pane (`#transcript`, class `Transcript`) is the agent chat log mixed with the ops feed
  and UI notes. It is not the decoded-text log.
- `style.css` has 10 `:root` variables, about 10 hex literals and about 15 `rgba()` literals outside
  `:root`; `waterfall.js` hard-codes the canvas background as `BG = [0,0,48]` (line 189) and
  `#000030` at three more sites (937, 955, 1480), the selection's white (1585-1586), and the same
  value as the colormap's first stop (223), which is the colormap's and stays literal; `graph.js` has
  no colours (its CSS does, through variables).
- `#waterfall` is `inset: 0` of `#main`; `#pane` covers its bottom 40 % and `#map` its right half when
  shown, so the canvas can be shorter than `#main`. `div.wf-axis` is a child of `#waterfall`.
- The Jobs pane's `h2` is static text today; no count is rendered anywhere.
- The base font is a monospace stack. No font is bundled today; `vendor/` holds Leaflet with its
  LICENSE.
- The job-tag long press (`LONG_PRESS_MS`) opens the outline menu in the narrow footprint.

## 4. The design

### 4.1 The theme layer

`view set theme console|plain` (default `console`). `view.js` adds the policy; `app.js` applies it as
`<html data-theme="console|plain">` on load and on change. `style.css` is reorganised so that every
colour, radius, font and the ribbon tilt is a `:root` variable, the `plain` block restating today's
values and the `[data-theme="console"]` block the new ones. The 12 hex and 15 rgba literals outside
`:root` become variables (an `rgba()` that is a translucent form of a variable colour becomes a
`color-mix()` or a second variable). Nothing selects by theme except `[data-theme]` blocks in CSS and
the canvas colour reads below.

Variables added (names are the builder's; these are the roles):

- Ground, panel, border, text, dim text, accent, ok, error, user: the ten today.
- Palette roles for the console: `--c-frame` (the elbow and bars), `--c-source`, `--c-action`,
  `--c-channel-1..6` (the colours the rail, outlines and ribbons cycle through per job), `--c-idle`,
  `--c-speaking`.
- `--radius-bar` (14px console, 3px plain), `--radius-elbow` (22px, 0), `--rail-w` (96px, 0 when the
  rail is hidden), `--axis-w`, `--side-w`.
- `--font-ui` (the console: the bundled condensed face; plain: today's monospace stack), with
  `font-variant-numeric: tabular-nums` on every numeric cell in both.
- `--ribbon-tilt` (-15deg console, 0deg plain).

Canvas colour reads: `waterfall.js` reads its background and selection colours from
`getComputedStyle(document.documentElement)` once at mount and again on a theme change (a `view.onChange`
on `theme`), replacing the four `[0,0,48]` literals and the selection's white with variables
`--wf-bg` and `--wf-select`. The spectrum colormap is computed and stays as it is in both themes.

Palette values are the author's own picks, documented in the CSS with a one-line comment per role;
they are warm (orange, peach, lilac, blue, pale yellow, teal) on black and deliberately not any
published fan-wiki table. The builder may tune them for contrast (WCAG AA against black for text roles,
against the bar colour for the black text on bars).

### 4.2 The font

One condensed sans with tabular numerals, bundled: **Antonio** (SIL Open Font License 1.1, Google Fonts;
weights 400 and 700 as `woff2`, about 40 kB together) under `crates/zeta-ui/web/vendor/antonio/` with its
`OFL.txt`. `ASSETS` gets the entries with MIME `font/woff2` and `text/plain`; the embed test lists them.
`@font-face` in `style.css`; no external fetch ever. The plain theme does not use it. If the builder
cannot fetch the files offline, the console theme falls back to the local condensed stack
(`"DejaVu Sans Condensed", "Roboto Condensed", "Arial Narrow", sans-serif`) and the fetch is a listed
follow-up, not a blocker.

Uppercase labels are CSS `text-transform: uppercase` on label roles only (bar captions, rail labels, pane
headers, the job row's first line). Decoded text, transcripts, the chat log and parameter values keep
their case: a transcript is evidence.

### 4.3 The frame

The `#app` grid keeps its three rows and two columns; the rail is not a grid column but a child of
`#waterfall` beside the axis (section 4.4), so it shares the canvas's height and y-frame and the
`#waterfall …` selectors keep finding the tags:

```
"top   top"     var(--bar-h)
"main  side"    1fr            main = [ rail | axis | canvas ] (+ pane, map as today)
"bar   bar"     var(--bar-h)
```

The frame's left spine (the elbow's vertical) is `#main`'s inline-start padding in the console theme,
painted in `--c-frame`, so the rail reads as sitting on the frame.

- **Top bar** (`#top`): the brand cell is the elbow (`.brand`, `border-start-start-radius:
  var(--radius-elbow)`), then the source tabs (`#tabs`): the selected source's tab is a wide bar carrying
  its live numbers from the `source_list` the page already polls (`pin` centre or `current` centre,
  `measured_rate_hz`, `overflows`, `gain.db` or `auto`, `holders.length`, `bank` or `direct`, `fault`
  when set, each as a `<span class="k">label</span><span class="v">value</span>` pair); the other sources
  are short bars; then the panes strip, listen, map and the drawer button as end-capped bars. In the plain
  theme the same DOM renders as today's tabs and buttons (the numbers show in the tab's `title`, not
  inline, to keep the plain look unchanged).
- **Rail** (`#rail`, new, section 4.4).
- **Main** (`#main`): unchanged content (waterfall, map, pane).
- **Side** (`#side`, section 4.6).
- **Bottom bar** (`#bar`): the prompt cell is the lower elbow; the command input, mic and status are bars.
  The input keeps its id and placeholder.

All offsets, paddings, radii and borders that have a side use logical properties (`inline-start`,
`inline-end`, `border-start-start-radius` and friends), so `dir="rtl"` on `<html>` mirrors the frame.
The builder does not set `dir`; section 4.9 says what happens later.

### 4.4 The rail

A new element `div#rail` inside `#waterfall`, before `div.wf-axis` (so the order is rail, axis,
canvas), `--rail-w` wide (96px console; 0 and `hidden` in plain, where tags stay on the outlines as
today, so the plain theme is unchanged in behaviour). It is the canvas's height, and `yFor` places
into it directly. `waterfall.js` creates it with the axis (`mountWaterfall`) and `overlays.js` owns its
content as it owns the tags: `tag(o, key)` creates the same `div.job-tag[data-job]` with the same
children and the same text contract (`.kind` the kind, `.channel` the plan channel such as `ch20`,
`.short` the id's tail, the `.dot`, the `.snippet`) plus one new span `.centre` (the centre in MHz to
three decimals, tabular) and appends it to `#rail` instead of the outline when the rail is shown
(`ov.setRail(el)`; `null` keeps today's placement). The tag's `data-job` is the outline's key, so
`setHot`/`setSpeaking`, the hover and click handlers and the long press keep working: hovering a rail
tag lights its outline and row, clicking scrolls to the row, a long press opens the menu. In the
console theme `.short` and `.snippet` are hidden by CSS (the rail has no room; the ribbon shows the
text); they stay in the DOM with their content for the e2e.

Layout (`layoutTags()` when the rail is on): each tag's `top` is `yFor(outline centre) - h/2`,
clamped to the rail. Tags are sorted by `top`; a tag that would overlap the previous one is collapsed
to a thin segment (`class="thin"`, `RAIL_THIN_PX` = 5 high, text hidden, colour kept) unless it is
`hot` or `speaking`, in which case it stays full and the neighbours it overlaps collapse instead. A run
of thin segments longer than `RAIL_GROUP_MIN` (6) is still drawn one segment each (they are the
frequency map of the cluster), with the run's count in the first segment's `title`. The rail re-lays
on `wf.onView` as the tags do today. The pin and a capture keep their `.outline-label` on the
waterfall (the e2e expects no `.job-tag` under the pin).

Colour: each job's outline, tag and ribbons take one of `--c-channel-1..6` by a stable hash of the job
id (`channelColour(jobId)` in `overlays.js`, exported for `agent.js` so the row's left edge matches).
`speaking` overrides with `--c-speaking`; a finished or idle (squelch closed) job's tag takes
`--c-idle` for its ground but keeps the channel colour as a 3px start border, so a thin segment is
still recognisable.

The rail's visible content in the console theme is `.channel` when the job sits on a plan channel,
else `.kind`, then `.centre`; CSS does the choosing (`.job-tag:has(.channel) .kind { display: none }`).

Ribbon colour follows the job, not the protocol: a ribbon's ground is its job's channel colour (so
rail label, outline and ribbons match, as the mockups showed), with the protocol class still on the
element for the e2e and for the two that must stand apart: `.transcript-error` and `[dropped]`
ribbons keep `--error`, and `.agent` ribbons (the agent's own notes) keep `--user`. The per-protocol
colour rules in `style.css` (282-288) become the plain theme's and are overridden under
`[data-theme="console"]`.

### 4.5 Riding ribbons

Three changes in `overlays.js`:

1. **Centred on the band.** `place(r)` sets `top` so the ribbon's vertical centre is `yFor(hz)`: that
   is today's formula; the change is that the base lane is the band line and a collision no longer
   pushes the new ribbon down (below).
2. **Tilt.** The ribbon's `transform` is `translateY(-50%) rotate(var(--ribbon-tilt))` with
   `transform-origin: center`, so the chunk pivots about its own centre on the band line and the
   tilt is the theme's. The rotation is visual only: the ribbon's `right` and `top` are the untilted
   box, so `scroll`, `expire` and the lane maths stay in axis-aligned coordinates and the e2e's text
   lookups are unaffected.
3. **Collision: the older chunk steps up.** When a new ribbon on a band would overlap a ribbon already
   there (at the right edge, as `place` checks today), the *new* one takes the base lane and every
   ribbon it overlaps is moved one lane away from the band in the direction `RIBBON_STEP_DIR` (-1 = up,
   the default; +1 = down restores today's direction), a lane being the ribbon's height plus
   `RIBBON_GAP_PX`, animated by a CSS transition on `top` of `RIBBON_STEP_MS` (160) so the move reads
   as a scroll. A ribbon that would step past `MAX_LANES` or into another band's base lane (any other
   job's `yFor(hz)` ± one lane) is clipped instead: it keeps its lane and loses its tail under the
   newer ribbon (newer ribbons have the higher z-index). As ribbons scroll left and no longer overlap at
   their lane, `layout()` lets them fall back toward the base lane one lane at a time (same transition).
   Keyed rows (subjects, `fold`) hold one ribbon per subject and never step: they are updated in place
   as today.

The constants and the rule live in one commented block at the top of `overlays.js` (operator: "inline
documented enough to tweak it later"):

```js
// Ribbon collisions. A new ribbon takes its band's line; the ribbons it overlaps step one lane
// away in RIBBON_STEP_DIR (-1 up, +1 down) over RIBBON_STEP_MS, at most MAX_LANES lanes and never
// into another band's base lane (they clip instead), and fall back as the line clears. Keyed
// (subject) ribbons never step. To return to the pre-plan-24 behaviour set RIBBON_STEP_DIR = 1
// and RIBBON_STEP_MS = 0.
export const RIBBON_STEP_DIR = -1;
export const RIBBON_STEP_MS = 160;
```

The snippet (last transcript line) on the tag stays in the DOM (the e2e asserts it at `ui.py:2774`)
and is hidden by CSS in the console theme, where the ribbon shows the text; the `snippets` map in
`overlays.js` is unchanged.

Every ribbon and every `#transcript .line` body gets `dir="auto"` (section 4.9).

### 4.6 The side column: chat log over jobs, both collapsible

`#side` keeps its two `section.pane` children (`.transcript`, `.jobs`). Each `h2` becomes a header bar
(`button.pane-head[aria-expanded]` inside the `h2`, text "Transcript" and "Jobs"; the Jobs header gains
a new `span.count` with the running-job count, set where `startJobs` renders the rows) that toggles
`data-collapsed` on its section. A collapsed section shows only its header bar; the
other grows to fill the column; with both collapsed `#side` shrinks to the two header bars stacked at the
top (`--side-w` becomes `--side-collapsed-w`, 44px) and the waterfall takes the rest. The state is a view
policy (`view set side transcript|jobs|both|none`, default `both`) so it persists with the others and the
agent transcript can report it. The narrow footprint's drawer behaviour (`[data-drawer="open"]`) is
unchanged; the fold buttons are hidden there.

The transcript pane is the agent chat log and the ops feed (what it is today); nothing decoded goes
there. Its lines get `dir="auto"`.

### 4.7 Two-line job rows

`jobRow()` keeps every span and class; `style.css` lays the row as a two-line grid in both themes:

```
line 1:  .kind  .tag.ch  .tag.squelch  .state  ........  .actions
line 2:  .band  .source  .events  .mix-now  .pos  .tag.elastic|auto  .reason  ....  .id
```

`.band` stays `x.xxxx MHz, 10.0 kHz` (the e2e reads it). `.id` stays the last six characters, right-aligned
on line two with the full id as `title` (today). Nothing in the row scrolls horizontally: line two uses
`overflow: hidden; text-overflow: ellipsis` on its text spans with the full text in `title`. The row's
start edge carries a 3px border in the job's channel colour (`channelColour`), and `speaking` and `hot`
colour the row's ground as the tag's. In the console theme the row is an end-capped bar with black text;
in plain, today's bordered row.

Idle plan channels: when a plan (`.tag.ch` present) has more than `JOBS_FOLD_MIN` (8) running jobs
whose squelch is closed and that have had no event for `JOBS_FOLD_IDLE_MS` (60 000), they are wrapped in
one `details.job-group[data-plan]` whose `summary` reads `78 idle fm_demod on uhf_cb_au` (and the
attached count). The rows stay in the DOM with their ids and classes; the group is `open` by default
on the first render and remembers the operator's fold in sessionStorage. A job that opens its squelch
or gets an event leaves the group within one poll. If a plan-start e2e scenario asserts visibility of
a folded row, the builder keeps the group `open` and notes it; the fold's default is not worth a
scenario change.

### 4.8 The graph modal

CSS first: `.graph-node rect` takes its fill from the node's `data-kind` through a family map in CSS
(`[data-kind="channel"]`, `fft`, `measure`, `detect` → channelising blue; the demods → orange; `denoise`,
`vad`, `mix`, `stream_in` → audio lilac; the decoders and `external_decode`, `transcribe` → yellow; the
sinks → teal; `passthrough` and unknown kinds → idle grey), rounded `rx` from `--radius-bar`, black
bold uppercase title, edges in `--c-frame` with their stream kind in the peach label; the two
`ui-monospace` font rules become `var(--font-ui)`. One JS change: `graph.js` draws a second text line
(`text.graph-node-headline`) under the kind with up to two params as `key value` pairs (the builder
picks a short list per kind: `center_hz`/`bandwidth_hz`, `deviation_hz`/`squelch_db`, `language`), so a
node says what it does without a click. The click-to-edit flow, `#graph-params` and `#graph-apply` are
unchanged. No builder, no drag.

### 4.9 Direction-ready, not direction-switched

From this plan on: every side-bound CSS property in the files this plan touches is logical
(`margin-inline-start`, `padding-inline-end`, `inset-inline-start`, `border-start-start-radius`,
`text-align: start|end`); the rail is `inset-inline-start`; every ribbon, chat line, job row text span
and transcript body has `dir="auto"` so a right-to-left utterance renders right to left inside its chunk
beside a left-to-right one on the next band. The time axis and the tilt are defined relative to time
("newest at the inline-end edge", "rising toward now") and do not change with script direction. Two
gists go to `TODO.md`: `view set dir ltr|rtl|auto` (apply `dir` on `<html>`; the chrome mirrors by the
logical properties above; the axis side and the ribbon anchor follow `inline-end`) and
`view set time_flow left|right` (flip the column direction, the axis edge and the tilt's sign together).

### 4.10 Footprints

The wide footprint is the design above. The narrow footprint (`html[data-footprint="narrow"]`): the rail
is thin segments only (`--rail-w` 14px, no text; a tap expands the tapped tag for 3 s over the waterfall
as a floating label), the top and bottom bars drop their captions to icons as they do today, the side
column stays the drawer. The pane fold buttons are hidden in the drawer. The builder checks the two
narrow-footprint e2e scenarios that touch tags still pass (the tag's bounding box is still clickable
as a segment).

## 5. Testing

Unit (Rust): `every_web_file_is_embedded_with_its_mime` lists the font files and `OFL.txt`.

E2E (`ui.py`), new scenarios, each under 3 s:

- `scenario_theme_switch`: `view set theme plain` sets `data-theme="plain"` on `<html>`, the rail is
  hidden and a job tag is inside its outline; `view set theme console` puts it back in `#rail`; the
  policy persists across a reload within the session.
- `scenario_rail_tags_align`: two fm_demod jobs on the fixture; each `.job-tag` in `#rail` has its
  vertical centre within 2 px of its outline's; hovering one puts `hot` on tag, outline and row.
- `scenario_rail_collapses_crowd`: twelve jobs 12.5 kHz apart at a zoom where labels overlap: all but
  the hovered one have class `thin`, the hovered one does not.
- `scenario_ribbon_rides_band`: a transcript ribbon's untilted box is centred on `yForHz` of its job
  within 2 px; a second ribbon on the same band within the same column places the new one on the band
  and the older one one lane above (its `top` smaller by ribbon height + gap) after `RIBBON_STEP_MS`.
- `scenario_ribbon_clips_at_other_band`: two bands one lane apart; the older ribbon does not step
  into the other band's lane.
- `scenario_job_row_two_lines`: a running fm_demod row's `.band` and `.id` are on a lower line than
  `.kind` (bounding boxes) and the row's `scrollWidth` equals its `clientWidth`.
- `scenario_side_folds`: clicking the Jobs header collapses the jobs section; `view get side` says
  `transcript`; both collapsed widens `canvas.wf-canvas` by at least 200 px.
- `scenario_graph_headline`: the graph modal's channel node has a `text.graph-node-headline` naming
  its `center_hz`.
- `scenario_dir_auto`: a ribbon and a transcript line carry `dir="auto"`.

Existing scenarios run on the console theme (the default) with exactly four edits: the three
outline-scoped tag lookups re-pointed to `#waterfall .job-tag[data-job=…]` (`ui.py:1734`, `:2565`,
`:2112`) and the `view show` policy count (`ui.py:1128`, 12 → 14). Any other existing scenario that
fails is a bug in the build, not a scenario to edit. Budget: the suite is at 352-440 s
of 600; nine short scenarios add under 30 s. If the run passes 520 s the builder trims
`finished_rows_s`-style waits in the new scenarios first, then reports.

Manual: `cargo run -p zeta -- serve --listen 7979 --source-file ...` against a recording, open the page,
check both themes at 1280 and 1920 px wide and the narrow footprint at 420 px.

## 6. Acceptance on the field host (the operator's)

Deployed as `plan24-zeta` beside the running node, then main. The operator says yes or no to:

1. The spectrum is the first thing the eye lands on; the frame reads as homage, not as a copy.
2. The rail: labels sit at their bands; the CB pin's 80 channels collapse to segments and the talking
   channel expands; hovering a label lights the band and the row.
3. Ribbons ride their band, tilted, drift left with the RF, and a fast exchange steps up and settles.
4. Jobs: two lines, no sideways scroll, the id's tail readable, idle CB channels folded.
5. The chat log and the jobs fold on their headers and the waterfall widens.
6. `view set theme plain` gives today's page back.
7. The graph modal reads at a glance.
8. Nothing on the page is a franchise name or mark (the operator checks the page and `git grep -i` for
   the words in the anti-goals).

## 7. Tidying `TODO.md`

Rename the 2026-10-27 restyle gist to "UI: console restyle" in the plan's first commit (its body
loses the franchise words too), then remove it and "UI: the job panel hides the id behind a horizontal scroll"
when shipped (evidence: the scenarios above). Rewrite "UI: staircase transcript ribbons on the
waterfall" to what remains after 4.5 (a lane budget from pixel height per footprint, the staircase
sharing lanes with keyed rows). Add gists: `view set dir`, `view set time_flow` (section 4.9); the theme
in localStorage rather than sessionStorage (so a new tab keeps it; all view policies share the choice);
the graph builder (unchanged entry, re-pointed at the skinned modal); the rail's floating label on a
phone if acceptance finds it wanting.

## 8. Left to the builder

- Exact palette values, bar heights, elbow radii, the font's size steps; the family map in 4.8.
- How `channelColour` hashes (stable per job id, six colours, no two adjacent rail tags the same where
  it can be helped).
- The order of operations in `layoutTags()` for the rail (sort, mark hot/speaking, collapse neighbours).
- Whether the collapsed rail tag on a phone floats its label over the waterfall or the axis.
- The e2e fixture jobs used by the new scenarios (reuse the fm_demod recording scenarios' setup).
- Deploy script changes for `plan24-zeta` (as `plan19-zeta` was deployed).

## 9. Out of scope, recorded

- A node-side theme or any op.
- Audio, the mix, the map's tiles and markers (the map keeps Leaflet's own styling; its container
  takes the frame's border radius only).
- The voice console's behaviour; only its bars are restyled.

## 10. Calls made in this document (check these while reading)

1. The console theme is the default; `plain` is one `view set` away. The e2e runs on the default.
2. The theme id is `console`; the font is Antonio (OFL), bundled, with a local-stack fallback.
3. One DOM for both themes: the rail, two-line rows, collapsible panes and centred ribbons apply to
   both; the tilt is a theme variable (0 in plain) and the rail is hidden in plain (tags stay on the
   outlines there), so the plain theme behaves as today.
4. Collisions step the *older* ribbon up (`RIBBON_STEP_DIR = -1`), clip at `MAX_LANES` or another
   band's lane, fall back as the line clears; keyed ribbons never step. All in one commented block.
5. The tag keeps its `.kind`/`.channel`/`.short`/`.snippet` text contract and gains `.centre`; the
   rail hides `.short` and `.snippet` by CSS, and the ribbon carries the text.
12. The rail is a child of `#waterfall` beside the axis, not a grid column, so it shares the canvas
    height and the `#waterfall .job-tag` selectors; four e2e edits are accepted and listed, no more.
13. Ribbons take the job's channel colour in the console theme; error, dropped and agent ribbons keep
    their own; protocol colours stay in plain.
6. Node health goes in the selected source's top bar (console) and in the tab's `title` (plain).
7. Side folding is a view policy `side` with four values, default `both`.
8. Idle plan channels fold into a `details.job-group` after 8 idle jobs and 60 s quiet, open by default.
9. Rail crowding collapses to 5 px segments; hot or speaking wins the space.
10. Palette values are the author's own, warm on black, tuned for AA contrast, not a published table.
11. Direction: logical properties and `dir="auto"` now; the switches are gists.
