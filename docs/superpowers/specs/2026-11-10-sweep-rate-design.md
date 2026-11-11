# Plan 36: a fast band sweep, on any source

Design spec, 2026-11-10. Decided with the operator in a brainstorming session, from two measured
runs on the field host's B210 the same day (the deployed node's scan, then a throwaway spike with
the node stopped) and a read of the B210 clone's gateware, UHD's AD9361 driver and the HackRF's
sweep firmware. A reviewer read it as the builder before hand-over; its findings are folded in
(the shortest dwell on a narrow source, who chooses the ring's format, the Kiwi's two sockets
and the fallback under a held channel, the capability record across versions). Section 4.6a
(the sample depth on the device's link) was added with the operator the next day. Builds on the scan job (`JobKind::Scan`, `scan_grid` in
`crates/zeta-core/src/ops.rs`; `launch_scan`, `scan_loop`, `dwell_block`, `fft_step` in
`crates/zeta-core/src/node/launch.rs`), the capability record (`Capabilities`, `Claim` in
`crates/zeta-core/src/types.rs`), the source trait (`Port`, `crates/zeta-core/src/port.rs`), the
worker's shared-memory ring (plan 28.1: `crates/zeta-source-worker/src/{ring,wire,serve,supervisor}.rs`),
the Soapy reader (`crates/zeta-source-soapy/src/source.rs`) and the KiwiSDR source
(`crates/zeta-source-kiwi/src/{proto,session,source}.rs`). It takes up three `TODO.md` entries:
"Worker: `sc16` on the ring", "Sources: a source that delivers spectrum rows" and the Kiwi half
of "Spike: wideband views from the Kiwi and from custom B210-clone gateware".

It is built as two plans: **36.1**, the stepped sweep made fast on every IQ source, with `sc16`
on the worker's ring; **36.2**, a source that sweeps for itself, with the KiwiSDR's waterfall as
its first user. 36.2 depends on 36.1's scan record and row fields only.

## 1. Intent and why

The operator, 2026-11-10: "the sweep is now so slow, making fullsweep useless for detecting
anything but continuous signals". What a sweep is for, in their words: "faint signals for general
rf spectrum usage". And the frame for the fix: "we dont want to only fix it for just this
hardware but to make sure that high speed sweep works for other hardware types too".

So the measure is **revisit time**: how often every part of a band is looked at. Today a scan of
the B210's whole range (34 MHz to 6 GHz) takes about 2.2 minutes, and a frequency is in view for
0.2 s of that, so only a signal that is always on is seen. A 0-30 MHz scan of a KiwiSDR takes
over 10 minutes, in 2500 steps of 12 kHz, while the Kiwi's own waterfall shows the whole band 23
times a second.

The operator suspected the alternate grid (plan 15's quarter-step shift between sweeps, there to
see under the DC notch). It is not the cause: the two grids alternate, one per sweep, and a step
cost 315 ms before it and 335 ms now. The time was always in the step.

### What was measured (the field host's B210, gain 76 dB, 2026-11-10)

Through the deployed node, 16 MHz steps at 20.48 Msps, 123 steps:

| Phase of one step | Median | p95 |
|---|---|---|
| `retune_shared` (of which the device's `set_frequency`: 3.1 ms) | 11.2 ms | 163 ms |
| the fixed dwell sleep | 251 ms | 252 ms |
| `dwell_block` (a 40 MB copy out of the ring) | 56 ms | 59 ms |
| `fft_step`, inline before the next tune | 15 ms | 16 ms |
| the step | 334 ms | 485 ms |

Bare driver (SoapySDR, no node), 210-227 hops a case:

- A tune within 100 MHz of the last calibration point takes **3.0 ms** (the same for 16 and
  28 MHz hops, at 20.48 and 30.72 Msps). The first tune 100 MHz or more away runs the AD9361's
  calibration: **154 ms at 20.48 Msps, 104 ms at 30.72 Msps**. With 16 MHz steps that is every
  seventh step, with 28 MHz steps every fourth: 22-26 ms a step on average.
- Around a fast tune: old-band samples stop 0.3 ms into the call; about 2.7 ms of exact zeros
  follow, ending just before the call returns; power is within 0.05 dB of steady 1 ms after the
  return. About 68 000 samples (old band, then zeros) are read after the return but were taken
  before it.
- One device overflow follows most tunes (the reader is blocked in the call for 1.6-2.7 ms).
  Steady streaming at either rate had none.
- Reading the device's native `CS16` or `CF32` costs the same CPU (18 % of a core at
  20.48 Msps, 25.5 % at 30.72 Msps).

Short dwells through the node (the dwell constant lowered, nothing else changed):

