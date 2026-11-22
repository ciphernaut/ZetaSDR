# OnAir's modes with no bus emitter: events from polled state

Design note for plan 46. The design above it is the operator's, approved 2026-11-21:
`2026-11-21-alternative-handlers-design.md`, section "OnAir", as re-cut by
`2026-11-22-onair-bus-modes-design.md` (plan 45), which named this plan. This note does not
restate either. It records what a read of OnAir (`a63c3a8`) and of the node found for the
six modes plan 45 left, the scope the operator chose, and the call made for each open
point. Where this note and the two above differ, this note is the later word; every such
place is marked **differs**. Status: approved by the operator 2026-11-23.

## Intent

RDS, DMR, P25, M17, DVB-T and analog video each gain OnAir as a second handler, chosen by
the policy or by hand, so that every mode OnAir shares with the node has an alternative
decoder. The operator's scope, 2026-11-22: **events only, for all six, for now**. What
these modes produce beyond events (speech, picture, a transport stream) comes back later
as plans of their own.

## What the reading found

- **All six receivers run bare.** Each is constructed alone, told to stay silent (no sound
  device), configured with a rate and fed blocks. None starts a thread unless asked. All
  six are already in the library `onair-rx` links; `build.sh` needs no new option.
- **None of the six publishes anything**: nothing on the automation bus, nothing on the
  Wireshark hub. The hub's channel table has no entry for any of them. Plan 45's generic
  path cannot serve them.
- **What they offer instead is state.** Every one has a `telemetry()` snapshot the
  receiver refreshes every 0.1 to 0.25 s of input: a station's PI, PS and radiotext; a
  call log with source, destination, colour code or NAC; the TPS of a multiplex; the
  system and sync of a picture. Only RDS also hands over each decode as it happens (a tap
  on every raw group). The text log callbacks are prose, not data.
- **OnAir decodes the payloads too**: speech (DMR and P25 through mbelib, M17 through
  Codec 2), RGBA picture frames (newest only, by polling), 188-byte transport stream
  packets with a demultiplexer for the service list.
- **A handler job carries events and packets only.** `external_decode` declares no audio
  or video output, a start that `needs: [audio]` passes over a handler, and attaching a
  transcriber to a handler job is refused. The parent spec names `audio` as a handler
  output; it was never built.
- **The rates differ widely.** DMR takes a 48 kHz channel. P25 and M17 want 250 kHz, RDS
  500 kHz, analog video 8 MHz or more, DVB-T 7.9 MHz or more for an 8 MHz multiplex.
- **Five of the six rows have a synthesised check**; DVB-T has none, and the node has no
  DVB-T modulator. OnAir has a signal generator for every one of the six.

## Approaches

1. **One polled path in `onair-rx`, a small function per mode** (chosen). The read loop,
   the poll by sample count, the remembering of what was already written and the final
   poll at the end of input are written once. A mode supplies its receiver's setup and a
   function from a snapshot to the lines that are new. This is plan 45's shape (one
   mechanism, a table row per mode) applied to state instead of to bus events.
2. **A run function per mode**, as ADS-B has. Six near-copies of the same loop, the
   variants the operator has asked not to build. ADS-B keeps its own because its line is
   one frame; none of these six is like that.
3. **Bus emitters added to OnAir's receivers by carried patches**, so that plan 45's
   generic path serves them. OnAir already defines event families for broadcast text and
   for voice calls that its core never emits. It is the tidiest end state, and it is
   OnAir's author's to accept: six carried patches to receiver code is a heavier burden
   than the one hub patch carried today. Not built; noted in `onair-rx`'s `UPSTREAM.md` as
   a possible third offer.

## The design

### `onair-rx` (0.3.0)

- Six modes: `rds`, `dmr`, `p25`, `m17`, `dvbt`, `video`. Same command line as today. None
  has packets, so `--pcap-port` with one of them is refused as it is for a bus mode
  without a channel (that refusal is keyed on bus modes today and gains the polled ones).
- **The polled path.** After every block fed, when the input has advanced by the mode's
  poll interval (counted in samples, so a replay gives the same lines as a live run), the
  mode's function is given the receiver's snapshot and writes a line for each thing that
  is new or has changed since the last poll. One more poll runs at the end of input, so
  a decode completed in the last block of a short replay (`capture_decode`) is not lost.
  A check's capture never ends (the check's source repeats it in real time), so every
  decode must also surface from the regular polls.
