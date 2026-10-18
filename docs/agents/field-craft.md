Measured facts first, then method. Verify against the node's `knowledge lookup` before relying on any of this.

### Where to look

- Radiosondes: 400-406 MHz. RS41 is GFSK at 4800 baud, about 10 kHz wide.
- APRS: 144.800 MHz in AU and EU, 144.390 MHz in the US. AFSK at 1200 baud.
- POCSAG: the preamble is 576 bits of alternating 1/0, then the 32-bit sync word 0x7CD215D8. Common rates are 512, 1200 and 2400 baud.
- ADS-B: 1090 MHz, PPM at 1 Mbaud, 112-bit extended squitter.
- MeshCore: LoRa around 915 MHz in AU and 869 MHz in EU, 250 kHz wide. Not decodable without the network key; classify and report, do not try to decode.

### Decoding APRS and reading measurements

To decode APRS from a source, start the job and tail its events in one call: `zeta job start --kind aprs_decode --source S --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000`. It prints `[job_result, frames_result]`; each frame's `payload` is one decoded packet (`src`, `dst`, `info`, `fcs_ok`). An empty frame list means no packet arrived within the timeout, not that none exist. A `measure` job emits one event per second of IQ: `center_hz` and `bandwidth_hz` are the occupied band (spectrum bins at least 6 dB above the median noise floor), `snr_db` is the peak above that floor, `burst` is true when the loudest 10 ms is more than 3 times the median 10 ms power, `symbol_rate_baud` is the keying rate when a symbol-rate line stands out and `null` otherwise, and `label.name` is one of `noise` (SNR under 6 dB), `carrier` (steady envelope, no modulation), `fm` (steady envelope with deviation), `fsk` (two tones 500-3000 Hz apart, on RF or in FM audio) or `unknown`. Report `unknown` as unknown, with the measured fields.

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
