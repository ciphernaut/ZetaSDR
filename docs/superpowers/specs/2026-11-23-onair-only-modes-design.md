# Modes only OnAir decodes: rows with no decoder of their own

Design note for plan 47. The design above it is the operator's, approved 2026-11-21:
`2026-11-21-alternative-handlers-design.md` (a row "may now have no decoder of its own",
batch 3 of section "OnAir", and "Decided by the operator"), as re-cut by
`2026-11-22-onair-bus-modes-design.md` (plan 45, which named this plan) and
`2026-11-22-onair-callback-modes-design.md` (plan 46). This note does not restate them. It
records what a read of OnAir (`a63c3a8`, patched) and of the node found, the operator's
rulings of 2026-11-23, and the call made for each open point. Where this note and those
above differ, this note is the later word; every such place is marked **differs**.
Status: written on the operator's "write the plan", 2026-11-23; the rulings below are the
operator's, the rest is put for review with the plan.

## The operator's rulings, 2026-11-23

- **One plan** for every mode below, not two.
- **ASTERIX is left out.** It is a network format for surveillance data (UDP datagrams or
  a file), OnAir's receiver for it takes no IQ, and nothing is planned for it.
  **Differs:** the parent's batch 3 lists it.
- **The rows get their own Decode entries** in the page's box menu.
- **The placeholder field is `content`,** not `policy`. In this project a policy is where
  a situation can be handled in more than one way, particularly where a handler might not
  be present; the field states a fact about the row, and a policy reads it later.
  **Differs:** the parent's `"policy": {"content": "network_traffic"}` becomes
  `"content": "network_traffic"`.

## Intent

Eleven modes the node has no decoder for become ordinary rows, decoded by OnAir through
`onair-rx`: **Zigbee, Thread, Wi-SUN, Bluetooth LE, LoRaWAN, Z-Wave, Wi-Fi, TETRA, GSM-R,
LTE and DVB-S IP.** Each is listed, started from the page, identified, watched and put in
sets as any row is, gives events, and gives packets a capture job writes for Wireshark,
which is the point of them. They are unrestricted, as decided 2026-11-21, and each says
what it carries (`content`) so that a later policy has something to read.

## What the reading found

**The node**

- **Neither `decoder: null` nor a row field for content exists.** A row written so loads
  as broken and every handler entry for it is dropped.
- **Once a handler is chosen, nothing reads the row's decoder.** Expansion with a program
  block, the band check, the external stage and its outputs all follow the handler.
  Identify, a watch, a set, a plan and `capture_decode` start through the handler pick
  and ask only whether the mode can be started.
- **The first handler of a mode is taken to be the row's own decoder** in about ten
  places (the registry's attach, probe carry and check times, the pick's report, the
  operation types, two reads on the page). A list with no first element would panic in
  one of them.
- **The row's own outputs are the baseline of plan 46's ranking rule.** With an empty
  baseline every handler counts as giving as much, which is right for these rows and is
  already pinned by a unit test.
- **`access` is the template for a row field that travels:** to `modes_list`, to a
  capture job's parameters and to the packet capture's record.
- **A handler entry cannot pass the job's width.** There is no `$bandwidth_hz` among a
  program's placeholders (plan 46's first follow-up).
- **Nothing caps a handler's input rate** but the source's own width.
- **The synthesiser makes none of these signals.** A row's check may instead name a
  SigMF pair found under `ZETA_FIXTURES_DIR`.
- **The box menu's Decode entries are written out one by one** (six rows have one); a
  row without one is reached through "Decode across this band" and a capture's Send to.

**OnAir**

- **All eleven receivers exist, run bare and are in the library `onair-rx` links.**
  Every one publishes its frames to the Wireshark hub, on a channel the hub's table
  already has, with a link type Wireshark knows (Z-Wave: a user type).
- **Two also publish to the automation bus:** Bluetooth LE (`ble.device`) and LoRaWAN
  (`lora.packet`).
- **Six publish from inside the feed call:** Zigbee, Thread (the Zigbee receiver in
  Thread mode), Bluetooth LE, LoRaWAN, Z-Wave, Wi-Fi.
- **Five publish from a thread of their own:** Wi-SUN (a decoder thread; frames are
  handed over by a later feed; it drops bursts when fed faster than real time unless set
  offline, and holds the last frames until flushed), and TETRA, GSM-R, LTE and DVB-S,
  which take a stretch of input (0.2 to 2.3 s), decode it on a worker and skip the
  samples that arrive meanwhile. OnAir's own tests pace their input on `busy()`.
- **The wrapper's hub sink is not locked.** It collects into a plain vector, which is
  safe only while every publisher is the feeding thread.
- **What they decode is the broadcast side.** Wi-Fi: beacons, probe responses and the
  low-rate frames (1 Mbit/s DSSS, 6 Mbit/s OFDM); Bluetooth LE: advertising, connections
  are not followed; TETRA, GSM-R and LTE: the cell's broadcast channels and system
  information, nothing addressed to a terminal; DVB-S: IP from GSE, and from MPE and ULE
  when the transport stream is passed back in. Zigbee, Z-Wave and LoRaWAN try keys only
  from a key store that is empty unless loaded; the hub gets frames as received.
