Measured facts first, then method. Verify against the node's `knowledge lookup` before relying on any of this.

### Where to look

- Radiosondes: 400-406 MHz. RS41 is GFSK at 4800 baud, about 10 kHz wide. Australian launches are usually around 401-403 MHz.
- APRS: 144.800 MHz in AU and EU, 144.390 MHz in the US. AFSK at 1200 baud.
- POCSAG: the preamble is 576 bits of alternating 1/0, then the 32-bit sync word 0x7CD215D8. Common rates are 512, 1200 and 2400 baud.
- ADS-B: 1090 MHz, PPM at 1 Mbaud, 112-bit extended squitter.
- MeshCore: LoRa around 915 MHz in AU and 869 MHz in EU, 250 kHz wide. Not decodable without the network key; classify and report, do not try to decode.

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
