# ZetaSDR plan series

The design (`docs/superpowers/specs/2026-10-18-zetasdr-design.md`) describes
six subsystems. Each gets its own plan; each plan ends with working,
testable software. Order is dependency order. Plan 1 is written; the rest
are written when their predecessor is done, so they can argue from real
interfaces rather than guessed ones.

| # | Plan | Delivers | Depends on |
|---|------|----------|------------|
| 1 | Core node | Cargo workspace, typed streams, port trait with leases, IQ ring buffer, SigMF file source, graph document, persistent job runtime, operation set with JSON schema and corrective errors, stage registry with multi-implementation selection, knowledge and classifier traits with stubs, `zeta` CLI, AGENTS.md seed generated from the schema. Test suite passes with no hardware. | none |
| 2 | DSP slice | Channeliser, NBFM demod to an audio stream, AFSK1200/APRS decode-to-identify to an event stream, layer-1 classical measurement producing feature records, CPU and GPU FFT implementations of one stage. **Delivered** on branch `dsp-slice`, 2026-10-19. GPU hardware equivalence (`gpu_matches_cpu_within_0_1_db`) is pending on the field host. | 1 |
| 3 | Transports and agent | Websocket+JSON server with shared secret, MCP server generated from the schema, OpenAI-compatible in-process agent adapter (thinking off, token budget, readiness sequence, prose-answer detection), skill and tool descriptions generated from AGENTS.md. **Delivered** on branch `transports-agent`, 2026-10-19. | 1 |
| 4 | Hardware and foreign sources | HackRF via SoapySDR plugin crate, KiwiSDR websocket client, external-process plugin SDK (Python) speaking the typed envelope. **Delivered** on branch `hardware-sources`, 2026-10-19. The `cfg(soapy)` driver build and clippy passed against SoapySDR 0.8.1 once `libsoapysdr-dev` was installed later the same day. Pending: the HackRF hardware test; GPU equivalence on the field host is still pending from plan 2. | 1, 2 |
| 5 | Federation | Node-to-node: VITA 49.2 for IQ, typed envelope for the rest, back-pressure, shared secret, Pi 5 build and deployment, remote node as a source. **Delivered** on branch `federation`, 2026-10-19. Pending: deployment on Pi hardware (no Pi is set up yet), the one-shot `--then-tail` finding against the B210, and releasing the remote job when the local job stops. | 3, 4 |
| 6 | UI | Wide renderer (waterfall, ribbons, context menu, agent pane, jobs), narrow renderer, graph view. All clients of the plan-3 websocket. **Delivered** on branch `ui`, 2026-10-19. Pending: the phone check of the narrow renderer, the microphone against a real `voiced`, the HackRF walk, and a live `/ask` answer from the field host. | 3 |
| 7 | Audio path and wideband FM | Rational resampler so every demod outputs 48 kHz audio; `wfm_demod`, `am_demod` and `ssb_demod` job kinds (`--sideband`); layer-1 rules for `wfm` (19 kHz pilot as evidence), `am` and `ssb`; iNTERCEPT signal catalogue (Apache-2.0, 594 entries) as knowledge provider and layer-2 classifier; UI decode entries and playback of any audio job. **Delivered** on branch `audio-wfm`, 2026-10-19. Pending: AM and SSB playback in a browser. | 2, 6 |
| 8 | Wideband | Polyphase filter bank shared by every job on a wide capture (a channel that fits one bin is bank-served, wider ones take the direct path); spectrum rows v2 (fixed `rows_per_s`, u8-quantised `q`, up to 8192 bins); UI band control (zoom and pan inside the capture render only, beyond it retune); `scan` job that time-shares a radio across a span wider than one capture and holds the lease on it; IQ ring sized from a memory budget (`--ring-budget-mb`); `wfm_demod` decimates before its audio filter. **Delivered** on branch `wideband`, 2026-10-20. Pending: the fix wave (sub-band scan clamp, overlays and scan redraw), the B210 at 24 Msps (its asserted `max_bandwidth_hz` is 16 MHz), and the bank's 20 % cost target (33 % measured at 16 Msps). | 2, 6, 7 |
| 9 | Decoders | Four decode job kinds: `pocsag_decode` (12.5 kHz FM channel, 512/1200/2400 baud detected automatically), `rds_decode` (200 kHz FM broadcast band; `pi`, `ps`, `rt`), `adsb_decode` (1 090 MHz, 2.4 MHz band, dump1090 run on the node's host through the new crate `zeta-ext`) and `acars_decode` (10 kHz AM channel, 2400 baud MSK); decode-to-identify in `classify` (APRS, POCSAG and ACARS on narrow bands, RDS on wide ones); `decoder` on catalogue signatures; UI entries; field craft for pagers, aircraft and FM data. **Delivered** on branch `decoders`, 2026-10-20. Pending: live acceptance on the field host. | 2, 7, 8 |
| 10 | Speech on a channel | Transcription of a radio channel: audio frames carry a channel count; stages `denoise` (RNNoise via `nnnoiseless`), `vad` (energy-over-floor voice gate) and `transcribe` (new crate `zeta-speech`, speech backends `SocketBackend` for a `local-voice`-protocol daemon and `NodeBackend` for another node's `/stt`, global flags `--stt-socket` and `--stt-node`); attached jobs (`stream_in` stage, `job_attach` op, `zeta job attach`) so any audio-producing job, demodulators now and digital voice decoders later, can be transcribed; UI entries and transcript ribbons; the source-separation slot proven with an external audio plugin (`examples/python/separate_stub.py`) swapped in with `graph put`; `docs/speech.md`. **Delivered** on branch `speech`, 2026-10-20. Pending: live acceptance on the field host (needs `local-voice` installed there, `docs/speech.md` section 1). | 2, 7, 9 |
| 11 | View policy | Resume restores the capture (widest first, built-in graphs re-expanded, `capture_rate_hz`); the ops log (`ops.log`, `zeta ops tail`); elastic leases (`job start --elastic`); atomic `replace` (`--replace` on `job start` and `source ensure`); a file-source `tune_range`; UI view policies (`follow`, `zoom_out`, `auto_scan_stop`, `finished_rows_s`, `scanner`) and `view set\|show\|reset\|clear`, automatic scans on a scanner source, a coarse scan layer, eased follow. **Delivered** on branch `worktree-view-policy`, 2026-10-20. Pending: live acceptance on the field host. | 3, 10 |
| 12 | Capture and transcripts | Resumed channels take the shared bank (the bank factory reaches `Node::open`; the channel's path in `job_context`, `bank` in `source_list`, lag lines by stage kind); rates the resampler accepts (a UHD ladder of exact master-clock divisors in the Soapy source, a snapping `Resampler`); the transcript log (`transcripts.log`, `zeta transcripts tail`); pinned captures (`source_pin`/`source_unpin`, `sources/<id>.json`, restored first on resume); live receive gain (`source_set_gain`, `source_list.gain`, persisted); UI pins and `source pin\|unpin\|gain` commands; `ppm=` correction and an automatic squelch in `fm_demod`/`am_demod`. Spec `docs/superpowers/specs/2026-10-21-capture-and-transcripts-design.md`, plan `docs/superpowers/plans/2026-10-21-capture-and-transcripts.md`. Delivered on branch `worktree-view-policy`, 2026-10-21. Pending: live acceptance on the field host. | 8, 10, 11 |
| 13 | Channel plans | A table-driven channel-plan registry in `zeta-core` (`uhf_cb_au`, 80 channels; `channel_plans` op, `zeta plans`); the page names channels on job rows, outlines and the axis (`view set plan auto\|<id>\|none`); an adjacent-channel guard in `nbfm_demod` for plan channels (a widened channel, the centre filtered in the stage, the gate held closed under a stronger neighbour; `guard_hz`/`guard_db` node params, `adjacent_db`/`guarded` on `squelch` events); `transcribe`'s `min_utterance_ms` (700); a bounded speech-daemon queue (4 connections, `queued_ms`, `busy`/`queue` drop reasons, one EAGAIN retry); optionally `examples/python/cb_watch.py` (demodulators only on active channels). Spec `docs/superpowers/specs/2026-10-21-channel-plans-design.md`, plan `docs/superpowers/plans/2026-10-21-channel-plans.md`. Delivered on branch `plan-13`, 2026-10-21 (tasks 1-5, base plan 12). Pending: live acceptance on the field host. | 10, 11, 12 |
| 14 | Field fix wave | The four defects of the 2026-10-23 HackRF session: the Soapy clock guard and stream reopen on a rate change (`source_list.stats`), a run generation on job entries so a stopped run cannot outlive its job (`/health` `stages_running`), `measure` partial windows and discard notes (`BankInfo.dropped`), device loss ending in `abandon` and an overflow-storm fault; field craft for a radio in its own process. Plan `docs/superpowers/plans/2026-10-27-field-fix-wave.md`. **Delivered** on branch `worktree-plan-14`, 2026-10-27 (tasks 1-5), merged to `main`. Live acceptance on the HackRF 2026-10-28 (short form, see "Plan 14 status"): clock, leak, measure and device loss pass; the child node survives a cable pull but must be restarted by hand. | 8, 11, 12 |

Plan 5 completes the first vertical slice (design section 8). Plan 6
satisfies the GUI half of slice item 4: the context menu's entries and the
MCP tools reach the same `Node::apply` code path, and the call shows in the
UI's transcript pane through the node's `ops` stream.

## Sprint 2026-10-27

Ordered from `TODO.md` on 2026-10-27, after plan 13's live acceptance on the field host. Each row is one plan;
specs are written in a brainstorming session before the plan, as before.

| Order | Plan | Why here | Depends on |
|---|---|---|---|
| 14 | Field fix wave (planned, above) | The timestamp bug blocks every UTC-slotted mode (FT8/FT4/WSPR) and TDMA (DMR, TETRA); the run leak would surface in minutes under a span of decoders; `measure` silence blocks layer 4 and `cb_watch.py`. | - |
| 15 | Scan: alternate the step grid (odd sweeps a quarter step lower) and mask the centre and edge bins | Spec gist in `TODO.md`; the survey tooling built on scan rows inherits the DC spike and edges today. Spec first. Spec `docs/superpowers/specs/2026-10-31-scan-grid-mask-design.md`, plan `docs/superpowers/plans/2026-10-31-scan-grid-mask.md`. **Merged** to `main` 2026-11-01 (83726a2); live acceptance on the desktop HackRF pending (see "Plan 15 status"). | 14 (`stats`, no leaks under repeated retunes) |
| 16 | Mode registry and external-decoder adaptor | Design spec first (brainstorm): a table-driven mode registry replacing the `JobKind` match for decoders, a declarative stdio wrapper for external decoders, the `video`/`text`/bits payload envelopes, a common decoded-event envelope and feed sinks, `tools/install-vocoders.sh`. No new mode ships here; `adsb_decode` is re-expressed as the first adaptor row. **Merged** to `main` (91886bc), 2026-10-28. Live acceptance on the HackRF 2026-10-28: registry, check, broken row, handlers, unknown kinds and `identify`'s unknown path pass; the on-air decode proofs await an antenna (see "Plan 16 status"). | 14 |
| 17 | Sonde frameworks (decoder wave 1) | Re-scoped 2026-10-28 with the operator: radiosondes as the proving mode for the generic frameworks, so that the later wave is rows only. Audio-input program rows (RS41 through `rs41mod`, DFM and M20 as rows), generic `fsk`/`psk`/`ook` synthesiser families plus an RS41 frame generator proven by the real decoder and the echo impairment, a `watch` job (spot a new signal in a pinned band, `identify` it, keep the proved decoder on a free radio or in the capture), `source_free`, `position` on events with a map layer (vendored Leaflet), a SondeHub sink last. Spec `docs/superpowers/specs/2026-10-28-sonde-frameworks-design.md`, plan `docs/superpowers/plans/2026-10-28-sonde-frameworks.md`. Voice (M17, DMR), AIS, FLEX, whole-span POCSAG/APRS and stereo move to the later wave (`TODO.md`). **Merged** to `main` (97383b0), 2026-10-29; live acceptance during a Brisbane launch window is pending (see "Plan 17 status"). | 16 |
| 18 | UI wave | Box a signal, demod it, refine it on the graph (no channel-grid assumptions); staircase transcript ribbons; mode picking by band through `knowledge_lookup`; the console theme behind `view set theme`. UI-only; can run beside 16/17. **Merged** to `main` 2026-11-01 (771ea93); live acceptance on the desktop HackRF pending (see "Plan 18 status"). | 13 |
| 19.1 | Layer 4 explore: features, the register, burst captures | Burst-aware layer 1 (`duty`, `bursts`, `burst_ms`, `period_ms`, the cyclostationary rate, FSK/PSK order, `psk`/`ook` labels); the node's station and antennas; the `acma` knowledge provider over the operator's register fork (MCP over HTTP) with the reach rule; identify, classify and the watch consulting the register; a detector that holds on a real receiver; the `capture` job and its store, the `capture_*` ops, `on_unknown: capture`; the pane tab strip and the Explore pane. spec `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`, plan `docs/superpowers/plans/2026-11-02-layer4-explore.md`. **Merged** to `main` 2026-11-03 (fast-forward, see "Plan 19.1 status"); live acceptance deferred to after 19.3 by the operator. | 16 (bits payload) |
| 19.2 | Layer 4 bits | Stages over a capture: `capture_in`, `resample`, fsk/psk/ook demods, clock recovery, slicer, `bit_view`, a Bits tab. spec `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`, plan `docs/superpowers/plans/2026-11-03-layer4-bits.md`. **Merged** to `main` (see "Plan 19.2 status"); live acceptance after 19.3. | 19.1 |
| 19.3 | Layer 4 frames and signatures | `frame_decode`, thin serial-protocol rows, operator-accepted signature write-back. spec `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`, not started. | 19.2 |
| 20 | Soapy worker process | A `zeta soapy-worker` child owning the driver behind a Port that faults when the child exits, so a driver crash never ends the node, and a source that heals is recovered. Spec `docs/superpowers/specs/2026-10-31-soapy-worker-design.md`, plan `docs/superpowers/plans/2026-10-31-soapy-worker.md`. **Merged** to `main` 2026-10-31 (3acbad4); live acceptance on the field host partly done (see "Plan 20 status"). | 14 |
| 21 | Agent bulk operations | `plan_start` (a kind, with an optional attach, on every channel of a plan under one lease decision), the agent's `Allow` gate (no unasked `job_stop` or `replace`), duplicate rigid starts refused, stage readiness, the turn budget and the model's reasoning in the transcript. Spec `docs/superpowers/specs/2026-11-01-agent-bulk-ops-design.md`, plan `docs/superpowers/plans/2026-11-01-agent-bulk-ops.md`. **Merged** to `main` 2026-11-01 (09080b0) and deployed on the field host; `jobs_stop` followed after the first evaluation (the stop-all request hit the turn limit); see "Plan 21 status". | 10, 18 |
| 22 | Listening to many channels | `mix_start` and the `mix` job: one audio stream summing every demodulator on any set of tuners (no lease, no transcript; transcription stays per demodulator), who is talking as `active` events, mutes by `{source, center_hz}`, "Listen to all" on the page. Spec `docs/superpowers/specs/2026-11-01-audio-mix-design.md`, plan `docs/superpowers/plans/2026-11-01-audio-mix.md`. **Merged** to `main` 2026-11-01 (fd126b0) and deployed on the field host; see "Plan 22 status". | 21 |
| 23 | Subjects | A mode row names what its events are about (`subject`: key, table columns, waterfall `summary` or `fold`, heading, label, units, `track_points`, `track_min_m`); the page merges events per subject (`subjects.js`), a Subjects tab lists one row per aircraft, sonde or station, the waterfall shows one line per job, the map tracks per subject; the agent may stop the jobs it started itself; the guide says plan ids come from `channel_plans`; `TODO.md` tidied. Spec `docs/superpowers/specs/2026-11-02-subjects-design.md`, plan `docs/superpowers/plans/2026-11-02-subjects.md`. **Merged** to `main` 2026-11-02 (812cf1b) and deployed on the field host; live acceptance open (`UAT.md` section 23). | 21, 22 |
| 24 | Console theme | The web page's console theme: elbow-and-bar frame with live source health on the tab bar, the rail of job tags beside the frequency axis (thin segments where they crowd), ribbons that ride their band tilted and step up a lane, two-line job rows, idle plan channels folded into one group, collapsible chat and jobs panes, the graph modal as coloured bars; `view set theme console\|plain` and `view set side`. Web UI only. Spec `docs/superpowers/specs/2026-11-03-console-theme-design.md`. **Merged** to `main` (4098fd2) and deployed on the field host 2026-11-03 as `~/zetasdr/plan24-zeta`; see "Plan 24 status". | 18 |
| 25 | Public release | The audit and packaging for a public repository. Parked on `worktree-public-release-plan` (a01b71c), not on `main`: re-audit and confirm its seven rulings before running. | 24 |
| 26 | Themes as folders | Themes as folders (`theme.json`, `style.css`, assets), built-ins and the operator's `--themes-dir`, `themes_list`, served under `/ui/themes/<id>/`; cyberpunk, terminal, psytrance, vibecoder. Spec `docs/superpowers/specs/2026-11-03-themes-design.md`, plan `docs/superpowers/plans/2026-11-03-themes.md`. Merged 576002a, 2026-11-04. | 24 |


### Outcome (closed 2026-11-01)

Six of the seven plans shipped; plan 19 was not started then and opens the next sprint as 19.1, 19.2 and 19.3.

| Plan | Merged | Proved live | Rolls forward |
|---|---|---|---|
| 14 field fix wave | 2026-10-27 (Plan 14 status) | short-form clock/leak/measure/device-loss on the HackRF (Plan 14 status) | the 2 h clock run |
| 15 scan grid and masks | 83726a2, 2026-11-01 | the mechanism on the B210 (alternation, `masked` rows, `graph put`, `job_context`); the HackRF constants measured on the field host 2026-11-01 (`SCAN_NOTCH_HZ` 50 kHz, `SCAN_EDGE_MASK` 0.02) | the HackRF's 4 MHz spur grid under the quarter-step shift (`TODO.md`) |
| 16 mode registry | 91886bc, 2026-10-28 | registry, check, broken rows, handlers, identify-unknown on the HackRF | on-air POCSAG, APRS and ADS-B proofs |
| 17 sonde frameworks | 97383b0, 2026-10-29 | RS41 X1142896 decoded on the B210 2026-11-01 (pre-placed decoder) | the watch finding a sonde on its own (detector, re-centre) |
| 18 box, fit, refine | 771ea93, 2026-11-01 | retune, drag, menu, tags, conflict and the fit mechanism on the B210; the DC-in-box outcome on the HackRF; the fit on a transmitting CB channel (ch9, +450 Hz, transcripts) on the B210 | the fitted width on a weak voice channel (the 5 kHz floor; `TODO.md`) |
| 19.1 layer 4 explore | 1b57cf7, 2026-11-03 (fast-forward; Plan 19.1 status) | not yet: acceptance deferred to after 19.3 | the live acceptance; the B210 noise recording; 19.3 not started |
| 19.2 layer 4 bits | fast-forward to 0b90359, 2026-11-04 (Plan 19.2 status); the OOK run-length fit after it (49ceb21..1ef6c69) | not yet: spec 9 item 3 after 19.3; the first real OOK remote (433.882 MHz) not recovered end to end | the test_host check (`UAT.md` Plan 19.2); 19.3 not started |
| 26 themes as folders | 576002a, 2026-11-04 (Plan 26 status) | deployed as plan26-zeta 2026-11-04; UAT open | per-footprint layouts, a node-level default theme (`TODO.md`) |
| 20 Soapy worker | 3acbad4, 2026-10-31 | items 1-2 on the B210 (SIGSEGV heal); the HackRF's cable pull and SIGSEGV on the field host's two-radio node 2026-11-01 | the B210's own cable pull |

Also shipped in the sprint's last days: the stream socket pool 16 → 64 (9d736ab), the
architecture page as of 771ea93, and the TODO gists this week left behind (serialise restarts of
one input; refuse `job_retune` on owned jobs; the fit's measure through `applyOp`; `job_start`
tunes twice on an unheld radio; a `--max-streams` flag and per-client fairness; the page's four
watched jobs; an attached record's stale tuning; repeated Fit walking the width down; the detector
floor versus a real RS41; the lease keeping a lone job's capture centre off its channel).
Two specs were amended during execution and say so in dated notes: plan 15 §3 (a quarter-step
grid, not a half) and plan 18 §4 (an SSB fit moves the dial and keeps its band).

The next sprint's first candidates, from `TODO.md`: staircase transcript ribbons (seen again on the
sonde decode), the watch detector, plan 19, and the lease offset.

## Plan 15 status

Plan 15 (scan grid and mask) is on the `plan-15` branch. Merged to `main` 2026-11-01 (83726a2). Live acceptance done: the B210 for the mechanism and the HackRF (on the field host, 2026-11-01) for the constants.

Built, per task:

1. Task 1: `scan_steps` alternates the grid; odd sweeps shift every centre down by a quarter step (at most one more step). The spec's half step was replaced by the final review (2026-11-01): with rows about as wide as the step (the HackRF at 16 Msps), a half-step shift puts each grid's notch on the other grid's step boundary, inside both neighbours' edge masks, so about 300 kHz at every half-step (448, 456, 464, 472, 480 MHz in the 440-488 MHz survey) was never seen clean. At a quarter step each grid's notch and boundaries fall at the 25 %/75 % points of the other's rows; a unit test checks every bin of several spans is clean in one of two sweeps under the default mask (it needs `edge_mask` under 0.25).
2. Task 2: `fft` gains `notch_hz` and `edge_mask`; spectrum rows carry `masked` bin ranges.
3. Task 3: the scan carries its mask in its `fft` node (written from the capabilities on the first launch, honoured on every launch; `graph put` changes it); `job_context.scan` gains `alternate` and `mask`. A `graph put` that drops the scan's `fft` node is re-expanded with the default mask on the next launch rather than refused. A one-step scan (no alternation) has no edge mask in force (final review): no other sweep could fill its edges in, so `scan.mask.edge_mask` reads 0 while the stored node keeps the configured value.
4. Task 4: `detect` ignores bins inside a row's masked ranges.
5. Task 5: `measure` ignores the bins within `MEASURE_DC_MASK_HZ` of the capture centre on a local hardware receiver; `job_context.measure.dc_mask_hz`.
6. Task 6: the page paints a masked bin as nothing (the pixel keeps what the previous row left) and redraws replay the last two sweeps.
7. Task 7: field craft, README, this status, TODO.

Controller rulings:

- `measure`'s DC mask is skipped when the DC lies within one bin of the measured band's centre: a lone job's capture is centred on its band (the lease does no offset tuning), so the literal mask would hide the very signal the box measures; a box inside a wider capture (a pin, another job's capture) keeps the mask. The lease follow-on gist (`TODO.md`, "Lease: keep a lone job's capture centre off its channel") is the proper fix.
- `MEASURE_DC_MASK_HZ` (25 000 Hz) and ch20 under the UHF CB pin (corrected by the final review, 2026-11-01): ch20 (476 900 000 Hz) is 18 750 Hz from the pin centre 476 918 750 Hz, so a 12.5 kHz box around it (offsets 12.5-25 kHz below the DC) does not contain the DC, `measure_band_masked` applies no mask, and the spike's skirt is measured unmasked. The mask applies only to a fit box wide enough to contain the DC (about 37.5 kHz or more around ch20); then ±25 kHz covers -6.25..+43.75 kHz relative to ch20 and hides it. Any half-width of 13.75 kHz or more hides a ±5 kHz signal 18.75 kHz from the DC, while the HackRF's hump is 150-300 kHz wide, so no single constant serves both. The constant stays; the live acceptance measures the hump at the pin's rate and records it, and the lease-offset follow-on (`TODO.md`, "Lease: keep a lone job's capture centre off its channel") is the real fix for a channel 18.75 kHz from the pin centre.

Constants: `SCAN_NOTCH_HZ` 50 000 Hz and `SCAN_EDGE_MASK` 0.02 from the HackRF run below (300 000 Hz and 0.10 until 2026-11-01, chosen by eye), `MEASURE_DC_MASK_HZ` 25 000 Hz unchanged.

Live acceptance (desktop HackRF, `--source-soapy hackrf=driver=hackrf,gain=45`):

- [x] (1) The 440-488 MHz survey at 45 dB with a 2 s dwell look shows no hump at any step centre, no 100 kHz grid of peaks moving with the parity, and no band left unpainted after two sweeps (448, 456, 464, 472 and 480 MHz in particular, where the half-step grid left holes). HackRF on the field host, 2026-11-01 (below): no hump and no unpainted band; a grid of peaks does move with the parity, but it is the HackRF's own spurs at ±4 MHz, not the mask (a gist in `TODO.md`).
- [x] (2) With `notch_hz: 0` for one run, the hump's width at the floor at 2, 8 and 16 Msps read from the rows, `SCAN_NOTCH_HZ` set to the widest plus 20 %, the same for the edge roll-off and `SCAN_EDGE_MASK` (which must stay under 0.25 for the quarter-step grid), figures recorded here and in the TODO gist; the hump's width at the UHF CB pin's 1 Msps measured and recorded too. Done on the HackRF 2026-11-01 (below); the rule's literal result (14 kHz, 0.0) was widened to 50 kHz and 0.02 so the B210's figures and the HackRF's edge spur are covered too. `MEASURE_DC_MASK_HZ` is not tuned from it: a 12.5 kHz box on ch20 (18 750 Hz from the pin centre) holds no DC and is measured unmasked, with the spike's skirt in it; a fit box wide enough to hold the DC has ch20 masked at any half-width of 13.75 kHz or more. Record what a fit on ch20 under the pin reads; the lease-offset follow-on is the fix. Still pending on the HackRF (the constants are the HackRF's); the B210 figures below are a second data point, not the setting.
- [x] (3) `zeta job context` on the scan shows `scan.alternate` and `scan.mask`, `zeta stream tail` shows rows with `masked`. Proved on the field host's B210 (below).
- [ ] (4) The `detect` change is covered by its unit test only (no watch inside a scan step is started).

### Live run on the field host's B210, 2026-11-01

USRP B210 on the field host (`sdr` node, gain 65 dB, `ppm=2.7`), driven over `--node` and the page; mechanism only, the HackRF sets the constants.

- P15-3 PASS. `job start --kind scan --center-hz 464000000 --bandwidth-hz 48000000`: `scan` = `{steps: 3, step_hz: 16000000, dwell_ms: 250, alternate: {shift_hz: 4000000, steps_shifted: 4}, mask: {notch_hz: 300000, edge_mask: 0.1}}`. Rows are 4096 bins of 5 kHz (20.48 Msps), 4 rows a step, every row `masked: [[0, 409], [2018, 2079], [3687, 4096]]` (409 bins = 10 % at each edge; the notch 61 bins = 305 kHz, centre bin 2048 ±30). Row centres in order (MHz): 448, 464, 480 / 444, 460, 476, 492 / 448, 464, 480 / 444, 460, 476, 492 ... (even sweeps 464 ± 16, odd sweeps a quarter step (4 MHz) lower, one step more).
- P15-1 PASS (scan-mode path). With the manual scan on the shown source the page follows it in scan mode (`waterfallMode` `scan`), so the coarse layer is not engaged (`coarsePainted`/`coarseMasked` stay 0); the masked bins go through the same `fillColumn`. After two-plus sweeps (`painted` 54) the pixel-row profile shows no hump at any step centre (444/448/460/464/476/480 MHz read 70-73 against a median of 72) and no grid that moves between sweeps; the only unpainted band is the 492 MHz step's notch (outside the 440-488 MHz span, only the odd grid reaches it), drawn as background, not as a carrier. The bright lines at 464.458 and 480.47 MHz are real signals (seen in both grids at 10-16 dB). Screenshots `p15-coarse.png`, `p15-coarse-2.png`.
- P15-2 B210 data point. `graph put` with `notch_hz: 0`, `edge_mask: 0.0` gave `scan.mask {notch_hz: 0, edge_mask: 0.0}` and rows without `masked`. DC hump, mean of 60-120 rows relative to the row median: 20.48 Msps (scan step) peak 8.3 dB, over 6 dB 5 kHz (1 bin), over 3 dB 15 kHz, a 1-2 dB pedestal about ±100 kHz; 10.24 Msps (an 8 MHz `spectrum`) peak 6.1 dB, 5 kHz over 6 dB, 15 kHz over 3 dB; 2.048 Msps (2 MHz) peak 7.3 dB, 6 kHz over 6 dB, 12 kHz over 3 dB; 1.024 Msps (the UHF CB pin) peak 7.3 dB, 5 kHz over 6 dB, 9 kHz over 3 dB. Edge roll-off (bins more than 3 dB under mid-row, low/high end): 20.48 Msps 190/245 kHz (0.9/1.2 %), 10.24 Msps 100/140 kHz (1.0/1.4 %), 2.048 Msps 66/50 kHz (3.2/2.4 %), 1.024 Msps 30/28 kHz (2.9/2.7 %). The B210's hump is far narrower than the 300 kHz notch and the 10 % edge mask; the HackRF's constants stay as they are until its run.

### Live run on the field host's HackRF, 2026-11-01

HackRF One r9 (`66a062dc`, firmware 2.3.2) moved from the desktop to the field host and attached to the `sdr` node beside the B210 (`--source-soapy hackrf=driver=hackrf,gain=45`; the node's unit now carries both), an indoor low-gain log periodic on it (the floor at 100.5 MHz moved 2.5 dB for 25 dB of gain and no broadcast station showed: the antenna hears nothing, which does not matter to the receiver's own artefacts). Driven over `--node` from the `claude` account; rows read with `stream tail` and `scratch/live/hump.py`, raw rows and the notes in `scratch/live/` (local).

- P15-1 PASS with a finding. `scan` 464 MHz / 48 MHz: `scan` = `{steps: 3, step_hz: 16000000, dwell_ms: 250, alternate: {shift_hz: 4000000, steps_shifted: 4}, mask: {notch_hz: 300000, edge_mask: 0.1}}`, rows 4096 bins of 3906 Hz, every row `masked: [[0, 409], [2010, 2087], [3687, 4096]]`, centres 448/464/480 then 444/460/476/492. Over two sweeps every 1 kHz of 437.6-498.4 MHz has an unmasked bin (the only gap is the 492 MHz step's notch, outside the span); at the even grid's centres the odd rows show no hump (a 3-bin spike only, see the spurs). The finding: every odd-grid row (444, 460, 476, 492 MHz) carries a 3-bin spur at exactly +4 MHz (+23, +29, +37, +30 dB over the row median) and most at -4 MHz too; the even rows (448, 464, 480) have none there but a spur on their edge bins at ±8 MHz (+13..+23 dB); tuned to 462 or 470 MHz the spur sits at +2 MHz (+26..+30 dB) with +4 and +6 MHz weaker; 480 MHz carries a comb every 100 kHz to ±500 kHz (+8..+14 dB). The HackRF's spurs fall on multiples of 4 MHz with a level that depends on the tune, and the quarter-step shift at 16 Msps is 4 MHz: one grid's +4 MHz spur sits on the other grid's centre and is painted exactly where that grid masks its DC. Gist in `TODO.md` ("Scan: the HackRF's spur grid coincides with the quarter-step shift").
- P15-2 PASS. `graph put` with `notch_hz: 0`, `edge_mask: 0.0` on the scan, then `spectrum` jobs at 8, 4, 2 and 1 Msps (a band inside the last capture is granted without a retune, so the lower rates needed centres outside it: 470 MHz / 8 and 4 MHz, 482 / 2 MHz, 100.5 / 2 MHz, 476.91875 / 1 MHz). Mean of 40-80 rows against the median of the middle half of the row. DC: a needle at -34.5 dBFS at every rate and at gain 20 and 45 alike (+41..+44 dB over the floor), 3 bins wide over 6 dB and over 3 dB: 11.7 kHz at 16, 8 and 4 Msps (3906 Hz bins), 5.9 kHz at 2 Msps, 2.9 kHz at 1 Msps (the CB pin); nothing over 3 dB beyond ±50 kHz except the 480 MHz comb (113 kHz over 6 dB, 219 kHz over 3 dB). Edge roll-off: none at any rate (0 bins more than 3 dB under the median; the outer 5 % reads +0.2..+3.2 dB), the edge bins instead carrying the ±fs/2 spur (+4..+23 dB at 16 Msps, +21 dB at 4 Msps). Rates delivered: 1.0114 Msps (1.1 % fast, as on the desktop), 2.0116, 4.0101, 8.0043, 16.000. `stats.timeouts` climbs about 8 per second at 1 Msps (0 at 2 Msps and up, no overflows), and the 1 Msps pin gets no channel bank (`channel.path` `direct`, `no bank`). Constants: `SCAN_NOTCH_HZ` 50 000 Hz (four times the widest hump on either radio, the same ±25 kHz as `MEASURE_DC_MASK_HZ`), `SCAN_EDGE_MASK` 0.02 (the B210's 0.9-3.2 % roll-off plus the HackRF's edge spur; the rule's literal 0.0 would paint that spur). `MEASURE_DC_MASK_HZ` stays. A fit on ch20 under the pin was not possible (no CB traffic on this antenna); a 100 kHz box over the pin centre is under plan 18 below.

## Plan 18 status

Plan 18 (box and refine) is on the `plan-18` branch (tasks 1-10). Merged to `main` 2026-11-01 (771ea93). Live acceptance done 2026-11-01: the mechanism and the fit on a transmitting CB channel on the B210, the DC-in-box outcome on the HackRF; the fitted width on a weak voice channel is the one open finding.

What was built (spec `docs/superpowers/specs/2026-10-31-box-and-refine-design.md`):

- Task 1, the op, the lease move and the same-id relaunch: `job_retune` in the node (same id, new centre and bandwidth); the job's lease moves inside the current capture without retuning the radio, outside it when no other rigid job or pin holds it, else a `conflict` naming the holders with the old lease put back; the run is relaunched under its id without the record leaving `running`. Known: `job_start` on an unheld radio tunes twice (`tune_if_unheld`, then `ensure_as`), a pre-existing inefficiency noted in `TODO.md`.
- Task 2, carried params and the re-derived guard: squelch settings are carried across a retune, the channel-plan guard is derived again at the new band.
- Task 3, attached jobs: an attached `transcribe` follows its input through a retune or a `graph_put`, relaunched under its own id. Known: two restarts of one input are not serialised (two retunes in the post-gate tail, or a `graph_put` racing a retune), so in a window of tens of ms the attached job can end `stopped`/`failed` visibly; nothing leaks; a per-input restart lock closes it (`TODO.md`).
- Task 4, the CLI and MCP: `zeta job retune` and the guide note.
- Task 5, the fit first (page): a demodulator started from a box is measured first and fitted. The fit waits about 3 s (`stream_tail` of 3 measure frames); on the noiseless `tone` fixture the fitted width is 6.0 kHz (a 4781 Hz leakage skirt x 1.25), so the e2e pins a range.
- Task 6, draggable outlines: edges resize, the body moves, a ghost and readout, `job_retune` on release.
- Task 7, the outline menu: Fit to signal, the width presets, Stop.
- Task 8, the tags: job tags linked by hover with the job rows; rows show the band.
- Task 9, the e2e deadline raised to 330 s (measured 256.9 s).
- Task 10: field craft ("Fitting a demod to a signal"), README, this status, `TODO.md`, regenerated guides.
- Final fix wave (whole-branch review, 2026-11-01): an SSB fit moves the dial only and keeps its 6000 Hz band (spec section 4 amended); the field craft says what plan 15's measure DC mask hides from a box holding the capture centre, and the page's no-signal note names the capture centre when it was in the box on a local radio; a band shorter than three handles keeps its handles outside it; "Fit to signal" reads an `ssb_demod`'s sideband from its graph; the fitted/as-drawn notes follow a successful start only; the outline menu offers Fit and the presets on demodulators only; `job_retune` checks a program row's `min_hz` before the gate; a lease put back after a failed move is released when the run ended meanwhile.

Outcomes to record (live):

- The fitted centre's error against the channel centre, and the fitted width.
- Whether the DC spike ever wins a fit.
- The drag's feel at 12.5 kHz on a 1 MHz capture.
- Whether the DC mask hid a channel near the pin centre from a wide box (ch20/21/60/61 under the UHF CB pin), and whether the outline menu's Fit on a 12.5 kHz band fitted it.

Live acceptance (spec section 10), on the desktop HackRF with the 2 m / 70 cm antenna, UHF CB pinned at 476 918 750 / 1 000 000 Hz:

- [ ] Box a transmitting channel generously (about 100 kHz) and start "Transcribe › NBFM": the job lands within 500 Hz of the channel's centre with a width of 10-16 kHz, the audio is clean, the transcript arrives. The transcript pane shows the `fitted` line. Done 2026-11-01 on the B210 through the op path (below): centre +450 Hz off ch9 (pass), transcripts arrived (pass), the width fitted to the 5 kHz floor on a 9-12 dB signal (the 10-16 kHz expectation fails: a finding, not a mechanism fault; gist in `TODO.md`). The page's `fitted` line itself was proved on broadcast FM, below.

### HackRF on the field host, 2026-11-01

The HackRF pinned at 476 918 750 / 1 000 000 Hz (indoor low-gain log periodic; a 10 s max-hold over the pin showed nothing but the receiver's 477.000 MHz spur at +8 dB), driven over `--node`. A `measure` on a 100 kHz box containing the pin centre (476.95 MHz / 100 kHz): `job_context.measure.dc_mask_hz` 25000, the event `center_hz 476999902, bandwidth_hz 98, label carrier (7.5 dB)`: the DC needle (+42 dB) was not measured and the whole-MHz spur was, labelled a bare carrier. A 12.5 kHz `fm_demod` on ch20 ran unmasked (its box holds no DC) with the squelch closed (`floor_db` -67.5 dBFS, `guarded` false). The page's `no signal found ... (the capture centre ... not measured)` line for that box was proved on the B210 above; the HackRF run took the op path.

### The fit on a transmitting CB channel, B210 with the discone, 2026-11-01

The sonde watch and the RS41 decoder parked (the morning sonde was down), the B210 pinned at 476 918 750 / 1 000 000 Hz (lease 1 024 000) and driven over `--node` with the page's fit replayed as ops (`scratch/live/cb-fit2.py`, local): ten `measure` jobs on 100 kHz boxes tiling the pin clear of its centre, watched at once, the first box reporting a signal fitted as the page does (measured centre, 1.25 x the occupied width, floored at 5 kHz, capped at the box). A first attempt that started the `measure` only after a `spectrum` row showed a channel 12 dB up missed a sub-2 s opening on ch9 (the measure's first full second came after the PTT ended: `no signal found`), hence the tiling.

- Fit on ch9 (476.625 MHz), 11:49:06 AEST: the box 476.66 MHz / 100 kHz measured `center_hz 476625450, bandwidth_hz 600, snr_db 8.8, label fsk ("two rf tones 500 Hz apart")`; `fm_demod` started at 476 625 450 / 5 000 Hz: centre +450 Hz from the channel (inside the 500 Hz criterion), width 5.0 kHz (the floor; the criterion wanted 10-16 kHz). The squelch opened (-65.8 dBFS over a -75.6 floor), 15 audio frames in 8 s, `vad` speech events, and the attached `transcribe` returned "How big do you want?", "Oh no, no.", "I'll send someone down.", "That's right, I'm thinking.", "Thank you." (`transcripts tail`, `error` null, nothing dropped; 7 utterances under 700 ms counted `short`). Re-measuring the channel for 6 s while it was active read 600-800 Hz at 9-12 dB (`fsk`, `unknown`) and `noise` between words: at that SNR the 6 dB occupied band holds the carrier line only, never the voice sidebands, so the fit lands on the width floor. A `job retune` to 476 625 450 / 12 500 moved the demod under its id with the `transcribe` relaunched (its record's `tuning` still reads 5 000, the known stale copy, and `transcripts.log` carries that). Centre: pass. Transcript: pass. Width: the finding "Fit: a weak voice channel fits to the width floor" in `TODO.md`.
- [x] Drag the outline a channel over; the audio follows after the retune, the transcribe stays attached (same id in the job list), the ribbons keep scrolling. Mechanism proved on the B210 (same ids, transcribe relaunched on the new audio stream); audio and ribbons not heard (no traffic).
- [x] Drag its edge to 25 kHz and back to 12.5 kHz with the presets; `zeta ops tail` shows each `job_retune`. On the B210 the ops stream (`stream tail`, the state directory being `sdr`'s) showed each one.
- [x] Unpin, then drag a job outside the capture while another demod holds the radio: the conflict shows in the transcript, the outline snaps back, and the retry button moves it. Proved on the B210.
- [ ] With four demods running, each tag reads its kind, id, squelch dot and channel; hovering a row lights its tag. B210: hover linking both ways proved; the oldest demod's tag had no squelch dot with a `transcribe` attached (the page watches at most 4 jobs, the attached `transcribe` first), see below.
- [x] `zeta job retune` from the CLI against the serving node moves a job the page shows. Proved on the B210.

### Live run on the field host's B210, 2026-11-01

USRP B210 on the field host (`sdr` node, gain 65 dB, `ppm=2.7`), UHF CB pinned at 476 918 750 / 1 000 000 Hz (lease 1 024 000 Hz), driven over `--node` and a headless page. The node's 16 stream sockets were nearly all held by another open page (12-16 in use), so the test page subscribed to its spectrum stream only and the ops were read through `stream_tail` of the node's `ops` stream; with four demods running the other page took all 16 and the test page could not paint (the tag screenshot has an empty waterfall).

- P18-6 PASS. `zeta job retune --job <ch20 fm_demod> --center-hz 476925000 --bandwidth-hz 12500` answered the same id; `job list` showed the demod `running` at 476 925 000 / 12 500 within 1 s and the attached `transcribe` `running` under its id, `input` unchanged, its `stream_in` on the demod's new audio stream; the ops stream logged `job_retune` (`transport: http`, `ok`). The `transcribe` record's own `tuning` still reads the attach-time 476 900 000 (the jobs pane labels it ch20).
- P18-2 PASS. The 12.5 kHz outline on a 1.024 MHz view is 11.3 px tall: class `handles-out`, two `.outline-handle`s. A body drag of 22.7 px (25 kHz) showed the ghost and the readout `476.9500 MHz, 12.5 kHz · release to retune`; the release moved the job 476 925 000 → 476 950 000 exactly (a drag back returned it to 476 925 000, `job_retune` `transport: ui`), the `transcribe` relaunched under its id. Screenshots `p18-drag.png`, `p18-drag-after.png`.
- P18-3 PASS. The outline menu lists Fit to signal, 2.5, 5, 6.25, 12.5, 25 kHz, Stop (footer `job=… center_hz=476925000 bandwidth_hz=12500`); 25 kHz then 12.5 kHz set `bandwidth_hz` 25 000 then 12 500 about 476 925 000, each a `job_retune` (`ui`, `ok`) on the ops stream. At 25 kHz the outline is 22.7 px, still `handles-out`. Screenshot `p18-menu.png`.
- P18-5 PASS with a finding. Four `fm_demod`s (ch19 476 875 000, ch20 476 900 000, ch21 476 925 000, ch40 477 400 000) plus the attached `transcribe`: tags read `fm_demod V2YFS8 ● ch19`, `fm_demod Y6RWRP ● ch20`, `fm_demod RS0QBS ● ch40` (dot `closed`) and `fm_demod XVART4 ch21` without a dot: the page watches at most 4 jobs, the `transcribe` first, so the oldest demod is over the cap (its row shows `—`). Hovering a row set `hot` on its tag and outline; hovering a tag set `hot` on its row. Screenshots `p18-tags.png`, `p18-tags-rowhover.png`.
- P18-1 PASS. DC case: a 100 kHz box 476.870-476.970 MHz under the pin, "Decode › NBFM audio": footer `measuring…`, then `no signal found in 476.8700-476.9700 MHz (the capture centre 476.9187 MHz is in the box: the 50 kHz around it are not measured); started fm_demod at the box as drawn`, the job at 476 920 000 / 100 000. Positive fit, unpinned, capture 98 MHz / 20.48 Msps, loudest station 100.5 MHz: a 600 kHz box, "Decode › FM broadcast audio" gave `fitted wfm_demod to 100.5000 MHz, 163.3 kHz (wfm)`, the job at 100 500 000 / 163 281. The outline's Fit to signal on it then read `fitted wfm_demod to 100.5004 MHz, 150.6 kHz (wfm)` (100 500 375 / 150 625): measured inside its own band the occupied width reads narrower, so a repeated Fit walks the width down to the 150 kHz floor. Screenshots `p18-fit.png`, `p18-fit-outline.png`, `p18-fit-dc.png`.
- P18-4 PASS (page path). Pinned, A `fm_demod` ch20 and B ch40 started, unpinned. The CLI `job retune` of B to 433 000 000 was a `conflict` naming A only, with `retry with replace: [A]`. In the page, B's body dragged above the canvas (readout `477.4661 MHz, 12.5 kHz · release to retune`) and released: `job_retune` refused, the outline snapped back to 477 400 000, the transcript showed the conflict with `stop fm_demod 54QVES and retry`; the button stopped A and moved B to 477 466 060, the source's `current` following (477 466 060 / 120 000, centred on B's channel: the DC lands on a lone job's channel, the lease-offset follow-on). A CLI retune of the now sole holder to 433 000 000 moved B and the capture there. Screenshot `p18-conflict.png`, `p18-conflict-after.png`.

## Plan 19.1 status

Plan 19.1 (layer 4 explore: features, the register, burst captures) is built on `worktree-plan19-spec` (cdc3658..378b6c4 plus the docs commit), 2026-11-02; not merged at the time of writing. Spec `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`, plan `docs/superpowers/plans/2026-11-02-layer4-explore.md`; the per-task ledger is `scratch/plan19.1-ledger.md`. Live acceptance is deferred to after 19.3 by the operator.

What was built:

- Layer 1: burst gating (`duty`, `bursts`, `burst_ms`, `period_ms`; at least two on-runs or one of 3 ms, a train under 20 ms period is keying and measured whole), the cyclostationary symbol rate, FSK order and tone spacing, PSK order, `psk` and `ook` labels.
- The station (`station_set/get/clear`, antennas with discone and dipole defaults), `sink_sondehub` defaulting to it, the emission designator parser and the reach rule, an MCP client and the `acma` knowledge provider (`--knowledge-acma URL[,reach_km=N]`, `.mcp.json`).
- identify, classify and the watch consult the register (allocations beside features, the 0.2 score term, `register says ...` in the unknown summary, identify re-centring its trials); classify's row-width slice and range counting.
- The detector: occupancy over the row median, `energy_db`, a 4 kHz width floor, three sightings, 10 events a second.
- The `BurstStore`, the `capture` job kind, `capture_list/show/classify/delete` and `GET /captures/{file}`, the watch's `on_unknown: capture`, `--bursts-keep-mb/-days`.
- The page: a pane tab strip with Subjects moved into it, the Explore pane and "Capture bursts", the cached-region bracket, a `too_old` classify re-issued on the latest span, `source_list.ring`.
- Field craft: "Exploring an unknown signal" (the ladder through 19.1, the legal line once), the register-first rule on a licensed band, the low-`duty` rule; the guide's stale "run measure yourself" advice for the watch now says identify re-centres.

Rulings: candidates publish in score order at each row's turn; `once per run` resets on a restart; under `view set subjects auto` the tab appears but opens on a click; `--knowledge-acma` takes the base URL (a trailing `/mcp` accepted); band class edges are lower-inclusive; a capture's age for expiry counts from write time.

`ENERGY_DB_DEFAULT` is 30 and `THRESHOLD_DB_DEFAULT` 6 over the row median, tuned on synthesised inputs only (the 6 dB RS41 clears both). The B210 noise recording is PENDING THE OPERATOR (`scratch/live/record-noise.sh`, then the ignored test `crates/zeta-dsp/tests/detect_noise.rs`): the defaults may need one more adjustment against real receiver noise.

Gate (2026-11-02): fmt per member, `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean; `cargo test --workspace --all-features --no-fail-fast`: 142 `test result` lines, 1459 passed, 0 failed, 4 ignored, wall time 1066 s (17 min 46 s), no `ui e2e skipped`. The one failure in that run was the new guide test's own needle (`legal line`, absent from the text); the text was fixed and `agents_md` and `transports` re-run green.

Final review fix wave (2026-11-03, after the whole-branch review): only the state dir's owner expires the burst store at open (`serve`, or a one-shot with its own `--bursts-keep-*` and no serve), so a read-only one-shot never deletes what serve keeps; with `--node` the environment's `ZETA_KNOWLEDGE_ACMA` and `ZETA_BURSTS_KEEP_*` are ignored and only the explicit flags refused; the bursts index is written compact with file names relative to the store; the Explore pane polls only while shown, once shown, while a capture job runs or a bracket is drawn, auto-selects only for a capture new to the load, skips brackets older than the columns (`xForNs` a binary search); `capture_list` is trimmed for the agent (newest 20, no paths or allocations, `count`); the station is the page's home while `view home` is none (`station_get` once per load); burst gating needs the off chunks within 6 dB of the in-band floor (`BURST_OFF_NEAR_FLOOR_DB`), so clicks over a continuous signal no longer gate it; a capture on a signal already on switches to back-to-back windows after 500 ms (`whole_auto`); a SondeHub sink with no position and no station is refused at append (decoder kept, `sink_error`) and idles rather than failing a hand graph; the ACMA overlap uses `COALESCE(d.BANDWIDTH, 0)`. Gate after the wave: fmt per member, clippy clean; 142 `test result` lines, 1470 passed, 0 failed, 4 ignored, no `ui e2e skipped`.

The in-band floor fix (2026-11-03, 1b57cf7, after the wave's re-review): the burst gate's noise floor was the 15th-percentile bin of the whole channel spectrum, most of it the channel filter's stopband, so on channelised IQ the gate never fired and a capture on a quiet channel switched to `whole` on plain noise (filtered noise read 98.6 dB over its "floor"); the floor is now taken over the in-band bins only (0.5 dB), `gated_view` takes the band, the capture stage passes its `bandwidth_hz`, and `Gate::reset()` redoes the warm-up check and drops an automatic `whole`. Final gate at 1b57cf7: 142 `test result` lines, 1474 passed, 0 failed, 4 ignored, no `ui e2e skipped`, 1008 s. Parked for 19.2 (ledger `scratch/plan19.1-ledger.md`): gating and the continuous check need about 15 % of the band's bins at the noise floor; a failed first Explore poll waits for a show; the map does not recentre on the station home; a one-shot that drops or rebuilds records rewrites `index.json` under `serve`; relative index names are not readable by an older binary (downgrade note). Merged to `main` (fast-forward) and deployed on the field host as `~/zetasdr/plan19-zeta` on the operator's order, 2026-11-03.

Deferred minors are gists in `TODO.md` ("Layer 1 and explore: deferred minors from plan 19.1", "Detect: a per-device spur list", "Station: a GPS fix from the B210").

## Plan 19.2 status

Plan 19.2 (layer 4 bits) was built on `worktree-plan19-2` (8ff4829..0b90359) and fast-forwarded to `main` 2026-11-04; deployed on test_host with plan26-zeta. The OOK run-length fit (layer 1 measuring on-off keying's chip from its run lengths, 49ceb21..1ef6c69) followed the first on-air remote. Spec `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`, plan `docs/superpowers/plans/2026-11-03-layer4-bits.md`. Live acceptance (spec 9 item 3) waits for test_host, after 19.3.

What shipped: stages `capture_in`, `resample`, `fsk_demod`, `psk_demod`, `ook_demod`, `clock_recover`, `slicer`, `correlate`, `bit_view` in `zeta-dsp`; the ops `graph_start` and `bits_start` (`zeta graph start --file`, `zeta bits start ... --then-tail`); the Bits pane and "Work the bits" on the Explore pane; the field craft's "Working layer 4" as the executable playbook (AGENTS.md and the skill regenerated); the agent may stop its own `bits_start` and `graph_start` jobs; `graph` and `bits` are reserved kind names.

What the gate proved: the chain BER tests (fsk 2 and 4 levels, psk, ook, through `resample` and `clock_recover`), `bit_view`'s hypotheses on a framed stream (planted sync, period and CRC; every CRC against a known vector), `graph_start` refusing an unfed input, `bits_start` from features and refusing without a rate or on mixed sets, `capture_in` on an expired capture, a leaseless job draining at its end, and the Bits tab e2e scenario on a synthesised FSK capture.

What waits for the field host: the real 404.025 MHz captures through `bits_start` and the Bits tab (`UAT.md` "Plan 19.2"). Decided during the plan: `bits_start` fills `modulation`, `symbol_rate_baud` and `order` from the feature record and refuses rather than guesses; 4-FSK uses the early-late clock; PSK's phase ambiguity is left to `slicer` `differential`; the PSK chain rate is the smallest multiple of 4 x baud covering a 2000 Hz offset.

Deferred minors are gists in `TODO.md` ("Bits: deferred minors from plan 19.2", "Feature: layer 4, working an unknown signal").

## Plan 26 status

Plan 26 (themes as folders) is merged to `main` (576002a, the merge of main into the themes branch, op count 36) and deployed on test_host 2026-11-04 as `~/zetasdr/plan26-zeta`. Spec `docs/superpowers/specs/2026-11-03-themes-design.md`, plan `docs/superpowers/plans/2026-11-03-themes.md`, guide `docs/themes.md`.

What was built: a theme is a folder (`theme.json`, an optional `style.css`, the fonts and images it names), built in under `themes/` or the operator's `--themes-dir`, listed by `themes_list` (`zeta themes list`) as ready or broken; the node wraps `style.css` (tokens and layout first, scoped by nesting, font faces and keyframes hoisted, imports and escaping urls refused) and serves a ready theme's files under `/ui/themes/<id>/` without the bearer; console's look moved into `themes/console`; four new built-ins: cyberpunk, terminal, psytrance and vibecoder (the light one). UAT (`UAT.md` Plan 26) is open; the deferred minors, per-footprint layouts and a node-level default theme are gists in `TODO.md`.

## Plan 24 status

Plan 24 (the console theme) is merged to `main` at 4098fd2 and deployed on the field host 2026-11-03 19:47 as `~/zetasdr/plan24-zeta`. Web UI only; nothing in the operations changed. Spec `docs/superpowers/specs/2026-11-03-console-theme-design.md`. It shipped the plan 18 console theme gist and the staircase ribbons' step-up rule; the lane budget and the shared keyed lanes stayed in `TODO.md`.

What was built: `view set theme console|plain` (mirrored on `<html data-theme>`, `plain` is the old look) and `view set side both|transcript|jobs|none`; the frame and the source tab bar's live health; the rail (tags beside the axis, thin segments where they crowd, a 14 px strip on a phone with tap-to-expand); one colour per job across outline, tag, row and ribbons; ribbons riding their band and tilted; two-line job rows; the idle-channel fold; the graph modal's stage-family bars; the bundled Antonio font (SIL OFL). Deferred minors are one gist in `TODO.md` ("UI: deferred minors from plan 24"); acceptance is `UAT.md` "Plan 24".

## Plan 23 status

Plan 23 is built on `worktree-subjects` (tasks 1-6, a track-thinning change, a final fix wave and three fixes found by its reviews, 6ab145e..3894778), 2026-11-02; merged to `main` the same day (812cf1b, fast-forward) and deployed on the field host as `~/zetasdr/plan21-zeta` (the name is kept across plans 21-23), the running jobs intact. Final gate: 1353 passed, 0 failed, the ui e2e at about 400 s of its 440 s deadline. Spec `docs/superpowers/specs/2026-11-02-subjects-design.md`, plan `docs/superpowers/plans/2026-11-02-subjects.md`.

What was built:

- Task 1, rows: `subject` on a mode row (`key`, `columns`, `waterfall`, `heading`, `label`, units, `track_points`, `track_min_m`) and `position.alt_unit`; ADS-B gains a position; the ADS-B, sonde, APRS, ACARS and RDS rows their subjects; POCSAG has none.
- Task 2, `subjects.js` and the Subjects tab: one merged state per (job, key), a row per subject in metres, m/s (signed, one decimal) and km/h (`kt` only), dim after `subject_dim_s` (60 s) and drop after `subject_drop_s` (300 s), sort by column, distance and bearing from `view set home`, the jobs the page does not watch listed last.
- Task 3, the waterfall: a job with a table is one line (`adsb 12 aircraft, last QLK335D`), a `fold` row one ribbon per subject updated in place; `protocolText`'s ADS-B fallback removed.
- Task 4, the map: a marker per subject turned to its heading, a label, a track, a click on a table row selects it on the map and the reverse.
- Task 5, the agent: a run may `job_stop` the jobs it started itself (`OwnJobs`); the guide says plan ids come from `channel_plans`.
- Track thinning (d3d19f1): a track is not "the last 100 fixes". A row sets `track_points` (the cap) and `track_min_m` (adsb 100 / 500 m, aprs 100 / 50 m, sondes 500 / 100 m); a fix joins when the subject has moved that far (3D) or turned over 10 degrees, and at the cap the older half is thinned by every other point, keeping the first point and the newest half at full detail.
- Task 6: `TODO.md` tidied (the removals and their evidence are in the commit message), the gists "Subjects: state does not survive a refresh" and "Subjects: leftovers" (with the sharp-curve gist for track thinning), `UAT.md` "Plan 23".

Rulings: `m_s` displays as m/s, never km/h (the plan text said km/h); the spec's section 4.5 and goals were amended for the tracks (dated note in the spec).

Acceptance (the operator's, the field host): open; `UAT.md` "Plan 23" is the checklist. Nothing was deployed.

Found by the whole-branch review and fixed on the branch (bfea35c, 935b8f1): a plain `identify`'s trials no longer flap the Subjects pane; a message costs no layout work (ribbons and the map coalesce to 2 Hz, the pane shows and hides on polls only); the default sort buckets last-seen into 10 s steps and holds still under the pointer, with selection on pointerdown; the table has padding and scrolls sideways with its key held. Two bugs outside the plan, both present in the deployed fd126b0: a worker crash shorter than a job's 200 ms fault poll left the job `Running` with no samples (7e5bfc8, 8cb5b1a: a healed fault stays reported until recovery has seen the jobs end, bounded at 10 s and logged); and the mix page read its `active` seed before its socket opened, so a change in between left a talking channel dark (3894778).

## Plan 22 status

Plan 22 is merged to `main` at fd126b0 (six tasks, each reviewed, a final fix wave), gate 1322 passed, 0 failed, and deployed on the field host 2026-11-01 as `~/zetasdr/plan21-zeta` (the binary's name was kept; it holds plan 22). Spec `docs/superpowers/specs/2026-11-01-audio-mix-design.md`, plan `docs/superpowers/plans/2026-11-01-audio-mix.md`; the plan's closing section lists the rulings.

Live so far: the operator reports it works. Three mixes of 2-3 `wfm_demod` inputs on the B210 showed 0 underruns and 0 ms dropped, `bank.dropped` 0, per-channel transcripts arriving and mutes applied. Open: all 80 CB channels, two radios through one mix, and a phone; `UAT.md` is the checklist. `PREBUFFER_MS` stays 40 unless the 80-channel run's `underruns` says otherwise (80 at most).

Changed along the way: a realtime recording paces by its clock with a bounded catch-up (a stall re-anchors rather than flooding), and the page cuts its audio queue past 1 s rather than dropping one frame at a time. Known limits: stopping a mix forgets its mutes; `graph_get` then `graph_put` is not atomic against another client (`TODO.md`).

## Plan 21 status

Plan 21 is merged to `main` at 09080b0 (nine tasks, a final fix wave), gate 1270 passed, 0 failed; `jobs_stop` followed at 817cd60 after the first evaluation showed "stop all jobs" spending 32 turns on 31 stops. Deployed on the field host 2026-11-01. Spec `docs/superpowers/specs/2026-11-01-agent-bulk-ops-design.md`, plan `docs/superpowers/plans/2026-11-01-agent-bulk-ops.md`.

Live so far: gemma-4-12b called `plan_start` in one call on each radio (80 channels with transcribers), and `jobs_stop` stopped 92 jobs in one call. Not run live, covered by tests: a clash reporting its holders, a repeat request answering `existing`. `UAT.md` is the checklist. Found in use: the model invented a plan `broadcast_fm`, refused as `unknown` (`TODO.md`).

## Plan 20 status

Plan 20 is merged to `main` at 3acbad4 (tasks 1-8, a final fix wave and one follow-up, 17 commits fast-forwarded from the `plan-20` branch), 2026-10-31. Deployed on the field host the same day (`~/zetasdr/plan20-zeta`, built from `~/zetasdr/src20`); live acceptance items 1 and 2 passed on the B210 2026-10-31, item 4 (the HackRF, cable pull and SIGSEGV) on the field host 2026-11-01; item 3, the B210's own cable pull, still awaits the operator.

What was built (spec `docs/superpowers/specs/2026-10-31-soapy-worker-design.md`):

- Tasks 1-4, Worker: the `zeta soapy-worker --id ID --args ARGS` child (same executable) owns the Soapy driver and speaks a framed wire format to the node; `WorkerPort` is the node's port behind it. The worker exits 3 when the device is absent, 4 when the open fails, 5 on a source fault (`no samples for 1000 ms (device unplugged?)`, `read failed: ...`, `overflow storm ...`, `soapy retune timed out`); a crash is a signal.
- Task 5, Supervision: the node respawns the worker (backoff 1 s doubling to 5 s, reset after 60 s alive; a device-not-found exit polls every 5 s; hello timeout 30 s). The fault shows as `fault` in `source list` (`soapy worker for <id> died: signal 11 (SIGSEGV)`, `device not found: <args>`, ...), jobs on it fail with `source <id>: <fault>`, and `source list` gains `worker: {pid, restarts, since_ns} | null`. Startup with the device absent attaches faulted when `sources/<id>.json` holds capabilities from an earlier open, else refuses the flag.
- Task 6, Healing: on the hello that heals, recovery runs under the source's gate (gain, pin, then every job that failed with `source <id>: ` in the episode, scan first and widest first, attached `transcribe` jobs after their inputs, the `watch` re-adopting its decoders), one stderr line `zeta: source <id>: recovered: ...`. Recoverable records are never pruned while the episode lasts.
- Task 7: `--soapy-in-process` (global; ignored with `--node`), the worker's stderr prefixed `zeta-soapy-worker[<id>]: `.
- Task 8: field craft ("A radio that comes back" replaces "A radio in its own process"), README, this status, `TODO.md`, regenerated guides; `scratch/serve-hackrf-child.sh` retired (the two-node launcher is replaced by one node with `--source-soapy hackrf=driver=hackrf,gain=32`; two nodes stay documented as federation).

Deployment: `source list` gained `worker`, and a node deserialises a remote node's source list with unknown fields refused. A plan-20 node attaches an older node's source (a missing `worker` reads as `null`), but an older node cannot attach a plan-20 node's source until it is upgraded: upgrade both nodes of a federation together.

Final fix wave (whole-branch review): the worker turns SIGTERM and SIGINT into `shutdown` and exits within 5 s of the first signal even with its driver hung in `open()`; a worker of another protocol is refused (`soapy worker for <id>: protocol N, expected 1`) and respawns exec `/proc/self/exe`, so a binary replaced under a running node is never paired with it; every node exit shuts the workers down (`shutdown`, 2 s grace, kill) instead of SIGKILLing them with the runtime; the worker's environment loses every `ZETA_*` variable naming a `SECRET`, `PASSWORD` or `KEY`.

Outcomes to record (live):

- Time from `kill -SEGV` to the fault in `source list`, and from the fault to the worker's hello (a UHD reopen is expected within 30 s).
- Whether `stages_running` returned to its pre-kill value.
- Whether `journalctl` shows exactly one "waiting" and one "back" line per episode.

Acceptance on the field host (spec section 6), for the live run (`RUSTFLAGS="--cfg soapy"`, `~/zetasdr/plan20-zeta`, the unit's `ExecStart` pointed at it):

- [x] 1. Restart the unit (2026-10-31 17:56 AEST): `source list` shows `b210` with `worker.pid`, gain 65 dB applied, the ADS-B job and the page's spectrum resumed on it (no pin was set at the time). `modes check --strict rs41_decode` not re-run (the unit stayed up); the page loads.
- [x] 2. Run with the ADS-B job and the page's spectrum job instead of the watch (the operator had re-tasked the radio), 2026-10-31 17:58 AEST: within 1.5 s `source list` showed `fault: soapy worker for b210 died: signal 11 (SIGSEGV)`, both jobs `failed` with `source b210: soapy worker for b210 died: signal 11 (SIGSEGV)`, `stages_running` 5 → 0; the worker was back after 4.2 s (`restarts` 1), the gain re-applied, both jobs `running` under their ids, `stages_running` 5 again; `journalctl` showed exactly one `waiting for the device`, one `back after 1 attempts, 4.2 s` and one `recovered: gain 65 dB, 2 jobs relaunched` line. Gotcha: the FIRST `kill -SEGV` is swallowed by the Rust runtime's stack-overflow SIGSEGV handler (it resets the disposition and returns); the second one kills. Use `kill -ABRT` for a one-shot simulation.
- [ ] 3. With the operator at the machine: pull the B210's cable, wait 10 s, replug. The same outcome through the device-lost path (`no samples for 1000 ms (device unplugged?)`, exit 5, polling, heal), and `journalctl` shows exactly one "waiting" and one "back" line.
- [x] 4. The HackRF form, done on the field host instead of the desktop (2026-11-01, 11:12-11:26 AEST): the `sdr` node's unit gained `--source-soapy hackrf=driver=hackrf,gain=45` beside the B210, so one node runs two radios in two workers (the two-node launchers on the desktop are retired). With the HackRF pinned at 476 918 750 / 1 000 000 Hz and `fm_demod`s on ch20 and ch40: `kill -SEGV` (twice, 1 s apart) on the worker at 11:23:24.97 gave `fault: soapy worker for hackrf died: signal 11 (SIGSEGV)` at 11:23:27 and the worker back at 11:23:28 (`back after 1 attempts, 1.0 s`), `recovered: pin 476918750/1000000 Hz, gain 45 dB, 4 jobs relaunched` once, both demods `running` under their ids, `worker.restarts` 1. The operator then pulled the cable and power-cycled the unit: `no samples for 1000 ms (device unplugged?); waiting for the device` at 11:25:43, `device not found: driver=hackrf,gain=45` from the worker every 5 s (three times), `back after 4 attempts, 16.1 s` at 11:25:59 and `recovered: pin 476918750/1000000 Hz, gain 45 dB, 5 jobs relaunched` once; the demods `running` under their ids, the CB floor the same as before the pull (-77.3 against -77.0 dB), `worker.restarts` 5 (one for the SIGSEGV plus one per `device not found` attempt: `restarts` counts respawns, not episodes, so the e14 note's "restarts is 1" after a pull is wrong by the polling attempts). The B210 kept its pin, its watch and its RS41 decoder throughout.

## Plan 17 status

Plan 17 is merged to `main` at 97383b0 (tasks 1-11 and a final fix wave, commits 37bf273..97383b0 from the plan-17 branch), 2026-10-29. Live acceptance (spec section 12) is pending.

What was built (spec `docs/superpowers/specs/2026-10-28-sonde-frameworks-design.md`):

- Task 1, Modes: audio program rows (`decoder.demod`, `input.container`), `band.range_hz`, `position` and `time` row fields, summary number formats (`{lat?:.4}`, `{vel_v?:+.1}`), a `priority` tie-break in `score_rows`.
- Task 2, Adaptor: audio-input rows fed under a WAV header (`channel -> nbfm_demod -> external_decode`); the `rs41_decode`, `dfm_decode` and `m20_decode` rows over the rs1729 decoders (package `sonde-decoders`).
- Task 3, Synth: generic `fsk`, `psk` and `ook` families and the `echo` impairment; the aprs and pocsag rows carry a 100 us / -6 dB echo in their tests.
- Task 4, Synth: an RS41 frame generator (from public protocol descriptions, `rs41mod` as the oracle only); `rs41_decode` passes `modes check` against the real `rs41mod`.
- Task 5, Sources: `free_sources` on the lease manager and the `source_free` op (`zeta source free`), a suggestion not a reservation.
- Task 6, DSP: the `detect` stage, `signal` events (`new`/`gone`) over spectrum rows.
- Task 7, Watch: the `watch` job kind (16 built-in kinds): holds its band, `fft -> detect`, identifies new signals one at a time, keeps the proved decoder (`decode_on: capture|free`, `prefer`), `identifying|decoding|unknown|lost` events, `job_context.watch`, owned identify jobs hidden and kept out of the 20-record history; `job_stop` or a failure tears down what it started, `graph_put` and a restart re-adopt; `graph_put` refuses bad `detect` params. Review fix: a drop guard and the Failed-exit teardown.
- Task 8, UI: watch jobs outlined with their signal markers, owned jobs hidden, "Watch this band".
- Task 9, UI: a map layer for events with a position (`view set map`, `view set home`; vendored Leaflet 1.9.4, OSM tiles when online), distance and bearing from home, a narrow-renderer map tab; the page's CSP allows the tile host only (ruling 10). Review fix: the e2e `fake_pos` row narrowed so no sweep tries it.
- Task 10, Sink: `sink_sondehub` (PUT batches to SondeHub v2 telemetry, `job_context.sondehub`), appended by the watch's `sondehub` param or put on any sonde job; a cancellable gated sink restart, `sondehub` validated at `job_start`/`graph_put`/resume naming `sondehub.<field>`, a failed restart falls back to the sink-less decoder or publishes `lost`.
- Task 11: field craft ("Watching a band for sondes", the audio-row shape), README, this status, `TODO.md`, regenerated guides.
- Final fix wave (whole-branch review): `dfm_decode` and `m20_decode` carry only flags their programs take (`--json --ecc` and `--json`; `--crc` made both exit `error: open`), checked on this host by `sonde_programs_accept_their_rows_args`; the map follows positioned jobs on every source (`MAX_POSITIONED` 4, map only); the watch's fft targets 1.5 kHz bins (`WATCH_BIN_TARGET_HZ`, 4096 bins at 6 MHz); a decoder whose signal went during identify still times out; `sink_sondehub` skips events SondeHub rejects; `prefer` refuses unknown sources; guide fixes.

Outcomes the plan asked to record:

- Echo variant: both embedded rows keep a 100 us / -6 dB echo and pass through the real chain (a 1000 us / 0 dB control failed). The plan's 10 us rounds to 0 samples at 48 kHz (an undelayed copy), so `validate` now requires at least one sample of delay (ruling 7).
- RS41 generator: the frame generator was ok against the real `rs41mod` on the first build (1 bit is the higher tone; inverted polarity decodes nothing); RS parameters and CRC byte order were confirmed against published example frames in scratch only. `zeta modes check --strict rs41_decode` exits 0 on this host. `dfm_decode` and `m20_decode` have no test (`skipped: no test`).
- `--strict` contract fix (ruling 5): `--strict` fails any row without a test, as AGENTS.md, the spec and the plan state; the code now matches, so a whole-tree `--strict` run fails on `dfm_decode` and `m20_decode` until they have a test (correct, and not a pass criterion).
- Known behaviour (ruling 6): the sonde rows score 0.6 on any narrow FM box and take `classify` `tried` slots as `skipped_quick` outside 400-406 MHz; gist in `TODO.md` (what `CLASSIFY_TRIES` counts). `--then-tail` on a program row with a squelched demod tails the demod's squelch events first; the sonde rows run squelch off (gist in `TODO.md`).

Gates at the branch head (Task 11):

- `cargo test --workspace --all-features --no-fail-fast`: 1101 passed, 0 failed, 3 ignored (121 `test result` lines), no `FAILED`, no `ui e2e skipped`; the UI suite (`ui_scenarios_pass_in_headless_chromium`) ok in 232.52 s. Baseline before the plan was 1023 passed.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` are clean; `zeta agents-md --check` and `--skill --check` pass.
- `zeta modes check` prints `ok` for `acars_decode`, `adsb_decode`, `aprs_decode`, `pocsag_decode`, `rds_decode` and `rs41_decode` (against the real `rs41mod`), and `skipped: no test` for `dfm_decode` and `m20_decode`.
- rs41 offset margin: `modes check` passes the row's test at `offset_hz` ±1500, ±2000, ±2500, ±3000, ±4000, ±6000 and ±8000 and at `ppm` 10, but it tunes the job on the shifted signal, so that is the signal's place in the capture, not a mis-tune. With the job held on the capture's centre and the signal off it (one-shot `rs41_decode` over synthesised 20-frame captures), `rs41mod` decodes to ±1000 Hz and nothing at ±1500 or ±3000 Hz: rs41 offset margin ok to ±1000 Hz, under the watch's ~1.46 kHz bins, so field craft says the watch may need `identify` to re-centre (gist in `TODO.md`).

Live acceptance (spec section 12), pending: on the home station with the discones back, during a Brisbane launch window, a `watch` on 400-406 MHz on the HackRF reports the sonde as a `signal` within `persist_ms` of its first frame, identifies it as `rs41_decode` within one dwell, and `job_context.watch.decoding` names a running `rs41_decode` job whose events carry `id`, `position` and a `summary`; the page's map shows the marker moving and the ribbon scrolling; `view set home` gives distance and bearing; with the field host's B210 attached as a `--source-node` and `decode_on: free`, the decoder lands on the B210 and the HackRF keeps watching; stopping the watch stops the decoder; `zeta modes check --strict rs41_decode` exits 0 (checked on this host in the gate); `zeta source free` lists the idle radio; nothing uploads until a `sink_sondehub` node with a callsign is on the job.

Deferred (ledger summary):

- Rows and adaptor: `envelope` overwrites a decoder's own `position` field; `position.*` schema types are not checked for number; `demod.deviation_hz`/`audio_rate_hz` are not range-checked at load (they fail at job start); a malformed `{a:x}` summary suffix breaks the row at validation while `fill` reads it as a field name; audio rows with `$center_hz` silently get 0; `Converter::write` lost its preallocation (hot path at 2.4 Msps); `external.rs` is about 990 lines (split `convert.rs`); `fake_lines.py`'s second-header detection matches any RIFF run.
- Synthesiser and check: no tests for repeat/gap, the pattern 1 s length, QPSK, zero baud/repeat, rate boundaries or the echo tail; the psk symmetry assertion pins nothing; three `synthesise` arms duplicate the burst/repeat/shift sequence; `synth.rs` is about 450 lines longer (generic families could move to a submodule); the STATUS byte 0x16 is uncommented; `rs41_iq` has seven positional arguments; the RS tables are rebuilt per frame; the `--strict` assertion rides in `a_row_without_a_test_is_skipped` and the no-test case borrows `dfm_decode`.
- Sources and detect: `free_sources` reads free/busy under two lock reads and `fault()` under the lease mutex; `bandwidth_hz` 0 is not rejected; the detect stage test writes a ~73 MB ci8 SigMF (about 290 MB in memory); no `edge_mask` unit test; the tracker does not reset on a retune (scan rows would report `gone`) and does not flush `gone` at stream end; greedy matching; `snr_db` rounded in f64 while the summary formats the raw f32; `SpectrumRowView.floor_db` is parsed but unused.
- Watch and sink: the verdicts map can leak an entry for an operator-stopped or stale identify; `start_on` tunes then launches in two steps; re-adoption waits serially 5 s per dead record; `start_identify` duplicates `identify`'s `start_child` cancellation block; `watch.rs` is about 1060 lines; under `decode_on: free` a decoder started by `start_on` is unguarded until `place_decoder` returns; a `graph_put` relaunch that fails after the stop leaves the old run's decoders unowned, as does a hand-edited record with bad detect params at resume; `operator_graph` freezes sink-bearing decoders, decoders re-adopted after a watch `graph_put` keep their old sink config, a batch buffered at a restart is lost (gist in `TODO.md`); `store_graph` clears `gated_stop` unconditionally and the replace-during-sink-restart test can flake (gist in `TODO.md`); a missing required `sondehub` field is reported `sondehub: ...` not `sondehub.<field>`; a pre-fix persisted watch with a bad `sondehub` fails at resume.
- UI: owned jobs are still subscribed for ribbons; `!j.owner` is duplicated in `agent.js` and `overlays.js`; poll-added markers use a built title rather than the event summary; the watch scenarios assert a marker exists, not its position; stale frozen markers for positioned jobs after a source switch; a throwing `onEvent` listener aborts the overlay render; the popup age freezes while open; the narrow `#map-toggle` tab has no scenario; the 2 s `FAKE_AFTER_BYTES` margin is thin on a loaded host; positioned jobs on other sources are looked up by kind (`modes_list` once, `modes_show` per kind; `modes_list` carries no `position`).
- Watch, two radios: the watch's `held_elsewhere` reads holders on its own source only (spec 6.2 says any running job); documented as a limitation in field craft.

First live run, 2026-10-29 10:45-11:20 on the field host's B210 (plan-17 binary under the `sdr`
deployment, discone, 10:00 Brisbane launch): the node restarted on the new binary with every handler
found; the watch on 403.025/5.95 MHz fired on receiver noise at the default detect params (about a
hundred 6-9 kHz "signals"; `TODO.md` "the watch's detect defaults fire on receiver noise") and held one
real but wrong signal at 404.02 MHz with `threshold_db` 18, `persist_ms` 5000, `width_hz` [8000,
30000]; identify on it ended `unknown` (ACARS was tried before the sonde rows). SondeHub placed the
sonde (RS41-SGE X1142834) on 401.4999 MHz, 15 km up and descending 120 km east at 10:52: a `measure`
there read 8-13 dB SNR fading to noise by 11:15 and `rs41_decode` on the published frequency produced
no frame before it faded. The on-air decode proof therefore still waits for the 22:00 launch, which
ascends towards the station.

Second live run, 2026-11-01 10:00-10:50 on the field host's B210 (`plan18-zeta`, main 9d736ab, discone,
10:00 Brisbane launch): the on-air decode proof is in. RS41-SGE X1142896 on 401.4999 MHz was decoded
by the `rs41_decode` pre-placed on the last known frequency (401.500250 Hz, 375 Hz off), frames with
`id`, `position` and the row's `summary` from about 10:12 AEST (21.8 km up, rising 5 m/s, about 70 km
east over the water) and still arriving at 10:50; the ribbons for one job staircase down the
waterfall (`TODO.md` "staircase transcript ribbons"). The watch half is not proved: with the
field-tested detector (`threshold_db` 18, `persist_ms` 5000, `width_hz` [8000, 30000]) the sonde
read 4.5 kHz wide at 19 dB and was never a `signal`; at `width_hz` [4000, 30000] it was, but so were
a dozen 4.5 kHz spikes at round offsets from the capture centre (402.0245, 402.1505, 402.9005,
403.1494, 403.8996 MHz: B210 spurs) and the watch churned identify runs on them, so the floor went
back to 8 kHz. Two 15-18 kHz bursty links at 404.025 (Unitywater) and 404.299 MHz identified
`unknown` three times each, correctly. The sonde's channel SNR was about 6 dB where `measure`
said 20 (a per-bin peak); B210 gain 65 → 76 dB raised signal and floor alike. What remains is the
detector (a tone-pair or run-energy criterion and a per-device spur list, `TODO.md` "the watch's
detect defaults fire on receiver noise") and the re-centre before identify.

## Plan 16 status

Plan 16 is merged to `main` at 91886bc (tasks 1-9, a final fix wave and a flake fix, 18 commits
from 8e3d1bf), 2026-10-28. Live acceptance on hardware (the HackRF laptop or the field host with the
B210 and dump1090) is pending.

Operator rulings on the review's calls (2026-10-28), carried as a gist in `TODO.md`:

- The fifo directory (`<state_dir>/run`, ruling 14) may also live in the system temp directory
  (`/tmp`), `/usr/local/var/run` or `/opt/ZetaSDR/var/run` once zeta is packaged; the state
  directory was the conservative choice, not a requirement.
- The install hint (`apt` → `dnf` → `brew`, ruling 15) may extend to snap, flatpak and AppImage
  where a handler ships that way.

What was built (spec `docs/superpowers/specs/2026-10-27-mode-registry-design.md`):

- Mode rows: `modes/*.json` embedded, operator rows in `--modes-dir`, states `ready|broken|unavailable`,
  `modes_list`/`modes_show`, `zeta modes list|show|check`; a broken row is logged and never fatal.
- The four in-crate decoders expand from their rows, pinned to today's graphs; `adsb_decode` is a
  program row (`channel` then `external_decode`); `JobKind::Mode(id)` takes any operator row.
- `external_decode`: a row's program under the cleared environment and a process group, parsers
  `json_lines|sbs|nmea|kiss|text|multimon`, the startup handler report, `ZETA_HANDLER_<PROGRAM>`.
- Events carry `protocol`, `summary`, `t_ns`; the page renders `summary`.
- `zeta modes check [ID] [--strict]` with a synthesiser (`aprs|pocsag|rds|acars|modes_ident|nbfm_tone`,
  impairments `snr_db|offset_hz|ppm`) and `checked` in `modes-checked.json`.
- `classify` returns `classification { labels, tried }` (top 5 rows by score, six outcomes);
  `knowledge_lookup` signatures name a mode id; the `identify` job tries every fitting ready row
  as a child job in score order, with `candidate` events and a verdict in `job_context.identify`.
- A `video` payload (`rgb8|gray8|png`) under plugin protocol 2 and the documented bits event shape.
- Task 9: field craft ("Decoding modes"), README, this status, `TODO.md`, and a carried fix:
  `tests/identify.rs` now has `required-features = ["test-fixtures"]` so `cargo test -p zeta-core`
  compiles without features; `every_job_kind_expands_to_a_valid_graph` (zeta bin) treats `identify`
  like `scan` (no graph), which the first gate run caught failing.

Gates at the branch head (task 9), then on the final tree and on the merged `main`:

- Final tree 91886bc on a clean target, and `main` after the fast-forward merge: `cargo test
  --workspace --all-features` 1023 passed, 0 failed, 3 ignored (118 `test result` lines), UI e2e ok
  (213.91 s on the branch), exit 0.

- `cargo test --workspace --all-features --no-fail-fast`: 1016 passed, 0 failed, 3 ignored (118
  `test result` lines). The one failure of the first run, `every_job_kind_expands_to_a_valid_graph`,
  was the test fix above and passes (the zeta bin's 11 tests re-run ok); no flake fired. No
  `ui e2e skipped`; the UI suite (`ui_scenarios_pass_in_headless_chromium`) ran ok in 248.78 s.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  the driver's clippy with `RUSTFLAGS="--cfg soapy"`, `cargo test -p zeta-core --no-run` (no
  features), `zeta agents-md --check` and `--skill --check` are clean; `zeta modes check` prints
  `ok` for the four in-crate rows and `adsb_decode` (dump1090 present on this host).

Live acceptance, 2026-10-28, on the desktop with the HackRF One (release build from `main`
126765e, operator rows in `scratch/modes`): `zeta modes check --strict` passes the five built-in rows
and a copied operator row, and fails the deliberately broken row; the node starts with that row,
logs `zeta: mode pocsag_broken (<file>): row: invalid type ...` and runs; `modes list` shows it
`broken` with its file and reason, the six others `ready`; the handler report names dump1090 found
(dump1090-fa 8.2); `job start --kind bogus_mode` answers `unknown` with `field: kind` and the known
list; `identify` on a UHF CB channel ends `unknown` (label `noise`, 4 attempts, 28 s) with no child
left and no holder; `adsb_decode` runs dump1090 on the node's feed. Not provable that day: the roof
discones were disconnected for a storm and the indoor log periodic delivered no decodable signal on
any band (broadcast FM, 2 m, UHF CB, 1090 MHz: only the DC spike and the spur comb, confirmed with
`hackrf_sweep`), so the POCSAG `--then-tail` with `summary`, the APRS `classify` proof, the POCSAG
`identify` verdict and an ADS-B frame remain pending for a day with the antennas back (the synthesised
`modes check` passes stand in for them). Notes: a broken row's `reason` names `row`, not the field
(the deferred serde-name minor); `checked` is per state directory, so a check run elsewhere does not
show on the serving node.

Deferred (ledger summary):

- Rows and registry: duplicate ids in the operator dir (last file wins, a broken entry can be
  overwritten); an unreadable operator dir is skipped silently; `parse` reports serde's bare field
  name and unknown enum variants fall back to `row`; operator ids equal to a non-decoder built-in
  (`fm_demod`, `scan`) load ready but are unreachable from `job_start`; `Mode("pocsag_decode")` built
  programmatically does not round-trip; `template::expand` ignores `input.stream`; the
  `nullable` helpers sit in `modes`, not `types`; no CLI test of `--modes-dir` with `--node`.
- Events and adaptor: the RDS `ps` trailing space doubles in the summary; a leading empty optional
  keeps a stray separator; `envelope` leaves a non-object `fields` un-enveloped; the adsb summary
  has no ` ft`; `external.unparsed` can lag until the run ends; an override naming a
  non-executable file is reported `found`; the KISS parser always sets `fcs_ok`; a mid-stream rate
  change resets the resampler silently; `external.rs` is 855 lines (split `convert.rs`/`proc.rs`).
- Check and synthesiser: `within_ms` is clamped to 10 000 (0 to 1); subset comparison is top-level
  value equality; a relative fixture path resolves against the cwd; startup lines reprint per row;
  RDS has no margin at 10 000 ms; the echo impairment is deferred to plan 17.
- Classify: `fits_band` is computed against the measured width but the sweep uses `Band::fits`;
  the catalogue lift applies on a `noise` layer 1; the whole-block cap drops RDS on 100-150 kHz
  blocks the old code tried; no test of the `unavailable` outcome or of the `tried` notes; a box
  between 12.5 and 150 kHz misses an off-centre packet (ruling 21; gist in `TODO.md`).
- Identify: core tests run without the bank (a plain `pocsag_decode` never decodes on the
  debug-build bank at 2.048 MHz, in `TODO.md`); stale-child cleanup after a killed process only
  runs after a successful measure; `launch_identify` repeats `launch_scan`'s scaffolding; identify
  scores with a zero catalogue term so its order can differ from classify's; a `source_ensure` with
  `replace: [identify]` racing a queued child launch can retune the source back (`source_ensure`
  takes no lease).
- Plugin protocol: `KindNotCarried` says "protocol 1" for a sink refused under 2; the 16 MiB
  `MAX_BODY` caps video quietly (a 4K `rgb8` frame is refused as too long, undocumented); the
  Python SDK stays on protocol 1.
- Plan 17 inputs (in `TODO.md`): generic `fsk`/`psk`/`ook` synthesiser families, the echo
  impairment, audio-input program rows.

## Sprint 2026-11-06

Opened with the operator on 2026-11-06: "a larger sprint", starting with video. The operator's
framing for the whole sprint: the node is a framework for radios yet to come (an FPGA front end
with its own channel bank among them), not a solution around one device; a question about one
radio is answered as a framework piece. Each row is one plan; specs come from a brainstorming
session before the plan, as before.

| Order | Plan | Why here | Depends on |
|---|---|---|---|
| 28.1 | The source transport, made rate-proof | A worker child hands IQ to the node through a shared-memory ring (worker protocol 2: descriptors on the pipe, `cf32` slots, drops counted), each Soapy source probes its rate ladder at open and reports a `measured` cap the lease applies (`insufficient bandwidth` names it), the B210 sweep before and after. Spec `docs/superpowers/specs/2026-11-06-video-decode-design.md` sections 3, 5.6, 5.7; plan `docs/superpowers/plans/2026-11-06-source-transport.md`. **Merged** to `main` (97d958a) and deployed on the field host as `plan28.1-main-zeta`, 2026-11-07 (see "Plan 28.1 status"). | 20 |
| 28.2 | Analog video, decoded and watched live | `video_decode` (wide FM, composite sync and line lock, luma `gray8` fields; the 6.5 MHz audio subcarrier if it falls out), the websocket's video body with `smooth`, `sharp` and `thumb` profiles, the thumbnail riding the band and the full view with feeds switchable, `zeta stream save`, fixtures of the operator's four transmitters from the B210. Spec as above (the rest of it); plan `docs/superpowers/plans/2026-11-08-video-decode.md` (written 2026-11-08, after 28.1 fixed the default rung at 30.72 Msps). UAT deferred: the operator sets up each transmitter. | 28.1 |

Queued behind them, specs first, in the operator's order of interest: colour (chroma), recording
(stills and clips into the capture store), discovery (a video carrier found by the watch), the
host-capabilities baseline (`TODO.md` "Node: a baseline of the host's capabilities"), VTX channel
tables from the open VTX firmware (`TODO.md` "Plans: VTX channel tables"), `sc16` on the worker
wire.

## Plan 28.1 status

What was built: the Soapy worker hands IQ to the node through a shared-memory ring (a memfd of `cf32`
slots of 2 MiB, 64 MiB per source by default, created per spawn and inherited as fd 3), and the pipe
carries worker protocol 2, control JSON plus 40-byte block descriptors, so control never queues behind
samples; a worker without a ring falls back to blocks on the pipe, and a protocol-1 child is still
accepted. `source_list[].worker` shows `transport` and `transport_dropped`. At open the worker probes
the device's rates from the top (at most 6 rungs, a 1.5 s settle and a 1.5 s dwell each, a rung rejected for overflows tried once more) and reports
the first clean one (at most one overflow in the dwell, at least 98 % delivered) as
`max_bandwidth_hz: {measured: N}`; with no rung clean it keeps the driver's asserted cap and logs the
`max_rate_hz=` to pin. `max_rate_hz=N` asserts a cap with no probe; a measured cap is cached in
`sources/<id>.json` and passed to every respawn, and a respawn's measured cap replaces an asserted one.
A request wider than a measured cap is refused `out_of_range` with `why: insufficient bandwidth: ...`.
The hello timeout is 60 s. The sweep script is `scripts/rate-sweep.sh`.

The ring holds `slots` blocks, not `slots × 2 MiB` of samples: each block takes a slot whatever its
size, and a B210 block is 10 ms (two slots at 30.72 Msps, as a block of 307 200 samples is wider than
one), so the 64 MiB default (31 slots) buffers about 155 ms there; packing small blocks into one slot is
a later gist.

### Sweep before

Run 2026-11-07 15:15-15:20 AEST on the field host's B210 through the deployed pre-change node
(`events-log-zeta`, main 6043e56), `bash scripts/rate-sweep.sh deploy/field-host/z.sh b210 1090000000 …`,
one minute per rate, the radio free of rigid jobs (the operator stopped the ADS-B decoder for it; the
page's elastic spectrum job followed the capture). CPU was not captured (the sampler read lifetime
averages; the after-sweep uses `top -b`).

| rate | delivered | overflows/min | worker CPU | node CPU | fault |
|---|---|---|---|---|---|
| 15.36 M | 15.31 M | 71 | – | – | none |
| 20.48 M | 20.39 M | 206 | – | – | none |
| 30.72 M | 30.71 M | 11 | – | – | none |
| 61.44 M | 51.48 M | 1702 | – | – | none |

The device holds 30.72 Msps almost cleanly through the f32 pipe; 61.44 Msps is the rung that falls
over (16 % short, overflows climbing, no fault within the minute). The "16-24 Msps reliable ceiling"
of the earlier note was pessimistic. The overflow counts at 15.36 and 20.48 are higher than at 30.72;
unexplained, to be compared with the after-sweep.

### Sweep after

Trial deploy 2026-11-07 16:04 AEST as `plan28.1-zeta` from the branch. At open the B210 probed 61.44
(an overflow storm), 30.72 (drops and overflows in the seconds after the master-clock change) and took
20.48 Msps as its measured cap; the HackRF took 20 Msps on its first rung. Both radios run on the ring
(`transport.path: shm`, no drops). CPU read with `top -p` at 20.48 Msps with only the page's spectrum
job.

| rate | delivered | overflows/min | worker CPU | node CPU | fault |
|---|---|---|---|---|---|
| 15.36 M | 15.36 M | 0 | – | – | none |
| 20.48 M | 20.48 M | 4 | 23 % | 10 % | none |
| 30.72 M | refused: `insufficient bandwidth: b210 delivers at most 20480000 Hz (measured at open)` | | | | |

The ring removes most of the overflows at 15.36 and 20.48 (71 and 206 a minute before). But the probe
under-capped the B210 at first: 30.72 Msps delivered cleanly on the old pipe once streaming, so the probe's 200 ms
settle was too short after a UHD master-clock change. Fixed on the branch: the settle is 1.5 s after every
tune, a rung rejected for overflows (not for a short delivered rate) gets one more settle and dwell, and
the hello timeout is 60 s (a cold FPGA load of 2-4 s plus up to 6 rungs of two 3 s attempts, about 40 s).
The retry re-issues the tune, which is a no-op on the running stream, so it is a second settle and a
fresh dwell, and each attempt's delivered rate is judged on the samples of its own dwell. Redeployed as
`plan28.1b-zeta` at 16:26 with the B210's `capabilities` key removed (node stopped): the B210 probed
`rate cap 30720000 Hz measured (2 rungs tried)`, and a minute at 30.72 Msps delivered 30.72 M with 0
overflows (11 a minute on the old pipe).

A `kill -ABRT` of the B210's worker healed in 4.4 s with `--rate-cap-hz 20480000 --ring-fd 3` on the
respawn and no second probe.

## Plan 14 status

Plan 14 is delivered on `worktree-plan-14` (tasks 1-5), 2026-10-27, merged to `main` the same day.

Live acceptance, 2026-10-28, on the desktop with the HackRF One (short form; the 2 h clock run and
the transcript timing check need a day with the roof antennas back):

1. Clock: pins cycled 1, 2, 3, 6, 12, 16 and 20 MHz, 12 s each: `stats.measured_rate_hz` within
   0.6 % from 2 MHz up (+1.1 % at 1 MHz, below the HackRF's 2 Msps spec), `restarts` 0, `overflows`
   0, `bank.dropped` 0, and `iq read` of the last 5 s succeeded at every pin.
2. Leak: `stages_running` 4 with two elastic spectrum jobs, 23 with eight more spectrum jobs and a
   `measure` (channel, measure, forwarder), back to 4 after the stops; no `lagged` line.
3. `measure` on a 12.5 kHz channel of a 12 MHz pin: first event after 1.0 s, `partial: false`; under
   the eight-job spike eight events in 9 s, none partial, `missed` 0 (the release build keeps up, so
   `measure.partial` had nothing to count; `stats.timeouts` rose to 93 with no overflow).
4. Device loss, two-node form (child `zeta serve` on 7374 owning the HackRF under a restart loop,
   main on 7373 with `--source-node ... max_bandwidth_hz=3000000`, a 3 MHz pin and an `fm_demod`):
   on the cable pull the main stayed up, its source faulted `remote node http://127.0.0.1:7374:
   stream closed: stream ended`, the job failed with that reason and `stages_running` fell to 0.
   The child also stayed up (the in-process hardening works): its fault read `no samples for 1000 ms
   (device unplugged?)` and a tune answered `device lost ...; restart the node`. Because it did not
   exit, the launcher loop did not restart it; after a manual kill the loop restarted it and a re-pin
   plus a new `fm_demod` from the main node recovered everything. Finding (in `TODO.md`): a node
   whose only device is lost should exit, or reopen the device when it reappears, so the launcher
   pattern recovers unattended. Plan 20 keeps its place: the node no longer dies in-process.

What was built:

- Task 1: the Soapy clock guard: the reader restarts its stream when stamps run 2 s ahead of the
  wall clock (`MAX_AHEAD_NS`), logs `zeta: soapy <id>: samples ran <n> ms ahead of the clock;
  stream restarted`, waits out the lead, and faults with `soapy: samples arrive faster than <rate>
  Hz (<n> stream restarts in 60s)` after more than 5 restarts in 60 s; a rate change reopens the
  stream; `source_list` items carry `stats` (`PortStats`: `measured_rate_hz`, `overflows`,
  `timeouts`, `restarts`; `null` without stream statistics).
- Task 2: a run generation on job entries so a stopped run never outlives its job and every stop
  waits for its run to release; `GET /health` carries `stages_running`.
- Task 3: `measure` emits `partial: true` events for a window cut short by a gap, lag or retune
  (at least 250 ms held, `MIN_PARTIAL_S`); `job_context` carries `measure: {discarded, partial,
  last}`; `source_list.bank.dropped` counts input blocks the bank dropped.
- Task 4: a lost device (fatal read, 1 s without samples, a device call failing during a retune,
  100 consecutive overflows with no samples) ends the reader with no further native call on the
  dead handle; the source stays faulted and `tune`/`source gain` answer `device lost: <fault>;
  restart the node`.
- Task 5: field craft ("Reading a source's health", "A radio in its own process"), README, the
  four `TODO.md` bug sections closed, this status.

Decision made (ruling R4): a device-call failure during a retune, a driver rejecting a rate
included, is treated as device loss, because it is indistinguishable from one and the native call
it avoids is the crash. Zeta picks rates from the device's advertised ranges, so a rejection is
rare; the cost if wrong is a node restart. Field craft says a source faulted during a retune is
held until the node restarts.

Gates at the branch head:

- `cargo test --workspace --all-features --no-fail-fast`: 923 passed, 1 failed, 3 ignored (114
  `test result` lines). The failure was `fake_program_round_trip` (`zeta-ext --test adsb`, event timeout); it passed alone on every rerun. Across 7 reruns of the whole `adsb` file, 6 passed all 11 tests and 1 failed `child_exit_fails_the_stage` on its 30 s bound (the test plan 13 recorded as a flake). No `ui e2e skipped`. The UI suite
  (`ui_scenarios_pass_in_headless_chromium`) ran ok in 238.71 s. The first gate attempt died on a
  full disk (the shared target dir holds 86 GB); its incremental cache was deleted and the run
  repeated with `CARGO_INCREMENTAL=0`.
- `cargo fmt -p` for each of the 17 workspace members, `cargo fmt` on the driver,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the driver's clippy with
  `RUSTFLAGS="--cfg soapy"`, `zeta agents-md --check` and `--skill --check` are clean.

Deferred:

- Task 1: the faulting trip counts as a restart (`restarts` reads 6 for 5 reopens); `restart_times`
  is not cleared when a tune clears the fault; the unit gate exempts the names restarts/timeouts;
  the `too_many_restarts` test takes about 12 s; a comment gap on activate failure after open.
- Task 2: a refused `graph_put` has already persisted its graph (in `TODO.md`); the `done.is_none()`
  skip can leave a mid-launch elastic job on a stale fit until the next retune; `stages_running`
  counts forwarders and scan loops and `debug_stage_count` backs a production field (rename);
  the soak test is sequential and was never shown failing; about 230 lines of helpers are copied
  into `runs.rs`; a stop during an
  elastic relaunch returns while the relaunch task still holds the lease (a `graph_put` in that
  window is refused `Stopped`).
- Task 3: a rate 0 window of 1024 or more samples can reach `measure_band` when cut; the gap test
  does not assert span >= 0.25 s; the `measure_bank.rs` module doc is stale; `futures_script`'s
  test module is oddly named.
- Task 4: `interleaved_overflows_never_fault` takes about 6 s; no RED run captured;
  `source.rs` is about 1010 lines; the race test does not assert `calls_after_gone()` empty.
  (The final-review fix wave closed two more: a failed clock-guard reopen now loses the device,
  and `tune`/`set_gain` re-check `lost` after the reader is joined, so a loss racing a queued
  command never reaches `open_stream`.) Trade-off accepted in that wave: `apply` now closes the
  stream before reconfiguring (no device call under an active stream, for UHD), so a USB pull in
  the second before `QUIET_READS` fires can still close the stream on a dead handle; the same
  exposure exists in the guard's reopen and the stop path's deactivate.
- Plan 13's leftovers stay open.

## Plan 3 status

Plan 3 is delivered on `transports-agent`: `zeta serve --listen` (authenticated HTTP and
websocket, `zeta-ws`), `zeta --node`/`--secret` (`zeta-client`), `zeta-mcp`, `zeta ask` with the
`zeta-agent` tool loop, and `AGENTS.md` plus `skills/zetasdr` generated by `zeta agents-md`. The
workspace gates passed on the branch (fmt, clippy, 186 tests passed, 1 ignored); after merging plan 2 they
pass with 261 passed, 1 ignored (264 passed with `--all-features`), and the by-hand slice ran against a live
`serve --listen` with a scripted fake model. The live-model test (`cargo test -p zeta-agent
--test live -- --ignored`) and a live `zeta ask` against `field-host:8080` (gemma-4-12b) both
failed on 2026-10-18 with the endpoint reachable but no chat completion within the 300 s attempt
bound. On 2026-10-19 the controller checked that 17.6 GiB of 24 GiB VRAM was free on the host, and a
direct 5-token `curl` POST to `/v1/chat/completions` returned no bytes within 20 s while
`/v1/models` answered and listed `gemma-4-12b`. Chat completions hang on the model server; the
cause is unknown and server-side. The live result is therefore unconfirmed and needs a rerun.
Deferred to later plans: browser websocket authentication and the operation log stream (plan 6),
binary audio and IQ over the websocket (plans 5 and 6), federation authentication (plan 5).
On 2026-10-19 in the evening the field host's chat completions answered again: the live test
`live_llm_tunes_a_source` passed (1.2 s) and `zeta ask` against the B210 node answered correctly.

## Plan 4 status

Plan 4 is delivered on `hardware-sources`: rate-aware narrowband jobs (the narrowest exact channel
rate), explicit classifier bands, capability records with `remote` and `push`, a KiwiSDR source
(`zeta-source-kiwi`, `--source-kiwi`), a SoapySDR port (`zeta-source-soapy`, tested against a mock)
with its real driver in the workspace-excluded `zeta-source-soapy-driver` behind
`RUSTFLAGS="--cfg soapy"` (a cfg, not a feature, so no default or `--all-features` build compiles
`soapysdr`; `cargo tree -p zeta -e normal --all-features | grep -c soapysdr` prints 0), and
external-process stages (`zeta-plugin`, plugin protocol 1, `--plugin`) with a standard-library
Python SDK and `examples/python/gain.py`. The workspace gates passed on 2026-10-19: fmt, clippy
with `--all-targets --all-features -D warnings`, `cargo test --workspace` 393 passed, 2 ignored,
and with `--all-features` 396 passed, 2 ignored. By hand against the LAN Kiwi
(`kiwisdr.local:8073`, v1.837, 3 of 8 users before and after, so the slot was released):
`source list` showed `remote` true and `current` null; a 10 kHz `measure` job at 7.1 MHz produced
two records with finite `snr_db` (5.1 and 5.4 dB, labelled `noise`, reception is poor) and
`center_hz` 7 104 582 and 7 103 305; 20 kHz was refused `out_of_range` on `bandwidth_hz`, max
12000; the ignored live test passed (24 blocks in 1.29 s, RSSI -71 dBm). By hand, `gain.py` ran
inside a job of a `serve --listen` node after `graph put`: `stream list` showed the job's two iq
streams, each tailed three 4800-sample frames, and after `job stop` and stopping the server no
`gain.py` process remained. Later on 2026-10-19, with `libsoapysdr-dev` installed, `RUSTFLAGS="--cfg soapy" cargo build -p zeta`
and `cargo clippy -p zeta --all-targets -- -D warnings` passed against SoapySDR 0.8.1 (HackRF
module 0.3.4); `zeta soapy find` printed `[]` with no device attached and a `--source-soapy`
open with no device exited 1 with a clear error. With a HackRF One (r9, firmware v2.3.2) attached later that day, `zeta soapy find` listed it,
`source list` showed its capabilities (0-7.25 GHz, 20 MHz, ci8/ci16/cf32), and a `measure` job at
100.1 MHz with a 200 kHz band produced two events a second apart (no antenna: snr 2.8 dB, label
noise). A 1 MHz band in the debug build lagged continuously and produced no event in 8 s; the
release build is the one to use for wide bands on real hardware. With an antenna, the release
build measured a 1 MHz band at 1 Msps without lag, `measure` at 104.9 MHz showed a broadcast
station (11-20 dB snr, occupied bandwidth 28-98 kHz between runs) and `fm_demod` at 104.9 MHz
with 75 kHz deviation streamed audio frames. Layer-1 labels broadcast FM `unknown` by design: the
plan-2 `fm` rule is narrowband (band at most 25 kHz, envelope cv below 0.2), and the occupied-
bandwidth estimate varies with the FFT bin width. A wideband-FM rule and a steadier bandwidth
estimate are layer-1 follow-ups.

## Plan 5 status

Plan 5 is delivered on `federation`: VITA 49.2 signal-data and context packets (`zeta-vrt`, `ci16`
on the wire, FNV-1a stream ids, a context packet about once a second), a binary mode of the stream
websocket (`?format=binary`: VRT packets for IQ, the typed envelope for audio, text and events,
paced by `{"type":"credit","packets":N}` with skipped frames reported as `lagged`), a binary
subscription with credits in `zeta-client`, another node as a push source (`zeta-source-node`,
`--source-node ID=SOURCE@URL`, secret from `ZETA_NODE_SECRET_<ID>`, 250 kHz default cap widened by
`max_bandwidth_hz=`), VITA 49.2 over UDP as a push source (`zeta-source-vrt`, `--source-vrt`), and
a Pi 5 cross-build, qemu smoke test and systemd deployment (`deploy/pi/`). Discovery is deferred:
v1 nodes talk by URL. The workspace gates passed on 2026-10-19: fmt, clippy with `--all-targets
--all-features -D warnings`, `cargo test --workspace --all-features` 456 passed, 0 failed, 2
ignored (the live Kiwi and the field host's model tests), `no_privileged_deps`, `zeta agents-md
--check` and `--skill --check`, and `deploy/pi/smoke.sh` (the aarch64 release binary under
qemu-aarch64). By hand on this box, node A served the tone fixture on 127.0.0.1:7979 and node B
federated it with `--source-node a=tone@http://127.0.0.1:7979` under `serve --listen
127.0.0.1:7980`: B's `source list` showed `a` with `remote` and `push` true and `max_bandwidth_hz`
48000 (the remote file's own limit, below the 250 kHz cap); a 12.5 kHz `measure` job on `a` at 100
MHz produced `carrier` records each second (snr 136.6 dB, as for the local tone), and A's `source
list` showed `tone` held by a `passthrough` job. `job stop` on B did not release it: A still showed
the holder 38 s later, because the port stays tuned until the next tune or exit; Ctrl-C on B
stopped the remote job before the next poll (under 10 ms), and the one-shot form (`job start ...
--then-tail 2,5000` with `--source-node`) returned two events and stopped its remote jobs. Each
federated job start created two `passthrough` jobs on A, the first stopped at once (fixed in the
fix wave: an identical re-tune reuses the remote job). On 2026-10-19 a second node on
field-host (x86, USRP B210 through the SoapySDR UHD module, built with `--cfg soapy`) served
`--source-soapy b210=driver=uhd,serial=31B92DD`; this box federated it with `--source-node
falc=b210@…` through an SSH tunnel because port 7979 is firewalled on that host. The federated
`source list` showed the B210 as a remote push source capped at 250 kHz; under a local `serve`, a
`measure` job at 104.9 MHz with a 200 kHz band produced events each second (snr 16-17 dB) and
`fm_demod` produced audio frames; the remote node showed the passthrough job as the lease holder. A
local `job stop` did not release it then (only a retune, the port's close or its drop did); since
the final-review fix wave the remote passthrough is released when the last local job on the source
stops, after a 2 s grace, and an identical re-tune reuses it, so a federated job start makes one
remote job. Open: the one-shot form (`job start … --then-tail`) streamed for 10 s but produced no
events in two runs, while the same job under `serve` did; it is recorded for the final review. A
federating `serve` stopped with SIGTERM now shuts down like Ctrl-C and stops its remote jobs (fix
wave). The HackRF step of the by-hand plan did not run (the HackRF was unplugged).

Later on 2026-10-19, with port 7979 opened on the field host's firewall, the two-host test ran directly over the
LAN with the merged `main`: one remote passthrough per federated job (the idempotent tune), no packet loss
or lag reported by the new counters, and `measure`, `spectrum` and `fm_demod` over the B210 all produced
output in both one-shot and `serve` form. The one-shot no-events finding turned out to be a cold-start
effect: in the first seconds after the remote node opens the B210, `measure` completes fewer or no 1 s
windows (one event instead of two in the first run after a restart, none in the earlier runs that also went
through an SSH tunnel with a debug build), and it is gone once the device has been streaming for a minute.
A settle period after a device starts streaming, or a more tolerant measure window, is the follow-up.

High-rate run over gigabit Ethernet (this box wired, the field host wired, release binaries both ends,
`max_bandwidth_hz=` raised explicitly): `spectrum` over the federated B210 at 1 Msps (610 frames in
10 s), 4 Msps (1000 frames in 3.3 s) and 16 Msps (10 000 frames over 10 s of signal through a local
`serve`, about 64 MB/s on the wire) all arrived with `missed` 0, no lag or loss counted, and every frame's
span accounted for. A request for 10 MHz with a 10 MHz cap was refused with `suggested` 16 000 000, because
the B210 ships 16 Msps for that request: the shipped-rate cap check from the fix wave working as intended.
Costs at 16 Msps: the local node ran at about 43 % of one core (VRT decode plus the spectrum FFT), the
remote at about 9 %, and both held about 2.1 GB resident, almost all of it the 10 s IQ ring. That ring
size is a follow-up for the Pi: `deploy/pi/zetasdr.service` caps memory at 1 GB, so a Pi node must keep a
shorter ring or refuse caps that would not fit.

## Plan 6 status

Plan 6 is delivered on `ui`: the node's `ops` event stream (one event per applied operation, from
any transport), browser websocket authentication by the `zeta.bearer.<base64url(secret)>`
subprotocol, `POST /ask` (`serve --llm-url/--llm-model`, the `zeta ask` agent run against the node
itself) and `POST /stt` (`--stt-socket`, the `local-voice` daemon), and the web app (`zeta-ui`,
plain JavaScript embedded in the binary and served without auth under `/ui/`): login with the
node secret, source tabs, a right-to-left waterfall fed by the UI's own `spectrum` job,
channel-pinned layer labels and decoded-text ribbons, the transcript fed by `ops` (successful
list polls hidden unless typed, errors always shown), the job list with live counters, the
six-entry context menu on a drag, audio playback of `fm_demod` jobs over the binary websocket, the
editable graph view, the microphone, and a narrow footprint below 700 px with the side pane in a
drawer and answers as ribbons. The workspace gates passed on 2026-10-19: fmt, clippy with
`--all-targets --all-features -D warnings`, `cargo test --workspace --all-features` 500 passed, 0
failed, 2 ignored (the live Kiwi and the field host's model tests), `no_privileged_deps`, `zeta agents-md
--check` and `--skill --check`, `deploy/pi/smoke.sh` (PASS, on the aarch64 binary built for plan 5;
it was not rebuilt with the UI), and `cargo test -p zeta --test ui`: all 23 Playwright scenarios
passed in headless Chromium (62 s, no skip line). The first test run filled the disk (22 GB of
incremental cache under `target/debug`); deleting `target/debug/incremental` and running with
`CARGO_INCREMENTAL=0` got it through.

By hand on this box, headless Chromium against `serve --listen 127.0.0.1:7979` with the `tone` and
`aprs` fixtures and `--llm-url http://field-host:8080 --llm-model gemma-4-12b`: both tabs
showed with their tuning ranges; the waterfall painted on each; a drag on the tone opened the menu
with the six entries and the footer `center_hz=100000000 bandwidth_hz=9600 start_ns=… end_ns=…`
(clamped to the last 8 s when the span was longer); "What is this?" logged `classify selection=… →
ok (1 label)` and drew `L1 carrier 0.70` at 100 MHz (snr 114 dB); Decode › NBFM audio started an
`fm_demod` job whose ▶ played 30 frames in 3 s with none dropped; Decode › APRS on `aprs` drew the
`VK3ABC > APRS: >hi` ribbon 2 s later, repeating with a ×N count as the fixture loops; the graph
view of that job showed `channel → nbfm_demod → aprs_decode` with `iq` and `audio` edges and the
channel's params as JSON; the mic, with no `voiced` for this user (no `--stt-socket`), showed "no
speech daemon on this node" (the 503 from `/stt`); at 390×844 the page took the narrow footprint,
the waterfall filled the screen with the ribbon on it, and ☰ opened the transcript and jobs
drawer. The agent question "what sources do you have", typed on the command line, got no answer in
120 s; a direct `POST /ask` under `timeout 120` also returned nothing, and a 5-token `curl` to
the field host's `/v1/chat/completions` returned no bytes in 20 s while `/v1/models` listed
`gemma-4-12b`: chat completions on that server still hang, as on 2026-10-18 and 2026-10-19, so the
live `/ask` answer is unconfirmed. A `/ask` whose client goes away frees the one-run slot (the next
request was not refused 409). Two observations for follow-up: the transcript hides the agent's own
successful list calls too (`source_list` for exactly this question), because the `ops` stream does
not say which client sent an operation; and in the narrow footprint the tab labels drop the tuning
range.

Follow-ups from the final review (fix wave on `ui`). Each `ops` record now names its caller in
`transport`: `Node::apply_from(op, transport)` (`apply` is `"local"`), `"ui"` for `/op` with the
allow-listed header `X-Zeta-Client: ui`, `"http"` for any other `/op` (MCP included, it is an HTTP
client), `"agent"` for the calls of a `/ask` run (`LocalExecutor::tagged`). The transcript hides only
the UI's own successful list polls and prefixes other callers' records (`[agent]`, `[http]`), so the
agent's `source_list` now shows; a list op typed on the command line is shown from its `/op` reply
(ruling R6, replacing R5's typed counter). Errors on the `ops` stream are bounded like ops and results
(`{"error": <variant>, "truncated": true, "bytes": n}` over 8192 bytes). The overlays resolve event
streams with one `GET /streams` per poll and remember jobs without one (`fm_demod`), which had cost
about four lookups a second per job; at most four jobs per tab are watched (the rest show `—`), and a
stream refused for want of slots (503, probed after a failed upgrade) is noted once in the
transcript. Minors: the secret field is cleared after login; switching source stops the UI's own
spectrum job; closing the menu erases the selection; ribbons are capped at 200; `/ui/` files carry a
Content-Security-Policy (`blob:` is needed for the microphone's AudioWorklet) and `nosniff`; narrow
tabs show a compact centre (`144.8`); the e2e run has a 300 s deadline. The aarch64 binaries were
rebuilt with the UI (cross, release, built from 013ceea): `zeta` 23 148 104 bytes, sha256
`be0e943b…09e9c0`; `zeta-mcp` 10 836 096 bytes, `acff40ff…2eb2e0861`; `deploy/pi/smoke.sh` PASS.

Parked: stopped job records are never pruned (keep the last N; a core follow-up); speech-to-text
ribbons for channels (spec §7) are deferred; the `window.zetaTest` hooks used by the e2e scenarios
ship in the production bundle.

Pending: the phone check. Start the node on the LAN address,
`ZETA_SECRET=… zeta --state-dir … --source-file tone=… --source-file aprs=… serve --listen
<wired-ip>:7979` (wired; <wifi-ip> on WiFi), open `http://<wired-ip>:7979/ui/` on the phone,
paste the secret, and check the narrow renderer: waterfall full width, ☰ drawer, a drag opening the
menu by touch, Decode › APRS and its ribbon. The mic will not work there over plain HTTP (browsers
allow it only on a secure origin). Also pending: the microphone against a real `voiced`, the
HackRF walk (`--source-soapy` in the release `--cfg soapy` build, 104.9 MHz, Decode › NBFM audio;
the HackRF was unplugged), and a live `/ask` once the field host's chat completions answer.
The live `/ask` is no longer pending: on 2026-10-19 in the evening gemma-4-12b answered again, and the
UI's agent pane works live against the field host (calls in the transcript, then the answer).

## Plan 7 status

Plan 7 is delivered on `audio-wfm`: a streaming rational resampler (`Resampler`, polyphase L/M,
at least 63 taps) behind every demodulator, so all audio streams are 48 kHz mono in ±1 whatever
the channel rate; `wfm_demod` (75 kHz deviation, 50 µs de-emphasis by default, 15 kHz mono),
`am_demod` (envelope, slow AGC) and `ssb_demod` (Weaver form, `sideband` usb or lsb, `center_hz`
is the dial frequency) as stages and job kinds, with `sideband` a required-nullable op field like
`deviation_hz` and each refused on kinds that do not take it; layer-1 rules for `wfm` (60-250 kHz,
cv below 0.35, rms deviation above 5 kHz, a 19 kHz pilot at least 10 dB up raising 0.6 to 0.9),
`am` (carrier line) and `ssb` (one-sided, with the dial frequency in the evidence); the iNTERCEPT
`signals.json` (594 entries, Apache-2.0, `data/NOTICE`) as a knowledge provider and a layer-2
classifier scoring frequency 0.4, bandwidth 0.3 and modulation 0.3, with the bandwidth required for
entries wider than 50 kHz and ties to the narrowest range; the `Classifier` trait takes the band
centre; and in the UI, Decode entries for wideband FM, AM, USB and LSB, ▶ on any audio job, and
the top layer-2 name drawn beside the layer-1 label. `rustfft` and `num-complex` are built at
`opt-level = 3` in debug builds. The workspace gates passed on 2026-10-19: fmt, clippy with
`--all-targets --all-features -D warnings`, `cargo test --workspace --all-features` 547 passed, 0
failed, 2 ignored (the live Kiwi and the field host's model tests) over 80 test binaries,
`no_privileged_deps`, `zeta agents-md --check` and `--skill --check`, and `cargo test -p zeta
--test ui`: all 30 Playwright scenarios passed in headless Chromium (125 s, no skip line; the two
classify scenarios take 19 s and 28 s of it). The `opt-level` override did not move the e2e time
(131.9 s before, 132.0 s after), so the FFT is not where that time goes.

By hand on 2026-10-19 against the field host's B210 (release `--cfg soapy` build of this branch, 104.9
MHz with a 1 MHz capture band), driven from this box with the debug `zeta --node`; one-shot
`--then-tail` is not supported with `--node`, so each job was started, its stream found with
`stream list`, tailed with `stream tail` and stopped. `wfm_demod` with a 200 kHz band streamed
audio frames at 48 000 Hz (786-787 samples, peaks 0.32, 0.50, 0.42). `measure` labelled the
station `wfm` at 0.9 with "19 kHz pilot +16.3 dB" (snr 18.8 dB, occupied 81-83 kHz, cv 0.34,
rms deviation about 17.5 kHz): the station is stereo. `classify` over 2 s spans inside the ring's
last 10 s, timed from fresh `measure` events, returned `L1 wfm` (0.9, pilot +16.1 to +16.7 dB),
`L2 FM Broadcast Radio` (1.0) and `L2 CBET` (0.7, "bandwidth off") on 19 of 20 spans; the first
span came back `unknown` (occupied 56 kHz, below the 60 kHz floor, and cv 0.356, above 0.35) and
so carried no layer-2 label. On the spans that passed, cv sat at 0.333-0.345, close under the 0.35 limit.
`knowledge lookup --region AU` at 104.9 MHz returned the catalogue signatures FM Broadcast Radio
(87.5-108 MHz, 250 kHz, WFM), CBET and HD Radio (FM), and no allocations. `am_demod` (10 kHz) and
`ssb_demod --sideband usb` (3 kHz) both started and streamed 48 kHz audio frames (peaks near
0.0002: a constant-envelope FM carrier carries no AM). Every job started was stopped; the node's
own spectrum job was left running.

Parked: `classify` measures the band twice (once for layer 1, once in the catalogue classifier);
broad catalogue entries such as CBET (88-108 MHz, 2.2 kHz) score 0.7 with the bandwidth off and
show up beside the right answer; `fm` does not map to AFSK, so APRS is named by the decoder, not
the catalogue; AM and SSB playback has not been tried in a browser; stereo and RDS are not built.
The wfm rule's cv limit has little margin on a real station (one span in 20 missed it); letting a
strong 19 kHz pilot carry a marginal band is a candidate fix. `knowledge lookup --region AU` returned no
allocation covering 104.9 MHz.

## Plan 8 status

Plan 8 is delivered on `wideband` (ff919fb..14f1df7 plus this docs commit): `PfbBank` in
`zeta-dsp` (2x-oversampled polyphase analysis, bins at least 750 kHz wide, 8..=256 of them,
Blackman prototype at 0.75 bin with 16·bins taps, at least 60 dB down in every bin whose span
excludes a tone) behind core's `ChannelBank`/`BankFactory`, one bank per source at 2 Msps or more on
its own thread; the `channel` stage draws from the nearest bin when the request fits one bin width
and the output rate divides the bin rate, and takes the direct path otherwise, with the same
semantics either way (`StageContext` passes the bank to every stage); spectrum rows v2 at a fixed
`rows_per_s`, quantised to u8 at 0.5 dB with `floor_db`, `size` up to 8192 and following the band;
the IQ ring sized from `--ring-budget-mb` (default a quarter of `MemTotal`, at most 4 GiB; 1..10 s,
recomputed on retune, released on shrink); `JobKind::Scan` (16 MHz steps at most, 250 ms dwell,
the lease on the whole span, `conflict` naming the scan for anything else on the source); and in
the UI, `#band`, pan and wheel zoom (render-only inside the capture, a retune beyond it) and the
stitched scan waterfall with "Scan this span". `wfm_demod` now decimates before its audio filter.

The workspace gates passed on 2026-10-20 at 14f1df7: fmt; clippy with `--all-targets
--all-features -D warnings`; `cargo test --workspace --all-features` 588 passed, 0 failed, 3 ignored
(live Kiwi, the field host's live model, and the release-only PFB timing test) over 86 test binaries;
`zeta agents-md --check` and `--skill --check`; `cargo test -p zeta --test ui`: all 35 Playwright
scenarios passed in headless Chromium (144.6 s, no skip line). The release timing test
(`cargo test -p zeta-dsp --release --test pfb pfb_runs -- --include-ignored`) pushed 2 s of 24 Msps
through 32 bins in 1.39 s on this laptop: 0.694 of one core.

By hand on 2026-10-20 against the field host's B210 (release `--cfg soapy` build of 14f1df7, 12 cores),
driven from this box with the debug `zeta --node`. The B210 asserts `max_bandwidth_hz` 16 MHz:
the SoapySDR capability probe runs while UHD's automatic master clock sits at 16 MHz, so
`source ensure --bandwidth-hz 24000000` is refused `out_of_range` (max 16000000) although the
radio, on USB 3, can do more; the acceptance ran at 16 Msps. At 98 MHz / 16 MHz the node logged
`ring for b210: 10.00 s at 16000000 Hz (1280.0 MB)`; the spectrum job's rows had `size` 4096
(3906.25 Hz bins), `q` present, `rows_per_s` 20 and `start_ns` 50 ms apart. Four `wfm_demod`
jobs (100.5, 94.5, 101.3 and 93.7 MHz, 200 kHz each, bank-served) and a `measure` (95.3 MHz:
`wfm` 0.9, pilot +40.9 dB, snr 29 dB) ran together beside the spectrum job; each audio stream
gave 48 000 Hz frames with peaks above 0.05 (2 s medians 0.20, 0.46, 0.84, 0.16). A fifth
candidate, 102.5 MHz (40 dB above the floor), demodulated to peaks near 0.008 and was replaced:
a carrier with little or no program audio there, not investigated. CPU over 10 s from
`/proc/<pid>/task/*/stat`: the node 121 % of one core in all (`top` agreed, 120 %), of which the
bank thread `pfb-98000000` 33 %, the SoapySDR reader 9 %, the tokio workers 72 % for the six jobs,
and 7 % another node thread; with the spectrum job alone the node used 23 %. So each job is far
under one core (about 13 % per channel job on average), but the bank at 16 Msps costs 33 % of one
core, over the 20 % target (it would be about 50 % at 24 Msps). `scan` over 0-1 GHz started at
the B210's 42 MHz floor and stepped 56, 72, ... 1000 MHz: 61 distinct `center_hz` values, four rows
per step about 270 ms apart, one sweep in 19.2 s (two full sweeps seen in 60 s). During the scan
`job start --kind measure` and `--kind wfm_demod` on b210 were both refused `conflict` with the
scan as holder and the option "stop the scan holding the source"; `source list` showed the scan
as the only holder and `current` 712 MHz / 16 MHz; after the stop, no holders and `current` at
the last step (232 MHz / 16 MHz). Headless Chromium against the live UI during a second scan
switched to scan mode and painted the stitched view (84 columns in 25 s; `scratch/ui/scan-live.png`),
with the FM band bright near 100 MHz. The B210 was left at 104.9 MHz / 1 MHz with no jobs. Not
done: the `--ring-budget-mb 1024` restart (the node was not restarted); the sizing line above is
the default budget's. A UI tab open elsewhere kept restarting its own 1 MHz spectrum job on the
B210 and had to be stopped before the wide retune.

Parked: the scan runs its FFT inline on a worker (spawn_blocking it); the dwell does not skip
the PLL settling time at its start; each dwell copies 32 MB from the ring; a remote node's
`current` is cleared when idle; the bank's cost is over its 20 % target on the field host (above); the
overlays do not reposition on a view change; the scan view does not redraw a full sweep from the
ring rows on zoom; core does not clamp a scan step to a sub-band span (min(step, span), centre
inside the span) pending the fix wave; and the Soapy capability probe should read the rate range
after the master clock is raised (or assert the device's real maximum).

After the plan-8 fix wave (2026-10-20): the Soapy source probes a UHD device's sample-rate range at its
maximum clock, so the B210 reports 61.44 MHz with no device argument and a 24 MHz request becomes a 30.72
Msps capture (ring 10 s, 2.46 GB on the field host). With a spectrum job, four `wfm_demod` jobs and a `measure`
on that capture the node used about 173 % of a core: the bank thread 30 % (down from 50 % before the PFB
optimisations), the decoders 12-20 % each, audio clean. The plan's 20 % bank target holds at 16 Msps and is
missed by ten points at 30.72 Msps; `target-cpu`/multiversioning and a partial DFT for few bins are the
parked next steps. The scan, lease conflicts and stitched view were exercised live at 16 Msps earlier the
same night (0-1 GHz in 61 steps, 19.2 s per sweep). The plan's Conventions block says Hann for the PFB
prototype; the code uses Blackman (ruling R4) with a 0.75-bin cutoff and 16 × bins taps (ruling R5).

## Plan 9 status

Plan 9 is delivered on `decoders` (branch from main 857dbaa, tasks 1-10; the last implementation
commit before this docs commit is fb84e51). Not yet tried live: the live acceptance on the field host (below)
has not run, so nothing here was received off air.

What each decoder is:

- `pocsag_decode`: POCSAG from a 12.5 kHz FM channel; 512, 1200 and 2400 baud detected
  automatically (BitSync tracks 1 % rate error); one event per message with `address`, `function`,
  `kind`, `text`, `baud` and `protocol`.
- `rds_decode`: RDS from a 200 kHz broadcast band (19 kHz pilot PLL, 57 kHz band-pass, mixed
  to baseband); events `pi`, `ps`, `rt`. The first PI emits an event; a PI change clears PS and RT.
  The sensitivity floor is about 16-20 dB carrier to noise over 240 kHz, measured on synthetic
  fixtures only.
- `adsb_decode`: 1 090 MHz with at least 2 MHz of band (2.4 MHz asked for); the stage in `zeta-ext`
  feeds dump1090 (GPL, so an external process; `ZETA_DUMP1090` overrides the path) and parses its SBS
  output into events (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`,
  `track_deg`). The job fails naming dump1090 when the host has none.
- `acars_decode`: ACARS from a 10 kHz AM channel, 2400 baud MSK; one event per CRC-clean block
  (`reg`, `flight`, `label`, `text`, `crc_ok`).
- `classify` tries APRS, POCSAG and ACARS on narrow bands and RDS on wide ones; the RDS attempt
  uses a measured bandwidth threshold of 100 kHz (the fixture measures 138.5 kHz; ruling R5).
  `knowledge_lookup` signatures carry `decoder` (a table in the catalogue module).
- UI: decode menu entries and event ribbons for POCSAG, ACARS, RDS and ADS-B, and the ADS-B refusal
  on a narrow band.

Oracles, all installed on the dev host: multimon-ng decodes the POCSAG fixture (it needed the
modulation resampled 48 kHz to 22 050 Hz first); acarsdec decided the ACARS details the plan had
guessed: the tones are differential (2400 Hz keeps the bit, 1200 Hz inverts it), the CRC is
CRC-16/KERMIT (0x8408 reflected, init 0, low byte first, over the parity-inclusive bytes from mode
to ETX; vector 0x2189) and the pre-key is 128 bits (64 gave no sync); dump1090 (dump1090-fa 8.2-1)
decodes the synthetic DF17 fixture (ICAO 4840D6, callsign ZETA1). The oracle tests skip with a
printed line when the program is absent. The acarsdec oracle was flaky at about 50 % until its WAV
was padded with 2 s of silence (62ea5b5); 3 of 3 full-suite runs were then green.

Tests, as the task reports and ledger give them (the whole workspace at the branch head: 671 passed, 0 failed, 3 ignored, the 40 browser scenarios included): POCSAG 9 tests at task 1, RDS 12,
ACARS 8 plus 3 framer tests added in task 7, zeta-ext 2 unit plus 11 integration (the real dump1090
oracle among them), `zeta-core` ops 51, `zeta` cli 40, catalogue 10, and the UI suite 40 of 40
Playwright scenarios (160 s of the 300 s deadline, no skip line).
Task 10's README one-shot was run: `pocsag_decode` on the
`gen_pocsag` fixture at 148 637 500 Hz gives one event, address 1234567, function 3, `alpha`,
"HELLO ZETA", 1200 baud.

Parked (from the ledger): `bch_failures` is not reset per batch in the POCSAG decoder; no tests for
its BCH-failure and Tone branches; the POCSAG stage and `decode_pocsag_iq` duplicate the pipeline;
no test for a mid-run rate change in POCSAG or ACARS, or for `Lagged` in POCSAG, RDS or ACARS; 38 kHz stereo content is
untested against the RDS 57 kHz band-pass; an RDS PI change clears PS and RT without confirmation;
`write_all` to dump1090 can block if a live child stops reading (cancel still ends it); `free_port`
has a check-to-use race; the ADS-B clean-exit regression test could not reproduce the pre-fix race;
`parity_error_drops_the_block` also fails the CRC, so it does not isolate parity; UI ribbon
formatters print "null" or "undefined" for absent pocsag, acars and rds fields (partial events
only).

Live acceptance (to do, on the field host with dump1090, multimon-ng and acarsdec installed and the B210):
`rds_decode` on 104.9 MHz / 200 kHz (expect PS within 10 s), `pocsag_decode` on 148.6375 MHz /
12.5 kHz for 60 s, `acars_decode` on 131.550 MHz / 10 kHz for 120 s, `adsb_decode` on 1 090 MHz /
2.4 MHz for 60 s (a clean run without events is a pass for the plumbing only). Outcomes are not yet
recorded.

Live acceptance on the field host's B210 (2026-10-20, 06:45-07:05 AEST, merged main 6063a2d, dump1090 at
/usr/local/bin/dump1090 found at startup): `rds_decode` on 100.5 MHz / 200 kHz (the strongest station in a
20 MHz spectrum row, 56 dB above the floor) produced within 40 s the events PI `1361`, PS `ZINC 96 `, RT
`Wake up with Bilko & Jade`, PTY 11, from 2A then 0A groups. On 104.9 MHz (16.8 dB SNR, pilot +15.3 dB,
84 kHz wide), 107.1 MHz and 94.5 MHz no RDS event arrived in 30 s: the decoder's 16-20 dB CNR floor, or
no RDS carried; `classify` over the 104.9 MHz capture gave the layer-1 `wfm` label and the catalogue's
`FM Broadcast Radio`, no `RDS`. `pocsag_decode` on 148.6375 MHz / 12.5 kHz (60 s), `acars_decode` on
131.550 MHz / 10 kHz (120 s) and `adsb_decode` on 1090 MHz / 2.4 MHz (60 s) each ran without error
(captures at 120 kHz, 120 kHz and 2.4576 MHz; dump1090 started with the SC16 float path and exited cleanly
at the stop, no process left) but saw no events: plumbing pass, no signal verified at that hour with the
antenna in use. The radio was left at 104.9 MHz / 1 MHz with no jobs.


## Plan 10 status

Plan 10 is delivered on `speech` (tasks 1-8), fast-forward merged into `main` at effb67d on
2026-10-20, and tried live on the field host the same day (below).

What was built:

- Audio frames carry a channel count (interleaved; 1 today) on the bus, the plugin wire and the
  websocket.
- Stages: `denoise` (RNNoise through the pure-Rust `nnnoiseless`, BSD-3-Clause), `vad` (energy over a
  tracked noise floor, with onset look-back and hangover; emits `vad` events and the speech segments)
  and `transcribe` (crate `zeta-speech`: accumulates an utterance, downmixes and resamples to 16 kHz
  mono, encodes WAV, calls a `SpeechBackend`, emits one `transcript` event per utterance).
- Speech backends behind one trait: `SocketBackend` (the `local-voice` line protocol, `languages`
  probe limited to 2 s) and `NodeBackend` (another node's `POST /stt`, one hop only); `/stt`,
  `GET /stt/languages` and `?lang=` run on the same trait. Global flags `--stt-socket` and `--stt-node`
  (exclusive; ignored with `--node`).
- Attached jobs: the `stream_in` stage, the `job_attach` op and `zeta job attach`; `transcribe` is the
  first attachable kind (graph `stream_in -> denoise -> vad -> transcribe`).
- UI: transcribe entries, job attach and transcript ribbons.
- The separation slot: `examples/python/separate_stub.py` (an external audio-to-audio plugin) replaces
  `denoise` through `graph put`; `crates/zeta/tests/speech_chain.rs` proves it ends in a `transcript`.
  `examples/python/stt_daemon_stub.py` is the bring-your-own daemon.
- Docs: `docs/speech.md` (installing `local-voice` on a node's host, bring your own daemon, one daemon
  for several nodes), "Transcribing a channel" in field craft, README.

Final fix wave (after the whole-branch review):

- The voice gate's floor on digital silence (R15): a frame under -80 dB still counts as quiet and
  never raises the floor, but now lowers it to -80 dB, its minimum (and seeds an unseeded floor
  there). A chain that starts or restarts inside a burst therefore hears speech from the first gap
  on. The trade-off, documented on `Vad`: after a gap of exact zeros, -50 dB channel noise is
  forwarded for about 1 s plus the hangover until the floor re-adapts.
- The floor across a lag (R16): a lagged input no longer recreates the detector. The floor is kept;
  only the run in progress, its look-back and the stage's pending samples are dropped, and a run
  already announced is closed with one `speech: false` where its forwarded audio stopped.
- A deterministic speech fixture (R17): every test that builds the `speech` fixture uses one
  helper (`crates/zeta/tests/common/mod.rs` `speech_fixture`), which puts the 2 s pause before the
  3 s of bursts, so a chain starting with the file opens on silence. The retry in `speech_chain.rs`
  is gone: one `graph put`, one tail.
- Because the pause delays the first transcript by about 2.7 s of file time, the two `attach.rs`
  transcript windows were widened from 10 s to 20 s (the UI ribbon scenario's bound).
- Wider timing margins in the `zeta-speech` tests, and the `/stt` 503 text is now `serve has no
  speech backend (--stt-socket or --stt-node)`.

Gates at the branch head, `cargo test --workspace --all-features --no-fail-fast`:

- Run 1: 763 passed, 0 failed, 3 ignored.
- Run 2: 761 passed, 2 failed, 3 ignored. The failures were `attach`'s two transcript tests on
  their 10 s windows, since widened.
- Run 3, with the 20 s windows: 762 passed, 1 failed, 3 ignored. The failure was `zeta-dsp`
  `acars::tests::acarsdec_decodes_our_fixture`, an external-oracle test unrelated to this plan,
  which passed 3 of 3 when run alone.

Each run included `zeta` `cli` 43, `attach` 5 (ok in runs 1 and 3), `speech_chain` 1, and the UI
suite (`ui_scenarios_pass_in_headless_chromium`) ok in 171.65 s, 164.04 s and 171.61 s, with no
`ui e2e skipped` line.

Repeated runs, unloaded:

- `cargo test -p zeta --test speech_chain`: 5 of 5 ok.
- `cargo test -p zeta --test attach`: 3 of 3 ok before the window change and 3 of 3 after.

`cargo fmt -p` for each of the 17 members, `cargo clippy --workspace --all-targets --all-features --
-D warnings`, `zeta agents-md --check` and `--skill --check` are clean.

Observation for live acceptance: in the in-process attach chain, the whole chain occasionally starts
about 4 s late (seen in 1 of 5 unloaded probe runs: the `vad` and `transcript` events all arrive
about 4 s later than usual). Watch the time to the first `transcript` on air.

Live acceptance on the field host (2026-10-20, as the `sdr` user): `local-voice` installed per docs/speech.md
section 1 (clone from a git bundle under `~/zetasdr/local-voice`, `uv python install 3.13`, `uv sync`,
the Parakeet model copied, `voiced.service` as a user unit with linger; `languages` answers
`error:unknown command`, `file:` answers a transcript). The field host's own source tree is on branch
`wideband` with uncommitted work and no shared history with this repository, so the speech branch was
delivered as `~/zetasdr/scratch/zetasdr-speech.bundle` and built in a separate clone
(`~/zetasdr/speech-src`, `RUSTFLAGS="--cfg soapy"`, target `~/zetasdr/speech-target`); the node was
restarted from that binary with its usual flags plus `--stt-socket $XDG_RUNTIME_DIR/voiced.sock`
(the log shows `speech backend: socket:/run/user/1021/voiced.sock`). The running `spectrum` and
`adsb_decode` jobs were stopped to free the B210, then `job start --kind wfm_demod --source b210
--center-hz 100500000 --bandwidth-hz 200000` and `job attach --kind transcribe --job <id>` over
`--node`. Within 40 s of tailing, the attached job's two event streams carried 18 `vad` transitions
and 7 `transcript` events from the programme's audio, for example "There's nothing left inside"
(3480 ms), "Maybe you can get down." (2160 ms) and "The storm is really bottom outside." (5080 ms),
each `language` en, `backend` the socket. Observations: `stream tail --timeout-ms` is capped at
10 000, so a tail loop is needed; on compressed broadcast audio the voice gate fragments speech into
runs of 0.6-5 s ("Um", "Baby."), the floor-rise-rate effect the final review predicted, so a slower
floor rise or a minimum-tracking floor is the first thing to try next; the first transcript arrived
about 20 s after the attach (the ~4 s late start was not isolated on air). The old binary at
`~/zetasdr/target/release/zeta` (the `wideband` tree) is untouched for rollback.

## Plan 11 status

Plan 11 is delivered on `worktree-view-policy` (tasks 1-8 and the final-review fixes, base `main`
f7757cd), 2026-10-20. Live
acceptance on the field host is pending.

What was built:

- Resume restores the capture: each source is first held at the band bounding its rigid jobs
  (when the radio can capture it), then its jobs launch a scan first, then widest first, elastic
  after rigid; a built-in graph whose `capture_rate_hz` differs from the actual rate is
  re-expanded.
- The ops log (`ops.log`, rotated at 8 MiB, appends serialised; successful read-only ops without
  their result) and `zeta ops tail`.
- A rigid start inside the capture elastic jobs follow is leased there without a retune.
- Elastic leases (`job start --elastic`, `spectrum` only; `source_list.elastic`).
- Atomic `replace` (`--replace ID[,ID]` on `job start` and `source ensure`; the conflict option
  `retry with replace: [...]`).
- The file-source `tune_range` (`--source-file ID=PATH,tune_min_hz=N,tune_max_hz=M`).
- UI: the page's own job is elastic; view policies (`follow`, `zoom_out`, `auto_scan_stop`,
  `finished_rows_s`, `scanner`) and `view set|show|reset|clear`; finished rows age out; the conflict
  offers "stop <kind> <id> and retry"; automatic scans on a scanner source when zoomed past the
  capture; a coarse scan layer under the fine capture; rigid bands outlined; eased follow; the
  shown-source fallback scan stops on a zoom back in and the view returns.

Upgrade: both ends of a `--source-node` link must run this version together (`job_start` gained
the required `elastic` and `replace`, which an older node rejects, and the reverse). A rigid page
`spectrum` job persisted by an older version resumes rigid; the page reuses an elastic one first.

Gates at the branch head:

- `cargo test --workspace --all-features --no-fail-fast`: 811 passed, 0 failed, 3 ignored (106 `test
  result` lines), no `FAILED`, no `ui e2e skipped`. The UI suite
  (`ui_scenarios_pass_in_headless_chromium`, 64 scenarios) ran ok in 233.93 s in the final gate
  (233.04 s alone).
- `cargo fmt -p` for each workspace member (all but the excluded `zeta-source-soapy-driver`),
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `zeta agents-md --check`
  and `--skill --check` are clean.

Deferred:

- The shown-source fallback scan's span does not follow later zooms (in scan mode the view stays
  inside the scan; a zoom back in to a capturable band stops it).
- A replace that times out (10 s) can leave the port tuned with no holder.
- An ease started in a hidden tab keeps `wf.easing` true until the tab is visible, which pauses automatic scan decisions.
- A source that faults before its first band shows no coarse layer.
- `reconcile_rate` is redundant with resume's `fit_capture` and can restart a job once needlessly.
- The UI suite runs at about 234 s of its 300 s deadline.

## Plan 12 status

Plan 12 is delivered on `worktree-view-policy` (tasks 1-10, base plan 11), 2026-10-21. Live
acceptance on the field host is pending.

What was built:

- Task 1: resumed channels take the shared bank; the channel path is in `job_context`,
  `source_list.bank` shows `bin_hz` and `consumers`, lag lines name the stage kind.
- Task 2: the resampler is exact up to a reduced ratio of 2 048 and snaps within 1 000 ppm above it.
- Task 3: UHD sources ask for exact master-clock divisors; an actual rate below the band is retried
  once, then refused.
- Task 4: `transcripts.log` and `zeta transcripts tail [--lines N] [--job ID]`.
- Task 5: pinned captures (`source_pin`/`source_unpin`, `sources/<id>.json`), restored first on
  resume; conflicts offer `unpin source <id> (source_unpin)`.
- Task 6: live receive gain (`source_set_gain`, `source_list.gain`, persisted, `zeta source gain`).
- Task 7: the page: pins outlined and counted as holders, `source pin|unpin|gain` commands,
  "unpin and retry".
- Task 8: `ppm=N` in `--source-soapy` args.
- Task 9: an always-on automatic squelch in `fm_demod`/`am_demod` (`squelch_db`/`squelch` node
  params, `squelch` events, the page's `squelch` tag); no `job_start` field.
- Task 10: README, field-craft ("Holding a band", "Reading transcripts", the squelch line), this
  status.
- Final-review fixes: a `graph_put` that changes only a demodulator's carried params (`squelch`,
  `squelch_db`, `deviation_hz`, `sideband`) keeps the job re-expandable; the page watches decoders
  before fm/am squelch streams; a holder-less pin conflict no longer offers "stop one of the
  holders"; `source_unpin` releases and writes under the source's gate; load warnings name what
  they are about; `transcribe` lag is counted by kind; the resampler refusal names the 2 048 limit
  and the 1 000 ppm bound; the tee drains past a lag at cancel; the guide lists optional stage
  params (`squelch`, `squelch_db` among them) and documents the squelch, gain and one-shot
  restores.
- Live-acceptance fix (R13): a pin retunes an unheld or elastic-only source to its band, even
  one inside the current capture (the elastic jobs follow); under decoders inside the band it joins
  their capture as before.

Upgrade: `source_list` items carry `pin`, `gain` and `bank`, so an older node using this node as a
`--source-node` fails its `source_list` round trip; both ends of a link move together (as plan 11).

Gates at the branch head:

- `cargo test --workspace --all-features --no-fail-fast` (after the final-review fixes): 878
  passed, 0 failed, 3 ignored (110 `test result` lines), no `FAILED`, no `ui e2e skipped`. The UI
  suite (`ui_scenarios_pass_in_headless_chromium`) ran ok in 239.10 s in that gate (233.28 s
  alone).
- `RUSTFLAGS="--cfg soapy" cargo build -p zeta --release` builds and links the driver.
- `cargo fmt -p` for each workspace member, `rustfmt --edition 2024 --check` on the driver,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `zeta agents-md --check`,
  `--skill --check` and `cargo check` in the driver crate are clean.

Deferred:

- Two Soapy gain tests have 50-100 ms timing margins (tighten); `set_gain` may hold the tune gate
  up to 2x `retune_timeout`.
- The squelch's floor window is 100 blocks, about 1 s live (10 ms blocks): tune at live
  acceptance. The squelch tag shows only on watched rows (decoders take the watch slots first).
- `ops.log` and `transcripts.log` writes are synchronous on runtime threads.
- `zeta_dsp::resample::MAX_FACTOR` (100 000) is no longer the limit anywhere; it is kept as a
  public constant.
- Plan 11's leftovers are still open: the fallback-scan span does not follow later zooms; the UI
  suite runs near its deadline (234 s of 300 s).

## Plan 13 status

Plan 13 is delivered on `plan-13` (tasks 1-5, base plan 12), 2026-10-21. Live acceptance on
the field host is pending.

What was built:

- Task 1: the `channel_plans` op (`zeta plans [--plan ID]`) and the `uhf_cb_au` registry (80
  channels, two rasters 12.5 kHz apart) in `zeta-core`.
- Task 2: the adjacent-channel guard in `fm_demod`: a plan channel's `channel` node carries
  `wide_hz`, the demod `guard_hz`/`guard_db`/`channel_hz`; `squelch` events carry `adjacent_db` and
  `guarded`; `graph put` with `guard_hz: null` turns it off; a job off every plan has the same graph.
- Task 3: `transcribe`'s `min_utterance_ms` (700) and the 4-slot speech-daemon queue (`queued_ms`,
  `dropped` reasons `queue`/`busy`, one EAGAIN retry).
- Task 4: the page: `ch20` tags on job rows and outlines, axis channel ticks, `view set plan
  auto|<id>|none`; the `cb` fixture source and one scenario.
- Task 5: `examples/python/cb_watch.py` (demodulators only on active channels, `--dry-run`),
  README, field-craft ("Channel plans"), this status.

Gates at the branch head:

- `cargo test --workspace --all-features --no-fail-fast`: 900 passed, 1 failed, 3 ignored (111
  `test result` lines) in the last of three runs; the failure was `acarsdec_decodes_our_fixture`
  (known flake), 3 of 3 reruns alone passed. No `ui e2e skipped`. The UI suite
  (`ui_scenarios_pass_in_headless_chromium`) ran ok in 225.68 s in that gate (234-240 s alone).
- The other two full runs each failed one test that passed alone: `child_exit_fails_the_stage`
  (`zeta-ext --test adsb`, 10 s timeout; 3 of 3 alone ok) and the UI scenario
  `quiet_channel_row_is_tagged_squelch` (also failed 1 of 3 UI runs alone; "squelch tag not found"
  within 3 s, a timing flake to fix).
- `cargo fmt -p` for each of the 17 workspace members, `rustfmt --edition 2024 --check` on the
  driver, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `zeta agents-md --check` and `--skill --check` are clean.

Final-review fix wave (on `plan-13`, after 014013d):

- `/stt` bounds a call at `stt_timeout` + the backend's queue wait, so a queued call answers or
  is told busy: 503 `{"error": "busy", "message", "wait_ms"}`, never a 504 at 30 s.
- `--stt-node`: a peer's busy 503 is `SpeechError::Busy` (a `dropped` `busy` event, not an
  error toward the job's limit), `NodeBackend::queue_wait()` is `STT_QUEUE_WAIT`, and its `/stt`
  request timeout is 75 s; `transcribe`'s outer bound has 1 s of slack.
- `cb_watch.py` fixes an opened channel's `squelch_db` at its measured floor + 6 dB (the row's
  per-bin dB converted to the channel's dBFS, `--squelch-margin-db`) with `graph_put` before it
  attaches `transcribe`.
- `fm_demod`/`am_demod` latch their last `squelch` event in `job_context` (`squelch`; cleared by
  a run with the squelch off), and the page seeds a watched job's tag from it, so the
  `quiet_channel_row_is_tagged_squelch` scenario is deterministic (it now opens the page after
  the gate event on purpose).
- The guard has 2 dB of hysteresis (`GUARD_HYSTERESIS_DB`); the squelch tag's title names the
  neighbour's `adjacent_db` and the guard; a failed `channel_plans` call is retried; the adsb
  `child_exit_fails_the_stage` bound is 30 s.
- Gate after the wave: `cargo test --workspace --all-features --no-fail-fast` 907 passed,
  0 failed, 3 ignored (111 `test result` lines), no `ui e2e skipped`; the UI suite 230.83 s
  alone, 235.63 s in the gate.

Deferred:

- `Segment` and `ChannelPlan` derive nothing; `channel_at` re-sorts per call.
- A same-rate re-expansion error at resume is silent; a bank-served record resumed with no bank is
  re-expanded onto the direct path (untested).
- A closed-state `squelch` event can carry a stale `guarded`; the guard is inert with the squelch
  off.
- The speech concurrency test should assert `max_concurrent() == 4`.
- The connect retry also delays the startup probe by 200 ms when the daemon is unreachable.
- A stored non-builtin `plan` id is dropped at page mount; a loaded plan list is cached for the
  page's lifetime; multi-plan axis ticks are unsorted.
- `cb_watch.py`'s floor is the row's 15th percentile, which reads a few dB low when a row
  averages few FFTs (about 4 dB on the 48 kHz test fixture; under 1 dB at live rates).
- Plan 12's leftovers are still open: two Soapy gain tests have 50-100 ms timing margins and
  `set_gain` may hold the tune gate up to 2x `retune_timeout`; the squelch's floor window (100
  blocks, about 1 s live) is untuned; `ops.log` and `transcripts.log` writes are synchronous on
  runtime threads; `zeta_dsp::resample::MAX_FACTOR` is an unused public constant. Plan 11's: the
  fallback-scan span does not follow later zooms; the UI suite runs near its deadline.
