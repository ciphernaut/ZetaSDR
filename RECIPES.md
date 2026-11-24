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

For part of the plan name one of its groups (`channel_plans` lists them with their channels)
in place of `channels`, never a typed list: `lower40_voice` is ch1-40 less the data channels 22
and 23, `upper40_voice` ch41-80 less the reserved 61-63, `voice` both, `repeater_outputs` ch1-8
and 41-48, `data` ch22 and 23. "Transcribe the lower 40":

```json
{"op":"plan_start","plan":"uhf_cb_au","channels":null,"group":"lower40_voice","kind":"fm_demod","source":"b210","deviation_hz":2500,"sideband":null,"attach":"transcribe","language":null,"replace":[]}
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

`"group":"lower40_voice"` beside `plan` stops only that group's channels.

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

## Packet capture for Wireshark

Not run live when written: a stored pcapng of the APRS row's frames, opened in Wireshark. Needs a running `aprs_decode` job (144.8 MHz is the row's own centre).

**Check.** `job_list`: the `aprs_decode` job is `running` and has not been captured; `packets_list` does not already hold its file.

**Attach.** One op; the job is the decoder's id from `job_list`.

```json
{"op":"job_attach","kind":"packets","job":"<aprs job id>","language":null,"stream":null}
```

**Verify.** The new job's context shows `capture` (the id), and `packets` and `bytes` climb as traffic arrives. Download while it records and open it:

```bash
curl -O -H "Authorization: Bearer $ZETA_SECRET" http://<node>/captures/<id>.pcapng
```

Wireshark dissects AX.25 and APRS; each packet's comment names the job, source and frequency.

**Stop.** `job_stop` on the packets job; the file stays listed until `packets_delete`.

**Gotchas.** A node restart leaves the job `stopped` with the note `packet capture does not resume: attach again`; attach again for a fresh capture. A frame row's capture is `USER0`: bind a user dissector under DLT_USER to read it past the raw bytes.

## Aircraft frames for Wireshark

Not run live when written: a stored pcapng of the Mode S frames an ADS-B job decodes, by dump1090 or by OnAir, opened in Wireshark with the node's dissector. Needs a radio that holds 2.4 MHz at 1090 MHz.

**Check.** `modes_show adsb_decode`: `handlers` lists `dump1090` and `onair` with their `state`, and the `outputs` of each list `packet` (dump1090's is its Beast output; OnAir's is the pcap stream `onair-rx` serves, installed by the operator). `source_list` and `job_list`: no rigid holder of the radio whose band the 2.4 MHz capture at 1090 MHz cannot share, and no `adsb_decode` job already there with the handler wanted (a second start is a `duplicate`).

**Start** the decoder with `needs: ["packet"]`, so the policy passes over any handler without packets. `handler: null` runs the policy's pick; `"onair"` or `"dump1090"` runs that one (a job of each may run at once on one band).

```json
{"op":"job_start","kind":"adsb_decode","source":"<source>","center_hz":1090000000,"bandwidth_hz":2400000,"deviation_hz":null,"sideband":null,"dwell_ms":null,"average":null,"bits":null,"method":null,"bin_hz":null,"audio_policy":null,"dwell_scale":null,"elastic":false,"replace":[],"handler":null,"needs":["packet"]}
```

On the CLI: `zeta job start --kind adsb_decode --source <source> --center-hz 1090000000 --bandwidth-hz 2400000 --needs packet [--handler onair]`.

**Attach.** One op on the decoder's id; the record's `handler` says which handler runs.

```json
{"op":"job_attach","kind":"packets","job":"<adsb job id>","language":null,"stream":null}
```

**Verify.** `packets_list`: the capture's `link_type` is `mode_s` and `packets` climbs as aircraft are heard. Install the dissector once on the machine that runs Wireshark, then download and open:

```bash
mkdir -p ~/.local/lib/wireshark/plugins && cp docs/wireshark/zetasdr.lua ~/.local/lib/wireshark/plugins/
curl -O -H "Authorization: Bearer $ZETA_SECRET" http://<node>/captures/<id>.pcapng
```

Each packet is one Mode S frame under `DLT_USER 148` (USER1), dissected as ZetaSDR Mode S: downlink format, capability, address, ME and type code, parity; the info column reads `DF17 <address>`. `tshark -X lua_script:docs/wireshark/zetasdr.lua -r <id>.pcapng -T fields -e zetasdr_modes.df -e zetasdr_modes.aa` lists them without installing it. The comment names the job and its handler.

**Stop.** `job_stop` on the packets job, then on the decoder if it is done.

**Gotchas.** A packet takes the time the handler's events take, the start of the latest block of samples fed to it, so frames heard within one block share a time. dump1090's Mode A/C replies are read and dropped; only Mode S frames are captured. OnAir writes Mode S as `USER0`, and its handler file names that `mode_s`: both handlers' captures read alike. Without `needs`, a policy that ranks a handler with no packets first runs it, and the attach is refused naming the handler that has them.

## Compare two decoders

Not run live when written: two handlers of one mode on one band, side by side. Needs a mode with more than one ready handler (`modes_show <mode>`: `handlers` with their `state`), such as `adsb_decode` with `dump1090` and `onair`, and a radio on the mode's band.

**Check.** `job_list`: a running job of the mode, with a handler of its own. The sibling needs the other handler `ready` and none of the mode's jobs already on it (a second start is a `duplicate`).

**Start the sibling.** One op; kind, source and band come from the running job, and nothing is retuned or stopped.

```json
{"op":"job_start","beside":"<job id>","handler":"onair"}
```

On the CLI: `zeta job start --beside <job id> --handler onair [--needs packet]`. On the page: the job's menu, **Send to** and the handler's name (unavailable handlers are greyed with their reason).

**Verify.** `job_list`: both records carry the same `group`. On the page the two fold as one group, each row with its handler, `N/min` (events in the last 60 s) and `N subjects`. In Subjects an aircraft both hear is one row and one marker; a field only one handler gave is drawn in that handler's mark, a field they give differently carries the `differs` mark, and a legend under the heading names the sources.

**Compare.** Leave both running for the same span, then read each row's `N/min` and `N subjects`, and count the subjects only one heard. Numbers are never compared by the page; read them side by side.

**Move or stop.** A retune from either member's menu moves the whole group, ids kept. Stopping is per job: the stopped job's rows and marks stay.

**Gotchas.** A node rolled back past this build resumes no jobs (the records carry `group`). The marks' colours and styles come from the theme: `--c-handler-1..4`, `--handler-style-1..4`, and `handlers` in `theme.json` pins a handler to a slot.

## Check a decoder against a recording

Not run live when written: replay a stored capture through a mode's handler instead of waiting for the air. Needs a capture in `zeta capture list` wide enough for the mode (a recording too narrow is refused by the mode's own range error).

**Capture.** Record a burst capture of the band while it is busy, or use one already listed: `zeta capture list` gives each capture's id, centre, width, time and subjects.

**Replay.** One op; the capture's whole set is played once, in real time, from its first sample, and nothing is retuned or leased, so live jobs are not disturbed.

```json
{"op":"capture_decode","capture":"<capture id>","kind":"adsb_decode","handler":null,"packets":false}
```

On the CLI: `zeta capture decode --capture <id> --kind <mode> [--handler <h>] [--packets]`. On the page: Explore's capture row, **Send to** and the mode's name (modes that do not fit are greyed with their reason).

**Read the note.** The replay ends `stopped` with a note of how many events it decoded, and `job_context.replay` names the captures and the radio that recorded them. A local `zeta capture decode` waits for the replay to end and prints its record, note included; with `--node` it prints the job at once, and `zeta job context --job <job id>` reads the note once it has ended. A silent replay says the decoder found nothing in that recording, not that the band is empty.

**Compare two handlers.** Replay the same capture once per handler (`--handler dump1090`, then `--handler onair`), and read each job's events and `N subjects`. The replays of one recording (whichever capture of its set each names) are one group on the page, titled from the set's first capture and marked per handler as in "Compare two decoders", and a replay of the same recording, mode and handler while one runs is a `duplicate`.

**Events and times.** A replay's events go only to the `replay` feed, never a radio's feed or `events.log`; every decoded event carries `replay: <capture>`, and its events carry the recording's own time in `t_ns`: read it against the capture's `start_ns`, not the clock.

**Packets.** `--packets` starts the packet capture before the first sample (attaching `packets` to a running replay would miss what was already decoded); open it in Wireshark as in "Packet capture for Wireshark". The handler must output `packet`.

**Gotchas.** A replay proves nothing live: it supersedes no signature, tallies nothing at the station and uploads nothing. It never resumes: after a restart, run it again.

## A second opinion on a decoder

Not run live when written: a mode's other handler (OnAir, through `onair-rx` 0.3.0 installed by the operator) run beside a job of the same mode, and its frames read in Wireshark for the modes that give them. The modes OnAir serves are `adsb_decode`, `pocsag_decode`, `flex_decode`, `acars_decode`, `aprs_decode`, `ais_decode`, `vdl2_decode`, `hfdl_decode`, `rs41_decode`, `dfm_decode` and `m20_decode`, and, events only, `rds_decode`, `dmr_decode`, `p25_decode`, `m17_decode` and `dvbt_decode`. `video_decode` has no OnAir handler: no one input rate both locked a full FPV swing and kept real time on an empty channel (`TODO.md`, plan 46).

**Check.** `modes_show <mode>` (CLI `zeta modes show <mode>`, or `zeta handlers list`): `handlers` lists `onair` with its `state`, and its `outputs`. `ready` needs `onair-rx` 0.3.0 on `PATH` or named by `ZETA_HANDLER_ONAIR_RX`. An older wrapper is still `ready` in `handlers list`, but a job it cannot serve ends at launch with exit 2: 0.2.0 has none of RDS, DMR, P25, M17 and DVB-T (`onair-rx: no mode rds`), and 0.1.0 has no `--freq`, which every entry but ADS-B's passes, so it ends with the wrapper's usage text; both were read from the wrappers' source, not run. For RDS and DVB-T OnAir gives as much as the row's own decoder, so a node-wide ranking naming `onair` picks it: on a host so ranked, a node deployed before 0.3.0 is installed fails every policy-chosen RDS and DVB-T job, a watch's and an identify's children included; install 0.3.0 before the node, or clear the node-wide ranking first. Only ADS-B, APRS and AIS list `packet`.

**Start** by OnAir beside a running job of the mode, which leaves the radio where it is:

```json
{"op":"job_start","beside":"<job id>","handler":"onair"}
```

Or on its own band, `{"op":"job_start","kind":"pocsag_decode", ... ,"handler":"onair"}` (the other fields as in "Aircraft frames for Wireshark"). On the CLI: `zeta job start --beside <job id> --handler onair`. Read the two as in "Compare two decoders".

**The wide input costs CPU.** OnAir's receivers need more rate than the row's channel has: the node resamples the narrow channel up to what the entry asks for, 1 MHz for POCSAG, FLEX and ACARS, 2 MHz for the sondes, 250 kHz for AIS and VDL2, 240 kHz for APRS, 24 kHz for HFDL. The only figure measured is one `zeta modes check pocsag_decode` run: 0.36 s of user CPU by OnAir against 0.06 s by the row's own decoder, over about 1.4 s of wall time, with the check feeding faster than real time. That is a total of one run, not a rate; the live cost is open (UAT 45.5). Watch `job_list`'s rows and the node's CPU before leaving a 1 MHz sibling running.

The entries for RDS, DMR, P25, M17 and DVB-T ask 500 kHz, 48 kHz, 250 kHz, 250 kHz and 10 MHz (DVB-T's is the row's own rate). Measured through a scratch node on the build host (12 cores, a release build, 30 s of a looping capture fed in real time), `onair-rx` took, per second: RDS 0.23 s, DMR and P25 0.02 s, M17 0.04 s, on the checks' captures; DVB-T 0.87 s locked on a generated 8 MHz multiplex (spread over OnAir's worker threads, 0.63 s on the thread that reads the input) and 0.63 s searching on noise. The node's own share was 0.24 s beside DVB-T (the 10 MHz conversion) and at most 0.02 s beside the others, and no job skipped any input. How much margin DVB-T leaves on a host with two cores is unmeasured. A handler entry has no notes field, so these figures live here.

**Packets for AIS and APRS.** Start with `needs: ["packet"]` (CLI `--needs packet`) and attach as in "Packet capture for Wireshark". AIS arrives as NMEA 0183, each sentence in raw IP and UDP to port 10110 (link type 101): Wireshark opens it as IP and UDP with the sentence readable in the payload (Wireshark 4.2 has no NMEA dissector; whether a newer one decodes it is unverified); APRS arrives as AX.25 (link type 3), read by Wireshark's own AX.25 dissector (the node's Lua dissector is not involved). A digipeater's copy of an APRS frame is a packet with no event.

**No speech and no picture.** A handler job outputs events, and packets where its `outputs` say so, never audio, pictures or a transport stream: a DMR, P25 or M17 call by OnAir is listed, not heard, and a DVB-T multiplex is described, not shown. Attaching a transcriber to such a job is refused (`job <id> has no audio stream`); a start with `needs: ["audio"]` passes over OnAir, and one that also names it is refused (`dmr_decode: handler onair outputs no audio`). OnAir's DMR and P25 events carry `audio: false`, and `silent` only as `encrypted` on an encrypted call. `onair-rx` decodes no speech at all (its own voice monitor refuses every call), so it needs neither mbelib nor Codec 2. To listen, run the row's own decoder: the node-wide ranking does not prefer a handler that lacks an output the row's own decoder gives (`handlers/README.md`), while a ranking for the mode or a named `handler` still runs OnAir.

**What RDS, DMR, P25, M17 and DVB-T give by OnAir.**

- RDS: an event per group whose block B and PI came with a right checkword (a group OnAir had to correct is skipped, so a weak station may give fewer events than by the row's own decoder): `pi`, `group`, `pty`, `tp`, and `ps` and `rt` once each is complete, in UTF-8.
- DMR, P25 and M17: an event for a call once its source, destination and colour code (DMR) or NAC (P25) are known (M17: its source, destination and type), another when a field of it changes (a talker alias that comes late), and a last one with `eot` true. DMR's talker alias is `alias`, a field the row has not. A direct-mode handheld's call has `slot: 1` from OnAir, where the row's own gives `slot` null for a handheld. DMR also gives a `channel` event when a channel is on the air with no call (a repeater between calls), P25 when a control channel is: written when it first holds, and again only if what it says changes, as the row's own does. Not given: DMR's `origin` and `fid`, P25's `mfid`, M17's `codec`, and the frame and error counts.
- DVB-T: an event on lock and on any change of signalling, and one at least every 2 s: `locked`, `lock`, and while locked `bandwidth_hz`, `mode`, `guard`, `constellation`, `code_rate`, `hierarchy`, `lp_code_rate` and `snr_db`, with `services`, the multiplex's services by `id`, `name` and `kind` (`tv`, `radio`, `data`), which the row's own program does not give. Not given: `cell_id`, the offset, clock and ripple figures and `resyncs`. The job's bandwidth does not reach the wrapper, so it finds the channel width itself, 8, 7, 6 then 5 MHz for 2 s of input each: a multiplex that is not 8 MHz wide gives `locked: false` as its first event and is first locked seconds in (see the gotchas).

**What differs from the row's own decoder.** ACARS position and `alt_ft` come from the row's `derive` on the text; `crc_ok` is always true (OnAir publishes only messages whose CRC fitted). AIS listens on the channels AIS 1 and AIS 2 only, not the long-range channels 75 and 76; every message type on them decodes (type 5 included). VDL2 has no text, registration, flight or label from OnAir, so those fields are empty, and `frame_type` is OnAir's word (`I`, `GSIF`). Of the row's fields HFDL gives only `icao` and `pdu` (plus its own `snr_db` and `uplink`), and OnAir's `pdu` words differ from dumphfdl's (`Logon request` against `Logon request (normal)`). APRS has no `data_type`, course, speed, altitude, symbol or comment; non-UI AX.25 frames also give events; an object's, item's or third-party packet's position is left out (it is not the sender's); a repaired frame has `fcs_ok` false. The sondes have no `frame`, `datetime` or `heading`, so their events carry the receive time; OnAir gives one event per sonde per 5 s. `onair-rx` writes these modes' text above 0x7e as Latin-1 characters, so a UTF-8 comment shows garbled; RDS's, DMR's and DVB-T's text is UTF-8.

**Gotchas.** `hfdl_decode` and `dfm_decode` entries are validated but were not checked against a capture. `dvbt_decode` has no check on the node (its row has no synthesised capture): a test pins its field map against two recorded lines of the wrapper, and the wrapper's own test locks it on OnAir's generator. A short replay (`capture_decode`) can lose the last message, since `onair-rx` does not flush at the end of its input; RDS, DMR, P25, M17 and DVB-T are read once more at the end instead (DVB-T: a replay shorter than 2 s that never locks gets its one `locked: false` event there). A DVB-T multiplex that is not 8 MHz wide gives `locked: false` as its first event and needs more than about 2.3 s (7 MHz) or 4.3 s (6 MHz) of capture to show a lock that is there, measured on the wrapper's generated multiplexes at 10 MHz (5 MHz was not measured): a `capture_decode` replay of a few seconds can miss it. The entries take `cs16`, which clamps at full scale: a radio's samples fit, but a file capture recorded above full scale (a synthesised one at unit power) is clipped on the way in, and DVB-T then locks with a low `snr_db` and no `services`. The node counts no dropped input for a handler: one that falls behind shows only as `input lagged, N frames skipped` on the node's stderr.

## Modes only OnAir decodes

Verified on the plan 47 build against generated signals only; no live traffic was heard (UAT 47). The spec is `docs/superpowers/specs/2026-11-23-onair-only-modes-design.md`.

Eleven rows have no decoder of their own (`decoder: null`): `zigbee_decode`, `thread_decode`, `wisun_decode`, `ble_decode`, `lorawan_decode`, `zwave_decode`, `wifi_decode`, `tetra_decode`, `gsmr_decode`, `lte_decode` and `dvbs_ip_decode`. Only OnAir, through `onair-rx` 0.4.0, runs them, so each reads `ready` while the wrapper is there and `unavailable` otherwise, with the reason `no decoder of its own, and no handler for it is ready` and the missing program's line. Each carries `content: network_traffic`, a fact about the row that nothing reads yet (a policy will; see `TODO.md`, "Policies"); it shows on `modes_list`, in a job's context and in a packet capture's record. Their point is the packets: a capture job writes them for Wireshark.

**Install first.** Put `onair-rx` 0.4.0 on the operator's `PATH` (or name it with `ZETA_HANDLER_ONAIR_RX`) before deploying a node build with these rows. An older wrapper is probed `ready`, and a job then ends at launch with `onair-rx: no mode <m>` and exit 2. It needs no FFmpeg at run time; for DVB-S it sets `DECT2_NOGPU`, and OnAir prints one stderr line saying its GPU decoder is off.

**Check.** `zeta handlers list`: `onair` is `ready`, and `zeta modes show ble_decode` lists it under `handlers` with `packet` among its `outputs`; a row that is not ready says why in `reason`. `zeta modes check <id> --strict` runs the row's check through the handler the policy picks (`<id>@onair: ok`) on a generated pair (`scripts/onair-fixtures.sh`, which needs `onair-gen` and `jq`; the pairs are megabytes and never committed).

**What each gives.** Every mode needs the signal's channel at the job's centre.

| row | one job reads | events | packets | not decoded |
|---|---|---|---|---|
| `ble_decode` | one advertising channel (2402, 2426 or 2480 MHz) | a device: address, name, signal | Bluetooth LE | connections (not followed), other PHYs |
| `zigbee_decode`, `thread_decode` | one 802.15.4 channel at the centre | a frame: type, PAN, addresses | IEEE 802.15.4; Thread adds its own analysis | secured payloads (no key); the sub-GHz PHY is untested |
| `lorawan_decode` | every channel of one regional plan inside 2 MHz | a packet: device address, spreading factor, frequency | LoRaWAN | payloads (no key) |
| `zwave_decode` | every channel of the region inside 2 MHz | a frame: home id, source node | Z-Wave (a user link type, below) | S0 and S2 payloads |
| `wisun_decode` | every channel of the band plan inside 2 MHz | a frame: source address | Wi-SUN FAN | 200 and 300 kbit/s; encrypted frames stay so |
| `wifi_decode` | 20 MHz at one centre | a beacon, probe response or FILS discovery: BSSID, SSID | every good frame, as 802.11 with radiotap | the higher rates, encrypted data |
| `tetra_decode`, `gsmr_decode`, `lte_decode` | one carrier (TETRA and GSM-R 2 MHz, LTE 20 MHz) | a cell when first read and when what is known of it changes: identity and system information | the cell's broadcast channels, as GSMTAP over UDP | traffic, anything addressed to a terminal, paging |
| `dvbs_ip_decode` | one DVB-S or DVB-S2 carrier at the IF, 10 MHz | a status line: lock, standard, symbol rate, datagram count | whole IP datagrams from GSE, MPE and ULE | DVB-S2X, the transport stream itself, encrypted payloads |

No key is loaded or passed: frames are given as received. A packet's time is the first sample of the block it was drained after; for TETRA, GSM-R, LTE and DVB-S it trails the frame by up to a stretch of input (up to 2.3 s for TETRA; the wrapper's README gives each).

**Start** from the page (the box menu's Decode entry: greyed with the row's reason when nothing can run it, hidden when the node does not list the row) or

```json
{"op":"job_start","kind":"ble_decode","source":"<source>","center_hz":2426000000,"bandwidth_hz":2000000,"needs":["packet"]}
```

On the CLI: `zeta job start --kind ble_decode --source <source> --center-hz 2426000000 --bandwidth-hz 2000000 --needs packet`; read the capture as in "Packet capture for Wireshark". A row's band is the standard's own, across regions, with the source in the row's notes (`modes_show`); the plan-based and wide rows list no centres, so give the one you measured.

**One channel per job.** Bluetooth LE, Zigbee and Thread read one channel; a second channel is a second job. The wide ones take what the rate holds: Wi-Fi and LTE need 20 MHz, about 80 MB/s into the wrapper, so start them only on a source that holds that rate without dropping. Read the node's dropped-sample count and its stderr for `input lagged, N frames skipped`.

**Cost.** Measured on the build host (12 cores, a release node, a looping generated pair in real time for 30 s, no dropped input), `onair-rx` and node cores per job: BLE 0.15 and 0.29, LoRaWAN 0.24 and 0.08, Zigbee 0.58 and 0.28, Thread 0.57 and 0.28, Z-Wave 0.03 and 0.08, Wi-Fi 0.46 and 0.46, Wi-SUN 0.11 and 0.08, TETRA 0.10 and 0.08, GSM-R 0.51 and 0.08, LTE 1.08 and 0.45, DVB-S IP 1.02 and 0.25. The host was loaded by other builds, so these are upper bounds. A host with two cores is unmeasured.

**LTE.** The wrapper does not keep real time at 20 MHz on the build host, so `lte_decode`'s entry passes `--drop`: the receiver skips the 0.2 s blocks that arrive while it is busy, as it does live, and decodes each block it takes whole. `capture_decode` replays at real time, in blocks of 50 ms: measured on the build host, a replay of the generated 20 MHz capture (0.9 s, and 7.2 s of it looped) gave one event, the cell whole (MCC, MNC, TAC, cell identity, SIBs 1 to 5), with no input dropped by the wrapper. A short replay is read from its first block, since a block takes the wrapper about 2.8 s at 20 MHz and those that come meanwhile are skipped; a feed faster than real time loses most of its input. The handler takes 20 MHz and about 1.1 cores while a job runs, even for a 1.4 MHz carrier.

**DVB-S IP is tried only when named.** The row has no range or centre, so identify, `classify`'s candidates and a watch's default modes pass over it (the row field `identify.when_named`); name it (identify's `candidates`, a watch's `modes`) or start it by hand and it runs anywhere. The entry passes `--width $bandwidth_hz`, the job's band width, which bounds the receiver's symbol-rate search.

**Z-Wave in Wireshark.** OnAir writes Z-Wave as link type `USER1` (148), but 148 is the node's Mode S, so the node writes Z-Wave packets as `USER2` (`LINKTYPE_USER2`, 149). Wireshark has no Z-Wave dissector. Run `onair-rx --lua > onair.lua` and load it as a Wireshark Lua plugin, then open Preferences, Protocols, DLT_USER, Encapsulations Table, and add an entry: DLT `User 2 (DLT=149)`, payload protocol `onair_zwave`. The dissector's name is confirmed from `onair-rx --lua`'s output; these DLT_USER steps were not tried in Wireshark when written. The plugin also registers `onair_zwave` for USER1, as the node's `docs/wireshark/zetasdr.lua` registers its Mode S dissector: with both loaded, which of the two Wireshark applies to USER1 was not checked, so open a Mode S capture and check that it still reads as ZetaSDR Mode S. The other ten modes open as their own protocols with no setup (802.11 with radiotap, 802.15.4, Bluetooth LE, LoRaWAN, GSMTAP).

**Verify.** The job's record names `handler: onair` and its context carries `content: network_traffic`; events appear on its feed; the capture opens in Wireshark as the protocol in the table. With `onair-rx` moved aside the rows read `unavailable` with the reason, and their Decode entries are greyed.

**Gotchas.** The Bluetooth LE line is the device only (address, name, signal), not the PDU type, channel or advertising data; the capture has them. A cell's line for TETRA, GSM-R and LTE is not repeated while only the measured frequency, SNR or network time moves, so a steady cell is quiet. Wi-SUN holds its last frames until the end of input, and the wrapper flushes them then. Where a receiver has no switch for something (Zigbee's sub-GHz PHY, Wi-Fi's rate), the wrapper's README and `UPSTREAM.md` say so.

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
