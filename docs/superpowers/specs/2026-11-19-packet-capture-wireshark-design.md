# Packet capture output for Wireshark

Design doc. From the operator, 2026-11-19 ("add Wireshark support for lawful and
research/education purposes only"; "expand the capabilities", scope confirmed
"Wireshark output, within boundary"; egress order confirmed "A then B").

TODO gist: `TODO.md`, "Feature: packet capture output for Wireshark".

## Intent and why

A decoder that cuts frames already holds the bytes of each frame, but today they
leave the node only as a JSON event. Wireshark is the standard tool for reading a
protocol frame by frame — the dissectors, the follow-stream, the filter language —
and nothing in the node can feed it. The operator wants to open the node's own
decoded traffic in Wireshark for teaching and research: a live amateur packet
channel (AX.25), the framed bursts the frame rows cut, and later a broadcast
multiplex's tables.

This is one shared output, not a feature per decoder: any job whose decoder
produces frames can give them as a packet capture, each packet carrying its own
time and, in a comment, the job, the source and the frequency it came off.

## Goals

- A capture a job writes that opens in Wireshark and dissects: AX.25 from the APRS
  row first, then the generic frame rows.
- Each packet timed from the frame's own timestamp; the job, source id and centre
  frequency recoverable from the capture.
- Phase A: a **stored** capture, served by id like the node's other artefacts,
  downloaded and opened offline.
- Phase B: a **live** feed into Wireshark on top of A.
- One byte-carrying payload on the bus that both phases, and the later DVB-T
  transport stream, build on.

## Non-goals

- No new decoders. This carries what the node already decodes.
- No transport-stream video path. DVB-T's TS is gated on the service-list step
  (DVB-T gist) and takes a different egress (see "TS is not pcapng" below); this
  design only reserves the payload for it.
- Phase A does not start Wireshark, watch it, or own a socket. It writes a file.

## Anti-goals (what would make this a failure even if it worked)

- Anything that captures traffic the operator did not decode themselves. A capture
  holds exactly what the decoder under it produced. The "research/education"
  purpose on the output decides nothing about scope; the decoder's scope does. The
  DVB-S2 payload boundary in `TODO.md` ("GEO DVB-S2 reception and measurement")
  is unchanged by this work, and the general GSE/MPE decapsulation stage stays
  unbuilt. If the authorised-carrier check in that section is ever built, it writes
  its already-filtered result through this output; it does not widen because this
  output exists.
- A capture of a restricted row (e.g. pager text, `access: restricted`) that is
  not itself restricted. Phase A's rows are not restricted, so this is a guard to
  carry, not a thing to build now (see "Restricted rows").

## Constraints

- **One operation set.** The operator-facing surface is an `Op` applied by
  `Node::apply`; transports only translate (`CLAUDE.md`, Architecture). No new
  side channel.
- **Typed frames over the bus.** A packet is a typed `Payload`, not hex smuggled
  through an event or transcoded at the edge.
- **Generated prose.** `AGENTS.md` and `SKILL.md` are generated; the new op's and
  the `packets` kind's prose goes in `crates/zeta/src/agents_md.rs`, regenerated,
  never hand-edited.
- **Public repo.** No real host names, positions or frequencies in fixtures or
  docs; demo station is Brisbane Airport. `public-check.sh` clean before `main`.
- **A wide/binary artefact must be built release** for real rates (DVB-T lesson);
  irrelevant to the low-rate phase-A rows but true for the reserved TS path.

## Approaches considered

**How the packet reaches an egress:**

1. *Transcode events at the edge.* No core change; a sink reads the existing
   `Event` streams (which already carry `bytes`/`info_hex`) and builds pcapng at the
   CLI/ws boundary. Rejected: the "packet" exists only at the edge, every egress
   re-parses hex from JSON, and it does not serve the DVB-T TS, which is never an
   event. Works against "typed frames over the bus".
2. *First-class `Payload::Packet` on the bus* (**chosen**). The frame stage emits a
   typed packet on a second output, the way `clock_recover` already emits timing
   separately from the sliced bits; the JSON event stays for the page. One typed
   thing feeds every egress and both phases, and it is the byte payload the DVB-T
   gist asked for. Cost: a new `Payload`/`StreamKind` variant touches every
   exhaustive match across the transports.
3. *Hybrid* (byte payload only for high-rate TS, events for low-rate framed rows).
   Rejected: two mechanisms for one job, and it is exactly the per-decoder split the
   operator did not want.

**Operator-facing operation:**

1. *A new standalone `capture packets` op.* Rejected: a second recording verb beside
   the attach model, for the same "add an output to a running job" intent.
2. *A job-start policy (a field on `job_start`).* Rejected: you decide to capture
   after a job is already running and interesting; a start-time flag is the wrong
   moment.
3. *A new attachable `kind` on `job_attach`* (**chosen**). `job_attach` already
   attaches `transcribe` to a running job and records the attachment on the job;
   `packets` is a second kind. Matches the existing mechanism (and the operator's
   "one shared mechanism, not N variants" preference), and `job_detach`/stop already
   exist to end it.

**Phase B transport** (decided in outline, built in phase B, not now):

- *extcap helper* — Wireshark lists the node's jobs as capture interfaces and pipes
  one job's packets in live. Most "native" in Wireshark; needs a small helper binary
  that reads a Packet stream over the existing `stream tail`.
