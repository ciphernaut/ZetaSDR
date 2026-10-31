# TODO

## Scan: alternate the step grid and mask the centre and edge bins

Done in plan 15 (docs/superpowers/specs/2026-10-31-scan-grid-mask-design.md); live acceptance pending: the notch and edge widths it measures set `SCAN_NOTCH_HZ` and `SCAN_EDGE_MASK` (300 kHz and 0.10 to begin with).

## Lease: keep a lone job's capture centre off its channel

From plan 15's non-goals, 2026-10-31. A demodulator's or decoder's own capture centre sits on its channel, so the receiver's DC spike lands in the channel (the same problem the scan grid had; a one-off demod read it as a 59 dB carrier on 2026-10-23). Keeping the centre off the channel needs the lease manager to know the capture's minimum width before it tunes, which the capability record does not carry, and the offset depends on the notch width plan 15 measures (`SCAN_NOTCH_HZ`: its live acceptance's figure is this gist's input). Its own small plan after plan 15. Seen live 2026-11-01 on the B210: after `replace` moved the last fm_demod to 477.466 MHz the capture centred on it, putting the DC spike on the channel.

## Bug: stopped jobs leave work running (CPU leak, "channel stage lagged")

Fixed in plan 14 (docs/superpowers/plans/2026-10-27-field-fix-wave.md); live acceptance pending.

## Bug: a USB drop of a SoapySDR device segfaults the node

Fixed in plan 20: the driver runs in a `zeta soapy-worker` child (docs/superpowers/specs/2026-10-31-soapy-worker-design.md); live acceptance pending.

## Bug: a node that loses its only device stays up faulted, so a restart loop never restarts it

Fixed in plan 20: the worker is respawned and the source heals (docs/superpowers/specs/2026-10-31-soapy-worker-design.md); live acceptance pending.

## Bug: sample timestamps run ahead of wall-clock time (HackRF)

Fixed in plan 14 (docs/superpowers/plans/2026-10-27-field-fix-wave.md); live acceptance pending.

## Bug: `measure` jobs emit no events

Fixed in plan 14 (docs/superpowers/plans/2026-10-27-field-fix-wave.md); live acceptance pending.

## Bug: a refused `graph_put` persists its graph

A `graph_put` that loses a race with another put on the same job returns an error, but its graph is already in the record (`graph_get` and a restart use it). Found in plan 14 review, pre-existing. Fix: under the jobs lock, refuse before persisting when the entry is `Running` again after this put's stop.

## Feature: stereo for `wfm_demod`

`wfm_demod` is mono only. Operators expect stereo for broadcast FM. A working offline reference is in `scratch/fm_stereo.py` (local, not committed):
- discriminator to MPX;
- 19 kHz pilot band-pass, squared and band-passed at 38 kHz, normalised, as the L-R carrier;
- L-R recovered by mixing and a 15 kHz low-pass;
- L/R matrix, 50 us de-emphasis per channel.

Report the pilot's level (event or `job_context`), so "no pilot" (no station, or too weak to lock) is visible. Keep mono as the fallback below a pilot threshold, as receivers do: stereo needs about 20 dB more signal for the same audio SNR.

## UI: box a signal, demod it, then refine it on the graph

Done by plan 18 (`docs/superpowers/specs/2026-10-31-box-and-refine-design.md`): fit first, `job_retune`, draggable outlines, the width presets, tags linked to the rows. Left to plan 15's follow-on: a lone job's capture centre off its channel (the DC spike).

## Jobs: serialise restarts of one input

A per-input restart lock, so a `job_retune` or `graph_put` racing another restart of the same input cannot leave its attached job visibly `stopped`/`failed` for a few tens of ms. From plan 18's review, 2026-11-01.

## Lease: job_start on an unheld radio tunes twice

`tune_if_unheld` and then `ensure_as`'s empty-held path each tune the radio. Harmless, but one tune too many. From plan 18's review, 2026-11-01.

