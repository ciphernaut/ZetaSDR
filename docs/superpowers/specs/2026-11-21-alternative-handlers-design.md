# Alternative handlers for a mode, chosen by policy, and "Send to"

Design doc. From the operator, 2026-11-20 and 2026-11-21, after plan 41's acceptance run
found no APRS traffic to capture and ADS-B traffic that could not be captured ("alternative
handler for each mode it supports"; "ZetaSDR is a framework for many options, not railroaded
limitations"; "we can use policies for selection amongst available options at runtime";
"include a 'send to' on jobs and objects to the new modes as appropriate, and vice versa";
"try a different decoder in parallel to the current one"; "one aircraft, merged, unique
fields visually marked ... could come from the theme"). Approach C below is the operator's
choice.

## Intent and why

A decoder mode has exactly one decoder today: the graph, program or frame chain its row
names. Where a row lists several programs (`dump1090`, `readsb`), the node takes the first
one installed. Two things follow that the operator does not want:

- **What a mode can do is fixed by one program.** The ADS-B row reads dump1090's decoded
  SBS text, so plan 41's packet capture has nothing to write for it, although aircraft are
  the steadiest traffic most stations hear. Another decoder (OnAir's) holds the frame bytes
  and would serve the capture.
- **The operator cannot compare decoders.** A decoder that hears nothing, or less than
  another would, looks the same as a quiet band.

So: a mode may have **any number of handlers** (the row's own decoder, and any program that
can decode it); a **policy** picks one when nobody names one; **Send to** runs another
handler beside a running job, on the same band, so the operator sees both; what both hear
about one aircraft or station is **one merged subject**, with the fields only one handler
gave marked as that handler's.

A spike on 2026-11-20 proved the first outside handler: a small wrapper (`onair-rx`, outside
this repository, GPL-3.0-or-later) ran OnAir's ADS-B receiver on IQ from stdin and wrote
OnAir's own Wireshark output as a pcap stream. Eight seconds of 1090 MHz IQ from the field
host's radio gave 207 Mode S frames from six aircraft, none dropped, at about twelve times
real time on one core. OnAir's receivers share one contract (`configure(rate)`, `feed`), so
one wrapper can serve each of its modes.

## Goals

1. **Handler files.** A program that decodes modes is described once, in its own file:
   which modes it serves and how it is run for each. Built-in files ship with the node; the
   operator adds their own from a directory. No row is edited to add a program.
2. **A policy picks.** Every start that names no handler gets one by a fixed, documented
   order of rules, with the operator's ranking first. A start may name its handler.
3. **Programs can give packets.** A program's frames reach plan 41's capture path as typed
   packets, for the row's own program (dump1090's Beast output) and for a handler file's
   (a pcap stream).
4. **Send to.** From a running job, a selection or a stored IQ capture, start another
   handler for the same mode beside what runs. Nothing is stopped or replaced by it.
5. **One merged subject, marked.** Siblings share their subjects; a field only one handler
   supplied is drawn in that handler's mark, which the theme defines.
6. **OnAir as the first outside handler**, for every mode it shares with a row, and rows for
   the modes only it decodes.

## Non-goals

- Replacing a running job's decoder in place. Send to always adds; stopping the first job
  stays the operator's own act.
- Choosing by measured results (which handler hears more, CPU used). The rules below are
  static; the comparison is the operator's, and a measured rule can join the list later.
- Merging subjects on the node. The merge stays in the page's subject store, as now; the
  node's part is that every event says which handler gave it.
- Sending a stored *packet* capture anywhere but to a download: the node has no decoder that
  reads pcapng.
- Phase B of plan 41 (live capture into Wireshark). OnAir's extcap protocol is prior art for
  it and no more.
- Shipping, linking or building any GPL program. `onair-rx` is installed by the operator,
  like `dump1090` and `dvbt-rx` (`docs/licensing.md`, "External handlers").

## Anti-goals (what would make this a failure even if it worked)

- A near-copy of a row per program (`adsb_decode_onair`): bands, subjects and event fields
  then drift apart.
- A handler that changes what a mode's events mean. Whatever handler runs, `adsb_decode`'s
  events carry the row's fields under the row's names; a handler's extra fields are added
  beside them, never in their place.
- Send to that stops, retunes or starves the job it was sent from.
- A capture, an event or a subject field whose handler cannot be told.
- A mark that only colour carries: the handler's name must be readable too (title, legend).
- A policy that silently moves a running job to another handler.

## Constraints

- **One operation set.** Starting with a handler, sending to one, setting the ranking and
  decoding a stored capture are `Op`s applied by `Node::apply`; the transports only
  translate. The agent gets them as tools with no further work.
- **A decoder mode is data.** Handlers are data too: JSON files validated when loaded, a
  broken one listed with its reason and never fatal, as rows are.
- **Typed frames over the bus.** A program's packet is a `Payload::Packet` built by the
  stage that reads the program, not bytes carried in an event.
- **Generated prose.** `AGENTS.md` and `SKILL.md` are generated; new prose goes in
  `crates/zeta/src/agents_md.rs` and `docs/agents/field-craft.md`.
- **Licensing.** The node stays Apache-2.0. Handler files describe how to run a program;
  they contain none of its code. The Lua dissector the node ships is written from the
  Mode S format, not taken from OnAir's.
- **Public repository.** No real host names, positions, observed traffic or aircraft
  addresses in fixtures or docs.
- **Existing rows keep working unchanged.** A node with no handler files behaves as today.

## Approaches considered

**How a mode gets several decoders:**

1. *A row per decoder*, linked by a shared mode name. Nothing about rows changes. Rejected:
   OnAir alone adds seventeen near-copies, and their shared parts drift.
2. *One row with a list of decoders.* No duplication. Rejected: adding a program means
   editing every row it serves, and an operator's own program means editing built-in rows.
3. *Rows describe modes; handler files describe programs* (**chosen by the operator**). A
   row keeps its decoder as its own handler. A file per program lists the modes it serves.
   One file adds a program; the policy chooses among whatever is installed. Cost: the larger
   schema change, and each handler's output must be mapped onto each row's event fields.

**How Send to is asked for:**

1. *A new op (`job_send`).* Rejected: a second way to start a job, with its own lease and
   validation path.
2. *`job_start` with a handler and the job it stands beside* (**chosen**). A sibling is an
   ordinary job, so stop, capture, transcribe and the job list need nothing new.

**Where the merge happens:**

1. *On the node, in a subject store.* Rejected for now: the node keeps no subjects today,
   and the page already merges jobs of one group.
2. *In the page's subject store, from events that name their handler* (**chosen**).

## The design

### Handlers

Every mode has a list of **handlers**. A handler has an `id`, a `name` for people, whether
it runs **in process**, what it **outputs** (`event`, `packet`, `audio`) and a state
(`ready`, or `unavailable` with the reason and the install pointer).

- The row's own decoder is the row's own handler. Its id is `builtin` for a graph or frame
  decoder (in process) and the row's first program name for a program row (`dump1090`). A
  graph row that needs a program for one stage (DMR and P25 with `zeta-mbe`, M17 with
  `c2dec`) is `builtin` and in process.
- **The word.** "Handler" is the operator's word for this and is the name in the operations
  (`handler`, `handlers`). Rows already use `decoder.handlers` for the programs a graph
  row's stages need; that key keeps its spelling and meaning (existing rows are unchanged),
  and prose calls those **helper programs** from now on. `ZETA_HANDLER_<PROGRAM>` keeps
  naming a program's path, for helper programs and handler files' programs alike.
- A **handler file** adds a handler to each mode it lists. Files are embedded like rows
  (`handlers/*.json`) and read from `--handlers-dir`; an operator file replaces the embedded
  one of the same id.

A handler file, in outline:

```json
{
  "id": "onair",
  "name": "OnAir",
  "programs": ["onair-rx"],
  "install": { "url": "..." },
  "modes": {
    "adsb_decode": {
      "args": ["--mode", "adsb", "--rate", "$rate_hz", "--format", "cs16",
               "--pcap-port", "$packet_port"],
      "input":   { "stream": "iq", "format": "cs16", "rate_hz": 2400000, "via": "stdin" },
      "output":  { "via": "stdout", "parser": "json_lines", "fields": { "icao": "/icao" } },
      "packets": { "via": "tcp", "port": "$packet_port", "parser": "pcap",
                   "link_types": { "147": "mode_s" } },
      "extra":   { "df": "/df", "level_dbfs": "/level" }
    }
  }
}
```

- Each mode entry is a program block as a program row has today (`args`, `input`, `output`,
  optional `demod`), so the existing external stage runs it. `output.fields` must yield the
  row's event fields under the row's names. `extra` names fields this handler adds; they
  are validated not to collide with the row's schema and are listed by `modes_show`.
- `packets` is new and optional, on a handler entry **and on a program row's own block**:
  a second output read as packets. Parsers: `pcap` (a classic pcap stream; each record is a
  packet under the stream's link type) and `beast` (dump1090's and readsb's binary output;
  each Mode S frame is a packet). The ADS-B row gains `packets` with `beast`, so dump1090
  becomes capturable without any outside handler.
- A row may now have **no decoder of its own** (`decoder: null`): a mode only outside
  handlers serve. It is `unavailable` until one of them is `ready`.
- A mode entry naming a mode the node does not have, or failing validation, breaks that
  entry only; the file's other modes load.

`modes_list` and `modes_show` give each mode's `handlers` (id, name, in_process, outputs,
state, reason, missing, program path, checked) and `handler`: the one the policy would pick
now. The mode's own fields keep their names and change meaning only where there is more
than one handler: `state` is `ready` when any handler is; `program`, `reason` and `missing`
describe the handler the policy would pick, or the row's own when none is ready; `outputs`
is the union over the ready handlers, so the page knows a capture is possible before it
knows which handler gives it; `checked` is the row's own handler's. A client that reads
only those fields sees what it saw before on a node with no handler files.

