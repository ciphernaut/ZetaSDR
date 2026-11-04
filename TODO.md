# TODO

## Scan: the HackRF's spur grid coincides with the quarter-step shift

From the HackRF run on the field host, 2026-11-01 (plan series "Plan 15 status"). The HackRF One has spurs on multiples of 4 MHz whose level depends on the tune: a 3-bin spike at +4 MHz (and mostly -4 MHz) of 21-37 dB when tuned to 444, 460, 476 or 492 MHz, ±8 MHz spikes on the edge bins of every 16 Msps row, a +2 MHz spike at 462 and 470 MHz, a comb every 100 kHz around 480 MHz. The scan's quarter-step shift at a 16 MHz step is exactly 4 MHz, so one grid's +4 MHz spur lands on the other grid's centre and is painted where that grid masks its DC: the survey shows a grid of peaks that moves between sweeps (the plan's failure sign) that no mask width fixes. Wanted: a per-device spur list the scan rows mask as `masked` ranges (offsets from the tune, as the HackRF's are, plus absolute frequencies as the B210's 402.0245/402.1505/402.9005/403.1494/403.8996 MHz are: the watch's `spurs_hz` is the absolute half), measured once per device by a `zeta soapy survey` over a quiet span and kept with the source record; and a step or shift that is not a multiple of the spur spacing on a device that has one (15 MHz steps, or a shift of a third). The B210 showed no such grid.

## Soapy: the HackRF's read timeouts at 1 Msps

From the same run. At the 1 Msps UHF CB pin `stats.timeouts` climbs about 8 per second (71 → 412 in a minute) with the rate reading 1.1 % fast and no overflows; at 2 Msps and up it stays at 0. The HackRF is under its 2 Msps minimum there and the driver's read times out between blocks. Harmless so far (the jobs ran) but the counter is useless as a health signal at that rate: either read with a timeout scaled to the block duration, or capture at 2 Msps and decimate for a 1 MHz pin. Also: the 1 Msps capture gets no channel bank (`channel.path` `direct`, `no bank`); say why in `source list`, or give it one.

## Lease: keep a lone job's capture centre off its channel

From plan 15's non-goals, 2026-10-31. A demodulator's or decoder's own capture centre sits on its channel, so the receiver's DC spike lands in the channel (the same problem the scan grid had; a one-off demod read it as a 59 dB carrier on 2026-10-23). Keeping the centre off the channel needs the lease manager to know the capture's minimum width before it tunes, which the capability record does not carry, and the offset depends on the notch width plan 15 measures (`SCAN_NOTCH_HZ`: its live acceptance's figure is this gist's input). Its own small plan after plan 15. Seen live 2026-11-01 on the B210: after `replace` moved the last fm_demod to 477.466 MHz the capture centred on it, putting the DC spike on the channel.

## Bug: a refused `graph_put` persists its graph

A `graph_put` that loses a race with another put on the same job returns an error, but its graph is already in the record (`graph_get` and a restart use it). Found in plan 14 review, pre-existing. Fix: under the jobs lock, refuse before persisting when the entry is `Running` again after this put's stop.

## Audio: the default `deviation_hz` on a 12.5 kHz channel

The voice low-pass and de-emphasis shipped 2026-11-01 (`fm_demod` carries `audio_lowpass_hz` 3400 and `deemphasis_us` 530; the voice demodulators take `denoise: true`). Left, from the operator listening to a fitted ch9 demod on the B210: on a 12.5 kHz channel the default `deviation_hz` 5000 halves the audio level of a 2.5 kHz-deviation CB signal; the fit (or the channel plan) could set 2500 there.

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

One open page held 12-16 of the node's 16 stream slots (`max_ws`); a second page got `stream refused: too many stream connections` on every stream and could not paint (the B210 live run of plans 15/18, 2026-11-01; the operator's page hit it again while agent `stream tail`s ran). The default was raised 16 → 64 the same day. Still to do: a `--max-streams N` flag on `serve` (env `ZETA_MAX_STREAMS`), per-client fairness so one page cannot take the whole pool, and the page releasing the streams it no longer shows.

## UI: the page watches at most 4 jobs, so a tag loses its squelch dot

