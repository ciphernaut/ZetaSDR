# Aircraft data links: the VDL Mode 2 and HFDL rows

Design spec, 2026-11-06. From two operator notes in `TODO.md`: "Modes: VDL Mode 2 beside ACARS,
and an airband capture plan" (2026-11-05) and "Feature: VDL Mode 2 and HFDL rows (the other
aircraft data links)" (2026-11-02). A reviewer read it as the builder before hand-over; its
questions are folded in (section 8 lists the calls, section 7 what only the operator can answer).
Builds on the mode registry (`docs/superpowers/specs/2026-10-27-mode-registry-design.md`,
`crates/zeta-core/src/modes/`), the external-program adaptor (`crates/zeta-ext/src/external.rs`),
`zeta modes check` (`crates/zeta/src/modes_check.rs`) and subjects
(`docs/superpowers/specs/2026-11-02-subjects-design.md`).

## 1. Intent and why

Plain VHF ACARS is `acars_decode`, and the field host runs seven of them on the B210. Most
airline traffic has moved on: to VDL Mode 2 on VHF (136.975 MHz worldwide, D8PSK at 10.5 kbaud
in a 25 kHz channel, ACARS carried over AVLC) and, over oceans and outback, to HFDL (ARINC 635,
PSK at 1800 baud in a 2.4 kHz USB channel on a dozen HF frequencies). The catalogue knows both
(`vhf-data-link-mode-2-vdl-m2`, `high-frequency-data-link-hfdl`) with `decoder` null, so
`knowledge_lookup` names the signal and offers nothing to prove it. The register has SITA's
assignments on 136.975 MHz at Brisbane Airport; VDL2 there is busy.

The decoders exist and are installed on the field host: `dumpvdl2` 2.6.0 and `dumphfdl` 1.7.0,
both over `libacars` 2.2.1. The program-row machinery of plan 16 fits them almost as it stands:
IQ over stdin at the rate the program asks for, one JSON object per line back. Two gaps stop a
row today, and both are generic rather than VDL2's:

1. `json_lines` takes the line's top-level object as the event. dumpvdl2 nests everything under
   `vdl2.avlc...` (the ACARS payload under `vdl2.avlc.acars`), dumphfdl under `hfdl.lpdu...`,
   so `summary`, `subject`, `position` and `time` cannot name their fields. Any program that
   prints structured JSON has this problem; rtl_433 and direwolf's JSON do not, which is why it
   has not bitten yet.
2. `dumphfdl` takes its frequencies in kilohertz and refuses hertz (`'8942000': value too
   large`, checked on the field host); the only frequency placeholder is `$center_hz`.

This plan closes both gaps in the row schema and adds two rows, `vdl2_decode` and `hfdl_decode`.
It changes nothing in the node's job machinery: a row is data, and the agent, the page and the
CLI get both modes the way they got the sondes. The fixtures are the one novelty: neither mode
has a synthesiser, so each row's check runs on a short capture recorded off the air.

## 2. Goals, non-goals, anti-goals

### Goals

- A program row can say where in its program's JSON each event field lives, so a nested output
  becomes a flat event with the row's own field names, and `summary`, `subject`, `position` and
  `time` work as on every other row.
