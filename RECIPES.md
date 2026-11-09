# ZetaSDR recipes

A recipe is a named, repeatable setup of the node: which radio, which band, which jobs, how to
check it took, how to take it down. Sets (`sets/*.json`, `set_list`, `set_start`) are the stored form of a recipe the node can
check: they name roles, never sources. A setup that is not a set yet lives here as text. An agent driving the node (`zeta ask`, the MCP tools, a
Claude Code session) copies the ops here instead of re-deriving the setup from field craft each
time, so two runs by two models give the same jobs.

Conventions:

- Ops are the JSON forms the node takes (`AGENTS.md`), in the order to send them. Prose stays
  around the blocks, never inside them, so a later loader can lift the blocks unchanged.
- `<source>` is an id from `source_list`; the examples name the field host's radios (`b210`, `hackrf`)
  where the recipe was verified there. Job ids come from the node's answers, never from here.
- Every recipe starts with **Check** (read `source_list` and `job_list`; a rigid holder outside
  the recipe's band is a `conflict` and the operator decides), ends with **Verify** (what the node
  must show) and **Stop** (one `jobs_stop` where the set has a filter). Running a recipe twice
  must not double anything: `plan_start` skips `existing` channels; a repeated `job_start` with
  the same band and kind is refused as `duplicate` naming the job, which is the right answer.
- Stopping or replacing a job the recipe did not start is the operator's call. A recipe never
  carries `replace` ids; a `conflict` is reported with its holders.

The frequencies in a recipe are the mode rows' well-known centres or a plan's channels. A signal
found by hand is still fitted on its measured centre (field craft, "Fitting a demod to a signal"),
never on a raster.

## Airband data links as a set

ACARS on every VHF channel and VDL Mode 2 in one capture of a wide radio, ADS-B on a second radio.
The set is `sets/airband.json`, a stored form of this recipe the node checks: `set_start airband`
starts it, `set_stop airband` takes it down. Run live on the field host's b210 (ACARS, VDL2) and
hackrf (ADS-B) before it became a set; the set itself is verified by the node's tests.

**Check.** `set_list`: `airband` is listed, not `broken`, with its two roles, `wide` (a radio that
captures 130.0-137.0 MHz at once, 8 MHz) and `second` (1090 MHz with 2.4 MHz). `source_list`: the
radios have no rigid `holders` and no `pin` outside the set's bands (the page's elastic spectrum
does not count). `modes_list`: `acars_decode` is `ready`; `vdl2_decode` needs dumpvdl2 and
`adsb_decode` needs dump1090 on the node's host (an `unavailable` row names the missing program and
its package). A member whose row is unavailable is `refused` at start and the rest go on: without
dump1090 the set still runs ACARS and VDL2.

**Start.** One op; map each role to a source of `source_list`, or to `null` to let the node pick the
first free one.

```json
{"op":"set_start","set":"airband","radios":{"wide":"b210","second":"hackrf"},"members":null,"replace":[]}
```

