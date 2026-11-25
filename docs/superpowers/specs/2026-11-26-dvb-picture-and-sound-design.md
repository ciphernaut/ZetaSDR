# Digital television, watched and heard: a transport stream from a handler, services as jobs

Design doc. **Status: draft, not approved.** Written 2026-11-26 while the operator was away, from
the gists in `TODO.md` ("DVB-T: picture, sound and services"; "Picture frames and a transport
stream from a handler"; "An audio output for handlers") and a spike of 2026-11-25. No plan number
is assigned. Every call that is the operator's to make is designed to a recommended default and
listed in section 11.

Parents: `2026-11-21-alternative-handlers-design.md` (handlers, and `audio` named as a handler
output), `2026-11-14-dvbt-signalling-design.md` (plan 39), `2026-11-22-onair-callback-modes-design.md`
(plan 46, which left picture, sound and the transport stream out), `2026-11-19-dvb-broadcast-modes-design.md`
(DVB-T2 and DVB-S, which say they reuse this path) and `2026-11-06-video-decode-design.md` (the
`video` stream and the page's Video tab).

## 1. Intent and why

A `dvbt_decode` job locks a multiplex and says what it signals; with the OnAir handler it also
names the services. The operator asked on 2026-11-14 for "signalling first, picture and sound
later". This is the later: pick a service by name on the page, see its picture in the Video tab,
hear its sound in the mix, and attach a transcriber to that sound like to any voice job.

The spike proved the signal path. A branch of `onair-rx` wrote the receiver's 188-byte transport
stream packets to a file. Thirty-two seconds from the field host's radio at 10.24 Msps gave
488,005 packets, none flagged, no continuity errors, and `ffmpeg` decoded a high-definition and a
standard-definition service from it. The receiver took 0.73 of a core locked.

What is missing is all node: a program's output is text lines and packets, nothing carries a
byte stream, nothing decodes compressed picture or sound, and a handler job has no audio.

## 2. Goals, non-goals, anti-goals

### Goals

- **G1. A transport stream is a handler output**, declared in data by any handler program (OnAir's
  `dvbt` now; `dvbt-rx`, a DVB-S or a DVB-T2 receiver later), carried as a typed stream on the
  bus, with a quality read-out and a mark where it breaks.
- **G2. A service is a job.** The operator picks a service by name; several services of one
  multiplex run at once from one receiver; changing service never touches the receiver.
- **G3. Picture in the existing `video` stream, sound in the job's audio stream.** The Video tab,
  the mix with its mute, `job_attach transcribe`, `stream tail`, the CLI and the agent then see a
  television service as they see any other job.
- **G4. The decoder is an external program the operator installs** (`ffmpeg`), found and probed
  for what it accepts.
- **G5. A weak multiplex and a slow host say so.** The page shows a word and numbers, never a
  frozen frame alone; a host that cannot decode at full rate steps down by measurement and says
  which step it is on.
- **G6. Named later steps, with their slots kept:** recording and replay of a transport stream,
  subtitles as text, the programme guide, the browser's own decoder for a full-resolution picture,
  DVB-S and DVB-T2 on the same path.

### Non-goals

- Descrambling. A scrambled service is listed as scrambled and cannot be started.
- A full-resolution, full-rate picture in the first plan (section 4, approach A, is its slot).
- Stereo or surround sound. The mix takes 48 kHz mono only (`zeta-dsp/src/mix.rs`); the service's
  sound is mixed down. Stereo follows the mix (`TODO.md`, "`wfm_demod` stereo").
- Interactive services, data carousels, MPE or any IP carried in a multiplex (the boundary of
  `2026-11-19-dvb-broadcast-modes-design.md` is unchanged).
- Finding multiplexes. A watch, a scan or the register finds them; this starts at a locked job.
- Any change to how the receiver locks, or to the row's signalling events.

### Anti-goals (a failure even if it works)

- A `dvbt_watch` kind, an OnAir-only byte path, or a second copy of the row per output: one
  mechanism for every handler that has a transport stream.
- A blocked reader stalling the receiver. The spike's notes say why: the receiver's packet
  callback runs on its feed thread.
- A number taken from one station: a channel raster, a channel width, a core count, a bit rate, a
  service count or a codec list. Widths come from the job and the lock, codecs from the stream
  and the decoder's probe, capacity from measurement.
- Copyleft code in this repository or linked into the node. `ffmpeg` and the receivers stay
  programs the operator installs (`docs/licensing.md`, "External handlers").
- A real broadcast, service name, broadcaster or frequency in a fixture, a test or a document.
- Programme content leaving the node by any path the operator did not open (section 5.9).

## 3. Constraints (how things stand, read from the code on 2026-11-26)

- **A program block has two outputs.** `external_decode` (`zeta-ext/src/external.rs`) reads
  events from stdout or a port and, when the block has `packets`, packets from a second local
  port: the program listens, the node picks a free port, passes it as `$packet_port` and connects.
  It declares the outputs `event` and `packet` only.
- **`$bandwidth_hz` is already a placeholder** for a program's arguments (`PROGRAM_PLACEHOLDERS`,
  `modes/row.rs`), and the OnAir DVB-S entry already passes it. The DVB-T entry passes no width,
  and `onair-rx --bandwidth` takes megahertz (8, 7, 6 or 5), so the receiver searches. The job's
  band is not the multiplex's width either: a job started without one takes the row's 8 MHz.
- **A slow program does not stall the node.** The stage's feed task waits on the program's stdin;
  the bus is a broadcast channel of 256 frames, so the feed then lags and skips input, and
  nothing upstream blocks. What a blocked write in the receiver's callback costs is its own
  input, then its lock.
- **Stream kinds** are `iq`, `audio`, `video`, `text`, `event`, `sink`, `packet`
  (`zeta-core/src/types.rs`). A `video` frame is `rgb8`, `gray8` or `png` with a width and a
  height; its time is its provenance span. The websocket sends every video frame as a PNG it
  encodes itself, newest frame only, paced by credits, in three profiles (`smooth` 360 wide,
  `sharp` at full width 200 ms apart, `thumb`). `TODO.md` records `sharp` at about one frame a
  second on analog video: the node's PNG encoder is the limit.
- **The Video tab** (`zeta-ui/web/video.js`) is a full pane beside the radios, eligible while a
  `video_decode` job runs, drawing one job's frames on a canvas with a status line and a strip of
  thumbnails. The page uses no `<video>`, MediaSource or WebCodecs anywhere. The page's content
  security policy has no `media-src`, so media from a `blob:` URL is refused.
- **Sound.** A job's last audio output with no role is what the mix and `job_attach transcribe`
  read. The mix is made on the node, takes 48 kHz mono, and mutes by `{source, center_hz}`: two
  services of one multiplex share both.
- **A helper program that gives sound exists**: the vocoder stage (`zeta-dsp/src/vocoder.rs`)
  runs `zeta-mbe` or `c2dec` as a child, bytes in, PCM out at 48 kHz mono, found by
  `ZETA_HANDLER_<PROGRAM>` or on `PATH`.
- **Attached jobs.** `job_attach` takes `transcribe` and `packets`. An attached job reads another
  job's stream, holds no lease and follows its input through a retune. Attaching `packets` to an
  attached job is refused.
- **Stores.** Two kinds of capture: IQ bursts (SigMF, 256 MiB kept by default) and packets
  (pcapng, rolled at 256 MiB, 4 GiB kept). No generic recording of a stream. `GET /captures/<file>`
  reads the whole file into one response body.
- **`content`** is a top-level row field with one value, `network_traffic`; nothing reads it yet.
- **OnAir's DVB-T generator** takes any source of 188-byte packets (`dvbt::Generator`,
  `PacketSource`). The row has no check: an IQ capture of a multiplex is tens of megabytes a
  second.

