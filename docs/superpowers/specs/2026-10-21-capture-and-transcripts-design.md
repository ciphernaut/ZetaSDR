# Capture and transcripts: rates the resampler accepts, pinned captures, live gain, the transcript log

Date: 2026-10-21. Status: draft for builder review.

## 1. Intent and why

Plan 11 was deployed on the field host (a B210 through SoapySDR/UHD) on 2026-10-20 and run with 80
`fm_demod` jobs, one per Australian UHF CB channel, each with a `transcribe` attached. Five things
came out of that night and the next morning, all accepted by the user as plan 12 work.

**Rates.** A demodulator started alone leases a capture of its own band, and the Soapy source then
asks for the smallest rate at least that wide, rounded up to 100 kHz
(`crates/zeta-source-soapy/src/caps.rs:7`, `pick_rate` at `:129-159`). UHD does not deliver that
rate: a B2xx runs its master clock at a fixed 61.44 MHz and decimates by the nearest integer, so a
400 000 Hz request becomes 61.44 MHz / 154 = 398 961.04 Hz, read back and rounded to an integer at
`crates/zeta-source-soapy/src/source.rs:234-237` and `:418-422`. The three rates seen live are all of
that shape: 398 961 (N = 154), 1 041 356 (N = 59) and 260 339 after channelising (1 041 356 / 4).
The channel expansion then picks the narrowest exact divisor of the source rate at least 48 kHz
(`crates/zeta-core/src/ops.rs:850-859`), which for a rate with no useful divisors is the rate
itself, and the demodulator's resampler refuses the ratio:

- `resampler 398961 -> 240000 Hz reduces to 80000/132987, beyond the 100000 limit` (`wfm_demod`,
  `crates/zeta-dsp/src/resample.rs:69-73`)
- `resampler 260339 -> 48000 Hz reduces to 48000/260339, beyond the 100000 limit` (`fm_demod`)

A 4 MHz request gave 4 096 000 Hz (61.44 MHz / 15) and every demodulator ran. Plan 11's resume
pre-tunes the band bounding a source's rigid jobs (`crates/zeta-core/src/node/mod.rs:305-345`) and
its ruling R20 leases a rigid request inside the capture without a retune, so narrow captures are now
the common case, and a restart of the 80-channel node fails its jobs.

**Transcripts.** The 80 `transcribe` jobs emit one `transcript` event per utterance
(`crates/zeta-speech/src/transcribe.rs:324-335`) on a stream frame and nowhere else. Nothing on the
node keeps them, and an outside recorder cannot hold 80 streams: `crates/zeta-ws/src/http.rs:57`
allows 16 websockets and 8 concurrent ops.

**A held band.** The user wants the B210 held at exactly the UHF CB band (channels 1-40 at
476.425 + (n-1) × 0.025 MHz, 41-80 at 476.4375 + (n-41) × 0.025 MHz: a 12.5 kHz grid from 476.425 to
477.4125 MHz) with the sampling centre, where the LO's DC spike sits, between two channels. Today
only a rigid job holds a capture, and resume restores only the band bounding rigid jobs.

**Gain.** Plan 11's late commits ec3998c and 8c55f1d added `gain=DB` to `--source-soapy`, applied
once when the device opens (`crates/zeta-source-soapy/src/lib.rs:22-40`,
`crates/zeta-source-soapy-driver/src/lib.rs:36-63`). Changing it means a restart, which drops jobs.

