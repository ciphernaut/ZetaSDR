# Plan 38: digital voice, P25 phase 1 and DMR, through a vocoder the operator builds

Design spec, 2026-11-14. From the operator's question of the same day ("have we added all of the
digital audio modes yet?"; only M17 has a row) and the conversation that followed, settled as:
the node frames each protocol itself, and the patented speech codec sits behind one external
program that the operator builds on their own machine against `mbelib-neo`. Section 9 records
the calls made here that the operator did not make.

Revised 2026-11-15 on the operator's word: both slots of a DMR channel are decoded at once, as
separate audio streams of one job, each open to `transcribe`, with an audio policy for what the
job's main stream carries (section 5.10).

Builds on decoder wave 2 (plan 29, `docs/superpowers/specs/2026-11-06-decoder-wave2-design.md`:
handlers on graph rows 4.2, what a row's job emits 4.3, the `access` marker 4.7, M17 and its
`c2dec` vocoder 7), on `crates/zeta-dsp/src/m17_decode.rs` and `m17.rs`, on the row machinery in
`crates/zeta-core/src/modes/` and on the synthesiser in `crates/zeta-dsp/src/synth.rs`.

It replaces the shape in the `TODO.md` gist "Build: an install script for the AMBE/IMBE vocoder
libraries" (`codecserver` plus `codecserver-softmbe`): no daemon and no socket protocol, one
small program on the shape of `c2dec`.

## 1. Intent and why

The field craft promises digital voice feeding `transcribe`, and one mode delivers it. M17 is
amateur-only and rare on the air. The digital voice an operator actually meets on VHF and UHF is
DMR (amateur repeaters and hotspots, commercial two-way) and P25 (public-safety and government
networks). Today the node measures both as "fsk, 12.5 kHz" and can say nothing more.

What the operator wants is the M17 experience for these: start a row on the measured centre, see
who is calling whom, hear the speech as ordinary job audio, and attach `transcribe` to it.

The obstacle is the codec. DMR uses AMBE+2 and P25 phase 1 uses IMBE; both are patented and no
open-source SDR application we surveyed ships them without a caveat. Four applications were
compared (SDRangel, SDR--, OnAir, OpenWebRX+). None wraps a whole external decoder such as
`dsd-fme`: all four frame the protocol themselves and treat the vocoder as a separate,
replaceable seat. Two of them leave the codec out of what they ship and have the user build it.
This plan takes that stance.

This is a framework, not two decoders. The vocoder seat, the 4-level FSK front end and the shape
of a voice call's events are built once, so that D-STAR, NXDN, Yaesu System Fusion and dPMR
arrive later as a framing module and a row each.

## 2. Goals, non-goals, anti-goals

### Goals

1. A `p25_decode` row and a `dmr_decode` row. Each reports calls as events and publishes clear
   speech as ordinary job audio, so the page plays it and `job attach --kind transcribe` works
   unchanged.
2. One vocoder seat shared by every voice row: frames into an external program, speech out. M17
   moves onto it with no change in behaviour.
3. `zeta-mbe`, a small program the operator builds with `tools/install-vocoders.sh` against
   `mbelib-neo` at a pinned commit. Nothing patented is in this repository, in any binary built
   from it, or in CI.
4. Calls show without the codec. A host without the vocoder still lists the two new rows as
   ready and reports every call; only the audio is missing, and each call says why on the page.
5. A vocoder that crashes, hangs or is missing never fails the job or the node.
6. Both rows are proved in CI against synthesised captures, with no vocoder present.
7. The legal position is written down once, in `docs/vocoders.md`.
8. A job may have more than one voice. Both slots of a DMR channel are decoded at once, each on
   its own audio stream that can be played and transcribed, and an audio policy says what the
   job's main stream carries: the first call, the first call then the other, or both.

### Non-goals (later plans, each a framing module and a row on this plan's pieces)

- D-STAR, NXDN, Yaesu System Fusion, dPMR, P25 phase 2.
- Trunking: following a control channel and starting a voice job per grant. This plan decodes
  the channel the job is started on. A control channel's own messages (P25 TSBK, DMR CSBK) are
  counted, not interpreted.
