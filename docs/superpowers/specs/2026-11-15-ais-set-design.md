# The `ais` named set

Short design, 2026-11-15.

## Intent

An operator asked the node's agent to decode AIS on a channel showing traffic. The agent started
a bare `watch` on channel A; it reported no signals and started no decoder, so the node looked as
if it had no AIS decode job. It has one: the `ais_decode` row (plan 29) and the `ais_vhf` channel
plan. Started by hand, the two decoders gave a CRC-clean message within three minutes.

What was missing is the one obvious thing to start, and prose that names it.

## Goals

- "Decode AIS" or "track the ships" resolves to one named thing, for the agent, the page's set
  list and the CLI.
- The agent is told why a watch is wrong on these channels.

## Non-goals

- No new job kind, stage or operation. The row and the plan stay as they are.
- No `type` field on subjects. Vessels already share the subject mechanism with aircraft and
  sondes (key `mmsi`, noun "vessels"); a type beside the noun is an open question for the
  operator, not part of this change.
- No change to the watch's detector (see `TODO.md`, "AIS: first decode on air").

## Approaches

1. **A named set holding the plan (chosen).** `sets/ais.json`: one role, one `plan_start` member
   of `ais_decode` on `ais_vhf`. Sets are what the agent is told to reach for first, the page
   lists them, and `set_stop ais` takes both jobs down. Data only.
2. **A watch-based set, as `pagers_au` is.** A watch with `modes: ["ais_decode"]` and a short
   `persist_ms`. Rejected: the two channels are fixed and known, so there is nothing to find, and
   a detector tuned to 27 ms bursts announces noise.
3. **Prose only.** A field-craft line pointing at `plan_start`. Rejected alone: the line existed
   and did not steer the agent; a set shows up in `set_list` where it looks.

## Design

- `sets/ais.json`, embedded in `zeta-core/src/sets.rs`. Role `radio`: a radio that captures
  161.9625-162.0375 MHz at once. Its notes say what it is for and that a watch is not.
- `docs/agents/field-craft.md`, the AIS line: the set by name, and "never a `watch` here" with
  the reason (one burst of about 27 ms against a detector that announces after `persist_ms`).
- The built-in set lists in the CLI help, the generated `AGENTS.md` and skill, and the tests.
- `modes/ais_decode.json` gains `subject.merge: ["mmsi"]`. A vessel alternates between the two
  channels and each channel is its own job, so without it the Subjects tab showed the same MMSI
  twice (seen on the first live run). With it the two jobs share one "vessels" table, one row
  per vessel, as the ACARS channels do on `reg`. The map still keeps a track per job and key, so
  a vessel heard on both channels has two markers at one place: `TODO.md` holds that.
- A test pins the member (`plan_start`, `ais_vhf`, `ais_decode`, every channel) and the notes.

## The payload's bit order (found on the first live run)

The first frames off air passed the CRC and all read as type 12 safety messages with noise for
text, 168 bits each: the length of a class A position report. `hdlc_bytes_to_payload` unpacked
each frame byte least significant bit first; the payload is each byte most significant bit
first. Read that way the same frames are type 3 position reports with plausible MMSIs, speeds
and positions at sea. The generator packed bytes with the mirrored error, so `zeta modes check`
and every test agreed with the decoder and none compared it with anything outside the crate.

- `payload_to_hdlc_bytes` and `hdlc_bytes_to_payload` (`zeta-dsp/src/ais_msg.rs`) pack and unpack
  most significant bit first. The CRC and the framing were right and do not change.
- A test pins the order from what a frame byte must hold (type in the top six bits).
- Left open in `TODO.md`: a check of the generator's frame against a decoder from outside the
  crate, as FLEX has against multimon-ng, so a mirrored error cannot pass again.

## Acceptance

`set_start` with `set: "ais"` on the field host starts both decoders in one capture; asked for
AIS, the agent starts the set and not a watch. Vessels under way appear once each in the
Subjects tab with a position, and on the map.
