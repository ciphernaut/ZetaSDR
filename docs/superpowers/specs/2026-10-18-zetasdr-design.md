# ZetaSDR design

Date: 2026-10-18. Status: draft for builder review.

ZetaSDR is an agentic RF runtime: a "tricorder" that owns one or more
signal sources (a HackRF on a laptop, a Pi at the antenna, a USRP or
LimeSDR on a bench, a custom FPGA front end, a KiwiSDR or OpenWebRX
receiver on the far side of the planet), keeps a live picture of the
spectrum, and exposes "what is there, what is it, decode it" as typed
operations that a human, a context menu, a voice command, or an LLM of the
user's choosing can invoke.

It is the second-last SDR anyone might need. The last one will have a very
different UX.

## 1. Intent and why

Today, going from "there is a blob on the waterfall" to "it is decoded and
I know what it is" means hopping between SDR apps, decoders, notebooks and
manual protocol reverse engineering. Each tool owns its own hardware,
UI and assumptions. None of them can be driven by an agent without
clicking for it.

ZetaSDR's job is to make the whole loop (detect, classify, isolate, decode,
overlay, remember) a set of composable operations on a runtime that does
not care where the samples came from, where the heavy maths runs, or which
model is reasoning about it.

The user's own words for the stories it must serve:

- "play every FM broadcast channel at the same time"
- click a frequency, right-click, "what is this?"
- "can you hear any weather balloons right now"
- "set up a background decoder for all ACARS and ADS-B activity"
- "decode all frequencies between 400 and 430 MHz and overlay the text"
- a right-to-left scrolling waterfall with voice-only-filtered
  speech-to-text overlaid on the channels it came from
- "play any drone footage"
- driven directly through context menus, or by voice, or by an LLM

## 2. Goals, non-goals, anti-goals

### Goals

1. One operation set, many callers: GUI, CLI, voice, MCP, in-process agent.
2. Sources of radically different shape look identical above the source
   boundary: local USB, remote node, remote third-party receiver, file.
3. Long-lived jobs that survive the UI closing, carry context, and are
   addressable later.
4. Typed output streams (IQ, audio, video, text, events) that any
   consumer can subscribe to and that the UI can pin onto the waterfall.
5. Knowledge (band plans, licences, signal signatures) is data from
   pluggable providers, never code in the core.
6. Three classification layers (see section 6), with layer 3 driven by an
   agent composing vetted DSP blocks.
7. Acceleration is opportunistic: a stage can declare CPU, GPU or FPGA
   implementations and the scheduler picks.
8. Runs on a Raspberry Pi at the antenna, a laptop, and a GPU/FPGA
   workstation from one codebase; nodes federate.

### Non-goals (for now)

- Training a SIGINT foundation model. That is its own project. ZetaSDR
  leaves a slot for it and should emit, as a by-product, the data shapes
  such a model would need (section 6.4).
- Hosting the stack on a PortaPack or phone. Those are sources and thin
  clients.
- Transmit. Nothing in this document precludes it, nothing requires it.
- A node-graph editor as the primary authoring surface. The graph is a
  document; a visual editor is one optional way to write it.

### Anti-goals (what would make v1 a failure even if it worked)

- **A privileged SDR backend.** If the core imports SoapySDR, GNU Radio,
  or any driver library, the design has failed. Every source is a plugin.
  The test suite must pass with a file source and no hardware.
- **A privileged model vendor.** The core speaks one generic
  chat-plus-tools client. It must run, fully, with no model attached.
- **Foreign frameworks locked out.** A Python classifier, a C++ decoder,
  an OpenWebRX instance, someone's Julia experiment: each must be
  integrable as an external process speaking the typed streams over a
  socket, without touching the core. The in-process plugin API is the fast
  path, never the only path. The architecture's acceptance test is
  "integrate something weird without changing core".
- **Hidden assumptions that force a rewrite**: one source, bandwidth that
  fits USB, audio as the only output, a local user, a particular
  classifier, a particular LLM.

## 3. Constraints

- **Language**: Rust for the core (source abstraction, job runtime,
  operation set, stream types, scheduler, transports). Decided by the user.
- **Reuse policy**: existing code is used in one of three ways, chosen per
  piece: run as-is as an external process or service; linked natively
  behind a Rust boundary; or ported when the piece is small and on the hot
  path. Nothing in the core depends on GNU Radio being installed.