- Data: DMR short messages and GPS, P25 packet data.
- FreeDV, DAB, TETRA.
- A hardware AMBE chip in the vocoder seat. The seat must not rule it out (section 5.1).
- Changing the audio policy of a running job. It is set when the job starts (section 5.10).
- Shortening the identify dwell these rows add (section 5.8).

### Anti-goals

- **No codec in the tree.** No vocoder source, table or binary is committed, vendored, fetched
  by `cargo`, or built by CI. `cargo deny` stays as it is: `mbelib-neo` is GPL-2.0-or-later and
  never becomes a dependency of any crate.
- **No decryption.** An encrypted call is reported with its algorithm and key identifiers and
  has no audio. The node accepts no keys, and that includes DMR's basic privacy.
- **No region or radio assumptions.** The rows carry no channel list, no band limits from one
  country's plan and no network's talkgroup names.
- **No second audio mechanism.** Every voice leaves the stage as an ordinary audio stream. A
  job with several voices has several streams, told apart by the role a stage already gives an
  output.

## 3. Constraints

- **Rows.** `crates/zeta-core/src/modes/row.rs`: unknown fields are rejected, so every new field
  below is a schema change with a default that keeps existing and operator rows loading.
- **Stages.** An embedded graph row may name only stages registered by `zeta-dsp`
  (`crates/zeta-dsp/src/lib.rs`, where `M17DecodeStage` is registered).
- **Channel rate.** `channel_rate_hz` gives at least 48 000 Hz, 10 samples per symbol at 4800
  symbols per second.
- **Handlers.** A stage finds its program through the row's `handlers[i].programs` with
  `zeta_core::modes::handlers::resolve`, which honours `ZETA_HANDLER_<PROGRAM>`. Graph params
  never carry a path.
- **Licences.** `deny.toml` forbids copyleft in the workspace. The repository is Apache-2.0 and
  public.
- **Generated guides.** `AGENTS.md` and `skills/zetasdr/SKILL.md` are regenerated, never edited.
- **Public repository.** No real station identifiers in tracked files: test signals use invented
  IDs, and a capture of the operator's own radio stays outside the repository.
- **CI.** Two CPUs. A test that reads state once right after an operation flakes there; poll for
  the end state.

## 4. The two signals (reference for the builder)

These are working notes from memory of the public standards, not a substitute for them. Check
every constant against the standard before relying on it: P25 against TIA-102.BAAA (common air
interface), DMR against ETSI TS 102 361-1 and -2, both freely readable.

### 4.1 P25 phase 1

- C4FM, 4800 symbols per second, deviations ±600 and ±1800 Hz, 12.5 kHz channel. Continuous
  FDMA: no slots.
- Frame sync: 48 bits, `0x5575F5FF77FF`. Then the network identifier: a 12-bit NAC and a 4-bit
  data unit ID, protected by a BCH(63,16) code plus a parity bit.
- A status symbol (one dibit) follows every 35 information dibits and is removed before
  deinterleaving.
- Data units: header (`0x0`), terminator (`0x3`), voice LDU1 (`0x5`), trunking signalling
  (`0x7`), voice LDU2 (`0xA`), packet data (`0xC`), terminator with link control (`0xF`).
- A voice superframe is LDU1 then LDU2, 180 ms each, nine IMBE frames each. LDU1 carries link
  control (group call: talkgroup and source unit; unit-to-unit: destination and source). LDU2
  carries the encryption sync: message indicator, algorithm ID and key ID. Algorithm `0x80` is
  clear speech.
- An IMBE frame on the air is 144 bits: four Golay(23,12) words, three Hamming(15,11) words and
  seven unprotected bits, with a pseudo-random mask derived from the first word, interleaved.

### 4.2 DMR

- 4FSK, 4800 symbols per second, deviations ±648 and ±1944 Hz, 12.5 kHz channel. Two-slot TDMA:
  30 ms per slot. A burst is 264 bits: 108 payload, 48 sync or embedded signalling, 108 payload.
