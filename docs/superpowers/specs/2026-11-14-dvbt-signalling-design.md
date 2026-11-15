# DVB-T signalling (plan 39)

2026-11-14. Small enough that this page is the spec and the plan.

## Intent

A register lookup named a digital television multiplex the node could not decode. The operator
asked for DVB-T support, signalling first and picture and sound later: a job on a multiplex that
says what the transmission is, by measurement.

## Goals and non-goals

- A `dvbt_decode` job reports the channel width, 2K or 8K carriers, the guard interval, the
  constellation, the code rate, the hierarchy and the cell id when one is sent.
- No channel raster, width or region is assumed: the job is centred on the measured multiplex and
  the receiver finds the width.
- Not in this plan: the transport stream, service names, picture, sound and DVB-T2 (TODO.md,
  "DVB-T: picture, sound and services").
- Never: copyleft receiver code in this repository or linked into the node (`docs/licensing.md`).

## Approaches

The operator named four receivers, each for its own reason (TODO.md keeps them): GNU Radio's
`gr-dtv` blocks (flexible, but no ready receiver and no signalling decoder in tree), the DVB-T
receiver in SDR-- (fast, but no way to pipe IQ in), OnAir (moving quickly, untried on a real
radio) and SDRangel's `modemdvb` library (self-contained C++, easy to drop in). For signalling
alone `modemdvb` was chosen: its OFDM demodulator is one class with a status that carries the
signalling, and it builds without Qt once five small headers are replaced by stand-ins. A spike
on a 1.5 s capture from the field host's B210 locked in about 0.2 s, at a quarter of one core.

## Design

- **`dvbt-rx`**, a separate GPL-3.0-or-later program in its own repository: complex float32 IQ on
  stdin at `--rate`, JSON lines on stdout. It links `modemdvb`'s reception sources and FFTW. With
  `--bandwidth auto` it gives each of 8, 7, 6 and 5 MHz half a second of signal to get past
  acquisition, then tries the next; a wrong width never acquires (guard interval correlation
  about 0.07 against 0.95). It prints a line when lock is gained or lost, when the signalled
  parameters change, every 2 s while locked and every 5 s while searching.
- **`modes/dvbt_decode.json`**, a program row: IQ at 10 Msps (inside the library's resampling
  range for all four widths) as `cf32` on stdin, `json_lines` out, default band 8 MHz, proof for
  identify the `locked` field. It has no `test`: a real capture is over 100 MB.
- Nothing in the node changed: `external_decode` already feeds wideband IQ to a program.

## Checked

- The row parses and validates with the embedded rows (`zeta-core` `modes` tests).
- End to end on a release build: a node playing the capture as a file source, `dvbt-rx` on the
  `PATH`, `job start --kind dvbt_decode`: `locked` events every 2 s with the same parameters the
  program gives alone. A debug build skips input at this rate and never locks.
- `dvbt-rx` on noise: `searching` every 5 s, never a lock.
- Live acceptance is UAT.md, plan 39.

## After the first live run (2026-11-15)

Five multiplexes locked on the field host; three weaker ones beside them did not, and identify's
register reading on a decoded multiplex said `disagrees`. What changed:

- **A channel filter in `dvbt-rx`**, not in the node. The node's channel stage leaves a band that
  is most of the capture unfiltered, and its filter at these rates is wider than the gap between
  multiplexes (about 5 % of the channel). The program knows the width it is trying, so it
  low-passes to it in the frequency domain (1025 taps, 80 dB, about 7 % of a core at 10 Msps).
  Measured on the capture with synthetic neighbours: no effect up to 10 dB above the wanted
  multiplex; from 20 dB the unfiltered receiver takes the neighbours' timing and the filtered one
  does not notice them.
- **`ripple_db` and `resyncs`** in each event, and `snr_db` explained: it is the unweighted mean
  error of the equalised pilots, so a strong multiplex with deep multipath reads low.
- **The register is judged against the decode.** An identify verdict whose decoder proved asks
  the register about the row's `modulation` and the proving event's `bandwidth_hz`
  (`decoded_query`, `node/identify.rs`), with the lookup already made. `ofdm` goes with the
  emission letters `D`, `V`, `W` and `X` (`modes/score.rs`), which also gives the row the
  register's score term on a licensed multiplex.
