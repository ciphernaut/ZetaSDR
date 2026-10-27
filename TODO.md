# TODO

## Scan: alternate the step grid and mask the centre and edge bins

Gist for a spec, not the spec itself. Spec work is scheduled for the week after 2026-10-23.

**Problem.** `scan_steps` (`crates/zeta-core/src/ops.rs`) lays the steps (up to 16 MHz each) end to end: no overlap, and every step centre falls on a round number (448, 464, 480 MHz...). Each step's DC spike and its weak band edges go into the rows as if they were signal.

Field survey, 2026-10-23, HackRF, 440-488 MHz at 45 dB:
- Smooth humps 150-300 kHz wide around the step centres.
- Steady peaks on the 100 kHz grid.
- A one-off demod with the capture centred on its channel read the DC spike as a 59 dB carrier.

The waterfall and any survey built on scan rows inherit all of this.

**Idea.**
- Alternate the grid instead of overlapping it: even sweeps use today's centres, odd sweeps shift every centre by half a step. A frequency that sits on a centre in one sweep is mid-step in the next. Same cost per sweep; every frequency gets a clean look every two sweeps.
  - Overlap alone does not fix this: a step's centre is covered by a neighbour only at a pitch of at most half a step, which doubles the sweep.
- Mask the bad bins in the node, not in each consumer:
  - each row drops a notch around its centre (a few hundred kHz, to be measured) and the outer ~10% at each edge;
  - the masked bins are flagged as no data, not shown as low power.
- Report the alternation (shift, notch width, edge width) in the scan's `job_context`.

**Open questions.**
- How rows carry the mask. Lean: an explicit field such as `masked: [[lo_bin, hi_bin], ...]`, which stays backward-compatible. Reserving a `q` value would not, since `q = 0` occurs naturally.
- Notch and edge widths: fixed, per device, or measured from the hardware.
- Should a demod/decoder lease avoid centring the capture on its own channel (the same DC problem outside scans)?

**Scope estimate.** About 200-300 lines, more than half of it tests:
- `scan_steps` + tests in `ops.rs`;
- `scan_loop` / `fft_step` in `node/launch.rs`;
- coarse-view stitching in `crates/zeta-ui/web/coarse.js` / `waterfall.js`: masked bins keep the previous value. The fiddly part.
- a few lines of docs in `crates/zeta/src/agents_md.rs` / `AGENTS.md`.

## Bug: stopped jobs leave work running (CPU leak, "channel stage lagged")

Seen 2026-10-23 (HackRF, `zeta serve`, about 2 h of uptime). During the session dozens of jobs were started and stopped: `measure`, `fm_demod` + `transcribe` from `cb_watch.py`, spectrum, scan, and several pins.
- Load: the node ended up at 430-470% CPU with only three light jobs running (page spectrum, one 12.5 kHz `fm_demod`, `transcribe`).
- Busy threads: tokio workers, not the channel bank.
- Log: about 100-360 `zeta-dsp: channel stage lagged, 1 frames skipped` per second, about 96k in total. Demodulated audio crackled.
- Restart: after a restart with the same jobs, 9% CPU and zero lag warnings.

Suspect: stages or bus subscribers of stopped jobs, or of relaunched elastic jobs on a retune, that never exit.
- Related symptom: `measure` jobs started late in the session emitted no events within 15 s.
- Repro idea: start/stop N `measure` or `fm_demod` jobs in a loop (or re-pin repeatedly), then watch the thread count and CPU.

## Bug: a USB drop of a SoapySDR device segfaults the node

HackRF on a flaky micro-USB cable: the device re-enumerated and `zeta serve` exited with 139. Expected: the source faults (`source list` `fault`) and its jobs fail with that reason, as the docs promise for an unplugged device.

## Bug: sample timestamps run ahead of wall-clock time (HackRF)

2026-10-23, HackRF via SoapySDR:
- Transcript `start_ns` values ran 17-20 min ahead of the log write time (`t_ns`); one ran 40 min ahead.
- An `iq read` SigMF capture taken at 19:57 AEST was stamped `core:datetime` 2026-10-23T11:18:01Z, i.e. 21:18 AEST: 81 min ahead.
- The ring's `earliest_ns` (from `too_old` errors) advanced about 3.4 s in about 1 s of wall time, so `iq read` and `classify` spans picked from wall-clock time miss the ring.

Suspect: block timestamps advanced by a nominal or stale sample rate (e.g. after a retune or rate change between pins of 1/2/3/6/12/16/20 MHz), not by the device's actual rate, or not re-anchored to the clock.

Workaround used: read `earliest_ns` from a deliberate `too_old` error, then request a span from the newer half of the ring.

## Bug: `measure` jobs emit no events

