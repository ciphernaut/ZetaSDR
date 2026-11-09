# Plan 19: layer 4, exploring an unknown signal (three plans)

Design spec, 2026-11-02. The operator answered the design questions in conversation; a reviewer read
this as the builder. Answers the operator gave are marked as the operator's; the rest are the author's
calls, listed in section 11. Builds on plan 16 (mode rows, the `bits` payload ruling), plan 17 (the
`watch` job, the synthesiser families, `source_free`), plan 21 (the agent's `allow` gate) and plan 23
(tabs beside the radios). The entries it answers in `TODO.md`: "Feature: layer 4, working an unknown
signal", "Feature: generic layer-4 framing decoders", "Feature: a burst-aware layer 1", "Feature: a
`knowledge` provider over the ACMA licence register", "Feature: a station position (`set position`)",
"Feature: the watch re-centres before it identifies", "Bug: the watch's detect defaults fire on receiver
noise", "UI: \"What is this?\" says the data is no longer in the buffer", "Feature: classify on a
12.5-150 kHz box tries a row-width slice at the measured centre", and "Agent guide: SIGINT and UX
additions" items 1-3. The plan series row for plan 19 is replaced by three rows, 19.1 to 19.3.

## 1. Intent and why

On 2026-10-29 a 16 kHz bursty FSK link at 404.025 MHz cost two hours of hand analysis that flip-flopped
between PSK and FSK. ZetaSDR's own layer 1 called it `noise` with a 47 Hz "bandwidth": `measure` averages
a 1 % duty burst over a one-second window into nothing. One query against the local ACMA mirror named it
in seconds: a licensed point-to-point telemetry link between two nearby sites, 1 W, 25 kHz channel. The
watch job identified it `unknown` three times, correctly, and had nothing more to say; the IQ ring had
moved on before anyone could look at the bursts themselves.

The operator's picture of what the node should hand over, in their words (2026-11-02): when it cannot
identify the signal type directly, or via an ACMA frequency lookup for in-band matches and a test of the
available decoders, then the base modulation characteristics and the ability to sample it for further
analytics; a buffer that keeps a few of these burst samples, cached on disk and expired by policy; the
scrolling display indicating the cached region while the waterfall is still fresh; a job that reflects
capturing and acquiring these samples, so it is not always active ("explore strange new signals" mode),
which is the watch plus a policy; the three consumers of a captured burst all first class (an export for
their own tools, a bit stream the agent reads, stages on the page); and a signature the node proposes
and the operator accepts, with provenance and confidence, superseded when a real decoder matches and
decodes on a later encounter.

This is the design's layer 4 (spec 6.4): vetted DSP blocks with typed parameters that an agent or the
operator chains to go from a channel to bits to frames, writing a signature back so the next encounter
is a layer 2 match, and leaving behind the labelled captures layer 3 would train on. It is three plans,
each ending with working, testable software; the operator accepts once, at the end of the third
(operator).

## 2. Goals, non-goals, anti-goals

### Goals

- Honest layer-1 features for bursty signals: duty, burst length and period measured on the bursts, the
  occupied band, symbol rate, FSK tone spacing and order, PSK order measured over on-time only.
- An identification ladder in one fixed order (operator): a direct answer (layer-1 label, catalogue, a
  decoder proof); the licence register's in-band assignments near the station and the decoders their
  bandwidth suggests; then the measured features, reported as `unknown` with them.
- A `knowledge` provider over the ACMA licence mirror, answering `allocations` with licensee, site,
  distance, power and channel width, consulted by `identify`, `classify` and the watch.
- A node-level station position, set once, used for distance and bearing.
- The watch's explore policy: on an unknown signal, capture a few of its bursts to disk, channelised,
  with their feature record, under a size and age policy; the same capture started by hand on a box.
- Every captured burst usable three ways (operator): as a SigMF pair the operator takes to their own
  tools; as a `bits` event stream the agent reads; as the input of stages on the page.
- The stages: `resample`, `fsk_demod`, `psk_demod`, `ook_demod`, `clock_recover`, `slicer`, `correlate`,
  `bit_view`, `frame_decode`, each a stage kind like any other, chained by a graph, with one operation
  that builds the usual chain from a capture's features.
- Signatures the node proposes and the operator accepts (operator), with provenance and confidence,
  scored by layer 2 on later encounters, and superseded by a mode row that proves the band.
- Every exploration leaves a labelled record: the captures, the features, the steps and the outcome.
- The page shows the cached region on the waterfall while its columns are on screen, and keeps the
  captures, the bits and the proposals behind their own tabs beside the radios.

### Non-goals

- Breaking encryption, or decoding any payload that is not a clear, published framing. An encrypted or
  proprietary payload is reported `framed, opaque` with its length and CRC status. No transmit path.
- Working a live channel to bits in 19.2: the stages run over captured bursts first; a live chain is
  composed by hand with `graph_put` on a job and is not a deliverable of these plans.
- A GPS fix from the B210. The station is set by hand; `station set --from-source ID` is a later gist.
- A second register transport. The provider speaks the fork's MCP endpoint only; the older Python
  `acma-mcp` build (plain `POST /tools`, no emission column) is not supported.
- A per-device spur list for the scan (the scan gist stays), the HackRF's 1 Msps timeouts, the detector's
  tone-pair criterion for the sondes beyond what section 5.6 changes.
- Chirp (LoRa) rate detection, multi-carrier (OFDM) features, a trained layer-1 classifier, layer 3.
- A node-graph editor on the page; the bits stages are composed by the node from a capture's features,
  or by hand with `graph_start`.
- Decay of a learned signature's confidence over time (`matches` and `last_matched_ns` are kept; a
  formula is a later gist).

### Anti-goals

- A guessed protocol. A `no_frame` or an unproved hypothesis never labels a signal; the `unknown` report
  with measured features stays the honest end of the ladder.
- A signature adopted without the operator (operator). The node proposes; nothing competes in layer 2
  until accepted.
- A privileged register. The ACMA provider is one `KnowledgeProvider` behind a flag; the node runs, and
  every test passes, without it.
- A fourth copy of the watch. The explore mode is a policy on the `detect` node, not a new job kind.
- Captures that outlive the policy. Disk use is bounded by the policy, not by the operator's attention.

## 3. Constraints

- Every op field is required and unknown fields are rejected; new params on existing stages are
  optional with a default so stored graphs keep loading. New job kinds and ops are added to the schema
  and `AGENTS.md` is regenerated.
