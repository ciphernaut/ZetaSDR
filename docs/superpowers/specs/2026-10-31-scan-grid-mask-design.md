# Plan 15: a scan whose rows can be trusted

Design spec, 2026-10-31. Brainstormed with the operator (short form: the TODO gist of 2026-10-23
carried the problem and the idea; the operator confirmed the why and left the mechanics to me); a
reviewer read it as the builder before hand-over. Builds on plan 9 (wideband: the `scan` job,
`scan_steps`, the page's coarse layer) and plan 14 (`source_list.stats`, no leaks under repeated
retunes). Written in the terms of `crates/zeta-core/src/ops.rs` (`scan_steps`, the scan's
expansion), `crates/zeta-core/src/node/launch.rs` (`scan_loop`, `fft_step`), the `fft` stage in
`crates/zeta-dsp`, the `detect` stage, and `crates/zeta-ui/web/waterfall.js` (coarse stitching).

## 1. Intent and why

A `scan` is the survey tool: it steps the radio across a band wider than one capture and emits
`spectrum` rows per step. Today those rows lie in two ways, measured on the HackRF over 440-488
MHz at 45 dB gain on 2026-10-23:

- Every step is centred on a round number (448, 464, 480 MHz) and the receiver's DC spike and its
  neighbourhood (smooth humps 150-300 kHz wide) go into the rows as if they were signal, at the
  same frequency on every sweep, so the artefact looks like a steady carrier.
- The weak outer band edges of each step (the anti-alias roll-off) read as quiet, so a signal at
  a step edge looks fainter than it is, or vanishes.

The waterfall's coarse layer and anything built on scan rows inherit both. The operator cannot tell
an occupied frequency from an artefact of the stepping, which defeats the point of a survey.

Success is a scan whose rows show only what is on the air, with the bins the receiver cannot see
honestly marked as no data rather than drawn as quiet or as a carrier.

## 2. Goals, non-goals, anti-goals

### Goals

- No frequency in the span sits at a step centre on every sweep: alternate sweeps shift the step
  grid by half a step, so every frequency gets a clean look at least every second sweep, at the
  same cost per sweep.
- The bins a step cannot see honestly (a notch around its centre, the outer edges) are masked in
  the node, once, and carried in the row as no data; every consumer (the page's coarse layer,
  the `detect` stage, an agent reading rows) reads the mask rather than re-deriving it.
- The alternation and the mask are reported in the scan's `job_context`, and are adjustable with
  `graph put` without a code change, so a device that behaves differently is a settings change.
- The ordinary `spectrum` job and the page's fine layer are unchanged by default.

### Non-goals

- Overlapping steps. Overlap alone does not fix the centre problem (a step's centre is covered by
  a neighbour only at a pitch of half a step, which doubles the sweep); alternation does, at no
  extra cost.
- Measuring the notch width from the hardware at runtime. The widths are params with defaults;
  the live acceptance measures the HackRF's and sets the default from it.
- Keeping a demodulator's or decoder's own capture centre off its channel (the same DC problem
  outside scans). It needs the lease manager to know the capture's minimum width before it tunes,
  which the capability record does not carry, and its offset depends on the notch width this plan
  measures. It is its own small plan after this one; `TODO.md` carries the gist (section 8).
- Changing the sweep order, the dwell, or the 16 MHz step cap.

### Anti-goals

- A mask that hides signal: the notch and edge widths err narrow by default, and a masked bin is
  marked, never zeroed, so a consumer that ignores the mask sees today's row.
- A row shape that breaks an older client: a row without a masked bin is byte-for-byte today's
  row; the new field is present only when it carries something.
- A slower sweep. An odd sweep has at most one more step than an even one.

## 3. The alternating grid

`scan_steps(span, caps)` today lays `n = ceil(span / w)` steps of width `w` end to end from the
span's low edge, each centre clamped into the tune range. It becomes a function of the sweep's
parity:

- **Even sweeps** (0, 2, 4, ...) use today's grid: centres at `start + w/2 + i*w` for `i` in
  `0..n`.
- **Odd sweeps** shift every centre by `w/2`: centres at `start + i*w` for `i` in `0..=n`, that is
  `n + 1` steps, so the span's two edges, which fall on step edges in the even grid and are
  edge-masked there, are mid-step in the odd grid. The first and last odd steps reach `w/2`
  outside the span; those rows are valid spectrum and are emitted as they are (the page's
  stitcher already paints whatever band a row covers).
- Both grids clamp each centre into `tune_min + w/2 ..= tune_max - w/2` as today. When the clamp
  makes two consecutive odd centres equal (a span at the tune floor or ceiling), the duplicate is
  dropped, so an odd sweep is never longer than `n + 1`.
