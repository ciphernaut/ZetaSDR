# OnAir's other modes: what a spike settles

Design note for plan 45. The design itself is the operator's, approved 2026-11-21:
`2026-11-21-alternative-handlers-design.md`, section "OnAir" (delivery item 4). This note
does not restate it. It records what a read of OnAir (`a63c3a8`) and a throwaway spike in
`onair-rx` (branch `spike-generic`) found that the design did not say, and the call made
for each. Where this note and the parent spec differ, this note is the later word; every
such place is marked **differs**. Status: awaiting the operator's approval.

## Intent, unchanged

Every mode OnAir shares with the node becomes a second handler for its row, chosen by the
policy or by hand, each proven by `zeta modes check --handler onair` against the capture
the row already synthesises. Modes only OnAir decodes come in as rows with no decoder of
their own. What is worth giving back to OnAir is proven in `onair-rx` first.

## What the spike found

- **OnAir's receivers already publish to two mode-independent places**, with no engine, no
  window and no audio device started:
  - the *automation bus*: one structured event per decoded message (a type and named
    fields), delivered on the caller's thread inside `feed()`;
  - the *Wireshark hub*: the frame as received, per channel, under a pcap link type.
- **One generic path in `onair-rx` serves a mode from those two.** The spike's template
  (a receiver type, one line of setup, a tap on the bus) decoded four modes:
  - POCSAG: `modes check` passes through the node, end to end;
  - APRS: decoded with its position, and its AX.25 frame reached the hub;
  - ACARS: decoded once the receiver was told its channel. It ignores a carrier at the
    tuned centre unless the channel is named, so the wrapper needs the job's centre;
  - AIS: twelve NMEA sentences reached the hub, but the bus gave one record of a vessel's
    state, held to one per five seconds. The row wants one event per message.
- **The bus's time is decode time.** Events arrive inside the `feed()` of the block that
  completed them, so the wrapper knows the sample they belong to.
- **The hub has no in-process reader.** It writes pcapng files, a loopback TCP stream and
  GSMTAP over UDP, and stamps every packet with the clock: all seventeen callers leave the
  time argument out.
- **OnAir's receivers want wide input**: 1 Msps for ACARS and the pagers, 240 ksps for
  packet, 250 ksps for AIS, where the rows are 10 to 25 kHz channels. The node resamples up
  to the handler entry's `rate_hz`, which works and costs CPU not yet measured.
- **Not every shared mode is on the bus.** ACARS, VDL2, HFDL, POCSAG and FLEX, APRS and
  the radiosondes are (the last three read from the source, not yet run). RDS, DMR, P25,
  M17, DVB-T and analog video have no emitter and need a callback written per mode, as
  ADS-B has.
- **The pipe patch has no premise.** OnAir's raw file source is a plain read; it sizes and
  seeks only SigMF, WAV and BLUE files. Wrapping bare receivers needs no engine at all.

## Approaches

1. **A generic bus mode in `onair-rx`, a table row per mode** (chosen). A mode is a
   receiver type, its setup (the channel, the protocols) and the hub channel if it has one;
   the handler file's `fields` pointers map the bus's names to the row's. New modes on the
   bus cost a table row, a handler entry and a check.
2. **A hand-written run function per mode**, as ADS-B was built. Full control of every
   member, and about 150 lines of C++ a mode: the near-identical variants the operator has
   asked not to build. Kept only for modes with no bus emitter.
3. **OnAir's engine fed from a pipe.** It adds an analysis thread and its channel
   placement, loses the sample a message came from, and still yields the same bus events.
   No mode in scope needs it.

## The design

### `onair-rx`

- `--mode <m> --rate <hz> --format cs16|cf32 [--freq <hz>] [--pcap-port <p>]`. `--freq`
  is the centre of the input; modes that place channels by frequency (ACARS, AIS) need it
  and end with one line on stderr without it. **Differs:** the parent's contract had no
  frequency.
- A line is the bus event's fields as JSON members, whole numbers without a decimal point,
  plus `event` (the bus type, so one mode's kinds can be told apart) and `t_sample` (the
  index of the first sample of the block that completed it). The wrapper adds what the row
  needs and the bus lacks where it knows it (APRS `fcs_ok`: OnAir only publishes frames
  whose check passed, or that it repaired and says so).
- Packets come from the hub through a sink the wrapper registers, stamped with sample time
  on the wall clock of the first sample, as ADS-B's are. This is a carried patch to
  OnAir's hub (a sink callback, and callers passing their time through), about fifteen
  lines, applied by `build.sh` to the checkout it builds.
- AIS events are built from the hub's NMEA sentences, one per message, by the wrapper;
  the bus's vessel record is not used.
- ADS-B keeps its own run function: its line carries what one frame established, which
  the bus's two-second aircraft record cannot give.

### The node

- `handlers/onair.json` gains one entry per mode, each with `--freq $center_hz`, the
  receiver's `rate_hz` and the `fields` map. AIS and APRS gain `packets` (`pcap`), with
  link types `ais` (raw IP carrying NMEA over UDP) and `ax25` named in the node's table
  and the Lua file's notes.
- A handler entry may give a constant for a row field (`"protocol": "pocsag"`), where the
  program's value is spelled differently or absent. This is the one change to the handler
  file's shape; whether it is a `const` map beside `fields` is the builder's.
- No change to rows, the policy, the page or the operation set.

### Upstream

**Differs:** the two patches to offer are now the hub's (an in-process sink; receivers
passing sample time), one pull request each with a test. The pipe patch is dropped. The
`onair-rx` README's sentence about the file source is corrected.

## Delivery

**Differs:** the parent's fourth delivery becomes three plans.

- **Plan 45: the modes on the bus.** The generic mode, the hub patch, then ACARS, POCSAG,
  FLEX, APRS and AIS with packets, VDL2, HFDL and the three radiosondes. Acceptance: each
  passes `modes check --handler onair`; an AIS and an APRS capture open in Wireshark; on
  the field host one of them runs beside the node's own decoder on live traffic.
- **Plan 46: the modes with no emitter.** RDS, DMR, P25, M17, DVB-T and analog video, each
  with its own callback. DVB-T has no synthesised capture today and needs one or a stated
  gap.
- **Plan 47: modes only OnAir decodes.** `decoder: null` rows and the `policy`
  placeholder, as the operator decided 2026-11-21. Their packets ride plan 45's hub sink.

## Non-goals for plan 45

- `decoder: null`, the `policy` field, any mode not listed under plan 45.
- OnAir's engine, its scanner, its audio or its transcripts.
- Any change to how a handler is chosen.

## Left to the builder

- The C++ shape of the mode table and whether setup is a function or data.
- How the wrapper turns an NMEA sentence into the AIS row's members (OnAir's own AIS
  parser is linked; multi-sentence messages are joined before the event).
- The exact members of each mode's line beyond the row's fields (`extra`).
- Whether the CPU cost of the wide input warrants a note in `modes_show`; measure first.
- The fixtures: every row in plan 45 already has a synthesised capture.

## Risks

- A radiosonde, VDL2 or HFDL receiver may want something the spike's four did not (a
  second channel, a much higher rate). Each is tried on the generic path first; one that
  does not fit moves to plan 46 with the reason written down.
- The hub patch is carried until OnAir takes it. If it is declined, the wrapper reads the
  hub's loopback stream instead, and packets keep decode time.