- A `bits` event is at most 64 KiB (plan 16 ruling). The agent's tool results are not truncated, so the
  bits the agent reads must come from `job_context` summaries and `stream_tail` with a small
  `max_frames`, not from whole streams.
- The IQ ring keeps 10 s at narrow rates and as little as 1 s at wide ones; a burst capture must be cut
  from the live stream as it passes, never read back from the ring.
- `measure` windows are one second; a burst-gated measurement must still emit one record per second
  (`partial` semantics unchanged).
- The ACMA register is the operator's acma-rrl-server (MCP over streamable HTTP) on another host of
  the LAN. The node never opens a mirror file: it calls the service, and a lookup must answer in well
  under a second or be dropped; the node and every test run without it.
- The watch's identify runs and capture jobs are owned jobs: `owner` set, no history, stopped with the
  watch.
- The page's tabs: a new pane goes behind its own tab beside the radios, never a slice of the waterfall
  (operator, 2026-11-02).
- The agent's `allow` gate: an operation that changes the operator's state (accepting a signature,
  deleting a capture) is refused through `ask` unless the run allows it.
- No arbitrary flowgraph code: the stages are fixed kinds with typed params (spec 6.4).

## 4. The ladder

Everything below serves one order of answers. When a signal appears in a watched band, or the operator
boxes one:

1. **Direct.** Layer 1 measures the band (section 5.1) and labels it. Layer 2 scores the catalogue, the
   seed and learned signatures, and every mode row; `classify` tries the quick rows, `identify` runs each
   fitting row for its dwell. A proof ends the ladder: the decoder is left running.
2. **The register and the decoders it suggests.** `knowledge_lookup` answers the band's `allocations`
   from every provider; with the ACMA provider configured and a station set, that is each licensed
   assignment overlapping the band within the band's reach, nearest first. The allocation's channel width
   feeds the row score (section 5.4), so a decoder that fits the licensed channel is tried before one
   that does not. The allocations ride along in the result whatever happens next.
3. **Features, and the evidence kept.** With no proof, the answer is `unknown` with the layer-1 features
   and the allocations: "unknown at 404.025 MHz: burst fsk, 16 kHz, 4800 Bd, 1 % duty every 1.0 s;
   licensed: <licensee>, <site>, 1 W, 25 kHz". With the explore
   policy on, the watch captures a few bursts to disk and says so.
4. **Bits.** From a capture, `bits_start` builds the chain the features suggest and the bit stream and
   its hypotheses appear on the Bits tab and in `job_context`.
5. **Frames and the signature.** A framing hypothesis that checks is run as `frame_decode`; a session
   that reaches a defensible result proposes a signature; the operator accepts it, and the next
   encounter is a layer-2 match, or a CRC-proved decode when the framing was found.

Plan 19.1 delivers steps 1-3, plan 19.2 step 4, plan 19.3 step 5.

## 5. Plan 19.1: explore (features, the register, captures)

### 5.1 A burst-aware layer 1

`measure` (and `measure_selection`, which `classify`, `identify` and the watch's `unknown` report use)
gates its measurement on the signal's bursts:

- A 1 ms power envelope over the window; a sample run is *on* when its envelope stands 6 dB over the
  window's floor (the 15th-percentile 1 ms power), runs closer than 2 ms are merged, and runs under
  1 ms are dropped.
- New `FeatureRecord` fields, each `null` when not measured: `duty` (on-time over window time, 0-1),
  `bursts` (count), `burst_ms` (median on-run length), `period_ms` (median on-edge to on-edge; `null`
  under two bursts), `order` (2, 4 or 8 for a keyed signal), `tone_spacing_hz` (FSK).
- When `duty` is under 0.5 the occupied band, SNR, symbol rate, tones and order are measured over the
  on-time samples only (concatenated, each run windowed); otherwise over the window as today. `burst`
  stays and is `true` when `duty` is under 0.5 or the existing ratio test fires.
- Symbol rate: the cyclostationary line (the spectrum of `|x|²` over the on-time, the strongest line
  at least `LINE_DB` over its local median, searched from 50 Hz to the occupied bandwidth) replaces the
  present keying-spectrum line as the first method; the present method stays as the second. The record
  says which in the label's evidence.
- FSK order and tone spacing: a histogram of the instantaneous frequency over on-time, smoothed; 2 or 4
  clusters within the occupied band of comparable weight give `order` and `tone_spacing_hz` (adjacent
  cluster distance). The existing two-peak rule stays as the fallback.
- PSK order: the m-th power test (a line at `m × symbol_rate` in the spectrum of `x^m` for m 2, 4, 8),
  taken when the envelope is steady over on-time; the smallest m with a line gives `order`.
- New layer-1 labels `psk` (confidence 0.6) and `ook` (steady on-time envelope with two levels and a
  symbol-rate line) beside the existing set; `fsk` keeps its name and now carries `order`.

Every rule is testable with the plan-17 synthesiser families: `fsk` order 2 and 4, `psk` order 2 and 4,
`ook`, each with `repeat` for bursts (and a new `gap_ms` on `repeat`, default 50, so a 1 % duty is
synthesisable) and the `snr_db`, `offset_hz` and `ppm` impairments.

### 5.2 The station

A node-level position, persisted in the state directory as `station.json`, restored before anything
else: `station_set {lat, lon, alt_m, region}` (CLI `zeta station set --lat --lon --alt-m [--region AU]`,
`region` defaulting to `AU`), `station_get`, `station_clear`. `source_list` and `schema` are unchanged;
`station_get` is the one place to read it. It is used for the ACMA provider's distance and bearing, as
the default for `sink_sondehub`'s `lat`, `lon` and `alt_m` (those params become optional: an omitted one
takes the station's, and with no station the sink is refused naming `sink.params.lat`), and the page's
home: `view set home` keeps its per-browser value, and with none set the page takes the node's station.
`region` is also the default `region` of `knowledge_lookup` through the page and the agent guide.

The station also declares its **antennas** (operator, 2026-11-02: the register's reach "will be
context dependent on the RX antenna setup"): `station_set` takes `antennas`, a list of `{id, kind,
min_hz, max_hz, height_m, azimuth_deg, elevation_deg}` (`kind` one of `discone`, `dipole`, `skyloop`,
`vertical`, `yagi`, `dish`, `other`; `azimuth_deg` and `elevation_deg` `null` for an omnidirectional
one; CLI `--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]`, repeatable), and
`null` for the defaults: a `discone` from 30 MHz up and a `dipole` below 30 MHz (operator: "assume
discone by default for VHF+, and tuned dipole or ground/skyloop for HF"), both at `alt_m` + 5 m. A
lookup picks the antenna whose band holds the centre, else the default for that range.

### 5.3 The ACMA provider

`--knowledge-acma URL[,reach_km=N]` (env `ZETA_KNOWLEDGE_ACMA`) attaches the operator's ACMA
register service at `URL` as the provider `acma`; `reach_km` is an override of the reach rule below
(default: derived per lookup). The node does not read
the mirror itself (the operator, 2026-11-02: "just fire up the mcp service"): the register's schema and
sync stay in the service, which the operator maintains. The service is the **acma-rrl-server fork**
(version 1.11.0, checked 2026-11-02 at `http://acma-host:3110`, a different host from
the node's): MCP over streamable HTTP at `URL/mcp` (`initialize`, `notifications/initialized`, then
`tools/call`, the session in the `mcp-session-id` header, answers as `event: message` / `data:` lines),
`GET URL/health` answering `{"status", "version", "isSyncing"}`. Its tools the provider uses:

- `execute_sql {sql, limit}` over the RRL's own tables: `device_details` (`FREQUENCY` and `BANDWIDTH`
  in Hz, `EMISSION` the designator, `DEVICE_TYPE` `T` or `R`, `TRANSMITTER_POWER` with
  `TRANSMITTER_POWER_UNIT`, `SITE_ID`, `LICENCE_NO`), `site` (`NAME`, `LATITUDE`, `LONGITUDE`),
  `licence` (`CLIENT_NO`) and `client` (`LICENCEE`). One fixed query per lookup: the devices whose
  `FREQUENCY ± BANDWIDTH / 2` overlaps the band, joined to site, licence and client, with a bounding
  box around the station for the reach; it answered the four 404.025 MHz assignments (a rail operator
  and the water utility, each a `T` and an `R` site, `16K0F2D` and `16K0F1D`) in under 100 ms.
- `get_frequency_allocation {freq_hz, include_footnotes: false}`: the Australian Spectrum Plan's
  services at the centre (403-406 MHz: FIXED and MOBILE primary, METEOROLOGICAL AIDS and RADIOLOCATION
  secondary), which become `Allocation` rows of their own.

The emission designator is decoded in the node (a small parser: the bandwidth prefix, then the
modulation, signal-nature and information-type letters; `16K0F1D` is 16 kHz, frequency modulation,
single digital channel, data), so a lookup is two calls, not one per row; the service's
`decode_emission_designator` is the reference the parser's tests check against. No change in the
fork is needed. At startup the node probes `GET /health` and logs `zeta: knowledge acma: <url>
unreachable` without refusing the flag; a call that fails or exceeds 5 s answers no allocations and
logs once per minute, so `identify` goes on without the register; the MCP session is opened once and
reopened on a `Server not initialized` error. `allocations(region, center_hz, bandwidth_hz)` answers
only for `region` `AU` (any other region: empty), the plan's services first, then the assignments:

- every assignment overlapping the band whose site lies within the lookup's **reach** of the station,
  nearest first, at most 50. Without a station the reach is not applied and the list is ordered by
  the assignment's distance from the band's centre in frequency, `distance_km` and `bearing_deg`
  `null`.

**Reach.** How far the register is searched is a propagation question, not a constant (operator:
"consider skips on HF bands, and refractions at higher frequencies; the SDR may also be pointed at a
satellite further away, maybe even abroad"). The provider derives it from the band and the antenna
that serves it (5.2), and states it in the result (`reach: {km, class, antenna, note}`) so the summary
reads "within 300 km (VHF, discone)":

| class | band | reach from an omnidirectional antenna | note |
|---|---|---|---|
| `ground` | under 2 MHz | 300 km | ground wave and night skywave: the far rows are kept, ordered by distance |
| `skywave` | 2-30 MHz | no cut: the whole register, nearest first | NVIS by day under 10 MHz reaches a few hundred km, a skip at night thousands; the transmitter may be abroad and outside the register, and the result's `note` says so |
| `tropo` | 30-300 MHz | 300 km | line of sight is 4.1 √h km each end; tropospheric refraction and ducting carry VHF much further, hence the margin |
| `uhf` | 300 MHz-1 GHz | 150 km | ducting still happens; a nearby UHF telemetry link is well inside |
| `microwave` | over 1 GHz | 60 km | |

A directional antenna (`yagi`, `dish`, or any with an `azimuth_deg`) doubles the reach inside a
sector of ±30° about its azimuth and halves it outside. An antenna with `elevation_deg` over 20°, or
a band whose plan services are space ones (`SPACE`, `-SATELLITE`, `EARTH EXPLORATION`), sets class
`space`: the assignments are searched at the `uhf` reach for earth stations only, and the note says
the transmitter may be a spacecraft the register does not list. `reach_km` on the flag replaces the
table's kilometres for every class but `skywave` and `space`, which it caps. The rows the reach
cuts are gone from the result, not demoted: a wider look is the operator's `reach_km` or a wider
antenna declaration. The table's numbers are the author's starting values; each is a named constant
and the field craft says how to read `reach`.
- each as an `Allocation`: the existing `provider`, `region`, `min_hz`, `max_hz` (the channel), `service`
  (a plan row: the service name with `primary` or `secondary`; an assignment: `assignment`), `note`
  (one line: licensee, site name, distance, power, the designator decoded), plus new optional fields
  (`null` when the service lacks them): `kind` (`plan` or `assignment`), `licensee`, `site`, `lat`,
  `lon`, `distance_km`, `bearing_deg` (computed from the station), `power_w` (converted from the unit
  column), `center_hz`, `bandwidth_hz`, `licence_no`, `role` (`tx` or `rx` from `DEVICE_TYPE`),
  `emission` (the designator as stored, trimmed) and `emission_decoded` (`{bandwidth_hz, modulation,
  nature, info}` from the parser, `null` for a designator it cannot read), `as_of` (the service's
  sync time from `health`, or `null`). Transmitters are listed before receivers at equal distance,
  and only `tx` rows feed the score term in 5.4.
- `signatures()` is empty: the register describes licences, not waveforms.

`Allocation` gains those fields as optional with defaults, so the seed provider's file keeps loading.
`knowledge_lookup`'s result is unchanged in shape; `AGENTS.md`'s "Neither has allocations" note is
regenerated. The project's `.mcp.json` registers the fork (`http://acma-host:3110/mcp`,
streamable HTTP) so a Claude session on this repo can query it directly; that is tooling, not the
node. The Python `acma-mcp` copy under `/Projects/acma-local` on the field host (a 45 GB mirror in MHz with
no emission column, 11 tools) is the older build and is not a target.