- Between a repeater's bursts sits the 24-bit CACH, whose TACT bits name the slot. A handheld
  transmitting directly sends one burst in every two slots with nothing between.
- Sync patterns (48 bits): repeater voice `0x755FD7DF75F7`, repeater data `0xDFF57D75DF5D`,
  handheld voice `0x7F7D5DD57DFD`, handheld data `0xD5D7F77FD757`. Direct-mode TDMA has its own
  four.
- A voice superframe is six bursts, A to F, 360 ms. Burst A carries the voice sync. Bursts B to
  F carry the EMB (colour code, privacy indicator, a fragment marker) and 32 bits of embedded
  signalling; B to E together carry the link control again, so a receiver joining mid-call has
  it within one superframe.
- Each voice burst carries three AMBE+2 frames of 72 bits: 216 bits around the centre field.
- A data burst has a slot type (colour code and data type, Golay(20,8)) and a BPTC(196,96)
  payload. Data types used here: voice link control header, terminator with link control, and
  the privacy header. Link control: call type (group or unit-to-unit), feature set, service
  options (the encryption and emergency bits), destination and source, 24 bits each.
- Slot timing comes from the sync patterns inside the channel's own sample stream. No absolute
  timestamp is needed, so the outstanding two-hour clock run (plan 14) does not block this.

## 5. The design

### 5.1 The vocoder seat

`crates/zeta-dsp/src/vocoder.rs`, lifted from the `Vocoder` in `m17_decode.rs`. One call's
vocoder is one child process: fixed-size frames on its stdin, 16-bit little-endian speech on its
stdout, resampled to 48 kHz mono and published on the stage's audio output with each frame's
span.

What the stage states about its vocoder when it spawns one. A "feed" is what the stage hands
over at a time, which for M17 is one stream frame holding two codec frames:

| Field | `c2dec` (M17, 3200) | `c2dec` (M17, 1600) | `zeta-mbe` (IMBE) | `zeta-mbe` (AMBE+2) |
|---|---|---|---|---|
| program | row handler 0 | row handler 0 | row handler 0 | row handler 0 |
| arguments | `3200 - -` | `1600 - -` | `imbe7200x4400` | `ambe3600x2450` |
| bytes per feed in | 16 | 8 | 184 | 96 |
| samples per feed out | 320 | 320 | 160 | 160 |
| output rate | 8 kHz | 8 kHz | 8 kHz | 8 kHz |
| context key | `m17.vocoder` | `m17.vocoder` | `p25.vocoder` | `dmr.vocoder` |

The M17 columns are what `m17_decode.rs` does today (`STREAM_PAYLOAD_BYTES`, `C2_FRAME_SAMPLES`,
`codec`); the seat takes these as values, it does not assume them.

Behaviour, all of it M17's today and kept:
- Spawned at the start of a call, with a cleared environment and `kill_on_drop`.
- Its exit mid-call ends that call's audio only. Events keep coming, `job context` says why once
  under the context key, and the next call spawns it again.
- At the end of a call its input is closed and the speech still in flight is published.

New:
- **A watchdog.** A vocoder that has taken frames and returned nothing for two seconds is killed
  and treated as an exit.
- **Absent is not a failure, for a row that says so.** Section 5.3. The program is looked up
  when a call starts, not once when the job starts, so a vocoder installed while a job runs is
  used from the next call.

The seat is a small trait or struct boundary inside `zeta-dsp`: frames in, speech out, a reason
when it stops. A later hardware chip or a different codec program fills the same seat without
touching a framing module.

M17 moves onto it and its tests pass unchanged. Its handler stays required: without `c2dec` the
row is `unavailable` and the stage fails at start, as today.

### 5.2 `zeta-mbe` and the install script

`tools/vocoder/zeta-mbe.c`: one C file, Apache-2.0 like the rest of the repository, that
includes `mbelib-neo`'s header and is linked against it by the install script. It is never
compiled by `cargo` or CI.