2026-10-23:
- Worked early in the session: 16 jobs on a 12 MHz pin, about 1 event/s each.
- Later it returned zero events within 6-15 s, on 1, 2, 3 and 20 MHz pins (`bank` path), and still did after a node restart.
- `fm_demod` squelch events and `spectrum` rows on the same captures were fine.

Possibly related to the channel bank path (`job_context` `channel.path: bank`) or to the timestamp drift above (a stage waiting for a span that never closes).

## Feature: stereo for `wfm_demod`

`wfm_demod` is mono only. Operators expect stereo for broadcast FM. A working offline reference is in `scratch/fm_stereo.py` (local, not committed):
- discriminator to MPX;
- 19 kHz pilot band-pass, squared and band-passed at 38 kHz, normalised, as the L-R carrier;
- L-R recovered by mixing and a 15 kHz low-pass;
- L/R matrix, 50 us de-emphasis per channel.

Report the pilot's level (event or `job_context`), so "no pilot" (no station, or too weak to lock) is visible. Keep mono as the fallback below a pilot threshold, as receivers do: stereo needs about 20 dB more signal for the same audio SNR.

## UI: box a signal, demod it, then refine it on the graph

From the operator, 2026-10-23. Do not assume channels sit on a fixed grid: that assumption has bitten before.

Today a demod started from a box gets the box's width (114-137 kHz for a 12.5 kHz channel: about 10 dB of extra noise, crackly audio), and there is no quick way to fix it.

Wanted:
- Box a frequency on the graph and create the demod (+ transcribe) from it.
- "Centre this demod" on the graph view: snap the job's centre onto the signal's measured centre (e.g. from a `measure` event or the spectrum peak), not onto a raster.
- "Shrink to 2.5 / 5 / 6.25 / 12.5 / 25 kHz" bandwidth presets on the demod.
- Make it easy to see which job is on which frequency. With more than about 3 channels this is hard today. Ideas: a label on the graph at each job's frequency (job kind, short id, squelch state, last transcript snippet), and a job list that highlights its marker on hover or click, and vice versa.

## Feature: more digital decoding modes

From the operator, 2026-10-27. Today's decoders: APRS (AFSK1200/AX.25), POCSAG, RDS, ACARS (MSK), ADS-B (dump1090 via `zeta-ext`). The design's layer-2 list (spec 6.2) also names DMR, LoRa/MeshCore and RS41 radiosondes, none of which decode yet, and the field craft promises digital voice (DMR, D-STAR, M17) feeding `transcribe`.

Reference lists for a comprehensive target set: OnAir's 142 modes by family (`/Projects/ham/OnAir/OnAir/README.md`, "Modes"; every mode has a generated test signal), OpenWebRX+'s decoder set and SDRangel's ~45 plugins, compared in `/Projects/ham/CAPABILITIES.md` ("Digital modes / decoders"). OnAir's families (TV, broadcast, aviation, maritime, satellite, two-way, cellular/rail, pagers, IoT, weather/science, amateur digital, amateur voice, HF utility) are a reasonable grouping for ours.

Gaps, roughly in order of field value on a HackRF with a 2 m / 70 cm antenna in AU:
- Digital voice: DMR (4FSK 4800 baud, AMBE+2), P25 phase 1 (C4FM, IMBE), D-STAR (GMSK, AMBE), M17 (4FSK, Codec 2, open). Each produces audio, so `job attach --kind transcribe` works unchanged. Codec licensing: Codec 2 is free; AMBE/IMBE need `mbelib` (patent-encumbered, external process via `zeta-ext` like dump1090).
- Radiosondes: RS41 (GFSK 4800, 400-406 MHz), plus DFM, M10/M20. `radiosonde_auto_rx` and `sondedump` exist as external decoders.
- LoRa / MeshCore: 125/250/500 kHz chirps around 915 MHz. Chirp detection and header decode are feasible without the network key; payload stays opaque. Report, do not decode (field craft rule).
- FSK/MSK family: AIS (161.975/162.025 MHz, GMSK 9600), FLEX pagers (1600/3200/6400), NOAA/Meteor APT and LRPT (image), DSC on marine VHF ch70.
- HF digital via KiwiSDR: RTTY, PSK31, FT8/FT4 (`wsjtx` externally), WSPR, Olivia, CW.
- Trunking: control-channel following (P25, DMR Tier III) needs a second lease model: a voice channel hop per call. Big; out of scope for a first wave.

Shape: one job kind per mode, graph-expanded like the plan-9 decoders; external decoders through `zeta-ext` where a mature tool exists (dsd-fme / DSD+, radiosonde_auto_rx, wsjtx), in-crate `zeta-dsp` where the mode is small (M17, AIS, FLEX, RTTY). `classify` gains each mode's decode-to-identify (CRC/sync-word pass) and the catalogue entries gain their `decoder`.