## Jobs: refuse job_retune on controller-owned jobs

An `identify` run's trial children (and a watch's identify runs) are ordinary records with an `owner`, so the CLI or the agent can `job_retune` one mid-trial and spoil the controller's dwell. Refuse `rec.owner.is_some()` naming the owner, as `job_stop` should for the same reason. From plan 18's review, 2026-11-01.

## UI: route the fit's measure start through applyOp

`fitBand` starts its `measure` job with `api.op` directly, so a conflict on it gets a bare `<kind>: <message>` note and no "stop … and retry" button. Route it through `applyOp` (or reuse its conflict rendering) so the retry is offered as for every other start. From plan 18's review, 2026-11-01.

## Node: 16 stream sockets starve a second page

One open page held 12-16 of the node's 16 stream slots (`max_ws`); a second page got `stream refused: too many stream connections` on every stream and could not paint. Raise the cap per client or per node, or make the page release the streams it no longer shows. From the B210 live run of plans 15/18, 2026-11-01.

## UI: the page watches at most 4 jobs, so a tag loses its squelch dot

With four demods and a `transcribe` attached, the oldest demod's tag had no squelch dot: the transcribe took a watched slot first (`MAX_WATCHED` 4, decoders before squelch-only jobs). Watch the squelch for every rigid job on the shown source, or prefer demodulators for the slots. From the B210 live run of plans 15/18, 2026-11-01.

## Jobs: an attached job's record keeps its input's old tuning after a retune

After its input's `job_retune` the `transcribe` row still read ch20 / 476.900 MHz while the demod sat at 476.925 MHz. `job_retune` (and `graph_put`) should update the attached records' `tuning` copy when they are relaunched. From the B210 live run of plans 15/18, 2026-11-01.

## UI: repeated Fit to signal walks the width down to the floor

Each fit measures the already-narrowed channel's occupied width and takes 1.25 times it, so repeating it converges on the kind's minimum (100.5 MHz `wfm_demod`: 163.3 → 150.6 → 150 kHz). Fit from a band at least as wide as the current one plus a margin, or measure a fixed wider box around the job's centre. From the B210 live run of plans 15/18, 2026-11-01.

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

Status (plan 16): modes are now rows (`modes/*.json`, operator rows in `--modes-dir`), so a new mode is a row plus its decoder, not an enum variant; in-crate decoders are graph rows, external ones program rows through `external_decode`, and each row carries its `classify`/`identify` hook and synthesiser test. Plan 17 needs from the synthesiser (`zeta-dsp` `fixtures.rs`, `zeta modes check`): generic `fsk`/`psk`/`ook` families (symbol rate, deviation or order, framing) so a row declares its test signal without new Rust, and the deferred echo impairment (multipath). Rows with audio-input programs also arrive then (`decoder.input.stream: audio` is refused today).

Original shape, now carried by rows: one job kind per mode, graph-expanded like the plan-9 decoders; external decoders through `zeta-ext` where a mature tool exists (dsd-fme / DSD+, radiosonde_auto_rx, wsjtx), in-crate `zeta-dsp` where the mode is small (M17, AIS, FLEX, RTTY). `classify` gains each mode's decode-to-identify (CRC/sync-word pass) and the catalogue entries gain their `decoder`.

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

## Feature: generic layer-4 framing decoders (bits to structured frames)

From the 2026-10-29 404.025 MHz investigation. The bit-level loop above gets an unknown signal to a bit stream; it stops there and the agent reads raw hex. A generic framing layer would carry the common clear, unencrypted link protocols seen on licensed UHF data channels, so the agent can report structured frames instead of hex.