- `zeta-mbe <codec>`, with `<codec>` one of `imbe7200x4400`, `ambe3600x2450`. The names are the
  rates, so `ambe3600x2400` (D-STAR) is a later addition here and nowhere else.
- **Input.** One frame is the codec's channel-coded frame after the protocol's own
  deinterleaving, laid out row by row as `mbelib` takes it: 8 rows of 23 for IMBE (184 bytes),
  4 rows of 24 for AMBE+2 (96 bytes). One byte per bit: 0 is a certain zero, 255 a certain one,
  values between are the demodulator's confidence. The codec's own error correction runs inside
  `zeta-mbe`.
- **Output.** 160 samples of signed 16-bit little-endian speech at 8 kHz per frame, written and
  flushed per frame.
- It uses the library's float frame functions and scales the result itself; OnAir found the
  short ones clip loud speech.
- If the pinned `mbelib-neo` has soft-decision entry points it uses them; otherwise it
  thresholds at 128. The wire format does not change either way.
- `zeta-mbe --version` prints its own version and the library's. End of input is a clean exit.

`tools/install-vocoders.sh`, on the shape of OnAir's `build-codecs.sh`:

1. Prints the patent and licence notice (section 5.9) and continues only on a typed `yes`, or
   with `--yes`.
2. Clones `https://github.com/arancormonk/mbelib-neo` at one pinned commit and refuses any
   other. `--src DIR` uses a source tree the operator already has and downloads nothing.
3. Builds it as a static library with `cmake`, then compiles `zeta-mbe.c` against it.
4. Installs `zeta-mbe` under `--prefix` (default `~/.local`, so `~/.local/bin/zeta-mbe`).
5. Runs a self-test: a run of silence frames in, the right number of samples out.
6. Prints what it built, from which commit, where it put it, and how the node finds it (`PATH`,
   or `ZETA_HANDLER_ZETA_MBE=/path/to/zeta-mbe`).

It needs `git`, `cmake` and a C compiler, and says so by package name when one is missing.

### 5.3 Optional handlers: calls without the codec

A handler block gains `optional`, default `false`:

```json
"handlers": [
  { "programs": ["zeta-mbe"], "optional": true,
    "install": { "script": "tools/install-vocoders.sh", "url": "https://github.com/arancormonk/mbelib-neo" } }
]
```

- A row whose only missing handlers are optional is `ready`. `reason` stays `null` for a ready
  row, as its documentation says. `modes_list` and `modes_show` items gain `missing`: a list,
  empty by default, with one entry per optional handler not found, each the program's name and
  its install hint. The startup report line for such a handler ends "voice silent" in place of
  naming the row as unavailable.
- The stage starts without the program and reports every call with no audio.
- `Install` gains `script`, an optional path in this repository. When present the hint leads
  with `bash <script>` (run from a source checkout) ahead of any package name, and keeps the
  URL.
- `modes check` runs such a row without the program. With `--strict` a missing optional handler
  fails the row, so a strict run still proves the real program where it is expected.
  `checked` is recorded only on a pass with every handler found.
- The probe runs at startup and on `reload_modes`, so `missing` empties after an install on one
  of those. The audio does not wait for it (section 5.1).

Only the two new rows use it. **`m17_decode` is left as it is**: making its `c2dec` optional
would put M17's five-second dwell into every identify on a host without `c2dec` and move a
pinned set of CLI tests (`crates/zeta/tests/cli.rs` pins `c2dec` missing and M17
`unavailable`). It is a one-line row change for a later plan. CI keeps installing `codec2` and
its strict M17 check.

**Why a call is silent.** Every event of a call in the two new rows carries `audio` (`true` or
`false`) and `silent`: `null` when `audio` is true, else one of `no_vocoder`, `encrypted`
or `vocoder_failed`. The row's summary
and a subject column show it, so the page says why with no new page code. The context note
under `p25.vocoder` or `dmr.vocoder` carries the detail: the program's name and the fix.

### 5.4 The 4-level FSK front end