- A span narrower than one step (`n == 1`, the step is the span) does not alternate: there is
  nothing to shift into. `scan.alternate` then reads `null`.

`scan_loop` holds the sweep count and takes the step list for the current parity; the step index
in the row's channel `scan/<step>` is the index within its sweep's list, and a new field in the
row's provenance is not needed: the row's `center_hz` says where it is, as today.

`job_context.scan` gains `alternate: {shift_hz, steps_shifted}` (`shift_hz` = `w/2`,
`steps_shifted` the odd sweep's step count) beside today's `steps` (the even sweep's count),
`step_hz` and `dwell_ms`. Report step counts from it, never from guesswork, as the guide says.

## 4. The mask

The mask is applied where the row is made, in the `fft` stage, by two new optional params:

- `notch_hz` (integer, default 0): bins whose centre lies within `notch_hz / 2` of the row's
  `center_hz` are masked. 0 masks nothing.
- `edge_mask` (number, default 0.0, range 0..0.5): the outer `floor(size * edge_mask)` bins at
  each end of the row are masked. 0 masks nothing.

The defaults keep every existing `fft` node, the `spectrum` job's and the watch's included,
exactly as it is.

A row with at least one masked bin carries `masked: [[lo, hi], ...]`: bin index ranges, `lo`
inclusive, `hi` exclusive, sorted, non-overlapping, each inside `0..size`. The `q` values of the
masked bins are still computed and present (so an older consumer sees today's row and the field is
purely additive); a consumer that honours the mask treats those bins as no data. A row with no
masked bin has no `masked` field. The `floor_db` / 2nd-percentile computation excludes masked bins
(the DC hump would otherwise lift the floor of a narrow step).

The mask defaults are a property of the receiver, not of the scan, so they are decided where the
source is known (my call, after the builder check). A scan on a local hardware receiver (a capability
record with `remote: false` and `push: false`: a SoapySDR device) gets `notch_hz: SCAN_NOTCH_HZ`
and `edge_mask: SCAN_EDGE_MASK`, new constants in `ops.rs`: 300 000 Hz and 0.10 to begin with,
from the survey figures, to be corrected by the live acceptance (section 9). Every other source
(a recording, a KiwiSDR, another node, a VRT sender) has no DC hump of its own and gets 0 and 0.0:
the e2e fixtures' 48 kHz scans paint as today. The notch is absolute hertz because the hump is a
hardware width, not a fraction of the step; a step narrower than four notches gets no notch (the
scan would mask most of its own row), and `job_context.scan.mask` reports the values in force.

`JobKind::Scan` expands to an empty graph today and `launch_scan` / `fft_step` build the `fft`
params inline, so a `graph put` on a scan is refused or ignored. It becomes a one-node graph: one
`fft` node whose `notch_hz` and `edge_mask` are the scan's mask, written by `launch_scan` (where
`port.capabilities()` is at hand) on the first launch and kept across resumes. The scan honours
only those two params from the stored node: `size`, `window` and `rows_per_s` stay the scan's own,
derived per step as today (`spectrum_size(w)`, `hann`, `SPECTRUM_ROWS_PER_S`), and the stored node
carries them as information only (the guide says so). So `graph get`, edit `notch_hz` or
`edge_mask`, `graph put` changes the mask on any source, a recording included, which is how the
e2e scenario gets a partially masked row.

A `job_start` or `graph put` with `edge_mask` outside 0..0.5 or a negative `notch_hz` is refused
naming the field, before anything starts or restarts, as the `detect` params are.

## 5. Consumers

- **The page's coarse layer** (`waterfall.js`, the coarse column buffer that scan rows are
  stitched into, and scan mode where the waterfall follows the scan): a masked bin is not painted,
  so the column keeps what the previous row left there, and a column that has never been painted
  there stays background. This is the fiddly part of the plan: the stitcher paints a row's band
  as one run today and must split it around the mask ranges. A redraw (resize, view change, zoom)
  replays stored rows into a blank coarse column; with alternation a complete picture needs both
  parities, so the replay keeps and replays the rows of the last two sweeps, counted by distinct
  step centres rather than by rows (a step yields several rows), so a redraw never leaves the
  notches as background until the next sweep (my call). The fine layer is untouched (no mask
  reaches it by default).
- **The `detect` stage** (the watch): bins inside a row's `masked` ranges take no part in a run
  and in the floor. Today's `dc_mask_hz` stays as it is (it masks the watch's own capture centre,
  which carries no `masked` field); the two compose.