Scope, generic only: a `frame_decode` stage (or a `framing` family of mode rows) that takes a `bits` stream and applies a declared framing: preamble/sync-word hunt, length field, bit order and whitening (NRZI, Manchester, differential), and a CRC check (a small library of polynomials: CRC-16-CCITT, -IBM, -DNP, CRC-32). On a clean frame it emits an `event` with the raw payload bytes and the framing that matched; it does not interpret the payload. Then thin, well-known serial-over-radio parsers as separate rows, each a pure bytes-to-fields decoder with a CRC gate: Modbus RTU, DNP3, IEC 60870-5-101/104. These are published open standards; the decoder is the generic protocol, chosen by signature match or named by the operator, never built against one named licensee's traffic.

Boundary (explicit, so a future session does not drift): this decodes clear, unencrypted, standards-based framing that the node is lawfully receiving. It never attempts to break encryption, never targets a specific operator's link as the deliverable, and emits no transmit path. An encrypted or proprietary payload is reported as `framed, opaque` with its length and CRC status, not decoded.

Needs: the `bits` stream kind and the layer-4 demod/slicer stages above (prerequisite); a CRC/whitening helper in `zeta-dsp`; one `frame_decode` stage; one row per serial protocol with its `event` schema and `summary`. Each protocol row gets a synthesiser test (plan 17's families plus a framing generator) so `modes check` proves it offline, no live capture of anyone's link required.


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

Delivered by plan 16 (`docs/superpowers/specs/2026-10-27-mode-registry-design.md`, `docs/superpowers/plans/2026-10-27-mode-registry.md`, status in the plan series): mode rows, the `external_decode` adaptor, the event envelope, the `video` and bits payloads, `classify` sweeps and `identify`. Still open from the same note:

- **Feeds.** `sink` stage kinds for the standard feeds (APRS-IS, SBS/Beast, AIS NMEA, PSK Reporter, WSPRnet, KISS TNC) so results leave the node the way other tools expect.
- **Time.** UTC-slotted modes (FT8/FT4 15 s, WSPR 2 min, JT65 1 min) and TDMA (DMR, TETRA) need true block timestamps. The HackRF drift bug above blocks the whole WSJT family.
- **Span decoders and trunking.** OnAir and OpenWebRX+ decode every POCSAG/APRS/VDL2 channel in a capture at once. On a pin that is one job per channel on the bank today; a decoder that finds its own channels would take the whole capture. Trunking (P25, DMR Tier III, MPT1327) needs a follower that starts voice jobs from control-channel events inside the pin.
- **Vocoders.** Codec 2 (M17, FreeDV) in-crate; AMBE/IMBE only through an external `mbelib` build, never shipped; HD Radio HDC off.
- **UI.** A flat menu of 100+ modes is unusable: pick modes through `knowledge_lookup` by band (what is plausible here) with a search fallback. Fits the Okudagram restyle.
- **Wideband modes** (DVB-T 8 MHz, DAB 1.5 MHz, LTE) run the direct path at full rate and want the GPU FFT; park them behind the HackRF-first set.
- **`sync_bank` flapping** (from the 2026-10-27 investigation, not reproduced). A bank replaced while `feed_bank` still drains old-band blocks can swap back and forth, stalling a channel stage silently.

## Feature: classify on a 12.5-150 kHz box tries a row-width slice at the measured centre

On a box between `MIN_BW_HZ` and the RDS gate, after a `no_frame` also try the row-width slice at the measured centre when layer 1 is not `noise` (plan 16 ruling 21): a packet 10 kHz off a 40 kHz box's centre is missed today and decodes on that slice. Fits the "centre on the measured signal" rule.

Related, from plan 17 (ruling 6): what `CLASSIFY_TRIES` counts. `classify` takes the best five rows by score, and the three sonde rows score 0.6 on any narrow FM box (centre list empty, width fits, `fm` in `modulation`) even outside 400-406 MHz, so on a 2 m or UHF channel they can fill three of the five `tried` slots as `skipped_quick` and push a quick-attempt row (APRS, POCSAG) out of the list. Count only rows with a quick attempt against the five (or take the five after dropping `skipped_quick`/out-of-range rows), and let the range term in the score drop a row whose `range_hz` does not contain the measured centre. Tests in `tests/classify.rs` carry an operator override for it today.

## Bug: a plain `pocsag_decode` never decodes on the debug-build bank at 2.048 MHz

Found in plan 16 task 7 (pre-existing): the debug-build bank path at 2.048 MHz does not deliver a decodable channel for a plain `pocsag_decode`, so the core identify tests run without the bank. Reproduce with the POCSAG fixture on a 2.048 MHz capture in a debug build, then check the release build before fixing.

## UI: staircase transcript ribbons on the waterfall

From the operator, 2026-10-27. Ribbons enter at the right edge and move left with the waterfall's columns (`crates/zeta-ui/web/overlays.js` `ribbon`/`place`): a new ribbon at the same frequency is pushed down a lane when it would overlap the previous one, up to `MAX_LANES`. On a busy channel this stacks downward with no order the eye can follow.

Wanted: when a new transcript line arrives on a channel, the previous one steps up (or the new one takes the base lane and older ones shift up), so a channel's lines always form a staircase: newest at the channel's frequency, older ones ascending and ageing out to the left. The eye reads one channel's conversation top to bottom, time ordered.

Points to work out:
- Animate the step (a short CSS transition on `top`) so the move reads as a scroll, not a jump.
- Screen real estate: the number of lanes must come from the pixel height available between this channel and the next job's ribbons, so phone, laptop and a 4K wall give 2, 4 and 8 lanes rather than a constant. Collisions between two channels' staircases: the lower channel's staircase should fold to fewer lanes rather than overlap.
- Interaction with `transcript-error` and `[dropped]` ribbons (they take a lane too) and with the `×N` repeat merge.
- Keep the UI e2e scenarios that find ribbons by text (they should not depend on lane).

Scope estimate: 100-200 lines in `overlays.js` plus CSS; one or two scenarios.

Related: a state-stream decoder (ADS-B) does not want a staircase at all but one ribbon per subject, folded by key; see "UI: ADS-B (and any state-stream decoder) on the page" below. The two should share the lane budget: a staircase for packet-shaped rows, a held roster line for keyed rows.

## UI: ADS-B (and any state-stream decoder) on the page

From the operator, 2026-10-31, with a screenshot (`scratch/Screenshot From 2026-10-31 16-13-17.png`, local): an `adsb_decode` job on a HackRF at 1090 MHz fills the waterfall with dozens of boxes for one aircraft (`7C5309` / `QLK335D`): `7C5309 ×14`, `7C5309 21000 ×6`, `7C5309 QLK335D ×2`, `7C5309 21000 -27.39,152.43`, ... stacked down `MAX_LANES` and spilling over each other. Nothing on the page can be read.

Cause (`crates/zeta-ui/web/overlays.js` `ribbon`): every decoder event is a ribbon at the job's tuning centre, folded into `×N` only when its text is identical and still at the right edge. The ADS-B row's summary is `{icao} {callsign?} {altitude_ft?} {lat?},{lon?}` and dump1090 delivers identity, altitude and position in separate messages, several a second per aircraft, with the position changing every time, so one aircraft yields four or five distinct texts that never fold. Ribbons were designed for packet-shaped rows (APRS, POCSAG: one readable thing per event); ADS-B is a state stream, and that is the mismatch. The map does not help: the `adsb_decode` row has no `position` block (the sonde rows do), and `map.js` keeps one marker and track per job, not per aircraft in a job.

Wanted, in order of value:
1. **Fold by key.** A mode row names the event field that identifies a subject (`icao`; `id` for the sondes, `src` for APRS, `reg` for ACARS, MMSI for AIS when it lands). The page keeps one ribbon per (job, key), merges each event's non-null fields into that subject's state, and re-renders the row's `summary` from the merged state: `7C5309 QLK335D 21000 -27.45,152.49 ×38`, updated in place, held at the right edge while the subject is heard and released to drift off after a silence (say 30 s). A row without a key keeps today's per-event ribbons. This alone fixes the screenshot and is generic. Suggested shape: `event.key: "<field>"` on the row (schema, `modes_show`, `AGENTS.md`); the node could do the merge too, so `job_context` carries a `subjects` map any client reads.
2. **A subject table for the job.** A pane like dump1090's own view: ICAO, callsign, altitude, ground speed, track, position, distance and bearing from `view set home` (`map.js` `distanceBearing` already does the maths), last seen, message count; sorted by last seen or distance; one row per subject, from the same merged state as item 1. The waterfall then shows one line per aircraft, or none for a job that has a table. The same pane serves APRS stations, AIS vessels and a POCSAG address list.
3. **Aircraft on the map.** Give the row a `position` block (`altitude_ft` needs a feet-to-metres factor the sonde rows never needed: a `scale` on the `alt_m` mapping, or an `alt_ft` alternative) and make `map.js` track per (job, key) rather than per job, so each aircraft gets a marker and a track and a job's popup names its subject. This is what the wave-2 "APRS positions on the map" item needs as well, since one APRS job hears many stations.

Not wanted: a per-protocol special case in `overlays.js` (`protocolText` already carries one fallback for `adsb`; the row should drive it). Keep the UI e2e scenarios that find ribbons by text working for the unkeyed rows.

Scope estimate: item 1 is a small change (a row field, 60-120 lines in `overlays.js`, a scenario); items 2 and 3 together are a plan of their own (a pane, the map rework, the row schema and the docs), roughly 400-700 lines.

## Build: an install script for the AMBE/IMBE vocoder libraries

From the operator, 2026-10-27. Digital voice (DMR, P25, NXDN, D-STAR, Yaesu Fusion) needs AMBE/IMBE vocoders that cannot be shipped: `mbelib` is patent-encumbered and OpenWebRX+ builds it on the user's machine. Their `buildall.sh` (`/Projects/ham/openwebrxplus/openwebrx/buildall.sh`) clones and builds `jketterl/codecserver` (plus `codecserver-softmbe` and `mbelib`) into debs with an interactive prompt per component.

Adopt the same shape: `tools/install-vocoders.sh` that clones `mbelib`, `codecserver` and `codecserver-softmbe` at pinned tags, builds, installs under a prefix the node finds, and prints what it did; `zeta doctor` (or the decoder's job failure reason) names the script when the library is missing, the way `adsb_decode` names dump1090. Codec 2 (M17, FreeDV) is free and goes in-crate or as a normal dependency instead. Note the legal position in `docs/` once, as OnAir does in "Good to know".

Scope estimate: 100-150 lines of shell plus a doc section; depends on the external-decoder adaptor for the voice decoders that consume it.

## Packaging: the run directory and install hints for handlers

From the operator, 2026-10-28, on two plan 16 review rulings. Today a program row's fifo lives under `<state_dir>/run/<unique>` (ruling 14) and a missing handler's hint falls through `apt install` → `dnf install` → `brew install` (ruling 15; the built-in row only names the Debian package).

- **Run directory.** A packaged install may put it in the system temp directory (`/tmp`), `/usr/local/var/run` or `/opt/ZetaSDR/var/run`: one `--run-dir PATH` (env `ZETA_RUN_DIR`) whose default follows the install layout (the state directory for a checkout, the package's `var/run` for a package), created with mode 0700, and the fifo stays one unique subdirectory per run.
- **Install hints.** Extend the row's install map to `snap` and `flatpak` where the handler ships that way (`snap install <pkg>`, `flatpak install <ref>`), and name an AppImage by URL in `url` (it is a download, not a package manager). The report line keeps one command and one URL; the order of fallbacks follows what the host has (`which apt dnf brew snap flatpak`).

Scope estimate: 50-100 lines in `crates/zeta-ext/src/proc.rs` and `node_setup.rs`, a row-schema field, two tests.

## Feature: decoder wave 2 (rows on the sonde frameworks)

From plan 17, 2026-10-29. Plan 17 shipped the frameworks (audio-input program rows, the generic `fsk`/`psk`/`ook` synthesiser families and the echo impairment, the `watch` job, `source_free`, `position` and the map, the SondeHub sink), so these are rows and small stages, not new machinery:

- **Voice.** M17 (4FSK 4800 baud, Codec 2, open) and DMR (4FSK, AMBE+2 through the vocoder libraries, see the install-script gist above): audio-output rows whose audio feeds `job attach --kind transcribe` unchanged.
- **AIS** (161.975 / 162.025 MHz, GMSK 9600, NMEA out; a `nmea` parser row, positions onto the map) and **FLEX** pagers (1600/3200/6400).
- **Whole-span POCSAG and APRS** as a `watch` with `decode_on: capture`: the detect stage finds the channels in a pin and the watch starts one decoder per signal, instead of one hand-started job per channel.
- **Stereo** for `wfm_demod` (see the stereo gist above) now that audio rows exist to consume it.
- **DFM and M20 frame synthesis.** `dfm_decode` and `m20_decode` have no `test` (they print `skipped: no test`, and `--strict` fails them); an `rs41`-style generator for each, proven against `dfm09mod`/`m20mod`, writes them in. The RS41 generator is the template (public protocol descriptions only, never `rs41mod.c`).
- **APRS positions on the map.** The APRS row has no `position` map; add one (the decoder's `info` carries it only after parsing, so a position field wants the parser) so APRS stations appear beside the sondes. One APRS job hears many stations, so this also needs the map's per-(job, key) tracks from the ADS-B display gist above; without them every station's fix lands on one marker.

## Bug: `--then-tail` on a program row with a squelched demod tails the squelch events first

Found in plan 17 task 7. `--then-tail` now tails the first event output of the job; for an audio-input program row whose `nbfm_demod` keeps its squelch on, that is the demod's `squelch` event stream, which publishes before the decoder's events, so the tail shows `squelch` frames and the decode may land after `max_frames`. The sonde rows run the squelch off, so they are unaffected; an operator audio row that keeps it on is not. Tail the last event output (the decoder's) or the one whose frames carry `protocol`, and test it with a squelched audio row.

## Bug: a sink-bearing decoder is frozen as an operator graph

Found in plan 17 task 10. A decoder with a `sink_sondehub` node (added by the watch's `sondehub` param) is stored `operator_graph: true`, so it is not re-expanded at a different capture rate, and the sink-less fallback after a failed sink relaunch is stored the same way with no capture rate. Keep the row's expansion and re-append the sink on re-expansion (store the sink config beside the graph), so a retune does not leave the decoder at the old rate. Same area: decoders the watch re-adopts after a `graph put` keep their old sink config, and a batch buffered at a restart is lost.

## Bug: `gated_stop` is cleared unconditionally by a sink restart

Found in plan 17 task 10. `store_graph` clears `gated_stop` before relaunching the same id so the relaunch is not suppressed; a `replace` that names only the decoder during the sink restart's teardown can therefore let it relaunch after being stopped. Clear only the entry the restart itself set (a token compare), and make `replace_during_a_sink_restart` deterministic (it can flake when the proof lands before the gate is taken).

## Architecture: a generic `sink` param on the watch

Found in plan 17's final review. The watch's feed out is wired for one sink: `DetectParams` carries a `sondehub` field (zeta-dsp `detect.rs`), `graph.rs` checks it against the `SONDEHUB_STAGE` stage's schema, and `watch.rs` appends a node of that kind to every decoder it keeps under the same name. A second feed (AIS NMEA or APRS-IS for the wave-2 rows, SBS for aircraft) would copy all three places and add another field the detect stage only passes through.

Replace it with `sink: {kind, params}` (or a list of them) on the detect node: `graph::validate` checks `params` against the named stage's `params_schema` and requires an `event` input and no outputs, the watch appends `kind` with `params` fed by the decoder's events, and `sondehub: {...}` stays accepted as shorthand for `sink: {kind: "sink_sondehub", params: {...}}` so existing watches resume. The `sondehub.<field>` error naming becomes `sink.params.<field>`, and the sink-restart path (`place_decoder`, the sink-less fallback) works on any kind.

## Bug: the watch's detect defaults fire on receiver noise

Found at the first live run (2026-10-29, the field host's B210 at 65 dB on 400-406 MHz with the discone): with the defaults (`threshold_db` 10 over the row's `floor_db`, which already sits 5 dB under the 2nd-percentile bin, and `width_hz` `[5000, 30000]`) the detect stage reported about a hundred "signals" 6-9 kHz wide across the band and the watch identified noise one 20 s slot at a time. `threshold_db` 18, `persist_ms` 5000 and `width_hz` `[8000, 30000]` left one real signal. The defaults were tuned on synthesised captures whose noise is flat.

Wanted: a threshold relative to the band's median (not the 2nd percentile), or a minimum run energy, so the defaults hold on a real receiver; a `signal` event rate limit; and `identify`'s candidate order on a 12.5 kHz FSK box should put the sonde rows before `acars_decode` (it tried ACARS first, then POCSAG, with the sonde rows after). Record the thresholds that worked in field craft meanwhile.

## Feature: a burst-aware layer 1

From the 2026-10-29 404.025 MHz investigation: `measure` labels whole one-second windows, so a 1 % duty burst signal (5 ms every 1000 ms) read as `noise` with a 47 Hz "bandwidth"; the FSK rule wants two steady tones 500-3000 Hz apart and never matches multi-level or bursty FSK; there is no symbol-rate estimate beyond a spectral line.

Wanted: gate the measurement on the burst (on/off at 1 ms, measure over on-time only); report `duty`, `burst_ms`, `period_ms` as features; a cyclostationary symbol-rate estimate (the `|x|^2` line); an M-level instantaneous-frequency cluster count for M-FSK and the squaring test for PSK. Each is testable with the plan-17 synthesiser families (`fsk`/`psk`/`ook` with `repeat`). Pairs with the ACMA knowledge provider: register says `16K0F1D`, layer 1 confirms "burst FSK, 16 kHz".

## Agent guide: SIGINT and UX additions

For `agents_md`/field craft: (1) on a licensed band call `knowledge_lookup` first and report licensee and emission before demodulating, noting data age; (2) when `measure` reports a low duty cycle, report burst timing and say the band label is unreliable rather than "noise"; (3) an unknown-signal playbook that ends at classification and signature write-back and stops before decoding third-party content, stating the legal line once; (4) lead the operator with the map and the one-line `summary`, name channels and licensees in plain words, and offer the next investigative step (measure, identify, knowledge lookup, direction-find).

## Feature: a `knowledge` provider over the ACMA licence register (acma-local)

From the 2026-10-29 investigation of a burst signal at 404.025 MHz: ZetaSDR's own layer 1 called it `unknown`/`noise` (`measure` averages a 1 % duty burst into nothing, reports a 47 Hz "bandwidth" and no symbol rate), two hours of hand analysis flip-flopped between PSK and FSK, and one SQL query against the local ACMA mirror (`/Projects/acma-local/data/acma.db`, the RRL as a SQLite mirror with an MCP server) named it in seconds: Unitywater fixed point-to-point telemetry, Example Suburb to Example Suburb, 1 W, emission `16K0F1D` (16 kHz FM digital data). `knowledge_lookup` should answer the same question: a provider that queries `device_details` + `site` + `licence` + `client` for assignments within the band and within N km of the station's position (a `--station LAT,LON` the SondeHub sink already wants), returning licensee, site, distance, power and the decoded emission designator (the mirror has the decode tables) as `allocations`, so `classify`'s layer 2 can say "a licensed 16K0F1D data link sits on this channel 12 km away" before anyone demodulates. Needs the mirror's path (or the MCP server's URL) as a node flag; the data is as fresh as its last `npm run sync` (29 Aug 2026 today). Also: register the acma-local MCP server in this project's `.mcp.json` so the agent can call it directly.

## Feature: the watch re-centres before it identifies

Found in plan 17's final review. The watch tunes its identify run on the detector's run centre, good to about one fft bin (1.46 kHz at a 6 MHz capture with `WATCH_BIN_TARGET_HZ` 1500), while `rs41mod` over NBFM audio decodes only within about ±1000 Hz of the sonde's centre (it slices the audio around zero, and an off-centre FM signal shifts that level); synthesised frames decode at ±1000 Hz and not at ±1500 Hz. A sonde at the wrong end of a bin is then reported `unknown` and retried on the same centre.

Measure before identifying: a short `measure` (or a power-weighted centroid over the run's bins in `detect`, which the spectrum rows already carry) gives the occupied band's centre to a few hundred hertz, and the identify run starts there. Alternatively the audio rows could remove the demodulated DC (a slow high-pass ahead of the WAV feed), which makes the decoders tolerant of a few kilohertz of mis-tune; measure both against the RS41 generator with the job held off the signal.

## Bug: a decoder the watch placed on another free source keeps running if the watch never recovers

Found in plan 20's reviews. With `decode_on: free` the watch puts a decoder on a second source; when the watch's own source faults and the watch fails, its decoders are left to fail on their own (plan 20 spec 4.7), but a decoder on a healthy other source keeps running with nothing to stop it, and recovery re-adopts it only if the watch heals. Decide whether a watch that stays failed (the fault outlasting some bound) stops the decoders it placed elsewhere.

## Bug: `source_ensure` and `source_pin` refusals from a faulted source lack the `source <id>: ` prefix

Found in plan 20's reviews. `job_start` and a job's failure read `source <id>: <fault>`, but `source_ensure` and `source_pin` still answer bare `io: <fault>`. Route them through the same helper `job_start` uses.

## Worker: a parent-side status-silence watchdog

Found in plan 20's final review. The worker writes its status every second; the node never notices when it stops. A driver deadlocked under the worker's status task (or a worker wedged with its stdout open) stalls silently, as it did in-process. Wanted: the supervisor faults the source with `soapy worker for <id>: no status for 10 s` and kills the worker, which the respawn path then handles like any other end.

## Sources: one helper for the source record update

Found in plan 20's final review. `note_capabilities` duplicates `Node::set_source_record`'s body (load, change one field, write atomically, keep the in-memory copy). Fold both into one `Inner` helper that takes the change as a closure.

## Tests: a shared fixture module for the node tests

Found in plan 20's final review. `tests/recover.rs` copies about 150 lines of fixtures (ports, configs, registries, wait helpers) from `watch.rs` and `attach.rs`. Move them into a `tests/common` module the three share.

## Node: `watch_healed` starts before `open` has resumed the jobs

Found in plan 20's final review. `watch_healed` is spawned in `add_port`, before `open` runs its own resume, so a heal during `open` runs its recovery concurrently with that resume. The source's gate serialises the two, so it is harmless today; start the watcher after the resume (or have recovery skip what the resume relaunched) so the order does not depend on the gate.

## Worker: control frames queue behind IQ blocks

Found in plan 20's final review. The worker's writer thread takes frames in order, so a reply or a status can wait behind up to 64 queued IQ blocks (at 20 Msps, tens of milliseconds and megabytes). Give control frames priority (a second queue the writer drains first) or cap the block queue in bytes rather than blocks.