| Dwell | Step / rate | Median step | Mean step | MHz/s | First row against later rows |
|---|---|---|---|---|---|
| 250 ms | 16 / 20.48 | 334 ms | 353 ms | 45 | (today) |
| 50 ms | 16 / 20.48 | 65 ms | 87 ms | 183 | -1.0 dB |
| 20 ms | 16 / 20.48 | 35 ms | 56 ms | 284 | -2 dB, floor -3.9 dB |
| 20 ms | 28 / 30.72 | 35 ms | 61 ms | 461 | -1.8 dB |
| 20 ms | 30.72 / 30.72 | 36 ms | 63 ms | 487 | band edges roll off |

The first row of every step reads low: the zeros of the tune reach the ring stamped with the new
centre. The second row agrees with later ones within 0.08 dB.

Raw results and scripts: `scratch/sweep-spike/` (untracked).

### What the gateware and the firmware offer

- The B210 clone's gateware is the stock Ettus B200 design ported to an Artix-7. It has no
  sweep, FFT or fast-tune logic. UHD cannot time an LO tune on this radio and does not implement
  the AD9361's fast-lock profiles. Anything faster than the host-commanded tune above is new
  gateware: a later plan, behind a spike (section 2, non-goals).
- The HackRF's firmware has a sweep mode (`hackrf_sweep`): the device retunes itself after a
  set number of blocks and the host does the FFT. It is a second real "the source sweeps for
  itself", after the Kiwi's waterfall.

## 2. Goals, non-goals, anti-goals

### Goals

- **A scan's revisit time is set by the radio, not by constants in the node.** A step costs the
  tune, the settle and the samples it needs, and nothing else: no fixed sleep, no copy of a
  quarter-second of samples, no FFT in the tune's way.
- On the field host's B210, a scan of the whole tuning range at the default settings completes
  a sweep in **20 s or less** (2.2 minutes today), and 1-2 GHz in 3 s or less.
- **Every IQ source gets it** through one contract (section 4.1) and measured capabilities, not
  a per-radio branch: a HackRF, a remote node's source, a file source and a radio yet to come
  are stepped by the same loop at the pace each one can keep.
- The first row after a tune is as good as the rest: within 0.5 dB of the later rows of the same
  step.
- The operator chooses the look: a scan takes a dwell per step and an average across sweeps, so
  the same job is a fast revisit, a long look, or quick passes averaged for a faint steady
  signal.
- The worker's ring carries `sc16` when the device's native format is 16-bit, converted at the
  node: half the memory traffic for a wide capture.
- A source whose link can carry a narrower sample depth uses the lossless one that sustains the
  highest measured rate (12-bit on a B210, if the probe shows it helps), and a scan can ask for
  a lossy depth (8-bit) as an explicit, never default, coarse and fast look (4.6a).
- **A source that can sweep for itself does** (36.2): the scan asks it for spectrum rows over a
  span and paints them as it paints its own. On the operator's KiwiSDR, 0-30 MHz at 8 kHz a bin
  or finer completes a sweep in **5 s or less** (over 10 minutes today), and the Kiwi's 12 kHz
  IQ channel stays free for a demodulator meanwhile.
- The page, `stream tail`, the agent and MCP see one `scan` job and one row shape whichever way
  the rows were made.

### Non-goals (later plans)

- Custom gateware for the B210 clone (fast-lock hopping, an FFT on the device). Its own spike
  first: the vendor's Vivado project, the lock check in the third-party source, a second stream
  through the USB path.
- Skipping or batching the AD9361's calibration (a patched UHD). After this plan it is the
  largest remaining cost of a B210 step (about 25 of 35 ms); the scan record reports it so the
  case can be made with numbers.
- The HackRF's firmware sweep as a second self-sweeping source. The seam of 36.2 is shaped to
  take it; no HackRF is attached to the node today.
- The Kiwi's unreleased wideband IQ mode.
- A `watch` reading a self-sweeping source's rows (the gist's `source_spectrum` node for
  `spectrum` and `watch`). 36.2 serves `scan`; the seam is the same one.
- A page control for dwell and averaging beyond what section 4.5 names.
- Widening a held capture by dropping its depth (the operator, 2026-11-11: jobs run on a
  20 MHz capture, the page zooms out to twice that; could the extra be had at 8 bits?). Depth
  belongs to the whole stream and a sweep of the extra would tune away from the jobs, so the
  form that works is one wider capture at a depth that sustains it, the jobs fed through the
  channel bank as now. Whether a narrower depth lifts a radio's rate is what this plan's probe
  measures; the live checks record it (section 6) and a follow-up round decides from the
  numbers. A `TODO.md` entry carries it.

### Anti-goals

- A B210 special case in the node: a constant, a branch on the driver name, a step table.
- A sweep that is fast because it paints rows that are wrong: tune garbage in a row, a band
  edge's roll-off painted as signal, a masked range that stops being reported.