Probing stays a lookup: the node never runs a handler's program to learn what it serves.
The handler file is the statement of its modes; a wrapper too old for a mode fails that
job at launch with its own message.

### Packets from programs

The external stage declares a packet output when its block has `packets`. It opens the
packet output at launch, keeps the stream's framing in step always, and builds packets only
while something reads them (plan 41's rule). A lost or refused packet connection is noted in
the job's context and does not fail the job: its events are the job's first duty.

`LinkType` grows from three names to any link-layer type number, with names for the ones the
node knows; a capture file still holds one link type. Private (`DLT_USER`) numbers are the
node's to assign, so the same protocol reads the same from every handler: `user0` stays the
frame rows' opaque bytes, and **Mode S is `mode_s`, USER1 (148)**. A `packets` block's
`link_types` maps what the program writes onto the node's names (OnAir writes Mode S as
USER0). The `beast` parser emits `mode_s` directly. Registered types (AX.25, 802.15.4,
Bluetooth LE, LoRaTap, raw IP) pass through as written.

A packet from a program takes the time that program's events already take: the start of
the latest input frame the stage fed it (sample time; wall-clock only before the first
frame). A capture's times so match the event feed's, and a replay (below) carries the
recording's time with no further mechanism. (A parser that can recover the exact sample, as
Beast's sample counter allows, may do better; not required.)