`crates/zeta-dsp/src/fsk4.rs`: discriminator, matched filter, symbol timing and level tracking
for 4-level FSK, giving soft dibits with a sample position each. Parameters: symbol rate,
deviation of the outer symbols, and the pulse shape (P25's C4FM filter, DMR's root raised
cosine). 2400 symbols per second must work, for NXDN and dPMR later.

`M17Demod` (in `m17_decode.rs`) has this built in. Moving M17 onto `fsk4.rs` is welcome if its tests stay
green and is not required.

Frame sync is by correlation against the sync pattern on soft symbols, tolerating a few symbol
errors, with both polarities tried (an inverted discriminator is common).

### 5.5 The framing modules and stages

Two library modules and two stages in `zeta-dsp`, on M17's shape (a library module that turns
IQ into frames, a stage that turns frames into events and vocoder input):

- `p25.rs` and `p25_decode.rs`, stage kind `p25_decode`.
- `dmr.rs` and `dmr_decode.rs`, stage kind `dmr_decode`.

The P25 stage takes `iq` and gives `event` and `audio`, as `m17_decode` does. The DMR stage
gives `event` and three audio streams (section 5.10).

**P25.** Sync, the network identifier with its BCH decode, status-symbol removal, then by data
unit: header, LDU1 (link control, nine IMBE frames), LDU2 (encryption sync, nine IMBE frames),
terminators. Trunking signalling and packet data units are counted in `job context` and not
parsed.

**DMR.** Sync for repeater and handheld, voice and data; the slot from the CACH when there is
one and from burst timing when there is not; the EMB and embedded link control; the slot type
and BPTC for the three data types of section 4.2; three AMBE+2 frames per voice burst.

**Two DMR calls at once.** A repeater can carry a call in each slot. Both are reported and
both are decoded, each by its own vocoder, each on its own audio stream. Section 5.10 says which
of them the job's main stream carries.

**A channel with no call.** A P25 control channel and an idle or control-only DMR repeater
transmit continuously and never carry a call. Each stage gives one `type: "channel"` event when
it first decodes the network field with no call in progress, and again only if that field
changes: `protocol`, `nac` or `color_code`, and `control: true` once trunking signalling units
have been seen. It has no `src`, so it makes no row in the subjects table. This is what lets
identify prove the row on such a signal.

**A call's events.** `type: "call"`. Two per call, as M17 gives two per stream:
- One when the call's link control is first in hand: at its header, or within one superframe
  when the job joins mid-call.
- One at its end, with `eot: true`: at its terminator, or when sync is lost for longer than one
  superframe.

Shared fields, the same names in every voice row added from here on. M17's events keep their
own names (`encryption`, `frame`, string callsigns) and gain nothing:

| Field | Meaning |
|---|---|
| `protocol` | `p25` or `dmr` |
| `src` | the calling radio's ID, an integer |
| `dst` | the talkgroup or the called radio's ID, an integer |
| `call` | `group` or `private` |
| `encrypted` | `true` or `false` |
| `emergency` | `true` or `false` |
| `type` | `call` or `channel` |
| `audio`, `silent` | section 5.3 |
| `stream` | the role of the audio stream carrying the call's speech (`slot1`, `slot2`); `null` in a stage with one voice |
| `program` | whether the call's speech is on the job's main stream: at this moment on the opening event, at any time on the closing one |
| `frames` | voice frames received so far |
| `errors` | bit errors the protocol's own codes corrected so far |
| `eot` | `true` on the closing event |

Per protocol:
- P25: `nac` (integer), `mfid`, `algid`, `kid`.
- DMR: `color_code`, `slot` (1 or 2; `null` when it cannot be told), `origin` (`repeater` or
  `handheld`), `fid`.

An encrypted call (P25: algorithm not `0x80`; DMR: the privacy bit in the service options or a
privacy header) gets both events and no vocoder.

The page keys its play and Transcribe buttons on the row's static `outputs`, so they show on
these jobs whether or not a call is audible.

### 5.6 The rows

`modes/p25_decode.json` and `modes/dmr_decode.json`, on the shape of `m17_decode.json`:

- `family`: `two_way`. `band.bandwidth_hz`: 12500. No `centers_hz`, no `range_hz`.
- `decoder`: a graph, `channel → p25_decode` or `channel → dmr_decode`, with the optional
  `zeta-mbe` handler of section 5.3. The DMR row's stage takes `"audio": "$audio_policy"`
  (section 5.10).
- `subject`: keyed on `src`, named `radios`, with columns for `dst`, `call`, `encrypted`,
  `silent` and the network field (`nac` or `color_code`).
- `summary`: who is calling whom, the call type, and "encrypted" when it is.
- `position`: none.
- `identify`: proof by event, one event of either type, a dwell of 2000 ms (a mid-call join has
  its link control within 360 ms, a channel event comes sooner), no quick attempt.
- `catalogue`: the existing entries `project-25-p25` and `digital-mobile-radio-dmr`.
- `access`: `restricted`. The marker reaches only the row's own events today
  (`summary::envelope`); the audio and an attached `transcribe` job's transcripts carry nothing.
  This plan adds a line saying so to the `TODO.md` gist "Access: ACLs on restricted rows" and
  does not widen the marker.
- `notes`: one paragraph each for the generated guide, covering the start command, the events,
  the audio and the vocoder, in `m17_decode`'s words where they apply.

### 5.7 The synthesiser and the tests

Two test families in `synth.rs`, `p25` and `dmr`, each building a short clear group call from
given IDs: header, a few voice superframes, terminator. The voice frames carry the codec's
well-known silence frame, already channel-coded, so the synthesiser needs no codec. Every other
field is encoded by the synthesiser's own implementation of the protocol's codes.

Row tests (`zeta modes check`, CI): each row decodes its family and gives the expected `src`,
`dst` and network field. Invented IDs only.

Unit and stage tests:
- Each protocol's codes round-trip and correct the number of errors they should.
- A capture with bit errors below the codes' limits still gives the call; one joined mid-call
  gives the opening event within one superframe.
- An encrypted call gives events and no vocoder is spawned.
- DMR: a repeater capture with a call in each slot gives both calls, each slot's speech on its
  own stream, and the main stream as each of the three policies says.
- Voices: an attached `transcribe` reads the stream it names; an unknown name is refused; an
  audio policy on a row with one voice is refused; a restart resumes both.
- **A fake vocoder**, a test program reached through `ZETA_HANDLER_ZETA_MBE`: audio frames
  arrive at 48 kHz with the right spans; a fake that exits mid-call ends that call's audio and
  writes the context note; a fake that hangs is killed by the watchdog; no program at all gives
  events, `audio: false`, `silent: "no_vocoder"` and the note. These poll for the end state.
- A capture of a control channel with no call gives one channel event, and identify proves the
  row on it.
- Optional handlers: a row with one missing is `ready` with `missing` filled; `modes check`
  passes it and `--strict` fails it.
- M17's existing tests, unchanged.

**An independent check, on a developer's machine only.** The synthesiser and the decoder are
written from the same reading of the standards, so they can agree and both be wrong. Before the
rows are called done, `dsd-fme` (or another established decoder) must decode the synthesised P25
and DMR captures and report the same IDs. This is a manual step recorded in the plan's outcome,
not a CI gate and not a dependency.

### 5.8 What changes around the rows

- **Identify.** Both rows fit any 12.5 kHz FM or FSK signal, as `m17_decode` does, so each adds
  a dwell of up to 2 s to identifying such a signal, 4 s together, on every host (the rows are
  ready with or without the vocoder). That grows the cost already recorded in
  `TODO.md` ("Identify dwell cost"). This plan accepts it and adds a line to that gist.
- **The page.** The subjects table follows the row's `subject`, and the reason a call is silent
  arrives in the event's summary and the `silent` column, with no page code. The one page
  change is the stream picker of section 5.10. Acceptance checks all of it on the page
  (section 7).