- Losing the alternate grid's property that every frequency is seen clean in one of two sweeps.
- A scan that starves the source's other users: the lease rules do not change, and a scan still
  shares the source only as the lease allows today (a stepped scan's lease is exclusive).
- A Kiwi sweep that takes more of a shared Kiwi than one waterfall connection.

## 3. Constraints

- `Capabilities` is `deny_unknown_fields`, travels in the worker's hello, in the cached source
  record replayed on a respawn, and from a remote node through `--source-node`, and a test
  requires every field in the schema. The fields this plan adds (`usable_fraction`, `depths`, `sweep`)
  are optional with defaults, so a new node reads an old or cached record; a new record is not
  promised to be readable by an older node (a `--source-node` pair is upgraded far end first or
  together, and the plan says so in `docs/`).
- The repository is public: roles, not host names, in every tracked file. No observed
  frequencies of real services in fixtures or docs.
- One operation set. A new scan field goes in `ops.rs` and reaches every transport;
  `AGENTS.md` and the skill are regenerated, never edited.
- The worker protocol is versioned (`PROTOCOL` 2, the ring's `VERSION` 1). A node must refuse or
  fall back cleanly with a worker of the other version: a deploy replaces both together, but a
  restart can race.
- The CI runners have 2 CPUs and tests run sources in real time: no test may depend on a wall
  clock margin thinner than the suites already use; poll for the end state.
- The persisted job record keeps resuming: a scan stored before this plan resumes with the new
  defaults.
- Scan rows keep their payload fields (`size`, `bin_hz`, `rows_per_s`, `center_hz`, `floor_db`,
  `step_db`, `q`, `masked`). The page keys on `center_hz`, `bin_hz` and `size` and assumes
  nothing about rows per step, dwell or step width (`crates/zeta-ui/web/waterfall.js`), so
  fields may be added and the pace may change without a page rework.

## 4. The design

### 4.1 The contract: a source delivers only settled samples (36.1)

Today each source stamps whatever it reads next with the new centre, and the scan hides the
tune's garbage inside a long dwell. The knowledge of when a tune has settled belongs to the
source, so the contract moves there:

> After `Port::tune` returns a new centre, the first block a source delivers with that centre
> holds only samples taken after the tune settled.

- **Soapy** (`zeta-source-soapy`): the reader discards what it reads from the start of
  `set_frequency` until the call has returned and a settle time has passed, by the device's
  sample clock where it has one (the samples read after the return but taken before it are the
  68 000 above). The settle time is a per-driver default the source owns (1 ms for the UHD
  B2xx family, by the measurement above; a conservative default for an unmeasured driver) with
  a device argument to override it, as `max_rate_hz=` overrides the rate. The overflow the
  reader's own blocked tune causes is counted as a tune gap, not as an overflow the health
  figures and the rate probe treat as loss.
- **Kiwi** (`zeta-source-kiwi`): frames in flight at the old frequency are today stamped with
  the new centre at once. The session drops `SND` frames until the server's sequence shows the
  tune took effect, or for a measured latency if it gives no such sign (the builder reads the
  protocol; section 7).
- **File, node and VRT sources**: a file source's tune is exact; a `--source-node` source
  inherits the remote node's contract and discards for the link's latency as the Kiwi does; a
  VRT source does not tune. Each states in a doc comment how it meets the contract.

The contract is written on `Port::tune` and tested per source with a recorded or synthetic step
in level between two centres.

### 4.2 The step: tune, wait for samples, hand off (36.1)

`scan_loop` becomes:

1. `retune_shared` (unchanged: the gate, the lease check, `port.tune`).
2. Wait until the node's ring for the source holds `dwell` of samples at the new centre (the
   ring already empties on a centre change, `IqRing::push`), or a timeout of the dwell plus a
   margin passes: then the step is skipped and counted, as a short block is today.
3. Take exactly those samples (a few megabytes at a 20 ms dwell, not the ring's last
   quarter-second).
4. Hand the block to the FFT **off the tune's path** and go straight to the next tune. Rows are
   published in step order: one step's FFT may still be running while the next step's samples
   arrive, never two sweeps' worth queued (a bounded hand-off; when the FFT cannot keep up the
   loop waits, and the wait is what the scan record reports).