### What was measured (2026-11-26)

On the spike's multiplex, with `ffmpeg` 6.1 on the field host (six cores, twelve threads) while
its node ran. Cores are CPU seconds over the 32 s of content, so 1.0 is one core at real time.
The scripts are in the builder's scratch (`measure.sh`, `ladder.sh`, `corrupt.py`, `browser.py`).

| Work on one service | HD (1080 lines, interlaced, 25 a second) | SD (576 lines, interlaced) |
| --- | --- | --- |
| Pick the service out, nothing decoded | 0.007 | 0.006 |
| Sound to 48 kHz mono PCM (AC-3 5.1 / MP2) | 0.008 | 0.007 |
| Picture copied, sound to AAC, fragmented MP4 (approach A) | 0.023 to 0.027 | 0.022 to 0.025 |
| Picture decoded, thrown away, one thread | 0.12 to 0.15 | 0.025 to 0.027 |
| **Picture to 640 wide JPEG at 25 a second, deinterlaced, plus sound** | **0.42** | **0.17** |
| The same, one field kept instead of deinterlacing | 0.26 | 0.11 |
| The same, key frames only | 0.064 | 0.053 |
| Picture to JPEG at full size, 25 a second, deinterlaced | 0.69 | 0.18 |
| Re-encoded to progressive H.264 at 720 lines (`veryfast`) | 0.79 to 0.86 | 0.36 to 0.42 |

- The 640-wide JPEG picture is 5 Mbit/s (HD source) to 8 Mbit/s (SD source) at 25 a second, half
  that at 12.5. The full-size one is 22 Mbit/s.
- One service's own packets are 0.67 MB/s (HD) and 0.17 MB/s (SD); the whole multiplex is
  2.9 MB/s. The same band as IQ at this rate is 41 MB/s as `cs16`.
- Key frames come at most 1.2 s apart on both services.
- The multiplex lists ten services over seven sets of streams: three service ids name the same
  picture and sound.
- **Damage.** Copies of the HD service with 0.07 %, 1 %, 5 % and 24 % of packets replaced by
  flagged noise, in bursts of eight: `ffmpeg` gave 764, 749, 702 and 513 of 764 pictures, and
  exactly the same counts when the flagged packets were removed first (its demultiplexer drops
  them itself). The spike's note that flagged packets hurt `ffmpeg` did not reproduce.