- `vdl2_decode`: dumpvdl2 on one 25 kHz channel, events keyed on the AVLC source address with
  the ACARS fields as columns, started like any decoder (`job_start --kind vdl2_decode`,
  `identify`, the page's decode menu), beside the ACARS jobs on the same radio.
- `hfdl_decode`: dumphfdl on one HFDL channel of a KiwiSDR (or any HF source), the same shape.
- Both rows carry a check: `zeta modes check --strict vdl2_decode hfdl_decode` passes on a host
  that has the programs, against fixtures in the repository; on a host without them it says
  `skipped: dumpvdl2 missing`, as `adsb_decode` does without dump1090.
- Both catalogue entries gain their `decoder`, so `knowledge_lookup` at 136.975 MHz answers
  `vdl2_decode`.
- The agent guide says how to run VDL2 beside ACARS on one radio, and how to find an HFDL
  channel on a Kiwi.

### Non-goals

- Positions from VDL2. Some ACARS texts carry one (and CPDLC/ADS-C in the X.25 half); parsing
  message texts is later work. HFDL's structured `pos` is taken, since it is a field, not a
  text (a call, section 8).
- Multi-channel decoding. dumpvdl2 and dumphfdl each decode several channels from one wide
  stream; a row decodes one channel per job, as every row does. Every-channel-in-the-capture
  decoding is the span-decoder work (TODO "Span decoders and trunking").
- The CPDLC, ADS-C, CM and X.25 payloads dumpvdl2 and dumphfdl decode. The rows carry the
  AVLC/LPDU identity and the ACARS block; the rest stays in the program's line and is not an
  event field in this plan (a later row revision can map more; nothing here prevents it).
- Ground-station tables, `--gs-file`, `--system-table`, `--station-id`, feeds to airframes.io
  or similar. The rows run the programs bare.
- The "airband" named set: one call that starts ACARS + VDL2 on one radio and ADS-B on another,
  as `plan_start` does for a channel plan. That is the sibling spec on span decoding and named
  sets, not this one. This plan gives it the rows it will start.
- Merging ACARS and VDL2 into one aircraft table on the page (the operator's "nicer still").
  Section 7 asks; it is page work on top of this plan.

### Anti-goals

- A field map that only VDL2 can use. Pointers into a parsed line are generic; nothing in the
  schema or the adaptor knows dumpvdl2's names.
- A fixture that is large or that carries anything private. The repository is public; each
  fixture is small, reviewed line by line, and holds only link-layer and operational traffic.
- A row that passes its check on the field host and nowhere else without saying why. A missing
  program or a missing fixture is a `skipped` with the reason, and `--strict` fails it.
- A row that works only when the capture is wide enough by luck. The rows state their channel
  width and rate; the node's channeliser and the adaptor's resampler do the rest, bank or no
  bank.

## 3. Constraints

- A row is `deny_unknown_fields`, every field required unless `#[serde(default)]`; errors name
  `<field>: <error>` (`RowError`), and a bad row is `broken`, never a dead node
  (`crates/zeta-core/src/modes/row.rs`). AGENTS.md is generated; prose goes in
  `crates/zeta/src/agents_md.rs` or `docs/agents/field-craft.md`.
- A program row expands to `channel → external_decode` (`modes/template.rs`). The channel
  decimates by an exact integer to `channel_rate_hz` (`ops.rs`): the narrowest exact divisor of
  the source (or bank bin) rate that is at least `max(bandwidth_hz, 48 000)`. The adaptor's
  `Converter` then resamples rationally to `input.rate_hz` (`external.rs`, `Resampler`), so the
  program's rate need not divide anything; a change of input rate restarts the resampler.
- `$center_hz` is the first IQ frame's centre: the channel's, since the channel node mixes the
  job band to baseband. Placeholders are the fixed list `PROGRAM_PLACEHOLDERS` (`$port`,
  `$rate_hz`, `$center_hz`, `$fifo`); an unknown one is `broken` at `decoder.args[i]`.
- The publisher (`external.rs`, `Publisher::publish`) parses each line with the row's parser,
  then `summary::envelope` fills `protocol`, `summary`, `t_ns` and `position` from the flat
  object's top-level keys. `time` must be an ISO 8601 UTC string.
- A subject's `key`, `columns[].field`, `heading`, `label`, the `position` map and the summary
  template must all name `event.schema.properties`, else `broken` naming the field.
- `zeta modes check` opens a node on a file source over the test's SigMF pair; a `fixture`
  test's path is read as given (today: relative to the current directory) and the job centre is
  the meta's `captures[0].core:frequency`. Skips are `no test` and `<program> missing`;
  `--strict` turns every skip into a fail. The file source reads `cf32_le`, `ci16_le` and `ci8`.
- A KiwiSDR source delivers 12 kHz of IQ, blocks stamped at the nominal 12 000 Hz; the true
  rate is a little over (`reported_sample_rate`). `bandwidth_hz` is at most 12 000.
- The B210 capture at 8 MHz gets a polyphase bank of 8 bins (1 MHz spacing, 2 MHz per bin,
  `pfb.rs`); a channel wider than a bin (`TooWide`) is filtered directly from the capture.
  The lease cannot widen a capture under a running rigid job ("widen capture ... not yet
  available", `lease.rs`): a band outside the capture is a conflict naming the holders.
- Checked on the field host (dumpvdl2 2.6.0, dumphfdl 1.7.0, zero-filled input):
  - dumpvdl2's `--sample-format` accepts `U8` and `S16_LE`; `S16LE`, which its `--help`
    prints, and `CS16` are refused as `Unknown sample format`. `--oversample 1` runs
    ("Sampling rate set to 105000 sps"), as does 2 (210 000).
  - dumphfdl runs at `--sample-rate 12000` with `CS16` input, takes frequencies in kHz only,
    and without `--centerfreq` computes the centre from the channel list ("computed center
    frequency: 8942.000 kHz" for one channel).
  - Neither program, nor libacars, is in Debian or Ubuntu. The field host has them from the
    OpenWebRX+ apt repository (`dumpvdl2`, `dumphfdl`, `libacars2`); upstream is
    `github.com/szpajder/{dumpvdl2,dumphfdl,libacars}`.
  - JSON key names seen in the binaries: dumpvdl2 `vdl2`, `app`, `avlc`, `src`, `dst`,
    `frame_type`, `xid`, `x25`, `sig_level`, `noise_level`, `Aircraft`, `Ground station`,
    `Airborne`, `On ground`; libacars `reg`, `flight`, `msg_text`, `crc_ok`, `err`, `more`,
    `blk_id`, `msg_num`, `msg_num_seq`, `sublabel`, `mfi`; dumphfdl `hfdl`, `lpdu`, `spdu`,
    `mpdu`, `hfnpdu`, `ac_info`, `icao`, `flight_id`, `pos`, `lat`, `lon`, `bit_rate`,
    `freq_skew`, `gs_status`. The exact nesting is confirmed against the first real lines
    (section 6); the rows' pointers below are the expected paths.
- The repository is public: fixtures and row notes use roles, not host names or positions
  (the demo station is Brisbane Airport). `scripts/public-check.sh` must stay clean.

## 4. The design

### 4.1 `output.fields`: event field ← JSON pointer

A program row's `decoder.output` gains one optional map:

```json
"output": {
  "via": "stdout", "port": null, "parser": "json_lines",
  "fields": {
    "addr":   "/vdl2/avlc/src/addr",
    "reg":    "/vdl2/avlc/acars/reg",
    "icao":   ["/hfdl/lpdu/src/ac_info/icao", "/hfdl/lpdu/dst/ac_info/icao"]
  }
}
```

- A key is an event field name; it must be a property of `event.schema`, else `broken` naming
  `decoder.output.fields.<key>`. Every schema property must be a key of the map when the map is
  present, else `broken` naming `event.schema.properties.<name>` ("not mapped by
  `decoder.output.fields`"): with a map, the event holds the mapped fields and nothing else, so
  an unmapped property would be a field that never arrives.
- A value is one JSON pointer (RFC 6901, `serde_json::Value::pointer`; must start with `/`,
  else `broken`) or a list of them, the first that resolves to a non-null value winning (the
  `{a?b}` rule of summaries, applied at the source). A pointer that resolves to nothing, or to
  JSON `null`, leaves the field out of the event; `{field?}` in the summary then drops it, and a
  subject key that is absent makes the event "about no subject" (a ribbon, no table row), as
  `row.rs` already defines for a null key.
- The value is copied as it is: numbers stay numbers, strings strings; an object or array is
  copied too (nothing in this plan needs one, and refusing them would gain nothing).
- Applied in `Publisher::publish` between the parser and `envelope`, for every parser: SBS,
  NMEA and the rest produce flat objects, on which `/icao` is the same as today's `icao`, so
  no existing row changes. Without `fields`, behaviour is exactly today's (the whole object).
- Validation lives in `validate_program` beside the placeholder checks; `modes_list` and
  `modes_show` report the map as part of the row, as they report everything.

**Why pointers, not flattening.** Flattening (`vdl2.avlc.acars.reg` as the field name) keeps
every consumer reading the decoder's nesting: the schema, the summary template, the subject
columns and the page's column labels would all spell `vdl2.avlc.acars.reg`, and a dumpvdl2 line
flattens to some sixty keys (`app`, `burst`, `xid` parameters), of which the page and the agent
want ten. A map names what the row promises, with the names the row chooses (`reg`, `flight`,
`label`, `text`: the same words `acars_decode` uses, so the two aircraft tables read alike). It
costs no dependency (`serde_json` has pointers), handles keys with dots and slashes by the
standard's escaping, and the list form gives "src or dst" without inventing syntax. It is also
what a later `time`-from-`{sec, usec}` or a text parser would plug into: the event is a flat
object with known names once the map has run.

### 4.2 `$center_khz`

A fifth placeholder, `$center_khz`: the centre in kilohertz, an integer when it divides, else
with up to three decimals (`8942`, `8942.5`, `136975`). Added to `PROGRAM_PLACEHOLDERS`, to
`substitute`, and to the `args` doc text. It exists because dumphfdl wants kHz; it is generic
(several HF decoders take kHz).

### 4.3 `vdl2_decode`

```json
{
  "id": "vdl2_decode",
  "name": "VDL Mode 2 aircraft data link",
  "family": "aviation",
  "band": {"bandwidth_hz": 25000, "centers_hz": [136975000, 136725000, 136775000, 136825000, 136875000], "min_hz": null, "range_hz": null},
  "modulation": ["psk"],
  "label": "VDL2",
  "protocol": "vdl2",
  "decoder": {
    "kind": "program",
    "programs": ["dumpvdl2"],
    "args": ["--iq-file", "-", "--sample-format", "S16_LE", "--oversample", "1",
             "--centerfreq", "$center_hz", "--output", "decoded:json:file:path=-", "$center_hz"],
    "input": {"stream": "iq", "format": "cs16", "rate_hz": 105000, "via": "stdin"},
    "output": {"via": "stdout", "port": null, "parser": "json_lines",
      "fields": {
        "addr": "/vdl2/avlc/src/addr", "addr_type": "/vdl2/avlc/src/type", "status": "/vdl2/avlc/src/status",
        "dst": "/vdl2/avlc/dst/addr", "frame_type": "/vdl2/avlc/frame_type",
        "reg": "/vdl2/avlc/acars/reg", "flight": "/vdl2/avlc/acars/flight", "label": "/vdl2/avlc/acars/label",
        "msg_num": "/vdl2/avlc/acars/msg_num", "ack": "/vdl2/avlc/acars/ack", "text": "/vdl2/avlc/acars/msg_text",
        "crc_ok": "/vdl2/avlc/acars/crc_ok", "sig_level": "/vdl2/burst/sig_level", "noise_level": "/vdl2/burst/noise_level"
      }},
    "install": {"debian": null, "url": "https://github.com/szpajder/dumpvdl2"}
  },
  "event": {"schema": {"type": "object", "properties": {"addr": {"type": "string"}, "...": "one per mapped field; the ACARS ones nullable"}},
            "summary": "{addr} {reg?} {flight?} {label?} {text?}"},
  "identify": {"proof": "event", "min_events": 1, "dwell_ms": 15000, "min_span_ms": 1000, "quick": null},
  "catalogue": ["vhf-data-link-mode-2-vdl-m2"],
  "test": {"kind": "fixture", "path": "fixtures/vdl2-136975k-1.sigmf-meta", "expect": {"addr": "<the aircraft's ICAO hex>"}, "within_ms": 10000},
  "position": null,
  "time": null,
  "subject": {"key": "addr", "name": "aircraft", "waterfall": "summary",
    "columns": [{"field": "addr_type", "label": "Type", "unit": null}, {"field": "reg", "label": "Reg", "unit": null},
                {"field": "flight", "label": "Flight", "unit": null}, {"field": "label", "label": "Label", "unit": null},
                {"field": "text", "label": "Text", "unit": null}],
    "heading": null, "label": "flight"},
  "notes": "..."
}
```

**Channel and rate.** The row asks for a 25 kHz channel. In the field host's 8 MHz capture the
bank serves it from a 2 MHz bin; `channel_rate_hz(2 000 000, 25 000)` is 50 kHz (the narrowest
exact divisor at or over 48 kHz), and the adaptor resamples 50 → 105 kHz (L/M = 21/10). From a
narrow capture without a bank the same rule gives the same shape. dumpvdl2 runs at
`--oversample 1`, 105 kHz: that is its per-channel rate anyway (ten samples a symbol at 10.5
kbaud; at N it decimates each channel by N to 105 kHz), the stream carries only the 25 kHz
channel, centred, so nothing beyond it needs filtering out, and it is the cheapest in CPU on
both sides (no decimation in dumpvdl2, the smallest resampler in the adaptor). The capture's
centre is the job's centre (`--centerfreq $center_hz` and the one channel `$center_hz`), so
dumpvdl2 never retunes or offsets. `S16_LE` is the spelling 2.6.0 accepts (its help says
`S16LE`; constraint above); `cs16` is the adaptor's `x * 32 767`.

**Events and the subject.** Every AVLC frame dumpvdl2 decodes is an event (XID, supervisory,
information frames, ground-station squitters), keyed on the source address: an aircraft's row
carries its downlinks with `reg`, `flight`, `label`, `text` from the ACARS block when the frame
has one; the ground station (`addr_type` "Ground station") gets a row of its own whose ACARS
columns name the aircraft each uplink is for. `label` is the subject's name field (`flight`),
`waterfall` is `summary` (one line for the job, as ACARS). `t_ns` is the IQ frame's time:
dumpvdl2's `t` is `{sec, usec}`, not ISO, and the frame time is within a block of it.

**Beside ACARS on one radio.** The lease cannot widen a running capture, so the order matters:
`source_pin` the radio at 133 500 000 Hz, 8 000 000 Hz first (130.025–136.975 MHz inside it,
well under the B210's 61.44 MHz), then `plan_start acars_vhf` and `job_start --kind vdl2_decode
--center-hz 136975000 --bandwidth-hz 25000`: every job fits the pin and is leased without a
tune. Started the other way round, VDL2 is a `conflict` naming the seven ACARS jobs, which is
correct and says what to do. The field-craft text says this.

### 4.4 `hfdl_decode`

```json
{
  "id": "hfdl_decode",
  "name": "HFDL aircraft data link (HF)",
  "family": "aviation",
  "band": {"bandwidth_hz": 6000, "centers_hz": [], "min_hz": null, "range_hz": [2800000, 22100000]},
  "modulation": ["psk"],
  "label": "HFDL",
  "protocol": "hfdl",
  "decoder": {
    "kind": "program",
    "programs": ["dumphfdl"],
    "args": ["--iq-file", "-", "--sample-format", "CS16", "--sample-rate", "$rate_hz",
             "--centerfreq", "$center_khz", "--output", "decoded:json:file:path=-", "$center_khz"],
    "input": {"stream": "iq", "format": "cs16", "rate_hz": 12000, "via": "stdin"},
    "output": {"via": "stdout", "port": null, "parser": "json_lines",
      "fields": {
        "icao": ["/hfdl/lpdu/src/ac_info/icao", "/hfdl/lpdu/dst/ac_info/icao"],
        "aircraft_id": ["/hfdl/lpdu/src/id", "/hfdl/lpdu/dst/id"],
        "gs_id": ["/hfdl/lpdu/dst/id", "/hfdl/lpdu/src/id", "/hfdl/spdu/src/id"],
        "gs": ["/hfdl/lpdu/dst/name", "/hfdl/lpdu/src/name", "/hfdl/spdu/src/name"],
        "flight": "/hfdl/lpdu/hfnpdu/flight_id", "pdu": "/hfdl/lpdu/hfnpdu/type/name",
        "lat": "/hfdl/lpdu/hfnpdu/pos/lat", "lon": "/hfdl/lpdu/hfnpdu/pos/lon",
        "reg": "/hfdl/lpdu/hfnpdu/acars/reg", "label": "/hfdl/lpdu/hfnpdu/acars/label", "text": "/hfdl/lpdu/hfnpdu/acars/msg_text",
        "bit_rate": "/hfdl/bit_rate", "sig_level": "/hfdl/sig_level", "freq_skew": "/hfdl/freq_skew"
      }},
    "install": {"debian": null, "url": "https://github.com/szpajder/dumphfdl"}
  },
  "event": {"schema": {"...": "one property per mapped field"}, "summary": "{icao?gs_id} {flight?} {pdu?} {text?}"},
  "identify": {"proof": "event", "min_events": 1, "dwell_ms": 40000, "min_span_ms": 1000, "quick": null},
  "catalogue": ["high-frequency-data-link-hfdl"],
  "test": {"kind": "fixture", "path": "fixtures/hfdl-<freq>k-1.sigmf-meta", "expect": {"gs_id": "<the ground station's number>"}, "within_ms": 40000},
  "position": {"lat": "lat", "lon": "lon", "alt_m": null},
  "time": null,
  "subject": {"key": "icao", "name": "aircraft", "waterfall": "summary",
    "columns": [{"field": "flight", "label": "Flight", "unit": null}, {"field": "gs", "label": "Ground station", "unit": null},
                {"field": "pdu", "label": "PDU", "unit": null}, {"field": "text", "label": "Text", "unit": null}],
    "heading": null, "label": "flight", "track_points": 100, "track_min_m": 1000},
  "notes": "..."
}
```

**The channel sits above the listed frequency.** An HFDL channel is listed by its dial
(suppressed-carrier) frequency; the signal is USB on a 1440 Hz subcarrier, 1800 baud, so its
energy lies about 540–2340 Hz above the listed frequency. dumphfdl expects exactly that: given
channel 8942 kHz in a stream centred on 8942 kHz it looks 1.44 kHz up. The job's `center_hz` is
therefore the listed frequency (what the agent reads off a frequency list and what the
catalogue and the register know), and `band.bandwidth_hz` is 6000, not 2400: a 2.4 kHz band
centred on the dial frequency would hold the lower sideband's silence and cut the signal. 6 kHz
(dial ± 3 kHz) holds the channel with margin and fits a Kiwi's 12 kHz band with the dial
frequency as the band's centre. On a Kiwi the channel rate is the source's 12 000 Hz (under the
48 kHz floor the rule returns the source rate), the adaptor passes it through unresampled, and
dumphfdl gets `--sample-rate 12000`, which it accepts; whether it decodes at 12 kHz is confirmed
on the first fixture (section 6; if not, `input.rate_hz` becomes 24 000 and the adaptor
upsamples). On a wider HF source (an SDR at 2 MHz) the rule gives 48 kHz and the adaptor
resamples down to 12 kHz; nothing in the row changes.

**Events and the subject.** dumphfdl prints a line per PDU: ground-station squitters (`spdu`,
every 32 s per station, no aircraft), and `lpdu`s between an aircraft and a station. The
aircraft is an 8-bit `id` assigned at logon; dumphfdl keeps a cache mapping it to the ICAO
address it learnt from the logon (`ac_info.icao`, `--aircraft-cache-ttl`). The subject key is
that ICAO (src for downlinks, dst for uplinks), so one table row per aircraft whichever way the
frame goes; an aircraft whose logon was missed has no `icao` yet and its frames are events about
no subject until the cache catches it. Squitters are events too (`gs`, `pdu`, no subject): on a
quiet channel they are the proof the channel is alive. A ground station is named by its number
(`gs_id`, always in the line) and by `gs`, its name, when dumphfdl has one: the names come from
the system table, which the row does not carry (a non-goal), so the summary, the check's
`expect` and the field craft lean on the number and show the name when it is there. `position`
is mapped from the
performance-data and frequency-data PDUs' `pos`; a track on the map per aircraft (1 km steps:
these are airliners hundreds of km out). `identify.dwell_ms` is 40 s so a dwell holds at least
one 32 s HFDL frame; `within_ms` for the check the same.

**Frequencies.** `centers_hz` is empty and `range_hz` spans the HF aero bands: the active
frequencies per ground station change with the hour (the system table), so the agent is told to
take them from a list (dumphfdl's `etc/systable.conf` upstream, the operator's own, or the
TODO's three bands near 8.9, 10.0 and 11.4 MHz for the nearest stations) and to start the job on
the dial frequency. A `--system-table` is not needed to decode.

### 4.5 Fixtures and `modes check`

Neither mode has a synthesiser (D8PSK and HFDL's PSK framing are weeks of work each, and a real
burst is a better proof). Each row's test is a `fixture`: a SigMF pair recorded off the air,
committed to the repository.

**Where.** `modes/fixtures/<mode>-<dial kHz>k-<n>.sigmf-{meta,data}`, beside the rows they
prove (the DSP crate's `crates/zeta-dsp/tests/data/` keeps the layer-1 captures its own tests
load by `CARGO_MANIFEST_DIR`; a mode fixture is loaded by `zeta`, from a row, so it lives with
the rows). A row's `fixture.path` is relative, as above. `modes check` resolves a relative path
against `$ZETA_FIXTURES_DIR` when set, else against `modes/` under the current directory (a
checkout root); an absolute path is taken as it is (operator rows). A pair that is not there is
`skipped: fixture <path> not found (ZETA_FIXTURES_DIR unset; looked in <dir>)`, and `--strict`
makes that a fail as it makes every skip a fail (no new rule: `--strict` means "every row proves
itself here"). The deployed binary on the field host is run from its checkout, so
`zeta modes check --strict vdl2_decode` there works without the variable. The embedded-row
check in CI (`crates/zeta/tests/modes_check.rs`) pins both programs to a path none lives at,
so it expects `skipped: dumpvdl2 missing` and `skipped: dumphfdl missing`, not a fixture lookup.

**Size.** Today's fixtures are 35–275 KB each, 1.9 MB in all. Budget: at most 300 KB per
fixture, one fixture per row. `iq read` writes `cf32` at the capture's rate (a minute of an 8
MHz B210 capture is 3.8 GB), so the TODO's "a minute off air with `iq read`" is the raw
recording, not the fixture:

- VDL2: a `capture` job on the 25 kHz box (the page's "Capture bursts", or `job_start --kind
  capture`) writes the gated bursts at the channel rate (50 kHz `cf32`), with pre- and
  post-roll; VDL2 bursts are tens of milliseconds, so three or four of them are under 100 KB.
  The burst gate was built on 30 ms OOK remotes and suits a VDL2 burst; should it clip one,
  the fallback is `iq read` on a narrow pinned capture and a cut by hand.
- HFDL: `iq read` 60 s of the Kiwi's 12 kHz (5.8 MB `cf32`), run `dumphfdl --iq-file` over it
  offline to find the slots that decode, cut 5–10 s around a squitter and an aircraft frame,
  and write the cut as `ci8` (the file source reads `ci8`; 12 kHz × 10 s × 2 bytes = 240 KB).
  A one-off conversion script under `scripts/` with uv inline metadata, not a node feature.

Each meta carries `zeta:expected` in its annotation as the OOK fixtures do (the event fields
the check expects), and the test's `expect` names the same subset.

**Recording is operator time, so the rows do not wait for it.** The B210 is held by the ACARS
plan (a `capture` job on 136.975 MHz is a `conflict` until the radio is pinned at 133.5 MHz /
8 MHz, and stopping or re-pinning jobs is the operator's call, never the builder's), and the
node has no Kiwi source configured. The default is two landings: the schema and the two rows
first, each with its fixture test pointing at a pair that is not yet there (`skipped: fixture
... not found` on every host; `--strict` fails them, as it should until the proof exists), then
the fixtures, recorded when the operator pins the B210 and names a Kiwi (section 7, questions
4 and 5). The fake-program tests (5.2) prove the rows' maps in CI meanwhile.

**Public.** Before a fixture is committed, run its decoder over it and read every line. Keep
link-layer frames (XID, squitters, logons), position reports and OOOI/airline operational
messages; cut any free-text message (crew text, `5Z`-style company messages, anything with a
name in it). Strip the meta of anything but the SigMF core fields, `zeta:expected` and a
`zeta:source` role (`b210`, `kiwi`): no host names, no station positions beyond the demo
station's. The row's `expect` names an aircraft's ICAO hex or a ground station's name, both
public. `scripts/public-check.sh` runs over the meta as over any tracked text file.

### 4.6 Install hints and the catalogue

`install` is `{"debian": null, "url": "https://github.com/szpajder/dumpvdl2"}` and the same
for dumphfdl: neither is in Debian or Ubuntu, so the startup handler line reads `missing
(<url>)`. The rows' `notes` add the one sentence that helps: the OpenWebRX+ apt repository
packages `dumpvdl2`, `dumphfdl` and `libacars2`, and both build from source in a few minutes
(libacars first). `ZETA_HANDLER_DUMPVDL2` and `ZETA_HANDLER_DUMPHFDL` name a program outside
`PATH`, by the existing rule.

The catalogue needs no edit: a signature's `decoder` is the ready-or-unavailable row whose
`catalogue` lists the entry (`catalogue.rs`, `CatalogueKnowledge::decoder`). The rows list
`vhf-data-link-mode-2-vdl-m2` and `high-frequency-data-link-hfdl`. One gap stays: the imported
HFDL entry lists only 2.885–2.914 and 21.89–22.11 MHz as frequency ranges, so `knowledge_lookup`
at 8.9, 10.0 or 11.4 MHz does not surface it. The default is to leave the import as upstream
has it and rely on the field craft's frequency list; section 7 asks whether to patch it instead.

### 4.7 Docs and TODO

- Field craft, "Decoding pagers, aircraft and FM data": a bullet each. VDL2: 136 975 000 Hz
  worldwide (the European channels listed), 25 000 Hz; needs dumpvdl2; beside ACARS pin the
  radio at 133 500 000 Hz / 8 000 000 Hz first, then the ACARS plan and the VDL2 job, in that
  order; report `summary`, and that a ground-station row's ACARS columns name the aircraft of
  each uplink. HFDL: on a Kiwi, `center_hz` is the listed (dial) frequency, 6 000 Hz; the
  signal is 1.44 kHz above it, so a `measure` box on the carrier reads 2.4 kHz of PSK offset
  up; a squitter every 32 s proves the channel; frequencies from a list, not a raster.
- The "Set up a background decoder for all ACARS and ADS-B activity" recipe gains the VDL2
  line; the sibling spec's named set replaces the recipe later.
- `agents_md.rs`, `args` doc text: `$center_khz`; `modes_list` text: `fields`.
- TODO: the two source notes are replaced by one line pointing at this spec and the plan, with
  the airband set and the merged aircraft table left as their own gists; "Modes: fixtures off
  the air" is a new short gist only if the HFDL rate question (section 6) leaves work behind.
- UAT: VDL2 on the B210 beside the ACARS plan, HFDL on the operator's Kiwi (section 5.4).

## 5. Testing

### 5.1 Row schema (`crates/zeta-core`, `row.rs` and `summary.rs` unit tests)

- A `fields` map with a pointer, a list of pointers (first non-null wins, null skipped), a
  pointer to nothing (field absent), a pointer to an object (copied).
- `broken` cases: a key not in the schema (`decoder.output.fields.<key>`); a schema property not
  in the map (`event.schema.properties.<name>`); a pointer not starting with `/`; an empty list.
- `summary`, `subject.key` and `position` resolved on a mapped event: `{reg?}` dropped when the
  pointer resolved to nothing; a null `icao` is "no subject".
- `$center_khz`: 8 942 000 → `8942`; 8 942 500 → `8942.5`; 136 975 000 → `136975`; an unknown
  placeholder is still `broken` at `decoder.args[i]`.
- `tests/modes.rs`: both embedded rows parse and validate against zeta-dsp's stages; their
  `label`s and `protocol`s are pinned (`VDL2`/`vdl2`, `HFDL`/`hfdl`).

### 5.2 Adaptor (`crates/zeta-ext`)

- `Publisher::publish` with a map over a captured real dumpvdl2 line (kept as a test vector in
  `crates/zeta-ext/tests/data/`, reviewed as a fixture is) yields `addr`, `reg`, `text` and no
  `vdl2` key; the same line without a map yields today's whole object.
- `substitute` fills `$center_khz`.
- A fake program (`crates/zeta/tests/data/fake-dumpvdl2.sh`: reads stdin, prints the captured
  line once, exits 0) pinned by `ZETA_HANDLER_DUMPVDL2` runs an operator-dir copy of
  `vdl2_decode` end to end in `tests/modes_check.rs` over any synthesised capture (the `psk`
  family): the first event's `addr` is the vector's, its `summary` reads as the template says,
  and `subject` folding on the page is not involved. The same for dumphfdl, with the `icao`
  list form exercised (a downlink line and an uplink line).

### 5.3 `modes check` (`crates/zeta/tests/modes_check.rs`)

- The embedded set with both programs pinned missing prints `vdl2_decode: skipped: dumpvdl2
  missing` and `hfdl_decode: skipped: dumphfdl missing`; `--strict` exits 1 naming them.
- A row whose fixture path does not resolve prints `skipped: fixture <path> not found (...)`,
  and `--strict` fails it; with `ZETA_FIXTURES_DIR` pointing at a directory holding the pair it
  runs. An absolute path is used as given (the existing `a_fixture_test_row_is_checked`).

### 5.4 Live (UAT, the field host)

- `zeta modes check --strict vdl2_decode` and `zeta modes check --strict hfdl_decode` from the
  checkout each print `ok` with the real programs (`modes check` takes one id; the builder may
  let it take several, but nothing here needs it).
- VDL2: pin the B210 at 133.5 MHz / 8 MHz, `plan_start acars_vhf`, start `vdl2_decode` on
  136.975 MHz; within a minute the Subjects tab shows aircraft rows with `Reg`/`Flight`/`Text`,
  the ground station's row with uplinks, `knowledge_lookup` on the band names `vdl2_decode`,
  and `job_context` shows no `external.unparsed` growth.
- HFDL: on the operator's Kiwi, `hfdl_decode` on one of the nearest station's listed
  frequencies; a squitter event within 40 s, an aircraft row with a position on the map when
  one logs on.

## 6. Left to the plan

- The exact JSON paths. The pointers above follow the key names found in the binaries and the
  decoders' documented output; the first task of the plan captures one real line of each (the
  field host has both programs and the traffic) and fixes the pointers and the schema types
  against it before the rows are written. Same for `lat`/`lon` being degrees as numbers.
- Whether dumphfdl decodes at 12 kHz input. It accepts the rate; if the fixture does not decode
  at 12 kHz but does at 24 kHz offline, `input.rate_hz` becomes 24 000 and the adaptor
  upsamples (L/M = 2/1). The rate is the row's, so nothing else moves.
- The HFDL fixture's frequency and the field-craft frequency list: taken from dumphfdl's system
  table for the two stations nearest the demo station, at the hour the fixture is recorded.
- `modes check` waits for the first event with one `stream_tail`, capped at `TAIL_MAX_MS`
  (10 s), so an HFDL `within_ms` of 40 000 would be cut silently. Either the check tails in a
  loop until `within_ms` has passed, or the HFDL fixture is cut so a squitter lands in its first
  seconds and `within_ms` drops to 10 000; the plan picks one and says which.
- Whether `fields` lives on `ProgramOutput` as `Option<BTreeMap<String, FieldPath>>` with
  `FieldPath` an untagged `String | Vec<String>`, or a small struct; the row tests are the
  contract.
- Recording the fixtures is operator time on the field host's radios (the B210 is on the ACARS
  plan; the Kiwi is the operator's choice): the plan's first tasks are the lines and the
  fixtures, the rows and tests follow.
- A lone `vdl2_decode` on a direct-conversion radio centres the capture on the channel and puts
  the radio's DC spur mid-channel. The plan checks once whether dumpvdl2 copes on the B210 (it
  has no DC-offset option); if not, the field craft says to pin the radio off the channel, as
  the ACARS case does anyway, and the general fix (a capture offset for lone jobs) is a gist.
- Order of work: the schema (`fields`, `$center_khz`) with its unit tests; the fake-program
  tests; the two rows and their fixtures; docs and AGENTS.md regeneration; UAT.

## 7. Questions for the operator

Only what changes what gets built:

1. **Whose row is an uplink?** This design keys VDL2 events on the AVLC source address, so the
   ground station gets a table row whose ACARS columns name the aircraft each uplink is for,
   and aircraft rows carry downlinks only. The alternative keys on the aircraft whichever
   direction the frame goes (needs a conditional in the field map, "src when src is an
   aircraft, else dst", which pointers cannot say), or keys on the ACARS `reg` and loses
   XID/link frames from the table. Which table do you want to read?
2. **One aircraft table across ACARS and VDL2?** The TODO's "one table with ACARS keyed on
   `reg` would be nicer still" is page work (two rows' subjects merged by a shared field),
   not a row. Do you want it in this plan's scope, or as its own gist after the rows land?
3. **The imported catalogue's HFDL entry** lists frequency ranges that miss 8.9, 10.0 and 11.4
   MHz, so `knowledge_lookup` there will not name HFDL even with the row in place. Patch the
   vendored iNTERCEPT `signals.json` locally (and note it in `docs/licensing.md`), or leave the
   import as upstream has it and rely on the field craft's frequency list? The default is to
   leave it.
4. **Which Kiwi for HFDL?** The node has no KiwiSDR source configured, and the HFDL fixture
   and the live check both need one. Name a Kiwi the node may hold a slot on (yours, or a
   public one you choose); without one, `hfdl_decode` lands with its check `skipped: fixture
   ... not found` and no live acceptance.
5. **May the B210 be re-pinned for the VDL2 fixture?** Recording it needs the B210 pinned at
   133 500 000 Hz / 8 000 000 Hz (the ACARS plan keeps running inside the pin) or the ACARS
   jobs stopped for a few minutes. Stopping or re-pinning is your call: say when, or do it
   yourself and say so. Until then the rows land with their fixture checks skipped (4.5).

## 8. Calls made in this document

- Pointers, not flattening (4.1); the list form for "first of"; with a map the event is the
  mapped fields only, and an unmapped schema property is `broken`.
- No conditional in the field map. "src when it is an aircraft, else dst" is a real need
  (question 1) but a small expression language is a worse answer than one more row revision;
  the list form covers HFDL because an uplink's `src.ac_info` is absent.
- `$center_khz` as a placeholder rather than a unit suffix on `$center_hz`, to match the
  existing fixed-list style and its validation.
- `--oversample 1` (105 kHz) for VDL2: the cheapest exact per-channel rate, since the node
  delivers only the one channel. Not 10 (1.05 MHz, dumpvdl2's SDR default, which would make the
  adaptor upsample a 25 kHz channel twentyfold for dumpvdl2 to decimate it back).
- `S16_LE`, the spelling 2.6.0 accepts, over `U8` (8-bit would lose dynamic range on a strong
  ground station beside a weak aircraft).
- VDL2 `bandwidth_hz` 25 000 (the channel raster; the D8PSK occupies about 17 kHz), the five
  common channels as `centers_hz`, `range_hz` null (the catalogue's 117.975–137 MHz is the whole
  voice airband and would score every airband signal as plausible VDL2).
- VDL2 events: every decoded AVLC frame, not only those with an ACARS block; a frame's `t_ns`
  is the IQ frame's time, not dumpvdl2's `{sec, usec}` (a `time` from a split seconds field is
  not worth a schema feature for a few milliseconds).
- HFDL `bandwidth_hz` 6000 centred on the dial frequency, for the reason in 4.4; `position`
  mapped from `hfnpdu.pos` (a structured field; the TODO's "no position" was about ACARS texts),
  `track_min_m` 1000; `identify.dwell_ms` 40 000 to hold one HFDL frame; squitters are events
  about no subject.
- HFDL `centers_hz` empty with `range_hz` [2.8, 22.1] MHz, the frequency list in prose, since
  the active set follows the system table by hour.
- HFDL ground stations by number (`gs_id`), the name (`gs`) shown when dumphfdl has one, rather
  than carrying a system table for the names (from the builder check).
- Two landings by default, rows before fixtures, with the fixture checks skipped until the
  operator's radio time (4.5; from the builder check).
- The vendored catalogue is not patched unless the operator says so (question 3).
- Fixtures under `modes/fixtures/`, relative to `ZETA_FIXTURES_DIR` or `modes/` under the
  current directory, at most 300 KB each, `ci8` allowed, reviewed for content before commit;
  a missing fixture is a skip with the lookup named, and `--strict` fails it like every skip.
- `install.debian` null with the OpenWebRX+ repository named in `notes`, rather than
  `debian: "dumpvdl2"` (which would print an `apt install` line that fails on a stock Debian).
- The catalogue entries are not edited for `decoder` (derived from the rows); the HFDL
  frequency-range gap is the operator's call (question 3).
- The airband named set and the merged aircraft table are out of scope and named as such.