- **The first line counts.** `modes check` compares the first event only. A call's first
  line therefore waits until the call's source, destination and colour code or NAC are in
  hand, as the rows' own first event does, and a status mode's first line waits for first
  lock or two seconds, as the row's own picture decoder does.
- **What a line is.** One JSON object, `event` and `t_sample` as the bus modes have them
  (`t_sample` is the first sample of the block after which the poll ran), a member only
  when its value is known. A line describes one thing, by mode:
  - `rds` (`rds.group`): one line per group from the group tap, with what the group
    itself holds (`pi`, the group type, `pty`, `tp`) and the programme service name and
    radiotext as the snapshot has them at that moment (absent until complete). The station
    finder is off: the station is the one at the centre.
  - `dmr`, `p25`, `m17` (`voice.call`): one line when a call is first seen, one when a
    field of it changes (a late talker alias, an encryption flag), one when it ends. The
    members are the call's own: source, destination, group or private, colour code and
    slot (DMR), NAC, manufacturer, algorithm and key ids (P25), channel access number,
    type and encryption (M17), and for M17 a position when the transmission carries one.
  - `dvbt` (`dvbt.status`): one line when lock or any signalling parameter changes, and
    one at least every two seconds, so a watcher always has a current one. Members: lock,
    bandwidth, mode, guard, constellation, code rates, hierarchy, cell id, SNR. When the
    transport stream's tables have been read, `services`: the list of service names with
    their ids and kind (television, radio), from OnAir's demultiplexer. The row's own
    program does not give a service list today.
  - `video` (`video.status`): the same cadence. Members: system, standard, lock, line
    rate, SNR. The receiver runs in FM video mode, which is what the row decodes.
- **No codec, no picture.** No audio tap is set and no frame is polled. Events must not
  depend on mbelib or Codec 2 being installed, and the receivers must not spend CPU
  decoding speech nobody takes. The first is a requirement, proven with the codec
  libraries hidden. The second holds only as far as OnAir allows without a patch: staying
  silent stops the sound device, not the decoding, and with a codec installed all three
  voice receivers decode every frame unless a voice monitor refuses the call. If a stock
  monitor can be set to refuse, the wrapper sets it; if not, the CPU is accepted,
  measured, and written into `UPSTREAM.md` as part of the possible offer.
- **DVB-T bandwidth.** A handler entry cannot pass the job's bandwidth (there is no
  placeholder for it), so the wrapper finds it: 8, 7 then 6 MHz in turn until the TPS
  locks, and 5 MHz as the row's own program also tries if OnAir's receiver takes it.
  `--bandwidth <mhz>` fixes it for use by hand.
- **Tests, in `onair-rx`.** `test/gen.cpp` gains OnAir's generator for each of the six;
  `test/run.sh` checks each mode's lines against what was generated. This is the only
  end-to-end proof for DVB-T (see below).
- `README.md`'s mode table gains the six; `UPSTREAM.md` gains approach 3 as a possible
  offer.

### The node

- `handlers/onair.json` gains six entries, each `--mode`, `--rate $rate_hz`,
  `--format cs16`, `--freq $center_hz`, the receiver's `rate_hz`, and a `fields` map from
  the wrapper's members to the row's. Members the row has no field for go in `extra`
  (a DMR talker alias, the DVB-T service list).
- **Rates** (`rate_hz`): `dmr` 48 kHz; `p25` and `m17` 250 kHz; `rds` 500 kHz; `dvbt`
  10 MHz, as the row's own program takes; `video` set by the builder by measurement (see
  Risks).
- **The rows' audio fields.** The DMR and P25 rows say on each event whether speech is
  being produced and, if not, why (the M17 row has no such fields). Under this handler
  none is. The entry gives
  `audio: false` as a constant. The reason (`silent`) is `encrypted` when the call is,
  and otherwise left out: the rows' other values (`no_vocoder`, `vocoder_failed`) say
  something about a vocoder that is not true here. No row's schema is changed for this
  plan. The rows write two events a call (link control in hand, and `eot` at its end);
  the wrapper's first and last lines of a call are those two.
- **Checks.** `zeta modes check --handler onair --strict` passes for `rds_decode`,
  `dmr_decode`, `p25_decode`, `m17_decode` and `video_decode` against the captures the
  rows already synthesise. **`dvbt_decode` has no check on the node: a stated gap.** Its
  entry is validated only, as HFDL's and DFM's are, and a hermetic test pins its field
  map against a recorded line of the wrapper's output. The test that lists OnAir's modes
  (`crates/zeta-core/tests/modes.rs`) and the one that needs the wrapper
  (`ZETA_TEST_ONAIR_RX`) are extended.