- **Each takes what the rate holds.** Zigbee and Bluetooth LE read one channel at the
  centre; Wi-SUN, LoRaWAN, Z-Wave and TETRA read every channel inside the input. Z-Wave
  needs the centre frequency. LoRaWAN needs a channel plan and does not pick one itself.
- **Rates:** from 1 MHz (LoRaWAN, Z-Wave, GSM-R) through 4 MHz (Zigbee, Bluetooth LE) to
  20 MHz (Wi-Fi; LTE's system information on a wide carrier).
- **OnAir has a signal generator for every one of the eleven.**

## Approaches

**A row with no decoder, in the registry**

1. **A stand-in own handler that is never ready** (chosen). The row keeps a first handler
   so that every place that takes the first as the row's own stays right; the stand-in
   has no outputs, is `unavailable` with a fixed reason, is never picked, is not listed
   in what the node reports and is never written into a job. The row's own state is then
   `unavailable` until a handler file's entry is ready, which is the parent's sentence.
2. **No first handler.** Truer to the data, and a rework of every place that indexes the
   first: more code, more chances of a panic, for the same behaviour seen from outside.

**The modes that publish from their own thread**

1. **The two existing paths, extended** (chosen). The hub tap is written once, locked,
   and both paths use it. Wi-SUN stays a bus mode (its lines are made from its packets)
   with an end-of-input flush. TETRA, GSM-R, LTE and DVB-S become polled modes, as plan
   46's are, whose sessions wait while the receiver is busy, and the polled path gains
   packets. No third path.
2. **A threaded runner of its own.** A third copy of the read loop.

**The test signals**

1. **Recorded pairs made by OnAir's generators at test time** (chosen). A row's `test`
   names a SigMF pair by a relative path; `onair-gen` makes it into a directory the
   wrapper-backed test passes as `ZETA_FIXTURES_DIR`. Nothing large enters git.
2. **Synthesiser families in the node** for eleven protocols: the right end state for a
   node that decoded them itself, and far outside this plan.

## The design

### The node

- **`decoder: null`.** A row's `decoder` may be `null`. Such a row:
  - has the stand-in own handler: id `none`, `in_process` false, no outputs, state
    `unavailable`, reason `the row has no decoder of its own`. `none` is refused as a
    handler file's id;
  - reports `state` `ready` when a handler file's entry for it is ready, as any row with
    handlers does, and otherwise `unavailable` with the reason `no decoder of its own,
    and no handler for it is ready` (and the missing program's line, as today, when the
    handler is there and its program is not);
  - reports `decoder_kind` `null` and `checked` `null`, and its `handlers` list leaves
    the stand-in out;
  - is refused at load with `quick` other than `null` in its `identify` block, and with
    a `test` of kind `synth`;
  - is started only through a handler: a start that names none gets the policy's pick,
    and a job's record always names its handler.
- **A handler entry for such a row must map every property of the row's schema** (bar
  the envelope fields), the rule a program row's own block meets, since no other decoder
  fills them.
- **`zeta modes check`.** For a row with no decoder, `modes check <id>` checks the
  handler the policy picks (recorded as `<id>@<handler>`), and is a skip when none is
  ready, which `--strict` fails as it does any skip. `--handler` is unchanged.
- **`content`.** A row may carry `"content": "network_traffic"` (one value today;
  `null` for the rest). It is validated, reported by `modes_list` beside `access`,
  carried in `modes_show`'s row, written into a job's context as `content` when the job
  launches, and copied to a packet capture's record. Nothing reads it. It is not put on
  events, and `access` and its rules are untouched. All eleven rows carry it.
- **`$bandwidth_hz` for a program's arguments:** the job's band width, so a wrapper can
  be told how wide the signal is. The OnAir entries that use it pass `--width`.
- **The eleven rows** (`modes/<id>.json`, embedded): `zigbee_decode`, `thread_decode`,
  `wisun_decode`, `ble_decode`, `lorawan_decode`, `zwave_decode`, `wifi_decode`,
  `tetra_decode`, `gsmr_decode`, `lte_decode`, `dvbs_ip_decode`. Each has its band, its
  event schema, a subject where the mode has something to follow (a device, a network, a
  cell), an identify block with no quick test, `content`, notes, and a `test` naming its
  generated pair. A row's ranges are the standard's own bands, across regions, with the
  source said in the notes; `null` where the standard has no bound. None is taken from
  one station or one country.
- **Eleven entries in `handlers/onair.json`,** with packets on the pcap port for each.
- **The page.** Eleven Decode entries in the box menu, by the rows' labels, behaving as
  the six there do (elsewhere when the selection does not fit the row, a handler submenu
  with two ready handlers). An entry whose row is not ready is greyed with the row's
  reason. Nothing else on the page changes; the two reads of a row's first handler are
  checked against a row whose first listed handler is a file's.
