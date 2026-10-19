Measured facts first, then method. Verify against the node's `knowledge lookup` before relying on any of this.

### Where to look

- Radiosondes: 400-406 MHz. RS41 is GFSK at 4800 baud, about 10 kHz wide.
- APRS: 144.800 MHz in AU and EU, 144.390 MHz in the US. AFSK at 1200 baud.
- POCSAG: the preamble is 576 bits of alternating 1/0, then the 32-bit sync word 0x7CD215D8. Common rates are 512, 1200 and 2400 baud.
- ADS-B: 1090 MHz, PPM at 1 Mbaud, 112-bit extended squitter.
- MeshCore: LoRa around 915 MHz in AU and 869 MHz in EU, 250 kHz wide. Not decodable without the network key; classify and report, do not try to decode.

### Decoding APRS and reading measurements

To decode APRS from a source, start the job and tail its events in one call: `zeta job start --kind aprs_decode --source S --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000`. It prints `[job_result, frames_result, {"stopped": true}]` (the job is stopped after the tail; add `--keep` to leave it running); each frame's `payload` is one decoded packet (`src`, `dst`, `info`, `fcs_ok`). An empty frame list means no packet arrived within the timeout, not that none exist. Exit 2 with `[job_result, job_record]` means the job is not running: read `state.reason` (e.g. a stage that failed on the source's blocks). A `measure` job emits one event per second of IQ: `center_hz` and `bandwidth_hz` are the occupied band (spectrum bins at least 6 dB above the noise floor, the 15th-percentile bin of the band), `snr_db` is the peak above that floor, `burst` is true when the loudest 10 ms is more than 3 times the median 10 ms power, `symbol_rate_baud` is the keying rate when a symbol-rate line stands out and `null` otherwise, and `label.name` is one of `noise` (SNR under 6 dB), `carrier` (steady envelope, no modulation), `fm` (steady envelope with deviation), `fsk` (two tones 500-3000 Hz apart, on RF or in FM audio) or `unknown`. Report `unknown` as unknown, with the measured fields. `classify` and `measure` analyse only the selection/job band (a `measure` event's `center_hz` is absolute RF, within that band); `spectrum` covers the whole source band.

### Remote HF receivers (KiwiSDR)

A KiwiSDR source covers 0-30 MHz and delivers 12 kHz of IQ per connection, so `bandwidth_hz` is at most 12 000: work one HF channel at a time with `measure` jobs or `classify` selections inside the band you tuned (e.g. 7 100 000 Hz with 10 000 Hz for 40 m), not wide `spectrum` sweeps. A Kiwi has a limited number of user slots; when they are all taken the source faults with `too_busy` and the job fails with that reason: report it, do not retry in a loop. The connection opens on the first tune and holds a slot until the node exits. Reception is site-dependent: the receiver's location, antenna, local noise and the time of day decide what is audible, so an empty band on one Kiwi says nothing about the band elsewhere.

### Sources on another node (federation)

A `--source-node` source is a receiver attached to another ZetaSDR node, typically a Raspberry Pi at the antenna. That node ships narrowband IQ by default (250 kHz at most), which a LAN or WiFi link carries comfortably: work one channel at a time with `measure`, `aprs_decode` or `classify` inside it. Widen it only when the job needs it, with `max_bandwidth_hz=N` on the flag, and expect a slow link to drop blocks rather than slow down. A `bandwidth_hz` over the cap is `out_of_range` with `suggested` set to the cap. Each tune runs a `passthrough` job on the remote node, so that node's `source list` and `job list` show the source as held: another caller there sees the lease conflict instead of a silent retune. If the remote node goes away, the local source faults naming its URL and the job fails with that reason: report it, do not retry in a loop.

### External stages

A plugin registered with `--plugin KIND=PATH` is a stage kind like any other: it is listed in `schema` under `stages` with its `params_schema`, and you use it by adding a node of that `kind` to a job's graph with `graph get`, edit, `graph put`. No job kind starts it on its own. A plugin that crashes or misbehaves fails its job with a reason naming the plugin.

### Working layer 4

When a signal has no known decoder, work it in this order:

1. Isolate the channel.
2. Resample to a rate suited to the symbol rate.
3. Recover the clock.
4. Slice symbols.
5. Inspect the bit stream.
6. Hypothesise framing (preamble, sync word, length, checksum).
7. Attempt a decode.
8. Write the result back as a signature, so the next encounter is a layer 2 match.

### The rule

When nothing matches, say "unknown" and report the measured features (centre, bandwidth, modulation, symbol rate, burst timing). Do not guess.