There is no sleep. `SCAN_DWELL_MS` becomes the default dwell of valid samples, **20 ms**
(`NodeConfig::scan_dwell_ms` stays as the node's default). One step publishes one row, the
average of the whole dwell: `rows_per_s` in a scan's row is `1000 / dwell_ms`. A dwell of 100 ms
or more keeps publishing 20 rows a second within the step, as today, so a long look still shows
movement inside it.

**A dwell never holds less than four FFT frames.** The FFT size comes from the step's width
(`spectrum_size`, 1024 at the least), so on a narrow source a 20 ms dwell is shorter than one
frame (a 12 kHz Kiwi channel gives 240 samples), and `scan_loop` skips a block shorter than
`size`. The effective dwell is therefore `max(dwell_ms, 4 × size / rate)` (341 ms on the Kiwi's
IQ channel, close to today's 250 ms; 0.8 ms on the B210, so the request rules there). The scan
record reports the effective figure as `dwell_ms` and the asked one as `dwell_ms_asked` when
they differ. The scan averages the dwell's frames into its one row itself or through the `fft`
stage, the builder's choice; the stage's `rows_per_s` limit of 1..=60 is not a limit on the
dwell, and the row's `rows_per_s` field reports `1000 / dwell_ms` clamped to that range (the
page does not read it). (The spec's call, from the builder check.)

The reader's 10 ms read (`max(16384, rate/100)` samples) delays a tune by up to a read; the
builder may shorten the read while a time-shared lease holds the source (section 7).

### 4.3 The step width follows the capability (36.1)

`SCAN_STEP_MAX_HZ` (16 MHz) goes. A step asks for the source's `max_bandwidth_hz` and advances
by its usable part: `step = usable × rate`, with `usable` a property of the source kind's
front-end filter that the capability record carries (`Capabilities` gains a
`usable_fraction`, an `Asserted` 0.9 for Soapy devices by default, giving 27.648 MHz steps at
30.72 Msps where the spike found 28 MHz clean and 30.72 MHz rolled off; 1.0 for a file source;
device argument to override). The rows of a step carry `masked` ranges for the unused edges as
they do for `edge_mask` today, so the page and `detect` never read the roll-off; the builder
folds `SCAN_EDGE_MASK` into the same mechanism.

The alternate grid stays: the odd sweep is shifted by a quarter step, and
`scan_grid_sees_every_frequency_clean_in_one_of_two_sweeps` keeps passing with the new width.
The existing TODO on the HackRF's 4 MHz spur grid coinciding with a 4 MHz shift is not made
worse (a 0.9 × 20 MHz step shifts by 4.5 MHz) and stays open.

A scan started before the source's rate cap is measured sweeps at the asserted width (the spike
saw one such run at 12.288 Msps). The scan re-reads the capability at each sweep's start, so the
next sweep after the probe uses the measured width.

### 4.4 The cost of a step, measured and reported (36.1)

So that the pace is the radio's and visible:

- The scan's context record (`context.scan`, today `steps`, `step_hz`, `dwell_ms`, `alternate`,
  `mask`) gains `rate_hz`, and a `timing` object refreshed each sweep: `sweep_ms` (the last
  sweep), `step_ms` (median), `tune_ms` (median and max), `slow_tunes` (tunes over ten times the
  median: the calibrating ones on an AD9361), `skipped` (steps whose samples did not arrive) and
  `fft_wait_ms` (time the loop waited on the FFT). `hz_per_s` is derived and stated.
- Nothing in the node branches on these. They are for the operator, the agent ("report step
  counts from it" in the agents' guide becomes "report the sweep time from it") and the next
  plan's case for the calibration.

### 4.5 The look is the operator's: dwell and average (36.1)

`job_start` for a `scan` takes two optional fields, on every transport:

- `dwell_ms` (1-5000, default the node's 20): valid samples per step, never less than four FFT
  frames (4.2).
- `average` (1-64, default 1): each published row is the mean power of that step's last
  `average` sweeps (per grid: an even step averages with even sweeps of the same centre). The
  row carries `sweeps`, the number averaged so far. Averaging 16 sweeps of 20 ms lowers the
  row's noise ripple as one 320 ms dwell does, while each sweep still revisits fast.

Both are stored on the job record, shown in `context.scan`, changeable on a running scan by the
same route `notch_hz` and `edge_mask` are today (`graph_put` on the stored node; the builder
may add them there or as a retune field, section 7). The page's "Scan this span" keeps starting
the default; the scan's job row offers (a select beside the row's controls: the rows have no menu today) "fast / averaged / long look" presets (20 ms × 1,
20 ms × 16, 250 ms × 1). The presets are a small table in the page, the only page change in
36.1 besides showing `sweep_ms` on the job row.

This is the spec's call, marked as such: the operator asked for faint signals and for bursts,
and was asked whether many quick looks averaged is the right trade against one long look; the
question was overtaken by the gateware dig and not answered. Offering both, default fast, is the
answer that needs no ruling.

### 4.6 `sc16` on the worker's ring (36.1)

- The worker's hello already names the ring's `format` (`"cf32"`, set in `serve.rs`, stored and
  not checked by the node). It becomes a real field: `"cf32"` or `"sc16"`.
- **The worker chooses.** The node creates the ring (`Ring::create` in `supervisor.rs`) before
  the worker has opened the device, so the node cannot know the native format. The ring stays a
  number of fixed-size slots in bytes (2 MiB each, as now); the worker, once the device is open,
  writes the sample format into the ring's header before its first slot and names it in the
  hello, and the node converts per slot by what the hello says. An `sc16` slot holds up to twice
  the samples, so the same ring holds twice the time. The ring's `VERSION` becomes 2 and
  `PROTOCOL` becomes 3.
- When the device's native format is `CS16`, the worker writes the device's samples to the ring
  unconverted. `SoapySource` hands out only `cf32` blocks through `Port::subscribe` today, so
  this is a second, raw path out of the reader for the worker's use, around `Port`; the
  discard of 4.1 sits before the split so both paths obey the contract. The node converts to
  `Complex<f32>` in `from_ring`, with the same scaling `cs16_to_cf32` uses today (one shared
  function, so a capture is bit-identical either way; a test pins it). A `CS8` device (the
  HackRF) and a `CF32` device keep `cf32` on the ring in this plan: `sc8` is the same mechanism
  and is left for when it is wanted. `--soapy-in-process` is unchanged.
- The pipe fallback (`Frame::Block`) keeps `cf32`.
- The worker is the node's own binary run as `zeta soapy-worker`, so a node and a worker of
  different versions meet only across a deploy's restart. A mismatch of `PROTOCOL` or of the
  ring's `VERSION` is refused with a fault that names both versions, as a protocol mismatch is
  today; falling back to the pipe is not required. A format the node does not know is the same
  refusal.
- The honest size of the win, from the spike: the reader's CPU is the same either way, so this
  does not speed the sweep. It halves the bytes through shared memory (246 to 123 MB/s at
  30.72 Msps) and doubles the ring's depth, which is what a 61.44 Msps or FPGA front end will
  need. It is in this plan because the operator asked for it here and the sweep now runs the
  B210 at its top rate by default.

### 4.6a The sample depth on the device's link (36.1)

Added with the operator, 2026-11-11: "is there a 12bit mode from the b210 too?", then "add it to
36.1, and an option to drop to 8bit for even faster sweep". This is a different link from 4.6:
the device to its driver (USB), not the worker to the node (shared memory). The two stack.

What exists, from UHD's B200 source (`host/lib/usrp/b200/b200_io_impl.cpp`, the `otw_format`
stream argument): `sc16` (4 bytes a sample, the default and what the node gets today, because
the driver opens the stream with no arguments), `sc12` (3 bytes, lossless: the AD9361's
converter is 12-bit), `sc8` (2 bytes, the low 4 bits dropped, about 48 dB of dynamic range) and
`fc32` (8 bytes, no use here). UHD unpacks on the host, so the reader still receives `CS16` or
`CF32`. Whether a narrower link format raises this radio's clean rate above 30.72 Msps is not
measured; the mechanism is built so the probe answers it.

