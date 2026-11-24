# Changelog

## Unreleased

- Modes only OnAir decodes: eleven rows with no decoder of their own, run by OnAir through
  `onair-rx` 0.4.0 and by nothing else: `zigbee_decode`, `thread_decode`, `wisun_decode`,
  `ble_decode`, `lorawan_decode`, `zwave_decode`, `wifi_decode`, `tetra_decode`, `gsmr_decode`,
  `lte_decode` and `dvbs_ip_decode`. A row may now have `decoder: null`: it reads `ready` while a
  handler file's entry for it is, and `unavailable` otherwise with the reason `no decoder of its
  own, and no handler for it is ready` (then each entry's own reason); `zeta modes check <id>`
  checks the handler the policy picks, on a SigMF pair that `scripts/onair-fixtures.sh` has
  OnAir's generator make. Each gives events and packets for Wireshark (Z-Wave's are
  `LINKTYPE_USER2`, 149, since 148 is the node's Mode S), has a Decode entry in the page's box
  menu (greyed with the row's reason when nothing can run it), and carries a new row field,
  `content` (`network_traffic`), shown on `modes_list`, in a job's context and in a packet
  capture's record; nothing reads it yet. A program's arguments take `$bandwidth_hz`, the job's
  band width (the DVB-S IP entry passes it as `--width`). A row may set `identify.when_named`,
  so that identify, classify's candidates and a watch's default modes pass over it unless named
  (DVB-S IP, which has no range). LTE is listed with `--drop` because it does not keep real time
  without it; a replay by `capture_decode` is at real time and reads a cell as a live job does.
  Install `onair-rx` 0.4.0 before deploying a node with these rows: an older wrapper is probed
  `ready` and the new modes then fail at launch with `onair-rx: no mode <m>`. A node with no
  `onair-rx` lists the rows `unavailable`, and nothing else changes. The dev profile now
  optimises `zeta-ext`, whose input conversion fell behind real time at 4 and 20 MHz.
- Sets: a serving node reads its operator sets directory (`--sets-dir`) again whenever the files
  there have changed, so a set file added, edited or removed is in force at the next `set_list`,
  `set_start` or `set_stop` with no restart. A set that is running keeps the jobs it started:
  `set_stop` stops them whatever its file now says, and the next `set_start` runs the file as it
  stands. A set an edit breaks is listed `broken` and reported on stderr once.
- OnAir serves ten more modes: `handlers/onair.json` gains entries for `pocsag_decode`,
  `flex_decode`, `acars_decode`, `vdl2_decode`, `hfdl_decode` and the sonde rows `rs41_decode`,
  `dfm_decode` and `m20_decode` (events), and for `aprs_decode` (AX.25) and `ais_decode` (NMEA
  over UDP port 10110), which also give packets for Wireshark. Each entry runs `onair-rx` on the
  row's narrow channel resampled up to the receiver's own minimum rate (1 MHz for the pagers and
  ACARS, 2 MHz for the sondes, 250 kHz for AIS and VDL2, 240 kHz for APRS, 24 kHz for HFDL), so a
  job by OnAir costs more CPU than the row's own decoder; one `modes check` run of
  `pocsag_decode` took 0.36 s of user CPU by OnAir against 0.06 s by the row, a total of one
  run and not a rate. `zeta modes check <id> --handler onair --strict` passes for every one of
  them but `hfdl_decode` and `dfm_decode`, which have no fixture or test to check against.
  `onair-rx` 0.2.0 is needed: an older wrapper (0.1.0) has no `--freq`, which every new entry
  passes, so such a job ends at launch with the wrapper's usage text and exit 2, while `handlers
  list` still says `ready` (read from the 0.1.0 source, not run). A node with no `onair-rx` is
  unchanged.
- A handler entry may carry `const`: row event fields given a constant value on every event, after
  the field map (see `handlers/README.md`). A constant for a key the event envelope sets
  (`protocol`, `handler`, `summary`, ...), for the subject key, for a field the program also
  gives, or of a type the row's schema refuses fails that entry alone. OnAir's ACARS entry sets
  `crc_ok` true: OnAir publishes a message only when its CRC fitted.
- Replay: `capture_decode` plays a stored IQ capture's whole set, once and in real time, into a
  decoder mode's handler, to check a decoder against a recording or compare two handlers on it.
  CLI `zeta capture decode --capture <id> --kind <mode> [--handler <id>] [--packets]`. The job
  holds no lease and tunes no radio, ends with a note of what it decoded, never resumes, and
  proves nothing live (no signature, session, station tally or upload). Its events go to the
  `replay` feed, which no source may be named, carrying `replay` and the recording's own times;
  `packets` writes a packet capture from the first sample. The packet reader of a program job
  now drains what its decoder wrote before it stopped, which a short burst needs. A node rolled
  back past this build reads every record that is not a replay.
- Send to: `job_start` takes `beside`, a running job of a mode, and starts a sibling on the same
  band and source by another handler, to compare two decoders on one signal or to capture packets
  from a handler that gives them. Nothing is tuned or stopped; both records carry `group`, a
  `job_retune` of either moves the whole group under one gate, `job_stop` stops one, and a
  relaunch never puts two members on one handler. CLI `zeta job start --beside <job> --handler
  <id>`, which reads the kind and band from the job. A node rolled back past this build resumes
  no jobs (records carry `group`).
- Packet capture for Wireshark (phase A): `job_attach` of `packets` writes an APRS job's AX.25
  frames (`LINKTYPE_AX25_KISS`) or a frame row's frames (`LINKTYPE_USER0`) to a pcapng under the
  state directory, each packet with the frame's own time and a comment naming the job, source and
  frequency. `packets_list` and `packets_delete` (CLI `zeta packets`), a download at
  `/captures/<id>.pcapng` while the file is written, a Capture packets entry on a capable job's
  menu and a list in Explore. A capture does not resume on a restart.
- Every APRS or frame-row job now lists a `packet` stream at `/streams`, which an older
  `zeta-client` cannot decode: upgrade the peers that read this node's streams (another node's
  `--source-node`, `zeta-mcp`) together with it.
- Handlers: a mode may be decoded by more than its row's own decoder. A handler file
  (`handlers/*.json`, and the operator's from `--handlers-dir`) adds a program to each mode it
  lists, its output mapped onto the row's events plus fields of its own (`extra`); a broken file
  or entry is reported at startup and never stops the node. A handler policy picks one
  (`handler_policy_get`/`handler_policy_set`, CLI `zeta handlers policy`, kept across restarts),
  and `job_start` takes `handler` to name one and `needs` (`["packet"]`) to pass over handlers
  without that output. `zeta handlers list` prints each mode's handlers, and `zeta modes check
  --handler <id>` checks one.
- `modes_list` and `modes_show` gain each mode's `handlers` and `handler` (the policy's pick now);
  the mode is `ready` when any handler is, its `program`, `reason` and `missing` describe that
  pick (or the row's own decoder when none is ready) and its `outputs` are the union over the
  ready handlers, so a mode with one handler reads as before. Job records gain `handler`,
  `handler_named` and `needs`, every decoder event gains `handler`, and a packet capture's comment
  names it.
- Packets from programs: a program row's or handler entry's `packets` block reads a second
  output, a pcap stream or dump1090's Beast output, into the job's packet stream. The ADS-B row
  reads dump1090's Beast output, so an `adsb_decode` job takes a packet capture: each Mode S frame
  is a packet under the private link type `mode_s` (`DLT_USER1`, 148), which
  `docs/wireshark/zetasdr.lua`, a small Lua dissector, reads in Wireshark. A link type is now any
  link-layer number, the ones the node knows by name.
- OnAir for ADS-B: `handlers/onair.json` ships with the node and runs `onair-rx`, a wrapper the
  operator installs (GPL-3.0-or-later, kept outside this repository), for events and packets
  beside dump1090.
- Peers older than this refuse the link types they do not know (`mode_s`, a number without a
  name) in a stream's frames and the new fields of `modes_list`'s answer: upgrade them together
  with the node.
- Every `adsb_decode` job now opens a second local port to dump1090 (`--net-bo-port`) for its
  Beast output, which matters to an operator whose own ADS-B row passes dump1090 its ports. An
  older CLI or peer does not know the `no_handler` error or the `handler_policy` result; this
  build's CLI still reads an older node's `modes list` and `modes show` (as no handlers).
- Rolling back past this build loses state. Once this build has run, the previous build resumes
  no jobs: every job record this build writes carries `handler`, `handler_named` and `needs`
  (every launch, resumes included, rewrites the record), and the previous build skips a record
  with fields it does not know, naming its file. One packet capture whose link type the previous
  build has no name for (`mode_s`, so any ADS-B capture, `ax25` or any other number) makes it
  move the whole packet index aside (`index.json.broken-<id>`, with one `index unreadable` line):
  every capture stops being listed, though the files stay on disk. `handler-policy.json` and the
  `<mode>@<handler>` keys of `modes-checked.json` are harmless: the previous build ignores them.

## 0.2.0 - 2026-11-17

- More decoders as rows: AIS in-process; FLEX over multimon-ng; M17 with its speech through
  `c2dec`; the VDL2 and HFDL aircraft data links as program rows, with fields mapped from a row's
  `output`; DVB-T signalling through `dvbt-rx`, a separate GPL wrapper (not yet published) around
  SDRangel's `modemdvb`.
- Digital voice: P25 phase 1 and DMR (both time slots, with an audio policy) report each call's
  talkgroup, radios and encryption. Their speech needs `zeta-mbe`, a small wrapper around
  mbelib-neo in [its own repository](https://github.com/ciphernaut/zeta-mbe), which
  `tools/install-vocoders.sh` builds on the operator's machine. No codec is shipped, and
  encrypted calls are never decoded.
- Layer 4: `frame_decode`, thin serial-protocol rows, and signatures the operator accepts written
  back.
- Positions: ACARS positions read from a message's text, ground stations as origins, a position
  per source (a KiwiSDR's own, and a remote node's through a node source), and a Map tab with the
  register's origins and tiles from five providers.
- Span decoding: a watch with `modes` decodes every channel of a band, and named sets (`pagers_au`,
  `airband`, `airband_vhf`, `airband_vhf_and_sos`) start across radios in one operation.
- The register's answer for a band: expected levels from EIRP, and network sites.
- The agent's radio follows the page's focus, under a selection policy the user sets.
- Sources: a shared-memory ring between the SoapySDR worker and the node, a probed rate ladder
  and the narrowest lossless sample depth; a fast stepped sweep on any IQ source, and sweeps a
  source runs itself (a KiwiSDR's own waterfall, live on its tab).
- Analog video: `video_decode` (wide FM, line lock), watched live on the page.
- A per-source event feed.
- Jobs: a job a running controller started can't be retuned, replaced or stopped from outside,
  and the refusal names its owner; `job_start` on an unheld radio tunes it once; channel plans
  carry their FM deviation (UHF CB at 2500 Hz).

## 0.1.0 - 2026-11-05

The first public release.

- A node that owns its sources and leases their tuning to jobs: SoapySDR devices in a supervised
  worker process that recovers from unplugs and driver crashes, KiwiSDR receivers, SigMF
  recordings, VITA 49.2 senders over UDP, and the radios of other nodes (federation).
- 36 typed operations, the same from the `zeta` CLI, an authenticated HTTP and websocket API, an
  MCP server (`zeta-mcp`) and an agent (`zeta ask`, `POST /ask`) over any OpenAI-compatible LLM;
  `AGENTS.md` is generated from the schema.
- Jobs as graphs of stages: channelisers and a shared polyphase filter bank, spectrum and wideband
  scan, NBFM, WFM, AM and SSB demodulators with squelch, an adjacent-channel guard and denoising.
- Decoders as data rows: APRS, POCSAG, RDS and ACARS in-process; ADS-B (dump1090) and RS41, DFM and
  M20 radiosondes (rs1729) as external programs; operator rows and `zeta modes check`.
- Measurement and classification: layer-1 features (burst-aware), a 594-signal catalogue, the
  ACMA licence register as a knowledge provider, `identify` and a band `watch` that keeps the
  decoder that proves.
- Unknown signals: burst captures to a SigMF store and a bit chain (demodulator, clock recovery,
  slicer, framing hypotheses) for working them by hand.
- Speech: transcription of any demodulator through a local speech daemon, channel plans (AU UHF
  CB) with one-operation starts, and a mix of every talking channel.
- A web page served by the node: waterfall, job outlines you drag to retune, box menu, map and
  subjects table for positioned events, captures and bits panes, and themes.
- Plugins as external stage processes speaking a small wire protocol, with a Python SDK.
- Uploads to SondeHub only with a callsign; a Raspberry Pi deployment example.
- The layer-3 stub classifier is named `layer3-slot` (it was `sigint-slot`) in `classify` results.
- ROADMAP, GOVERNANCE, a licensing note and a CITATION file.