- **A browser on the same damage.** The clean service, remuxed to fragmented MP4, played in
  headless Chromium 156 and Chrome 158 through MediaSource (1920 by 1080, no error). The 1 % copy
  stopped after 2.2 s with a fatal `PIPELINE_ERROR_DECODE`, and the 5 % copy before the first
  picture. Both browsers refuse AC-3 and MP2 and accept AAC and Opus. What a browser does with
  interlaced fields was not looked at.

## 4. Approaches

### How the bytes leave the program

1. **A local port the program listens on, raw 188-byte packets, marks in band** (chosen). It is
   how `packets` already works, so the stage, the port allocation and the connect rules are
   reused. A raw stream needs no parser in the program, a recording of it is an ordinary `.ts`
   file, and anything that reads a transport stream can be pointed at it by hand.
2. *A second pipe (a file descriptor passed at spawn).* Rejected: `proc::spawn` passes none, and a
   pipe's 64 KiB buffer puts the back-pressure in the kernel, where the program cannot count it.
3. *Packets wrapped in records on stdout beside the events.* Rejected: megabits of binary through
   the JSON line reader, and no other program could read it.
4. *A named stream the node offers and the program connects to.* Rejected: a second convention
   beside `packets` for the same job.

### Where a service is chosen

1. *A parameter of the multiplex job* (`service: "…"`). Rejected: one service per receiver, so
   two services cost two receivers at 0.73 of a core each, and a change of service is a relaunch
   that loses the lock.
2. **An attached job per service** (chosen): `job_attach` with a new kind, reading the multiplex
   job's transport stream, as `transcribe` reads a demodulator's sound. It holds no lease, costs
   the receiver nothing, and stop, list, the mix and the transcriber need nothing new.
3. *Child jobs the multiplex job starts by itself for every service.* Rejected: ten decoders
   nobody asked for; the operator chooses.

### Where the picture and the sound are decoded