The node ships a small Lua dissector, `docs/wireshark/zetasdr.lua`, that reads `mode_s`
(downlink format, address, type code, parity), with a line in `RECIPES.md` on installing it.

### Choosing a handler

`job_start` gains two optional fields:

- `handler`: a handler id, or `null` for the policy's choice.
- `needs`: stream kinds the job must output (`["packet"]`); a handler without them is
  passed over.

The policy, among the mode's `ready` handlers:

1. a handler that lacks an output the start `needs` is passed over. This is how "a
   packet-capable handler while a capture is asked for" is said, and it comes first so that
   a ranking never hides the only handler that can serve the capture;
2. the operator's ranking for that mode, then the operator's node-wide ranking;
3. an in-process handler before a program;
4. the row's own handler, then handler files by id.

(The operator approved the order: ranking, packet-capable while capturing, in-process,
row order. Making the second a filter ahead of the ranking is the builder-facing form of
it: with no capture asked for, the ranking leads as approved.)

No handler left is an error naming each handler and why it was passed over. A named
`handler` that is `unavailable` is an error with its reason; the policy is not consulted.

The ranking is node state, set by a new op and kept in the state directory like the
station: `handler_policy_set { mode: <id> | null, order: [handler ids] }` (`mode: null` is
the node-wide ranking; an empty `order` clears it) and `handler_policy_get`. It belongs with
the other operator choices in `TODO.md`'s "Policies" gist and moves there when that home
exists.

A job's record keeps `handler` (what runs) and whether it was named or chosen. A relaunch
(restart, recovery, retune) keeps the handler. If it is no longer `ready`: a named handler
fails the job with the reason, as a missing program does today; a chosen one is chosen again
by the policy and the job's context notes the change. Sets, watches and identify start their
decoders with `handler: null`.

Every event carries `handler` (the id), as it carries `job` and `source`. A packet capture
records it in its index entry and in each packet's comment (`job=... handler=...`).