With four demods and a `transcribe` attached, the oldest demod's tag had no squelch dot: the transcribe took a watched slot first (`MAX_WATCHED` 4, decoders before squelch-only jobs). Watch the squelch for every rigid job on the shown source, or prefer demodulators for the slots. The jobs fold uses the watched-mix active set for unwatched jobs, so without a mix the fold only covers watched and squelch-less jobs. From the B210 live run of plans 15/18, 2026-11-01.

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

## Bug: the bits chain does not recover a real OOK remote (first on-air test, 2026-11-04)

The operator's 433 MHz fixed-code remote on the field host B210 (pin 435 MHz / 6.144 MHz): EV1527/PT2262 style at 433.882 MHz, about 30 kHz wide, 30-34 dB SNR; pulses 280 us (0) and 910 us (1) in a 1.25 ms bit, chip about 313 us (3200 Bd); a sync of one short pulse and 9.7 ms of silence (31 chips), then 24 bits, repeated about every 40 ms; code <button-code>, decoded by pulse width from the samples (every whole frame of both fixture windows). Captures on the node: the narrow set of job 01M6HTR8A8FK7E0P4G64RPPZTX (8 windows, 51.2 kHz at 433.882 MHz, layer 1 measured 3197 Bd) and the wide set of job 01M6HTNMP9ESVNSG6DXJPPE9DC (8 windows, 2.048 MHz at 433.4 MHz). Pin them before the store expires them, and copy one into a test fixture.

- Fixed offline (slicer seed, early_late for OOK); the live `bits_start` re-run on the field host is pending. On the node, `bits_start --modulation ook --symbol-rate-baud 3200` on the narrow set (job 01M6HTS9GDRVCNY236ZRPFDXWH) ran `clock_recover` with `algorithm: null` (the `bits_graph` OOK default then), which is Mueller and Müller on one channel, not early-late: it read 3199 Bd but never locked (error_rms 1.38). The slicer's eye_open was 0.04, the bits were almost all zeros, and the code's chip pattern matched at best 42/96 chips. M&M reads no timing across an OOK pulse's flat top, so `bits_graph` now gives every OOK chain `early_late`. Offline on the fixtures (`crates/zeta-dsp/tests/data/ook-remote-433882k-{1,2}`), `ook_demod` and early-late were sound: strobes read about 1.0 on and 0.0 off, error_rms 0.43/0.39, locked, and a fixed 0.5 threshold reads <button-code> on every whole frame. The chain still sliced to zeros there: the slicer seeded its upper level from the first frame's maximum, which held the demodulator's onset transient (4.7 against an on-level of 1). No symbol reached the midpoint, and the level never came back. The slicer now seeds by a 2-means on the first frame (falling back to its extremes when the frame shows one level). `a_real_ook_remote_decodes_to_its_code` runs the chain `bits_plan`/`bits_graph` build and reads <button-code> on 5 and 4 frames. The levels' drag is real but did not cause it (see the deferred minor below).
- Fixed (2026-11-04): `bits_start` built the chain around the capture's centre, so on the wide set (482 kHz off a 433.4 MHz centre) the resampler cut the signal away. A signal off the centre whose measured band reaches past the resampler's passband (offset + half the measured width over 0.45 × the chain rate; not on a `noise` label) now gets a `channel` stage at the measured centre (band the measured bandwidth × 1.25, at least 4 × baud), `job_context.bits.shift_hz` recording the offset.
- Fixed (2026-11-04): the B210 windows (`ook-remote-b210-{1,2}`, 61.44 kHz, measured 1110 Hz off the capture centre and 40 kHz wide, so `bits_start` takes the `channel` path) sliced to all zeros, and the node's `bits_start` over three such windows read only 4 whole frames. Not the shift: the `channel` stage and the 61.44 kHz rate were sound (the centred chain failed the same way, the HackRF window decoded). Each window opens on 20 ms of silence, so `ook_demod`'s levels settled on the noise and scaled the first pulse by the noise's range: one strobe read 73 against an on-level of 1. The slicer's 2-means seed made that strobe a cluster of its own (midpoint 37, every bit 0; the degenerate check then fell back to the extremes). The seed now holds first-frame values within one quantile span past its 10th/90th percentiles (`SEED_FENCE_SPANS`) and judges the degenerate case on the held values; every whole frame reads <button-code> (12 and 13 frames), `b210_windows_decode_every_whole_frame`.
- Fixed (2026-11-04): layer 1 measured the HackRF window (`ook-remote-hackrf-1`) at 1598 Bd, the 800 bit/s bit rate's second harmonic, and labelled it, and both 433882k windows, `fsk` (the instantaneous-frequency clusters counted the off samples' noise, which spreads across the band, as a second tone 2.5-8 kHz from the carrier), so a bare `bits_start` picked FSK. The FSK clusters now count only the samples whose envelope reaches half its 90th percentile (FSK's envelope is constant), and a two-level envelope whose on and off runs are whole multiples of one unit measures its chip rate from the run lengths (`ook_unit_rate`, the label's evidence `(runs)`) before the cyclostationary line: all five windows read `ook` at 3195-3198 Bd, and `bits_start` with nothing given picks OOK at the chip rate (`bits_start_infers_ook_at_the_chip_rate_on_a_real_remote`).
- A fixed-code OOK remote like this one wants a mode row (rtl_433-style pulse-width decoding) more than layer-4 bits: a `pulse_ook` stage measuring on/off widths and a row per common timing (EV1527, PT2262) would decode it directly.