- **Field craft.** "Decoding digital voice" in `docs/agents/field-craft.md` is rewritten to
  cover the three rows, the vocoder, calls without it, and encrypted calls. The third-party
  paragraph gains one sentence: these rows decode clear speech on public protocols; whose
  traffic it is, and whether listening to it is lawful where the node sits, is the operator's
  responsibility, said once when a request goes that way.
- **Generated guides.** `AGENTS.md` and the skill are regenerated.
- **`TODO.md`.** The install-script gist is replaced by a pointer to this plan; "DMR voice" and
  "A `vocoder` handler" are struck; the claim that DMR needs true block timestamps is corrected.

### 5.9 The legal note

`docs/vocoders.md`, once, in plain words:

- AMBE and IMBE are patented. This project ships no implementation of them and builds none.
- `tools/install-vocoders.sh` downloads `mbelib-neo` from its authors, under their licence
  (GPL-2.0-or-later), and builds it on your machine at your request. The program it produces is
  yours and is not to be redistributed with this project.
- Whether you may build or use it where you live is your responsibility.
- Encrypted calls are never decoded.
- Without the vocoder everything except the speech still works.

The install script prints the same points before it asks for `yes`.

### 5.10 Voices: a job with more than one audio stream

A stage already gives each of its outputs an optional role (`Stage::output_roles`), the stream
list shows it, and both the page's player and `job_attach` take the job's audio stream that has
no role. This plan builds on exactly that.

**The streams.** A stage with several voices publishes:
- one audio stream with no role, the job's **main stream**. It is what the play button, the
  mix and a plain `job_attach` read today, and they keep reading it.
- one audio stream per voice, with the voice's role. `dmr_decode` has `slot1` and `slot2`. A
  call whose slot cannot be told (a handheld transmitting directly) uses `slot1`.

Every clear call is decoded onto its voice's stream whenever a vocoder is present, whatever the
policy. A stage states its voices through `Stage::voice_roles`, empty by default, and
`modes_list` items gain `voices`: the list, empty for a row with one voice.

**The audio policy** says what the main stream carries. It is a parameter of the stage:

| Policy | The main stream carries |
|---|---|
| `first_active` | the call that started first, until it ends. A call that began while another held the main stream never joins it. The default. |
| `follow` | the call that started first; when it ends and another is still going, that one, from that moment. |
| `all` | every call, mixed. |

`job_start` gains `audio_policy`, `null` for the row's default, and the CLI `--audio-policy`.
The row's template names it as `"$audio_policy"`, a placeholder the expander fills as it fills
`$center_hz`. On a row whose template does not name it, a policy other than `null` is refused:
the mode has one voice. The job's record keeps it, so a restart or a retune resumes the same
policy.

**Transcribe.** `job_attach` gains `stream`, `null` for the main stream as today or a voice's
role, and the CLI `--stream`. A role the job does not have is refused naming the roles it has.
Transcribing both slots is two attached jobs. The attached job's record keeps `stream`, each
transcript event and each `transcripts.log` line carries it, and a restart resumes it.

**The page.** A job whose row lists `voices` gets a small picker beside its play button: main,
then each voice. Play plays the picked stream, and Transcribe attaches to it. A transcribe job
attached to a voice shows the voice's name beside its kind.

**The mix** is unchanged: it reads each job's main stream.

## 6. Order of work

1. The vocoder seat, with M17 moved onto it; optional handlers; the fake-vocoder tests.
2. `zeta-mbe.c`, the install script, `docs/vocoders.md`.
3. The 4-level FSK front end.
4. P25 phase 1: module, stage, synthesiser family, row.
5. Voices in the node: `voices`, `job_attach`'s `stream`, `job_start`'s `audio_policy`.
6. DMR: module, stage, synthesiser family, row.
7. The page's stream picker.
8. Field craft, generated guides, `TODO.md`, `UAT.md`.

Steps 1, 2 and 5 are independent of each other; 4 and 6 are independent once 3 exists.

## 7. Proof

In CI, with no vocoder: sections 5.7's row tests and stage tests, and the usual gates.

On a developer's machine: the independent check of section 5.7, and the install script run end
to end with its self-test.