### Send to: a sibling beside a running job

`job_start` gains `beside: <job id> | null`. With it, the kind, source and band are the
named job's (the request must give the same, or omit them), and `handler` is required and
must differ from the handler of every running job of that group. The node's "same kind on
the same band is a repeat" rule treats a different handler as a different job. The band is
already held, so the lease is granted without a tune and nothing else is disturbed.

A sibling is an ordinary rigid job with its own lease. What ties siblings is a **group**:
each member's record carries `group`, the id of the job the first Send to named (that job
gets it too). The request field is `beside`; the record field is `group`.

- The group id is only a key. Stopping any member, the first included, leaves the others
  running and in the group; a group of one is an ordinary job that still carries the key.
  After a restart each member resumes or fails on its own.
- `job_retune` of any member moves the whole group, under the source's one gate: the
  members' leases are released and granted together, so they never conflict with each
  other, and the request is a `conflict` only for holders outside the group. Each member
  then relaunches; one that fails is `failed` with its reason and the others stand.
- A member can be captured, transcribed and sent from. Send to from any member adds to
  that member's group.

On the page:

- A job's menu gains **Send to ›**, listing the mode's other `ready` handlers (an
  `unavailable` one is shown greyed with its reason). It is offered on any job whose mode
  has more than one handler: so an OnAir job offers `dump1090` and `builtin` as readily as
  the reverse.
- **Capture packets** on a job whose handler outputs none is no longer left out when
  another handler would give them: it reads `Capture packets via <name>` and starts that
  sibling with `needs: ["packet"]`, then attaches the capture to it.
- The box menu's **Decode ›** entry starts with the policy's choice; a mode with more than
  one ready handler opens a submenu of them.
- Siblings are drawn as one group in the job list, the first job's row with its siblings
  under it, each row showing its handler's name, its events per minute and how many
  subjects it has heard, so a silent decoder shows beside a busy one.

### One merged subject, marked

The page's subject store groups jobs today only for a watch's kept decoders; every other
job is its own group, and a group's subjects are dropped when its job stops. Siblings
become a second kind of store group, keyed on the record's `group`, alive while any member
runs: an aircraft both hear is one subject, one table row and one map marker, and stopping
the first job loses nothing its sibling heard. This holds for every mode with a subject,
whether or not its row declares `merge`.

The marks below also apply in the existing merged view (rows sharing a subject name and
declaring `merge`, so an aircraft heard by ADS-B on one radio and ACARS on another is one
row). There the source of a field is the job's mode and handler together, so a field only
the ACARS job gave is marked as that source's.

The store keeps, per subject field, the latest value **per handler**. From that:

- a field every handler of the group has supplied is drawn plain;
- a field only one handler has supplied (for this subject) is drawn in that handler's mark;
- a text field for which two handlers hold different current values is drawn with the
  `differs` mark, the newest value shown and each handler's value in its title. Numbers
  that change between messages (altitude, position, speed) are never compared: the newest
  shows.

Marks come from the theme: four slots, each a colour and a font style
(`--c-handler-1..4`, `--handler-style-1..4`), and `--c-differs`. The base style gives
defaults, so a theme without them still works. Handlers take slots in the order they joined
the group; `theme.json` may pin a handler id to a slot (`"handlers": {"onair": 2}`). The
marks apply in the Subjects table and the map popup, each marked value's title names its
handler, and the table's header carries a legend. The event feed tags each line with its
handler's name.

### Send to: a stored IQ capture

A burst capture in Explore gains **Send to ›**: the modes whose channel fits the
recording (inside its band and no wider than it; the others under `elsewhere`, as Decode
does), each with its handlers.

A new op, `capture_decode { capture, kind, handler | null }`, starts a job whose graph is
the mode's, rooted on the existing `capture_in` replay stage instead of a radio channel: one
pass (`capture_in` with `loop: false`, which exists) at real-time pace, then the job ends
`stopped` with a note of what it decoded. It replays the capture's whole set, as
`bits_start` does. It holds no source lease and does not resume after a restart.

A replay is marked as one: the job record and each of its events carry `replay: <capture
id>`, and the events carry the recording's own time. Because of the mark, a replay's events
feed no upload or sink (SondeHub), count as no identify proof, and stay out of the live
subject groups and the live map: the page lists a replay's subjects as a group of its own,
titled with the capture. `packets` may be attached to it like any job. A handler that
cannot be fed from a replay (none known) would be refused by name.

