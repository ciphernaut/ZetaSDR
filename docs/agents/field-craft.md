Measured facts first, then method. Verify against the node's `knowledge lookup` before relying on any of this.

### Where to look

- Radiosondes: 400-406 MHz. RS41 is GFSK at 4800 baud, about 10 kHz wide.
- APRS: 144.800 MHz in AU and EU, 144.390 MHz in the US. AFSK at 1200 baud.
- POCSAG: the preamble is 576 bits of alternating 1/0, then the 32-bit sync word 0x7CD215D8. Common rates are 512, 1200 and 2400 baud.
- ADS-B: 1090 MHz, PPM at 1 Mbaud, 112-bit extended squitter.
- MeshCore: LoRa around 915 MHz in AU and 869 MHz in EU, 250 kHz wide. Not decodable without the network key; classify and report, do not try to decode.

### Decoding APRS and reading measurements

To decode APRS from a source, start the job and tail its events in one call: `zeta job start --kind aprs_decode --source S --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000`. It prints `[job_result, frames_result, {"stopped": true}]` (the job is stopped after the tail; add `--keep` to leave it running); each frame's `payload` is one decoded packet (`src`, `dst`, `info`, `fcs_ok`). An empty frame list means no packet arrived within the timeout, not that none exist. Exit 2 with `[job_result, job_record]` means the job is not running: read `state.reason` (e.g. a stage that failed on the source's blocks). A `measure` job emits one event per second of IQ: `center_hz` and `bandwidth_hz` are the occupied band (spectrum bins at least 6 dB above the noise floor, the 15th-percentile bin of the band), `snr_db` is the peak above that floor, `burst` is true when the loudest 10 ms is more than 3 times the median 10 ms power, `symbol_rate_baud` is the keying rate when a symbol-rate line stands out and `null` otherwise, and `label.name` is one of `noise` (SNR under 6 dB), `carrier` (steady envelope, no modulation), `fm` (steady envelope with deviation), `fsk` (two tones 500-3000 Hz apart, on RF or in FM audio) or `unknown`. Report `unknown` as unknown, with the measured fields. `classify` and `measure` analyse only the selection/job band (a `measure` event's `center_hz` is absolute RF, within that band); `spectrum` covers the whole source band. A `spectrum` event is one row of power-averaged spectrum (20 per second of signal time) with `size` bins lowest frequency first, `bin_hz`, `center_hz` and the levels quantised as `q` (0-255 per bin): the dB of bin k is `floor_db + q[k] * step_db`; `floor_db` sits 5 dB under the row's 2nd-percentile bin, so a `q` near 10 is the noise floor and 255 means at least 127.5 dB above `floor_db`.

### Decoding pagers, aircraft and FM data

Where to find them:

- POCSAG: 148-149 MHz in AU (Telstra at 148 637 500 Hz among them), 466 075 000 Hz in much of Europe, 929-932 MHz in the US. Use `pocsag_decode` on a 12 500 Hz band.
- ACARS: 131 550 000 Hz (worldwide primary), 131 725 000 and 131 825 000 Hz (Europe), 130 025 000, 130 450 000 and 131 125 000 Hz (North America), 131 450 000 Hz (Asia). Use `acars_decode`, 10 000 Hz each.
- RDS: any FM broadcast station. Use `rds_decode` with the station's frequency and a 200 000 Hz band. The first event takes a few seconds.
- ADS-B: 1 090 000 000 Hz with a 2 400 000 Hz band. Use `adsb_decode`. It needs dump1090 on the node's host; the job fails naming dump1090 when it is missing. A band under 2 000 000 Hz is refused.

"Set up a background decoder for all ACARS and ADS-B activity":

1. `source_ensure` the radio at 130 925 000 Hz with 2 400 000 Hz. That band covers every ACARS channel above.
2. Start one `acars_decode` per channel inside it. They share the capture.
3. Start `adsb_decode` on a second source. A radio serves one band at a time, so on one radio it must wait until the ACARS jobs are stopped.

`knowledge_lookup` says which decoder proves a band's signature (`decoder` names the job kind). `classify` tries APRS, POCSAG and ACARS on narrow bands and RDS on wide ones, as decode-to-identify; it never attempts ADS-B (use `adsb_decode`). A CRC-clean decode is the strongest layer 2 evidence there is. A signature match without one is a guess: say so.

### Remote HF receivers (KiwiSDR)

A KiwiSDR source covers 0-30 MHz and delivers 12 kHz of IQ per connection, so `bandwidth_hz` is at most 12 000: work one HF channel at a time with `measure` jobs or `classify` selections inside the band you tuned (e.g. 7 100 000 Hz with 10 000 Hz for 40 m), not wide `spectrum` sweeps. A Kiwi has a limited number of user slots; when they are all taken the source faults with `too_busy` and the job fails with that reason: report it, do not retry in a loop. The connection opens on the first tune and holds a slot until the node exits. Reception is site-dependent: the receiver's location, antenna, local noise and the time of day decide what is audible, so an empty band on one Kiwi says nothing about the band elsewhere.

### Sources on another node (federation)

A `--source-node` source is a receiver attached to another ZetaSDR node, typically a Raspberry Pi at the antenna. That node ships narrowband IQ by default (250 kHz at most), which a LAN or WiFi link carries comfortably: work one channel at a time with `measure`, `aprs_decode` or `classify` inside it. Widen it only when the job needs it, with `max_bandwidth_hz=N` on the flag, and expect a slow link to drop blocks rather than slow down. A `bandwidth_hz` over the cap is `out_of_range` with `suggested` set to the cap. Each tune runs a `passthrough` job on the remote node, so that node's `source list` and `job list` show the source as held: another caller there sees the lease conflict instead of a silent retune. If the remote node goes away, the local source faults naming its URL and the job fails with that reason: report it, do not retry in a loop.

### External stages

A plugin registered with `--plugin KIND=PATH` is a stage kind like any other: it is listed in `schema` under `stages` with its `params_schema`, and you use it by adding a node of that `kind` to a job's graph with `graph get`, edit, `graph put`. No job kind starts it on its own. A plugin that crashes or misbehaves fails its job with a reason naming the plugin.

### Transcribing a channel

Start the demodulator, then attach the transcription to it; the attached job reads the
demodulator's audio, holds no radio lease and ends when that job stops:

1. `job start --kind wfm_demod --source S --center-hz 100500000 --bandwidth-hz 200000` (`fm_demod`, `am_demod` or `ssb_demod` for voice channels). Note the job id.
2. `job attach --kind transcribe --job THAT_ID`. Its chain is `stream_in → denoise → vad → transcribe`.
3. `stream list` shows two event streams on the attached job (`vad`'s and `transcribe`'s); `stream tail` them. A `transcript` event is one utterance (`text`, `language`, `start_ns`, `end_ns`); `vad` events mark where speech starts and stops. An empty tail means no utterance ended in the window, not that nothing was said.

The node needs a speech backend (`--stt-socket` or `--stt-node`); without one the attached job fails naming it, so report that and do not retry. Language: the attached job transcribes `en` unless it was attached with `language`; a language the backend does not list (see `GET /stt/languages`) fails the job naming the languages it has. Do not guess a language from the band.

Music and noise are gated, not separated: `vad` passes any loud audio, so a music station or a broadcast with a bed under the voice yields poor or empty text. Say so rather than reporting the text as the speech. Separation is a slot: an external audio-to-audio plugin (`--plugin separate=PATH`, a Demucs wrapper for instance) replaces the `denoise` node with `graph get` of the attached job, edit, `graph put`. The job restarts on `graph put`. `docs/speech.md` has the daemon install and the plugin stub.

`queued_ms` on a transcript is how long it waited for the speech daemon (at most 4 calls at once); `dropped` with `reason: busy` means the daemon had no slot for 30 s, `reason: queue` that the job's own queue of 4 was full. Utterances under `min_utterance_ms` (700) are not sent at all; the job's context counts them under `transcribe.short`.

A `fm_demod` or `am_demod` has an automatic squelch: an idle channel publishes no audio, so a quiet transcript is normal. `squelch` events say why a row is quiet (idle, or open). It opens 6 dB over the tracked noise floor (`squelch_db: null`, the default) and closes after 300 ms under it. To fix or disable it, `graph get`, set `squelch_db` (an absolute threshold in dBFS, -120..=0, not dB above the floor) or `squelch: false` on the demodulator's node, `graph put`; such a put keeps the job re-expandable at later capture changes. A `--then-tail` of an idle squelched channel is empty: that is the squelch, not a fault.

### Reading transcripts

Every transcript event of an attached job is also written to the node's `transcripts.log`. `zeta transcripts tail --lines N --job ID` reads the last lines (default 50, one JSON line each: `job`, `source`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns`, `text`, `language`, `dropped`, `error`), so you can answer "what was said on 476.4 MHz" after the fact without a live `stream tail`. It reads the state directory, so it does not work with `--node`. A line with `error` set is a failed utterance, not silence.

Digital voice decoders (DMR, D-STAR, M17) will be transcribed the same way: they produce audio, so `job attach --kind transcribe` to them.

### Taking the radio from another job

A source holds one tuning, and a second job that needs a different one gets a `conflict` error. Read it before acting: it names the holders (job ids, kinds and what each is doing) and lists options, the last being `retry with replace: [<ids>]`. Decide whether the holders matter. If they do not, or the user asked for the new job, retry the same request with `--replace ID[,ID]` (`job start`, `source ensure`; `replace` is an array in the op). The named jobs are stopped and the new job started in one step under the source's gate, so nothing else can slip in between. Replace is not transactional: if a holder you did not name still conflicts, the jobs you named stay stopped and the new conflict names the remaining holders. Every id must hold that source as a rigid lease (`unknown_job` or `job <id> does not hold source <src>` otherwise); naming an elastic job is that error and nothing is stopped, the elastic job keeps running. Never replace a holder you have not read, and say in the report what you stopped.

The web page's own `spectrum` job is elastic (`job start --elastic`, `spectrum` only): it never conflicts, never blocks a retune and follows each retune of its source, so it is not a holder to wait for or replace. `source list` shows such jobs under `elastic`, not `holders`. A scan on the source stops it, and the page restarts its view afterwards. After a restart of the node the capture is restored (the band bounding a source's rigid jobs, when the radio can capture it) and the jobs resume in it, a scan first, then widest first. `zeta ops tail --lines N` (default 50) shows the last applied ops with the transport that sent them (`local`, `http`, `ui`, `agent`): use it to see who changed the radio.

### Holding a band

To keep many channels running on one radio, pin the capture instead of letting each job retune it: `source pin --source S --center-hz C --bandwidth-hz B` (add `--replace ID` for the jobs in the way). Centre the pin between the channels you want, not on one of them, so no channel sits on the DC spike. Example: UHF CB (80 channels) is held by `--center-hz 476918750 --bandwidth-hz 1000000`; the lease may round up (1 024 000 Hz) and every channel fits. The pin retunes the radio to its band, even one inside a wider capture the page's spectrum holds (the spectrum follows); only when a decoder already holds the source inside the band does the pin join the current capture instead. Jobs started inside the pin run on it without a retune; the pin and the gain (`source gain --source S --db N`, or `--auto`) are saved and come back after a restart, and one-shot commands, read-only ones included, restore the pins and the gain of the sources they attach.

A start outside the pin is a `conflict` offering `unpin source S (source_unpin)` (and `stop one of the holders` only when jobs hold the source). A pin is not a holder and is not in `--replace`. Unpinning frees the radio for other work and does not stop the jobs that were inside it; they conflict with the next retune. Read the conflict, and unpin only when the user wants the pinned channels given up.

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