Live, as `UAT.md` checks:
1. The install script on the field host: a job already running gains audio from its next
   call, and after a restart both rows list nothing under `missing`.
2. DMR from the operator's own handheld, transmitting in the 70 cm amateur band into a dummy
   load or at low power: the call appears on the page with its IDs, the speech plays, and
   `transcribe` gives the words spoken. The capture is kept as a fixture outside the repository.
3. The same with the vocoder moved aside: the call appears on the page, no audio, and its line
   says `no_vocoder`.
4. An on-air DMR repeater with a call in each slot: both are listed; the picker plays either;
   a `transcribe` attached to each slot gives two transcripts; and a job started with each
   policy in turn makes the main stream do what the policy says.
5. P25 phase 1 on the air, where there is clear traffic to hear: NAC, talkgroup, speech. An
   encrypted call: reported, silent.
6. M17 still decodes as before.

## 8. Left to the builder

- The exact trait or struct boundary of the vocoder seat.
- Whether M17 moves onto `fsk4.rs`.
- Timing recovery and level tracking methods in `fsk4.rs`.
- The pinned `mbelib-neo` commit, checked against an upstream release tag with `git ls-remote`,
  and whether its soft-decision entry points are used.
- Sync correlation thresholds, and how long a lost sync is tolerated inside one superframe.
- The summary wording and column labels of the rows.
- How the fake vocoder is built for the tests (a test binary or a script).
- The names of the counters in `job context` for the data units that are not parsed.

## 9. Calls made here that the operator did not make

1. **`zeta-mbe` is C, not Rust.** The library is C and needs `cmake` and a C compiler anyway; a
   single C file avoids FFI bindings for its structures.
2. **A process per call**, as M17 does, not one long-lived process per job.
3. **Soft bits on the vocoder's input**, one byte per bit, whether or not the library uses them
   yet. SDRangel credits soft decisions for its weak-signal audio.
4. **M17 is left alone.** It moves onto the shared seat and nothing else about it changes: its
   `c2dec` stays required and its events keep their names (section 5.3 says why).
5. **`first_active` is the default audio policy.** The operator named the three policies
   (2026-11-15) and listed it first; the default is this plan's reading of that.
6. **`access: restricted`** on both new rows.
7. **No band limits on the rows**, at the cost of more identify dwell.
8. **The clock run does not block DMR** (section 4.2). An earlier answer in the conversation
   said it did.
9. **Build order P25 then DMR**, as agreed, though the only on-air proof the operator can make
   at will is DMR (section 7, check 2). The two are independent after step 3 and may swap.
10. **`mbelib-neo` is GPL-2.0-or-later.** It is linked only into `zeta-mbe`, on the operator's
    machine, and reached over a pipe. The wave 2 ruling against copyleft was about the
    workspace's dependencies, which this leaves untouched; it is recorded here because the
    operator chose the library before its licence was stated.

11. **A channel event for a signal with no call** (section 5.5), so identify can prove a
    control channel or an idle repeater.
12. **`silent` on every call event**, as the way the page says why there is no audio, in place
    of new page code.
13. **`modes_list` gains `missing`**, a new public field, in place of overloading `reason`.
14. **`--strict` fails a row whose optional handler is missing.**

15. **The policy is set when the job starts**, not changed while it runs.
16. **A handheld's call, with no slot to read, uses the `slot1` stream.**
17. **The page gets a stream picker and no policy control**: a policy is given at `job start`
    from the CLI, the agent or a set.
18. **`modes_list` gains `voices`**, a second new public field.

## 10. Set aside

`blip25-vocoder` (a Rust crate, MIT, IMBE and half-rate AMBE+2, soft decisions, used by SDR--)
would fill the seat in-process with no install script. It was read on 2026-11-14 and set aside:
its own `PATENT_NOTICE.md` and `ATTRIBUTION.md` say its codec core was recovered from a compiled
vendor binary, and name an active patent on the half-rate codec running to 2028-05-20. Revisit
when that position is clearer. The vocoder seat is the place it would go.