### OnAir

`handlers/onair.json` ships in the node. `onair-rx` stays outside the repository
(GPL-3.0-or-later, carrying OnAir's attribution line), installed by the operator and found
on `PATH` or by `ZETA_HANDLER_ONAIR_RX`. Its contract with the handler file:

- `onair-rx --mode <m> --rate <hz> --format cs16|cf32 [--pcap-port <p>]`, IQ on stdin;
- one JSON object per line on stdout for each decoded message, its members named for the
  handler file's `fields` pointers;
- with `--pcap-port`, a classic pcap stream served on that local port for modes OnAir has a
  Wireshark channel for;
- a mode it does not have ends it at once with one line on stderr, which becomes the job's
  failure reason.

Modes arrive in batches, each mode with a `zeta modes check` test against the synthesised
capture its row already has:

1. **ADS-B**, end to end (events, packets, sibling beside dump1090): the acceptance case.
2. **The other rows OnAir shares** (AIS and APRS with packets; ACARS, VDL2, HFDL, POCSAG,
   FLEX, RDS, DMR, P25, M17, the radiosondes, DVB-T, analog video with events).
3. **Modes only OnAir decodes**, as rows with no decoder of their own (Zigbee, Thread,
   Wi-SUN, Bluetooth LE, LoRaWAN, Z-Wave, Wi-Fi, TETRA, GSM-R, LTE, DVB-S IP, ASTERIX).
   See "Open decisions".

Two changes are worth offering upstream to OnAir, each first carried and proven as a patch
in `onair-rx`: its file source reading a pipe (today it sizes, seeks and loops the file, so
its whole engine cannot be fed from stdin and only bare receivers can be wrapped), and
receivers passing sample time to its Wireshark hub (it stamps decode time). OnAir takes
focused pull requests with a test and a DCO sign-off; they would come from the operator's
account.

## Delivery

Four plans, each working software on its own:

1. **Handlers, policy, packets from programs.** Handler files, `modes_list`/`modes_show`,
   `job_start.handler`/`needs`, `handler_policy_*`, the `packets` output with `pcap` and
   `beast`, `LinkType` widened, `mode_s` and the Lua dissector, `handlers/onair.json` with
   ADS-B, `handler` on events and captures. Acceptance: a packet capture of live aircraft
   from dump1090 and from OnAir, opened in Wireshark.
2. **Send to and the merged subject.** `job_start.beside`, the job menu's Send to, the
   capture entry's fallback, the group in the job list, per-handler subject fields, marks
   and theme tokens.
3. **Stored captures.** `capture_decode` and Explore's Send to.
4. **OnAir's other modes**, batches 2 and 3, and the upstream patches.

## What's left to the builder

- The exact Rust shape of a handler entry and how much of `ProgramBlock` it reuses; whether
  rows and handler entries share one validated type.
- `$packet_port` allocation beside `$port`, and the connect/retry behaviour of the packet
  output.
- Whether `LinkType` is a number with a name table or an enum with an `other` variant, and
  its JSON spelling for unnamed numbers; `FrameSummary` and the peers that refuse packets
  follow.
- How the duplicate-job rule and identify's and a watch's trial children read `handler`.
- The page's layout for sibling rows and the legend; the default mark colours per built-in
  theme; the events-per-minute window.
- How the `replay` mark reaches the sinks, identify and the page's stores.
- The wrapper's mode table and each mode's JSON members.
- Whether Beast's sample counter is used for packet time.

## Decided by the operator

- **The data-network modes of batch 3** (Wi-Fi, Bluetooth LE, Zigbee, Thread, Wi-SUN,
  LoRaWAN, Z-Wave, TETRA, GSM-R, LTE, DVB-S IP) carry other people's traffic, where plan
  41's boundary was the frames the node's own decoders produce. Decided 2026-11-21:
  **unrestricted for now, with placeholders for the policy framework.** They come in as
  ordinary rows, decoded, shown and packet-capturable from day one. Each such row carries a
  `policy` placeholder: a field naming the class of content it carries (`"policy":
  {"content": "network_traffic"}`), validated, reported by `modes_list` and copied to the
  row's packet captures, and read by nothing yet. The policy framework (`TODO.md`,
  "Policies" and "Access: ACLs on restricted rows") gates on it when it exists; the
  existing `access: restricted` marker and its rule for captures are unchanged for the rows
  that carry it today.