As one mechanism, not a B210 special:

- **Capability.** `Capabilities` gains an optional `depths`: the sample depths a source's link
  can carry, each `{bits, lossless, max_bandwidth_hz}` with the bandwidth a `Claim` as the
  top-level one is. A source without the notion has none. The Soapy source fills it from the
  driver's stream-argument information where the driver names its link formats (SoapyUHD's
  `WIRE` argument), and the top-level `max_bandwidth_hz` stays the figure for the depth in
  force.
- **The source's depth.** A device argument on `--source-soapy` (the builder names it) sets
  the link depth the source runs at. Unset, the source runs the deepest **lossless** depth
  whose measured rate is highest: on a B210, `sc12` if the probe finds it sustains more than
  `sc16`, else `sc16`. A lossy depth is never chosen by default.
- **The probe.** The rate ladder (`probe.rs`, plan 28.1) runs per depth. At open it probes the
  depths the driver names, lossy ones included: a source's capability record cannot change
  while it runs (the worker's hello carries it once), so every depth's rate is measured at open
  and cached in the source record as the rate cap is today. The top-level `max_bandwidth_hz`
  is the figure for the source's own depth.
- **The probe says what it is doing** (the operator, 2026-11-11: "the high load may also lead
  to user wondering why their system is unresponsive whilst that happens. we probably want to
  warn them"). A probe of three depths streams the radio flat out for up to about a minute on
  a source's first start, before the worker's hello, and today the only sign is one line on the
  worker's stderr when it ends. While a worker measures, it reports progress to the node (what
  it is measuring, which depth and rate, step n of m, the time left at most); the node shows it
  in the source's `worker` record in `source_list`, prints one line when it starts ("measuring
  what this radio's link can sustain (up to about N s; the host will be busy; `max_rate_hz=`
  skips it)", with a reason first inside the parentheses for a link-speed change or a
  `source_measure`) and one when it ends ("measured in N s"), puts both on the source's event
  feed, and the page shows the same notice on the source's tab while it lasts. A start from
  cached figures measures nothing and says nothing.
