# TODO

## UI: themes as folders (plan 26)

Merged to `main` 576002a and deployed on test_host 2026-11-04: spec `docs/superpowers/specs/2026-11-03-themes-design.md`, plan `docs/superpowers/plans/2026-11-03-themes.md`. Themes are folders (`theme.json` + `style.css`) under repo-root `themes/` (console, plain, cyberpunk, terminal, psytrance, vibecoder), the operator's own in `--themes-dir`, listed by `themes_list` / `zeta themes list`, served at `/ui/themes/<id>/`; the author's guide is `docs/themes.md`, the UAT section is "Plan 26: themes". Mobile and other per-device layouts are later (see the footprints entry below).

## UI: per-footprint layout in theme.json (`footprints`)

Spec 4.6 of plan 26. `layout` in `theme.json` is footprint-independent today; an optional `footprints` object (keyed `wide`, `narrow`, later others) overriding `layout` per footprint is reserved and the loader refuses the key. Build it with the first per-device layout (a phone or a wall display), not before.

## UI: a node-level default theme

Declined 2026-11-03: the stored theme is per browser (`view set theme`), `console` is the fallback. Revisit if asked for a node-wide default.

## UI: plan 26 deferred minors

Loader: a startup line splits a folder name at `@`; the required-field slice in `theme_json.rs` repeats the struct's fields; nested font errors name `fonts.<x>` not `fonts[i].<x>`; built-in bytes are copied twice at load; `--animation` custom properties are renamed like animation properties; resource strings outside `url()` (`image-set("...")`, `src("...")`) are not checked; the lexer collects `char_indices` (about 16 MiB for 1 MiB of css); a future `web/themes/` directory would be shadowed by the theme route; each theme request copies its bytes. Page: an unparsable `rgba()` ground reads as transparent and text alpha is dropped; gradient-only grounds are unknown to the contrast checks and the light checks skip `--ok`/`--accent` on the overlay, the axis ticks and the badge; `canvas()` is effectively white. Built-ins: cyberpunk uses yellow for error, pin tag and selected graph node, squelch tags collapse to a box in the job row, 9-10 px Orbitron is marginal and the chamfer rule catches every input; psytrance has `--error` equal to channel 3 and `--ok` to channel 2, gradient-clip text has no fallback colour, the side pattern is nearly invisible and Comfortaa tags are 10 px; terminal's fallback cursor shows only on an unfocused empty line and bracketed actions truncate `fm_demod` to `fm_de...`; vibecoder tints "stopped" green like running, gives the thin segment the speaking glow, truncates the kind badge and embeds Inter and JetBrains Mono twice (about 1.06 MB). Tests: the screenshot diff pins `data-colour=1` so channel colours 2-6, the graph view, collapsed panes and speaking/hot states are not covered, and its steady-state replace is page-wide; `theme_hooks` adds the `.speaking`/`.hot` classes itself, so those assertions are vacuous; `.job[data-state=failed]` and `.dot.open` are documented but not asserted; the motion positive control only asserts non-empty and the hit test has one point; no permanent `.thin` rail test; no at-rest diff for cyberpunk and terminal after their menu selector change; a broken console override and the ask-path `themes_dir` pass-through are untested; weak-signal headroom on psytrance's violet noise is unverified on real traffic.

## Scan: the HackRF's spur grid coincides with the quarter-step shift

From the HackRF run on the field host, 2026-11-01 (plan series "Plan 15 status"). The HackRF One has spurs on multiples of 4 MHz whose level depends on the tune: a 3-bin spike at +4 MHz (and mostly -4 MHz) of 21-37 dB when tuned to 444, 460, 476 or 492 MHz, ±8 MHz spikes on the edge bins of every 16 Msps row, a +2 MHz spike at 462 and 470 MHz, a comb every 100 kHz around 480 MHz. The scan's quarter-step shift at a 16 MHz step is exactly 4 MHz, so one grid's +4 MHz spur lands on the other grid's centre and is painted where that grid masks its DC: the survey shows a grid of peaks that moves between sweeps (the plan's failure sign) that no mask width fixes. Wanted: a per-device spur list the scan rows mask as `masked` ranges (offsets from the tune, as the HackRF's are, plus absolute frequencies as the B210's 402.0245/402.1505/402.9005/403.1494/403.8996 MHz are: the watch's `spurs_hz` is the absolute half), measured once per device by a `zeta soapy survey` over a quiet span and kept with the source record; and a step or shift that is not a multiple of the spur spacing on a device that has one (15 MHz steps, or a shift of a third). The B210 showed no such grid.

Plan 36.1 made the shift a quarter of the stride (the usable fraction of the capture, 4.5 MHz for a HackRF's 20 MHz capture at 0.9), so it is no longer a multiple of 4 MHz by construction; the entry stays open until a HackRF sweep on the new grid shows whether the spur still lands on the other grid's centre, and for the per-device spur list.

## Soapy: the HackRF's read timeouts at 1 Msps

From the same run. At the 1 Msps UHF CB pin `stats.timeouts` climbs about 8 per second (71 → 412 in a minute) with the rate reading 1.1 % fast and no overflows; at 2 Msps and up it stays at 0. The HackRF is under its 2 Msps minimum there and the driver's read times out between blocks. Harmless so far (the jobs ran) but the counter is useless as a health signal at that rate: either read with a timeout scaled to the block duration, or capture at 2 Msps and decimate for a 1 MHz pin. Also: the 1 Msps capture gets no channel bank (`channel.path` `direct`, `no bank`); say why in `source list`, or give it one.

## Lease: follow-ups to widening a held capture

`source_ensure` with `widen` moves a held capture and restarts the holders on it, and `source_set_capture` makes the widest capture a source's habit (2026-11-12). Left open:

- A widened capture is not kept across a restart: the jobs resume on their own bounding capture unless the source's policy is `widest`.
- `job_start`, `job_retune` and `set_start` take no `widen` of their own: a caller widens with `source_ensure` first, then starts the job inside the new capture.
- The gap is unmeasured on real hardware: how long each stream is silent across a rate change, and whether an external decoder loses more than the frames in flight.
- The page offers the widest capture the view asks for; it does not offer a middle rate, and under `widest` it offers a move of the capture when the view is dragged past its edge, each a restart of every holder.
- `widest` is the source's full rate at its own depth. A policy naming a rate (or a fraction of the full rate) would let a host with less CPU take a middle course.

## Lease: keep a lone job's capture centre off its channel

From plan 15's non-goals, 2026-10-31. A demodulator's or decoder's own capture centre sits on its channel, so the receiver's DC spike lands in the channel (the same problem the scan grid had; a one-off demod read it as a 59 dB carrier on 2026-10-23). Keeping the centre off the channel needs the lease manager to know the capture's minimum width before it tunes, which the capability record does not carry, and the offset depends on the notch width plan 15 measures (`SCAN_NOTCH_HZ`: its live acceptance's figure is this gist's input). Its own small plan after plan 15. Seen live 2026-11-01 on the B210: after `replace` moved the last fm_demod to 477.466 MHz the capture centred on it, putting the DC spike on the channel.

## Bug: a refused `graph_put` persists its graph

A `graph_put` that loses a race with another put on the same job returns an error, but its graph is already in the record (`graph_get` and a restart use it). Found in plan 14 review, pre-existing. Fix: under the jobs lock, refuse before persisting when the entry is `Running` again after this put's stop.

## Audio: the default `deviation_hz` on a 12.5 kHz channel

The voice low-pass and de-emphasis shipped 2026-11-01 (`fm_demod` carries `audio_lowpass_hz` 3400 and `deemphasis_us` 530; the voice demodulators take `denoise: true`). Left, from the operator listening to a fitted ch9 demod on the B210: on a 12.5 kHz channel the default `deviation_hz` 5000 halves the audio level of a 2.5 kHz-deviation CB signal; the fit (or the channel plan) could set 2500 there.

## Speech: a Whisper server over the network

Asked 2026-11-04. Today speech is local to the node or goes through another ZetaSDR node: `--stt-socket` speaks the local-voice protocol, whose `file:<absolute wav path>` hands the daemon a path, so the daemon must share the node's filesystem (same host); `--stt-node URL` forwards to another node's `POST /stt`, so the GPU host must run zeta. Wanted: a third `SpeechBackend` in `zeta-speech` that POSTs the utterance's WAV bytes to a speech server on another host, e.g. `--stt-url URL` for an OpenAI-compatible `/v1/audio/transcriptions` (faster-whisper-server, speaches, vLLM) or whisper.cpp's `server` (`/inference`, multipart), with a bearer from `ZETA_STT_URL_KEY` (stripped from the worker and plugin environments like the other secrets) and `GET /stt/languages` answered from a flag or the server's model list. Keep the backend's 4 slots, the 30 s busy drop and the 4 MiB cap; exclusive with `--stt-socket` and `--stt-node`, as those two are with each other.

Caveat, network use: an utterance is 16 kHz mono 16-bit PCM, 32 kB per second of speech (256 kbit/s while it uploads), up to `max_utterance_s` 15, so at most about 480 kB per call, sent as one burst after the utterance ends, not streamed. One channel is nothing on a LAN; a `plan_start` with `attach: transcribe` on all 80 UHF CB channels can have every slot uploading at once in a busy minute, and on a slow uplink (a Pi at the antenna on WiFi or 4G) the upload time adds to `queued_ms` and counts against the 30 s busy window, so drops (`reason: busy`) rise before the GPU is the limit. Say so in the docs and the startup line; measure on the test_host-to-desktop LAN and over WiFi; consider FLAC or 8 kHz for narrowband voice (CB audio is 3.4 kHz wide) to cut it by half or more, only if a Whisper server accepts it without a quality loss. The audio of third-party transmissions leaves the host: the operator's choice, said once in the guide. The mudmap's speech boxes gain the REMOTE row.

## Feature: stereo for `wfm_demod`

`wfm_demod` is mono only. Operators expect stereo for broadcast FM. A working offline reference is in `scratch/fm_stereo.py` (local, not committed):
- discriminator to MPX;
- 19 kHz pilot band-pass, squared and band-passed at 38 kHz, normalised, as the L-R carrier;
- L-R recovered by mixing and a 15 kHz low-pass;
- L/R matrix, 50 us de-emphasis per channel.

Report the pilot's level (event or `job_context`), so "no pilot" (no station, or too weak to lock) is visible. Keep mono as the fallback below a pilot threshold, as receivers do: stereo needs about 20 dB more signal for the same audio SNR.

## Jobs: serialise restarts of one input

A per-input restart lock, so a `job_retune` or `graph_put` racing another restart of the same input cannot leave its attached job visibly `stopped`/`failed` for a few tens of ms. From plan 18's review, 2026-11-01.

## Lease: job_start on an unheld radio tunes twice

`tune_if_unheld` and then `ensure_as`'s empty-held path each tune the radio. Harmless, but one tune too many. From plan 18's review, 2026-11-01.

## Jobs: refuse job_retune on controller-owned jobs

An `identify` run's trial children (and a watch's identify runs) are ordinary records with an `owner`, so the CLI or the agent can `job_retune` one mid-trial and spoil the controller's dwell. Refuse `rec.owner.is_some()` naming the owner, as `job_stop` should for the same reason. From plan 18's review, 2026-11-01.

## UI: route the fit's measure start through applyOp

`fitBand` starts its `measure` job with `api.op` directly, so a conflict on it gets a bare `<kind>: <message>` note and no "stop … and retry" button. Route it through `applyOp` (or reuse its conflict rendering) so the retry is offered as for every other start. From plan 18's review, 2026-11-01.

## Node: stream sockets are a fixed pool

One open page held 12-16 of the node's 16 stream slots (`max_ws`); a second page got `stream refused: too many stream connections` on every stream and could not paint (the B210 live run of plans 15/18, 2026-11-01; the operator's page hit it again while agent `stream tail`s ran). The default was raised 16 → 64 the same day. Still to do: a `--max-streams N` flag on `serve` (env `ZETA_MAX_STREAMS`), per-client fairness so one page cannot take the whole pool. The page now holds one feed socket per shown source (plan 27).

## Page: positioned jobs on other sources onto their feeds

`MAX_POSITIONED` and the per-job sockets remain for the map's jobs on other sources. One feed per such source replaces them.

## Feed: follow-ups from plan 27's final review

Found in the whole-branch review of plan 27 (2026-11-05); none blocks the feed, each is a known gap.

- Feed capacity: one feed's 256-frame buffer (BUS_CAPACITY) is shared by every job on a source, so a burst on one decoder (adsb_decode sends one event per Mode S message) lags the whole feed and triggers a full re-seed of every squelch dot. A larger capacity for feeds would keep one busy decoder from costing the page its gates.
- Stale dots while the feed is down: the page keeps squelch dots and counts every job on the shown source as watched while its feed socket is closed, so a feed down longer than the fold idle age can fold a channel on stale evidence. Gate `activity().watched` on the feed being open, or clear the dots on close.
- Out-of-order job polls: agent.js `refresh` has no in-flight guard, so an older `job_list` landing after a newer one makes `ov.sync` forget a just-started job (its counter and gate reset). The feed's poll requests make it slightly more likely.
- A failed `job_context` seed read is marked gateless and not asked again until a lag, a reconnect or a graph change, so a transient error leaves that row without a dot. Retry errors on the next sync instead.
- Test nits: no test that `unregister` clears the feed table and the unfed set; a bare `panic!()` in ops_stream's `a_port_added_after_open_has_a_feed`; attach.rs's feed drain stops on `Lagged`; no websocket test of `lagged` on a feed socket; `node_source_frames_are_not_fed` uses a fake mix provenance rather than a real mix job.
## Modes: HFDL ground stations as origins