- *GSMTAP over UDP* — the node sends UDP, Wireshark listens; no file format and no
  helper binary. Smallest live path, strong for radio protocols with GSMTAP
  dissectors. Recommended to build first in phase B; extcap second if wanted.

## The design (phase A)

### Payload and stream kind

Add `Payload::Packet { link_type: LinkType, data: Arc<[u8]> }` to
`zeta-core/src/stream.rs`, and `StreamKind::Packet`. `Payload::kind()`, the
`StreamKind` all-list in `stage.rs` (around lines 364–369), `outputs_for`'s default,
and every exhaustive match on `Payload`/`StreamKind` in `zeta-ws`, `zeta-mcp`,
`zeta-ui` and the node are updated. A bus validation (like `VideoFormat::check_body`)
refuses an empty `data`.

`LinkType` is a small enum wrapping the pcap/pcapng **LINKTYPE** number the egress
writes into the interface header:

- APRS / AX.25 → `LINKTYPE_AX25_KISS` (202): Wireshark dissects it as AX.25 and APRS.
- Generic frame rows (`frame_decode`, opaque parser) → `LINKTYPE_USER0` (147): bytes
  visible, no dissector claims them, the operator can bind a user dissector per row.
- Reserved for later: raw IP (`LINKTYPE_RAW`, 101) for the authorised-carrier check's
  output; the DVB-T TS does **not** use this payload's pcapng egress (below).

The `link_type` is set by the stage from the row, not inferred at the egress.

### Emitting the packet

The frame chain (`channel → … → frame_decode`) gains a second output of
`StreamKind::Packet` on the frame stage, registered only when wired (lazy, like other
streams), so a frame job with no packet consumer pays nothing. `frame_decode` already
produces the frame bytes for its event; it wraps the same bytes as a `Packet` with the
row's `link_type`. APRS's `aprs_decode` does the same from its AX.25 frame.

A frame row's `outputs` (derived by `modes/template.rs::outputs`) then lists `packet`
alongside `event`, so `modes_list` shows which rows can be captured. The `outputs`
prose in `agents_md.rs` gains the `packet` kind.

### The attach op and the stored artefact

`job_attach` gains `kind: "packets"`. Attaching to a running frame job registers a
packet sink on the job's Packet stream; the sink opens a **pcapng** file in the state
directory and writes:

- one Section Header Block and one Interface Description Block carrying the
  `link_type` and the time resolution;
- one Enhanced Packet Block per frame, timestamp from the frame's `Timestamp`, with an
  `opt_comment` option holding `job=<id> source=<id> freq_hz=<centre>` (the gist's
  "job, source and frequency in a comment").

pcapng is chosen over classic pcap for the per-packet comment and the typed interface
metadata. The attachment is recorded on the job like a transcribe attachment and ends
on `job_detach` or when the job stops; the finished file becomes an artefact served by
id, following the capture-serving pattern (`ops.rs` `GET /captures/<id>…`), at a
`.pcapng` suffix. It is listed on the page the way captures are.

### Restricted rows

If a captured row is `access: restricted`, its capture artefact is restricted the same
way its events are, and gates behind the same ACL work (`TODO.md`, "Access: ACLs on
restricted rows"). Phase-A rows (APRS, frame rows) are not restricted, so phase A
carries the guard (the artefact inherits the row's `access`) without depending on the
ACL feature; a restricted row cannot be packet-captured until that lands.

### TS is not pcapng (reserved, not built)

The byte payload is shared with the later DVB-T transport stream, but the **egress is
not**: an MPEG-2 TS has no clean pcapng link type (Wireshark reads a TS as its own
file type or inside UDP/RTP, not as pcap link frames). So the TS, when it arrives,
is written as a raw `.ts` artefact (as the DVB-T gist already says), consuming the same
`Payload::Packet`/byte stream but through a TS egress, not this pcapng sink. This design
only reserves the payload; the `.ts` egress is DVB-T's to build.

## What's left to the builder

- The exact `LinkType` enum members and their LINKTYPE numbers beyond the three named;
  pick from the registered pcap list, AX25_KISS for APRS, USER0 for opaque frames.
- Whether a frame row needs a per-row `link_type` field in its JSON, or whether the
  family (APRS vs generic `frame_decode`) determines it. Default: family determines it;
  add the field only if a second AX.25-family row needs a different type.
- pcapng writer: a tiny hand-rolled writer (four block types, one endianness) versus a
  crate. Prefer hand-rolled to avoid a dependency for ~60 lines; builder's call if a
  maintained crate is cleaner.
- The fixture for the row's test: a short synthetic AX.25/APRS capture run through the
  attach, asserting the pcapng parses and the first packet's bytes and comment match.
  Reuse the framing synthesiser (`test: {kind: synth, family: framing}`) the frame rows
  already use; keep it under the few-MB fixture budget.
- The embedded-row / test bookkeeping the DVB-T note lists (updating `EMBEDDED`, the
  row-name lists in `tests/modes.rs`, `cli.rs`, `modes_check.rs`) applies only if a new
  row is added; this feature adds no row, only an output and an op, so most does not.
- Phase B (separate plan): GSMTAP-over-UDP first, then extcap, both reading the Packet
  stream through `stream tail`.

## Open decisions for the operator

Carried separately in the handover message, not pre-settled here.