### 5.4 Identify, classify and the watch consult the register

- `identify` calls every provider's `allocations` for its band (the station's region) in its layer-1
  step and keeps the nearest five under `features.allocations`; each `candidate` event carries
  `allocation` (the best match for that row, or `null`). The row score gains 0.2 when a `tx`
  assignment's decoded designator fits the row: its `bandwidth_hz` within 0.5-1.5 times the row's
  `band.bandwidth_hz`, and its modulation letter compatible with the row's `modulation` (`F` and `G`
  with `fm`/`fsk`, `G` and `D` with `psk`, `A` and `H`/`J`/`R` with `am`/`ssb`, `N` with `carrier`)
  (the "decoders the register suggests" rung). `classify`'s result gains `allocations` the same way,
  and its `tried` scoring the same term. The `unknown` summary pairs the two: "register says 16K0F1D
  (16 kHz FM data); measured burst fsk 16 kHz".
- The watch's `unknown` event and `job_context.watch.signals[].features` carry the record with its new
  fields and `allocations`; its `summary` names the nearest licensee, site and distance when there is
  one.
- The watch re-centres before identifying (the plan-17 gist). The detector's `center_hz` is already
  the run's power-weighted centroid (`detect.rs`), good to one FFT bin; the fix is downstream:
  `identify` already measures its band for 1000 ms, so each trial job it starts is centred on that
  measurement's occupied centre when it lies inside the identify band (else on the band's centre as
  today), and the verdict's `features` say which. The watch keeps starting identify on the detector's
  centre.
- `classify` on a box between `MIN_BW_HZ` and the RDS gate also tries the row-width slice at the
  measured centre after a `no_frame` when layer 1 is not `noise`, and the five `tried` are counted
  after dropping rows whose `range_hz` does not contain the measured centre (the plan-16/17 gist).

### 5.5 The explore policy and burst captures

Two new `detect` params, read by the watch as policy like `decode_on`:

- `on_unknown`: `report` (default; today's behaviour) or `capture`.
- `capture`: `{bursts: 32, window_ms: 2000, pre_ms: 20, post_ms: 20, timeout_ms: 120000}`, checked
  like `sondehub` (a bad field is refused naming `capture.<field>`). Thirty-two because a 1 % duty
  link's burst is 10 ms, about 50 bits at 4800 Bd, and a few hundred bits are the least a sync hunt
  can work with; a window of a 25 kHz channel at 50 ms is about 10 KB, so the set is small.

With `on_unknown: capture`, the first time a signal's identify run ends `unknown` the watch starts a
**capture job** on the signal's measured band (centroid, occupied width × 1.25, at least 12 500 Hz) as
an owned child, like its identify runs: `job_start`'s new kind `capture`, graph `channel →
burst_capture`. The watch publishes `capturing` (`signal`, `job`) and, when the child ends, `captured`
(`signal`, `captures: [ids]`, `summary`); `job_context.watch.signals[].captures` lists the ids. A signal
is captured once per watch run; its retries identify only. The operator starts the same job by hand on
a box: `job start --kind capture --source S --center-hz C --bandwidth-hz B` (the page's box menu:
"Capture bursts"), with the params on the `burst_capture` node through `graph put`.

`burst_capture` (iq → event; params `bursts`, `window_ms`, `pre_ms`, `post_ms`, `timeout_ms`, with the
defaults above, and `whole: false`, which when true writes `bursts` windows of `window_ms` back to back
regardless of the gate, for a continuous signal or when the operator wants the gaps) cuts bursts from
the channelised stream as they pass:

- the same 1 ms envelope gate as 5.1; a window runs from `pre_ms` before an on-edge to `post_ms` after
  the off-edge, at most `window_ms` long (a longer burst, or a continuous signal, is cut into
  `window_ms` windows back to back, `bursts` of them).
- each window is one SigMF pair under `state_dir/bursts/<ulid>.sigmf-{meta,data}`: cf32 at the channel's
  rate, `core:frequency` the channel centre, `core:datetime` the window's start, one annotation over
  the whole window carrying `zeta:features` (the burst's own feature record, measured with 5.1 over the
  window), `zeta:signal` (centre, width, SNR as detected), `zeta:job` (the capture job), `zeta:watch`
  (the owning watch, or `null`), `zeta:source`, `zeta:allocations` (as 5.4 found them).
- a `capture` event per window on the job's event stream: `{protocol: "capture", id, start_ns,
  end_ns, bytes, center_hz, bandwidth_hz, features, summary}`.
- the job ends `stopped` when `bursts` windows are written or `timeout_ms` passes, with
  `job_context.capture = {written: [ids], bytes, bursts_seen, timed_out}`. Captures written by one job
  share a `set` (the job's id) so later consumers take them together (6.2).
- at its end a hand-started capture job runs the quick classifiers over its first window
  (`capture_classify`) and records the result under `job_context.capture.classified`; a watch's child
  inherits the watch's `unknown` verdict instead. 19.3's features-only proposal is made only when
  nothing proved and the label is not `noise`, so a capture boxed on a known APRS channel proposes
  nothing.

The store is `state_dir/bursts/` with `index.json` (one record per capture: id, path, source, band,
times, bytes, features, signal, job, watch, `pinned`). The policy, node flags with defaults:
`--bursts-keep-mb 256`, `--bursts-keep-days 7`; per signal, the capture job's `bursts` caps what one run
writes. Expiry runs on every write and at startup: oldest first until the budget holds, then anything
older than the age; a capture named in an accepted signature's provenance (19.3) is `pinned` and never
expired. Ops: `capture_list {signal: null}` (every record, newest first; `signal` narrows to captures
whose band overlaps a `{center_hz, bandwidth_hz}`), `capture_show {capture}` (the record with the
SigMF meta), `capture_classify {capture}` (the classifiers over the capture's samples; the same result
shape as `classify`), `capture_delete {capture}` (the operator's: through `ask` it needs the run's
`allow.delete_captures`, false by default). A capture's files are served by `zeta serve` at
`GET /captures/<id>.sigmf-meta` and `.sigmf-data` so the page can offer a download; `capture_show`
carries the paths for a local caller.

### 5.6 The detector on a real receiver

The explore policy is worth nothing on a detector that fires on noise (the 2026-10-29 and 2026-11-01
runs). `detect` changes its occupancy rule: a bin is occupied when it stands `threshold_db` over the
row's **median** bin (not the 2nd-percentile `floor_db`), and a run is a signal when its energy (the
sum over its bins of dB over the median) is at least `energy_db` (new). The width floor drops to
4000 Hz (`width_hz` `[4000, 30000]`), since the real RS41 of 2026-11-01 read 4.5 kHz wide; the energy
rule, not the width, is what keeps noise out. The defaults are set by the builder against two inputs:
the synthesised captures (the existing watch tests, plus an RS41 at 6 dB channel SNR, which must be
detected) and a recorded B210 noise capture, which does not exist yet: the first task of 19.1 records
10 s of 400-406 MHz on the field host's B210 with no sonde up (`iq read`, 2 Msps at 403.025 MHz is enough,
about 160 MB), kept outside the repo and read by an `#[ignore]` test that runs when
`ZETA_NOISE_CAPTURE` names it; that capture must yield no signal in 60 s. Tracking is already right for
intermittent signals (`persist_ms` counts from `first_ns`, a track survives `gone_ms` unmatched); new
is that a track needs at least 3 sightings before `new`, so a 5 ms burst every second is `new` after
3 s, and that `signal` events are rate-limited to 10 per second per job. The field-tested values stay
in field craft as the fallback.

### 5.7 The page

- A **cached region**: while a capture's columns are on screen the waterfall draws a bracket over its
  band spanning its time columns (the column ring carries `start_ns`/`end_ns`), tagged with the capture
  id's tail; it scrolls off with the columns. `overlays.js` gains a time-span primitive for it.
- A **pane tab strip**. Today `#tabs` is the sources tablist (one tab per radio) and Subjects is a
  toggle over a `div` in `main`, which the operator has asked to move ("alongside the radios", `TODO.md`
  "UI: the Subjects list must not take over the page"). 19.1 builds the pane strip beside the radio
  tabs: one tab per pane (Subjects, Explore, and from 19.2 one Bits tab per bits job), one pane shown
  at a time under the waterfall at the height the jobs panel has, none shown when none is selected,
  the selection in the view state (`view set pane subjects|explore|bits:<job>|none`). Subjects moves
  into it first, with its `view set subjects` kept as an alias; the plan-23 UI scenarios keep passing.
- An **Explore** pane in that strip (auto-selected the first time a capture appears, never again):
  the captures newest first (time, band, size, the feature summary, the nearest licensee), each with
  What is this (`capture_classify`), Download (the two files), Work the bits (19.2) and Delete; and
  from 19.3 the proposals below them.
- The box menu gains "Capture bursts" (a `capture` job on the box, fitted first as a demod is).
- `classify`'s `too_old` error already carries the ring's `earliest_ns` (`RingError::TooOld`); the page
  half is new: it re-issues the classify on the latest span the ring covers of the same length (or what
  it has), says so in the transcript (`buffer moved on; classified the last <n> s`), and the box footer
  shows the ring's depth.
- The watch's `capturing` and `captured` events are ribbons like its others.

### 5.8 The guide

Field craft gains: on a licensed band call `knowledge_lookup` first and report licensee, site, distance
and power before demodulating, noting `as_of`; when `measure` reports a low duty, report the burst
timing and say the band label is measured over the bursts; the explore mode and how to read its
captures. AGENTS.md's "Working layer 4" names the capture ops; 19.2 rewrites the rest.

## 6. Plan 19.2: bits (stages over a capture)

### 6.1 Streams

No new stream kind. Demodulated decision signals travel as `audio` frames (f32, `channels` 1 for a
real baseband, 2 for I/Q), symbol-rate streams likewise, and bits as `event` frames in the plan-16
shape: `{"protocol": "bits", "rate_baud": N, "bits": "<hex>", "count": N, "t_ns": N, "summary"}`,
most significant bit first, the last byte zero-padded, at most 64 KiB. `t_ns` is the time of the first
bit in the capture's own clock.

### 6.2 The stages

All in `zeta-dsp`, registered like any stage, listed by `schema`:

| kind | in → out | params (defaults) | notes |
|---|---|---|---|
| `capture_in` | → iq, event | `captures` (ids, in order), `loop` (false), `realtime` (true), `loop_pause_ms` (500) | replays the captures back to back at their common rate (a mixed rate or band is refused naming the id); frames carry each capture's original `t_ns`; a `segment` event (`index`, `capture`, `start_ns`) at each seam, which `clock_recover` and `slicer` take as a reset and `bit_view` as a boundary it never correlates across; `realtime` paces at the rate (off, the chain runs as fast as the consumer takes); `loop` replays after `loop_pause_ms` of silence; ends the job `stopped` at the end unless `loop` |
| `resample` | iq → iq | `out_rate_hz` | rational, from `zeta_dsp::resample` run on I and Q |
| `fsk_demod` | iq → audio (1) | `symbol_rate_baud`, `order` (2), `tone_spacing_hz` (null: estimated as 5.1) | quadrature discriminator, low-passed at 0.6 × rate × order; output normalised so the outer tones sit at ±1 |
| `psk_demod` | iq → audio (2) | `symbol_rate_baud`, `order` (2), `rolloff` (0.35), `loop_bw` (0.02) | RRC matched filter, Costas loop of the order; output I/Q at 4 samples per symbol |
| `ook_demod` | iq → audio (1) | `symbol_rate_baud` | envelope, low-passed, normalised 0-1 |
| `clock_recover` | audio → audio, event | `symbol_rate_baud`, `algorithm` (`mm` for 1 channel, `gardner` for 2; `early_late` selectable), `loop_bw` (0.01), `max_ppm` (2000) | one sample per symbol out; a `timing` event each second: `rate_est_baud`, `error_rms`, `locked`; `job_context.clock` holds the latest |
| `slicer` | audio → event | `levels` (2; 4 for 4-FSK), `constellation` (null; `bpsk`, `qpsk`, `8psk` for 2 channels), `mapping` (`natural`; `gray`), `differential` (false), `chunk_bits` (4096) | thresholds from running statistics; a `bits` event per chunk and at input end; a `constellation` event per second with up to 512 symbol points; `job_context.slicer` holds `symbols`, `eye_open` (1-channel) or `evm` (2-channel) |
| `correlate` | event → event | `pattern` (hex), `bits`, `max_errors` (0) | a `sync` event per hit: `offset_bits`, `errors`, `t_ns` |
| `bit_view` | event → event | `window_bits` (65536) | see 6.3 |

Stage params that depend on the signal (`symbol_rate_baud`) are required so a stored graph is explicit;
`bits_start` fills them. `psk_demod` and `fsk_demod` refuse a rate that `resample` could not serve
(under 4 samples per symbol at the input rate: `other`, naming the rate to give `resample`).

### 6.3 `bit_view`

Over a sliding window of `window_bits` it publishes a `hypotheses` event each time the window grows by
a quarter, and keeps the latest under `job_context.bit_view`:

- `runs`: the run-length histogram (lengths 1-32, counts for 0 and 1), and the longest run.
- `period_bits`: autocorrelation peaks of the bit stream, the three strongest with their lag and score
  (frame period candidates).
- `sync_candidates`: the most frequent 16-, 24- and 32-bit words that are not runs or alternations,
  with their count and the mean gap between occurrences, at most five.
- `encoding`: `nrz`, `nrzi`, `manchester` or `differential` with a score for each (the run-length
  signature: Manchester has no run over 2; NRZI shows up as a flat-topped histogram after decoding).
- `crc_candidates`: for each sync candidate and each of the three period candidates, frames cut at
  the sync are checked against the CRC library (6.5 of 19.3 defines it; 19.2 ships the table with
  CRC-16-CCITT, CRC-16-IBM, CRC-16-DNP, CRC-32) at the frame's tail, with and without bit reversal;
  a polynomial that checks on at least 3 frames is listed with its frame count.
- `summary`: one line, e.g. `sync 0x2DD4 ×14 every 1120 bits, nrz, crc16-ccitt 12/14`.

### 6.4 Starting a chain

- `graph_start {graph}`: starts a job from a graph whose first node has no inputs (`capture_in`,
  `stream_in`, `mix`); the record's source is `node`, its band the capture's (so the page can place
  it), no lease. A graph with an unfed `iq` input is refused (`other`, naming the node). Persisted and
  resumed like any job; a `capture_in` job whose capture was expired fails `capture <id> not found`.
- `bits_start {capture, modulation, symbol_rate_baud, order}` (CLI `zeta bits start --capture ID
  [--modulation fsk|psk|ook] [--symbol-rate-baud N] [--order N]`): takes every capture of the named
  capture's `set` (5.5), in time order, and builds `capture_in → resample → <demod> → clock_recover →
  slicer → bit_view` from the named capture's feature record, each `null` taken from the features
  (`modulation` from the label, `symbol_rate_baud` and `order` from the record), refusing with `other`
  naming the field when neither gives it ("no symbol rate measured; give `symbol_rate_baud`"). The
  resample rate is 8 × symbol rate rounded to a rate the capture's rate resamples to within
  `MAX_FACTOR`. The result is the job id, as `job_start` answers; `--then-tail` tails the `bits` events.
  `loop` is `true` and `realtime` `true`, so a set of 32 bursts of 50 ms replays in under 2 s per pass
  with a pause between passes and the page is never flooded; the job runs until stopped.
- A running chain is changed with `graph_get`, edit, `graph_put` as any job (a different sync pattern
  on `correlate`, `differential` on the slicer).

### 6.5 The page

A **Bits** tab per bits job (opened by "Work the bits" on a capture, or from the job's row): the bit
stream as rows of hex with the binary under the cursor, `sync` hits highlighted, the latest hypotheses
(period, sync candidates, encoding, CRC) as a list with a "Try" that puts the candidate on a
`correlate` node through `graph_put`, the eye (1 channel) or constellation (2 channels) drawn from the
`constellation` events, and the clock's `rate_est_baud` and `locked`.

### 6.6 The agent

AGENTS.md's "Working layer 4" becomes an executable playbook: `capture_list`, `bits_start`, read
`job_context.bit_view` (never the whole bits stream: `stream_tail` at most 4 frames), `graph_put` a
`correlate` or a changed slicer, report the hypotheses with their counts, and stop at "unknown" with
the measured features when nothing checks. The agent's own `bits_start` jobs count as jobs it started
(it may stop them).

## 7. Plan 19.3: frames and signatures

### 7.1 `frame_decode`

`frame_decode` (event bits → event): params `encoding` (`nrz`; `nrzi`, `manchester`, `differential`),
`sync` (`{hex, bits}`), `preamble` (null; `{hex, bits}` required ahead of the sync when given),
`whitening` (`none`; `pn9`, or `{poly, seed}`), `length` (`{fixed_bits: N}` or `{field: {offset_bits,
bits, big_endian, unit: bits|bytes, add}}`), `crc` (null; a name from the library, or `{width, poly,
init, refin, refout, xorout}`), `bit_order` (`msb`; `lsb`), `max_errors` (0, sync bit errors tolerated).
It decodes the encoding, hunts the sync, cuts a frame by the length rule, unwhitens, checks the CRC and
emits one event per frame: `{"protocol": "frame", "t_ns", "sync", "length_bytes", "bytes": "<hex>",
"crc_ok": true|false|null, "payload": "opaque", "summary"}`. `job_context.frame_decode` counts
`frames`, `crc_ok`, `crc_bad`, `sync_hits`. The CRC library in `zeta-dsp` (`crc.rs`): CRC-8, CRC-16-CCITT
(X.25, the existing `crc16_x25`), CRC-16-IBM/ARC, CRC-16-Modbus, CRC-16-DNP, CRC-32, by name, plus the
general form; `bit_view` uses the same table.

### 7.2 Frame rows

A mode row may be `decoder_kind: "frame"`: `decoder.demod` (`{modulation: fsk|psk|ook,
symbol_rate_baud, order, tone_spacing_hz}`), `decoder.framing` (the `frame_decode` params) and
`decoder.parser` (`opaque`, or a named parser). The node expands it to `channel → resample → <demod> →
clock_recover → slicer → frame_decode [→ frame_parse]` on the radio, so a frame row is a decoder like
any other: `job_start --kind <id>`, scored by `classify` and run by `identify` (a quick attempt exists:
a CRC-clean frame in the span proves it), checked by `modes check` over a synthesised capture (the
synthesiser gains a `framing` family: bits framed by the same rules, so a row's test needs no live
capture of anyone's link).

Parsers, each a pure bytes-to-fields decoder behind the CRC gate, each its own built-in row with an
event schema, `summary` and `subject` where one fits, in the `iot` family: `modbus_rtu`, `dnp3`,
`iec101`, `iec104`. These are published open standards; a row is the generic protocol, chosen by
signature match or named by the operator, never built against one named licensee's traffic. A payload
that is not one of them is `opaque`. This task group is the last of the plan and is droppable without
touching the rest.

Boundary (verbatim from the gist, so a later session does not drift): this decodes clear, unencrypted,
standards-based framing that the node is lawfully receiving. It never attempts to break encryption,
never targets a specific operator's link as the deliverable, and emits no transmit path. An encrypted or
proprietary payload is reported as `framed, opaque` with its length and CRC status, not decoded.

### 7.3 Signatures: proposed, accepted, superseded

The learned signature record, `state_dir/signatures/<ulid>.json`, one file each:

- `id`, `name` (the node's: `<modulation> <width> kHz at <MHz>`; the operator's on accept), `state`
  (`proposed`, `accepted`, `rejected`, `superseded`).
- the waveform: `min_hz`, `max_hz` (the band), `bandwidth_hz`, `modulation`, `order`,
  `symbol_rate_baud`, `tone_spacing_hz`, `burst` (`{duty, burst_ms, period_ms}`), `framing` (null, or
  the `frame_decode` params that checked), `decoder` (null, or the frame row 7.4 generates).
- `confidence`: 0.3 for features only, 0.6 with a framing whose sync and period hold, 0.9 with a CRC
  that checks on at least 5 frames.
- `provenance`: `source`, `station` (position at the time, or null), `captures` (ids), `jobs` (the
  capture and bits jobs), `allocation` (the nearest licensed assignment, or null), `proposed_ns`,
  `proposed_by` (`node`, or the transport of a hand proposal), `accepted_ns`, `accepted_by`
  (transport), `superseded_by` (a mode id), `superseded_ns`.
- `matches`, `last_matched_ns`: counted by the layer-2 scorer on later encounters.

Proposals are made by: the `capture` job at its end, for an unknown signal, with features only (one per
signal per watch run; a hand-started capture proposes too); a bits or frames job when `bit_view` finds a
CRC that checks on 5 frames, or `frame_decode` reaches 5 CRC-clean frames (once per job); and by hand,
`signature_propose {capture, name, framing}`. Each proposal publishes `{"protocol": "signature",
"state": "proposed", "id", "confidence", "summary"}` on the job's event stream. Ops: `signature_list
{state}` (`null` for all), `signature_show {id}`, `signature_accept {id, name}` (`name` null keeps the
proposed one), `signature_reject {id}`. Through `ask`, `signature_accept` and `signature_reject` need
`allow.accept_signatures` (false by default; "allow signatures on" in the console), else `not_allowed`:
the node proposes, the operator says yes (operator). Accepting pins the provenance captures (5.5).

`LearnedKnowledge` is a provider over the accepted records: `signatures()` answers each as a
`Signature` (which gains optional `order`, `tone_spacing_hz`, `burst`, `sync`, `encoding`, `crc`,
`confidence`, `tier` fields, the seed file unchanged) with `tier: learned`; `knowledge_lookup` lists
them beside the seed and the catalogue. A `LearnedClassifier` scores accepted signatures in layer 2:
0.4 for the centre within the band, 0.3 for a bandwidth within 0.5-1.5×, 0.2 for the modulation and
order, 0.1 for a symbol rate within 5 %, 0.1 for a burst period within 20 % (capped at 1.0), labelling
`learned <name>` at confidence × score with evidence `learned <id>: ...`; a match increments `matches`.
Seed and catalogue signatures are not re-scored by it.

Superseding: when a built-in or operator mode row proves a band (an `identify` verdict, a `classify`
`proved`, a decoder job's first event) that overlaps an accepted learned signature within its
bandwidth, the record becomes `superseded` with `superseded_by` the mode's id and leaves the scorer; its
captures stay pinned. A learned signature is never superseded by another learned one.

### 7.4 From a signature to a decoder

Accepting a signature whose `framing` has a CRC writes a generated frame row (7.2) to the operator's
modes directory as `learned_<id>.json` (`parser: opaque`, `name` the signature's, `family` `general`,
`band` the signature's), sets `decoder` on the record to that id, and reloads the rows; from then on the
signal is proved by a CRC-clean decode like any mode, and `identify` leaves `learned_<id>` running. The
operator edits the file like any row (a parser, a `subject`); a row with that id already present is not
overwritten (the accept says so). Rejecting or deleting the signature does not delete the file.

### 7.5 The labelled record

Each capture, bits and frames job writes `state_dir/sessions/<job id>.json` at its end: the captures,
the feature record, the graph it ran, the latest hypotheses, and the outcome (a proposal id, a mode
proof, or `none`). It is the by-product spec 6.4 asks for; nothing reads it yet. Expired with its
captures.

### 7.6 The page and the agent

The Explore tab lists proposals under the captures (name, confidence, the summary, the evidence) with
Accept, Rename and Reject; an accepted one shows its tier and, when one exists, its row's id. A
`signature` event is a ribbon. The agent guide gains the signature ops and the rule that acceptance is
the operator's.

## 8. Testing

- `zeta-dsp` unit tests over the synthesiser: burst features (`fsk` `repeat` with `gap_ms` 995 and a
  5 ms burst: `duty` 0.005 ± 0.002, `period_ms` 1000 ± 20, the occupied band within 10 % of the
  synthesised deviation × 2 + rate); symbol rate within 2 % for `fsk`, `psk`, `ook` at 10 and 20 dB SNR
  with `offset_hz` 500 and `ppm` 20; `order` right for 2/4-FSK and 2/4-PSK; the demod → clock → slicer
  chain recovering the synthesised `bits` with no errors at 20 dB and under 1 % at 10 dB; `bit_view`
  finding a planted sync word and period and a CRC-16-CCITT over framed bits; `frame_decode` on each
  encoding and length rule; every CRC against a known vector.
- `burst_capture` over a synthesised burst stream: `bursts` files, each window's `pre_ms`/`post_ms`
  and features; the policy expiring oldest first and never a pinned capture.
- The ACMA provider over a stub MCP server in the test (streamable HTTP: `initialize`, the session
  header, `tools/call` for `execute_sql` and `get_frequency_allocation`, answers recorded from the
  real fork's shape with the four 404.025 MHz rows and the 403-406 MHz plan entry): the request's
  band and bounding box, overlap, ordering nearest first, `tx` before `rx`, the `AU`-only rule, the
  no-station rule, the session reopened on `Server not initialized`, a 5 s timeout and a JSON-RPC
  error each giving no allocations and one log line; the designator parser against a table of codes
  with the fork's `decode_emission_designator` answers (`16K0F1D`, `16K0F2D`, `25K0G7W`, `2K80J3E`,
  `6K00A3E`, an invalid one).
- Node tests: `identify` and `classify` carrying `allocations` and the score term; the watch with
  `on_unknown: capture` capturing once per signal and publishing `capturing`/`captured`;
  `capture_classify`; `graph_start` refusing an unfed input; `bits_start` from a capture's features and
  refusing without a rate; a `capture_in` job on an expired capture; proposals, accept with the pin and
  the generated row, reject, supersede by a proved row, the `ask` gate for accept and delete;
  `station_set` persisted and read by the provider and the sink defaults.
- `modes check` on the frame rows over the `framing` synthesiser family.
- UI e2e: the cached-region bracket while the columns are on screen and gone after; the Explore tab's
  list and actions; the Bits tab showing a stream and a sync hit; a proposal accepted from the page.
- The gate for each plan: `cargo test --workspace --all-features --no-fail-fast` green with no
  `ui e2e skipped`, clippy clean, `AGENTS.md` regenerated and committed.

## 9. Acceptance on the field host (the operator's, after 19.3)

1. `station set` with the field host's antenna's position; `--knowledge-acma
   http://acma-host:3110` on the node with the fork up. `knowledge lookup` at
   404.025 MHz names the 403-406 MHz plan services and the link's tx and rx assignments with
   distance, power and `16K0F1D` decoded.
2. A watch on 400-406 MHz with `on_unknown: capture` on the B210: the 404.025 MHz link is reported
   `unknown` with burst features (duty about 1 %, period about 1 s, `fsk`, a symbol rate) and the
   licensee in the summary, then `captured` with captures in `capture list` and on the Explore tab; the
   bracket shows on the waterfall while the burst's columns are on screen.
3. `bits start` on one capture: the Bits tab shows a stream, the clock locks near the measured rate, and
   `bit_view` reports a period and a sync candidate (or says nothing checks, honestly).
4. A proposal appears; accepting it from the page pins its captures; the next watch run labels the
   signal `learned <name>` (or decodes it through `learned_<id>` when a CRC was found).
5. A synthesised check in the same session: `modes check --strict` passing for the frame rows.

## 10. Left to the builder

- The exact loop constants (Costas and Gardner bandwidths, the discriminator's low-pass), the
  instantaneous-frequency histogram's smoothing, `energy_db` for the detector, the slicer's threshold
  statistics; all under tests that pin behaviour, not numbers.
- The SQL for the provider and its prepared statements; the haversine; the fixture builder.
- The SigMF annotation keys under `zeta:` and the `index.json` layout.
- `StageContext` carries no state directory and no station today: `burst_capture`, `capture_in` and
  the station default of `sink_sondehub` need a new context field or launch-time param filling; the
  builder picks.
- `FeatureRecord` is `deny_unknown_fields` and is every `measure` event's shape: the new fields in 5.1
  are optional on it, while `allocations` sits beside the record in the identify verdict, the classify
  result and the watch's `unknown` event, never inside it.
- `crc.rs` absorbs the three CRC-16s that exist (`util::crc16_x25`, `rs41_frame::crc16_ccitt`,
  `acars::crc16`), each kept as a name in the table.
- `overlays.js`'s time-span primitive, and the pane strip's markup beside the radio tablist.
- Which `bit_view` CRC candidates are worth the cost at 64 kbit windows (a budget per hypothesis event
  of 200 ms).
- The order of tasks inside each plan; each plan ends with the gate in section 8 and a status entry in
  the plan series.

## 11. Calls made in this document (check these while reading)

1. Three plans, 19.1 explore, 19.2 bits, 19.3 frames and signatures, in that order (operator: "split as
   you see fit").
2. The explore mode is a `detect` policy, `on_unknown: capture` (operator agreed to "watch plus a
   policy"); a hand capture is `job_start --kind capture`.
3. Captures live under `state_dir/bursts/`, 256 MB and 7 days by default, 8 bursts per signal per run,
   oldest first, pinned by an accepted signature.
4. The ACMA provider calls the operator's acma-rrl-server fork over its MCP streamable-HTTP endpoint
   (operator: "fire up the mcp service", then "check acma-host:3110, it has a newer
   fork"), never a mirror file, through `execute_sql` and `get_frequency_allocation`; the designator
   is decoded in the node; reach per band class and antenna (call 5); `AU` only; no change in the fork; an unreachable service is
   logged, not fatal.
5. The station is a node record, not a source property; `sink_sondehub` takes it as a default. It
   carries the antennas (operator's defaults: discone from 30 MHz, dipole below), and the register's
   reach is derived per lookup from the band class and antenna, with `reach_km` as the override; the
   reach kilometres in the 5.3 table are the author's starting values.
6. Demod outputs travel as `audio` frames; bits as `event` frames; no new stream kind.
7. `graph_start` is the generic "job from a graph with no radio"; `bits_start` is the shorthand that
   builds the chain from a capture's features.
8. Bits work runs over captures only in these plans; live chains are a later gist.
9. Proposals come from the capture job (features only, 0.3), from a bits or frames job on 5 CRC-clean
   frames (0.9), or by hand; `allow.accept_signatures` and `allow.delete_captures` gate the agent.
10. Accepting a framed signature writes a `learned_<id>` frame row to the modes directory, so the next
    encounter is proved by CRC; superseding is by a non-learned row only.
11. The serial-protocol rows (Modbus RTU, DNP3, IEC 101/104) are the last task group of 19.3 and
    droppable.
12. The detector's occupancy rule moves to the median with a run-energy criterion in 19.1, because
    explore depends on it; the width floor drops to 4 kHz; a B210 noise recording is 19.1's first task.
13. 19.1 builds the pane tab strip and moves Subjects into it (the operator's 2026-11-02 ask), so
    Explore and Bits have somewhere to go.
14. `capture_in` replays a whole capture set with seams the chain resets at; `realtime` is on by
    default so a looped replay never floods the page.
15. A hand-started capture classifies its first window at its end and proposes only when nothing
    proved; a watch's child inherits the `unknown` verdict.
16. ACMA rows with no `power` are reported `rx`, listed after transmitters and left out of the score.
17. Identify re-centres its trial jobs on its own 1 s measurement; the detector's centroid stays.