Plan 35 gave VDL2 and ACARS a `subject.station`; HFDL (`hfdl_decode`) is left out. dumphfdl splits a station's frames across an SPDU (`spdu.src`) and LPDUs (`lpdu.src`), its ids are integers, and the row's `gs_id` names the addressed station on a downlink and nothing on an uplink. Before `subject.station` can hold, the row needs output pointers covering both PDU types and a numeric key.

## Origins: a node shutdown loses up to a minute of station counts

`Node::drop` only cancels, and `serve` returns without awaiting the job supervisors, so the run-end flush of `job_context.stations` never lands on a restart or a deploy. The counts never go backwards: a launch seeds each station from the larger of `context.stations` and the kept answer's `stations`, and a relaunch carries both from the last run's entry. A shutdown still loses up to a window (a minute) of counts not written yet.

## Aircraft data links: done in plan 30

Done in plan 30 (`docs/superpowers/specs/2026-11-06-aircraft-links-design.md`, `docs/superpowers/plans/2026-11-06-aircraft-links.md`); the airband named set is `sets/airband.json` (plan 31): its `vdl2` member is live now that `vdl2_decode` exists, and needs `dumpvdl2` on the host (refused `unavailable` without it).

## Aircraft links: deferred minors from plan 30

- `adsb_decode` has no `subject.merge` yet (one line adds `["icao"]`).
- ADS-C over the X.25/ATN half is not mapped.
- The VDL2 fixture is unrecorded until UAT, and so is the HFDL one (Task 8 was skipped: no Kiwi reachable from the build shell).
- The ADS-C tag wrapper is confirmed only on the fixture: the slot index varies (the map lists slots 0 and 1) and the altitude unit (feet) is from the ADS-C spec.
- `--source-position` is per node start, not an op.
- The merged table's `Via` lists protocols, not job ids.
- In a merged group keyed by `reg` the ICAO address is shown nowhere (vdl2's `icao` is its key, not a column).
- A 200 `/status` page from a Kiwi without a usable `gps=` is neither counted nor diagnosable.
- The `$center_khz` first-frame wait has no direct test.
- A string that trims to "" is kept as "".
- An older federated node refuses a plan-30 node's `source_list`: `--source-node` from an older binary rejects the unknown `position` field (as `ring` and `worker` did before it). Either both ends upgrade together, or `zeta-source-node` parses `SourceInfo` leniently.
- `NodePort` (`crates/zeta-source-node`) does not forward a remote source's `position`: a one-line `Port::position` copying `remote.position`.
- The fake-program checks assert no `summary`, and HFDL's only the uplink.
- No scenario covers a merged group's member dropped by `subject_drop_s` (Review Focus 4).

## Jobs: an attached job's record keeps its input's old tuning after a retune

After its input's `job_retune` the `transcribe` row still read ch20 / 476.900 MHz while the demod sat at 476.925 MHz. `job_retune` (and `graph_put`) should update the attached records' `tuning` copy when they are relaunched. From the B210 live run of plans 15/18, 2026-11-01.

## Fit: a weak voice channel fits to the width floor

From the B210 CB run, 2026-11-01 (plan series "Plan 18 status"). A ch9 transmission at 9-12 dB measured `bandwidth_hz` 600-800 (`fsk`: "two rf tones 500 Hz apart", or `noise` between words) because `measure`'s occupied band is the bins 6 dB over the floor, and at that SNR only the carrier line clears it, never the ±3 kHz voice sidebands; 1.25 x 800 Hz floors at 5 kHz, so the demod was fitted 5 kHz wide on a 12.5 kHz voice channel (intelligible, transcribed, but clipped). The width rule should not trust an occupied width under a voice channel's minimum: when the layer-1 label is `fm`, `fsk` or `unknown` and the measured width is under the kind's floor, take the channel plan's width at that centre (12.5 kHz on UHF CB) or a measured width at a lower threshold (bins within 20 dB of the peak, or the width that holds 99 % of the band's excess power), and say which in the `fitted` line. The same 1 s window also mislabels an FM voice carrier as `fsk` (the carrier plus one modulation line); a burst-aware layer 1 (below) helps here too.

## UI: repeated Fit to signal walks the width down to the floor

Each fit measures the already-narrowed channel's occupied width and takes 1.25 times it, so repeating it converges on the kind's minimum (100.5 MHz `wfm_demod`: 163.3 → 150.6 → 150 kHz). Fit from a band at least as wide as the current one plus a margin, or measure a fixed wider box around the job's centre. From the B210 live run of plans 15/18, 2026-11-01.


## UI: more actions on an existing job, and a selected job lit on the waterfall

From the operator, 2026-11-01 (after the B210 day). A running job's row and its tag/outline menu should offer the next steps directly: listen (the audio player on a demodulator), attach `transcribe`, decode or investigate its band (`identify`, `classify` on the job's band and span, `measure`, a `watch` on it), stop. Today the row has graph/stop and the outline menu has Fit, the width presets and Stop (plan 18), and the box menu is the only way into identify/classify. Selecting a job (a click on its row, tag or outline) should also light it on the waterfall as a sticky selection, not only on hover (`hot` from plan 18 is hover-only), so the operator can see which job the next action applies to. One entry table shared by the row, the tag and the outline menu.

## UI: on-waterfall messages cover the newest columns

From the operator, 2026-11-01. Event ribbons (`signal new`, `identifying`, `unknown`, transcripts, the watch's lines) are laid out from the right-hand edge, over the newest columns, so they hide the fresh RF activity they announce. Anchor them at the left (the oldest columns) and let them run towards the right, or into a gutter, so the latest spectrum stays visible; the staircase lanes (above) and this share the layout code in `overlays.js`.

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

## Process: lessons for DSP and layer-1 fix rounds (2026-11-04)

From five fix rounds against the operator's 433 MHz OOK remote (the section below). Each cost a round or a wrong conclusion:

- **Real captures first.** Every round passed its synthesised tests and failed on air: the slicer's start-up seed broke three ways (an onset transient, a mostly silent first frame, a 73x pulse after silence) that no synthesised test held. Copy real windows from the node's burst store into `crates/zeta-dsp/tests/data/` and assert on them before changing code.
- **Test the chain the node builds.** A hand-built test chain ran early-late while `bits_graph` gave OOK M&M, so it proved nothing about the live job. Build test chains through `bits_plan`/`bits_graph`.
- **Sweep base against head.** A reviewer's ~110-case synthesised sweep through `measure_selection` (FSK, PSK bursts, POCSAG/APRS/ACARS, RS41, Mode S, AM, carrier, noise at 30/15/8 dB) found a 30x cost blow-up and new false `ook` labels the targeted tests missed. Make it a permanent test (today an uncommitted `probe.rs` under `target/tmp/review-ook-sparse/`) and run it on any layer-1 change.
- **Fix at the source.** The onset overshoot came from the shared `Levels` (fsk.rs); patching only the slicer left every other consumer exposed.
- **Verify live with a fresh capture.** A capture keeps the features of the binary that measured it, and `bits_start` reads them, so a layer-1 fix shows only on new presses. An op to re-measure stored captures would remove the trap.
- **Re-check thin margins.** The 8 dB off-level veto (2.7 dB over the worst real window), the 1/8 band-narrowing stop (exactly where the weakest window fits) and the 5 % whole-window refit threshold were set on eight windows and shipped without the final review: collect real windows across radios and SNR, and review them.
- **Mechanics.** Run gates with a private `CARGO_TARGET_DIR` and deploy from a copied binary (`ZETA_RELEASE=...`), since another session's `cargo test` swaps binaries in the shared target; the permission classifier refuses stopping the operator's jobs, so hand the operator the command; the worktree guard refuses compound shell commands, so keep helpers as script files.

## Bug: the bits chain does not recover a real OOK remote (first on-air test, 2026-11-04)

The operator's 433 MHz fixed-code remote on the field host's B210 (pin 435 MHz / 6.144 MHz): EV1527/PT2262 style at 433.882 MHz, about 30 kHz wide, 30-34 dB SNR; pulses 280 us (0) and 910 us (1) in a 1.25 ms bit, chip about 313 us (3200 Bd); a sync of one short pulse and 9.7 ms of silence (31 chips), then 24 bits, repeated about every 40 ms; code <button-code>, decoded by pulse width from the samples (every whole frame of both fixture windows). Captures on the node: the narrow set of job 01M6HTR8A8FK7E0P4G64RPPZTX (8 windows, 51.2 kHz at 433.882 MHz, layer 1 measured 3197 Bd) and the wide set of job 01M6HTNMP9ESVNSG6DXJPPE9DC (8 windows, 2.048 MHz at 433.4 MHz). Pin them before the store expires them, and copy one into a test fixture.

- Fixed offline (slicer seed, early_late for OOK); the live `bits_start` re-run on the field host is pending. On the node, `bits_start --modulation ook --symbol-rate-baud 3200` on the narrow set (job 01M6HTS9GDRVCNY236ZRPFDXWH) ran `clock_recover` with `algorithm: null` (the `bits_graph` OOK default then), which is Mueller and Müller on one channel, not early-late: it read 3199 Bd but never locked (error_rms 1.38). The slicer's eye_open was 0.04, the bits were almost all zeros, and the code's chip pattern matched at best 42/96 chips. M&M reads no timing across an OOK pulse's flat top, so `bits_graph` now gives every OOK chain `early_late`. Offline on the fixtures (`crates/zeta-dsp/tests/data/ook-remote-433882k-{1,2}`), `ook_demod` and early-late were sound: strobes read about 1.0 on and 0.0 off, error_rms 0.43/0.39, locked, and a fixed 0.5 threshold reads <button-code> on every whole frame. The chain still sliced to zeros there: the slicer seeded its upper level from the first frame's maximum, which held the demodulator's onset transient (4.7 against an on-level of 1). No symbol reached the midpoint, and the level never came back. The slicer now seeds by a 2-means on the first frame (falling back to its extremes when the frame shows one level). `a_real_ook_remote_decodes_to_its_code` runs the chain `bits_plan`/`bits_graph` build and reads <button-code> on 5 and 4 frames. The levels' drag is real but did not cause it (see the deferred minor below).
- Fixed (2026-11-04): `bits_start` built the chain around the capture's centre, so on the wide set (482 kHz off a 433.4 MHz centre) the resampler cut the signal away. A signal off the centre whose measured band reaches past the resampler's passband (offset + half the measured width over 0.45 × the chain rate; not on a `noise` label) now gets a `channel` stage at the measured centre (band the measured bandwidth × 1.25, at least 4 × baud), `job_context.bits.shift_hz` recording the offset.
- Fixed (2026-11-04): the B210 windows (`ook-remote-b210-{1,2}`, 61.44 kHz, measured 1110 Hz off the capture centre and 40 kHz wide, so `bits_start` takes the `channel` path) sliced to all zeros, and the node's `bits_start` over three such windows read only 4 whole frames. Not the shift: the `channel` stage and the 61.44 kHz rate were sound (the centred chain failed the same way, the HackRF window decoded). Each window opens on 20 ms of silence, so `ook_demod`'s levels settled on the noise and scaled the first pulse by the noise's range: one strobe read 73 against an on-level of 1. The slicer's 2-means seed made that strobe a cluster of its own (midpoint 37, every bit 0; the degenerate check then fell back to the extremes). Fixed at both ends (after review: a first fence on the seed clamped the true on-level when under 10 % of the first frame was on). The shared `Levels` sets the high level at once on an onset more than `ONSET_SPANS` (8) spans over it and follows the rising edge for `ONSET_PERIODS`, so the first pulse reads about 1. The slicer seeds by a 2-medians from the 3rd/97th percentiles (from the 10th/90th a 5 % on-cluster starts in the noise and no median leaves it), judges the degenerate case on the starting span, and clamps a level's update to one span past the levels. Every whole frame reads <button-code> (12 and 13 frames), `b210_windows_decode_every_whole_frame`.
- Fixed (2026-11-04): layer 1 measured the HackRF window (`ook-remote-hackrf-1`) at 1598 Bd, the 800 bit/s bit rate's second harmonic, and labelled it, and both 433882k windows, `fsk` (the instantaneous-frequency clusters counted the off samples' noise, which spreads across the band, as a second tone 2.5-8 kHz from the carrier), so a bare `bits_start` picked FSK. The FSK clusters now count only the samples whose envelope reaches half its 90th percentile (FSK's envelope is constant), and a two-level envelope whose on and off runs are whole multiples of one unit measures its chip rate from the run lengths (`ook_unit_rate`, the label's evidence `(runs)`; not when FSK tones show; the fitted runs at least half of all and the largest of the unit, 2× and 3× that fits kept, so a half or a third of the chip is not taken) before the cyclostationary line: all five windows read `ook` at 3195-3198 Bd, and `bits_start` with nothing given picks OOK at the chip rate (`bits_start_infers_ook_at_the_chip_rate_on_a_real_remote`).
- Open: on `ook-remote-b210-1` the centred chain (no `channel`) reads 10 of 12 frames and the shifted path `bits_start` takes reads 12; likely the resampler's 0.45 passband cutting a 40 kHz band at 24.6 kHz. Not investigated.
- Fixed (2026-11-04): the live `bits_start` on the narrow B210 set and on the HackRF decoded four of the remote's codes (`<button-code>`) with the clock locked, given `--modulation ook --symbol-rate-baud 3200`; the layer-1 fixes above make both unnecessary (to confirm live after deploy).
- Fixed (2026-11-04): a weak HackRF set (`ook-remote-hackrf-sparse-{1,2,3}`, one press of two buttons, 431/71/471 ms at 62 500 Hz, 20-34 dB SNR): layer 1 read the 800 bit/s bit rate from the cyclostationary line and `unknown` on two windows and `fsk` with no rate on the third, so a bare `bits_start` refused (`no symbol rate measured`), and given the rate it refused the third window's 56 580 Hz band (`... leaves the capture ...`). Measured: the burst gate was not the cause on the 431 ms window (it was not gated); in a 62.5 kHz band the signal is a few dB over the noise, so the whole-band gate cut 1 ms "bursts" out of the 471 ms window, its occupied band read 37-57 kHz of mostly noise on the shorter or gated windows, and in the 431 ms window's 6.6 kHz band the pulses read a third of a chip short and the gaps as much long, so the fit took two thirds of the chip (4791 Bd). Now `ook_unit_rate` fits the edge bias with the unit (`OOK_UNIT_MAX_BIAS` 0.3 units), tries 1.5x the converged unit too, sets its edges between the envelope's two levels (a 2-medians) rather than off its 99th percentile, narrows the band by halves to 2 kHz when the occupied band holds no fit (an FSK tone check on the occupied band first, the off runs 6 dB under the on runs at twice the narrowed band, and a rate at most the band), and runs over the whole window when the gate cut bursts; a rate from the runs carries no FSK or PSK order and labels `ook`. Every window reads `ook` at 3180-3210 Bd; a bare `bits_start` on each window of the set picks OOK at the chip rate (`bits_start_infers_ook_on_every_window_of_a_weak_remote_set`), the chain from the 431 and 471 ms windows' own features reads 9 frames of <button-code> and 8 of <button-code> (`a_weak_remote_decodes_from_its_own_measured_features`), and a padded band past the capture is clamped to what fits about the measured centre (the capture less twice the shift), refused only under the 4 x baud floor. The record's `bandwidth_hz` on such a window is still the noisy measured one (57 kHz): only the chain's channel is clamped.
- Open (minors from the 2026-11-04 review): the `Levels` onset jump (`ONSET_SPANS` 8) fires on one strong impulse in silence or over keying (commit it only when the next period's maximum stays past the bound, or only while the levels are the warm-up noise's), and in `fsk_demod` only `hi` jumps, so a carrier-led FSK burst's first symbols scale to about ±2; a first frame of 17 strobes or fewer seeds the 2-medians on its min and max (nearest rank at 0.03/0.97), so an onset outlier there is its own cluster again (a minimum seed length would close it); an on-cluster under about 3 % of the first frame still starts in the noise; the HackRF fixture has no chain-decode test and the 433882k chain thresholds (3 and 2 frames) sit under the 5 and 4 they read; `fsk_clusters` runs twice on a two-level envelope (`ook_unit_rate`'s guard and `measure_band_noted`): compute it once.
- The real captures (`ook-remote-*`) are the operator's and are not published; their tests read the expected codes from each capture's `zeta:expected` annotation and skip without the file. The public `ook-ev1527-synth-*` (the `gen_ook` example, an invented ID) run the same chains. Open: on the synthesised fob at 20 dB (51 200 Hz, a tenth of a chip of edge bias) the run-length fit reads 1600 Bd with 10 us raised-cosine edges and 4800 Bd with 40 us ones; the fixture uses 20 us, where it reads 3193. Another thin margin of `ook_unit_rate` to review with the ones above.
- A fixed-code OOK remote like this one wants a mode row (rtl_433-style pulse-width decoding) more than layer-4 bits: a `pulse_ook` stage measuring on/off widths and a row per common timing (EV1527, PT2262) would decode it directly.

