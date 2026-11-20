# Changelog

## Unreleased

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
  ready handlers, so a mode with one handler reads as before. Job records gain `handler` and
  `handler_named`, every decoder event gains `handler`, and a packet capture's comment names it.
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