- **A. Remux for the browser.** The node copies the H.264 picture and transcodes the sound to AAC
  in fragmented MP4 (0.025 of a core); the browser decodes through MediaSource. Cheapest for the
  node and the only way to a full-resolution picture at this cost. Against it, measured or read:
  the browser's pipeline dies on a 1 % damaged stream, which is the ordinary state of most
  multiplexes a station hears (three of 32 channels were clean on the field host's antenna); the
  page's content policy forbids `blob:` media today; the sound would play in a `<video>` element,
  outside the mix, its mute and the transcriber; the picture would be a second kind of video on
  the page beside the `video` stream; the codec must be one the browser has (H.264 today, an HEVC
  service tomorrow), and the UI suite's browser decides what can be tested.
- **B. Decode on the node** (**chosen**). An external `ffmpeg` per service turns the service's
  packets into JPEG pictures for the existing `video` stream and PCM for the job's audio stream.
  Measured at 0.42 of a core for an HD service and 0.17 for an SD one at 640 wide and 25 a
  second, with steps down to 0.06. `ffmpeg` conceals damage (749 of 764 pictures at 1 %) where
  the browser stops. Everything downstream exists: the Video tab, the thumbnails, the mix, mute,
  `transcribe`, `stream tail`, the agent. Any codec the installed `ffmpeg` decodes works in any
  browser. The cost is CPU on the node and a picture smaller than the broadcast.
- **C. Hybrid:** picture remuxed to the browser, sound decoded on the node. It keeps the mix and
  the transcriber, and keeps every weakness of A's picture; and the two halves then run on two
  clocks (the browser's media clock and the page's audio queue), so lip sync is a new problem.

B is chosen because it is the one that survives a weak signal, joins the paths the node already
has, and is testable with what the suites have. **The slot kept for the heavier one is A's
picture**: the service job's own transport stream is a stream on the bus (section 5.3), so a
later `picture: native` can remux it for the browser's decoder, for an operator who has a clean
multiplex and wants every line. It is section 9's part N, not built here.

## 5. Design

### 5.1 A transport stream as a program's output (G1)

A program block (a program row's own, or a handler entry) may declare a third output:

```json
"transport": { "via": "tcp", "port": "$ts_port", "container": "mpegts" }
```

- `$ts_port` joins `$port` and `$packet_port`: a free local port the node picks. The program
  listens on it; the node connects, with the rules `packets` has (retry for five seconds, give up
  when the child exits). A refused or lost connection is a note in the job's context
  (`external.transport`) and never fails the job: its events are its first duty.
- `container` is `mpegts` (188-byte packets, sync byte first) and the only value now. It is the
  slot for another container (a DVB-T2 or DVB-S2 receiver that hands over baseband frames).
- The handler's `outputs` gain `ts`, so `modes_list` says which handler of a mode can give
  pictures, `job_start`'s `needs: ["ts"]` passes over one that cannot, and the page can say
  "Watch via OnAir" as it says "Capture packets via".

**The program's side of the contract** (any program; `onair-rx` 0.5.0 is the first):

- Whole packets only, in the order received, aligned from the first byte.
- **Never block the receiver.** The packet callback copies into a bounded queue (two seconds of
  packets at the rate seen, or `--ts-buffer`), and a writer thread sends. A full queue drops
  whole packets, oldest first, and counts them. With nobody connected the queue is not filled at
  all.
- **Flagged packets are sent, flagged.** A packet the error correction could not repair goes out
  with its `transport_error_indicator` set, so that the count of them is the node's to make and a
  recording holds what was received. Readers drop them (below).
- **Marks in band.** The program inserts null packets (PID 0x1FFF, which every reader ignores)
  whose payload begins with an eight-byte tag and a version, followed by a kind and its numbers:
  - `time`: the sample count of the input at this point of the stream, at least four a second
    while locked. This is what gives pictures and sound their place in the node's time, on a
    replay as on air.
  - `gap`: packets were dropped here (how many), or lock was lost and regained here. It is the
    discontinuity marker: a reader resets its continuity counts and tells its decoder.
  The layout is written once in `docs/` beside the plugin protocol, so that a program in any
  language can write it. A stream without marks is still read: times then come from arrival, and
  gaps from the continuity counters alone.
- **The width.** The entry passes `--width $width_hz`. `$width_hz` is new: the width this job
  last locked at, when it has one, else the job's band. The program tries the standard width
  that fits it first and falls back to its search, so a job started on a measured band or
  relaunched after a lock never searches (0.25 s to lock in the spike against 2 s and a
  reconfiguration), and a job on the row's default band is no worse than today. The node writes
  the locked width into the job's context from the first event that carries `bandwidth_hz`.

**The node's side.** The stage connects at launch and always reads the port, as it always reads
a packet output's framing: reading is the back-pressure release and the source of the counts. It
publishes frames of a new stream kind only while something reads them.

- **`ts`, a new stream kind.** A frame is a whole number of packets (at most 64 KiB or 20 ms,
  whichever comes first), with `container`, `gap: true` on the first frame after a mark or a
  resynchronisation, and the provenance span from the `time` marks. Flagged packets are in it.
  Like `packet`, the peers that cannot take it refuse it by name.
- **A quality event** on the job's event stream once a second while the port is connected,
  beside the row's own events and under its own protocol so no row's schema changes:

  ```json
  {"protocol": "ts", "type": "quality", "packets_s": 14950, "bitrate_bps": 22484800,
   "flagged_s": 12, "dropped_s": 0, "continuity_s": 3, "resyncs": 0, "gaps": 1, "readers": 2}
  ```

  `flagged_s` are packets with the indicator, `dropped_s` what the program's `gap` marks
  reported, `continuity_s` the continuity counter errors over all PIDs, `resyncs` the times the
  reader lost the sync byte and found it again. The summary is one line the page shows as it is:
  `22.5 Mbit/s · 0.08 % bad · 1 gap`.
- **The service list, read by the node** (section 5.2), so that it is one reader for every
  handler: `dvbt-rx` gives no list today, OnAir gives names only.

### 5.2 The services of a multiplex (G2)

The stage's reader keeps the programme tables (PAT, PMT, SDT; a few hundred lines written from
the standard, no dependency) and publishes, when they are first complete and when they change:

```json
{"protocol": "ts", "type": "services", "network_id": 1, "services": [
  {"id": 1, "name": "ZETA TEST 1", "provider": "…", "kind": "tv", "scrambled": false,
   "video": {"codec": "h264"}, "audio": [{"codec": "ac3", "language": "eng"}],
   "subtitles": ["teletext"], "same_as": null, "decodable": true, "why": null}]}
```

- `codec` is the stream type as the tables give it, under `ffmpeg`'s decoder names where the
  node knows one, else the type number. `same_as` names an earlier service id carrying the same
  streams (three of ten on the multiplex measured). `decodable` and `why` come from the decoder's
  probe (section 5.4): `why: "this ffmpeg has no hevc decoder"`.
- The job's context keeps the latest list (`context.ts.services`), so a page opened later and the
  CLI (`job show`) have it without waiting.
- The handler's own `services` extra, where it has one, stays as it is.

### 5.3 A service is an attached job (G2, G3)

`job_attach` gains the kind `service`, attachable to a job whose handler outputs `ts`:

```json
{"op": "job_attach", "kind": "service", "job": "<multiplex job>", "service": "ZETA TEST 1",
 "picture": null, "sound": null}
```

- `service` is a name or an id from the list. A name is matched exactly, then without case; a
  name two services share is refused with their ids. An unknown, scrambled or undecodable service
  is refused with the reason and the list.
- `picture` is `null` (the policy of 5.5), or `{"width": 640, "rate": "full" | "half" | "keys" |
  "off"}`; `sound` is `null` (the first audio stream), a language, or `"off"`.
- The attached job's graph is three stages: `stream_in` on the `ts` stream → **`ts_service`** →
  **`av_decode`**.
  - `ts_service` passes the service's packets (its PMT's PIDs, a PAT rewritten to that one
    service, the PMT, the clock reference), drops flagged packets, counts what it dropped and
    the service's own continuity errors, and republishes a `ts` stream: the single-service stream
    a recording (5.8), a later browser picture (4, A) and the decoder all read. It follows a
    change of the PMT. Cost: a table lookup a packet.
  - `av_decode` is 5.4.
