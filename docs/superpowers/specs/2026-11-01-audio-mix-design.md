# Plan 22: listening to many channels at once

Design spec, 2026-11-01. Brainstormed with the operator; a reviewer read it as the builder before
hand-over. Builds on plan 21 (`plan_start`, `jobs_stop`, `Stage::readiness`, the agent gate), the
attached jobs (`crates/zeta-core/src/stream_in.rs`) and the page's player
(`crates/zeta-ui/web/audio.js`).

## 1. Intent and why

UHF CB activity is sporadic. With 80 channels running, by the time the operator reaches for the job
list, finds the job and presses play, the transmission is over. In the operator's words: "all 80
channels should mix outputs and as long as there is one active audio then the client will hear
something."

Each `fm_demod` already publishes nothing while its squelch is closed, so summing the demodulators
gives silence until someone keys up, and that voice when they do.

The operator's rulings: transcription stays per channel by default ("transcribing 3 concurrent TX
from one audio chain would make a mess"), but a transcript can still be attached to the mix output
when someone asks for it ("it just isnt the default"). One mix can take audio from any number of
tuners. Specific channels can be muted in it.

## 2. Goals, non-goals, anti-goals

### Goals

- One action starts a mix of every demodulator on every tuner (or a chosen set) and plays it on the
  page. Demodulators
  started or stopped later join and leave by themselves, within a second.
- While a channel is heard, its job row and waterfall outline light up, and the mix's row names it.
  The last channel heard stays named after it ends, because the operator looks up late.
- A channel that will not shut up (a data channel, a squelch left open) can be muted in the mix
  without stopping its demodulator or its transcript.
- Transcription happens per demodulator. Starting a mix attaches no transcript to it. As an option,
  the operator can also attach a `transcribe` to the mix's output, alongside the per-demodulator ones.
- It costs one websocket for audio and one for events, however many channels are mixed.

### Non-goals

- Stereo placement, per-channel gain, level matching or AGC, solo, ducking.
- Recording the mix.
- Removing the page's limit of 4 watched jobs for other purposes.

### Anti-goals

- A mix that takes a lease, retunes a radio or blocks a job.
- Silence that lies: a muted or dropped input must be visible in `job_context`.

## 3. Constraints

- Every voice demodulator and `wfm_demod` outputs 48 kHz mono f32 (`Payload::Audio`). A closed
  squelch publishes nothing, so input timestamps have gaps and are not a clock.
- `run_stage` (zeta-dsp) reads only `inputs[0]`, and a graph's inputs are fixed at launch, while the
  mix's set changes as jobs come and go. It needs its own loop.
- A job's graph has inner audio streams too (a `transcribe` job's `stream_in` to `denoise` to `vad`
  edges are `audio`). Only each demodulator's output counts: what `Node::job_audio_stream` names.
- All streams of a job share one provenance, and a stage may publish frames with its own.
- Audio to the page is `GET /stream/{id}?format=binary`, credit-based; `MAX_WATCHED = 4` applies to
  the page's per-job event watchers, not to this.
- Every op field is required, unknown fields rejected, as for every op. Guide regenerated.

## 4. The design

### 4.1 Operation

`{"op": "mix_start", "sources": null}`, CLI `zeta mix start [--sources ID[,ID]]`. `sources` is `null`
for every source of the node, else a list of ids from `source_list`. It answers the job id. It is
idempotent: with a mix already running on the same set it answers that job's id rather than
`duplicate`, so "listen to everything" pressed twice is harmless. A different set is another mix. An
unknown source is `unknown`.

The job is a new `JobKind::Mix`, name `mix`:

- It takes no lease and never tunes. `source_list.holders` never lists it. A record needs one
  `source` and `tuning`, and a mix has neither: they are `node` (as the `ops` stream's provenance) and
  `0`/`0`, and every path that reads a record's source or band must take that (`source_free`, capture
  restore, resume, fault recovery, `jobs_stop --source`, the page's per-source lists and outlines).
  `jobs_stop` with a `source` filter therefore leaves a mix alone; with none it stops it.
- `job_start` with `kind: mix` is refused, naming `mix_start`, as `transcribe` names `job_attach`.
  `job_retune` refuses it by kind. `job_attach` of `transcribe` onto it works as on any demodulator:
  the attached job follows the mix through a mute's restart and ends with it. Its `source` and tuning
  are the mix's, so a transcript line names no channel, and `transcripts.log` shows `node` and 0 Hz.
- It runs until stopped. Its inputs ending does not end it. `job_stop` and `jobs_stop` stop it, and
  it counts in `jobs_stop` like any job. A node restart resumes it, after the jobs it mixes.
- It has no port and no IQ forwarder, so fault handling ignores it: on a source fault it falls silent
  and picks the jobs up when they are relaunched.
- A stopped or failed mix is history: `mix_start` then makes a new job.

Its graph is one node of stage kind `mix`, params `{"sources": null | [ids], "muted": [{"source",
"center_hz"}]}` (`muted` default `[]`), with outputs
`audio` and `event`.

### 4.2 The `mix` stage

**Inputs.** Every 500 ms the stage asks the node for the audio outputs of the running jobs on its
sources and subscribes to those it does not hold. It drops those gone. Attached jobs, owned jobs
(`owner` set: a `watch`'s or `identify`'s trials) and other `mix` jobs are excluded. Any other job
with an audio output counts, an operator's `graph_put` graph included. Getting this list needs a new
hook on `StageContext`, a function from nothing to a list of `{job, stream, source, center_hz}`, built on
`Node::job_audio_stream`; a test may stand in for it. Each poll refreshes `center_hz`, so a retune
shows. The same job under a new stream id (a retune, a `graph_put`, a recovery) stays the same input:
its receiver is swapped, its queue and `active` state kept, so `active` does not flicker. An input with another rate or more than one channel is skipped once with a line in
`job_context.mix.skipped`.

**Clock.** A 20 ms wall-clock tick. Each input has a queue. Every tick takes up to 960 samples from
each non-muted queue, sums them and publishes one frame only if any input had audio. A short input
contributes what it has. So a gap publishes nothing, and the page's player re-anchors as it does for
a squelched demodulator today.

**Latency and drift.** A queue starts feeding after a short delay from its first samples of a burst,
to ride out block-sized delivery: 40 ms is a starting value, to be set from what the real chain
delivers, and frames may be larger than 20 ms if that needs it. The requirement is that a continuous
transmission plays without gaps (tested on the real chain, section 5). A queue longer than 200 ms loses its oldest samples, counted per job in
`job_context.mix.dropped_ms`, so a fast radio clock cannot build delay. The bus's `Lagged` is counted
with `note_lag("mix", n)`.

**Level.** The sum passes through unity gain while it is inside -1..1, then a soft limit (`tanh`-style
above 0.8), so one voice is untouched and a collision does not clip. There is no normalisation.

**Who is talking.** An input is `active` from its first queued samples until 500 ms after its last.
The event output publishes on each change:

```json
{"protocol": "mix", "active": [{"job": "<id>", "source": "b210", "center_hz": 476625000}], "t_ns": 0,
 "summary": "476.625000 MHz"}
```

A muted job is never active. `job_context.mix` holds `inputs` (count), `active`, `last` (`job`, `source`,
`center_hz`, `t_ns` of the last job heard), `muted`, `dropped_ms` and `skipped`.

**Restarts.** `job_context.mix` starts empty after a restart of the mix, a mute included. The page
keeps its own last-heard, so a mute does not lose it on screen; after a node restart it is gone, which
is accepted.

**Frames.** Output frames carry the mix job's provenance. The span is the tick's wall-clock span.

### 4.3 Muting

A mute is a channel, not a job: `{source, center_hz}`, matching a job on that source whose centre is
within `CHANNEL_MATCH_HZ` (1 kHz). So ch22 stays muted when its demodulator is stopped and started
again, and a fit's small move keeps it; a job retuned elsewhere is no longer muted. `muted` is set with
`graph_put`, like the demodulator's squelch params. It restarts the mix under its id; the page's player
reconnects as it does after any restart.

### 4.4 The page

- A **Listen to all** button beside the source tabs, and a typed `mix [ID,ID]`: `mix_start` with
  `sources: null` (or those ids), then the existing `playJob` on that job. Pressing it again stops playback. The mix job
  stays running until stopped from its row, since it costs little while silent; the next press
  reuses it.
- `mix` joins `AUDIO_KINDS`, so its row has the ordinary play button. Only one player plays at a
  time, as today: playing one channel stops the mix's playback, and the reverse.
- While a mix is listed, the page holds one event socket on it, outside `MAX_WATCHED`, playing or
  not; with several, on the one playing, else the newest. A mix's row is listed whichever source is
  shown and has no outline.
- A mix's row offers Transcribe like any audio job, as an ordinary button with no default on.
- Each job in `active` gets the class `speaking` on its row and outline (a new class, distinct from
  hover's `hot`), removed when it leaves `active`.
- The mix's row shows the speaker and the last heard, by plan channel when it has one: `ch9 now`, or
  `last ch9 12 s ago`, from the same channel lookup the outlines use.
- A demodulator's outline menu and row gain **Mute in mix** / **Unmute in mix** when a mix covers
  its source: `graph_get`, edit `muted`, `graph_put`.

### 4.5 Guide and agent

The op section, plus field craft "Listening to many channels": `mix_start`, what `active` means, how
to mute, that a transcript normally comes from `job_attach` on each channel, and that one on the mix
runs overlapping voices together and names no channel, so attach it there only when asked. No rule is added
to the gate: starting a mix stops nothing. Muting through the agent is a `graph_put`, which is
ungated (see the TODO gist).

## 5. Testing

Stage tests, driving the 20 ms clock with tokio's paused time and a stand-in input list:

- One input's samples come out unchanged, exactly once, in order. A silent period publishes nothing.
- Two inputs sum, and a collision over full scale stays within -1..1. A value under 0.8 is untouched.
- A new input joins and a removed one leaves without a restart; `active` events fire on each change,
  with the 500 ms hangover. A muted input is not heard and never `active`.
- A queue over 200 ms drops its oldest and counts it. A 44.1 kHz or stereo input is skipped and
  listed.

Node tests over the `Radio` fixture of `crates/zeta-core/tests/plan_start.rs`:

- `mix_start` takes no lease and `holders` stays empty on every source; a rigid job's tune is not
  refused by it. A second `mix_start` with the same set answers the same id, another set a new one.
- Two radios: jobs on both are inputs with `sources: null`, only the named one with a list.
- `muted` holds through stopping and restarting a channel's job, and a job retuned off it is heard.
- `source_free`, a restart's capture restore and a fault's recovery ignore a mix's `node` source.
- A `transcribe` attached to a demodulator is not an input: its inner audio streams are ignored.
- `job_start` kind `mix` and `job_retune` are refused with their texts. `job_attach` of a stand-in
  `transcribe` onto it runs, relaunches through a `graph_put` and ends with the mix.
- `jobs_stop` stops it. Restart resumes it with no lease and picks its jobs up.

Real chain (`crates/zeta/tests`): two `fm_demod`s on recorded tones in one capture, squelch on: the
mix's audio contains both tones when both are open, and a continuous tone has no gaps in its frames'
spans.

Page (`ui.py`): Listen to all starts one mix and plays (`window.zetaAudio` counts frames); pressing
twice reuses the job; the speaking row gains `speaking` and the mix's row names it; Mute in mix puts
the channel in the graph and the row stops lighting.

## 6. Acceptance on the field host

1. With all 80 CB channels running on the B210 under the pin, Listen to all: CB traffic is heard
   with no job selected, and the channel's row and outline light up as it talks.
2. Two channels at once: both are heard, without clipping.
3. After a transmission, the mix's row still names the last channel.
4. `source list`'s `bank.dropped` and `measured_rate_hz` do not change when the mix starts.
5. With demodulators on both radios, both are heard through one mix.
6. Each channel's transcripts keep arriving while the mix plays.

## 7. Left to the builder

- How the stage holds many receivers (forwarding tasks into per-input queues, or a select loop).
- The exact limiter curve above 0.8, and the row's wording within what 4.4 gives.
- Where the leaseless launch rule lives (today only `input.is_some()` gets it).

## 8. Calls made in this document (check these while reading)

- A record's `source` is the placeholder `node` for a mix, instead of making `source` optional across
  every record and reader. A real source named `node` would be confused with it: the flags refuse the
  name.
- A mute is `{source, center_hz}` rather than a job id, so it outlives the job.
- `mix_start` is a new op rather than `job_attach`, whose input is one job.
- Muting goes through `graph_put` to avoid a new op, at the cost of an audio blip per mute.
- Stopping playback leaves the mix job running, so the row and its last-heard stay.
- `wfm_demod`, `am_demod` and `ssb_demod` are mixed as well as `fm_demod`, since all output 48 kHz
  mono. `wfm_demod` and `ssb_demod` have no squelch and would hold the mix open until muted.