Scope estimate: 300-600 lines per in-crate mode including fixtures and tests; 150-250 per external wrapper. A first wave of M17 + DMR voice + RS41 + AIS is a plan on its own.

## Feature: layer 4, working an unknown signal

From the operator, 2026-10-27. Today layer 4 is prose only (AGENTS.md "Working layer 4", spec 6.4): `measure` gives centre, bandwidth, SNR, burstiness, a symbol-rate guess and a coarse label; `classify` tries four decode-to-identify protocols and the catalogue; `iq read` saves a capture. There is no stage an agent can chain to go from a channel to bits.

Missing blocks (the spec's "vetted DSP blocks with typed parameters"):
- `resample` (exists in `zeta-dsp` as a library, not a stage).
- Demod primitives as stages: `fsk_demod` (2/4-level, measured tone spacing), `psk_demod` (BPSK/QPSK with Costas loop), `ook_demod` (envelope + threshold).
- `clock_recover` (Gardner / M&M, early-late for OOK) → symbol stream.
- `slicer` → bit stream events, with eye/constellation statistics in `job_context`.
- `bit_view`: run-length histogram, autocorrelation for frame period, sync-word candidates, Manchester/NRZI/differential hypotheses, a few candidate CRC polynomials over hypothesised frames.
- `correlate` against a given preamble or sync word.
- Write-back: a successful hypothesis becomes a signature (sync word, rate, modulation, framing) a future `classify` matches; every session writes a labelled SigMF capture + feature record + steps (the layer-3 training data of spec 6.4).

Needs a `bits` stream kind (or `event` frames carrying packed bits) and an event payload for symbol statistics. The agent path exists (`zeta ask`, `/ask`, MCP), so the loop is driveable once the stages are there.

Also a better `measure`: `symbol_rate_baud` by cyclostationary / spectral-line detection is `null` far too often today; add modulation-order estimates (constellation moments), tone spacing for FSK, chirp-rate detection for LoRa, and burst timing (on/off durations, period) for TDMA-like signals.

Scope estimate: the stage set about 1500-2500 lines in `zeta-dsp` with fixtures (synthesised FSK/PSK/OOK at known rates); `bit_view` heuristics 400-600; signature write-back 200-300; the `measure` upgrades 300-500. Two or three plans.

## UI: Okudagram (LCARS) restyle

From the operator, 2026-10-27. Restyle the web page in the LCARS / Okudagram idiom: rounded elbow frames, horizontal bar segments with end caps, the signature palette (orange, peach, lilac, blue, pale yellow on black), condensed uppercase labels with right-aligned numerals, blocks of pseudo-random numeric readouts as status fillers.

Today: `crates/zeta-ui/web/style.css` (391 lines, CSS variables from `:root`, dark `#0b0e14` background), 12 JS files, about 6200 lines. Layout lives partly in JS (overlays, waterfall, graph canvas), so a restyle is not CSS-only.

Shape:
- A theme layer: all colours and radii through `:root` variables, an `lcars` theme switching them (keep the current theme as `plain`). `view set theme lcars|plain`, persisted like the other view policies.
- Frame: an elbow-and-sidebar chrome around the waterfall, job rows as LCARS bar buttons (kind, channel tag, squelch state as colour), transport/ops pane as a numeric readout column.
- Canvas-drawn parts (axis ticks, channel outlines, graph nodes) read the theme variables rather than hard-coded colours.
- Font: a condensed sans with tabular numerals (Antonio or Swiss 911 lookalike; free options exist). No external CDN; bundle it in the UI assets.
- Keep the UI e2e suite green: scenarios select by id/role, not by colour or layout, so a restyle should not touch them; check the budget (the suite already runs near its 237 s deadline).

Scope estimate: 300-500 lines CSS, 100-200 lines JS for the theme hooks and canvas colours, plus the font asset. One plan, UI-only, no node changes.

## Architecture: a mode registry and a generic external-decoder adaptor

From the operator's decoder question, 2026-10-27. Before a decoder wave, four shapes to settle:

- **Mode registry.** `JobKind` is a closed enum of 14 variants with its graph expansion in one `match` (`crates/zeta-core/src/ops.rs`). A hundred modes cannot be enum variants. Make a mode a table row, like `channel_plans`: id, family, default `bandwidth_hz`, graph template, event payload schema, decode-to-identify hook for `classify`, catalogue `decoder` link, test-signal generator. `job_start --kind` takes a mode id; `schema` and the UI list the registry.
- **External-decoder adaptor.** `zeta-ext` is bespoke for dump1090 (~700 lines). Replace with a declarative wrapper: binary, args, input (IQ cu8/cs16/cf32 or audio s16 at a stated rate, via pipe), output parser (line JSON, SBS, NMEA, KISS, text). Then direwolf, multimon-ng, dsd-fme, dumpvdl2, dumphfdl, acarsdec, rtl_433, radiosonde_auto_rx, AIS-catcher, wsjtx `jt9`, satdump plug in with a row each. This is how OpenWebRX+ gets its breadth. Decide whether it is the existing `--plugin` envelope or a plain stdio contract.
- **Stream payloads.** `video` and `text` stream kinds exist but nothing emits them. Image modes (SSTV, APT, LRPT, fax) need an image frame payload; layer 4 needs a `bits`/symbols payload. Fix the envelopes before the first producer.
- **Event envelope and feeds.** Every decoder has its own payload. A common envelope (mode, `t_ns`, source, channel, `fields`) plus `sink` stage kinds for the standard feeds (APRS-IS, SBS/Beast, AIS NMEA, PSK Reporter, WSPRnet, KISS TNC) so results leave the node the way other tools expect.

Also to consider:
- **Time.** UTC-slotted modes (FT8/FT4 15 s, WSPR 2 min, JT65 1 min) and TDMA (DMR, TETRA) need true block timestamps. The HackRF drift bug above blocks the whole WSJT family.
- **Span decoders and trunking.** OnAir and OpenWebRX+ decode every POCSAG/APRS/VDL2 channel in a capture at once. On a pin that is one job per channel on the bank today; a decoder that finds its own channels would take the whole capture. Trunking (P25, DMR Tier III, MPT1327) needs a follower that starts voice jobs from control-channel events inside the pin.
- **Vocoders.** Codec 2 (M17, FreeDV) in-crate; AMBE/IMBE only through an external `mbelib` build, never shipped; HD Radio HDC off.
- **Test signals.** Extend `zeta-dsp` `fixtures.rs` into a per-mode synthesiser (offset, noise, clock error), OnAir-style, so every mode has a fixture before a capture exists.
- **UI.** A flat menu of 100+ modes is unusable: pick modes through `knowledge_lookup` by band (what is plausible here) with a search fallback. Fits the Okudagram restyle.
- **Wideband modes** (DVB-T 8 MHz, DAB 1.5 MHz, LTE) run the direct path at full rate and want the GPU FFT; park them behind the HackRF-first set.

## UI: staircase transcript ribbons on the waterfall

From the operator, 2026-10-27. Ribbons enter at the right edge and move left with the waterfall's columns (`crates/zeta-ui/web/overlays.js` `ribbon`/`place`): a new ribbon at the same frequency is pushed down a lane when it would overlap the previous one, up to `MAX_LANES`. On a busy channel this stacks downward with no order the eye can follow.

Wanted: when a new transcript line arrives on a channel, the previous one steps up (or the new one takes the base lane and older ones shift up), so a channel's lines always form a staircase: newest at the channel's frequency, older ones ascending and ageing out to the left. The eye reads one channel's conversation top to bottom, time ordered.

Points to work out:
- Animate the step (a short CSS transition on `top`) so the move reads as a scroll, not a jump.
- Screen real estate: the number of lanes must come from the pixel height available between this channel and the next job's ribbons, so phone, laptop and a 4K wall give 2, 4 and 8 lanes rather than a constant. Collisions between two channels' staircases: the lower channel's staircase should fold to fewer lanes rather than overlap.
- Interaction with `transcript-error` and `[dropped]` ribbons (they take a lane too) and with the `×N` repeat merge.
- Keep the UI e2e scenarios that find ribbons by text (they should not depend on lane).

Scope estimate: 100-200 lines in `overlays.js` plus CSS; one or two scenarios.

## Build: an install script for the AMBE/IMBE vocoder libraries

From the operator, 2026-10-27. Digital voice (DMR, P25, NXDN, D-STAR, Yaesu Fusion) needs AMBE/IMBE vocoders that cannot be shipped: `mbelib` is patent-encumbered and OpenWebRX+ builds it on the user's machine. Their `buildall.sh` (`/Projects/ham/openwebrxplus/openwebrx/buildall.sh`) clones and builds `jketterl/codecserver` (plus `codecserver-softmbe` and `mbelib`) into debs with an interactive prompt per component.

Adopt the same shape: `tools/install-vocoders.sh` that clones `mbelib`, `codecserver` and `codecserver-softmbe` at pinned tags, builds, installs under a prefix the node finds, and prints what it did; `zeta doctor` (or the decoder's job failure reason) names the script when the library is missing, the way `adsb_decode` names dump1090. Codec 2 (M17, FreeDV) is free and goes in-crate or as a normal dependency instead. Note the legal position in `docs/` once, as OnAir does in "Good to know".

Scope estimate: 100-150 lines of shell plus a doc section; depends on the external-decoder adaptor for the voice decoders that consume it.