- **The page's suite.** Its node keeps OnAir to ADS-B, so no scenario should change. The
  full UI suite is run once before landing all the same, because the handler file
  changed.
- **Prose.** The handler's notes and the recipes say, per mode, what the OnAir handler
  gives and that it gives no speech and no picture. `AGENTS.md` and the skill are
  regenerated. `UAT.md` gains plan 46's live checks: one of the voice modes and RDS
  beside the node's own decoder on live traffic, the DVB-T service list on a real
  multiplex, and the CPU cost of each.
- **The node-wide ranking does not take speech or picture away.** These are the first
  handler entries that give fewer kinds of output than the row's own decoder, and every
  start from the page, a watch or an identify asks for no particular kind. With `onair`
  in the node-wide ranking, every DMR, P25 and M17 job would start silent and an analog
  video job without its picture. The rule: the node-wide ranking (`all`) passes over a
  handler that lacks a kind of output the mode's in-process handler gives. The mode's
  own ranking still picks it, and so does naming it on the start: both are the operator
  saying so for that mode. **Differs:** plan 45's "no change to how a handler is chosen".
  Put to the operator as an open point with this rule as the proposal; approved with the
  note.
- No change to rows, the page or the operation set.

## Non-goals

- Speech, picture or a transport stream from a handler. Each is a TODO gist written with
  this note: an audio output for handlers (any handler, not OnAir's alone), picture
  frames from a handler, a transport stream as a stream kind.
- DMR data messages and location reports, P25 trunking (site, adjacent sites, control
  channel messages), P25 phase 2, DVB-T2, teletext and captions. OnAir's snapshots hold
  them; a line each in `TODO.md` under plan 46.
- Any other change to how a handler is chosen, and plan 47's `decoder: null` rows.
- A second carried patch to OnAir. If a receiver has no switch for something this plan
  wants off, the plan lives with it and says so.

## Anti-goal

A handler listed as ready for a mode whose check it cannot pass. A mode that does not fit
is left out of the handler file, with a line in `TODO.md` saying what it needed, as plan
45 ruled.

## Left to the builder

- The C++ shape of the polled path (a template over the receiver and its snapshot, or a
  table of closures), and each mode's poll interval.
- How a call is keyed between polls (the snapshot's call log is a list that grows and
  ages) so that one call gives one first line and one last line.
- Each mode's exact member names and which go to `fields` and which to `extra`. The row's
  field names and enumerations are the target: the wrapper's `shape` spells values as the
  row does (`group` and `private`, `voice` and `data`, the DVB-T mode and guard strings
  as the row's own program writes them).
- Whether the P25 and DMR rows' `channel` events (a channel seen with no call) can be
  made honestly from the snapshot; if not, only `call` events are written.
- Whether the service list is one `services` member or a line per service.

## Risks

- **Analog video is the poor fit.** The receiver's FM video mode is built for 16 MHz
  or more of samples and warns below that, the row's band is 24 MHz, and the check's
  capture is 8 MHz. The builder measures first: the lowest `rate_hz` that passes the
  check and holds a real FM video signal, and its CPU cost through the node's resampler.
  The bar for listing it, here and for DVB-T: the job keeps real time on the builder's
  host with the node dropping no samples, and the measured cost goes into the handler's
  notes. A mode that misses the bar stays in `onair-rx` and out of the handler file,
  with its line in `TODO.md`.
- **A snapshot is state, not a decode.** A call shorter than the receiver's refresh may
  appear only as a finished entry, and a field may arrive a poll late. The check captures
  (three superframes, 25 frames) are short and repeat without end, so the same call comes
  round again at the seam: how a call is keyed must give each pass its own first line.
- **DVB-T on the node is unproven until live.** Its proof here is `onair-rx`'s own test
  against OnAir's generator; UAT carries the rest. The synchronous receiver on one core
  may not keep real time at 10 MHz; the builder measures, and may turn on the receiver's
  second thread.
- **The DMR receiver's lower rate path** (`configureChannel` at 48 kHz) is read from the
  source, not yet run. If it does not decode the row's capture, DMR takes the 1 MHz path
  and its cost is measured.
