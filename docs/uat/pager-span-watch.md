# Acceptance test: the pager span watch

A handout for one tester, with one radio, for one hour. You do not need to know ZetaSDR to follow
it. You do need to be comfortable with a Linux terminal, and to know roughly where pagers sit on
VHF in your area.

## Who this is for, and what it proves

This handout is for a radio amateur who lives somewhere with more pager traffic than the
developers have. Our own test radio, near Brisbane Airport, sits on a quiet pager band, so a whole
feature has never had a busy band to work against. You run one hour on your band and send back a
filled-in results block (counts only, see [Privacy and the law](#privacy-and-the-law)).

The feature under test is the **span watch**. It is one job that watches a slice of spectrum about
1 MHz wide. Each time a pager carrier appears in the slice, the watch starts a POCSAG decoder on
that carrier. It keeps the decoder through the gaps between pages and stops it once the carrier
has been gone for a long time. It never runs more decoders than a set cap. The pages it decodes
fill a table keyed by pager address, and every page is marked `access: "restricted"`. The test
shows whether all of that holds on a real, busy band: decoders start quickly, do not churn, stay
under the cap, and actually decode.

## What you need

**Hardware**

- An SDR that SoapySDR supports and that can capture at least 1 MHz: an RTL-SDR dongle (one with
  a TCXO is best), a HackRF, an Airspy or a USRP. The project is run day to day on a USRP B210 and
  a HackRF. Other SoapySDR devices should work but have been tested less, so tell us which device
  you used.
- An antenna for VHF high band, about 138-174 MHz. A 2 m (144 MHz) vertical or a discone is fine.
  If you also want to try the US/Canada 929-932 MHz pager band, use something that works there as
  well (a discone, or a 900 MHz whip).
- A Linux PC (Debian or Ubuntu commands are given below) with a web browser, and about 5 GB of
  free disk for the build.

**Packages** (Debian/Ubuntu names):

```bash
sudo apt install build-essential pkg-config git jq \
    libsoapysdr-dev soapysdr-tools soapysdr-module-rtlsdr
```

Swap `soapysdr-module-rtlsdr` for the module that matches your device: `soapysdr-module-hackrf`,
`soapysdr-module-airspy` or `soapysdr-module-uhd`. `jq` is only used to count things in the
node's output. `multimon-ng` is needed only for the optional FLEX part (see
[US 929-932 MHz and FLEX](#optional-us-929-932-mhz-and-flex)).

Rust: install it with [rustup](https://rustup.rs). The repository pins its own toolchain, so
rustup downloads the right version on the first build.

**A build.** There are no prebuilt binaries yet, so you build from source. Named sets and the span
watch might not have reached `main` when you read this. If you were sent a branch or revision
name, check it out before you build:

```bash
git clone https://github.com/ciphernaut/ZetaSDR
cd ZetaSDR
git checkout <the branch or revision you were sent>     # skip if told to use main
RUSTFLAGS="--cfg soapy" cargo build --release -p zeta
```

The first build takes several minutes. The `--cfg soapy` flag is what links SoapySDR. Without it,
the build succeeds but cannot open a radio.

Check the build and the radio:

```bash
./target/release/zeta soapy find          # your device should be listed
./target/release/zeta sets                # must list "pagers_au"; if not, the build is too old
```

## Find your local pager channels

You need a slice about 1 MHz wide that holds the pager carriers you can hear. The ranges below
are **starting points only**. Allocations change and vary within a country. Check a local band
plan or frequency database, then look at the waterfall for yourself (next section).

| Where | Where to start looking | Notes |
|---|---|---|
| Australia | 148.0-149.0 MHz | The built-in `pagers_au` set covers exactly this range. |
| United Kingdom | about 153.0-153.5 MHz | Commercial POCSAG. Some emergency-service paging is elsewhere. |
| Europe (several countries) | about 466.0-466.3 MHz | POCSAG. This is UHF, so a VHF antenna is weaker here. |
| Netherlands | 169.650 MHz | Emergency paging (P2000). It is **FLEX**, not POCSAG (see the optional FLEX part). |
| United States / Canada | 152-153 MHz, 157-159 MHz, and 929-932 MHz | VHF hospital and private paging, and the 900 MHz networks. Many 900 MHz carriers are FLEX. |
| New Zealand, others | check a local band plan | We have no reliable starting point. |

POCSAG is the protocol under test. FLEX carriers show up in the same bands. The POCSAG decoder
will not decode a FLEX carrier, and that is expected: it is not a failure.

### Look at the waterfall first

Start the node (next section) with nothing else running, open the page, choose your radio's tab
and tune its view to the range above. Pager carriers are easy to spot. Each is a short, bright,
solid block about 10-20 kHz wide that appears for a second or a few seconds, again and again on
the same frequency. Write down where they are. Then pick a **centre frequency** for the watch:

- Put all the carriers you want inside centre ± 450 kHz. The outer 5 % or so of the capture at
  each edge is ignored, so a carrier right at the edge is missed.
- Keep every carrier you care about **more than 25 kHz from the centre**. The radio's DC spike
  sits at the centre, so the watch ignores ±25 kHz around it.
- If your radio has a steady spur (a thin, constant line that never changes) inside the slice,
  either put the centre on it or list it as a spur (see [Troubleshooting](#troubleshooting)).

## The test

You will use two terminals. Terminal 1 runs the node; terminal 2 sends it commands.

### 1. Start the node (terminal 1)

```bash
cd ZetaSDR
export ZETA_SECRET=pick-a-long-random-secret
./target/release/zeta --state-dir "$HOME/zeta-uat" \
    --source-soapy sdr=driver=rtlsdr,gain=30 \
    serve --listen 127.0.0.1:7979
```

- `sdr` is the name this handout gives the radio. Keep it.
- `driver=rtlsdr` names the device. Use `driver=hackrf`, `driver=airspy` or `driver=uhd` for
  yours; `zeta soapy find` shows what to write. If you have more than one device, add `serial=...`.
- `gain=30` is a starting gain in dB. You can change it live later.
- `--state-dir "$HOME/zeta-uat"` keeps everything from this test in one folder, so it is easy to
  delete afterwards. This matters for privacy.
- If your dongle's frequency is known to be off, add `ppm=N` to correct it. The decoders centre on
  the signal they measure, so a small error does not stop decoding. It only shifts the
  frequencies you report.

Leave the node running. Ctrl-C stops it.

### 2. Open the page

Go to <http://127.0.0.1:7979/ui/>, paste the secret, and choose the `sdr` tab. Keep the page open
for the whole hour.

### 3. Point terminal 2 at the node

```bash
cd ZetaSDR
export ZETA_NODE=http://127.0.0.1:7979 ZETA_SECRET=pick-a-long-random-secret
Z=./target/release/zeta
$Z source list            # "sdr" should be there, with no fault
```

If the band looks dead, raise the gain a little: `$Z source gain --source sdr --db 35`.

### 4a. Australia: start the built-in set

```bash
$Z set start --set pagers_au --radio radio=sdr
```

This starts one watch over 148.0-149.0 MHz (centre 148.500 MHz) with the policy described in the
next step. The answer lists the watch under `started`, with its job id, and `refused` is empty:

```json
{"result":"set_start","value":{"set":"pagers_au","radios":{"radio":{"source":"sdr","capture":{"center_hz":148500000,"bandwidth_hz":1024000}}},"started":[{"member":"watch","channel":null,"center_hz":148500000,"job":"01M6...","attached":null,"attach_error":null}],"existing":[],"refused":[],"warnings":[]}}
```

If it shows a `conflict`, something else already holds the radio: stop it, or ask us.

From revision `79ff0826` on, `pagers_au` keeps its 148.500 MHz centre (on the developers' radio a
steady spur sits exactly there, and the centre's DC mask hides it), and a narrow carrier that
the detector reads at its 6 kHz width floor now fits POCSAG and gets a decoder. Before that
revision such a carrier was skipped with `no listed mode fits 6 kHz`.

### 4b. Anywhere else: your own copy of the set

Write a set file with the same policy, but with your centre in place of 148.500 MHz. The node reads
the folder at start-up, so write the file first and then restart the node in terminal 1. Here is an
example with a UK-style centre of 153.250 MHz:

```bash
mkdir -p ~/.config/zetasdr/sets
cat > ~/.config/zetasdr/sets/pagers_local.json <<'EOF'
{
  "id": "pagers_local",
  "name": "Local POCSAG pagers",
  "notes": "Acceptance test copy of pagers_au on the local pager band.",
  "radios": {
    "radio": {"note": "a radio that captures the local pager band (1 MHz)"}
  },
  "members": [
    {"id": "watch", "radio": "radio", "op": "job_start", "kind": "watch",
     "center_hz": 153250000, "bandwidth_hz": 1000000, "deviation_hz": null, "sideband": null,
     "params": {"modes": ["pocsag_decode"], "persist_ms": 500, "width_hz": [6000, 25000],
                "gone_ms": 60000, "hold_ms": 600000, "cooldown_ms": 120000, "confirm_ms": 180000}}
  ]
}
EOF
```

Restart the node (Ctrl-C in terminal 1, then the same `serve` command again). Then:

```bash
$Z sets | jq -r '.value[] | "\(.id) \(.state) \(.reason // "")"'   # pagers_local must be "ready"
$Z set start --set pagers_local --radio radio=sdr
```

Change only `center_hz` (in Hz) and, if you need to, `bandwidth_hz` (up to 2000000 if your radio
captures that much). Leave the `params` exactly as they are: they are the policy under test.

- `modes` lists the decoder the watch may start (POCSAG).
- `persist_ms` 500 means a carrier must be seen for 0.5 s before it counts.
- `width_hz` is the range of carrier widths, 6-25 kHz, that the watch considers at all. A carrier
  whose measured width is 0.5x to 2x of POCSAG's 12.5 kHz channel gets a decoder. A carrier read
  at the 6 kHz floor itself (up to about 6.6 kHz) fits from 0.4x, so it gets a decoder too: a
  clean FSK pager often reads that narrow.
- `hold_ms` 600000 keeps a decoder through up to 10 minutes of silence once its carrier is gone.
- `cooldown_ms` 120000 rests a channel for 2 minutes after its decoder is dropped.
- `confirm_ms` 180000 drops a decoder that decodes nothing in 3 minutes while its carrier is
  present.
- The cap (`max_decoders`) is left at its default, 16.

If the set file is refused (`broken`), send us the `reason` line. Do not work round it.

### 5. Find the watch's id

```bash
W=$($Z job list | jq -r '.value[] | select(.kind=="watch" and .state.status=="running") | .id')
echo "$W"            # one job id, such as 01M6ZKW9ZBJJ1CPEGZYQBNFFYY
```

The same id is the `job` under `started` in the `set start` answer. If `echo` prints nothing,
the watch is not running: look at `$Z job list | jq -r '.value[] | "\(.id) \(.kind) \(.state.status)"'`.

On the page, the watch appears as an outline over your slice. Its decoders appear as smaller
outlines on each carrier, folded under the watch's row in the jobs list.

### 6. Run for 60 minutes and take a snapshot every 10 minutes

Paste this block into terminal 2 once at the start (minute 0), then again at 10, 20, 30, 40, 50
and 60 minutes. The event counts are running totals since the start. None of these commands print
message text or addresses.

The event counts read the node's event log straight from its state directory, with
`zeta events tail`. That command is local: it must run on the machine that runs the node, with
the same `--state-dir` as `serve`, and it refuses to run while `ZETA_NODE` is set
(`events tail reads the state directory: not supported with --node`). The function `L` below
does both. Define it once in terminal 2:

```bash
L() { env -u ZETA_NODE "$Z" --state-dir "$HOME/zeta-uat" events tail --lines 1000000 "$@"; }
```

Then the snapshot block:

```bash
date +%H:%M
# The watch: decoders now and waiting against the cap, channels in cooldown, carriers tracked
$Z job context --job "$W" | jq -c '.value.watch | {cap, cooled: (.cooled | length), signals: (.signals | length)}'
# POCSAG decoders running now (must never exceed 16; a decoder you started by hand counts too)
$Z job list | jq '[.value[] | select(.kind=="pocsag_decode" and .state.status=="running")] | length'
# Events so far
echo "watch events (state, via/why):"; L --job "$W" --protocol watch | jq -r '"\(.event.state) \(.event.via // "") \(.event.why // "")"' | sort | uniq -c
echo "pages per channel (MHz, kind):"; L --protocol pocsag | jq -r '"\(.center_hz / 1e3 | round / 1e3) \(.event.kind)"' | sort | uniq -c
echo "access marker on pages:"; L --protocol pocsag | jq -r '.event.access // "MISSING"' | sort | uniq -c
echo "distinct addresses heard:"; L --protocol pocsag | jq -r '.event.address' | sort -u | wc -l
```

On a quiet band (no carrier seen yet) a snapshot reads:

```text
17:48
{"cap":{"decoding":0,"max":16,"waiting":0},"cooled":0,"signals":0}
0
watch events (state, via/why):
pages per channel (MHz, kind):
access marker on pages:
distinct addresses heard:
0
```

On a busy band the lists fill in, for example (the numbers are made up):

```text
{"cap":{"decoding":3,"max":16,"waiting":0},"cooled":1,"signals":4}
3
watch events (state, via/why):
      4 decoding modes
      1 lost  gone and silent
pages per channel (MHz, kind):
     12 148.638 alpha
      5 148.638 tone
access marker on pages:
     17 restricted
distinct addresses heard:
9
```

`cap.decoding` is the number of decoders the watch runs now, `cap.waiting` the carriers held back
by the cap, `cooled` the channels resting after a drop and `signals` the carriers the watch is
tracking. The watch and signal lines are kept to this watch with `--job "$W"`. The page lines
come from the decoders, each a job of its own, so they are counted over the whole log: run the
test in a fresh `--state-dir`, as step 1 does. A page line's `center_hz` is its decoder's
channel, rounded here to the kHz.

These two are also useful, at any time:

```bash
# What the watch did, in time order (no message content: frequencies, widths, reasons)
L --job "$W" --protocol watch | jq -r '"\(.t_ns / 1e9 | floor | strflocaltime("%H:%M:%S")) \(.event.summary)"' | tail -30
# Carriers the detector saw appear, with the width it measured
L --job "$W" --protocol signal | jq -r 'select(.event.state=="new") | "\(.t_ns / 1e9 | floor | strflocaltime("%H:%M:%S")) new \(.event.center_hz / 1e6) MHz \(.event.bandwidth_hz / 1000) kHz"' | tail -30
```

They print lines like these:

```text
16:43:38 decoding pocsag_decode at 148.6376 MHz on sdr (capture, modes)
16:43:38 new 148.637612 MHz 6 kHz
```

To time how fast a decoder starts, compare a carrier's first `new` line with the watch's
`decoding ... at <that MHz>` line.

Look at the page now and then, without screenshotting message text:

- Each carrier that is paging should have its own decoder outline.
- The jobs list should fold the decoders under the watch's row (under the set's row, if you used
  a set).
- The **Subjects** tab should show **one** table titled by the watch, with a **Channel** column and
  one row per pager address. The page drops an address that has been silent for 5 minutes, so the
  table holds fewer rows than the "distinct addresses" count.

### 7. Stop

```bash
$Z set stop --set pagers_au        # or pagers_local
$Z job list | jq '[.value[] | select((.kind=="pocsag_decode" or .kind=="watch") and .state.status=="running")] | length'   # must be 0
```

`set stop` answers with the watch's id under `stopped` and nothing under `failed`:

```json
{"result":"set_stopped","value":{"set":"pagers_au","stopped":["01M6..."],"ended":[],"failed":[]}}
```

The watch stops its own decoders with it. A decoder you started by hand is not part of the set:
stop it with `$Z job stop --job "$D"` first, or the count above stays at 1.

Then Ctrl-C the node, close the page, and do the clean-up under
[Privacy and the law](#privacy-and-the-law).

## Pass criteria

| # | Check | Pass when |
|---|---|---|
| 1 | Start | `set start` lists the watch under `started`, with nothing `refused` and no `conflict`. |
| 2 | Decoders start on active carriers | Every carrier you can see paging on the waterfall has a decoder within 5 minutes. The design aims for a couple of seconds after a carrier is first seen. Report the delay you measured on at least one carrier. |
| 3 | Direct start | The watch's `decoding` events read `decoding modes` in the snapshot (their summary ends `(capture, modes)`). There are few or no `identifying` events. |
| 4 | Pages decode | At least one channel has POCSAG pages counted. FLEX-only channels decoding nothing is expected. |
| 5 | No churn | Over the hour, each channel's decoder is started only a few times. Report the totals: `decoding` started against `lost` by `why`. A channel that both decoded pages and was dropped `unproven` is a finding. |
| 6 | Hold | A channel that pages every few minutes keeps one decoder all hour. It is not dropped "gone and silent" between pages. |
| 7 | Lost and cooled | A carrier that stops for good is dropped `gone and silent` about 10 minutes after it goes. It is listed in `cooled` for about 2 minutes and can start again after that. Report it if you saw it. |
| 8 | Cap | `cap.decoding` never exceeds `cap.max` (16), and neither does the running `pocsag_decode` count. With more carriers than the cap, `waiting` counts the extra ones. |
| 9 | Subjects | One pager table titled by the watch, with a Channel column and rows keyed by address. |
| 10 | Access marker | Every page line reads `restricted`, and `MISSING` stays at 0. |
| 11 | No phantom decoder | No decoder runs for long on a steady line that never pages (a spur or the centre). |
| 12 | Stop | After `set stop`, no watch or `pocsag_decode` is running. |

The thresholds in 5 and 7 are our best guesses. The counts you report matter more than a yes/no.

## Privacy and the law

Pager messages are personal, and often medical. In many countries (Australia among them) it is
an offence to record, keep, use or tell anyone what is in them, even when receiving them is
allowed. Check your local law before running this test.

- Report **counts and kinds only**: pages per channel, decoders started and lost, the numbers in
  the template below. **Never** report message text or pager addresses, and never send a
  screenshot of the Subjects tab or of a decoder's ribbons.
- The commands in this handout print only counts, frequencies and reasons. Do not run
  `zeta events tail` without the `jq` filters shown.
- After the test, stop the node and delete everything it kept, including the log of decoded
  pages:

  ```bash
  rm -rf "$HOME/zeta-uat"
  ```

  If you made `~/.config/zetasdr/sets/pagers_local.json`, keep it or delete it: it holds no
  messages.

## Results to send back

Copy this block, fill it in, and send it back. Counts only.

```text
ZetaSDR pager span watch: acceptance results

Tester region (country / state, no street address):
Date and local start time:
Build (git rev-parse --short HEAD):
Radio (make/model, SoapySDR driver):           Antenna:
Gain used (dB):          ppm correction (if any):
Set used: pagers_au | pagers_local          centre_hz:          bandwidth_hz:
Carriers seen paging on the waterfall before the test (MHz, approx.):

Snapshot  time   cap.decoding  cap.waiting  cooled  pocsag jobs running
  0 min
 10 min
 20 min
 30 min
 40 min
 50 min
 60 min

Totals at 60 min:
  watch decoding events:
  watch lost events by why  (gone and silent / not seen again / unproven / other):
  watch waiting events:
  watch unknown events ("no listed mode fits"), with the widths they name:
  pages per channel (MHz: count, kinds):
  access marker: restricted = ____   MISSING = ____
  distinct addresses heard (a count only):

Fastest / typical delay from a carrier's first "new" to its "decoding" (s):
Widths the watch measured for pager carriers (kHz, range):
Carriers you saw paging that never got a decoder (MHz):
Phantom decoders on a steady line or spur (MHz):

Pass criteria 1-12 (pass / fail / not seen), with a note for each fail:
  1  2  3  4  5  6  7  8  9  10  11  12

Page: decoders folded under the watch (and the watch under the set)? yes / no
Subjects tab: one table, titled by the watch, Channel column? yes / no

Anything else (errors in terminal 1, crashes, odd behaviour):

Log folder deleted after the test (rm -rf ~/zeta-uat): yes / no
```

## Troubleshooting

**Nothing is detected, although the waterfall shows pages.**

- Gain: try `$Z source gain --source sdr --db <n>` a few dB up or down. A carrier needs to stand
  clearly above the noise, but too much gain raises the floor and adds intermod.
- Antenna: a 2 m antenna works at 148-158 MHz. One for 70 cm or 900 MHz alone often does not.
- The carrier is within 25 kHz of the centre, or in the outer ~5 % of the slice. Both areas are
  ignored. Move the centre: edit `center_hz` in your set file, restart the node and start the
  set again. (In AU, use a copy of `pagers_au` named `pagers_local`.)
- A watch event `unknown at <MHz>: no listed mode fits <width> kHz` means the carrier was seen
  but its measured width did not fit POCSAG. From revision `79ff0826` on, every width from the
  6 kHz floor to 25 kHz fits, so this should only name widths over 25 kHz. If it names
  `6 kHz` or anything else under 25 kHz, your build is older than that: report it with the build.
  See the next two items.

**A strong carrier is read too wide.** A strong pager can measure 19-25 kHz, and that still fits
(the fit goes up to 2x of 12.5 kHz). Wider than 25 kHz is ignored or reported "no listed mode
fits". Lower the gain a few dB. Report the widths you saw.

**A clean pager reads narrow.** This is a known field risk. A clean FSK carrier can measure as
one or two narrow tones rather than a 12.5 kHz block. The detector drops anything narrower than
its 6 kHz floor, so such a carrier shows up in the `signal` lines at about 6 kHz. From revision
`79ff0826` on, a carrier read at the floor fits POCSAG and gets a decoder (on older builds it was
skipped with `no listed mode fits 6 kHz`). Report the frequency and the widths from the `signal`
lines. If a carrier you can see paging never shows in the `signal` lines at all, or to check
that a channel decodes at all, start a decoder on it by hand (the watch leaves a hand-started
decoder alone). The answer is `{"result":"job","value":"<id>"}`, so keep the id:

```bash
D=$($Z job start --kind pocsag_decode --source sdr --center-hz <carrier Hz> --bandwidth-hz 12500 | jq -r .value)
echo "$D"
```

The carrier must lie inside the watch's slice: the watch holds the radio on it. Stop the decoder
with `$Z job stop --job "$D"` (it answers `{"result":"ok"}`) when you are done. Its pages land in
the same `pocsag` counts as the watch's decoders, so note its channel and subtract them in your
report.

**A decoder keeps starting on a line that never pages, then is dropped `unproven`.** That is a
spur or a steady local carrier. You have two fixes:

- Put the capture centre on it, so the DC mask hides it. The built-in `pagers_au` set does this:
  the developers' B210 on the field host has a steady spur at exactly 148.500 MHz, and the set is
  centred there for that reason.
- Leave the centre where it is and list the spur: add `"spurs_hz": [<Hz>]` to the `params` in
  your set file. The watch then ignores ±5 kHz around it. Restart the node and start the set
  again.

**A carrier sits exactly at the capture centre.** The DC mask hides it, so the watch never sees
it. Move `center_hz` by 100 kHz or so, so that no carrier sits within 25 kHz of the centre.

**`set start` answers `conflict`.** Another job holds the radio on a different band. `$Z job list`
shows it. Stop it if it is yours, then retry.

**`set start` answers `other` naming a watch.** A watch with a different policy already holds the
band (for example one started from the page's menu). Stop that watch, then retry.

**The radio shows a `fault` in `source list`.** The node lost the device (cable, USB power). It
reconnects by itself when the device comes back and restarts the watch. Note the time in your
report.

## Optional: US 929-932 MHz and FLEX

The 900 MHz band carries both POCSAG and FLEX. The FLEX decoder (`flex_decode`) needs
`sudo apt install multimon-ng`. With it installed, `$Z modes list` should show `flex_decode` as
`ready`. Try this only after the main test, as a separate hour. Use a set file like
`pagers_local` with your 900 MHz centre, `"modes": ["pocsag_decode", "flex_decode"]` and
`"width_hz": [6000, 50000]`. A carrier then gets the decoder whose channel width (12.5 kHz for
POCSAG, 25 kHz for FLEX) is nearest its measured width. Report it as its own results block, and
mark it FLEX. The same privacy rules apply.

More about the node: [README](../../README.md), [the guide](../guide.md).