- **Footprints**: Raspberry Pi (ARM, no GPU, thermally constrained) is the
  smallest host. The first-slice Pi is a Pi 5 on WiFi (user's answer), so
  the Pi-to-laptop link is tens of megabits at best: the Pi channelises
  and ships narrowband; wideband IQ never crosses WiFi in the slice. A RISC-V board running a 4B-parameter model behind an
  OpenAI-compatible endpoint is the smallest *agent* host. A 4090-class
  workstation and FPGA hosts are the top.
- **LLM wire**: OpenAI-compatible chat completions with tools is the
  lowest common denominator (it is what the user's local model servers on
  `orangepirv2` and `field-host` expose, both behind LiteLLM). Anthropic and
  other vendors are reached through LiteLLM or a thin adapter. The runtime
  probes model capabilities (tools, audio, vision, "decide" parallel
  decision mode) instead of assuming them.
- **Tiny-model friendliness**: the operation vocabulary must be driveable
  by a 4B model: strict JSON schemas, no synonyms, mandatory units, small
  tool set with tool search, errors that name the bad field.
- **Builder discipline**: the project is built with the ponytail skill's
  minimalism ladder (reuse before stdlib before dependency before new
  code). This matters doubly for the future "agent writes FPGA stages"
  loop.
- **Licensing**: permissive for the core (MIT or Apache-2.0, builder's
  call). GPL components (GNU Radio, many decoders) are isolated behind the
  process boundary or as optional plugins so the core stays permissive.
- **Trust boundary**: LAN only for now (user's answer). v1 uses a shared
  secret between nodes and callers on every transport, no per-caller
  rights. The wire is framed so that authentication and authorisation can
  be added without changing operation shapes, but they are not built.
- Existing assets to build on, all under `/Projects`: `acma-local` (an
  MCP server over the Australian ACMA licence register, TypeScript),
  `local-voice` (Parakeet speech-to-text daemon on a Unix socket, Piper
  TTS), `MeshCore` (LoRa mesh firmware; a layer-2 signature target),
  `rs41` (radiosonde firmware; a decode target), `fpga-host`, and the
  survey workspace `ham` with its CAPABILITIES.md and CLASSIFIER.md
  comparisons of 21 upstream projects.

## 4. Architecture

```
                    callers
  GUI (wide / narrow) · CLI · voice · MCP server · in-process agent
          \              |            |           /
           \_____________|____________|__________/
                         |
               operation set (typed, versioned)
                         |
   +---------------------+----------------------+
   |              ZetaSDR node (Rust)            |
   |  sources  →  jobs/graphs  →  streams        |
   |  scheduler (CPU/GPU/FPGA, thermal aware)    |
   |  knowledge providers · classifier plugins   |
   |  federation (this node ↔ other nodes)       |
   +---------------------+----------------------+
                         |
         plugins (in-process) · external processes (socket)
```

### 4.1 Sources

A source is anything that yields samples or pre-channelised data.

- Declares a **capability record**: tuning range, instantaneous
  bandwidth, sample formats, channel count, gain controls, whether it is
  local or remote, push or pull, and which of those are *asserted* by the
  driver versus *measured* by the runtime (the same distinction the user's
  model cards and the ham survey's "Claimed" convention make).
- **Push and pull both supported**. A HackRF is pull (the runtime reads
  it). A KiwiSDR websocket or a remote node is push (samples arrive). The
  boundary presents both as a stream.
- **Remote is the normal case.** A HackRF on the Pi node is reached the
  same way a KiwiSDR is. Local USB is the special case that happens to
  have low latency.
- **Arbitration (author's call)**: a physical source is held under a
  *lease* by the jobs using it. `source.ensure` succeeds if the request
  fits the current tuning and bandwidth, and otherwise fails with an
  error that names the holding jobs and the options (stop them, widen the
  capture if the hardware can, or run a `scan` job that time-shares).
  There is no silent pre-emption and no last-writer-wins; a background
  decoder is never knocked off by a right-click. Time-sharing is an
  explicit job kind, not a hidden policy.
- **Sample history (author's call)**: every source has a raw IQ ring
  buffer, sized by configuration (seconds in RAM on the Pi, minutes on
  disk on a laptop). A *selection* is a time-frequency rectangle, so
  "what was that burst five minutes ago", right-clicking something that
  has scrolled past, `classify(selection)` and "save as SigMF" all read
  from it. Feature records and events are kept longer than samples.
- v1 sources (the builder may order them): file (SigMF), HackRF via
  SoapySDR plugin, KiwiSDR websocket client, another ZetaSDR node. Later:
  OpenWebRX client, USRP and LimeSDR via SoapySDR, FPGA wideband stream,
  PortaPack over USB.

### 4.2 Jobs and graphs

- A **graph** is a plain serialised document (JSON or similar) describing
  stages and what feeds what, with typed parameters. It is what the LLM
  reads and writes, what the GUI renders, and what a high-level operation
  such as "background ACARS decoder" expands into. Clicking nodes together
  is one optional way to author it (the SDR-- workflow layout is the
  inspiration; ComfyUI is the warning about trying to *drive* such a
  thing through its UI).
- A **job** is a running graph with identity, lifecycle (create, start,
  pause, stop, resume), and **context**: what it was asked, what it has
  seen, what it decided, so that "what was that burst five minutes ago"
  and an agent reconnecting later both work. Jobs survive the UI closing
  and are listed and addressable.
- Jobs compose: scan, detect, classify, demod, decode, transcribe, overlay.
- **Durability (author's call)**: job definitions, ids and context are
  persisted. On node restart, jobs auto-resume best-effort with the same
  ids; a peer sees a restarted node's jobs come back rather than vanish.
  Stream contents are not persisted beyond the ring buffer.

### 4.3 Streams

Typed, subscribable, with provenance (which source, which job, which
channel, which time span):

- IQ (with sample rate, centre, format)
- audio (demodulated)
- video (demodulated analog FPV; digital links such as DJI are reported
  honestly as not decodable rather than attempted)
- text (decoded messages, speech-to-text)
- events (structured: an ADS-B position, an ACARS message, a
  classification result, a job state change)

Any stream can be overlaid on the waterfall pinned to its channel, or
shipped out of the node. Add-on applications that do one thing very well
(OpenWebRX is the user's example) consume streams rather than being
rewritten.

### 4.4 Knowledge providers

A provider interface answering "what should be at this frequency in this
region" and "what does signal type X look like".

- **Band plans and licences**: `acma-local` is the first implementation
  (Australia). Others plug in FCC, Ofcom, or a hand-written file.
- **Signal signatures**: a second provider type, data not code: band,
  bandwidth, modulation, symbol rate, preamble or sync pattern, and which
  decoder plugin proves it. Seeded from the classifier survey; grown by
  layer-3 successes (section 6.3).

### 4.5 Classifier plugins

Flat contract: input is IQ (primary) or a spectrogram (secondary), plus
the layer-1 feature record; output is labels with confidence. Audio is
**not** a classifier input; it is a post-demod content stage (voice,
Morse). Implementations are swappable as the research crop changes.

### 4.6 Acceleration and scheduling

- A stage may register more than one implementation (CPU, GPU, FPGA) with
  identical semantics. The scheduler picks by availability and load.
- The scheduler consumes host telemetry (CPU thermal zones, throttle
  state, GPU memory) and sheds or moves load. The Pi tier depends on this.
- Ladder, in order of expected arrival: LLM on GPU; FFT and channeliser on
  GPU; FPGA stages; finally an agent that writes, deploys and registers an
  FPGA stage itself. The stage interface must not preclude the last step.

### 4.7 Federation

Nodes discover and talk to each other. A laptop's job runtime can use the
Pi node's HackRF as a source and run heavy stages where the hardware is.
The wire between nodes carries the same typed streams and operation set.

Data plane (author's call): a node ships narrowband, channelised streams
by default; shipping wideband IQ across the wire is allowed but explicit,
with flow control and back-pressure in the federation protocol from v1,
because a Pi Zero on WiFi and a Pi 5 on Ethernet differ by two orders of
magnitude. Layer 1 runs where the samples are, so the Pi does its own
cheap layer 1 on the channels it ships.

### 4.8 Plugin and process boundaries

- **In-process plugins** (Rust trait objects, dynamically loaded or
  compiled in): the fast path for sources, stages, classifiers, knowledge.
- **External processes**: anything else, speaking the typed streams and a
  subset of the operation set over a socket, with a small SDK in Python
  at minimum. GNU Radio flowgraphs, Python classifiers, GPL decoders and
  OpenWebRX all live here. GNU Radio is one stage host among several.

## 5. Control: the operation set and its transports

### 5.1 The vocabulary

The operation set is the product. It must be small, typed, versioned and
unambiguous:

- one name per operation, no synonyms
- every quantity carries its unit in the field name (`center_hz`,
  `bandwidth_hz`, `symbol_rate_baud`), integers where possible
- strict schemas (`additionalProperties: false`, all fields required,
  enums for kinds)
- errors name the field and the acceptable range
- resources (sources, jobs, streams, channels) are addressable by stable
  ids and can be listed
- high-level operations exist for the common stories ("start a
  background decoder of kind K on source S") so a small model does not
  have to compose a graph by hand; the graph is still what gets created

Illustrative, not final:

```
source.list / source.ensure(id, center_hz, bandwidth_hz)
spectrum.snapshot(source, span)            → peaks with L1 features
job.start(kind, source, center_hz, …) / job.list / job.stop / job.context
classify(selection)                        → L1 + L2 labels
decode(selection, protocol)
knowledge.lookup(region, band | signature)
stream.subscribe(id) / overlay.pin(stream, channel)
graph.get / graph.put(document)
```

### 5.2 Transports (thin adapters over 5.1, several shipped together)

- **CLI**: every operation as a command. Lets any bash-capable agent,
  script or skill drive the node.
- **MCP server**: tools for operations, resources for live state
  (spectrum snapshot, job list, recent events). Lets Claude Code, Claude
  Desktop, any MCP-capable harness and the Claude API's MCP connector
  drive it with no bespoke integration.
- **HTTP/websocket + JSON**: the GUI, voice front end and remote nodes.
- **In-process agent loop**: an optional adapter that runs a small local
  model (OpenAI-compatible endpoint) against the same operation set, for
  the Pi tier and offline use. The runtime never calls a model from
  anywhere else.
- ACP / A2A are deferred until there is a second agent to talk to.

### 5.3 Companion skill

A Claude Code skill (and equivalents for other harnesses) teaches an
agent the vocabulary plus the field craft: where radiosondes live, what a
POCSAG preamble looks like, how to work layer 3. The vocabulary makes
commands precise; the skill makes the agent competent.

### 5.4 Voice

Voice is the command line with a microphone. v1 uses the existing
`local-voice` Parakeet daemon for speech-to-text; where the connected
model is audio-native, the audio may go straight to it instead.

## 6. Classification layers

### 6.1 Layer 1: modulation

What kind of signal: NBFM 12.5 kHz, SSB, 2-FSK at 9600 baud, QPSK,
LoRa chirp. Classical measurement (bandwidth, cyclostationary symbol rate,
constellation, burst timing) plus a conventional trained classifier (for
example one trained with TorchSig on synthetic data). Output is a
**feature record** with numbers, not just a label, so layers 2 and 3 and
the LLM have something to reason over. Cheap, runs on the Pi, always on
for anything above the noise floor.

### 6.2 Layer 2: known signal types

Given the feature record, match against the signature library: MeshCore,
LoRa, POCSAG, ADS-B, ACARS, RS41 and other radiosondes, DMR, APRS,
broadcast FM, and so on. Two kinds of evidence, both reported:

- declarative signature match (band, bandwidth, modulation, rate, preamble)
- **decode-to-identify**: run the decoder plugin and see whether its CRC
  or framing passes. Exact, cheap, zero training. For digital protocols
  this is the most reliable classifier that exists today.

### 6.3 Layer 3: the agentic loop

For anything layer 2 cannot name, an agent drives a job with a toolbox of
**vetted DSP blocks with typed parameters** (filter, resample, clock
recovery, symbol slicer, correlator, descrambler, bit viewer) and
iterates: isolate the channel, recover symbols, inspect bits, hypothesise
framing, attempt a decode, look again. Context is retained so it can
resume. A successful outcome is written back as a new layer-2 signature.
That write-back is the project's real flywheel.

Arbitrary user- or agent-written flowgraph code is **not** allowed in v1.
It arrives later behind an explicit sandbox flag.

### 6.4 The parked SIGINT model

No usable RF-native foundation model exists for the bands of interest as
of this date. Verified: LWM-Spectro (weights released, spectrogram input,
knows LTE/WiFi/5G only); EMind (1.35 GB checkpoint, undocumented input,
no licence); cvnet-rf (RadioML toy). Paper-only: IQFM, Radio-FM,
SpectrumFM, RF-GPT, RFPrompt, WirelessJEPA. Layer 1 and 2 contracts leave
a slot. ZetaSDR should be able to export labelled SigMF captures with
feature records and layer-2 outcomes, which is the data shape that
project will want. Nothing more is in scope here.

## 7. The user interface

One runtime state, two renderers, chosen by footprint (user's decision):

- **Wide** (laptop, desktop): right-to-left scrolling waterfall (newest at
  the right edge, frequency vertical) with source tabs across the top;
  layer-1 and layer-2 labels on each detected channel with confidence;
  decoded text and speech-to-text as ribbons pinned to their channel and
  scrolling with it; a side pane with the agent transcript, its tool
  calls shown verbatim in the vocabulary, and the background job list
  with live counters; a command line with microphone at the bottom.
- **Narrow** (phone, thin client beside a Pi): overlay-only, agent replies
  appear as channel-pinned ribbons, collapsible transcript, maximum
  spectrum.
- **Context menu** on a selection: "What is this?", decode, start
  background job, save as SigMF, send to layer-3 agent. It emits the same
  operations the agent does.

The UI is a client of the node's websocket; nothing is "desktop only" by
accident. Mockup: `/Projects/ham/.superpowers/brainstorm/*/content/waterfall-layout.html`.

## 8. The first vertical slice

Because the deliverable is the set of boundaries, version one is not the
easiest story done well. It is the thinnest slice that crosses every
boundary once, each stage deliberately boring:

1. Two sources at once: a local HackRF and a remote KiwiSDR (or a second
   ZetaSDR node with a file source if no Kiwi is reachable).
2. One channelised job producing two output types: an NBFM audio stream
   and a decoded event stream (POCSAG or APRS; builder's call, APRS
   suggested because MeshCore and RS41 come later).
3. Layer 1 on everything above the noise floor; layer 2 via
   decode-to-identify for the chosen protocol.
4. One operation invoked from the GUI context menu and the same
   operation invoked by an LLM through MCP, hitting the same code path,
   with the call visible in the transcript pane.
5. A CLI exposing the same operations.
6. One stage with two implementations (CPU FFT and GPU FFT) to prove the
   scheduler boundary, even if the GPU path is trivial.
7. Runs on the Pi (headless, source and cheap stages) and the laptop
   (everything else), federated.
8. Test suite passes with the file source and no hardware.

The GPU implementation in item 6 is accepted on whichever host has a
GPU; the `field-host` 4090 box is the acceptance host if the dev laptop has
none (author's call). If no KiwiSDR is reachable for item 1, use a public
one from the kiwisdr.com directory for the slice (author's call).

Layer 1 in the slice is classical measurement only (author's call). The
trained classifier arrives afterwards as a plugin; the slice exercises the
classifier plugin contract with a stub so the boundary exists.

If this slice works, every later story is "add a plugin", not "move a
wall".

## 9. Left to the builder

- Crate layout, async runtime, serialisation format for graphs and the
  wire, plugin loading mechanism (dynamic vs compiled-in vs both).
- Choice of GUI toolkit for the wide renderer and web stack for the
  narrow one, provided both are clients of the same websocket.
- Which SoapySDR / driver bindings; how the KiwiSDR protocol client is
  implemented.
- Exact v1 protocol for decode-to-identify (APRS vs POCSAG).
- Thermal and load heuristics for the scheduler.
- Licence choice between MIT and Apache-2.0.
- Layer-1 classifier training recipe, provided the plugin contract holds.
- Repository tooling, CI, packaging for the Pi.

## 10. Open items the author is still resolving

- **Floor model for the in-process agent.** A tool-calling probe of the
  four small models on `orangepirv2` (gemma-4-e2b, lfm2.5-8b-a1b,
  smallthinker-4b-a06b, olmoe-1b-7b) is in progress. Its result sets
  how small the vocabulary must be and whether constrained JSON output is
  needed as a fallback to native tool calling. The vocabulary rules in
  5.1 are designed to survive either answer.

## 11. Prior art and why not

- **GNU Radio**: modular and the obvious stage host, but it is a
  flowgraph runtime, not a job runtime with sources, context, knowledge
  and agents. Used as one stage host behind the process boundary.
- **SDR--**: the reconfigurable workflow layout is the right mental model
  for graphs. Its UI is the wrong driving surface for agents.
- **OpenWebRX / KiwiSDR**: remote receivers with their own UIs. Sources
  and add-on consumers, not spines.
- **ka9q-radio**: multicast channel model worth studying for the stream
  layer; its C core and Unix-centric design are not the base.
- **ComfyUI**: the cautionary tale for node graphs driven by clicking.