**Frequency error.** The B210 reads high: repeater carriers on 146.975 and 146.825 MHz appeared at
146.9754 and 146.8254 (about 0.4 kHz at 147 MHz; the user's estimate is +3.4 ppm).

Two further items were raised while this document was being written. **Squelch**: 80 tuners cost
about 483 % CPU and the 80 transcribe chains about 75 % more, on channels idle most of the time;
`fm_demod` on an empty channel outputs full-scale noise (peak about 1.2 on 147.050 MHz), so
`denoise` (RNNoise) runs continuously and the `vad` gate saves only the daemon call. And a
**restart regression**, confirmed and reproducible on the field host: the same 161 jobs (one
`passthrough` holding 476 920 350 / 4 000 000 Hz, so a 4 096 000 Hz capture; 80 `fm_demod` with
`channel out_rate_hz 51200`, `capture_rate_hz 4096000`; 80 `transcribe`) started fresh one by one
cost 555 % CPU with no `zeta-dsp: stage lagged` line; resumed by a `zeta serve` restart they cost
980-1143 % with 23-36 lag lines a second (`stage lagged, 128 frames skipped` dominating: whole bus
buffers dropped), indefinitely; stopping the 80 transcribers left 1143 %, so the tuners carry the
doubling. Section 3.8 names the cause in the code and designs the fix and the diagnostics that make
it observable (perf is not permitted on the field host: `perf_event_paranoid` 4).

## 2. Goals, non-goals, anti-goals

Goals:

- No capture the node leases ever yields a chain the resampler refuses: a UHD source asks only for
  rates it produces exactly, and a demodulator's resampler accepts any integer rate a source can
  report.
- Every `transcript` event is on disk the moment it is published, one JSON line each, with the job,
  its input, the source and the channel's centre, rotated like `ops.log`, readable with
  `zeta transcripts tail`.
- A source can be pinned to a band: a first-class holder that is not a job, persisted, restored before
  anything else on resume, shown in `source_list`, conflicting like a rigid job, released only by an
  explicit op.
- The receive gain of a source is set live by an op, shown with its range in `source_list`, and
  survives a restart.
- A resumed channel costs what a freshly started one costs: resumed jobs get the shared channel bank,
  and the node says, per source and per job, whether a channel is on the bank and which stage kinds
  lag.
- Optionally: a per-source frequency correction in ppm; an IQ-power squelch in the NBFM and AM
  demodulators.

Non-goals (later work): widening a held capture so two rigid jobs coexist; exposing the B210's second
chain; an LO offset with digital shift (3.3 recommends the caller places the centre); a settings
panel; per-source element gains (LNA/TIA/PGA) rather than the overall gain; plan 11's deferred
follow-ups (`docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md`, "Plan 11 status").

Anti-goals: a pin is never released by a job's `replace` (it is not a job); the resampler never
silently accepts an error larger than its documented bound; a demodulator with its squelch closed
never publishes noise as audio; nothing here changes the lease rules for jobs.

## 3. Node side

### 3.1 Rates the resampler accepts

Two layers, both needed: the first makes friendly rates the normal case on the hardware that showed
the fault; the second guarantees that no source, file recording or VRT sender can produce a rate the
demodulators refuse.

#### 3.1.1 The UHD rate ladder (`zeta-source-soapy`)

For a UHD device (`is_uhd`, `caps.rs:29-37`) the source does not round to 100 kHz. It builds a
ladder from the probed range (`probe_rate_ranges`, `caps.rs:50-89`): with `mcr` the range's maximum
(61 440 000 for a B210; the master clock, which `master_clock_rate=` in the args also fixes) and
`min` its minimum, the ladder is every `mcr / N` for integer `N` in `1..=min(512, mcr / min)` that
divides `mcr` exactly, so every rung is an integer number of hertz. `pick_rate` returns the smallest
rung at least `bandwidth_hz`, `None` above `mcr`. For the B210: 12 500 → 120 000 (N 512), 200 000 →
204 800 (300), 1 000 000 → 1 024 000 (60), 2 400 000 → 2 457 600 (25), 4 000 000 → 4 096 000 (15),
10 240 000 → 10 240 000 (6), 16 000 000 → 20 480 000 (3). 61 440 000 = 2^15 × 3 × 5^4 and
48 000 = 2^7 × 3 × 5^3, so every rung's ratio to 48 kHz reduces to a factor of at most 1 280: all
rungs are friendly to the audio rate without further checks. Odd `N` is allowed (UHD's CIC rolloff at
the band edges is the price of a rate near the request; the channel filter sits well inside).

Non-UHD devices keep today's `pick_rate` (discrete rates, stepped ranges, the 100 kHz quantum, all
multiples of which are friendly to 48 kHz).

Whatever the device, after `set_sample_rate` the source compares the rate read back with the band it
was asked for. When the actual rate is below `bandwidth_hz` (a device that rounded down), it asks once
more for the next rung or quantum above the actual rate; when that too falls short, `tune` fails
`PortError::Unsupported("soapy: device gives <actual> Hz for <requested> Hz, below the <bandwidth_hz> Hz band")`
rather than lease a capture narrower than the job's band (which `expand` would refuse at
`ops.rs:445-453`).

#### 3.1.2 The resampler snaps (`zeta-dsp`)