- **The `measure` stage** (added after the builder check of plan 18, whose "fit to signal" relies
  on it): on a local hardware receiver (the same gate as section 4) a `measure` whose band contains
  the capture centre ignores the bins within `MEASURE_DC_MASK_HZ` (25 000 Hz, `detect`'s default
  `dc_mask_hz`) of it when it picks the peak, the occupied band and the floor, so a box that holds
  the DC spike measures the signal, not the spike. On every other source nothing changes (the e2e
  `tone` fixture sits at its capture's centre). The `measure` event is unchanged in shape; the
  job's `job_context.measure` gains `dc_mask_hz` (0 when none).
- **Anything else that reads `q`** (the e2e harness's row assertions, `examples/python/
  cb_watch.py`) is checked for `masked` awareness by the builder; those that read fine rows need
  nothing.

## 6. Documentation

- `AGENTS.md` (`crates/zeta/src/agents_md.rs`): the `scan` paragraph of `job_start` says the grid
  alternates, that rows may carry `masked`, and that `scan.alternate` and `scan.mask` are in the
  job context; the `fft` stage kind lists the two params; "Reading measurements" in the field
  craft says a masked bin is no data, not low power.
- The plan-series table marks plan 15 and its status section records the live measurements.
- `TODO.md`: the scan gist closes; a new gist "Lease: keep a lone job's capture centre off its
  channel" carries the non-goal of section 2 with the notch width this plan measured as its input.

## 7. Tests

- `scan_steps` unit tests in `ops.rs`: the even grid is today's (existing tests pass unchanged);
  the odd grid has `n + 1` steps shifted by `w/2`; a span at the tune floor drops the clamped
  duplicate; a one-step span does not alternate.
- `fft` stage tests in `zeta-dsp`: a row with `notch_hz` 0 and `edge_mask` 0 has no `masked`
  field and equals today's row; a notch of `k` bins produces one range of `k` (or `k + 1` for an
  even `size` with the DC bin in the middle, as the builder settles) centred on the DC bin; an
  edge mask of 0.1 on a 1024-bin row produces `[0, 102]` and `[922, 1024]`; the floor excludes the
  masked bins (a synthetic DC spike no longer lifts `floor_db`).
- A `scan_loop` test on a fake source: two sweeps' rows have the two grids' centres; the job
  context carries `alternate` and `mask` (0 and 0.0 on a recording); a `graph put` changing
  `notch_hz` changes the rows; the existing `size == 4096` assertion in `tests/scan.rs` holds.
- A capability test: a fake local device (`remote: false`, `push: false`) scans with the default
  notch and edge mask in force; a remote or push source with none.
- A `detect` test: a run entirely inside a masked range is not a signal.
- A `measure` test: on a fake local device a synthetic DC spike louder than a tone 20 kHz away is
  ignored and the tone is the measured centre; on a recording the spike is measured as today.
- UI e2e (`crates/zeta-ui/tests/e2e/ui.py`, against the `scanner` fixture): after a `graph put`
  of a notch on the page's scan, a coarse row with a masked range leaves the pixels under it as
  they were, and a redraw keeps them (the harness already reads the coarse column;
  `wf.coarsePainted` and the ring are there for it). The existing
  `scenario_coarse_layer_paints_outside_the_capture` passes unchanged. Mind the suite's budget: it
  runs near its 300 s deadline, so one scenario, not three.
- A `schema` test: the `fft` stage's `params_schema` lists the two params with their defaults.

## 8. Left to the builder

- Whether the DC bin is one bin or two for an even `size` (the notch range is symmetric about the
  row's centre frequency; pick the convention and test it).
- How `scan_loop` carries the parity (a counter on the `Scan` struct is the obvious place) and
  whether the odd step list is computed once at launch or per sweep.
- The exact split of the coarse stitcher; the only requirement is that a masked bin paints
  nothing.
- How the `measure` stage learns the capture centre and the gate (the expansion has the source id;
  `launch` has the port's capabilities and current tuning, as for the scan's mask).

## 9. Live acceptance

On the desktop HackRF (`--source-soapy hackrf=driver=hackrf,gain=45`), the 440-488 MHz survey of
2026-10-23 repeated with a 2 s dwell look at each step:

- The coarse layer shows no hump at any step centre and no 100 kHz grid of peaks that moves with
  the sweep parity; a frequency that was under a centre in the even sweep is drawn from the odd
  sweep's row.
- The notch width is measured: with `notch_hz: 0` for one run, the hump's width at the floor on
  the HackRF at 2, 8 and 16 Msps is read from the rows, and `SCAN_NOTCH_HZ` is set to the widest
  with 20 % margin; the same for the edge roll-off and `SCAN_EDGE_MASK`. The figures go into the
  plan-series status and the new TODO gist.
- `zeta job context` on the scan shows `scan.alternate` and `scan.mask`; `zeta stream tail` shows
  rows with `masked`.
- A watch on a band inside one scan step is not started by this plan; the `detect` change is
  covered by its unit test only.