## Feature: layer 4, working an unknown signal

From the operator, 2026-10-27. Plan 19.2 shipped the stages (`capture_in`, `resample`, `fsk_demod`, `psk_demod`, `ook_demod`, `clock_recover`, `slicer`, `correlate`, `bit_view`), `graph_start`, `bits_start` and the Bits pane; the playbook is "Working layer 4". What is left:
- Live chains by hand on a real capture (spec 9 item 3, after 19.3): the 404.025 MHz captures through `bits_start` and the Bits tab.
- LoRa chirp-rate detection in `measure` (an unknown 250 kHz chirp is reported unknown today).
- The `measure` upgrades 19.1 did not take: `measure` finds no symbol rate on short FSK bursts (the e2e fixture's captures had none, so `bits_start` needs `symbol_rate_baud` given), and better order estimates from constellation moments.

## Bits: deferred minors from plan 19.2

- A bits job's outline on its capture's source tab (it has source `node` and shows on no radio tab).
- `bit_view` decoding the likely line code (Manchester, NRZI) and re-running its candidates on the decoded stream.
- An 8PSK chain test (the BER tests cover BPSK and QPSK).
- `correlate` on `capture_in`'s segment boundary, when a sync straddles one.
- `resample` does not flush its filter at input end: a one-pass chain loses one symbol.
- The decision demods' shared levels release both levels every period, so a long one-level run drags the other (OOK silence without a seam, a long FSK carrier lead-in). Measured on the real remote: after each 31-chip sync silence the first pulse reads 1.33-1.43 instead of 1.0 (the on-level released to about 0.7); harmless to a midpoint slicer, so left as is.
- `eye_open` reads 0 on any outlier: use a percentile edge.
- `psk_demod` holds up to 5 x 4096 samples while its coarse estimate defers.
- A general fan-out (tee) for a node output; today one output feeds one edge.
- The Bits pane's minors: stale sync marks across a seam, a full redraw per event, `withCorrelate` matching the node id `slicer` rather than the kind.
- `bit_view`'s `hypotheses` off the async worker (`spawn_blocking`) so the window can grow again: `window_bits` is capped at 262144 because the analysis runs on the async worker (55 ms on 256 kbit framed, release).
- A `stream_in` root in `graph_start` names a stream id that changes per launch (a relaunch or a resume reads a dead stream): fail it at `graph_start`, or document it.
- A wider sync word hides a 16-bit sync followed by a constant header byte: the CRC search then cuts frames at the 24-bit word; try the CRC from each contained width.

## Feature: built-in rows for the layer-4 parsers

Plan 19.3 delivered `frame_decode`, frame rows, and the `frame_parse` parsers (`modbus_rtu`, `dnp3`, `iec101`, `iec104`), which an operator frame row reaches through `decoder.parser`. What remains (2026-11-08): built-in parser rows for dnp3, iec101 and iec104 were dropped, because a row with no centre is tried on every band in classify and identify, and iec104 has no check strong enough against noise. A built-in parser row wants a way to be excluded from blind scoring and chosen only by a signature match or by name; then each row gets its synthesiser test so `modes check` proves it offline.

Boundary (explicit, so a future session does not drift): this decodes clear, unencrypted, standards-based framing that the node is lawfully receiving. It never attempts to break encryption, never targets a specific operator's link as the deliverable, and emits no transmit path. An encrypted or proprietary payload is reported as `framed, opaque` with its length and CRC status, not decoded.

Scope estimate: the stage set about 1500-2500 lines in `zeta-dsp` with fixtures (synthesised FSK/PSK/OOK at known rates); `bit_view` heuristics 400-600; signature write-back 200-300; the `measure` upgrades 300-500. Two or three plans.

## Signatures: deferred minors from plan 19.3

- `matches` counts every classify, a `capture_classify` replay included.
- No decay of a learned signature's confidence (a spec non-goal).
- A hand `signature_propose` with a framing carries no CRC count, so it is 0.6 at most.
- The Bits pane does not show `frame` events (a `crc ok|bad` tag per frame row would).
- A learned row's `band.centers_hz` is one centre, so a link that moves channel scores only the signature.
- The IEC 101 fixed-length frame is not decoded.
- `frame_decode`'s `manchester` phase is chosen per segment by the sync hunt.
- `measure` recovers no symbol rate from direct FSK keying on the e2e fixture (7 of 8 captures), so a framed accept from such a capture writes no learned row (it needs a rate): improve FSK symbol-rate measurement (2026-11-08).
- Sessions with no captures (graph jobs, frame-row jobs on a radio) are never swept, so `state_dir/sessions` grows without bound: add a cap or an age limit (2026-11-08).
- The demod/slicer chain loses bodies with long zero runs (the 11a iec104 vector does not decode end to end): a scrambled or bit-stuffed test vector, or a slicer that holds its threshold through runs (2026-11-08).
- `matched()` writes the record on every match, watch retries included.
- identify counts a learned match even when a decoder then proves the row.
- A panicked stage in `offline::run_chain` never unregisters its bus stream.
- Pins are left behind when an accept loses a race to a reject or hits an I/O error.
- One malformed allocation drops all of a capture's allocations in `signature_propose`.
- The page's event views show a frame job's `timing` and `bits` events as noise.
- A superseded signature's `learned_<id>` row stays live in the registry, so it still takes a classify slot and an identify dwell: skip learned rows whose record is superseded (2026-11-08).
- Several proposals can be pending for one signal (a capture's 0.3 and a bits job's 0.9), and accepting both gives overlapping records: fold a new proposal into a pending one of the same band (2026-11-08).
- `LEARNED_MIN_SCORE` lets centre and bandwidth alone label a different emitter of similar width `learned`, after which the watch never captures it: require a rate or framing match too, or keep capturing learned matches now and then (2026-11-08).

## Signatures: take back a wrong accept

An accepted signature cannot be undone (2026-11-11). `signature_reject` refuses anything but `proposed`, there is no `signature_delete`, and the store is read only at start. A mis-clicked accept on the page (a features-only 0.3 proposal) went on labelling every matching classify `learned <name>` in the transcript. Clearing it took moving the record out of `state_dir/signatures` and restarting the node.
- An op to retract an accepted record (to `rejected`, or a `retracted` state that keeps the provenance) and one to delete a record. Both unpin its captures unless another record holds them, drop its `learned_<id>` row from the registry, and are gated like accept (`allow.accept_signatures`).
- On the page: an Undo on the accept's note, and the signatures listed with Retract and Delete (behind its own tab or a hover box, not on the waterfall).
- Perhaps a confirm before accepting a features-only proposal, since its confidence is low and it labels by width and centre alone.

## Program rows on events

A program row whose `decoder.input.stream` is `event` (JSON lines of a running job's events, `frame` events included, on stdin), so an operator pipes a frame row to an external parser or handler, Modbus RTU being the operator's example (2026-11-06). Today only a plugin stage on a graph row takes event input.

## Architecture: a mode registry and a generic external-decoder adaptor

Open work left from plan 16's mode registry (delivered; see the plan series):

- **Feeds.** `sink` stage kinds for the standard feeds (APRS-IS, SBS/Beast, AIS NMEA, PSK Reporter, WSPRnet, KISS TNC) so results leave the node the way other tools expect.
- **Time.** UTC-slotted modes (FT8/FT4 15 s, WSPR 2 min, JT65 1 min) and TDMA (DMR, TETRA) need true block timestamps. The HackRF timestamp bug is fixed (plan 14); the 2 h clock run that would clear the WSJT family is still outstanding (plan series, "Outcome").
- **Trunking.** Span decoding is plan 31's (a watch with `modes`). Trunking (P25, DMR Tier III, MPT1327) needs a control-channel follower that starts voice jobs from control-channel events inside the capture; it plugs into the watch's known-channels seam.
- **Vocoders.** Codec 2 (M17, FreeDV) runs as the external `c2dec` (the operator's decision, plan 29; no copyleft dependency in-crate); AMBE/IMBE only through a handler on an external `mbelib` build, never shipped; HD Radio HDC off.
- **UI.** A flat menu of 100+ modes is unusable: pick modes through `knowledge_lookup` by band (what is plausible here) with a search fallback.
- **Wideband modes** (DVB-T 8 MHz, DAB 1.5 MHz, LTE) run the direct path at full rate and want the GPU FFT; park them behind the HackRF-first set.
- **`sync_bank` flapping** (from the 2026-10-27 investigation, not reproduced). A bank replaced while `feed_bank` still drains old-band blocks can swap back and forth, stalling a channel stage silently.

## Bug: a plain `pocsag_decode` never decodes on the debug-build bank at 2.048 MHz

Found in plan 16 task 7 (pre-existing): the debug-build bank path at 2.048 MHz does not deliver a decodable channel for a plain `pocsag_decode`, so the core identify tests run without the bank. Reproduce with the POCSAG fixture on a 2.048 MHz capture in a debug build, then check the release build before fixing.

## UI: `view set dir ltr|rtl|auto`

Plan 24 draws the frame, rail and ribbons with logical properties (`inset-inline-start` and friends), so a right-to-left page is mostly the `dir` attribute. Add the view policy (`auto` follows the browser) and check the canvas-drawn parts (axis ticks, ribbons' tilt) for left/right assumptions. Spec 4.9 of the console theme.

## UI: `view set time_flow left|right`

The waterfall's newest columns are on the right and ribbons drift left with the RF. A policy to flip it (newest on the left, drift right) independent of `dir`; ribbons, the rail's end strip and the squelch scroller read it. Spec 4.9 of the console theme.

## UI: view policies in localStorage

The theme and the side-column fold are held per tab, so a new tab starts from the defaults. Move the `view set` policies to localStorage (one choice shared across tabs), keeping the page's refill-from-live-traffic behaviour for everything else.

## UI: staircase transcript ribbons on the waterfall

From the operator, 2026-10-27. Plan 24 shipped the step-up rule (`crates/zeta-ui/web/overlays.js` `place`/`fallBack`, behind `RIBBON_STEP_DIR` and `RIBBON_STEP_MS`): a new chunk takes its band's line, the unkeyed ribbons it overlaps step one lane up over 160 ms, a ribbon that would pass `MAX_LANES` (8) or reach another job band's base lane clips under the newer one, and lanes fall back as the line clears. `RIBBON_STEP_DIR = 1` with `RIBBON_STEP_MS = 0` restores the old push-down.

Left:
- A tilted long chunk visually crosses the neighbouring lanes: the rotation is outside the lane maths, so a lane's height ignores it.
- A lane budget from the pixel height between this band and the next job's band, per footprint, so phone, laptop and a 4K wall give 2, 4 and 8 lanes rather than the constant `MAX_LANES`; the lower channel's staircase folds to fewer lanes rather than clipping.
- The staircase sharing lanes with keyed rows (subjects, `fold`): today a keyed ribbon takes the first free lane and never steps, and an unkeyed chunk on the line simply draws over it. A held roster line for keyed rows and a staircase for packet-shaped rows should share one lane budget (see plan 23 and "UI: state-stream decoders beyond the first subjects" below).

## UI: deferred minors from plan 24

Operator-visible nits from the plan 24 reviews, none blocking. The rail's label box is about 66 px and may clip the centre on long kinds; hot does not beat speaking when both overlap on the rail; thin rail tags under a full tag cannot be hovered; the frame spine stops at the waterfall bottom when the pane strip is open; a tilted long ribbon crosses neighbouring lanes visually; the ribbon `top` transition glides 160 ms on zoom; mixed ribbon heights space lanes unevenly; keyed redraws keep their old z-index; the graph headline truncates mid-number; the `.events` cell's tooltip goes stale; in plain, speaking and hot do not colour the row ground; the fold samples the mix's speaking state per 2 s render, so a short burst can be missed; a watched squelch-kind job with `squelch: false` re-asks `job_context` every poll; a stored `plain` theme flashes `console` for a frame at load; the selected pane tab, map and listen buttons reuse the speaking colour; the source health spans are rebuilt in unselected tabs. Tests: no scenario pins the end caps, the narrow tap-to-expand, the row-hover fold path, the ribbon colour or tilt, or hot/speaking across a theme switch, and the narrow `narrow_hides_pane` scenario keeps a now-redundant bare-string wait.

## UI: state-stream decoders beyond the first subjects

Plan 23 delivered the ADS-B request: a row names its subject (`key`, columns, `waterfall`, `heading`, `label`), the page folds by key, the Subjects tab lists one row per aircraft, the waterfall shows one line, and the map tracks per subject (`adsb_decode` has a `position`; plan 32 gave the map its own tab and the dock took the pane host, so the map half of this note is done). Left from the original note:

- ~~POCSAG has no subject~~ (done in plan 31: `pocsag_decode` has an addresses table keyed by address).
- AIS vessels (MMSI as key) when the AIS row lands; it needs only a row.
- The node-side half is a separate gist ("Subjects: a running job keeps its tables on the node").

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

Plan 29 shipped AIS, FLEX, M17, M20 synthesis and APRS positions (see "Decoder wave 2: leftovers from plan 29"). What stays:

- **DMR voice** (4FSK, AMBE+2 through the vocoder libraries, see the install-script gist above): an audio-output row on the shape of `m17_decode` with a `vocoder` handler.
- ~~**Whole-span POCSAG and APRS**~~ (done in plan 31: a `watch` with `modes`, and the `pagers_au` set).
- **Stereo** for `wfm_demod` (see the stereo gist above) now that audio rows exist to consume it.
- **DFM serial.** `dfm_decode` keeps `test: null` (it prints `skipped: no test`, and `--strict` fails it). Plan 29 task 9 built `crates/zeta-dsp/src/dfm_frame.rs` and the `dfm` family, and `dfm09mod --json --ecc` decodes their position, climb, time and satellites through the row's own chain, but the id stays `DFM-xxxxxxxx`: the serial's place in the sensor (configuration) field is not settled (channels 4-15, half layouts `half << 8 | index` and others, sequential or fixed-plus-alternating sensor sequences and runs up to 1200 frames all left the placeholder). The operator's ruling at the fallback was to leave `test: null` rather than a position-only test. Next: a public description of the DFM-09 configuration channels (the TH Koeln thesis's appendix A "From a Frame to a Data Packet" was not in the published excerpt), then set the row's `test` (`expect {id, lat}`, `within_ms` about 15000).

## Decoder wave 2: leftovers from plan 29

- **APRS objects and items** as subjects of their own (they are named, not placed, today).
- **An in-crate FLEX decoder.** `flex_decode` runs `multimon-ng` (the slot): the row turns into a graph row with the same schema and test.
- **M17:** packet-mode content; the 1600 mode's data bytes, and the 1600 mode against `c2dec`; a real M17 capture (no recording is in the repository, so the decoder has only been proven against its own synthesiser).
- **AIS:** message types beyond 1-5, 18, 21 and 24; per-burst AFC beyond about 1.5 kHz of offset; `gfsk_modulate`'s BT if the stage wanted 0.4 (it runs at 0.5).
- **Classify tie-break.** POCSAG off any row's centre loses its slot to AIS: maritime ties sort before pagers, so AIS takes the fifth `CLASSIFY_TRIES` slot. Break the tie on modulation or label, or try more rows.
- **Identify dwell cost.** Plan 29 added three dwells to every identify and to each signal a watch reports, tried before POCSAG and the rest: `m17_decode` scores 1.0 on every 12.5 kHz FM/FSK signal anywhere (its row has no `band.range_hz`) and dwells 5 s; `ais_decode` scores 0.6 on any FM signal 12.5-37.5 kHz wide and dwells 10 s; `flex_decode` dwells 15 s when `multimon-ng` is installed, and its band range, 151.8-932 MHz, makes it a candidate across the pager and UHF bands. That is up to about 15 s more per narrow FM signal, 30 s with FLEX in range. Narrow the ranges or shorten the dwells if watches feel it; an amateur-band `range_hz` for M17 is the operator's call (a spec change). Two smaller edges: `flex_decode` still scores where it is unavailable (no `multimon-ng`), costing a skipped slot; and `c2dec` is spawned with `env_clear`, so a snap or relocated install that needs its environment fails with only an exit status (its stderr goes to null).
- **A `vocoder` handler** for AMBE/IMBE on the M17 stage's shape (ties to the install-script gist).
- **Feeds out** for AIS and APRS (the Feeds gist under the sink architecture below).
- **The FLEX synthesiser** at 3200 and 6400 bps.
- **Deferred minors from review**, each small: `reload_modes` never probes new or edited rows (a new graph row with a missing handler fails inside the stage, not at `job_start`); APRS parser guards (13+ character garbage may read as a compressed fix, Mic-E accepts A-K in destination characters 4-6, items under 3 characters are accepted); AIS type 21 name extension joins only when the base is 20 characters; the DFM and M20 generators' small edges (`dfm_dat_field` never rolls the day, M20 altitude clamps negative to 0, fixed east velocity undocumented); M17 `rrc_modulate_4fsk` does not validate baud or alpha and address round trips drop trailing spaces.

## Access: ACLs on restricted rows

From the operator, 2026-11-06. Pager events and the rows that produce them (`pocsag_decode`, `flex_decode`) carry `access: "restricted"` now, and nothing reads it. The design to come:

- an access level per transport caller (the secret is one level today), or per secret;
- `events.log`, `--then-tail` and `stream_tail` of a `restricted` row gated to the higher level;
- the page's ribbons and the agent's view likewise;
- the level stated in `GET /health`;
- the Subjects tab's text column, which shows pager text today (plan 31's `pocsag_decode` addresses table).

Plan 29 landed the marker on both pager rows and `summary::envelope`, which inserts it on every event of a marked row. Plan 31 added a new place where pager text shows: the pager addresses table in the Subjects tab. Still nothing reads the marker.

Until then the README's Legal paragraph is the guard.

The radio seam (plan 33, `docs/superpowers/specs/2026-11-09-radio-selection-design.md` section 5) is where this lands for radios:
- `Selection.candidates` filtered per caller in `resolve`;
- `Selection::admit` is the agent's check, and the same check in `Node::apply_from` covers direct ops;
- transmit ops join `RADIO_OPS` as transmit and require the caller's TX level, and likely a named radio: `focus` never implies a transmit radio;
- `Capabilities.tx` is the radio's flag;
- the selection is appended to `ops.log` with the caller.

## Radio selection: later

From plan 33. Not built:
- `radio_prefer` by driver or role, which needs a driver in `SourceInfo`;
- a node-stored per-caller policy, replacing the browser's `zeta.radio_prefs` once users exist;

## Bug: `--then-tail` on a program row with a squelched demod tails the squelch events first

Found in plan 17 task 7. `--then-tail` now tails the first event output of the job; for an audio-input program row whose `nbfm_demod` keeps its squelch on, that is the demod's `squelch` event stream, which publishes before the decoder's events, so the tail shows `squelch` frames and the decode may land after `max_frames`. The sonde rows run the squelch off, so they are unaffected; an operator audio row that keeps it on is not. Tail the last event output (the decoder's) or the one whose frames carry `protocol`, and test it with a squelched audio row.

## Bug: a sink-bearing decoder is frozen as an operator graph

Found in plan 17 task 10. A decoder with a `sink_sondehub` node (added by the watch's `sondehub` param) is stored `operator_graph: true`, so it is not re-expanded at a different capture rate, and the sink-less fallback after a failed sink relaunch is stored the same way with no capture rate. Keep the row's expansion and re-append the sink on re-expansion (store the sink config beside the graph), so a retune does not leave the decoder at the old rate. Same area: decoders the watch re-adopts after a `graph put` keep their old sink config, and a batch buffered at a restart is lost.

## Bug: `gated_stop` is cleared unconditionally by a sink restart

Found in plan 17 task 10. `store_graph` clears `gated_stop` before relaunching the same id so the relaunch is not suppressed; a `replace` that names only the decoder during the sink restart's teardown can therefore let it relaunch after being stopped. Clear only the entry the restart itself set (a token compare), and make `replace_during_a_sink_restart` deterministic (it can flake when the proof lands before the gate is taken).

## Architecture: a generic `sink` param on the watch

Found in plan 17's final review. The watch's feed out is wired for one sink: `DetectParams` carries a `sondehub` field (zeta-dsp `detect.rs`), `graph.rs` checks it against the `SONDEHUB_STAGE` stage's schema, and `watch.rs` appends a node of that kind to every decoder it keeps under the same name. A second feed (AIS NMEA or APRS-IS for the wave-2 rows, SBS for aircraft) would copy all three places and add another field the detect stage only passes through.

Replace it with `sink: {kind, params}` (or a list of them) on the detect node: `graph::validate` checks `params` against the named stage's `params_schema` and requires an `event` input and no outputs, the watch appends `kind` with `params` fed by the decoder's events, and `sondehub: {...}` stays accepted as shorthand for `sink: {kind: "sink_sondehub", params: {...}}` so existing watches resume. The `sondehub.<field>` error naming becomes `sink.params.<field>`, and the sink-restart path (`place_decoder`, the sink-less fallback) works on any kind.

## Station: discipline the B210 to its GPS module

The operator says the B210 has a GPS module attached. The node's unit runs it as `driver=uhd,serial=31B92DD,gain=65,ppm=2.7` with no `clock_source` or `time_source`, UHD has never logged an internal GPSDO for this B210 (no "Detecting internal GPSDO" line in the journal), and no USB GPS is on the host. So how the module is fitted must be found out before the unit changes: an Ettus internal GPSDO board takes `clock_source=gpsdo,time_source=gpsdo`; an external reference into REF/PPS takes `clock_source=external,time_source=external`. Once disciplined, drop `ppm=2.7` and calibrate the HackRF against the B210 on a steady carrier: the 433.88 MHz remote read 2.6-3.2 kHz higher on the HackRF than on the B210, about 6-7 ppm.

## Detect: a per-device spur list

Plan 19.1 changed the detector (occupancy over the row median, a run-energy floor `energy_db` 30, `threshold_db` 6, a 4 kHz width floor, three sightings, at most 10 events a second), tuned on synthesised captures only: the B210 noise recording (the operator's own recording script, then the ignored tests in `crates/zeta-dsp/tests/detect_noise.rs`) was made 2026-11-03 and both tests pass. What is left: the B210's spurs (402.0245, 402.1505, 402.9005, 403.1494, 403.8996 MHz, 4.5 kHz wide at round offsets from the capture centre) still need a per-device spur list that `detect` masks (`spurs_hz` is the absolute half, empty by default), measured once per device by a survey over a quiet span and kept with the source record; the scan gist "the HackRF's spur grid" is the same list for the offsets-from-tune half. Also a tone-pair criterion for a narrow FSK sonde (4.5 kHz at 19 dB on 2026-11-01).

## Agent: requests too big for the voice console

Done: plan 21 (`docs/superpowers/plans/2026-11-01-agent-bulk-ops.md`), plus `jobs_stop` for "stop all jobs".

## Agent guide: layer-3 and UX additions

Items 1-3 of the first list shipped in plan 19.1 (the register and `as_of`, the low-duty rule, the unknown-signal playbook with its legal line: "Exploring an unknown signal"). Still open: (5, from the operator, 2026-11-01) expand this guidance as each mode and layer-3 component lands, including when not to touch running jobs; (4) lead the operator with the map and the one-line `summary`, name channels and licensees in plain words, and offer the next investigative step (measure, identify, knowledge lookup, direction-find). Plan 19.2 and 19.3 extend the playbook with bits, frames and signature write-back.

## Station: a GPS fix from the B210

The node-level station (`station set/get/clear`) shipped in plan 19.1. What is left is a source of truth for it: the B210 carries a GPSDO option whose UHD sensors (`gps_locked`, `gps_gpgga`, `gps_time`) are reachable through SoapySDR's sensor API from the worker. Wanted: `source_list` shows `gps` (`locked`, `lat`, `lon`, `alt_m`, `satellites`) for a source that has one, and `station set --from-source ID` takes a locked fix as the station (marked as from GPS; hand-set wins over an unlocked fix, a locked fix updates a position marked as from GPS); the clock discipline is under "Time, trunking, span".

## Bug: a decoder the watch placed on another free source keeps running if the watch never recovers

Found in plan 20's reviews. With `decode_on: free` the watch puts a decoder on a second source; when the watch's own source faults and the watch fails, its decoders are left to fail on their own (plan 20 spec 4.7), but a decoder on a healthy other source keeps running with nothing to stop it, and recovery re-adopts it only if the watch heals. Decide whether a watch that stays failed (the fault outlasting some bound) stops the decoders it placed elsewhere.

## Bug: `source_ensure` and `source_pin` refusals from a faulted source lack the `source <id>: ` prefix

Found in plan 20's reviews. `job_start` and a job's failure read `source <id>: <fault>`, but `source_ensure` and `source_pin` still answer bare `io: <fault>`. Route them through the same helper `job_start` uses.

## Worker: a parent-side status-silence watchdog

Found in plan 20's final review. The worker writes its status every second; the node never notices when it stops. A driver deadlocked under the worker's status task (or a worker wedged with its stdout open) stalls silently, as it did in-process. Wanted: the supervisor faults the source with `soapy worker for <id>: no status for 10 s` and kills the worker, which the respawn path then handles like any other end.

## Worker: `heal_settled` clears a later fault unseen

Found in the review of 7e5bfc8. `heal_settled` clears whatever fault is held, so a further crash and hello inside the gap between `wait_ended` and the call is cleared without its jobs having been seen. A generation on the held fault would close it. It needs a sub-second respawn, so it is not reachable on real hardware today.

## Sources: one helper for the source record update

Found in plan 20's final review. `note_capabilities` duplicates `Node::set_source_record`'s body (load, change one field, write atomically, keep the in-memory copy). Fold both into one `Inner` helper that takes the change as a closure.

## Tests: a shared fixture module for the node tests

Found in plan 20's final review. `tests/recover.rs` copies about 150 lines of fixtures (ports, configs, registries, wait helpers) from `watch.rs` and `attach.rs`. Move them into a `tests/common` module the three share.

## Node: `watch_healed` starts before `open` has resumed the jobs

Found in plan 20's final review. `watch_healed` is spawned in `add_port`, before `open` runs its own resume, so a heal during `open` runs its recovery concurrently with that resume. The source's gate serialises the two, so it is harmless today; start the watcher after the resume (or have recovery skip what the resume relaunched) so the order does not depend on the gate.

## Worker: control frames queue behind IQ blocks

Found in plan 20's final review. The worker's writer thread takes frames in order, so a reply or a status can wait behind up to 64 queued IQ blocks (at 20 Msps, tens of milliseconds and megabytes). Give control frames priority (a second queue the writer drains first) or cap the block queue in bytes rather than blocks.

## Agent: `graph_put` and `job_retune` change an operator's job without the allowance

Found in the plan 21 final review, 2026-11-01. `graph_put` and `job_retune` can change an operator's job, or in effect end it (a graph that fails, a retune off the signal it served), without the allowance: the spec gates only `job_stop` and `replace`. Recorded as a slot: a gate for changes to jobs the run did not start, if the agent is ever seen doing this.

## Mix: mutes do not survive the mix, the page offers fewer than it hears, a phone is untested

Found in the plan 22 final review, 2026-11-01. Mutes live in the mix's graph, so stopping the mix (`stop all` included) forgets them. The page offers Mute in mix only on the demodulator kinds, though the mix hears any job with audio. And how the page's audio behaves on a real phone is untested: the resume and the re-anchor on a suspended context were only run against a context suspended in headless Chromium.

## Subjects: a running job keeps its tables on the node

From the operator, 2026-11-06: "whilst a job is running then the relevant tables should be updated and maintained", whether or not a page is open. Found that morning: the seven ACARS decoders on the B210 ran all night (00:02 on) and decoded fine (CRC-clean blocks such as `C-FGEO` / `AC0035` on 131.450 MHz reached the job streams and the b210 feed), but events are live-only (the bus hands each frame to whoever is subscribed then; nothing in the state directory keeps them), so with no page open the night's traffic went nowhere and could not be reviewed.

Plan 23 merges each subject's events on the page, so a refresh starts the Subjects tab, the tracks and the dim and drop clocks empty and refills them from live traffic (an aircraft re-announces within seconds; a sonde every second, an APRS station only when it next transmits). Wanted: the node maintains each running job's tables itself, the subject state as `job_context.subjects` (the merged state by key, first and last seen, count, the track) and, for rows with no subject (POCSAG, RDS), a bounded recent-events list; persisted with the job so a node restart keeps them, cleared when the job is stopped (or kept as a closed record: decide). The page then seeds the Subjects tab from the node and follows the feed, so a refresh, a second page, the agent and the CLI ("what did ACARS hear since midnight?", "which aircraft are up?") all see the same picture. The cheap first half is done (2026-11-06): the node tees every source's feed to `events.log` in the state directory (one JSON line per event with the job, source, tuning and the event; rotated past 32 MiB) and `zeta events tail [--job] [--source] [--protocol]` reads it, so the night's traffic can be reviewed after the fact and real events can be lifted as fixtures. Still wanted: the node-held tables above, an op (and so `--node`, the page and the agent) that reads them or the log, and where the dim and drop settings live.

## Subjects: leftovers

Rulings of plan 23 an operator may want otherwise:

- `m_s` shows as signed m/s to one decimal (the plan text said km/h; metres and m/s are the metric pair, and a climb rate in km/h misreads). `ft` shows as metres and `kt` as km/h. One line in `metricText` (`crates/zeta-ui/web/units.js`) changes it.
- The table is stacked under the map on the wide footprint, since seven or eight columns do not fit a quarter of the screen.
- Jobs with an `owner` (a watch's runs and their trials) and a running identify's current trial have no subjects; their events keep today's ribbons. A proved decoder left running is an ordinary job and has them.
- Two decoders tracking one aircraft give two rows and two markers: subjects are per job, not merged across jobs.
- The row cache (`store.lookup`) lives for the page: an operator row edited on the node needs a page reload.
- The agent's own jobs: `mix_start` ids are not recorded as the run's own, so a run cannot stop a mix it started (the stop gate was not widened).
- Tracks keep a fix when the subject has moved `track_min_m` (or turned over 10 degrees) and, at `track_points`, thin the older half by every other point (the first point stays). Keeping sharp curves when thinning old points is a future sprint: today a thinned half can cut a corner, and a long straight leg and a tight turn lose points alike.

## Layer 1 and explore: deferred minors from plan 19.1

For an operator, not blocking: the burst gate's `on` reference is the 15th-percentile chunk, so one impulse over a continuous signal gives duty 0.001 (wants a minimum on-time guard); an 8 kHz noise window averages only 7 spectra and reads `unknown` (the noise rule should scale its margin with the spectrum count); the AM tone reads its tone frequency as a baud; the `am` rule reads the whole block's cv, so a keyed constant-envelope burst train reads `am` (it should read the on-runs' cv); `clusters()` has no prominence or merge, so ripple could read a 2-FSK as 4-FSK; CW/Morse (gated low-rate OOK) never reaches the `ook` rule; the register's space class is not limited to earth stations (the RRL has no flag); a rebuilt bursts index loses pins (and the same open may expire a former pin); the detector's Pacer can add latency under a flood of tracks; `unregister` on the page does not re-render the Explore pane (19.2); the B210 noise recording is pending the operator.

## Lease: cap a Soapy source's capture at the rate the device can stream

Done by plan 28.1: the worker probes the rate ladder at open, `max_rate_hz=` asserts a cap, and a request wider than a measured cap is refused `insufficient bandwidth`.

## Calibration: a set of receiver and antenna calibration tools

From the operator, 2026-11-03, after UAT 19.1.A was done by hand. Two tools wanted as ops (and a page menu), not scripts:

- **RX calibration on a 50 Ω load.** What the operator's noise-recording script does today, as a node operation: with the operator's word that the input is terminated, record the receiver's own noise over a band at the operating gain, keep it as the source's noise baseline (per device, per gain and rate: the floor in dBFS per bin, the DC spike's width, the spur list the "Detect: a per-device spur list" gist wants), and let `measure`, `detect` and the page read against it (a signal is what stands over the device's own floor, a spur is masked by name). A second recording with the antenna on is the positive control; store both with the source record and show `calibrated_at` in `source list`. The B210's floor was recorded 2026-11-03 at 65 dB over 403.025 MHz / 6.144 Msps (`~/zeta-fixtures/b210-noise-403m`), the detector announced nothing on it; that is the baseline the tool replaces.
- **A VNA-style sweep over a TX/RX port pair.** For a device with a TX port (the B210, the HackRF), sweep a tone across a band from TX into the device's own RX (through the antenna under test and a coupler or a reflection bridge, or TX to a second RX port through the cable) and plot return loss / insertion loss against frequency: antenna resonance and bandwidth, cable loss, filter shape. Needs a TX path the node does not have yet (`tx: false` in every capability record): a `tx_tone` stage and a `sweep` job that steps both tuners together, a TX-safe gate (power limits, a band list the operator allows transmitting on, a dead-man that stops TX when the job ends or the node exits), and a `calibration` result stored with the source (through, open, short, load references when the operator has them). Report in dB against frequency on the page (its own tab, per "panes earn a tab"), and as a `sweep` event stream for the agent.

Both are the operator's tools, run on their word (an `allow`-class gate for TX). Specs are to be scheduled; the measured facts from the 19.1.A run (the ring holds just under 10 s, a pin moves the radio where an ensure does not, the 5 % edge mask hides a signal 1 MHz from a 2 MHz capture's centre) go in the first one.

## Watch: the identify dwell misses a marginal sonde the detector now sees

Seen on the B210 2026-11-03 14:10 AEST during UAT 19.1.D: the watch's detector raised RS41 `X1342539` at
401.5001 MHz (5 kHz, 4800 Bd, 15 dB, 130 km away at 15.8 km up) as `new`, so plan 17's miss is fixed, but
its identify ended `unknown` and, with `on_unknown: capture`, spent the run's 32 captures on a signal a
decoder row proves. An `rs41_decode` started by hand on the measured centre got 3 frames in the first 10 s and
none in the next 20: at that strength a frame comes only now and then, so the row's 5 s `identify.dwell_ms`
sees none. Also, the hand decoder took the signal out of the watch's list (a job on its band), so the watch
never re-tried it. Options, to spec: (1) a dwell that grows with the retry (`tries`): 5 s, then 15, then 30;
(2) when layer 1 reads a sonde-shaped signal (4800 Bd, 4-6 kHz, in 400.05-406 MHz) run the sonde rows for a
longer dwell before the capture, since the capture is what an `unknown` costs; (3) classify the captured
windows with the program rows too (they are `skipped_span` today at 46 ms, but a sonde's 640 ms windows
hold frames). Beside it, `capture classify` on a 46 ms window of the 404.025 MHz telemetry link ranks three
catalogue radiosonde names at 0.7 above the layer-1 `unknown` 0.5 on the frequency range alone: cap a
range-only catalogue match under the layer-1 label, or say "range only" in its evidence.

## Channel bank: a capture under 2 Msps gets none, so 80 CB channels run direct at 5 cores

Seen in UAT 22.1 on the field host 2026-11-03: the UHF CB pin (476 918 750 / 1 024 000 Hz) is under
`PFB_MIN_RATE_HZ` (2 Msps), so `sync_bank` makes no bank and every one of the 80 `fm_demod` channels took
the `direct` path (`job_context.channel.reason: no bank`). Counters stayed clean (underruns 0, dropped 0,
rate held) but the node sat at 498 % CPU on 12 cores for 80 demods and 80 transcribes, the host 45 % busy.
The floor was chosen when a bank's fixed cost outweighed a few direct channels; with 80 consumers it is the
other way round. Options, to spec: make the bank when the capture is narrow but the consumer count is high
(a bank per `plan_start` of 20 or more channels), or lower the floor to the rate at which a 500 kHz bin
still fits the plan's channels. Measure both on the B210 CB pin before choosing.

## Mix: the page's audio stutters under channel load while every node counter reads 0

UAT 22.1 on the field host 2026-11-03: Listen to all on 80 CB channels stuttered on the operator's laptop; with
channels 41-80 stopped (40 left, the node from 498 % to 260 % CPU) it still stuttered, "nowhere near as
badly". The mix's `underruns` and `dropped_ms` stayed 0 throughout and `measured_rate_hz` held, so the
node's accounting cannot see it. What the code says: the mix emits a 20 ms frame per tick and nothing in
silence; the page (`audio.js`) schedules each frame `LEAD_S` (100 ms) ahead and, when its queue runs dry,
re-anchors at now + 100 ms without counting it, so any delivery hiccup over 100 ms (a tokio stall while 40
direct channels each filter the full 1 Msps capture, a late WebSocket flush) is a silent gap. Three things,
to spec: (1) count the dry re-anchors in `window.zetaAudio` and show them in the mix's row, so the operator
can tell a starved page from a starved mix; (2) an adaptive lead: start at 100 ms and grow by the gap seen,
to 500 ms, falling back when quiet for a minute; (3) the node side: the bank under 2 Msps (its own gist)
and a cheaper idle channel. The operator's question: "is there a way to reduce CPU resource contention on
idle channels". The channel filter at the capture rate is the cost, not the demod: with a bank a closed
squelch could skip the demod and the resampler and read its gate from the bank bin's power alone; on the
direct path there is nothing to skip before the filter. Measure the page counter first: if the page never
runs dry the gap is in the browser's own output and (2) is the fix.

## Agent: the language for the node's own model to work out automations itself

From the operator, 2026-11-04, after a session spent decoding their 433 MHz OOK remote by hand: "create the language needed to allow the supplied llm inference back end to figure some of these automations itself, with the view that those backend llms will become more capable as time passes." The session's loop was done by the coding agent from outside, step by step: pin a radio off the signal's centre, arm a short burst capture on a band, wait for the operator's press, read the windows' features, start `bits_start` (giving the modulation and rate layer 1 got wrong), tail the bits, decode whole frames, compare two radios on the same press, notice a fault (a refused band, a 1600 Bd bit-rate read, an `fsk` label) and route around it, re-arm. None of that is a step the node's own model (`zeta ask`, `POST /ask`, gemma-4-12b on the field host today) can do: it has single ops and the field-craft prose, not the vocabulary for waiting on an event, for a loop of "arm → wait → measure → decode → report", for comparing results across sources, or for knowing a step's result is suspect.

What "the language" should give it, as a direction (not designed yet):

- Waiting on events as ops, so a model with a small turn budget can say "until 3 windows land or 60 s" instead of polling: a `wait` op over a job's context or an event stream with a condition and a timeout.
- Composable recipes (see the next gist) that a model can write, store and run by name, so a sequence worked out once (the press-capture-decode round, the two-radio comparison) becomes a step the next run calls rather than re-derives; the model proposes, the operator accepts, as with signatures in plan 19.3.
- Results that say how far to trust them (a layer-1 rate with its source, `runs` or `cyclo`; a decode with its frame count and integrity), so a model can tell a good answer from one to double-check, the way the session caught the 1600 Bd read.
- A graded surface: the same vocabulary works for today's small model through a few high-level ops and recipes, and for a stronger model through the low-level ops and graph editing, so the node does not need rewriting as models improve; the `allow` gate stays the operator's line for what any model may change.
- The field craft as the playbook a model reads (it is already AGENTS.md), extended with worked sessions like this one written as recipes rather than prose.

Prerequisites already in TODO: recipes (below), the rtl_433 live decoder (spec draft `docs/superpowers/specs/2026-11-04-ism-remotes-design.md`), the layer-1 OOK fixes. Brainstorm with the operator before a spec.

## Architecture: fan-out, and the transcriber as part of its demodulator's graph (towards a ComfyUI-style editor)

From the operator, 2026-11-05. A future sprint makes graph editing on the page a real node editor. Two engine facts stand in the way today. (1) No fan-out: `node_outputs` (`launch.rs`) gives an output kind to one leaving edge, and an output with an edge leaving it is no longer a job output, so `channel → nbfm → denoise → vad → transcribe` in one graph would take the demodulator's audio away from the mix and the page. The bus is broadcast, so letting one output feed several edges *and* stay a job output is a small change, and it is the primitive a node editor needs anyway. (2) An operator graph launches as stored: a merged demodulator could not be retuned (`job_retune` refuses an operator graph) until re-expansion works on arbitrary graphs (band-derived params as templates, the carried-param rule generalised). What a merge gives up versus `job_attach`: starting or stopping transcription without a `graph_put` restart of the demodulator (an audio glitch, a mix drop), per-stage readiness at attach time, and `plan_start`'s `attach` becoming a graph template. What it does not change: the page reads the source's feed, one socket whatever the job count, and each stage keeps its own event stream; the ribbon problem is solved by the per-source feed above, not by merging. Suggested shape: keep the attached job as the runtime unit (a `stream_in` is already an edge across job boundaries), add fan-out, and let the editor show an attached job inside its input's node group (the page already draws its ribbons at the input's tuning in the input's colour). The layout half of the same direction (the operator, 2026-11-06, plan 32): the page's views and panes as things the operator places and sizes on a canvas; the `main` (RF view or Map in the main area) and `dock` (panes under the main view or in the right column) view policies of plan 32 are the first two "where does this view go" switches, and a canvas layout would subsume them. Both shipped in plan 32 (`view set main rf|map`, `view set dock auto|bottom|right`, a theme's `layout.dock`).

## Plans: named channel groups (`lower 40 voice`) for `plan_start`

From the operator, 2026-11-05, after two CB transcribe setups in one day. `plan_start` takes a plan and a list of channel numbers, so every setup of "the lower 40" is the agent (or the operator) typing `[1, 2, ..., 40]` and remembering to leave out 22 and 23 (data, no voice: the plan's `notes` already say so, nothing reads them) and, on the upper raster, 61-63 (reserved). Today both runs started all 40, so `fm_demod` + `transcribe` ran on the two telemetry channels and transcribed their bursts as `""`, `Yeah.` and `Uh` (ch22 was the busiest transcriber on the plan, 34 `short` in 20 minutes), and ch24 was cleaned up by hand.

Wanted: a plan carries named `groups`, each `{id, name, channels}` expanded like its segments and served by `channel_plans` (`uhf_cb_au`: `lower40_voice` 1-40 less 22, 23; `upper40_voice` 41-80 less 61-63; `voice` both; `repeater_outputs` 1-8/41-48; `data` 22, 23), and `plan_start` takes `group: "<id>"` as an alternative to `channels` (one of the two, the other `null`); `jobs_stop` with a `plan` could take the same `group`. The page's plan menu and the agent guide name groups, not lists: "transcribe the lower 40" is `plan_start` with `group: lower40_voice`, nothing enumerated and nothing forgotten. A recipe (below) is the general case; a group is the one-field special case that fixes the daily CB setup now, and its ids are the vocabulary a recipe would use.

## Architecture: recipes the node stores and runs by name

From the operator, 2026-11-02. A job setup (all ACARS channels on one radio, the CB plan with transcribers, the sonde watch with a pre-placed decoder) is reproduced from text each time: field craft prose, the scratch scripts on the field host, now `RECIPES.md`. The node cannot tell "the ACARS set" from seven unrelated jobs, and an agent re-derives the ops every run. A recipe should be a thing the node stores next to pins and plans (one JSON file each, `recipes/*.json` built in plus an operator directory as for mode rows): a name, the preconditions, the ops in order, and a start, a status and a stop by name (`recipe_start`, `recipe_status`, `recipe_stop`), with `plan_start` as the one-step special case it generalises. `RECIPES.md` is the stop-gap and the spec material: its JSON blocks are the ops a loader would lift unchanged.

Plan 31 delivered the named set (`set_start`, `set_stop`, `set_list`; `sets/airband.json`, `sets/pagers_au.json`, `--sets-dir`), which covers what this gist asked for in the way of starting and stopping a group of members across radios. What stays: pins, mixes, waits, verify steps and ordered arbitrary ops in one stored recipe, with `set_start` as one step of it.

## Architecture: long-running jobs on narrowband tuners

From the operator, 2026-11-07. Long-running jobs (the airband set's ACARS, a pager watch) move to additional narrowband tuners (HackRF, LimeSDR), freeing the wideband radio for exploring. The long-term goal is a tap off an FPGA polyphase channel bank over a very wide band. Sets already name roles, not radios, so a set file does not change when the radios do.

## Node: a baseline of the host's capabilities

From the operator, 2026-11-06, raised while scoping plan 28 (analog video): a video job wants the B210's 30.72 Msps rung, a demodulator at that rate and a frame encoder, and whether the host can carry that beside 39 CB transcribers, the speech daemon and the agent's model is known only by trying. Wanted: the node measures and reports the host it runs on, once at startup and on request (an op, `zeta host`, `/health`): CPU model, cores and threads, memory, the state directory's storage (free space and a short read/write speed probe), a spare GPU and what already uses it, the attached radios and how each is connected (USB 2 or 3, the rung that bus can sustain, network for a KiwiSDR or another node), the speech backend's resource use and how many transcribers it can run at once, LLM inference (which model, where, tokens a second), and the network reach to off-host components (the speech daemon, the LLM, the register, another node, latency and bandwidth); the RF LLM and FPGA hardware when they are folded in. The point is a capability table the lease, the watch, `plan_start` and the agent can consult before committing ("this host can run N transcribers and one video job"), and a line the operator can read before a deploy. Spec first; it touches every crate that owns a resource.

## Plans: VTX channel tables from the open VTX firmware

From the operator, 2026-11-06, while scoping plan 28. The 5.8 GHz FPV channels are letter-named (A1-A8 descending, B on 19 MHz steps, E irregular, F, R on 37 MHz steps) and the plan registry numbers channels on uniform steps and matches a job within 1 kHz, so plan 28 ships no video plan. Later: lift the channel tables from the open VTX firmware (OpenVTx's and Betaflight's VTX tables, which carry every band and power level the transmitters themselves use), as `fpv_58` and `fpv_13` rows with letter names, and give a plan a match tolerance wide enough for a job centred on a measured carrier (plan 28's rule) to take its channel's name. `plan_start` on a video plan is not wanted.

## Bug: `source_list` reports the startup ring length after a retune

From plan 30's live acceptance, 2026-11-07. The B210's ring was sized for 10 s at the startup capture of 3.84 MHz. Pinned at 10.24 MHz it held about 3.5 s, yet `source_list` still reported `ring.seconds: 10.0`, and `iq read` of 9 s answered `too_old`. Either resize the ring on a retune (it is a byte budget, so seconds shrink as the rate grows) or report the seconds the current rate actually holds.

## Capture: the burst gate does not suit VDL2 on the B210

From plan 30's live acceptance, 2026-11-07. A `capture` job on 136.975 MHz, 25 kHz fired on spikes of about 1 ms, wrote 32 windows in about 20 s, then stopped at its burst cap; none decoded. The VDL2 fixture was recorded off the ring instead (an `iq read` of the last 3 s on each `vdl2_decode` event, channelised offline). Spec 4.6 assumed the gate would serve VDL2. Look at a minimum burst length for the gate and at "record on decode" as a capture mode.

## Station: the B210 hears little in airband

From plan 30's live acceptance, 2026-11-07, at midnight local: no visible carriers at 136-138 MHz, no ACARS frame in 30 minutes on seven channels, one strong aircraft on VDL2. Night traffic explains part; the antenna or its coverage at airband is the other suspect. UAT 30.2 needs a daytime rerun.

## Map layer: deferred minors from plan 32

Plan 32 (origins from the register lit on a Map tab, the dock, a Kiwi's position, map tiles) shipped without these:

- Positions for `zeta-source-node` (forward the remote node's station as its position) and for VRT sources: today only a Kiwi's `/status` or `--source-position` gives a remote source a vantage, and a lookup on any other remote source answers `from` null.
- Two strip panes at once (the Map beside the RF view), and a resizable dock: the dock is a fixed share (`DOCK_SHARE_DEFAULT`), switched between the bottom and the right column only. See "Page: resizable side column and bottom pane" for the direction.
- Fuzzy matching of decoded names against licensee names: identity confirmation matches a callsign exactly, so only an APRS-like `src` confirms a site.
- Demotion rules for one-transmitter services (a broadcaster with one licence on one site scores like any fit): a service-aware term in the origin score.
- The Explore table's licensee column still reads `allocations` (the nearest transmitter), not `origins` (the best-scored site), so the two can name different licensees for one capture.
- The CALL_SIGN probe never resets: a register upgraded to a build with the column needs a node restart before callsigns appear.
- The tile notice goes quiet after a partial failure, and a provider that serves blank tiles raises none: a server answering 200 with a placeholder image (CARTO's ~2 KB "API KEY REQUIRED" tile, seen on the field host 2026-11-08, which is why CARTO was dropped) is a `tileload`, not a `tileerror`, and `naturalWidth` is 256 either way. A check would sample a loaded tile's pixels (a canvas read, which needs CORS-enabled tiles) or its byte size (a fetch, outside `img-src`).
- The console theme's kept tab is only un-dimmed, not restyled as the other themes do.
- The dock-head Subjects count is the store count (every subject held), not the rows the table shows after its filters.
- The RF thumbnail (spec 4.12, Task 12, dropped because the e2e suite ran about 570 s against its 690 s deadline): a small live RF view in the map's corner (`RF_THUMB_W` 240 x `RF_THUMB_H` 120, `RF_THUMB_FPS` 2) drawn from the source's spectrum feed, a click returning to the RF view. It wants its own scenario and a timer, so it waits for a faster suite or a deadline change.

## Watch: deferred minors from plan 31

Field risks:
- A clean FSK pager may read as two narrow tones under the span width floor, and a single tone of about 3 kHz fits nothing until its next `new`. A strong page reads 19-25 kHz, which the tolerant 0.5-2x fit now takes; a fit over the widest or median width across the persist window would be steadier.
- The detector's DC mask hides a carrier on the capture centre.
- A seeded decoder that decodes but never gets a detector `new` is lost at `hold_ms` (spec-faithful).

Small:
- `known.register` cannot say `unreachable`: `KnowledgeProvider::allocations` has no error channel, so `job_context.watch.known` holds counts only.
- The ACMA provider caps a query at 200 rows before `band_assignments` dedupes.
- A seeded decoder's centre is the known centre until the first `new`.
- A repeated `--radio` role on `zeta set start` collapses silently: the last mapping wins, with no usage error.
- Set and plan groups on the page share sessionStorage fold keys, so a set and a plan with the same id share one fold state.
- A failed `graph_put` after the page's Decode across leaves a plain watch running.
- The unready-attach fallback in `set_start` swallows any other attach error when the stage is also unready: the member starts without attach and only the readiness reason is warned.
- Re-adopted decoders' signals are not tracked (`Seed` does not persist `signals`), so after a restart an operator stop of such a decoder does not requeue its carrier until it goes and comes back.
- A decoder's recorded signal follows the detector's latest centre while the decoder stays tuned where it started, and the cooldown after it uses the drifted centre.
- A set file's `radios` order is the file's order on the node, but `set_list`'s JSON is a BTreeMap, so the page's picker orders roles by first use.
- `pick_source` never considers `replace`: a source held only by a `replace` job counts as busy.
- The menu's `modes_list` is cached per page load.
- Set -> watch -> decoders nesting on the page has no automated test (UAT with `pagers_au`).
- The watch row sits inside `<summary>`, so it toggles on click and nests buttons in a summary (accessibility).

From the field host's pager hour (UAT 31.a, 2026-11-09):
- A span watch's `unknown` and `decoding` events carry empty `allocations` and `origins` even on a register-assigned frequency (149.1 MHz in the run): no identify runs, so nothing asks the register. The watch already holds `known.assignments` for the band; the events could name the overlapping ones.
- A `lost` event names the signal's latest centre (the detector's drifted one), not the frequency the decoder is tuned to, so it cannot be matched with its `decoding` event by frequency. It should carry the decoder's tuning, with the signal's centre beside it.

## Modes: several ranges per row, and centres-only rows tried anywhere

From plan 31's post-deploy fix wave (2026-11-09), where identify on the sonde band tried `flex_decode`, `ais_decode` and `aprs_decode`. Those three now carry a `band.range_hz`, but a row holds one range, so `flex_decode` dropped the catalogue's 927.3-936.6 MHz band from identify (job_start still runs it there). Other rows with centres and no range are still tried by identify anywhere a signal fits their width: `pocsag_decode` (148.6375 and 466.075 MHz), `acars_decode`, `vdl2_decode`, `adsb_decode` (only where the band is wide enough). Ideas: `range_hz` as a list of ranges (POCSAG 138-175 and 450-470 MHz, FLEX VHF and UHF, APRS 2 m and 70 cm), then give those rows their ranges. The watch's `identify_band` also ignores `in_range` when it picks a band width (harmless today: an out-of-range row scores lower).

## Jobs: test runs push the operator's stopped records out of the job history

From the 2026-11-08 live acceptance. The node keeps the 20 newest job records (`JOB_HISTORY_MAX`, `crates/zeta-core/src/node/mod.rs`), so 16 stopped test jobs evicted 16 of the operator's older stopped records (a 477 MHz demod with its transcriber and mix, FM demods, RDS, measures). Ideas: a larger cap, keep records the operator started apart from agent or test ones, or keep one record per (kind, band) so a re-run replaces its predecessor instead of a stranger's.

## Modes: further ACARS position layouts

Plan 34 reads the H1, 80, 16, 3L and QQ position forms from a downlink's text. Not yet read: the `RSI` table form (signed decimal degrees under a `LAT LON` header), and any further airline layouts that turn up in `events.log`. No dumphfdl fixture yet carries a text report, so HFDL's derive is untested end to end.

## Page: resizable side column and bottom pane

From the operator, 2026-11-10 (UAT 32.5). The right-hand column and the pane under the map or waterfall (Subjects, Bits, Transcript, Jobs) should both be resizable by the operator. Today all three sizes are fixed: the column is `--side-w` (360 px, a theme token), the right dock is `--dock-share` (40% of the column), and the bottom pane is a literal 40% of the main view's height in `style.css`, not even a token. A wide table such as the ADS-B Subjects table scrolls sideways in the column, and nothing on the page widens it.

Direction: the sizes are the operator's layout, not a theme's look, so they must not mean editing every theme. Express each as a share of the screen: the side column as a percentage of the page's width, the bottom pane as a percentage of the main area's height, and the right dock as a percentage of the column's height. Set them by dragging the boundary, with a `view set` for each (so the agent and the CLI can set them too), kept with the other view policies. A theme may still give defaults; the operator's setting wins. Clamp each to a usable range (neither the main view nor the pane may vanish), and keep the narrow footprint's own layout. This is the sizing half of the canvas-layout direction in the node-editor gist.

## Modes: MIAM and ARINC 622 on plain ACARS

From the plan 34 UAT (2026-11-10). A plain-ACARS downlink with label `MA` carries a MIAM-compressed payload, which often wraps an ARINC 622 message: an ADS-C position report, or CPDLC. dumpvdl2 and dumphfdl decode these through libacars, so their events get the ADS-C fix; the in-process `acars_decode` stage publishes the compressed text as is, so the same aircraft's position on plain ACARS is lost. Ideas: run libacars on the block (its MIAM and ARINC 622 decoders; behind a `cfg` as `soapy` is, check its licence), or a Rust MIAM core decoder (the framing plus deflate) feeding the ADS-C shapes the VDL2 row already reads. Either way the decoded basic report fills `lat`, `lon` and `alt_ft` as structured fields, so it wins over the text derive of plan 34.

## AIS: not yet proven on air

From the 2026-11-08 live acceptance: 15 min on the HackRF (indoor antenna) and 11 min on the B210 at 161.975/162.025 MHz gave no events; a ring cut showed one 25 ms burst about 12 dB over the floor in 9.5 s, which did not decode. The channels are nearly empty at the field host, so this says little about the decoder. Decode the kept cut with another AIS decoder offline, and retry from a site in view of shipping.

## Feature: SatNOGS support

From the operator, 2026-11-07. SatNOGS (Libre Space Foundation) is the open network of satellite ground stations. SatNOGS DB holds the transmitters (frequency, mode, baud) and the TLEs. SatNOGS Network schedules observations onto stations, and `satnogs-client` with its `gr-satnogs` flowgraphs records each pass and uploads the waterfall, the audio and the decoded frames. Three layers, smallest first:
- **The DB as a catalogue.** Read transmitters and TLEs the way the register is read (`knowledge_lookup`), predict passes for the station, and run a pass job that follows Doppler by retuning its channel inside a held capture. Nothing tracks a moving carrier today; the lease already lets a channel move within its band.
- **Decoders as rows.** The common amateur downlinks: AFSK1200 and G3RUH 9600 AX.25 beside the APRS decoder, GMSK, CW beacons, and NOAA APT from the digital-modes gist, fed by the pass job.
- **The node as a SatNOGS station.** Either take scheduled observations from the Network API and upload the artefacts and frames (an API token and the station id in config, like the SondeHub feed), or run `satnogs-client` against the node's source as an external program.

Antenna caveat: the field host's 2 m / 70 cm vertical hears strong LEO passes only. Spec first; layer 1 is a plan on its own.

## Worker: a deployed worker's restart count ran into the thousands

Found by the sweep spike on the field host's B210 (plan 36, spec section 8, item 5): the deployed node's worker record for the radio showed 2395 restarts. The cause was not looked into. Read the worker's own stderr lines in the journal (prefixed `zeta-soapy-worker[<id>]: `) and the node's `soapy worker for <id>:` lines to see whether the count is a driver crash loop, the clock guard's stream restarts or a respawn loop on a faulted device, and when it climbs; plan 36.1 ships the worker's new protocol and ring, so measure the count again after the deploy first.

## Soapy: the reader's read length while a scan holds the source

A tune waits up to one read of the Soapy reader (10 ms: `max(16384, rate/100)` samples) before it can run, and a scan tunes every few tens of milliseconds. A shorter read while a time-shared lease holds the source would let the tune in sooner, but each shorter read costs a ring slot (a slot holds a whole block whatever its size), so the ring fills sooner. Measure the sweep with the read at 2 and 5 ms against the ring's drops before changing it.

## Sources: widen a held capture by dropping its depth

What was asked: a source's capture held by narrowband jobs widened by dropping its sample depth to a lossy one. The depth belongs to the whole stream, so every holder would get the coarser samples, and a sweep of the extra band would tune away from the jobs. The form that works is one wider capture at a depth that sustains it, with the jobs fed through the channel bank. It waits on plan 36.1's live numbers (`UAT.md` "Plan 36.1" check 5: each depth's measured rate, and a narrowband demodulator and a decoder compared at the source's own depth and at the lossy one).

## Soapy: probe a lossy depth on first use

Plan 36.1 probes every depth the link carries at open, the lossy ones included, because a source's capability record cannot change while it runs. Probing a lossy depth on its first use (a scan's `bits`) would keep a first start short, but needs a capability record that can change under a running source (and a `Port::capabilities` that is not read once).

## Node: measure a struggling link again by itself

The deferred half of plan 36.1's link diagnostics (spec 4.6a): the node notices recurring sample loss while streaming at a rate it measured clean (devices added around the radio on the same bus, a cable gone marginal) and measures again when the source is free, saying why. Today a measure again is the operator's call (`source_measure`, the page's "Run link diagnostics") or a change of the link's negotiated speed. `PortStats.tune_gaps` is what makes the steady-streaming loss countable: a tune's own overflows no longer count as `overflows`. It belongs with "Node: a baseline of the host's capabilities" below.

## Worker: pack small blocks into one ring slot

Each block takes a whole 2 MiB slot, so a B210's 10 ms blocks fill the 64 MiB ring in about 155 ms at 30.72 Msps; packing several small blocks into one slot (or sizing slots to the block) would buffer the full ring's worth of samples.

## Test: `ops::too_old_carries_the_rings_earliest_ns` flaked once with a subtract overflow

Seen 2026-11-07 during plan 28.1, passed on rerun. Likely a timing assumption between the ring's `earliest_ns` and now.

## Test: `modes_check::a_bad_synth_block_fails_its_row_and_the_rest_still_run` flaked once on a leftover scratch dir

Seen 2026-11-09 in a full workspace run (the suite in parallel), six single reruns passed. The checker's `remove_dir_all` of `check-<pid>-pocsag_decode` has its result ignored (`modes_check.rs`, after `run_test`), so a node still writing its state dir as it shuts down leaves the directory for the test to find. Wait for the node to finish, or retry the removal, before reporting the row.

## Page: pointer movement over the page is costly

From the operator, 2026-11-09: the desktop mouse lags, worst while the pointer is over the node's page in the browser (the browser held about a third of a core with the page open; the host itself was idle). Profile the page's pointer handlers and waterfall redraws (hover readouts, tooltips, the box menu's tracking): coalesce pointer events to one per animation frame, skip redraws when nothing under the pointer changed, and make sure the canvas path is GPU-friendly. Check with the browser's performance panel on a page with several live jobs.

## Origins: which site sent this burst

From the operator, 2026-11-09: when several licensed sites share one channel, the map lights every candidate and nothing says which one sent a given burst. Options, cheapest first:
- **Per-burst strength against each candidate.** Compare a burst's received level with each candidate site's expected level (power, antenna height, distance, a simple path-loss model) and rank the sites per burst, not per band. Uses only what the register and the detector already give.
- **Bearing from a two-channel coherent receiver** (the B210 has two). Two antennas half a wavelength apart give an angle of arrival per burst from the phase difference; front/back ambiguity is resolved against the register's candidates. The map draws one bearing line per burst. Needs a phase-calibration step and a second antenna.
- **Time difference of arrival with a second synchronised receiver.** A fix without directional antennas; needs GPS-disciplined clocks on both (see "Station: discipline the B210 to its GPS module") and a federated node.
- **A directional antenna**, swept or fixed, judged by burst strength.
- **An identity in the frame** (station ID), only where the protocol is a clear, standards-based one the node may decode; never for payloads the field craft says to report, not decode.

Keep live examples out of the repository: no site names, frequencies, distances or bearings from real observations of infrastructure.

## Plans: a `pocsag_au` channel plan

From the operator, 2026-11-09: there is no POCSAG channel plan, only the `pagers_au` set (a span watch that starts decoders on carriers it detects). An always-on plan (one pocsag_decode per register-assigned pager channel in the region, as `acars_vhf` does for ACARS) would decode a quiet channel's first page without waiting for detection. Build it from the register's assignments rather than a hand list, and keep the set as the default.

## Station: USB 3 noise near a 2.4 GHz mouse receiver

Field note, 2026-11-09: the desktop's wireless mouse receiver shares the room with the B210's USB 3 link (5 Gbps), a known 2.4 GHz noise source. If drop-outs return with the B210 streaming, move the receiver onto an extension lead away from USB 3 cables, or put the B210 on a ferrite-clipped or USB 2 cable. Not a node change; recorded so the cause is known.

## Access: APRS station movements are personal data

From the operator, 2026-11-09: aircraft and radiosonde positions are fine to keep, but an APRS station's position reports are a person's movements, even when the ham chose to beacon them. Today `aprs_decode` writes every position into `events.log` and the map keeps a track per station. Treat them like pager text: mark the row `access: "restricted"` (the marker plans 29 and 31 introduced) so the coming ACLs can gate them, add a retention limit for APRS positions in the event log, and let the map show a station's latest fix without keeping a trail by default. Never commit real APRS tracks or callsign positions to the repository (fixtures stay synthetic, as they are now: `VK3ABC`, the APRS specification's example positions, the demo station).

## Sources: attach and detach a radio on a running node

From the operator, 2026-11-09. Sources exist only as flags on the node's command line (`--source-soapy`, `--source-kiwi`, `--source-node`, `--source-vrt`, `--source-file`) and are not persisted, so adding a radio or taking one away means editing the service unit and restarting the node: every running job drops and resumes from state, the page reconnects, and a one-shot cannot do it at all. The day's example: detaching an unplugged HackRF, which the worker had been faulting every 5 s, was a unit edit and a restart. Wanted, as one mechanism for every source kind:
- `source_attach` (`id`, `kind`, the kind's args as the flag takes them, optional `position`) and `source_detach` (`id`), applied by `Node::apply` like every other op, on the CLI (`zeta source attach soapy hackrf driver=hackrf,gain=45`), HTTP, MCP and the agent. Attach validates as the flag does (gain range, device present or seen before, id unused) and starts the worker; detach refuses while a job holds or follows the source, naming the jobs, and takes `replace` for the ones the operator says may stop (the agent never decides that itself). A detached Soapy source's `sources/<id>.json` stays, so a later attach keeps its cap, pin and gain.
- Persist the set of sources in the state directory beside the jobs, pins and gain, so `serve` resumes them and the flags become the first seeding, not the truth; `source list` is the truth. Decide what a flag and a persisted record disagreeing means (the flag wins and is logged, or is refused as a duplicate id).
- Secrets: a Kiwi password or a node secret must not land in `ops.log` or the persisted record as an op payload would; the op takes an environment variable name (`ZETA_KIWI_PASSWORD_<ID>`, as the flag does without `/PASSWORD`) or a reference into a secret store, never the secret.
- Page: a Radios menu with Add (kind, id, args) and Remove on the source's tab, behind confirmation, as the Jobs pane's Clear all is.
Not the same as the worker's healing (plan 20), which keeps an unplugged device attached and faulted until it returns: that is the device's state, this is the operator's intent.

## Sources: a source that delivers spectrum rows (a Kiwi's waterfall, an FPGA front end)

From the operator, 2026-11-09, attaching a KiwiSDR: "the Kiwi has a wideband RF view; are you sure we cannot tap into that?" Today the node takes only IQ from a source and runs its own `fft`, so a Kiwi source is its 12 kHz IQ channel and nothing else. The Kiwi's waterfall is a second websocket (`W/F` beside `SND`, which `zeta-source-kiwi` recognises and ignores) that streams its own FFT of the whole 0-30 MHz: 1024 bins of 8-bit dB per frame at the chosen zoom (zoom 0 is the whole band, about 29 kHz a bin; each zoom level halves the span; up to about 23 rows a second), with `SET zoom=.. start=..`, `SET maxdb/mindb` and `SET wf_speed`. It is a spectrum, not IQ: nothing can be demodulated from it, but it is exactly what a `spectrum` job's `fft` node produces and what the page's waterfall and a `watch`'s `detect` consume. A waterfall takes one of the Kiwi's waterfall-capable channels, fewer than its receiver channels on many Kiwis (the status page's `mode=rxN.wfM`). Wanted, as one mechanism and not a Kiwi special: a source capability `spectrum: {min_hz, max_hz, bins, rows_per_s}` for a source that can deliver spectrum rows itself; `spectrum` (and a span `watch`) on such a source expands to a `source_spectrum` node in place of `fft`, publishing `spectrum` rows with the source's `center_hz`/`bandwidth_hz` and a calibration offset, and the band it asks for picks the zoom and start; `measure`, the demodulators and the decoders still take the IQ channel. The same shape serves a future FPGA/PFB front end that computes its spectrum on the device and a `--source-node` forwarding another node's rows. The page then shows the Kiwi's whole HF band on its tab while one 12 kHz channel is worked on it.

Built in plan 36.2: a `scan` on a source whose `capabilities.sweep` holds the span is swept by the source itself (`Port::sweep`; a KiwiSDR's waterfall stepped through zoom and centre), its rows forwarded as scan rows beside the source's other jobs. Still wanted: a `spectrum` job (and a span `watch`) reading a source's rows instead of running `fft`, and a `--source-node` forwarding a far source's rows (today a scan on it steps).

From the KiwiSDR server source (jks-prv/Beagle_SDR_GPS, read 2026-11-09), what the stream is and where its limits sit:
- Each `W/F` frame is a 16-byte header (`W/F` tag and a pad byte, then `x_bin_server`, `flags_x_zoom_server`, `seq` as little-endian u32) and 1024 unsigned bytes, one per display pixel: `dB = byte - 255`, nominally dBm at the antenna (the server maps 0..-200 dBm to 255..55; `MSG wf_cal=` carries the operator's calibration offset, applied by the client). Frames with the compression flag are IMA ADPCM, 2 pixels a byte and 10 tail samples to drop; `SET wf_comp=0` turns it off (zoom 0 is never compressed). `SET wf_speed=1..4` picks 1, 5, 13 or 23 frames a second. `SET zoom=Z cf=KHZ` (servers from v1.329) places the view: span is 30 MHz / 2^Z, Z 0..14; `SET interp=` picks how the server's FFT bins (8192 points, 4096 used at zoom 0, about 8 kHz each) fold into the 1024 pixels (max, min, last, drop, cma; 13 is drop with CIC compensation); `SET window_func=` picks the window. The first 2 to 4 bins of each frame are zeroed for DC. A `W/F` connection needs a waterfall-capable channel, and the connection's `MSG wf_chans=0` says none was free (an audio-FFT channel instead).
- The 1024-pixel width is a server constant (`WF_WIDTH`), so the 29 kHz-per-pixel full-band view cannot be sharpened from our side at zoom 0. It can be sharpened the way our `scan` already does on a local radio: step one `W/F` connection through zoom and centre (`SET zoom= cf=`, a dwell for the CIC to settle, 23 frames a second), and assemble `spectrum` rows per step. That is the same job shape as `scan` with the Kiwi's own FFT in place of ours, with a calibrated dBm scale our local radios do not have. Changing the width itself would mean running a forked Kiwi server.
- The same tree carries an unreleased wideband IQ mode: a `wb` FPGA configuration (one receiver, one waterfall, IQ at 72, 108, 144, 192, 204, 240 or 300 kS/s as 16-bit little-endian over `/wb/<ts>/SND`; kiwiclient already has `--wb`), an experimental SpyServer-protocol server on port 5555 for SDR++, and a browser refusal ("configured for non-web, wideband connections only"). It is hidden: the admin page's selector is compiled out (`var wb = 0`), the `wb.*.bit` bitstreams are not installed by the Makefile and the changelog has no entry. It would turn a Kiwi into a single-user 300 kHz receiver, so it is not for a shared one; watch the releases, and if it ships, a `--source-kiwi` with `wb` is a sample-rate change, not a new source kind.

## Spike: wideband views from the Kiwi and from custom B210-clone gateware

From the operator, 2026-11-09. Two wanted outcomes, both through the spectrum-rows source above: (a) the Kiwi's wideband view on the page, one `W/F` connection stepped through zoom and centre as `scan` steps a radio; (b) custom gateware for the station's B210 clone (a larger FPGA than the original; UHD 4.6 sees it as a stock B210 today) that computes a wideband spectrum on the device and delivers rows beside the IQ, shown on the page the same way. Spike first, no spec yet: confirm the clone's FPGA part, how its image is loaded and built, and how rows would reach the host; then decide. Start with (a), which needs no hardware.

Outcome (a) is closed by plan 36.2: a scan on a KiwiSDR sweeps its whole range over one `W/F` socket stepped through zoom and centre. Outcome (b) stays open.

## Video: the audio subcarrier stage

`subcarrier_demod` per spec 5.4 of `docs/superpowers/specs/2026-11-06-video-decode-design.md` (the 6.5 MHz sound of a TV-style carrier). It takes its input from the demodulator before decimation when the composite rate is under 2 x 6.5 MHz, so the subcarrier survives the decimation.

## Video: a true chroma notch and chroma decode

The luma path's 3 MHz low-pass stands in for a chroma notch, and colour is not decoded. Add the notch at the subcarrier of the detected standard, then PAL and NTSC chroma decode.

## Video: fixtures of the four bench transmitters

Recorded 2026-11-10 (UAT 28.2) under a private path, kept out of the repository, with a note beside them.
- What: 0.5 s IQ cuts at 30.72 Msps cf32 (about 123 MB each).
  - The analog 1.3 GHz VTX in the 23 cm amateur band: clean at its rated 12 V, distorted when undervolted at 5 V.
  - Two digital links, for a later layer-4 look that is parked: a 2.4 GHz frequency-hopping reversing camera (linked and idle), and a 5 GHz Wi-Fi-based HDMI sender's beacons.

Still to do: point the composite tests at the analog cuts. A test reads them through an environment variable and skips when they are absent, as `bits_chain.rs` does for private captures. Decimate to the composite rate in the test, rather than keeping a second copy.

## Video: a thin rail tag hides its thumbnail

A live thumbnail rides a video job's tag on the waterfall, but the rail gives a tag `.thin` when it overlaps the tag above it (`layoutRail` in `overlays.js`), and the rule `#rail .job-tag.thin > *` (in the page's `style.css`) hides a thin tag's children, so the thumbnail canvas is hidden (and holds no socket). That is common in the narrow footprint's short rail; a lone tag there is a full tag and shows it. The Video tab and the full view always work. Give a thin tag's feed a place (the tab's strip, or a taller tag).

## Video: PAL line numbering

The decoder counts line 1 from the pre-equalising pulses; real PAL counts from the first broad pulse, so a real VTX's picture may sit about 2.5 lines low. Check on the bench fixtures and shift the count if so.

## Video: odd and even field rows are not tested for alignment

Fields are shown one at a time. Weaving the two fields into a frame is later, and needs a test that odd and even rows line up.

## Video: B210 overflows at 30.72 Msps since plan 28.1

UAT 28.2 (2026-11-10) found the field host's B210 overflowing about 15 times a minute at 30.72 Msps, where plan 28.1's probe measured none. On the 20.48 rung it was about 6 a minute. Each overflow costs the decoder a field, which the operator sees as ripping.

The B210 sits on a chipset USB controller, shared with a webcam and an idle second SDR, while the CPU's own USB controller is empty. To do:
1. Move the B210 to a CPU-attached port, or to a USB card of its own.
2. Run the rate probe again (re-probe a radio: stop the node, drop the `capabilities` key from its source record, start it), and count the overflows a minute against today's.
3. Report the overflow rate on the source (`stats.overflows` is cumulative), so a regression shows without a stopwatch.

## Video: `sharp` runs at about 1 frame a second

UAT 28.2: `sharp` (full width) showed about 1 frame a second on the bench, which is a slideshow. The spec's profile allows up to 5 a second, and `smooth` keeps up.

Find where the rate goes: the full-width PNG encode on `spawn_blocking`, the newest-frame slot dropping under a slow encode, or the page's credit (`ws_credit_skipped` and `video_frames_skipped` on `/health`). Then either reach the profile's rate or make `sharp` an explicit stills mode.

## Video: line timing jitter at modest SNR

UAT 28.2: at 16-23 dB SNR the picture tears between the fields lost to overflows, presumably from line syncs misread in noise. Tune the line PLL (its gain and how it rejects an outlier edge) against the private 1.3 GHz fixtures, and add a test with noise added to a real cut.

## Kiwi: pair the waterfall socket with the IQ session to save a user slot

A scan's `W/F` socket stands alone and costs one more user slot while it runs (plan 36.2). Paired with the IQ session's timestamp it would share the session's channel. The server ends a pair together (closing the `W/F` stops the `SND`), so the socket would have to be parked, not closed, at a sweep's end. To settle on the live Kiwi's server version: UAT 36.2 check 2 (V1, V3, V4).

## Sources: the HackRF's firmware sweep behind `Port::sweep`

The HackRF's `hackrf_sweep` firmware mode is a second self-sweeping source: a span in one pass at a much higher rate than stepped tunes. Its `Capabilities.sweep` and `Port::sweep` would reuse the scan's source path; the open questions are its bin widths and the gaps between its steps.

## Page: a self-sweep's rows as the coarse layer under the same source's fine capture

Today the waterfall follows the scan and hides the fine rows of a capture on the same source. A self-sweep's rows could draw as the coarse layer beneath the fine capture, as another source's scan does for a source with a scanner.

## Scan: a `source` scan left waiting

A `source` scan left waiting retries every 16 s for as long as it runs; an operator stops it.

## Scan: the self-sweep half of `node/scan.rs` in its own module

`crates/zeta-core/src/node/scan.rs` carries the stepped loop and the self-sweep's start, wait and clock. Move the self-sweep half to its own module.

## Follow-ups from the live view and the plan 36 run (2026-11-12)

From the first live run of plan 36 and the live-view fixes after it. Each is small.

- **The rate probe under-measures on a busy host.** On the field host the probe at a node's start put all three link depths at the same cap, and a `source measure` on the idle radio an hour later put the lossy depth at twice that. A probe taken while the host is starting other work should not be cached as the radio's figure: probe when the host is quiet, or keep the best of two.
- **`slow_tunes` under-counts calibrating tunes.** The rule is ten times the median tune; through the node a tune's median is about four times the device's own, so a calibrating tune (about eight times the median on the field host's radio) is not counted. Count against the fastest tunes, or a fixed multiple of the lower quartile.
- **The probe's time estimate is the worst case.** The line says "up to about 108 s" where the probe takes under 30 s. Say the usual time, or count rungs as they pass.
- **Two pages on one self-sweeping source share its one scan.** Both adopt it and each moves the other's view. Give each page its own view where the source allows a second waterfall connection, or show who moved it.
- **A KiwiSDR pauses its own waterfall** for about two seconds every quarter of a minute under load (seen with a raw connection too). The stream rides through it; the page could mark the gap rather than stretch the last row.
- **A Soapy test flakes under full-workspace load:** `a_cs16_device_hands_raw_blocks_only_after_the_tune_window` failed once in a workspace run and passes alone and under two CPUs.