## Feature: layer 4, working an unknown signal

From the operator, 2026-10-27. Plan 19.2 shipped the stages (`capture_in`, `resample`, `fsk_demod`, `psk_demod`, `ook_demod`, `clock_recover`, `slicer`, `correlate`, `bit_view`), `graph_start`, `bits_start` and the Bits pane; the playbook is "Working layer 4". What is left:
- Live chains by hand on a real capture (spec 9 item 3, after 19.3): the field host 404.025 MHz captures through `bits_start` and the Bits tab; `UAT.md` "Plan 19.2".
- LoRa chirp-rate detection in `measure` (an unknown 250 kHz chirp is reported unknown today).
- The `measure` upgrades 19.1 did not take: `measure` finds no symbol rate on short FSK bursts (the e2e fixture's captures had none, so `bits_start` needs `symbol_rate_baud` given), and better order estimates from constellation moments.
- Write-back of a successful hypothesis as a signature (plan 19.3).

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

## Feature: generic layer-4 framing decoders (bits to structured frames)

From the 2026-10-29 404.025 MHz investigation. The bit-level loop above gets an unknown signal to a bit stream; it stops there and the agent reads raw hex. A generic framing layer would carry the common clear, unencrypted link protocols seen on licensed UHF data channels, so the agent can report structured frames instead of hex.

Scope, generic only: a `frame_decode` stage (or a `framing` family of mode rows) that takes a `bits` stream and applies a declared framing: preamble/sync-word hunt, length field, bit order and whitening (NRZI, Manchester, differential), and a CRC check (a small library of polynomials: CRC-16-CCITT, -IBM, -DNP, CRC-32). On a clean frame it emits an `event` with the raw payload bytes and the framing that matched; it does not interpret the payload. Then thin, well-known serial-over-radio parsers as separate rows, each a pure bytes-to-fields decoder with a CRC gate: Modbus RTU, DNP3, IEC 60870-5-101/104. These are published open standards; the decoder is the generic protocol, chosen by signature match or named by the operator, never built against one named licensee's traffic.

Boundary (explicit, so a future session does not drift): this decodes clear, unencrypted, standards-based framing that the node is lawfully receiving. It never attempts to break encryption, never targets a specific operator's link as the deliverable, and emits no transmit path. An encrypted or proprietary payload is reported as `framed, opaque` with its length and CRC status, not decoded.

Needs: the `bits` stream kind and the layer-4 demod/slicer stages above (prerequisite); a CRC/whitening helper in `zeta-dsp`; one `frame_decode` stage; one row per serial protocol with its `event` schema and `summary`. Each protocol row gets a synthesiser test (plan 17's families plus a framing generator) so `modes check` proves it offline, no live capture of anyone's link required.


Scope estimate: the stage set about 1500-2500 lines in `zeta-dsp` with fixtures (synthesised FSK/PSK/OOK at known rates); `bit_view` heuristics 400-600; signature write-back 200-300; the `measure` upgrades 300-500. Two or three plans.

## Architecture: a mode registry and a generic external-decoder adaptor

Open work left from plan 16's mode registry (delivered; see the plan series):

- **Feeds.** `sink` stage kinds for the standard feeds (APRS-IS, SBS/Beast, AIS NMEA, PSK Reporter, WSPRnet, KISS TNC) so results leave the node the way other tools expect.
- **Time.** UTC-slotted modes (FT8/FT4 15 s, WSPR 2 min, JT65 1 min) and TDMA (DMR, TETRA) need true block timestamps. The HackRF timestamp bug is fixed (plan 14); the 2 h clock run that would clear the WSJT family is still outstanding (plan series, "Outcome").
- **Span decoders and trunking.** OnAir and OpenWebRX+ decode every POCSAG/APRS/VDL2 channel in a capture at once. On a pin that is one job per channel on the bank today; a decoder that finds its own channels would take the whole capture. Trunking (P25, DMR Tier III, MPT1327) needs a follower that starts voice jobs from control-channel events inside the pin.
- **Vocoders.** Codec 2 (M17, FreeDV) in-crate; AMBE/IMBE only through an external `mbelib` build, never shipped; HD Radio HDC off.
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

Operator-visible nits from the plan 24 reviews, none blocking. The rail's label box is about 66 px and may clip the centre on long kinds; hot does not beat speaking when both overlap on the rail; thin rail tags under a full tag cannot be hovered; the frame spine stops at the waterfall bottom when the pane strip is open; a tilted long ribbon crosses neighbouring lanes visually; the ribbon `top` transition glides 160 ms on zoom; mixed ribbon heights space lanes unevenly; keyed redraws keep their old z-index; an open graph modal ignores a theme switch until the next draw; the graph headline truncates mid-number; the `.events` cell's tooltip goes stale; in plain, speaking and hot do not colour the row ground; the fold samples the mix's speaking state per 2 s render, so a short burst can be missed; a watched squelch-kind job with `squelch: false` re-asks `job_context` every poll; a stored `plain` theme flashes `console` for a frame at load; the selected pane tab, map and listen buttons reuse the speaking colour; the source health spans are rebuilt in unselected tabs. Tests: no scenario pins the end caps, the narrow tap-to-expand, the row-hover fold path, the ribbon colour or tilt, or hot/speaking across a theme switch, and the narrow `narrow_hides_pane` scenario keeps a now-redundant bare-string wait.

## UI: state-stream decoders beyond the first subjects

Plan 23 delivered the ADS-B request: a row names its subject (`key`, columns, `waterfall`, `heading`, `label`), the page folds by key, the Subjects tab lists one row per aircraft, the waterfall shows one line, and the map tracks per subject (`adsb_decode` has a `position`). Left from the original note:

- POCSAG has no subject (each page is its own message); an address list in the Subjects tab would need a key on the pager's address.
- AIS vessels (MMSI as key) when the AIS row lands; it needs only a row.
- The node-side half is a separate gist ("Subjects: state does not survive a refresh").

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

## Detect: a per-device spur list

Plan 19.1 changed the detector (occupancy over the row median, a run-energy floor `energy_db` 30, `threshold_db` 6, a 4 kHz width floor, three sightings, at most 10 events a second), tuned on synthesised captures only: the B210 noise recording (`deploy/field-host/record-noise.sh`, then the ignored tests in `crates/zeta-dsp/tests/detect_noise.rs`) was made 2026-11-03 and both tests pass. What is left: the B210's spurs (402.0245, 402.1505, 402.9005, 403.1494, 403.8996 MHz, 4.5 kHz wide at round offsets from the capture centre) still need a per-device spur list that `detect` masks (`spurs_hz` is the absolute half, empty by default), measured once per device by a survey over a quiet span and kept with the source record; the scan gist "the HackRF's spur grid" is the same list for the offsets-from-tune half. Also a tone-pair criterion for a narrow FSK sonde (4.5 kHz at 19 dB on 2026-11-01).

## Agent: requests too big for the voice console

Done: plan 21 (`docs/superpowers/plans/2026-11-01-agent-bulk-ops.md`), plus `jobs_stop` for "stop all jobs". Live acceptance is in `UAT.md`.

## Agent guide: SIGINT and UX additions

Items 1-3 of the first list shipped in plan 19.1 (the register and `as_of`, the low-duty rule, the unknown-signal playbook with its legal line: "Exploring an unknown signal"). Still open: (5, from the operator, 2026-11-01) expand this guidance as each mode and SIGINT component lands, including when not to touch running jobs; (4) lead the operator with the map and the one-line `summary`, name channels and licensees in plain words, and offer the next investigative step (measure, identify, knowledge lookup, direction-find). Plan 19.2 and 19.3 extend the playbook with bits, frames and signature write-back.

## Feature: VDL Mode 2 and HFDL rows (the other aircraft data links)

From the operator, 2026-11-02 ("ACARS and VDL"). Plain VHF ACARS is `acars_decode`; most airline traffic has moved to VDL Mode 2 (136.975 MHz in Australia plus a few channels, D8PSK at 31.5 kbaud, 25 kHz spacing). `dumpvdl2` 2.6.0 and `dumphfdl` are installed on the field host (`libacars` inside both). Wanted: a program row `vdl2_decode` like `adsb_decode` (IQ over stdin at the sample rate the decoder wants, its JSON output through `json_lines`), subject key the AVLC source address (the aircraft's ICAO), the ACARS fields it carries (`reg`, `flight`, `label`, `text`) as columns, no `position` (some message texts carry one; parse later); and `hfdl_decode` the same shape on a KiwiSDR source (HFDL ground stations around 8.9, 10.0 and 11.4 MHz; a 2.4 kHz channel fits the Kiwi's 12 kHz). Neither has a synthesiser: the `modes check` fixture is a minute recorded off the air with `iq read` (VDL2 near Brisbane is busy). One small plan, no node changes.

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

## Subjects: state does not survive a refresh

Plan 23 merges each subject's events on the page, so a refresh starts the Subjects tab, the tracks and the dim and drop clocks empty and refills them from live traffic (an aircraft re-announces within seconds; a sonde every second, an APRS station only when it next transmits). A node-side store, kept as `job_context.subjects` (the merged state by key, last seen, count, the track), would survive a refresh, serve the agent and the CLI ("which aircraft are up?") and let a second page open on the same picture. Decide where the dim and drop settings live then.

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

Seen on the field host 2026-11-03 01:17 (plan 19.1 deploy): the page's zoom-out retuned the B210 from the watch's 12.288 Msps to 61.44 Msps (the lease allows any width that holds the rigid holders' bands, and the B210's `max_bandwidth_hz` is unset); the device delivered 56 Msps with `stats.overflows` climbing by thousands, so every job inside the capture saw gaps (a `measure` discarded 381 windows in 8 s, the watch's rows and identify trials ran on gappy data). The earlier plan-8 note put the B210's reliable ceiling near 16-24 Msps over USB 3. Wanted: a per-device capture ceiling (a `max_rate_hz=N` item in `--source-soapy` args, or a measured one: the clock guard already knows when samples run short) that the lease applies as the source's `max_bandwidth_hz`, so a zoom beyond it is refused with `out_of_range` and `suggested`, and the page clamps its zoom to it; meanwhile the operator pins the capture (UAT 19.1.C).

## Calibration: a set of receiver and antenna calibration tools

From the operator, 2026-11-03, after UAT 19.1.A was done by hand. Two tools wanted as ops (and a page menu), not scripts:

- **RX calibration on a 50 Ω load.** What `deploy/field-host/record-noise.sh noise` does today, as a node operation: with the operator's word that the input is terminated, record the receiver's own noise over a band at the operating gain, keep it as the source's noise baseline (per device, per gain and rate: the floor in dBFS per bin, the DC spike's width, the spur list the "Detect: a per-device spur list" gist wants), and let `measure`, `detect` and the page read against it (a signal is what stands over the device's own floor, a spur is masked by name). A second recording with the antenna on is the positive control; store both with the source record and show `calibrated_at` in `source list`. The B210's floor was recorded 2026-11-03 at 65 dB over 403.025 MHz / 6.144 Msps (`~/zeta-fixtures/b210-noise-403m`), the detector announced nothing on it; that is the baseline the tool replaces.
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

## Architecture: recipes the node stores and runs by name

From the operator, 2026-11-02. A job setup (all ACARS channels on one radio, the CB plan with transcribers, the sonde watch with a pre-placed decoder) is reproduced from text each time: field craft prose, the scratch scripts on the field host, now `RECIPES.md`. The node cannot tell "the ACARS set" from seven unrelated jobs, and an agent re-derives the ops every run. A recipe should be a thing the node stores next to pins and plans (one JSON file each, `recipes/*.json` built in plus an operator directory as for mode rows): a name, the preconditions, the ops in order, and a start, a status and a stop by name (`recipe_start`, `recipe_status`, `recipe_stop`), with `plan_start` as the one-step special case it generalises. `RECIPES.md` is the stop-gap and the spec material: its JSON blocks are the ops a loader would lift unchanged.
