# ZetaSDR recipes

A recipe is a named, repeatable setup of the node: which radio, which band, which jobs, how to
check it took, how to take it down. The node does not store recipes yet (it persists live job
records, pins and gain, and knows mode rows and channel plans, none of which name a setup); this
file is where they live until it does. An agent driving the node (`zeta ask`, the MCP tools, a
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

## All ACARS channels on one radio

Decode every VHF ACARS channel the `acars_decode` row knows (worldwide primary, Europe, North
America, Asia) in one capture. Verified on the field host's B210, 2026-11-02.

**Check.** `source_list`: the radio has no rigid `holders` and no `pin` outside 129.7-132.2 MHz
(the page's elastic spectrum does not count). `modes_list`: `acars_decode` is `ready`.

**Ensure the capture.** 2 400 000 Hz at 130 925 000 Hz holds all seven channels, with the
capture centre on none of them (the receiver's DC spike sits there). The B210 answers
2 457 600 Hz; the lease rounds up, every channel still fits.

```json
{"op":"source_ensure","source":"b210","center_hz":130925000,"bandwidth_hz":2400000,"replace":[]}
```

Without this step the first `job_start` tunes the radio to a 10 kHz capture on its own channel and
the second channel conflicts with it.

**Start one decoder per channel.** Seven `job_start`, each inside the capture, so none retunes:

```json
{"op":"job_start","kind":"acars_decode","source":"b210","center_hz":130025000,"bandwidth_hz":10000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
{"op":"job_start","kind":"acars_decode","source":"b210","center_hz":130450000,"bandwidth_hz":10000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
{"op":"job_start","kind":"acars_decode","source":"b210","center_hz":131125000,"bandwidth_hz":10000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
{"op":"job_start","kind":"acars_decode","source":"b210","center_hz":131450000,"bandwidth_hz":10000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
{"op":"job_start","kind":"acars_decode","source":"b210","center_hz":131550000,"bandwidth_hz":10000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
{"op":"job_start","kind":"acars_decode","source":"b210","center_hz":131725000,"bandwidth_hz":10000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
{"op":"job_start","kind":"acars_decode","source":"b210","center_hz":131825000,"bandwidth_hz":10000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
```

The centres are `band.centers_hz` of `modes_show acars_decode`; read them from the row when it
changes rather than from here.

**Verify.** `source_list` for the radio: `current` is 130 925 000 / 2 457 600 Hz, `holders` has
the seven ids, `bank.consumers` is 7 and `bank.dropped` stays 0. `job_list`: seven
`acars_decode` jobs `running`. A `stream_tail` of the primary's event stream (the job's `event`
stream in `stream_list`) for 10 s seeing no frame means no message arrived in those seconds, not a
dead channel; ACARS traffic depends on aircraft in range.

**Stop.**

```json
{"op":"jobs_stop","source":"b210","kind":"acars_decode","plan":null}
```

**Gotchas.** The page watches at most four jobs for tag state (`TODO.md`, "the page watches at
most 4 jobs"), so with seven running some tags show no squelch dot; the jobs are fine. A decoder's
outline dragged on the waterfall is a `job_retune` off its channel and stops decoding ACARS there:
put it back with `job_retune` to the row's centre and 10 000 Hz. ADS-B on the same radio must wait
for these to stop; put it on a second radio (next recipe).

## ADS-B on a second radio

Mode S / ADS-B at 1090 MHz, run by dump1090 on the node's host. Pairs with the ACARS recipe when
the node has two radios: aircraft data links on both.

**Check.** `modes_list`: `adsb_decode` is `ready` (an `unavailable` row names the missing
dump1090; `apt install dump1090-fa`, then restart the node). `source_list`: the radio has no rigid
`holders` and no `pin`.

**Start.**

```json
{"op":"job_start","kind":"adsb_decode","source":"hackrf","center_hz":1090000000,"bandwidth_hz":2400000,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}
```

A `bandwidth_hz` under 2 000 000 is refused by the row (`band.min_hz`). The job tunes the radio
itself; no `source_ensure` is needed for a single job.

**Verify.** `job_list`: one `adsb_decode` `running`; `source_list`: `current` 1 090 000 000 Hz, the
job its holder. Events carry `icao`, `callsign`, `altitude_ft`, `lat`, `lon` as separate messages
complete; the Subjects tab lists one row per aircraft.

**Stop.**

```json
{"op":"jobs_stop","source":"hackrf","kind":"adsb_decode","plan":null}
```

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

## Adding a recipe

One section per recipe, in this order: a one-line intent with where and when it was verified;
**Check**; the ops as JSON blocks with a sentence each on why; **Verify**; **Stop**; **Gotchas**.
Take frequencies from `modes_show` or `channel_plans` and say so, so a row change does not strand
the recipe. A recipe that could not be run live says so in its first line.
