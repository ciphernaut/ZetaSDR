# Changelog

## 0.1.0 - unreleased

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