- **Prose.** Per-operation notes, `handlers/README.md`, the recipes and the field craft
  say what a row with no decoder is and what each of the eleven gives and does not give;
  the generated files are regenerated. `UAT.md` gains plan 47's live checks.

### `onair-rx` (0.4.0)

- **Eleven modes:** `zigbee`, `thread`, `wisun`, `ble`, `lorawan`, `zwave`, `wifi`
  (bus modes) and `tetra`, `gsmr`, `lte`, `dvbs` (polled modes), all with packets.
- **One hub tap, locked,** used by the bus path and the polled path: it enables the
  mode's channel, collects packets under a mutex from whatever thread publishes, and is
  drained by the read loop after each block.
- **Replay gives the same lines as live.** A receiver with a worker is never allowed to
  skip input: Wi-SUN is set offline, and a polled session waits in its feed while the
  receiver is busy. At the end of input the receiver is flushed and one last drain runs.
  `--drop` turns the waiting off (the receiver's own live behaviour) for use by hand and
  for a mode that cannot keep real time otherwise.
- **Lines.** Bluetooth LE and LoRaWAN: the bus event, as the bus modes of plan 45.
  Zigbee, Thread, Wi-SUN, Z-Wave and Wi-Fi: one line per frame OnAir's own parser reads,
  made from the hub packet as AIS's are. TETRA, GSM-R and LTE: one line per cell when it
  is first read and when what is known of it changes, from the receiver's snapshot.
  DVB-S: a status line with plan 46's cadence (first lock or 2 s, on change, at least
  every 2 s), with the count of IP datagrams so far.
- **A packet's time** is the first sample of the block after which it was drained. For a
  receiver with a worker that trails the frame by up to the receiver's own stretch of
  input; the README says so per mode.
- **Options:** `--width HZ` (the job's band; DVB-S bounds its symbol rate search with
  it), `--plan NAME` and `--region NAME` (LoRaWAN, Wi-SUN, Z-Wave by hand). Without
  `--plan`, LoRaWAN takes the plan with the most channels inside the input around
  `--freq`, a tie going to OnAir's table order.
- **Tests.** `onair-gen` gains each generator and a `--sigmf <stem>` output; a new
  `test/only.sh` checks each mode's lines and packets against what was generated.

## Non-goals

- ASTERIX, in any form.
- Anything that reads `content`: who may start such a row, or capture or view its
  packets. That is the policies framework's (`TODO.md`).
- Decryption. No key is loaded, passed or stored; frames are given as received.
- What OnAir's receivers do not decode: Wi-Fi's higher rates, Bluetooth connections,
  traffic to a terminal on TETRA, GSM-R or LTE, DVB-S2X.
- Following a hopping network by retuning, and more than one channel of a one-channel
  receiver per job (a second job is a second channel).
- Speech, picture or a transport stream from a handler (plan 46's deferred items).
- Synthesiser families for these protocols; a second carried patch to OnAir.
- A Decode entry for every row made from the rows themselves: a `TODO.md` line.

## Anti-goal

A handler listed as ready for a mode whose check it cannot pass, as plans 45 and 46
ruled. A mode that misses the listing bar stays in `onair-rx`, and its row and entry
stay out of the node, with a line in `TODO.md` saying what it needed. A row with no
decoder and no entry would be a row nothing can ever run.

## Left to the builder

- Each row's event schema, subject and summary, within the members the plan names.
- Each row's `band` (its width, narrowest width and ranges) and each entry's `rate_hz`,
  starting from the plan's table and settled by measurement.
- The C++ shape of the hub tap, and whether a polled session's wait is a sleep on
  `busy()` or the receiver's own blocking switch where it has one (DVB-S has).
- How DVB-S turns the job's width into its symbol rate search, and whether MPE and ULE
  can be fed back without a patch.
- Whether a cell's line for TETRA, GSM-R and LTE is better read from the snapshot or
  from the hub packet.

## Risks

- **Width.** Wi-Fi and LTE want 20 MHz of samples, about 80 MB/s into the wrapper. No
  radio at hand is known to hold that without dropping; a source that cannot is a
  refused start with the existing words. The check (a file) proves the decode, and the
  listing bar is measured on a file source at real time. Live proof is UAT's.
- **Cost.** The wide receivers are heavy. The bar is plan 46's: the check passes and the
  job keeps real time on the build host with the node dropping nothing; a mode that only
  passes with `--drop` is listed with `--drop` and says so in its notes.
- **The receivers were read, not run.** Rates, setters and thread behaviour in this note
  come from reading OnAir's source and tests; the first step of each wrapper task is to
  run the generator into the receiver and see.
- **Twenty-eight rows in the box menu's reach.** Eleven more Decode entries make a long
  menu where many fit one selection (2.4 GHz holds four). They fall under "elsewhere"
  when the selection does not fit; the look is the operator's to judge on the page.
- **Size.** Five wrapper tasks and seven node tasks: about 4 to 5 million subagent
  tokens by plan 45's and 46's actuals.