`Resampler::new(in, out)` keeps its exact reduced ratio when `max(L, M) <= SNAP_FACTOR` (2 048; the
prototype is then at most 16 385 taps, 64 KiB). Above that it searches outward from `in`
(`in + 1`, `in - 1`, `in + 2`, ...) for the nearest `in'` whose ratio to `out` reduces to a factor at
most `SNAP_FACTOR`, within `SNAP_MAX_PPM` (1 000 ppm) of `in`, and resamples as if the input were
`in'`. The output is nominally `out` Hz and really `out × in / in'`: for a 48 kHz or 240 kHz output
the error is under 300 ppm for every input rate up to 20 MHz (a divisor `d` of `out` with
`in' / d <= 2048` always lies within `d / 2` of `in`, and `d / 2 / in` peaks just above each
threshold at about 254 ppm), which is the order of a cheap crystal's error and far below what a
decoder's timing recovery or a listener notices. Past the cap the existing error text stands.
`Resampler::input_rate_hz()` reports the rate the ratio was built for; a snap logs one line
`zeta-dsp: resampler <in> -> <out> Hz snapped to <in'> (<ppm> ppm)`.

The live failures become: 398 961 → 48 000 snaps to 398 976 (= 384 × 1 039, ratio 125/1 039,
38 ppm); 260 339 → 48 000 snaps to 260 352 (= 384 × 678, ratio 125/678, 50 ppm); 398 961 → 240 000
snaps to 398 976 (ratio 625/1 039). Every existing exact ratio (96 000/48 000, 200 000/48 000,
2 048 000/2 400 000 in `crates/zeta-ext/src/adsb.rs:505-509`) is below the factor and unchanged.

#### 3.1.3 The expansion

`channel_rate_hz` (`ops.rs:850-859`) is unchanged: for a rate without a useful divisor the channel
runs at the source rate and the demodulator's snapped resampler takes it from there (one block of
cost, no refusal). `expand`'s refusal of a band wider than the channel rate (`ops.rs:445-453`) stays
as the last guard; 3.1.1 keeps it from firing on the B210. `reconcile_rate` (`ops.rs:1387-1442`) is
left as plan 11 noted (redundant with `fit_capture`; not this plan's).

### 3.2 The transcript log

Every `transcript` event (`"protocol": "transcript"`: text at `transcribe.rs:324-335`, a backend
error at `:339-347`, a dropped utterance at `:290-298`) is appended as one JSON line to
`<state_dir>/transcripts.log`, rotated to `transcripts.log.1` past 8 MiB exactly as `ops.log`
(`crates/zeta-core/src/node/opslog.rs:10-49`, generalised to a `JsonLog` both files use). The line:

```
{"t_ns": <write time>, "job": "<transcribe job>", "input": "<its input job>" | null,
 "source": "<id>", "center_hz": N, "bandwidth_hz": N, "start_ns": N, "end_ns": N,
 "text": "..." | null, "language": "en" | null, "dropped": bool, "error": "..." | null}
```

`center_hz`/`bandwidth_hz` are the record's `tuning` (an attached job copies its input's, so this is
the channel transcribed); `start_ns`/`end_ns` the event's; `text`/`language` null on a dropped or
failed utterance; `dropped` true for the stage's `dropped` event; `error` the stage's error text.

**Where the hook lives: the node, not the stage.** In `Inner::launch_inner`, once a job's output
streams are known (`crates/zeta-core/src/node/launch.rs:196-248`), a job whose `kind` is `transcribe`
gets one tee task per `Event` output: it subscribes to the stream, filters `protocol == "transcript"`
and appends; it ends when the stream is unregistered at the job's end (`launch.rs:695-697`) or on the
job's cancel token, and is not one of the job's supervised stages, so it can never end the job. The
reasons it is not in `zeta-speech`: the stage knows neither the job id, the input id nor the tuning
(`StageContext` is a source and a bank, `crates/zeta-core/src/stage.rs:63-66`), and the frames it
publishes carry only the input frame's provenance (`transcribe.rs:265-272`); the rotating writer and
its lock already live in core; 80 stages writing one file need one lock; and a later stage that emits
`transcript` events (a digital voice decoder, a plugin) is logged without knowing about the file. A
tee that lags (`RecvError::Lagged(n)`) records `transcripts.log: job <id>: <n> events skipped` once in
`persist_errors` and continues; a write failure is recorded once as `transcripts.log: <e>` exactly as
`ops.log`'s (`mod.rs:699-714`) and never fails anything.

`zeta transcripts tail [--lines N] [--job ID]` prints the last N lines (default 50) of
`transcripts.log.1` + `transcripts.log` in order, keeping only lines whose `job` or `input` equals
`ID` when given; one-shot, local; with `--node` the usage error
`transcripts tail reads the state directory: not supported with --node` (exit 1), like `ops tail`
(`crates/zeta/src/main.rs:250-264`).

### 3.3 Pinned captures

A pin is a rigid lease that is not a job. `LeaseManager` gains a `pin` flag on `Lease`
(`crates/zeta-core/src/lease.rs:68-73`) under the holder id `pin:<source>`:

- `source_pin { source, center_hz, bandwidth_hz, replace }`: under the source's gate, the `replace`
  holders are stopped (`Node::replace_holders`, `mod.rs:842-886`), any existing pin on the source is
  released, then the band is leased as a rigid request through `ensure_locked` (`lease.rs:229-290`):
  inside the capture other rigid jobs hold it is granted without a tune; elsewhere it tunes when only
  elastic jobs hold the port, else it is a `conflict` naming those jobs. A tune relaunches the elastic
  jobs as any rigid tune does. The result is the capture (`Tuning`), as `source_ensure`'s. The pin is
  persisted (3.6) with the requested band; a persist failure releases it and is `io`.
- `source_unpin { source }`: releases the pin, persists `pin: null`, idles the port if nothing else
  holds it (`idle_if_unheld`, `lease.rs:184-190`); `Ok`. A source without a pin is
  `other("source <id> is not pinned")`.
- `source_list` gains `pin: Tuning | null` (the pinned band). `holders` keeps listing job ids only;
  a pin is never in `holders` nor in a conflict's `holders` (`rigid`, `lease.rs:141-147`, excludes
  it), so an agent is never handed an id that is not in `job list`. A conflict on a pinned source adds
  the option `unpin source <id> (source_unpin)` before the `retry with replace` line
  (`conflict_options`, `lease.rs:113-138`).

Interactions:

- Elastic jobs join the pinned capture and follow it (`ensure_locked` sees the pin as the first rigid
  lease); `source_ensure` inside the pin returns the capture, outside it is a `conflict`
  (`tune_if_unheld_locked`, `lease.rs:393-433`); a rigid `job_start` inside the pin is leased without
  a tune, outside it is a `conflict` with the unpin option (and `retry with replace` only when jobs
  hold it too).
- `replace` never names a pin: the id is not in the job table, so it is `unknown_job` as today
  (`mod.rs:852-854`).
- A `scan` on a pinned source is a `conflict` (`acquire_shared_locked`, `lease.rs:320-330`, finds a
  rigid lease); stop it with `source_unpin`, or scan another source.
- Resume: pins are restored right after the ports are added and before `resume_captures`
  (`mod.rs:440-446`): each pinned source present is leased at its pin; a tune that fails leaves an
  entry `source <id>: pin <center_hz>/<bandwidth_hz> Hz: <reason>` in `load_errors` and the record
  on disk. `resume_captures` skips a pinned source (the pin is its capture); records that do not fit
  fail with the `conflict` reason as they would live, a persisted running scan included.
- The page treats a pin as a rigid holder (4.1).

**LO placement.** Recommended: the caller places the centre, and the field craft says how. For a
channel grid of spacing `s` whose first channel is `f0`, a centre at `f0 + (k + 1/2) × s` sits halfway
between two channels, so the DC spike falls in a guard band. For the UHF CB plan the band's own
midpoint, 476 918 750 Hz, is 39.5 channel spacings above 476.425 MHz, so
`source_pin { center_hz: 476918750, bandwidth_hz: 1000000 }` holds the whole plan with the spike
between channels 40 and 41 (the B210 ladder gives 1 024 000 Hz, 476.40675-477.43075 MHz). The
alternative, an LO offset with a digital shift, needs driver support: SoapyUHD accepts an `OFFSET`
argument on `setFrequency` (the Rust crate passes `Args`,
`crates/zeta-source-soapy-driver/src/lib.rs:136-137` passes none) and UHD's DSP shifts the stream to
the requested centre, but HackRF and RTL drivers do not, the wider capture needed to keep the offset
inside the band is device-specific, and it cannot be tested against the mock. Listed as an open
question (8.3), not built.

### 3.4 Live receive gain

`Port` (`crates/zeta-core/src/port.rs:72-88`) gains two methods with defaults:

```rust
fn gain(&self) -> Option<GainInfo> { None }
async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
    Err(PortError::Unsupported("source has no adjustable gain".into()))
}
pub struct GainInfo { db: Option<f64>, min_db: f64, max_db: f64, automatic: bool, automatic_available: bool }
```

`db` is `null` while automatic gain is on. `source_set_gain { source, gain_db: number | null }`:
`null` turns the driver's automatic gain on where the device has it, else
`other("source <id> has no automatic gain")`; a number turns it off and sets the overall receive gain;
a value outside the range is `other("gain_db <db> out of range <min>..=<max> dB for source <id>")`
(not `out_of_range`, whose fields are unsigned integers, `ops.rs:1108-1115`). The result is the
`GainInfo` after the change. `source_list` gains `gain: GainInfo | null` (`null` for a source without
adjustable gain: file, Kiwi, node, VRT). `GainInfo` fields end in `_db`, a new unit suffix the
guide's rules and `unit()` (`crates/zeta/src/agents_md.rs:97`) must name.