The set puts the ACARS channels and VDL2 under one lease decision for the band that bounds them
(about 129.7-137.0 MHz, inside the b210's capture), so nothing retunes between members; ADS-B tunes
the second radio itself. With one radio only, map `second` to the same source and the set is
refused `other` naming both roles, since 130-137 MHz and 1090 MHz do not share a capture: use
`members` to start `acars` and `vdl2`, and `adsb` later on its own. The named set `airband_vhf`
(`sets/airband_vhf.json`) is that one-radio form stored: the same two members on one role, `wide`, so one `set_start`
with `members: null` starts it and `set_stop airband_vhf` takes it down. `airband_vhf_and_sos`
(`sets/airband_vhf_and_sos.json`) adds a `guard` member, `am_demod` on 121.5 MHz with 10 kHz to
listen to, in a 16 MHz capture instead of 8.

**Verify.** `set_list`: `running` names the sources and each job `running`. `source_list`: the
wide radio's holders are the seven `acars_decode` jobs and the `vdl2_decode` job, `bank.dropped`
stays 0. A `stream_tail` of an event stream for 10 s seeing no frame means no message arrived in
those seconds, not a dead channel: traffic depends on aircraft in range. Events carry `reg`,
`flight`, `label`, `text` (ACARS), `icao` (VDL2 and ADS-B) and `callsign`, `altitude_ft`, `lat`,
`lon` (ADS-B); the Subjects tab lists one aircraft table across the links.

**Stop.** The set stops only the jobs it started:

```json
{"op":"set_stop","set":"airband"}
```

**Gotchas.** The ACARS centres come from `band.centers_hz` of `modes_show acars_decode`; read them
from the row when it changes. A decoder's outline dragged on the waterfall is a `job_retune` off its
channel and stops decoding there: put it back with `job_retune` to the row's centre and band. A
`conflict` names the role, the source and the holders and starts nothing anywhere: report it, the
operator decides, and `members` narrows the next request to what is free.

## UHF CB listeners with transcription

Narrowband FM on the Australian UHF CB plan (`uhf_cb_au`, 80 channels on two interleaved
12.5 kHz rasters) with a transcriber on each. Lifted from `~/zetasdr/scratch/cb-setup.sh` on
the field host (B210, 2026-10-29 onward), which did this as 80 `job_start` plus 80 `job_attach` before
`plan_start` existed.

**Check.** `channel_plans`: `uhf_cb_au` is listed with `bandwidth_hz` 12 500. `source_list`: no
rigid `holders` outside 476.4-477.5 MHz, no other `pin`. For transcripts the node needs a speech
backend (`GET /stt/languages` answers; without one `plan_start` with `attach` is refused naming
the stage: start without `attach`).

**Pin the band.** 1 000 000 Hz at 476 918 750 Hz holds ch1 (476.425) to ch80 (477.4125) with the
capture centre in the guard band between ch60 and ch21, so no channel sits on the DC spike. The
B210 answers 1 024 000 Hz. The pin outlives the jobs: the radio stays on the band when they stop,
and one-shot commands restore it.

```json
{"op":"source_pin","source":"b210","center_hz":476918750,"bandwidth_hz":1000000,"replace":[]}
```

**Start the plan.** One operation for every channel (or `channels` as a list of numbers, the
default set in the script was `[6, 9, 20, 27]`, the channels with speech seen so far).
`deviation_hz` 2500 for 12.5 kHz channels; `attach` `transcribe` with the default `en`.

```json
{"op":"plan_start","plan":"uhf_cb_au","channels":null,"kind":"fm_demod","source":"b210","deviation_hz":2500,"sideband":null,"attach":"transcribe","language":null,"replace":[]}
```

Read all four lists of the answer: `started` (each with `attached` or an `attach_error`),
`existing` (left alone), `refused` (that channel's error; the rest went on) and `warnings`
(repeat them to the operator: the speech daemon has four slots, so 80 transcribers queue). Run it
again to complete a partial set.

**Verify.** `source_list`: `pin` 476 918 750 / 1 024 000 Hz, `holders` one id per channel,
`bank.dropped` 0. `job_list`: an `fm_demod` per channel and a `transcribe` with `input` set to
each. A quiet channel publishes `squelch` events and no audio; read `guarded` before calling it
broken (a strong ch20 holds ch59 and ch60 closed by design). `zeta transcripts tail --lines 50`
on the node reads what was said after the fact.

**Stop.** The transcribers end with their inputs.

```json
{"op":"jobs_stop","source":"b210","kind":null,"plan":"uhf_cb_au"}
```

Then `source_unpin` if the radio is wanted elsewhere; the pin is not a holder and `replace`
cannot name it.

```json
{"op":"source_unpin","source":"b210"}
```

**Gotchas.** A box drawn over a CB channel on the page is about 100 kHz wide: the page fits it
first. The script used a 10 000 Hz band per channel so a strong neighbour did not open the
squelch; `plan_start` uses the plan's 12 500 Hz and relies on the adjacent-channel guard instead.
`examples/python/cb_watch.py` is the alternative that runs demodulators on active channels only.

**On the page.** With more than 8 idle channels the jobs list folds them into one `<n> idle fm_demod on uhf_cb_au` group, but only on evidence the page has: press **Listen to all** (the mix) and idle channels fold as the mix hears none of them; without it most demodulators stay listed, which is not a fault. `view set side jobs` gives the jobs list the whole side column.

## Radiosonde watch, 400-406 MHz

Hold the sonde band, let a `watch` spot and identify what appears, and pre-place an RS41 decoder
on the frequency the current flight is known to use. Lifted from `~/zetasdr/scratch/sonde-setup.sh`
on the field host (B210 and discone; first on-air RS41 decode 2026-11-01).

**Check.** `modes_list`: `rs41_decode` is `ready` (the `sonde-decoders` package; `dfm_decode`,
`m20_decode` likewise). `source_list`: no rigid `holders` other than a `watch` or a sonde decoder
on this radio (an ADS-B job or a stale `scan` is in the way; the operator decides). Know what is
up: SondeHub lists live sondes near a position,
`https://api.v2.sondehub.org/sondes?lat=-27.39&lon=153.12&distance=300000&last=10800`
(Brisbane launches from Redland Bay at 00Z and 12Z, RS41, a flight of two to three hours). Do not
guess a frequency.

**Pin the band.** 5 950 000 Hz at 403 025 000 Hz is 400.05-406 MHz with the centre off any
channel of interest.

```json
{"op":"source_pin","source":"b210","center_hz":403025000,"bandwidth_hz":5950000,"replace":[]}
```

**Start the watch** on the same band. It takes a rigid lease; decoders it keeps share the capture.

```json
{"op":"job_start","kind":"watch","source":"b210","center_hz":403025000,"bandwidth_hz":5950000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
```

**Raise the detector.** The default `detect` params fire on receiver noise on the B210 (about a
hundred "signals" in the first live run). `graph_get` the watch, set these on its `detect` node,
`graph_put` (it restarts under its id and re-adopts its decoders):

```json
{"threshold_db":18.0,"persist_ms":5000,"width_hz":[8000,30000]}
```

The 8 kHz width floor is a trade: under it the B210's own spurs (402.0245, 402.1505, 402.9005,
403.1494, 403.8996 MHz) show as signals; at it a real RS41, read as about 4.5 kHz wide, is missed
by the detector. Hence the next step.

**Pre-place the decoder** on the sonde's published frequency, 12 500 Hz, inside the capture (no
retune). One sonde per job. `rs41mod` decodes only within about 1 kHz of the sonde's centre, so a
job on the published frequency (401 500 250 Hz below, the 2026-11-01 flight's) decodes where the
watch's detector, good to one 1.46 kHz bin, may not.

```json
{"op":"job_start","kind":"rs41_decode","source":"b210","center_hz":401500250,"bandwidth_hz":12500,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
```

**Verify.** `source_list`: `pin` and the watch (plus the decoder) under `holders`. `job_context` of
the watch: `watch.signals`, `watch.decoding`, `watch.current`. The decoder's events carry `id`,
`position` and `summary` (`<id> <lat>,<lon> <alt> m <climb> m/s`); the map shows a marker and
track. A watch `unknown` event is reported as unknown with its `features`; it retries after a
minute, three times. Nothing uploads to SondeHub unless `sondehub` with a real callsign and
position is put on the `detect` node.

**Stop.** Stopping the watch stops its identify run and every decoder it keeps; the pre-placed
decoder is separate.

```json
{"op":"jobs_stop","source":"b210","kind":"watch","plan":null}
{"op":"jobs_stop","source":"b210","kind":"rs41_decode","plan":null}
{"op":"source_unpin","source":"b210"}
```

**Gotchas.** The 404.025 MHz carrier the watch keeps seeing is the Unitywater telemetry link
(16K0F1D in the ACMA register), written to `signatures.json` as `404.025-beacon`; it is not a
sonde. `measure.snr_db` is a per-bin peak and reads 20 dB where the channel SNR is 6 dB.

**On the page.** The watch is outlined on the waterfall and its decoder's rail tag shows the centre; `view set side jobs` gives the jobs list the whole side column, and if the watch keeps several decoders **Listen to all** lets the idle ones fold.

## Make your own theme

Verified on the plan 26 build; the folder format and every hook are in `docs/themes.md`.

**Check.** `zeta themes list` shows the six built-ins (`console`, `plain`, `cyberpunk`, `terminal`,
`psytrance`, `vibecoder`) as `ready`.

```bash
mkdir -p ~/.config/zetasdr/themes
cp -r themes/plain ~/.config/zetasdr/themes/mine
```

Edit `mine/theme.json` (`id` `mine`, the same as the folder; a few `tokens`) and put any extra rules
in `mine/style.css`. Restart the node, then type `view set theme mine` in the command bar.

**Verify.** `zeta themes list` shows `mine` as `ready`; a typo shows it `broken` with the field, and
the node logs `zeta: theme mine (<path>): <field>: <error>` at startup.

**Stop.** `view set theme console` returns the default; delete the folder to remove the theme.

**Gotchas.** A stored theme that is no longer ready falls back to `console`. Style the hooks listed in
`docs/themes.md`, not other markup.

## Adding a recipe

One section per recipe, in this order: a one-line intent with where and when it was verified;
**Check**; the ops as JSON blocks with a sentence each on why; **Verify**; **Stop**; **Gotchas**.
Take frequencies from `modes_show` or `channel_plans` and say so, so a row change does not strand
the recipe. A recipe that could not be run live says so in its first line.
