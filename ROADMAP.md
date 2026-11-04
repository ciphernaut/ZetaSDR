# Roadmap

This is the public view of where ZetaSDR is going. It promises no dates. [TODO.md](TODO.md) is
the working list of gists (bugs, ideas, deferred minors) this page is drawn from; the design
history is in [docs/superpowers](docs/superpowers/).

## Now (0.x)

The project is pre-1.0: operations and their fields may still change between releases. Work in
hand, from the open items in TODO.md:

- More decoding modes as rows: the VDL Mode 2 and HFDL aircraft data links, AIS, FLEX pagers, and
  digital voice, each a mode row plus a decoder rather than new code in the node.
- Scanning and detection on real hardware: a per-device spur list that the scan and the band watch
  mask, keeping a lone job's capture centre off its channel, and the HackRF's read timeouts at low
  sample rates.
- The web page: per-device layouts, view policies kept in the browser, and the deferred minors of
  the theme work.
- Station and clock: a GPS fix from the B210 as the node's station, and disciplining the B210 to
  its GPS module.
- The agent guide: leading the operator with the map and the one-line `summary`, and guidance for
  each new mode, including when not to touch running jobs.
- Pull requests open at 0.2.0 (see [GOVERNANCE.md](GOVERNANCE.md)).

## Seeking funding

Four tracks that are larger than one maintainer's spare time. Each says what exists today and what
the track would add.

### Amateur digital and voice modes

Today: decoders are data rows (`modes/*.json`: APRS, POCSAG, RDS, ACARS, ADS-B, and the RS41, DFM
and M20 radiosondes), in-process in `crates/zeta-dsp` or as external programs through
`crates/zeta-ext`. A new mode is a row, its decoder and a synthesised test signal checked by
`zeta modes check`. No FT8, WSPR, CW, M17 or satellite AX.25 row exists yet.

Would add: rows for FT8/FT4 and WSPR (HF through a KiwiSDR, with the WSJT-X programs run as
external processes), CW, M17 with Codec 2 (an open vocoder, so its audio can feed the existing
`transcribe` attachment; Codec 2 is LGPL-2.1, so it would run as an external handler, see
[docs/licensing.md](docs/licensing.md)) and AX.25 from amateur satellites, each with its
synthesised test.

### A multi-stream FPGA source over VITA 49.2

Today: `crates/zeta-vrt` encodes and decodes VITA 49.2 packets and `crates/zeta-source-vrt`
receives them over UDP as one source, following one sender: the stream id of the first context
packet is pinned and packets of other streams are dropped. `Backend::Fpga` exists in
`crates/zeta-core/src/stage.rs` as a rank in the backend order, with no implementation behind it.

Would add: a source that takes several VRT streams from one sender (one per channel of an FPGA
channeliser) as channels of one source, an open reference design for the sender, and a first stage
kind with an FPGA backend, so that the "best implementation of a stage" choice has something to
choose between.

### A layer-3 IQ classifier with an amateur-only dataset

Today: `classify` is a ladder. Layer 1 measures the band, layer 2 scores the signal catalogue and
the decoder rows and labels a signal only on a CRC-clean decode, layer 3 is an empty slot
(`StubClassifier` in `crates/zeta-core/src/classify.rs`, which labels nothing).

Would add: a classifier in that slot, trained on a labelled IQ dataset recorded and released by the
project. Labels come from CRC-proven decodes, so they are measured and not guessed, and the dataset
holds amateur transmissions only. Weights would be released with a model card; an unknown signal
would stay reported as unknown.

### An agent evaluation suite

Today: the node's operations are a typed set an LLM calls through `zeta ask`, `POST /ask` or
`zeta-mcp`, driven by the guide in [AGENTS.md](AGENTS.md) (`crates/zeta-agent`). The rules in it
(do not stop an operator's jobs, report unknown as unknown, use the id the error suggests) are
checked by unit tests; an opt-in live test (`crates/zeta-agent/tests/live.rs`, given an LLM's URL)
drives a real model through one request, but outcomes are not yet scored across models.

Would add: a suite of replayable requests against recorded sources with scored outcomes (right
operation, right fields, no forbidden stop, no invented id), so that a model, a prompt or a change
to the guide can be compared on the same tasks.

## Not planned

Recovering the content of third-party links. The playbook for an unknown signal stops at
measuring, classifying, capturing a few bursts to identify it and reporting who the licence
register says transmits there. Decoders for public protocols are in scope; reading someone else's
private traffic is not.