The Soapy source: `SoapyDevice` (`crates/zeta-source-soapy/src/device.rs:31-42`) gains
`gain_range`, `gain`, `set_gain`, `has_gain_mode`, `gain_mode`, `set_gain_mode`, mapped to the
soapysdr 0.5 calls at channel 0 by the driver (`has_gain_mode`/`set_gain_mode`/`set_gain` are what
`set_rx_gain` already uses, `lib.rs:36-43`) and recorded by the mock. `SoapySource` reads the range,
mode and value when it opens (`with_device_args`) and keeps them in `Status`; `set_gain` goes through
the reader thread's command channel when a reader is alive (the `Retune` command becomes a `Cmd`
enum, `source.rs:60-64`, so a gain change and a retune keep their order) and directly otherwise, then
updates `Status`. The flag `gain=DB` is applied at open through the same path.

Persistence: the gain set by the op is written to the source record (3.6) and re-applied at
`Node::open` after the port is added (a failure is a `load_errors` entry
`source <id>: gain: <reason>`); so the op's value wins over the flag's on the next start, and the
flag is the default for a source whose record has no gain. Whether to persist at all is open
question 8.2.

### 3.5 Frequency correction (optional)

`--source-soapy ID=ARGS` accepts `ppm=N` (a finite number, zeta's own key like `gain=`, taken out by
`split_gain`'s successor `split_zeta_args`). The Soapy source applies it in `tune`: the hardware is
asked for `round(center_hz × (1 + ppm / 1e6))` and every block, `current()` and `source_list` carry
the requested `center_hz`; a device that reads high by `p` ppm (the B210's case: `ppm=+3.4` by the
user's estimate, 2.7 by the 0.4 kHz offset at 146.9 MHz) is thus tuned higher so the labelled axis is
true. No driver call is involved (SoapySDR's `setFrequencyCorrection` is not implemented by SoapyUHD,
and the correction belongs to the device, like `gain=`, not to the node), so it works for every
device. Not persisted: it is a calibration of the device the flag names. Open question 8.4.

### 3.6 Source records on disk

`<state_dir>/sources/<source id>.json`, written atomically as job records are
(`crates/zeta-core/src/node/persist.rs:22-29`):

```
{"format": 1, "pin": {"center_hz": N, "bandwidth_hz": N} | null, "gain": {"db": N | null} | null}
```

`gain: null` means never set by the op (the flag or driver default applies); `{"db": null}` means
automatic. Loaded at `Node::open` after the ports; a record for a source that is not attached is kept
untouched (sources are not persisted, the flags may return). `persist_errors` and `load_errors` carry
failures as for jobs. Format policy as `JobRecord`'s (`crates/zeta-core/src/job.rs:24-30`).

### 3.7 Squelch in the NBFM and AM demodulators (optional)

`nbfm_demod` and `am_demod` gain params `squelch_db: integer | null` and `squelch: bool` (default
true). Per IQ block at the channel rate the stage measures the mean power in dBFS; the noise floor is
the 15th percentile of the last 100 block powers (about 2 s at 20 ms blocks; `measure`'s floor
rule, `crates/zeta-dsp/src/measure.rs:88-96`), seeded by the first block. The squelch opens when the
power exceeds the threshold (automatic: floor + 6 dB; fixed: `squelch_db` dBFS) and closes when it
has stayed under the threshold minus 3 dB for 300 ms (hang). While closed the stage skips the
discriminator (or envelope), the resampler and the audio frame: it publishes nothing on its audio
output. On each transition, and once for the first block, it publishes a `squelch` event on a new
`Event` output (`{"protocol": "squelch", "open": bool, "power_db": P, "floor_db": F}`); the demod
stages' `outputs` become `[Audio, Event]`, which `node_outputs` (`launch.rs:624-646`) gives a fresh
stream, and `job_attach` still finds the audio output (`mod.rs:253-266`, the last audio output).

Nothing on the audio stream while closed is the right choice for the attached chain: the
`transcribe` stage closes an open utterance on wall-clock silence (`transcribe.rs:422-424`), so a
closing squelch ends the utterance; `denoise` and `vad` run only on frames, so an idle channel costs
them nothing. That makes the proposed reorder of the chain (gate before `denoise`) unnecessary: the
squelch is the gate. Silence frames were rejected (they keep RNNoise running and still carry noise
during the hang); so was dropping the event (the page and the agent need to know a quiet row is
squelched, not dead). `job_start` gains `squelch_db: integer | null` (null = automatic; only
`fm_demod` and `am_demod` take a value, every other kind passes `null` as `deviation_hz` does), and
the CLI `--squelch-db N`; the squelch cannot be turned off from `job_start` (`graph_put` with
`squelch: false` can). Range for a fixed threshold: -120..=0 dBFS, `out_of_range` otherwise... but
`OutOfRange` is unsigned, so it is `other("squelch_db <v> out of range -120..=0")`. Open question
8.5: the two params, and whether `job_start` should carry the field at all (it changes a required
field, so every caller).

Note the squelch does not save the `channel` stage's cost, which dominates a direct-path tuner (up to
1 025 taps per output sample, `crates/zeta-dsp/src/channel.rs:21-33`); the bank path is what saves
that (3.8).

Decision (2026-10-21, the user): alternative (a). The squelch is always on and automatic; `job_start`
gains no field and no caller changes. `expand` writes `squelch_db: null, squelch: true` into the demod
node's params so `graph_get` shows them and `graph_put` sets a fixed threshold or turns the squelch off.

### 3.8 Resumed jobs and the channel bank

**Cause.** `crates/zeta/src/node_setup.rs:267-271` opens the node and only then installs the bank
factory, and says so: "Jobs resumed by `open` predate the factory and run without a bank."
`Node::open` resumes every record inside itself (`crates/zeta-core/src/node/mod.rs:443-486`), each
launch asking `sync_bank` for the capture's bank (`launch.rs:158-168`), which without a factory keeps
nothing (`mod.rs:274-302`), so every resumed `channel` stage gets `ctx.bank == None` and filters the
full capture itself (`crates/zeta-dsp/src/channel.rs:255-280`: the bank path needs a bank whose bin
holds the channel and whose bin rate `out_rate_hz` divides; otherwise the direct path, up to 1 025
taps per output sample, `channel.rs:21-33`). A fresh start has the factory: at 4 096 000 Hz
`bins_for` gives 8 bins (`crates/zeta-dsp/src/pfb.rs:11, 25-31`: doubling while
`rate / (2 × bins) >= 750 000`, clamped to 8), a 512 000 Hz bin and a 1 024 000 Hz bin rate, and
`expand` channelises at `channel_rate_hz(1 024 000, 12 500) = 51 200` (`ops.rs:435-438, 850-859`),
which is exactly the graphs seen; 1 024 000 is a multiple of 51 200, so those channels are
bank-served live. The same graph resumed runs 80 direct decimations of 4.096 Msps by 80 (641 taps
each) instead of one shared PFB plus 80 decimations of 1.024 Msps by 20: that is the doubling, and
the saturated runtime is what lags (`crates/zeta-dsp/src/lib.rs:146-147`); a lag of the port's
broadcast then resets the bank (`launch.rs:818-843`, `bank.rs:294-296`), which the direct-path
stages never used anyway. `set_bank_factory` remakes the banks (`mod.rs:599-609`) but a running
stage never re-chooses its path.

**Fix.** `NodeConfig` gains `bank_factory: Option<Arc<dyn BankFactory>>`, installed before any
record resumes; `node_setup` passes `PfbBankFactory` there and the comment goes. `set_bank_factory`
stays for tests that install a fake factory on an open node.

**Diagnostics**, so the next regression of this kind is seen without perf:

- `StageContext` gains `note: Option<Arc<dyn Fn(serde_json::Value) + Send + Sync>>`, a callback the
  node builds per job that merges the value into the job's context (`job_context_merge`,
  `mod.rs:1015-1038`). The channel stage notes `{"channel": {"path": "bank", "bin_hz": N}}` when it
  subscribes to a bin and `{"channel": {"path": "direct", "reason": "<why>"}}` otherwise (`no bank`,
  `band <bw> Hz wider than a bin (<bin_hz> Hz)`, `bin rate <r> Hz not a multiple of out_rate_hz <o>`,
  or the bank's `subscribe` error), so `zeta job context` answers the question in one call.
- `source_list` gains `bank: {"bin_hz": N, "consumers": N} | null` (`ChannelBank::bin_hz`,
  `consumers`, `crates/zeta-core/src/bank.rs:29-41`), `null` for a source without a bank.
- `run_map` and `run_bin` take the stage kind and the lag line names it:
  `zeta-dsp: <kind> stage lagged, <n> frames skipped`; `zeta_dsp::lagged_frames_by_kind()` returns the
  counts per kind for tests.

Until this ships the operator re-creates the CB jobs fresh after any restart
(`~/zetasdr/scratch/cb-setup.sh` on the field host).

## 4. Page side

### 4.1 Pins

A pinned source's tab carries the text `pin` after its range (`.src-tab .tag.pin`) and a title with
the pinned band; the overlays outline the pin across its band labelled `pin` (key `pin`) and skip the
capture outline while a pin exists (`overlays.js:224-236` outlines rigid holders, else the capture).
Everywhere the page asks "does a rigid job hold this source" (the waterfall's decision to retune
through `source_ensure`, the coarse layer's choice of a free scanner, `holders.length === 0` in
`coarse.js`), a non-null `pin` counts as a holder. A `conflict` whose `holders` is empty on a source
with a pin offers one action `unpin <source> and retry` (issues `source_unpin`, then the original op),
beside plan 11's per-holder `stop <kind> <id> and retry`.

### 4.2 Commands

Typed on the command line, handled like `view` before the op and `/ask` paths
(`app.js:362-377`):

| command | does |
|---|---|
| `source pin` | `source_pin` of the shown source at its current capture (`source_list` `current`); error line when it has none |
| `source pin <center_hz> <bandwidth_hz>` | `source_pin` at that band (integers in hertz, as every op) |
| `source unpin` | `source_unpin` of the shown source |
| `source gain <db>` / `source gain auto` | `source_set_gain` with the number / `null` |

Replies are transcript lines: `pinned <source> at <center_hz> Hz, <bandwidth_hz> Hz`,
`unpinned <source>`, `gain <source>: <db> dB (<min>..=<max>)` or `gain <source>: automatic`; an error
is the op's error text. `source` alone or any other text starting `source ` goes to `/ask` as before
(ruling R24's shape).

### 4.3 Squelch (with 3.7)

A running `fm_demod` or `am_demod` row carries the text `squelch` (`.tag.squelch`) while the last
`squelch` event the page saw on the job's event stream has `open: false`; the overlays' decoded-event
counters ignore `protocol: squelch`.

## 5. Agent and CLI

`zeta source pin --source S --center-hz N --bandwidth-hz N [--replace ID[,ID]]`,
`zeta source unpin --source S`, `zeta source gain --source S --db N` or `--auto` (exactly one, else a
usage error), `zeta transcripts tail [--lines N] [--job ID]`, `zeta job start --squelch-db N`
(optional), `--source-soapy ID=...,ppm=N` (optional). The guide gains the three ops' sections from
their doc comments, `source_list`'s `pin` and `gain`, a "Running the node" sentence for
`transcripts.log`, the `_db` unit rule, and field craft: "Holding a band" (pin it, place the centre
between channels, the UHF CB example, unpin to move on; a conflict's `unpin source` option) and
"Reading transcripts" (`transcripts tail --job`).

## 6. Constraints

- `zeta-core` gains no dependency; `JobRecord` is unchanged; the new `sources/<id>.json` record is
  format 1 with the same policy. The ws and plugin wires are unchanged but for the new ops, which
  flow through `apply` like any other.
- Every field of an op is required: `source_pin` carries `replace` (`[]` for none) like
  `source_ensure`; `source_set_gain` carries `gain_db` (`null` for automatic). The optional
  `squelch_db` on `job_start` is the one change to an existing op; every in-repo caller sends it.
- The Soapy driver crate stays out of the workspace; everything new in the source is tested against
  the mock, and the UHD behaviours (the nearest-decimation rate, the gain calls) are modelled in it
  from the live observations quoted in section 1; the live acceptance confirms them.
- The UI suite runs about 234 s of its 300 s deadline (plan 11 status): two new scenarios at most,
  each a few seconds, no new fixture source.
- Gates as plan 11: `cargo fmt -p` per crate, clippy `-D warnings`, the crate tests, the Playwright
  suite with no skip line, the guide and skill regenerated and checked.
- Secrets and flags unchanged but for `ppm=`.
- Interim operation: until 3.8 is deployed, the operator re-creates the CB jobs fresh after any
  restart of `zeta.service` (`~/zetasdr/scratch/cb-setup.sh` on the field host) rather than let them
  resume.

## 7. Left to the builder

The `Cmd` enum's ack shape for gain; whether `JsonLog` keeps `OpsLog`'s name as an alias or the file
is renamed; the tee's placement relative to `record_running` (it must see the streams registered at
`launch.rs:184-220`, and the record's `input`/`tuning` are in `rec` there); the exact search order
of the resampler snap (outward, `+k` before `-k`, as the test values assume); the page's tag styles;
how `source pin` with no capture reads; the Playwright scenario details (4.1, 4.2, 4.3 in at most two
scenarios); whether the `sources/` loader shares `persist::load`'s shape.

## 8. Open questions

1. **Odd decimations on UHD.** The ladder allows odd `N` (3.1.1), accepting CIC rolloff at the edges
   for a rate nearer the request (16 MHz → 20.48 MHz rather than 30.72). Recommendation: allow; the
   channel filters never reach the capture's edges, and the spectrum view's droop is cosmetic.
2. **Persisting the gain.** Recommendation: persist it in the source record with the pin, re-applied
   at open, the op's value winning over `gain=` on the flag (the operator's last word). The
   alternative (gain is a device setting, lives on the flag only, the op is for the session) is
   simpler but loses the setting on every restart, which is what the user complained of.
3. **LO placement.** Recommendation: the caller places the centre between channels (3.3), documented
   with the UHF CB example; no LO offset. Revisit if a band has no guard band to put the spike in.
4. **Frequency correction `ppm=`.** Decided 2026-10-21: build it. Recommendation: build it as the small optional task it is (a
   multiply in `tune`), since the error is measured and affects every reported frequency; the user
   measures the value (2.7 or 3.4 ppm) and may strike the task.
5. **Squelch.** Recommendation: build it; the CPU case is strong and the idle hiss is a defect. Two
   sub-questions: (a) `job_start` gaining `squelch_db` changes a required field, so every caller and
   both ends of a `--source-node` link must move together (as plan 11's `elastic`): the cheaper
   alternative is automatic squelch always on with no field, and `graph_put` for a fixed threshold or
   off — decided: this alternative (2026-10-21); (b) the 6 dB / 3 dB / 300 ms constants are a first guess the live acceptance tunes.
6. **Transcript lines for errors and drops.** Recommendation: log them (with `text: null`), so a gap in
   a channel's transcript is explained in the same file; strike if the log should hold only speech.
7. **Resampler snap bound.** Recommendation: `SNAP_FACTOR` 2 048 and `SNAP_MAX_PPM` 1 000; the error is
   under 300 ppm in practice. The alternative, exact resampling up to the old 100 000 factor, costs
   4 MiB and about a million sinc evaluations per demodulator for a prime-ish rate (80 jobs: 320 MiB).

## 9. Known issues to investigate

- **The lag storm's second-order effects.** Once 3.8 removes the cause, two paths remain worth a
  look if lag lines ever return: a lag of the port's broadcast resets the bank
  (`crates/zeta-core/src/node/launch.rs:818-843`, `crates/zeta-dsp/src/bank.rs:294-296`), the bank
  thread then rebuilds its PFB (`bank.rs:169-192`, `crates/zeta-dsp/src/pfb.rs:91-103`) and every
  bin consumer restarts its filter (`channel.rs:392-396`); and a lag in `transcribe` flushes the open
  utterance to the daemon (`transcribe.rs:449-455`), so a lag storm is also a daemon-call storm. The
  per-kind lag counters of 3.8 make both measurable.
- **The bank path is invisible in `job_list`.** 3.8's `channel` context note fixes that for new
  launches; records written before it carry no note until the job is restarted.
- **`reconcile_rate`** (`ops.rs:1387-1442`) is still redundant with `fit_capture` (plan 11 status).

## 10. Calls made in this document

- Two layers for rates: a UHD ladder of exact divisors of the master clock in the Soapy source, and a
  snapping resampler in `zeta-dsp`; the channel expansion is left alone.
- The resampler snaps rather than refuses above a factor of 2 048, bounded at 1 000 ppm; the exact
  path is unchanged below it.
- The transcript log is a node-side tee on `transcribe` jobs' event streams, not a stage concern.
- A pin is a rigid lease under a non-job holder id, never listed in `holders`, released only by
  `source_unpin`; `replace` cannot name it; conflicts offer `unpin source <id> (source_unpin)`.
- Pins and the gain live in one per-source record, `sources/<id>.json`; the gain set by the op wins
  over the flag at the next start.
- The caller places the capture centre between channels; no LO offset.
- Gain is the device's overall receive gain through `Port::set_gain`, with `null` for automatic;
  errors are `other` with the range in the text, since `out_of_range` is unsigned.
- The squelch publishes nothing on audio while closed and a `squelch` event on each transition; the
  attached chain keeps its order.
- `ppm=` is a software correction in `tune`, device-agnostic, not persisted.
- The restart regression is fixed at its cause (the factory reaches `Node::open`), and made
  observable three ways (the channel's context note, `source_list.bank`, lag lines by stage kind)
  rather than patched in the stage.