- The job's outputs are `video`, `audio`, `event` and `ts`. Its record copies the multiplex
  job's source and tuning and keeps `service` (the id and the name); its events carry `service`.
- **Several at once**: one attachment a service, any number, one receiver. Each costs what 5.4
  measures and nothing in the receiver.
- **What changing service costs.** Stop one attachment, start another: a decoder start, the
  tables (already held), and the next key frame, which was at most 1.2 s away on the services
  measured. The receiver, its lock and every other service are untouched. Sound starts with the
  first decodable audio frame, before the picture.
- **It follows its input** as attached jobs do: a retune or a relaunch of the multiplex job
  relaunches it; when the multiplex job stops, it stops. Lock lost and regained is not a
  relaunch: the `gap` mark flushes the decoder and the job runs on.
- **`transcribe` attaches to a service job.** Its sound is an ordinary audio stream, so the check
  "has an audio stream" passes; the builder makes sure an attached job may itself be an input
  (the refusal exists for `packets`), and that a relaunch walks the chain in order.
- **The mix.** The service's audio is 48 kHz mono with no role, so it joins the mix. The mute key
  gains the job: `{source, center_hz, job}`, with `job` absent meaning what it means today, so two
  services of one multiplex mute apart.

DVB-S and DVB-T2 reuse all of 5.1 to 5.3 by declaring `transport` in their entry; nothing here
names DVB-T.

### 5.4 The decoder: `ffmpeg`, found and probed (G4)

`av_decode` runs one child per service, on the vocoder stage's pattern: the service's packets to
its stdin; JPEG pictures back on stdout; PCM (signed 16-bit, 48 kHz, mono) on a second local
port. One process for both keeps picture and sound cut from one clock.

- **Found** by `ZETA_HANDLER_FFMPEG` (an absolute path) or on `PATH`, like every helper program.
  Missing: every service is `decodable: false` with `why: "ffmpeg is not installed"` and the
  install pointer; the multiplex job, its events and its recording are unaffected.
- **Probed, once at start-up and when the path changes**, by running it: `-version`,
  `-decoders`, `-demuxers`, `-encoders`, `-filters`, two seconds each. The node keeps what it
  found (which decoders, whether `mpegts`, `mjpeg`, `yadif`, `scale` and, for 5.7, a teletext
  decoder are there) and reports it in `modes_list`'s handler entry and the start-up report. A
  service is decodable when its codecs are in the probe. There is no table in the node of what
  "an ffmpeg" can do, and no flag per feature.
