# Plan 28: analog video, decoded and watched live

Design spec, 2026-11-06. Decided with the operator in a brainstorming session; the operator chose
"the video stream modes first" from the video work the original design named (section 4.3: a
`video` stream, "demodulated analog FPV; digital links such as DJI are reported honestly as not
decodable rather than attempted"). A reviewer read it as the builder before hand-over; its
findings are folded in (the B210's real ceiling and the worker wire, the channel rate, the mix's
selection rule, where "not decodable" lives, `stream save` as a client command, the identify
proof, the full-area pane, the modes-check bandwidth, the FPV plans). Builds on the stream bus (`crates/zeta-core/src/stream.rs`, `Payload::Video` since plan 16), the
plugin wire's protocol 2 (`crates/zeta-plugin/src/wire.rs`), the typed binary envelope
(`crates/zeta-vrt/src/envelope.rs`, used by `crates/zeta-ws/src/stream.rs`), the mode registry
(`crates/zeta-core/src/modes/`, `modes/*.json`), the WFM demodulator (`crates/zeta-dsp/src/wfm.rs`),
the pane strip (`crates/zeta-ui/web/panes.js`) and the waterfall overlays (`overlays.js`).

## 1. Intent and why

The operator has four analog video transmitters on the bench: a consumer reverse-camera kit, a
DIY FPV VTX and a "wireless HDMI" sender, all on 5.8 GHz, and a 1.3 GHz VTX. All four put a PAL
or NTSC composite picture on a wideband FM carrier, most with one or two FM audio subcarriers
above the video. Today the node has the `video` stream type (rgb8, gray8 or png frames), the
plugin wire carries it under protocol 2 and `stream tail` summarises it, but nothing produces a
frame, the websocket's binary envelope sends video with an empty body, and the page has no
picture. The operator's words on what matters first: "watching it live on the page first, record
later", and "luma now, chroma later".

So this plan builds one thing end to end: a `video_decode` mode row that demodulates an analog
FM video carrier (on the B210 today, on any wide source tomorrow) into black-and-white frames,
a websocket path that carries them to the page compressed and paced, and a page that shows the
picture as a live thumbnail riding its band on the waterfall and, on a click or the Video tab,
as the full main area with the other feeds switchable. Before the video stages, it makes the
transport from a source process to the node carry a wide front end without loss, because the
node is a framework for radios yet to come (an FPGA front end among them), not a solution
around one device.

## 2. Goals, non-goals, anti-goals

### Goals

- Each of the operator's four transmitters shows as a live black-and-white picture on the page,
  received by the B210 on the field host, tuned by hand to the carrier.
- The picture is smooth by default (the full field rate at reduced resolution) and sharp on
  request (full resolution, fewer frames a second); the trade-off is a page setting.
- Several video jobs on different radios run at once; the full view switches between them.
- The job is an ordinary mode row: `job start --kind video_decode`, the agent's tools, MCP, the
  page's menu and `stream tail` all see the same job and the same `video` stream. The CLI can
  save one frame to a file.
- The synthesiser produces a composite test signal, so the row has a `zeta modes check` test
  and the unit tests run without a radio; the real proof is captures of the four transmitters.
- A digital video link (WHDI, WirelessHD, the WiFi-based senders, DJI) is reported as not
  decodable where the node sees it, never attempted.
- The transport from a source process to the node carries a wide front end without loss
  (compact samples, blocks through shared memory, control never behind data) and each source
  reports the rate it can deliver, so this and every later wide source (another radio, an
  FPGA front end) plugs into a node that can take it (section 5.7).

### Non-goals (later plans)

- Colour (chroma demodulation, PAL or NTSC). The composite stage keeps the burst and the
  subcarrier out of the luma; nothing more.
- Recording: stills or clips into the capture store. The one-frame save is for fixtures only.
- Discovery: a detector that spots a video carrier in a band and a watch that brings it up
  unasked. The row gets a classify hook only if it falls out of the sync detector for free.
- The audio subcarrier into the mix and `transcribe`. It is designed for (section 5.4) and ships
  only if it falls out of the same baseband work; otherwise it is a gist.
- The HackRF. Its 20 Msps ceiling and USB 2 path put it below a 5.8 GHz video channel; the plan
  does nothing to stop a narrower capture being tried, and the picture degrades rather than
  the job refusing.
- Plugin video producers (protocol 2) gain nothing new; they already publish `video` frames and
  the page path built here shows them.

### Anti-goals

- A video job must not degrade the rest of the node: a 30 Msps capture, a demodulator at that
  rate and a frame encoder must stay on their own threads, drop frames rather than fall behind,
  and leave the CB transcribers, the mix and the page's spectrum running.
- No raw frames over the websocket. A PAL frame at 720 × 576 × 25 a second is 10 MB/s; the page
  gets compressed frames at a rate it has asked for, under the existing credits.
- No channel-grid assumptions (operator rule): the job centres on the measured carrier, and the
  FPV band tables are a channel plan for naming and the menu, never a snap.
- No guess at a digital sender's content.

## 3. Constraints

- The B210 on the field host is on a USB 3 port (5 Gb/s) with a 61.44 MHz master clock; the
  Soapy source's UHD ladder offers 20.48 and 30.72 Msps below the full rate (and 15.36 and 10.24
  below those). What the node has actually sustained is less: the record (`UAT.md` "Lease: cap a
  Soapy source's capture", `TODO.md` "Lease: cap a Soapy source's capture at the rate the device
  can stream") puts the reliable ceiling near 16-24 Msps, with overflows climbing at 56. Two
  costs sit between the device and the stage: the Soapy worker ships IQ to the node over a pipe
  as f32 pairs (`crates/zeta-source-worker/src/wire.rs`, 8 bytes a sample: 246 MB/s at 30.72
  Msps, parsed a sample at a time), and control frames already queue behind data blocks at 20
  Msps (`TODO.md` "Node: stream sockets are a fixed pool" and the worker gist). The operator's
  decision (2026-11-06): fix the transport first, as a framework piece (section 5.7). **The
  plan's first task is a measurement on the field host**, before and after: each rung from
  15.36 to 61.44 for a minute through the node, overflows and drops read from
  `source_list.stats`. The row's default band (24 MHz) assumes the 30.72 rung holds after
  the change; the numbers go in the plan. The host has 12 threads (Ryzen 5 5600), 125 GB of
  memory and an RTX 4090 that also serves speech and the agent's model; the video path must
  not need the GPU.
- The channel stage decimates by an integer (`crates/zeta-dsp/src/channel.rs`), and the mode
  expander fills `$out_rate_hz` from the capture rate and the band, so a 24 MHz band on a 30.72
  Msps capture gives the demodulator the full 30.72 Msps. The video demodulator takes the
  channel rate as it comes and decimates the composite itself.
- `zeta-core` takes no new dependencies (the mode-registry rule). An image encoder goes in the
  crate that encodes (section 5.5).
- The websocket pool is 64 sockets for every client; a page costs a fixed number (plan 27).
  A video feed adds one socket per shown picture, never one per video job.
- The plugin wire's `MAX_BODY` (16 MiB) and the envelope's `MAX_BODY` (16 MiB) bound a frame;
  a 4K rgb8 frame does not fit and is not a target.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated; the row's prose goes in
  `crates/zeta/src/agents_md.rs` and `docs/agents/field-craft.md`.
- UAT is deferred: the operator sets up each transmitter on the bench in turn. The plan's live
  acceptance section lists the four and what "shows live" means for each; none runs in CI.

## 4. The analog video signal (reference for the builder)

- Composite video: 625 lines / 50 fields (PAL, 15.625 kHz line rate, 64 µs line, 4.7 µs sync
  tip, 288 active lines a field, 52 µs active line) or 525 / 60 (NTSC, 15.734 kHz, 63.6 µs
  line, 240 active lines a field). Luma occupies 0-5 MHz (PAL) or 0-4.2 MHz (NTSC); the colour
  subcarrier sits at 4.43 MHz (PAL) or 3.58 MHz (NTSC) and is low-pass filtered out of the luma
  here. Sync is below black ("blacker than black"): the demodulated baseband is negative-going
  on sync and the detector finds the tips, then the vertical interval by its broad pulses.
- FM video: the carrier is frequency modulated by the composite with a deviation of several
  MHz (FPV transmitters are not standardised; 5-8 MHz peak is common), with pre-emphasis
  (CCIR 405 for PAL, ~50 µs-ish networks in cheap VTXs) that the receiver de-emphasises. The
  occupied bandwidth is about 20-27 MHz. Audio rides as FM subcarriers at 6.0 and 6.5 MHz
  (FPV) or 6.5 MHz alone (many cameras), each a narrowband FM channel on the baseband.
- Channels: 5.8 GHz FPV bands A (5 865 down to 5 725, descending), B (5 733 up on 19 MHz
  steps), E (irregular), F (FatShark/IRC, 5 740 up on 20 MHz steps) and R (RaceBand, 5 658 up
  on 37 MHz steps), 40 letter-named channels; reverse cameras sit on a few fixed 5.8 GHz or
  2.4 GHz channels; 1.2/1.3 GHz VTXs on 1 080-1 360 MHz. Reference only: no channel plan ships
  in this plan (section 5.3) and the job centres on the measured carrier.
- Digital senders: WHDI (5 GHz, ~40 MHz OFDM), WirelessHD (60 GHz), WiFi-based HDMI kits (5
  GHz, 20/40 MHz OFDM). An OFDM carrier that wide with no line structure in the demodulated
  baseband is what the row's "not decodable" verdict keys on (section 5.6).

## 5. The design

### 5.1 The job and the row

A mode row `modes/video_decode.json`, family `tv`, label `Video`, `protocol: "video"`, graph
kind. Band `bandwidth_hz` 24 000 000 (the requested channel; the lease rounds the capture up the
B210's ladder to 30.72 Msps, or 20.48 when the operator asks for 18 MHz), `centers_hz` empty.
The graph:

```
channel (center_hz, bandwidth_hz, out_rate_hz)  →iq→  video_fm_demod  →audio(composite)→  composite_decode  →video→ (stream)
                                                                        └→audio(subcarrier) → (stream, if built)
```

- `channel` is the existing stage; at a width the bank cannot serve it filters directly, as
  today. `$out_rate_hz` is whatever the expander gives (the capture rate for a band this wide,
  section 3), so the demodulator sees 30.72 Msps on the default rung.
- `video_fm_demod` (new, `zeta-dsp/src/video_fm.rs`): a limiter and the phase discriminator
  from `nbfm.rs` at the channel rate, then an integer decimation of the composite to a
  `composite_rate_hz` the stage picks (the largest integer divisor of the input rate at or
  under 16 MHz: 15.36 MHz from 30.72, 10.24 from 20.48; it must hold the 6.5 MHz subcarrier
  and the luma with de-emphasis room, and 10.24 does, barely), video de-emphasis (a CCIR 405
  network for PAL, a selectable `deemphasis` param: `ccir405`, `none`, `<tau_us>`), a DC/black-
  level tracker, output a composite `audio` frame (one channel, the composite rate, f32, sync
  negative). It is an `audio` payload because the bus has no composite kind and `audio` carries
  rate and channel count; the frame's provenance names the job, and the row's docs say a video
  demodulator's audio output is composite, not for ears. Keeping it out of the mix and
  `job_attach` is section 5.4.
- `composite_decode` (new, `zeta-dsp/src/composite.rs`): sync-tip detection (a clamp and a
  slicer below black), horizontal lock (a PLL on the line period, both standards tried, the one
  that locks wins; `standard` param `auto|pal|ntsc`), vertical interval detection (the broad
  pulses), field and line assembly, a 4.43/3.58 MHz notch and a luma low-pass, then resampling
  each active line to the output width. It publishes one `gray8` `video` frame per field (50 or
  60 a second), `width × height` = 720 × 288 (PAL field) or 720 × 240 (NTSC field) by default,
  with the frame's `start_ns` the field's first line. Interlace is left to the page (it draws
  fields; weaving two fields into a 576-line frame is a later nicety). A `scale` param
  (`full|half|quarter`) sets the width the stage emits, so a subscriber that wants the thin
  stream can ask the job for it, but the per-subscriber trade-off lives in the ws path (5.5).
- Lock state is a `video_lock` event on the job's event stream (and so the source feed):
  `{"protocol":"video","summary":"PAL 625/50 locked · 28 dB", "standard":"pal"|"ntsc"|null,
  "locked":bool, "line_hz":N, "fields":N, "snr_db":x}` on each change and every 5 s. The
  `summary` goes on the job's tag snippet, as other rows' summaries do. A job that never locks
  stays running (the picture is noise); the row's `identify` proof is `proof: field, field:
  locked` (`min_events: 1`, `dwell_ms: 4000`), and `quick: null`: there is no in-crate quick
  attempt for video (`QUICK_ATTEMPTS` is aprs, pocsag, acars and rds), and none is written here.
  Layer 1 labels nothing wider than 250 kHz, so the row's modulation score term never fires;
  identify is the only path to a video verdict, and the page's menu starts the job from the box
  (5.3), never through the fit.
- Audio subcarriers (5.4) are a third stage, `subcarrier_demod`, only if built.

Why a graph row and not an external program: there is no mature external analog-video decoder
to wrap the way dump1090 is wrapped, the DSP is a few hundred lines, and the stage contract
puts the picture on the bus where every transport already is.

### 5.2 The stream to the page

The binary envelope carries a video frame's bytes as its body (today the body is empty for
video): `u32LE header_len | FrameSummary JSON | u32LE body_len | body`, the header's payload
`{"width","height","format","bytes":N}` as `stream tail` already prints, the body the frame
in its `format`. `parse_envelope` accepts it; `zeta-client`'s envelope reader likewise.

Compression and pacing are per subscriber, in the ws layer, not in the stage:

- `GET /stream/{id}?format=binary&video=<profile>` with profile `smooth` (default),
  `sharp` or `thumb`. The ws task owns a per-connection encoder: it takes the newest frame the
  bus delivered (never a queue: a video subscriber keeps the latest, so a slow page sees
  fewer frames, never older ones), scales it to the profile's width (`smooth` 360 wide, every
  field; `sharp` full width, at most 5 a second; `thumb` 120 wide, 2 a second), encodes it as
  `png` (gray8, fast compression level) and sends it under the existing credit scheme. The
  builder may choose JPEG (a `jpeg` `VideoFormat` added to core, a pure-Rust encoder in
  `zeta-vrt` or `zeta-ws`, never in `zeta-core`) when PNG proves too large for `sharp`;
  measure first: a 360 × 288 gray8 field as PNG is roughly 30-60 KB, 50 a second is 2-3 MB/s on
  a LAN, which is the "smooth" budget. A profile that cannot be met is reported, not papered:
  the `lagged` message's `missed` counts the frames skipped.
- Text mode (`format=text`) prints the summary only, as now.
- The ws stats gain `video_frames_sent` and `video_frames_skipped`.

### 5.3 The page

- **The thumbnail on the band.** On the radio's own tab, a running video job's tag on the
  waterfall carries a small live picture (`thumb` profile) beside the kind badge, pinned to the
  job's outline like the ribbons and the squelch tag, in the lane logic `overlays.js` already
  has. One socket per shown thumbnail; thumbnails of jobs on sources not shown are not opened.
  The thumbnail shows "no lock" greyed when the last `video_lock` event says so.
- **The full view.** A `Video` tab in the pane strip, registered while any video job runs on any
  source (`eligible`), and a click on a thumbnail, both select it. Unlike the other panes it
  takes the whole main area in both footprints. That is new: today a strip pane is the bottom
  of `#main` in the wide footprint (`data-strip`) and takes the waterfall's place only in the
  narrow one (`data-pane`), and the map is not a pane at all but the right half of `#main`.
  `panes.js` grows a `full: true` registration option; `style.css` gives a full pane the whole
  of `#main` under `<html data-strip="video">`, the waterfall hidden (not destroyed: its
  sockets and ring stay, so leaving the pane shows it at once). The view: the selected feed's
  picture scaled to fit, a status
  line over it (source, centre, standard, locked, fields a second received, SNR, the profile),
  and a thumbnail strip of every running video feed on every source along the bottom; a click
  switches the feed (one `smooth` or `sharp` socket for the selected feed, `thumb` sockets for
  the strip). Leaving the pane closes its sockets and returns the waterfall.
- **The setting.** `view set video smooth|sharp` (default `smooth`), a view policy in
  `view.js`, persisted like the others; a toggle on the status line sets the same policy.
- **Jobs pane.** The job row keeps its cells (kind, source, state, id, band, events); the
  latest `video_lock` summary is the tag's snippet, and the full view's status line.
- **Menu.** "Decode video" on a boxed signal starts `video_decode` at the box's centre and
  width directly (the fit has no path for a signal this wide, section 5.1; never a grid).
- **Channel plans.** None in this plan. FPV channels are letter-named (A1-A8 descending,
  B on 19 MHz steps, E irregular, F, R), and the registry numbers channels on uniform steps
  and matches a job to a channel within 1 kHz, which a job centred on a measured carrier never
  meets. Letter names and a per-plan match tolerance are a registry change for a later plan
  (a `TODO.md` gist); `plan_start` on a video plan is not wanted.
- **Themes.** The thumbnail and the full view use the theme's tokens; console's frame draws
  the picture inside its bars. No new theme work.

### 5.4 Audio subcarriers (designed, built only if free)

`subcarrier_demod` on the composite: a band-pass at one subcarrier (`subcarrier_hz`, default
6 500 000), an NBFM discriminator, 48 kHz mono audio frames on an `audio` stream with the
job's provenance. Then "Listen to all", `job attach --kind transcribe` and the transcript log
work unchanged. One subcarrier per job: the mix and `job_attach` both take a job's **last**
audio output in graph order (`last_audio` in `node/mod.rs`, the same rule in `ops.rs`), so a
second subcarrier stream would be unreachable; a second job on the same band is the way to
hear the other.

The composite stream must not be that "last audio output". The mix filters on the job record
(`mixable`: not attached, not owned, not a mix, not a no-voice plan channel) and then takes
the last `Audio` output; neither looks at the stream's registration, and `StreamInfo` carries
only id, kind and provenance. The mark goes on the stage output: `StageSpec`'s output
declaration gains `role: "composite"` (default none), `JobRecord.outputs` keeps it, and
`last_audio` skips outputs with a role. A video job without the subcarrier stage then has no
mixable audio at all, which is right: "Listen to all" never sums a composite.

### 5.5 Where the code goes

- `zeta-core`: `VideoFormat::Jpeg` only if 5.2 chooses JPEG; the `video_lock` event shape in the
  stage-kinds doc; the output `role` (5.4) on `StageSpec` and `JobRecord`; the test block's
  `bandwidth_hz` (section 6). No new op: `zeta stream save <stream> <path>` is a CLI command in
  `crates/zeta` over the binary websocket through `zeta-client` (one frame, written as the
  PNG it arrives as under the `sharp` profile), so it works with `--node` and needs no encoder
  in `zeta-core`. Audio and IQ streams are refused with a pointer to `capture_*`.
- `zeta-dsp`: `video_fm.rs`, `composite.rs`, `subcarrier.rs` (5.4); `synth.rs` gains a
  `SynthSpec::Video { standard, deviation_hz, subcarrier_hz, rate_hz }` family whose signal is a
  test card (bars and a moving block, so a decoded field can be checked by pixel) FM-modulated
  at `rate_hz`, alongside the existing families; the row's test block is
  `{"kind":"synth","family":"video","standard":"pal",...,"expect":{"locked":true,"standard":"pal"}}`.
- `zeta-vrt`: the envelope's video body; the encoder and scaler for 5.2 (a `png` crate or
  `image` with only the png and jpeg features; the builder picks and justifies the size).
- `zeta-ws`: the `video=` query, the per-connection encoder, stats.
- `zeta-ui`: `video.js` (the subscriber, decoding `png` bodies with `createImageBitmap`, the
  thumbnail element, the full view), hooks in `overlays.js`, `panes.js`, `view.js`, `menu.js`,
  `agent.js`, `style.css`; the e2e suite gets a scenario on a synthesised video fixture
  (a synthesised video capture at the test bandwidth of section 6, a few seconds, written by
  the suite into its temp dir, never committed).
- `zeta`: `agents_md.rs` prose for the row, the stages, the `video=` profiles and the
  "digital links are reported, not decoded" rule; `AGENTS.md` and the skill regenerated;
  `docs/agents/field-craft.md` on tuning to a VTX (centre on the measured carrier, the
  bandwidth, the B210 rung, what "no lock" looks like); `zeta stream save`.
- `zeta-source-worker`: only under 5.7.

### 5.6 The digital-sender verdict

The operator wants two honest answers kept apart (2026-11-06): **not yet decodable** and
**insufficient bandwidth**. They have different causes and different fixes, and a report that
blurs them sends the operator to the wrong one.

- **Not yet decodable** is the digital link: `identify` on a digital sender runs
  `video_decode` among the plausible rows, it never locks, and the verdict is the existing
  unknown path with the rows tried; the field craft and the row's notes say that a 20-40 MHz
  OFDM carrier at 5 GHz with no line structure is a digital video link (WHDI, a WiFi HDMI kit,
  DJI) that the node reports and does not decode, "not yet": a later row may. It is prose and
  a verdict, not a catalogue field: the catalogue (`crates/zeta-dsp/data/signals.json`) is
  third-party data with no such field and no DJI, WHDI or FPV entries, and the precedent for
  an honest "we do not decode this" is MeshCore in `docs/agents/field-craft.md`. No OFDM
  detector, no catalogue overlay, no new classify field.
- **Insufficient bandwidth** is the source: the row's band is wider than the source can
  deliver (its device ceiling, or the measured transport rate of 5.7). The lease refuses the
  start naming the cap (`insufficient bandwidth: <source> delivers at most <rate>, the row
  needs <band>`), `identify` lists the row in `tried` with that reason instead of running it,
  and the page's menu greys "Decode video" with the same text on a source that cannot. The
  operator then knows to move to a wider radio, not to wait for a decoder.

### 5.7 The source transport, made rate-proof first

The operator's framing (2026-11-06): "we are building a framework here and not simply a
solution around one bit of hardware"; other radios follow, and at some stage RF at high
bandwidth fed into an FPGA with its own polyphase bank. The transport between a source process
and the node is the first place every wide front end meets the node, so it is fixed as a
framework piece, before the video stages, not patched for the B210.

Today (`crates/zeta-source-worker/src/wire.rs`, protocol 1): the child's stdout carries
length-prefixed frames, control and IQ blocks in one ordered stream, a block being a 28-byte
header and `n` complex samples as little-endian `f32` pairs; the node parses a sample at a
time; the child drops blocks past `MAX_QUEUED_BLOCKS` and counts them. Control replies queue
behind data.

The change, worker protocol 2:

- **Sample format on the wire** is declared per stream, `cf32` or `sc16` (the device's native
  format where it has one: the B210 and the HackRF deliver 12- and 8-bit samples, so `sc16`
  loses nothing), with a `scale`; the node converts to `Complex<f32>` in blocks on receipt
  (a vectorisable loop over the whole block, not a per-sample read). The child picks the
  compact format by default.
- **Blocks move through shared memory** when both sides can (a memfd ring the node creates
  and passes to the child, blocks as offsets in control frames) and through the pipe
  otherwise; the pipe path stays for a remote or sandboxed child and for the tests. The ring's
  size is a budget in samples per source (a `Limits` entry, default two seconds at the
  device's top rate), and a full ring drops the oldest block and counts it, as the pipe does.
- **Control is not behind data.** Control frames go on their own pipe (the child's stderr is
  logs today, so a third fd, or the existing stdout kept for control with data in the ring),
  so a tune reply never waits behind queued blocks (`TODO.md` "Node: stream sockets are a
  fixed pool" notes the wait at 20 Msps).
- **The source says what it can deliver.** `source_list` gains `transport: {format, path:
  "shm"|"pipe", rate_hz_max_measured}` from a short self-test on open (the child streams for
  a second at the top rung and reports what reached the node without drops), and the lease
  caps a request at it (`TODO.md` "Lease: cap a Soapy source's capture at the rate the device
  can stream" is done by this), naming the cap in a `conflict`-style refusal rather than
  letting the capture overflow.
- **A protocol-1 child is still accepted** (the first hello decides, as plan 20 settled), so a
  deployed worker binary older than the node keeps working on the pipe.

Proof: the measurement of section 3 before and after, on the field host, at every rung from
15.36 to 61.44 Msps; the fault paths of plan 20 (a killed child, a cable pull, an overflow
storm) retested on protocol 2; the node-source crate (`zeta-source-node`, VRT over the
network) untouched, since it already carries `sc16` and is the shape a remote FPGA front end
would speak. A front end that channelises in hardware (a PFB on the FPGA) arrives later as a
source whose port offers channels, and the video row needs nothing from it beyond a channel
at some rate, which section 5.1's "decimate the composite yourself" rule already allows.

## 6. Proof

- Unit: the sync detector and line PLL on the synthesised composite (PAL and NTSC, with AWGN at
  10, 20 and 30 dB, with a 1 MHz carrier offset, with the 6.5 MHz subcarrier present); a decoded
  test-card field matches the card within a tolerance per bar; the envelope round-trips a
  video body; the ws profile scaler meets its rate and size budget on a 50-field stream.
- `zeta modes check video_decode` passes on the synthesiser. The checker starts the job at the
  row's `band.bandwidth_hz`, and a file source's ceiling is the file's rate, so a 24 MHz row
  would need a 24 Msps synthesised capture (hundreds of MB a few seconds). The test block gains
  an optional `bandwidth_hz` the checker uses in place of the row's band, and the video test
  uses 8 000 000 with a low-deviation synthesised carrier (2 MHz peak, no subcarrier), which
  the same stages decode: the DSP is rate-agnostic by construction, and the real-rate proof is
  the fixtures. The e2e suite's fixture is the same synthesised capture.
- Fixtures: captures of the four transmitters from the B210 on the field host (a few seconds
  each, SigMF, cut as the OOK remote captures were) under `crates/zeta-dsp/tests/data/`, or
  under a fetched-fixtures rule if a few seconds at 30.72 Msps is too large for the repository
  (the builder's call: a 2 s cf32 capture is 490 MB, so expect a short, decimated cut of each,
  1 s at 15.36 Msps sc16 is 61 MB, still large; the plan says which and where). The composite
  tests run on them (locked, standard, a plausible field). Captured after the first deploy, as
  the DSP fix-round lessons say, and the margins measured base against head.
- Live (deferred, the operator sets each up): each transmitter shows live on the page from the
  B210; the thumbnail, the full view, the switch between two feeds (two jobs, section 8 says
  which radios), smooth and sharp, and the node's CPU and the other radio's jobs unharmed while
  it runs. A video job on the B210 is a lease conflict with anything else there (the CB plan
  included), so coexistence is tested across radios, not on one. The digital sender's verdict,
  if the operator's HDMI kit turns out to be digital.

## 7. Left to the builder

- The composite rate, the de-emphasis network and the discriminator's handling of the sync
  tip (FM video's sync is at the carrier's lowest deviation; a limiter helps).
- PNG against JPEG for `sharp`, and the crate.
- Whether fields or woven frames leave the stage (fields, by default here).
- The exact thumbnail size and lane behaviour beside the existing tags.
- The fixtures' cut and where they live (section 6).
- The worker wire's form, if 5.7 applies.
- The classify hook, if the sync detector gives one for free.

## 8. Open questions for the operator

Put to the operator after the builder check and answered 2026-11-06.

1. The source transport: fixed first, in this plan, as a framework piece ("option A, fix the
   belt first"), after the implications were laid out: the alternative, taking the rung the
   pipe holds today, loses the audio subcarrier on a wide 5.8 GHz transmitter and no
   channelising brings it back, since the audio only exists after FM demodulation.
2. The operator's framing for every bench question: the node is a framework, not a solution
   around one radio. The live checks are therefore written against roles ("a radio wide
   enough for the channel", "a second radio") and run with what is on the bench: if the
   HackRF gives a soft 1.3 GHz picture at its 20 Msps ceiling, that is a data point; if the
   B210 is the only wide radio, the two-feed switch is proved in the e2e suite and the
   "other jobs unharmed" check runs with the CB plan on the other radio.

Non-blocking: whether the reverse-camera kit is 2.4 GHz (the operator thinks 5.8), and whether
the HDMI sender is analog or digital (either outcome is a test).