- **A measurement belongs to the link's speed** (the operator, 2026-11-11: "a host device
  shuffle may also impact performance", then: "rescan is really only needed if underlying port
  speed changes, or if we somehow notice system becoming unresponsive at intervals because of
  interaction with adjacent usb devices being added around ours"). A source reports its
  `attachment` where it can tell (a Soapy USB device: the bus and port path and the negotiated
  speed, found from the device's serial; a source that cannot tell reports none), and
  `source_list` shows it.
  - **A source measures again by itself only when the negotiated speed differs** from the one
    the cached figures were measured at (a USB 3 radio now on a USB 2 port, or the reverse),
    with the notice above saying why. Another port or controller at the same speed keeps the
    cache.
  - **Otherwise it is the operator's choice.** A new operation, `source_measure`, drops a
    source's cached figures and measures again; it is refused with a conflict while jobs hold
    the source. The page offers it on the source's tab as "Run link diagnostics" (the
    operator's words: offer it "as an option to 're-calibrate' or 'running diagnostics'"),
    with a line saying what it does, how long it takes and that the host will be busy, and the
    result shown when it ends (each depth's measured rate, and what changed from before). It is
    what to reach for when the system stutters after devices were added around the radio.
  - **Deferred** (the operator, 2026-11-11: "deferred is right"): the node noticing by itself
    that a link is struggling (recurring sample loss at a rate measured clean) and measuring
    again. It stays with the host-capabilities baseline (`TODO.md`, "Node: a baseline of the
    host's capabilities"), and this plan's `TODO.md` entry for it names the tune-gap counter
    of 4.1 as the piece that makes steady-streaming loss countable.
- **The trait.** `Port` gains a defaulted `set_depth(bits)` in the shape of `set_gain`: the
  source reopens its stream at that depth (a stream argument is fixed when the stream is set
  up, so this costs what a rate change costs today) and reports the new `max_bandwidth_hz`.
  The default is "not supported".
- **The scan's option.** `job_start` for a `scan` takes an optional `bits` (the value must be
  one of the source's `depths`). A scan asking for a lossy depth, 8 on a B210, holds the source
  at that depth for its run and steps at that depth's width; when it stops, the source returns
  to its own depth. Because every holder of the source would get the coarser samples, a scan
  with a lossy `bits` is granted only when it is the source's sole holder and is a `conflict`
  naming the others otherwise, and a job started on the source while it runs is the usual
  conflict naming the scan. The scan record shows `bits`, and each row carries it, so a row
  taken at 8 bits is never read as a full-depth one. The page's presets (4.5) gain "coarse and
  fast" (the lossy depth, where the source has one).
- **What it costs, stated where the option is offered** (the agents' guide, the field craft,
  the preset's tooltip): at 8 bits a weak signal beside a strong one in the same capture is
  lost in quantisation noise. It is for a coarse survey of occupancy, not for the faint-signal
  look, and it is never the default.

### 4.7 A source that sweeps for itself (36.2)

One mechanism, per the "spectrum rows" gist and narrowed to what a scan needs:

- **Capability.** `Capabilities` gains an optional `sweep`: `{min_hz, max_hz, bin_hz_min,
  rows_per_s_max, calibrated}` for a source that can deliver power spectra of spans inside
  `min_hz..max_hz` itself. `calibrated` says the levels are absolute (dBm at the antenna), which
  a local radio's rows are not.
- **Trait.** `Port` gains a defaulted method, in the shape of
  `async fn sweep(&self, req: SweepRequest) -> Result<SweepRows, PortError>` where the request
  names a span, a target bin width and a dwell, and the result is a receiver of rows
  (`center_hz`, `bin_hz`, power per bin in dB, the time span) plus the plan the source chose
  (steps, bin width). The default returns "not supported". The builder settles the exact types;
  what is fixed is that the source chooses its own stepping and the node never learns how.
- **Scan.** `launch_scan` asks the port's capability: a span inside `sweep` and no explicit
  request for IQ goes to a self-sweep loop that forwards the source's rows on the scan's event
  stream in the scan row shape (quantised by the same `quantise_row_masked`, `floor_db` and
  `step_db` computed the same way, `scan/<step>` channels numbered by the source's steps), and
  fills `context.scan` with the source's plan and the same `timing`. A scan's `method` in the
  record says `stepped` or `source`; `job_start` takes an optional `method` to force
  `stepped`.
- **Lease.** A self-sweep does not retune the source's IQ channel, so it does not take the
  time-shared lease a stepped scan takes: it holds a separate, shared `sweep` claim on the
  source that conflicts only with what the source says it cannot do at once (nothing, on a
  Kiwi with a free waterfall channel). The builder fits this to `lease.rs` without changing the
  rules for IQ holders.
- **When the source cannot sweep.** At start, a refused `sweep` makes the scan try `stepped`,
  which needs the time-shared lease as any stepped scan does: with the IQ channel held (a
  demodulator on the Kiwi) that is today's `conflict` naming the holders, with the source's
  reason for refusing the sweep added to its message. The scan does not wait and does not
  replace anyone. Mid-run, a lost sweep (the socket, the waterfall channel) is retried by the
  source with the session's own backoff while the scan stays up and its record says `waiting`
  with the reason; it does not change method while running.
- **Averaging and dwell** apply as in 4.5: `dwell_ms` is passed to the source, `average` is
  applied by the node to the rows it forwards.

### 4.8 The KiwiSDR's waterfall as the first self-sweeping source (36.2)

From the gist's reading of the server source (frame format, zoom, rates; `TODO.md`, "Sources: a
source that delivers spectrum rows"):

- The Kiwi source opens a second websocket, `W/F`, on demand (the first `sweep` call) and
  closes it when the last sweep ends. Read from the server's source for plan 36.2 (v1.711,
  cloned to `scratch/upstream/`; the plan's first task writes the note): a Kiwi pairs a
  client's `SND` and `W/F` sockets by the timestamp in their paths into one user, but closing
  a paired `W/F` stops its `SND`. A sweep must be able to end without ending the demodulator
  beside it, so **the `W/F` socket always stands alone**, with a timestamp of its own that
  cannot collide with the IQ session's, and costs one user slot of the Kiwi beyond the IQ
  session's while a sweep runs. That is the anti-goal's "one waterfall connection": never
  more than that one slot, and none when no sweep is running. (The plan's call, for the
  operator to check: section 8.)
  `proto.rs` gains the `W/F` frame codec (the 16-byte
  header, 1024 bytes of `dB = byte - 255`, compression off with `SET wf_comp=0`) and the
  commands (`SET zoom= cf=`, `SET wf_speed=4`, `SET maxdb/mindb`, `SET interp=`).
- A sweep of a span at a target bin width picks the zoom whose pixel is at or under the target
  (zoom Z spans 30 MHz / 2^Z over 1024 pixels: zoom 2 is 7.3 kHz a pixel in four steps, zoom 3
  is 3.7 kHz in eight) and steps `cf` across the span, taking frames per step for the dwell
  after discarding the frames a zoom or centre change leaves unsettled (measured in the plan's
  live task, as the tune's settle was for the B210; the frame header's `x_bin` and zoom say
  which view a frame belongs to, which is the contract of 4.1 for this stream).
- The scan's default target bin on a self-sweeping source is the source's `bin_hz_min` or
  `SPECTRUM_BIN_TARGET_HZ` (6 kHz), whichever is coarser to reach within the goal's time; on
  the Kiwi that is zoom 2 or 3 for the whole band. At zoom 0 the whole band is one frame, 23
  times a second, 29 kHz a pixel: the scan uses it when the operator asks for the coarsest
  bins.
- Rows are `calibrated`: the server's dBm scale plus its `MSG wf_cal=` offset. The first 2-4
  pixels of a frame (zeroed for DC by the server) are reported as `masked`.
- When the Kiwi refuses the socket (every slot or waterfall channel taken: the server answers
  `too_busy`), `sweep` fails with that reason, and the scan falls back to `stepped` and says
  why in its record. The Kiwi's status page does not say how many waterfall channels it has,
  so the capability is asserted and the refusal is what corrects a wrong hope.
- The `SND` connection, its 12 kHz IQ channel and its lease are untouched: a demodulator runs
  on the Kiwi while the sweep paints its band.

## 5. Where the code goes

| Piece | Place |
|---|---|
| The settled-samples contract and its Soapy implementation | `zeta-core/src/port.rs` (doc), `zeta-source-soapy/src/source.rs`, `caps.rs` |
| The same for the Kiwi's `SND` | `zeta-source-kiwi/src/session.rs` |
| The step loop, the hand-off, timing, averaging | `zeta-core/src/node/launch.rs` (the scan grows its own module, `node/scan.rs`, if the builder prefers; `launch.rs` is already large) |
| The grid by `usable_fraction`, the scan fields, the constants | `zeta-core/src/ops.rs`, `types.rs` |
| Averaging rows across sweeps | `zeta-dsp/src/fft.rs` or beside the scan loop (builder's call) |
| Link depths: the capability, the device argument, the probe per depth, `set_depth`, the scan's `bits` | `zeta-core/src/{types,port,ops}.rs`, `zeta-source-soapy/src/{source,caps,probe}.rs`, `zeta-source-soapy-driver/src/lib.rs` (`open_rx` takes stream arguments), `zeta-source-worker` (the op across the worker's pipe), `crates/zeta/src/soapy_worker.rs` |
| `sc16` ring | `zeta-source-worker/src/{ring,wire,serve,supervisor}.rs`, `zeta-source-soapy/src/{source,convert}.rs`, `zeta-source-soapy-driver` |
| The `sweep` capability and trait method, the self-sweep scan loop, the claim | `zeta-core/src/{types,port,lease}.rs`, `node/launch.rs` |
| The `W/F` codec and session | `zeta-source-kiwi/src/{proto,session,source}.rs` |
| The page's presets and `sweep_ms` | `zeta-ui/web/{menu,agent}.js` |
| Prose for agents | `crates/zeta/src/agents_md.rs` (the scan note names 16 MHz and 250 ms today), `docs/agents/field-craft.md`; `AGENTS.md` and the skill regenerated |

## 6. Proof

- **Unit.** The grid at 0.9 of a rate keeps every frequency clean in one of two sweeps; the
  step count for a span; averaging (N sweeps of a noisy step lower the ripple as expected and
  `sweeps` counts up); the row of a dwell shorter than 50 ms; `sc16` and `cf32` rings give
  bit-identical blocks; a ring or protocol of the wrong version, or an unknown format, is refused with a fault naming both versions (4.6); the `W/F`
  codec on frames built to the server's format (header, the DC pixels, compression refused).
- **The contract, per source.** A Soapy source over a scripted device (the test device behind
  `zeta-source-soapy/tests/probe.rs`) whose stream has zeros and old-band samples around a
  tune: the first delivered block of the new centre has neither. The Kiwi session on scripted
  `SND` frames: a frame of the old frequency after a tune is dropped.
- **The scan, on a file source and the fake port** (`zeta-core/tests/scan.rs`): no step waits
  longer than its samples take to arrive (the loop is driven by the ring, so a port that
  delivers fast is stepped fast); a port that stops delivering makes a counted skip, not a hang;
  rows come out in step order with the FFT handed off; the record's `timing` fills. The tests
  that pin 16 MHz steps and a 250 ms dwell are updated, and
  `scan_context_carries_its_step_count` keeps a pinned step count.
- **A self-sweeping fake port** in the same suite: the scan forwards its rows in the scan row
  shape, takes no time-shared lease, runs beside a demodulator on the same source, falls back to
  `stepped` when the port refuses, and `method` forces it.
- **The UI end to end**: the existing scan scenarios pass unchanged (they assume no pace); one
  scenario starts each preset and reads `sweep_ms` on the row.
- **Live, on the field host** (written against roles in `UAT.md`; run with what is attached):
  - a radio with a measured rate cap sweeps its whole range at the default: the sweep time, the
    `timing` record, and the first row of a step against its later rows (0.5 dB). On the B210:
    20 s or less;
  - the same span three ways (fast, averaged, long look) with a weak steady carrier in it and a
    keyed one: the averaged look shows the weak one, the fast look catches the keyed one;
  - base against head on one quiet span: the floor and the masked ranges agree (the DSP
    fix-round lesson: fresh captures after the deploy);
  - a radio whose link has more than one depth: `source list` shows each depth's measured rate
    and the one in force; a scan at the lossy depth sweeps at that depth's width, its rows say
    so, a second job on the source is refused while it runs, and the source is back at its own
    depth after it stops;
  - for the follow-up on widening a held capture: each depth's measured rate is written into
    the check's result, and, where a lossy depth is the only one wide enough for twice a held
    capture, a narrowband demodulator's audio and a decoder's frame count on a known channel
    are compared at the source's own depth and at the lossy one, with the strongest signal in
    the wider capture noted;
  - a first start measures aloud: the notice on the page, in `source list` and in the log, for
    about the time it promised; the radio moved to a port of another speed is measured again
    and says why, and to another port of the same speed is not; "Run link diagnostics" on the
    radio's tab measures again, shows the result against the last one, and is refused while a
    job holds the radio; `source measure` is refused while a job holds the radio;
  - `sc16`: the worker record names the format, a demodulator on a known signal sounds as
    before, and the ring's drops stay zero at the top rate;
  - a KiwiSDR sweeps 0-30 MHz at the default in 5 s or less while a demodulator plays on it;
    the levels read as the Kiwi's own page shows them; with the waterfall channel taken, the
    scan says it fell back.

## 7. Left to the builder

- The mechanism of the bounded FFT hand-off (a blocking task, a one-slot channel) and where
  averaging lives.
- How the Soapy reader measures "taken after the return" on a device with and without hardware
  timestamps, and whether the read is shortened while a scan holds the source.
- The per-driver settle defaults other than UHD B2xx, and the names of the device arguments.
- How the Kiwi session knows a `SND` tune took effect (a sequence field, or a measured delay).
- Whether `dwell_ms` and `average` change on a running scan through `graph_put` or a retune.
- The exact types of `SweepRequest` and `SweepRows`, and the shape of the `sweep` claim in
  `lease.rs`.
- The Kiwi's zoom choice rule and the settle after `SET zoom= cf=`, from the live measurement.
- Whether `scan` moves to its own module.
- The split of each plan into tasks, and the model for each.

## 8. Open questions for the operator

None blocks the plans. Calls made on the operator's behalf, to check while reading:

0. Answered 2026-11-11: the link's sample depth is part of 36.1, with 8-bit as a scan option
   (4.6a).
1. The default look is fast (20 ms, no averaging); averaged and long looks are a field and a
   preset away (4.5).
2. `sc16` is built although the spike shows it does not speed the sweep (4.6): it was asked for,
   it is small, and it is what the next wider front end needs.
3. The B210's calibration cost is reported, not removed (non-goals): removing it means patching
   UHD or new gateware.
4. Two plans, 36.1 then 36.2; the Kiwi's waterfall is the only self-sweeping source built, the
   HackRF's firmware sweep and the clone's gateware are shaped for and left.
6. The Kiwi sweep takes one user slot of its own while it runs (4.8): pairing it with the IQ
   session would save the slot, but then ending the sweep would end the demodulator.
5. Found by the spike and not part of this plan: the deployed B210 worker's record showed 2395
   restarts. It goes to `TODO.md` as its own entry to look into.
