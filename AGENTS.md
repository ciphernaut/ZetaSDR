# ZetaSDR agent guide

ZetaSDR is an agentic SDR runtime. You drive it through a small typed operation set. The CLI form and the JSON form of every operation are equivalent: `zeta <group> <verb> --flag value` and `{"op": "<name>", ...}` do the same thing.

## Rules

- One unit per field: frequencies and bandwidths are integers in hertz in fields ending `_hz`, rates are in baud in fields ending `_baud`, times are integer nanoseconds since the UNIX epoch in fields ending `_ns`.
- Every field is required and unknown fields are rejected.
- Call operations, do not explain.
- Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it.
- Unknown signals are reported as unknown, never guessed.

## Operations

### source_list

JSON: `{"op":"source_list"}`

CLI: `zeta source list`

### source_ensure

- source: string
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"source_ensure","source":"<id>","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta source ensure --source <id> --center-hz 144800000 --bandwidth-hz 12500`

### job_start

- kind: string (JobKind: passthrough)
- source: string
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"job_start","kind":"passthrough","source":"<id>","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta job start --kind passthrough --source <id> --center-hz 144800000 --bandwidth-hz 12500`

### job_list

JSON: `{"op":"job_list"}`

CLI: `zeta job list`

### job_stop

- job: string

JSON: `{"op":"job_stop","job":"<id>"}`

CLI: `zeta job stop --job <id>`

### job_context

- job: string

JSON: `{"op":"job_context","job":"<id>"}`

CLI: `zeta job context --job <id>`

### iq_read

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"iq_read","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta iq read --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

### stream_list

JSON: `{"op":"stream_list"}`

CLI: `zeta stream list`

### graph_get

- job: string

JSON: `{"op":"graph_get","job":"<id>"}`

CLI: `zeta graph get --job <id>`

### graph_put

- job: string
- graph: object (Graph: id, nodes[Node: id, kind, params], edges[Edge: from, to, kind])

JSON: `{"op":"graph_put","job":"<id>","graph":{"id":"<id>","nodes":[{"id":"<node-id>","kind":"passthrough","params":{"kind":"iq"}}],"edges":[]}}`

CLI: `zeta graph put --job <id> --file graph.json`

Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed by `schema`.

### knowledge_lookup

- region: string
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"knowledge_lookup","region":"AU","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta knowledge lookup --region AU --center-hz 144800000 --bandwidth-hz 12500`

### classify

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"classify","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta classify --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

### schema

JSON: `{"op":"schema"}`

CLI: `zeta schema`

## Field craft

Measured facts first, then method. Verify against the node's `knowledge lookup` before relying on any of this.

### Where to look

- Radiosondes: 400-406 MHz. RS41 is GFSK at 4800 baud, about 10 kHz wide.
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
