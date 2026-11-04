# ISM remotes and sensors, decoded live (draft)

Design spec draft, 2026-11-04, written while the operator was away; nothing here is built. The
operator's ask (2026-11-04): "the main thing is to be able to decode future signals like this, with
their frequency shift quirk as it happens", after the first on-air test of plan 19.2 on their 433 MHz
remote. Questions for the operator are in section 7.

## 1. What the remote showed

- An EV1527-class fixed-code OOK remote on the field host's B210: 433.876-433.885 MHz, 23-42 kHz occupied,
  30-36 dB SNR. The centre moved 9 kHz across 22 presses in two minutes (a SAW/LC fob's normal drift).
- Pulse-width coding: 280 us and 907 us pulses, 344 us and 967 us gaps, a 1249 us bit, a 9.69 ms reset
  after each 25-pulse row (24 bits and the sync pulse). Three codes seen, one 20-bit ID `<id>` and three
  button nibbles, each code `<id>` and a nibble (`<button-code>`).
- Plan 19.2's layer-4 chain decodes it after the slicer fix (branch `worktree-ook-remote-gist`), but only
  from a stored capture, after the fact, by hand.

## 2. The idea: rtl_433 as a program row

`rtl_433` (installed on the field host: debian/25.12-1, 253 of 290 protocols on by default) is the reference
decoder for this whole class: OOK and FSK remotes, doorbells, weather stations, tyre sensors, energy
meters on 315/433/868/915 MHz. It demodulates the envelope (OOK) or the discriminator (FSK) across its
whole input band, so a carrier that drifts inside the band changes nothing; with `-M level` each event
carries its measured frequency, RSSI and SNR, so the drift is reported per press rather than fought.

Measured on the remote's capture (a 2.048 Msps window, converted to `cu8` at full scale):

- the built-in "Generic Remote SC226x EV1527" decoder stays silent on it (its timing checks reject this
  fob), but rtl_433's flex decoder `-X 'n=ev1527,m=OOK_PWM,s=280,l=907,r=5000,t=150,bits=25'` decodes
  every row: `{25}<row-code>`, which is `<button-code>` with the sync bit (short pulse = 1 in rtl_433's PWM);
- `-A` (the pulse analyser) prints the pulse, gap and period histograms that a flex spec is written from;
  on a file holding many rows its automatic guess says "No clue", so the spec was written by hand.

So the node gains, as rows and not code: an `ism_decode` program row running rtl_433 with its default
protocols plus the operator's flex decoders, fed by a channel of the band; the events are rtl_433's JSON.
This is the reuse rung of the minimalism ladder, and rtl_433 (GPL) stays behind the process boundary as
the design requires.

## 3. The gap: program input level

The node converts IQ for a program as `cu8 = (x + 1) * 127.5` (`crates/zeta-ext/src/external.rs:913`)
and `cs16 = x * 32767`, with no gain. The B210 at 65 dB delivers this remote at a peak of 0.02 of full
scale (about -34 dBFS), so in `cu8` it spans +-2.5 counts and rtl_433 detects nothing; scaled to full
scale it decodes. (rtl_433 also read nothing from a `cs16` file in the tests, normalised or not, while
`cu8` worked; to be pinned down before choosing the format.)

Proposal: an optional `input.gain` on program rows, `{"agc": {"target_dbfs": -6, "attack_ms", "release_ms"}}`
or a fixed `{"db": N}`, applied before the sample conversion, with the gain in force noted in
`job_context` and its clip count. A slow AGC suits rtl_433 (it estimates its own noise floor and wants the
signal well clear of quantisation); a fixed gain suits a decoder that wants calibrated levels.

## 4. The row

- `ism_decode`: `programs: ["rtl_433"]`, `args: ["-r", "cu8:-", "-s", "$rate_hz", "-F", "json", "-M",
  "level", "-M", "time:unix:usec", ...flex specs]`, `input: {stream: iq, format: cu8, rate_hz: 250000,
  via: stdin, gain: {agc: ...}}`, `output: {via: stdout, parser: json_lines}`.
- `band`: `bandwidth_hz` 250 000 (rtl_433's default rate; covers 433.795-434.045 MHz around 433.92,
  so the 9 kHz drift and the 25 kHz fob width fit many times over), `centers_hz` [433920000, 868300000,
  915000000, 315000000].
- `event.schema`: `model`, `id`, `channel`, `button`, `code`, `data`, `rows`, `freq`, `rssi`, `snr`,
  `mic` (rtl_433's integrity check: `CRC`, `CHECKSUM`, `PARITY` or none); `summary` `{model} {id?}
  {button?} {code?}`; `subject` keyed by `model` and `id` so the Subjects tab keeps one row per device.
- `identify.proof`: an event with `mic` set, or two identical rows in one press for a flex decoder with no
  integrity check (a fixed-code remote's only proof is its own repetition).
- The operator's own flex decoders: an operator row copies `ism_decode` and adds `-X` specs; the EV1527
  spec above is the first. A later step (19.3's signature write-back) could write a flex spec from a
  layer-4 session, so a signal worked by hand once is decoded by rtl_433 from then on.

## 5. Running it live, as it happens

A remote press lasts well under a second, so the watch's detector (a signal must persist 3 s and three
rows) never sees it, and a hand capture only helps after the fact. The live form is an ordinary
always-on decoder job, as ADS-B is: `job_start --kind ism_decode` on 433.92 MHz / 250 kHz on a radio
pinned there (the HackRF, or the B210 when the watch is not using it), every press an event within
milliseconds, its measured frequency in the event, transcribed nowhere, shown as ribbons and Subjects.

## 6. Unknown signals in the same band

What rtl_433 does not know shows as nothing. Two paths, both existing: run `-A` alongside (a second
program row, `ism_analyse`, printing the analyser's histograms as events for any package it detects) so an
unknown fob's timing is in front of the agent at once; or the existing capture and `bits_start` chain on
the band. The flex spec written from the histograms becomes an operator row (section 4).

## 7. Questions for the operator

1. Bands: 433.92 MHz only for now, or 868/915 too (the HackRF covers all of them; the B210 too)?
2. Radio: give the HackRF to `ism_decode` permanently at 433.92 MHz, or share the B210's pin?
3. rtl_433 as a dependency (GPL, external process, `apt install rtl-433`): acceptable? It is already on
   the field host.
4. Input gain: AGC by default, or a fixed gain per row?
5. Should the page notify on a decoded press from a known device (the Subjects tab already lists them)?

## 8. Left to the plan

The AGC constants; the exact rtl_433 arguments (`-Y` level options once the input is at a sane level);
the `cs16` puzzle; a `modes check` test (rtl_433 decodes synthesised EV1527 pulses: `synth` `ook` family
with PWM bits); the identify dwell for a press-driven signal (a fob only transmits when pressed: identify
cannot prove it unless the press falls inside the dwell, so the row's `identify` stays event-only and the
always-on job is the main path).
