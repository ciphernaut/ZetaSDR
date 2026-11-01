# HackRF acceptance on e14 (plans 15, 18, 20)

**Run on the field host instead, 2026-11-01:** the operator moved the HackRF to the field host, where it was attached to the `sdr` node beside the B210; sections 1 and 3 and the DC-in-box half of section 2 are done and recorded in the plan series ("Plan 15/18/20 status", "Live run on the field host HackRF"). The indoor log periodic there hears no UHF CB, so section 2's fit on a transmitting channel was done on the B210 with the discone instead (ch9, +450 Hz, transcripts; the width fitted to the 5 kHz floor on a weak signal, a TODO gist). Note for section 3: `worker.restarts` counts respawns, so after a cable pull it reads one plus the `device not found` attempts, not 1.

The items of the 2026-10-27 sprint that only the HackRF on the ThinkPad E14 can settle, with the
exact commands. Everything else was proved on the field host's B210 on 2026-11-01 (see "Plan 15
status", "Plan 18 status" and "Plan 20 status" in `2026-10-18-zetasdr-plan-series.md`). The
outcome of this session is three constants and a few ticked boxes.

## 0. Before the radio

1. Pull `main` from the field host: e14 is behind since 8053f43. `git pull field-host main` from
   `/Projects/ZetaSDR` (the remote `field-host` exists on e14; the head should be at or past
   379940c).
2. Antenna: the roof discones were disconnected on 2026-10-28 and an indoor log periodic heard
   nothing. Check `lsusb | grep 1d50:6089` and a broadcast-FM max-hold before blaming zeta: a
   floor that does not move with gain means no antenna. UHF CB needs the 2 m / 70 cm antenna.
3. Stop the old two-node launchers if they are still up (7373 and 7374 ran the pre-plan-20
   binary): `pkill -f "[z]eta --state-dir .* serve"`.
4. Build: `RUSTFLAGS="--cfg soapy" cargo build -p zeta --release` (a debug build lags at
   1 Msps).
5. Start the single node (the form plan 20 item 4 wants): `scratch/serve-hackrf.sh`, or by
   hand

       ./target/release/zeta --state-dir scratch/state \
         --source-soapy hackrf=driver=hackrf,gain=45 --stt-socket /run/voiced/voiced.sock \
         serve --listen 7373

   with `ZETA_SECRET` set. Then `export ZETA_NODE=http://127.0.0.1:7373` and the secret for
   every `zeta` below. Page: `http://127.0.0.1:7373/ui/`.
6. `zeta source list`: `worker {pid, restarts: 0}`, `fault: null`, `gain.db` 45.

## 1. Plan 15 §9: the scan and the constants

The HackRF's DC hump and edge roll-off set `SCAN_NOTCH_HZ`, `SCAN_EDGE_MASK` and
`MEASURE_DC_MASK_HZ` (`crates/zeta-core/src/ops.rs`, today 300000 / 0.10 / 25000, chosen from
the 2026-10-23 survey by eye). `edge_mask` must stay under 0.25 for the quarter-step grid.

### 1a. The survey look (item 1)

    zeta job start --kind scan --source hackrf --center-hz 464000000 --bandwidth-hz 48000000
    zeta job context --job <scan>        # scan.alternate {shift_hz: 4000000, steps_shifted: 4}, scan.mask {300000, 0.1}

Watch the page's coarse layer for two sweeps or more (the waterfall follows the scan in scan
mode). Pass: no hump at 448, 464 or 480 MHz, no grid of peaks that moves between sweeps, and no
band left unpainted after two sweeps (448, 456, 464, 472, 480 MHz: the old half-step holes).
Screenshot it. Record under "Plan 15 status".

### 1b. The hump and the edges (item 2)

Turn the mask off on the running scan, then read rows:

    zeta graph get --job <scan> > g.json          # set the fft node's notch_hz to 0 and edge_mask to 0.0
    zeta graph put --job <scan> --file g.json
    zeta stream list                                # the scan's event stream
    zeta stream tail --stream <id> --max-frames 6 --timeout-ms 8000 > rows16.json

For a 16 MHz step (4096 bins, 3906 Hz each): in a row, `dB = floor_db + q[k] * step_db`; take the
row's median level; the hump is the contiguous run of bins around bin 2048 more than 6 dB over
the median (also note the run over 3 dB); the edge roll-off is the number of bins at each end
more than 3 dB under the mid-row level. Convert both with `bin_hz`. Repeat at 8 and 2 Msps with
short spectrum jobs after stopping the scan (a scan time-shares the radio):

    zeta job stop --job <scan>
    zeta job start --kind spectrum --source hackrf --center-hz 464000000 --bandwidth-hz 8000000
    zeta job start --kind spectrum --source hackrf --center-hz 464000000 --bandwidth-hz 2000000
    zeta job start --kind spectrum --source hackrf --center-hz 476918750 --bandwidth-hz 1000000   # the UHF CB pin rate