- **The command line is data**: a decoder file beside the handler files
  (`handlers/ffmpeg.json`, kind `av`), with the arguments for each step of 5.5 and placeholders
  (`$width`, `$rate`, `$audio_port`, `$program`). Another decoder program (or a hardware-decoding
  build's arguments) is an operator's file in `--handlers-dir`, not a code change.
- **Pictures.** `VideoFormat` gains `jpeg`: the frame is the JPEG as the decoder wrote it, with
  its width and height. The websocket sends a `jpeg` frame as it is for the `smooth` and `sharp`
  profiles (no PNG encode, which is the node's limit today) and scales only for `thumb`, at its
  500 ms pace. The page's `createImageBitmap` takes it unchanged.
- **Time.** A picture's and a sound block's spans come from the `time` marks through the
  decoder: the stage counts pictures and samples out against packets in. The page delays a
  picture by the lead its audio queue holds for that job's sound (100 to 500 ms today), so lips
  and sound meet within about a fifth of a second; the test fixture has a flash and a beep to
  hold the builder to it.
- **Failure.** The child exiting notes the job's context and restarts it with a back-off (a
  damaged stream must not end the job); three exits inside ten seconds fail the job with the
  stderr tail. A child that gives nothing for five seconds of input is killed and restarted.

### 5.5 Capacity: steps, chosen by measurement (G5)

The steps of a service's decode, each a set of arguments in the decoder file:

| Step | What the operator gets | HD | SD |
| --- | --- | --- | --- |
| `full` | 25 pictures a second, deinterlaced | 0.42 | 0.17 |
| `field` | 25 a second, one field kept | 0.26 | 0.11 |
| `keys` | key frames only (about one a second) | 0.06 | 0.05 |
| `off` | sound only | 0.01 | 0.01 |

(cores on the field host; another host's figures are its own.)

- With `picture: null` the stage starts at `full` and **measures**: packets in against pictures
  and samples out. A decoder that falls behind real time for three seconds is restarted one step
  down; the job's context and an event say so (`picture: keys (the decoder kept 0.6 of real time
  at field)`), and the page shows it. It tries one step up after a minute at under half load, and
  not again for ten minutes if that fails.
- Sound is never stepped down; it is 1 % of a core.
- A named `picture` is held, and a decoder that cannot keep it says so and drops pictures, not
  sound.
- No core count, no table of hosts. `TODO.md`'s "Node: a baseline of the host's capabilities"
  would let the node refuse or warn before starting; until it exists the measurement after
  starting is the mechanism.
- The receiver itself is outside this: a host that cannot run it loses input, and the row's
  `resyncs`, `channel.dropped` and now `ts` quality say so.

### 5.6 The page (G3, G5)

- **The picture lives in the Video tab.** Its eligibility becomes "a running job with a `video`
  stream", so a service is a feed like an analog video job, with its thumbnail in the strip and
  on its multiplex's tag. No new pane and nothing on the waterfall. The tab's `smooth` / `sharp`
  switch keeps its meaning for analog video; for a `jpeg` feed both show the decoder's picture.
- **The service picker** is in the Video tab's strip: under each multiplex job that outputs `ts`,
  its services by name, a running one lit. A click starts that service; a click on another
  **switches** (stops the one this page started on that multiplex, starts the other); a
  modifier-click or the entry's `+` adds it beside. An undecodable or scrambled service is greyed
  with its reason in the title. The same list is the multiplex job's menu entry **Watch ›**, and
  a job on a handler without `ts` offers `Watch via <handler>` (a sibling with `needs: ["ts"]`),
  as `Capture packets via` does.
- **Sound** is the mix: the service appears in the mix's list with its name, with mute. The
  Video tab's status line has a speaker toggle that sets that mute. Nothing plays outside the
  page's one audio path.
- **The read-out.** The status line gains the stream's summary and the step:
  `<source> · <service> · 1080i h264 · full · 0.08 % bad · 1 gap/min · snr 21 dB`. When no new
  picture has come for two seconds the canvas dims the last one and writes why over it, from the
  node's own words: `no picture: 41 % of packets bad (snr 16 dB)`, `no lock`, `waiting for a key
  frame`, `key frames only: the decoder cannot keep up`, `sound only`, `ffmpeg is not
  installed`. The Jobs pane's row for a service shows the same summary as its note.
- **Sizes.** The picture fills the Video pane as analog video does (the largest fit at the
  picture's own aspect, which the frame carries). No fixed size is added. The strip's share of
  the pane becomes a view setting (`view set video_strip <percent>`), dragged like the side
  column, for a multiplex with many services.
- **The agent and the CLI** get all of it from the op: `zeta job attach --kind service --job <id>
  --service "<name>"`, and the list in `job show`.

### 5.7 Subtitles and the guide: named later steps (G6)

Neither is in the first plan; each has its place fixed now.

- **Subtitles as text** (part K). Teletext and DVB subtitles of a running service become
  `subtitle` events on the service job (`text`, `language`, the span), through the decoder when
  the probe finds a teletext decoder in it, and are written where transcripts are: the event
  feed, the transcript ribbon and `transcripts.log`, marked `subtitle` so nothing mistakes the
  broadcaster's words for the transcriber's. The service list already says which services carry
  them.
- **The guide** (part L). The node's table reader adds the event information table's present and
  following entries: `now` and `next` (a title and times) on each service of the list and in the
  picker. The full schedule is not planned.

### 5.8 Recording and replay (G6, part J)

A transport stream is megabytes a second where IQ is tens (0.17 to 0.67 MB/s a service, 2.9 the
multiplex, against 41), so it is the affordable recording of a broadcast.

- `job_attach` kind **`record`**, on a multiplex job (everything, flagged packets and marks
  included) or on a service job (its single-service stream): a `.ts` file in a third store beside
  bursts and packets, with a keep limit of its own and a roll size, listed by `capture_list` as
  kind `ts` with the job, handler, service, band, times, bytes and the row's `content`.
- It is served at `GET /captures/<id>.ts`, and that route learns to send a file in pieces: it
  reads a whole file into memory today, which a pcapng at its 256 MiB roll already strains.
- **Replay** is `capture_decode` on a `ts` capture: a job with no receiver whose root is the
  capture replayed at the pace of its `time` marks, publishing the `ts` stream and the service
  list, to which services attach as on air; marked `replay` with all that follows from the mark.
  A capture's IQ replay through the receiver works already and is unchanged.
- `TODO.md`'s "Captures: a RAM-backed store for wide captures" is about IQ; if its `--bursts-dir`
  is built, the `ts` store takes the same option. Nothing here waits for it.

### 5.9 Content and what leaves the node

- `Content` gains **`broadcast`**: programme content sent to the public. `dvbt_decode` carries
  it (and a DVB-S or DVB-T2 television row will). As with `network_traffic` it is copied to the
  job's context, to service jobs and to `ts` captures, and read by nothing until the policy
  framework exists (`TODO.md`, "Policies"; "Access: ACLs on restricted rows"). It is a different
  word from `network_traffic` on purpose: receiving a broadcast is what it is for, while relaying
  or publishing it is the act a policy would gate.
- **Nothing is redistributed by default.** Pictures and sound go only to clients of the node's
  own authenticated stream sockets. This design adds no UDP, RTP or HLS output, no sink, no
  upload, and no unauthenticated route; `$ts_port` is a loopback port that lives as long as the
  job. A recording is the operator's act and its download needs the node's secret. A "send to a
  player" output, if ever wanted, is a new op under the policy framework, not a flag here.
- Subtitles (5.7), when built, enter `transcripts.log`: they carry `content: broadcast` too.

## 6. Testing

No real broadcast enters the repository.

- **A synthetic service, as a transport stream** (`modes/fixtures/tv-test.ts`, a few hundred
  kilobytes, made by a script kept beside it): four seconds, loopable, from `ffmpeg`'s test
  sources. Service `ZETA TEST 1`: a small interlaced H.264 picture with a moving bar, a white
  flash each second and a beep on the flash, MP2 sound. Service `ZETA TEST 2`: the same picture
  with AC-3 sound. Service `ZETA RADIO`: sound only. A fourth entry marked scrambled. The script
  needs `ffmpeg`; the file is committed so the tests need only what they test.
- **A stand-in receiver** for the stage tests (a script under `zeta-ext`'s test programs): status
  lines on stdout and the fixture on `$ts_port`, paced, with switches for marks, flagged packets,
  a dropped stretch, a stall of its own writer, and a loss of lock. With it: the framing, the
  counts of the quality event, a `gap` reaching the reader, a stalled reader losing packets and
  not the program's events, the service list, the refusals of `job_attach service`.
- **The decoder** against the fixture where `ffmpeg` is on the host (else skipped with the
  reason printed, as the UI suite does): pictures of the stated size arrive, the beep is in the
  audio stream, a flash's picture and its beep are within 100 ms by their spans, damage injected
  at 5 % still gives pictures, a step down happens when the stand-in decoder is slowed, and the
  probe refuses a codec the stand-in probe lacks.
- **The page** (UI suite, scenarios named for the smoke rule): the picker lists the fixture's
  services under the multiplex job, a click shows a picture whose flash the canvas shows, the
  mix lists the service and mute silences it, a switch replaces the service, the scrambled entry
  is greyed with its reason, and the weak-signal words appear over a dimmed picture when the
  stand-in flags half its packets.
- **Through a real receiver, generated at test time.** `onair-rx`'s generator program gains a
  scene that modulates a given transport stream file (null packets to fill the multiplex) with
  OnAir's DVB-T generator. A gated test (`ZETA_TEST_ONAIR_RX`, `ZETA_TEST_ONAIR_GEN`, as plan 45)
  generates a few seconds of IQ from the fixture, runs the real handler on it and expects the
  service list and a picture. The IQ is never stored: it is 40 MB a second. The row's `test`
  stays absent until `zeta modes check` can ask a program for its capture; that is noted, not
  built.
- **In the wrapper** (`onair-rx` 0.5.0, its own tests): the port, the queue's bound under a
  reader that stops, the counts, the marks, `--width`.

## 7. Acceptance checks (the operator's, on the page of a deployed node)

1. On a multiplex the node locks cleanly: open the Video tab, see the services by name under the
   job, click one, and see its picture within three seconds and hear it in the mix.
2. Click another service: the picture changes within three seconds and the multiplex job's lock
   time does not restart.
3. Add a second service beside the first: both thumbnails live, both in the mix, each muted on
   its own.
4. Attach a transcriber to a service with speech: transcripts appear as for a voice job.
5. On a multiplex that locks weakly: the status line shows a percentage of bad packets, and when
   the picture stops the canvas says why in words.
6. Pull the antenna for ten seconds and put it back: `no lock`, then picture and sound return
   with no click on the page, and the job never failed.
7. With the node held to two cores (`taskset`): an HD service still gives sound, the page names
   the step it fell to, and the waterfall and other jobs keep running.
8. With `ffmpeg` moved away: the services are listed, greyed, with `ffmpeg is not installed`;
   the multiplex job is unaffected.
9. Thirty minutes of one service: no growth in the node's memory, `dropped_s` zero throughout,
   and sound and lips still together at the end.
10. (Part J) Record five minutes of a service, download the file, and play it in a desktop
    player; replay it on the node and watch it in the Video tab marked as a replay.

## 8. What changes outside the node

- **`onair-rx` 0.5.0** (its own repository, GPL): `--ts-port` for `dvbt` with the queue, the
  flags and the marks of 5.1; `--width HZ` for `dvbt` as a hint; the generator scene of section
  6. The spike's `--ts-out FILE` is dropped. `handlers/onair.json`'s DVB-T entry gains
  `transport` and `--width $width_hz`; a 0.4.0 wrapper fails that job at launch with its own
  message, as every too-old wrapper does, so the entry's change ships with a note in `RECIPES.md`.
- **`dvbt-rx`** needs nothing for the first plan: OnAir is the handler with a transport stream,
  and `needs: ["ts"]` chooses it. A `dvbt-rx` 0.2.0 with the full receiver (it stops at
  signalling today) is part M, and is what makes "several handlers" true for pictures.
- **`ffmpeg`** is named in the install notes with what the probe wants of it.

## 9. Parts, sizes and order

Sizes are the house ones (XS about 50 k tokens, S about 0.4 M all in, M about 0.8 M). Each part
is working software.

| Part | What | Size | Needs |
| --- | --- | --- | --- |
| A | `onair-rx` 0.5.0: `--ts-port`, bounded queue, flags, marks, `--width` | S | (wrapper repository) |
| B | `transport` on a program block, `$ts_port`, `$width_hz`, the `ts` stream kind, the reader and the quality event, `ts` in handler outputs and `needs`; the fixture and the stand-in receiver | M | - |
| C | The node's table reader and the `services` event | S | B |
| D | The `ffmpeg` probe and decoder file, `av_decode`, `VideoFormat::jpeg` through the websocket, `job_attach service` with `ts_service`, sound into the mix | M | B, C |
| E | The Video tab takes any `video` job (XS, built with D) | XS | D |
| F | The steps of 5.5: measurement, stepping, the words | S | D |
| G | The page: the picker, Watch, the read-out and the dimmed picture, the mix's per-job mute, `video_strip`; the UI scenarios | M | D, F |
| H | `transcribe` on a service job; `content: broadcast`; prose, recipes, the acceptance list | S | D |
| I | The generated-IQ test and the generator scene | S | A, D |
| J | Recording (`record`, the `ts` store, the download in pieces) and replay | S + S | B (replay: D) |
| K | Subtitles as text | S | D |
| L | Now and next from the guide | S | C, G |
| M | `dvbt-rx` 0.2.0 with the full receiver and `--ts-port` | S | B (wrapper repository) |
| N | The browser's own decoder for a native picture (approach A) | M | D, a clean multiplex |
| O | DVB-S and DVB-T2 entries declaring `transport` | XS each | B, their receivers |

- **The first useful slice is A, B, D with C's list reduced to ids, and E**: the operator starts
  a service by id from the CLI, watches it in the Video tab and hears it in the mix. About 2.1 M.
- **The first plan is A to I**: about 4.5 M of parts and about 0.8 M for the whole-branch review
  and its fixes, **5.3 M** in all, two repositories.
- J, K and L are a second plan of about 2.0 M with its review. M, N and O are each their own
  decision.

## 10. Left to the builder

- The byte layout of a mark, and whether `time` also carries the receiver's own seconds.
- The frame size of the `ts` stream against the bus's 256 frames at the highest multiplex rate,
  and whether the reader's publish is by size or by time.
- How pictures and samples out of the decoder are tied to packets in, and the exact delay rule
  on the page; the fixture's flash and beep are the test.
- Whether the decoder's second output is a port or the node gives one child each to picture and
  sound (then sound survives a picture decoder that dies, at the price of two clocks).
- The JPEG quality and the default width; 640 wide at quality 5 is what was measured.
- The decoder file's schema and how much of a handler file's validation it shares.
- How `thumb` is made from a `jpeg` frame without a decode a picture.
- The mix's mute key change and its effect on stored mutes.
- The thresholds of 5.5 (three seconds, a minute, ten minutes), set by trying.

## 11. Open questions for the operator

1. **Decode on the node, at a reduced size (recommended), or the browser's own decoder first?**
   Designed to: the node decodes, 640 wide. The alternative gives a full-resolution picture for
   2 % of a core, and loses the picture on a 1 % damaged stream, the mix, mute and the
   transcriber for that sound, and needs the page's content policy opened to `blob:` media. It
   would swap part D's decoder for a remuxer and part G's canvas for a `<video>` element, and
   make part N the first plan.
2. **The default picture: 640 wide at 25 a second (recommended), or smaller?** It is 5 to
   8 Mbit/s to each page showing it and 0.42 of a core for an HD service. Half the rate halves
   the bit rate and saves little CPU; 360 wide halves the bit rate too. A remote page on a slow
   link would want the smaller one; it could also be a view setting per page later.
3. **A click on another service switches (recommended), or adds?** Designed to: a click switches
   the service this page started, a modifier-click adds. The alternative never stops anything
   without an explicit stop, at the price of decoders left running.
4. **Sound mixed down to mono (recommended, forced by the mix today), or wait for a stereo
   mix?** Waiting would put this behind `TODO.md`'s stereo item.
5. **Recording in the second plan (recommended), or the first?** It is the cheapest part with a
   use of its own (a file a desktop player opens at full quality), and could come before the
   decoder if a full-quality picture off the node matters more than a picture on the page.
6. **`content: broadcast` as a new word (recommended), or no `content` on the row?** The word
   lets a later policy tell receiving from relaying; without it the row is unmarked like the
   aviation and maritime rows.
7. **The fixture: a committed synthetic `.ts` of a few hundred kilobytes (recommended), or
   generated on every test run?** Generating needs `ffmpeg` wherever the tests run, CI included.
8. **`ffmpeg` on the CI runner?** Recommended: installed there, so the decoder tests and two
   smoke scenarios run on a push; otherwise they are local gates only and print their skip.
9. **Is the mark format (null packets with a tag) acceptable in a stream a recording keeps?**
   Recommended: yes, every reader ignores null packets. The alternative is marks on the event
   pipe, which cannot be ordered against the bytes.
10. **`dvbt-rx` 0.2.0 (part M): wanted, or is OnAir enough for pictures?** Without it the DVB-T
    row has one handler that can show a picture.