Record per rate: hump width over 6 dB and over 3 dB, edge roll-off low and high, in Hz. The
B210 read 5-6 kHz / 9-15 kHz and 30-245 kHz for comparison; the HackRF's hump was 150-300 kHz
by eye on 2026-10-23, which is why the defaults are what they are.

Then: `SCAN_NOTCH_HZ` = the widest 6 dB hump across 2-16 Msps plus 20 %; `SCAN_EDGE_MASK` = the
widest roll-off as a fraction of the row plus 20 %, under 0.25. `MEASURE_DC_MASK_HZ` is not set
from this: a 12.5 kHz box on ch20 holds no DC and is measured unmasked, and any half-width of
13.75 kHz or more hides ch20 from a wider box; record the 1 Msps hump width and leave the
constant to the lease-offset follow-on. Hand the figures back; the commit, the status sections
and the TODO gist are mine.

## 2. Plan 18 §10 item 1: the fit on a transmitting channel

Needs UHF CB traffic. Pin and wait for a channel to open (the squelch dots on the tags show it):

    zeta source pin --source hackrf --center-hz 476918750 --bandwidth-hz 1000000

On the page, box a transmitting channel generously (about 100 kHz, box drawn beside the pin
centre, not across it) and pick "Transcribe › NBFM". Pass: the transcript shows
`fitted fm_demod to <centre> MHz, <width> kHz (fm)`, `zeta job list` shows the demod within
500 Hz of the channel centre and 10-16 kHz wide, the audio is clean, a transcript arrives.
Record the centre error and the width.

Also record the two outcomes the B210 could not give: a wide box that contains the pin centre on
ch20, ch21, ch60 or ch61 (expected: `no signal found ... (the capture centre ... not measured)`,
the demod starts at the box as drawn), and the outline menu's "Fit to signal" on a 12.5 kHz
`fm_demod` placed on that channel (expected: it fits, the band holds no DC). Items 2-6 passed on
the B210 and need no repeat; drag one outline anyway to feel the 12.5 kHz handles at the 1 MHz
view (`handles-out`) and say whether it feels right.

## 3. Plan 20 items 3 and 4: the HackRF comes back

With the pin and two demods running (`zeta job start --kind fm_demod ... 476900000 ... 12500`,
and ch40 477400000), pull the HackRF's USB cable, wait 10 s, replug. Pass: within about a second
`source list` shows `fault: "no samples for 1000 ms (device unplugged?)"` and the jobs fail with
`source hackrf: ...`; after the replug the journal (or the serve log) shows `waiting for the
device` once, `back after <n> attempts` once and `recovered: pin 476918750/1000000 Hz, gain 45
dB, 2 jobs relaunched` once; the jobs are `running` under their old ids; `worker.restarts` is 1.
Then `kill -SEGV <worker pid>` (the pid from `source list`): the same outcome through the
`signal 11 (SIGSEGV)` fault. Item 4 is the single-node form itself: the two-node launchers are
gone and this node did both of the above.

## 4. Lessons from the B210 day, so they are not relearned

- `measure.snr_db` is the peak bin over the floor: a narrow FSK pair (an RS41) reads 20 dB
  while its channel SNR is 6 dB. Judge a weak digital signal by a decoder, not by that figure.
- More front-end gain lifts signal and floor alike once the antenna's noise dominates (B210
  65 → 76 dB changed nothing); on the HackRF 45 dB was fine with an antenna, 32 without.
- A `stream tail` holds one of the node's stream sockets for its whole timeout (max 10 s): the
  pool is 64 now, but tail from one shell at a time while a page is open.
- The watch's 8 kHz detector floor hides a real RS41 (4.5 kHz wide at 19 dB); a 4 kHz floor
  surfaces the receiver's spurs at round offsets from the capture centre. Pre-place the
  decoder on the last known frequency until the detector gets a tone-pair criterion.

## 5. Where it all lands

Figures and screenshots → me, or straight into "Plan 15 status" / "Plan 18 status" / "Plan 20
status" with the date and `e14, HackRF One r9, <antenna>`; the three constants → one commit
`Scan: SCAN_NOTCH_HZ/SCAN_EDGE_MASK from the HackRF (...)` with the figures in the message;
then the sprint closes.
