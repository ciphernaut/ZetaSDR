# ZetaSDR agent guide

ZetaSDR is an agentic SDR runtime. You drive it through a small typed operation set. The CLI form and the JSON form of every operation are equivalent: `zeta <group> <verb> --flag value` and `{"op": "<name>", ...}` do the same thing.

## Rules

- One unit per field: frequencies and bandwidths are integers in hertz in fields ending `_hz`, rates are in baud in fields ending `_baud`, times are integer nanoseconds since the UNIX epoch in fields ending `_ns`.
- Every field is required and unknown fields are rejected.
- Call operations, do not explain.
- Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it.
- Unknown signals are reported as unknown, never guessed.
- Ids are never invented: `source` values come from `source list`, `job` values from `job list`.

## Running the node

Every command takes two global flags:

- `--state-dir PATH`: where jobs and captures persist (default `$XDG_STATE_HOME/zetasdr`, else `~/.local/state/zetasdr`). Pass the same directory on every invocation.
- `--source-file ID=PATH.sigmf-meta`: attach a SigMF recording as source `ID`. Sources are not persisted: repeat the flag, with the same ids, on every invocation, `serve` included.

There are two ways to run:

- One-shot, `zeta <group> <verb> ...`: opens the node, applies one operation, prints the JSON result and exits (0 on success, 2 with a JSON error). It never resumes jobs: a job started one-shot is persisted as `running` and runs once `zeta serve` starts. A one-shot `iq read` or `classify` reads the ring that the command itself fills from the source, waiting up to 5 s for it to cover the span.
- `zeta serve`: resumes persisted jobs and keeps them running until Ctrl-C.

While `zeta serve` runs it owns the state directory (it holds `serve.lock` there). Mutating one-shot commands (`job start`, `job stop`, `graph put`, `source ensure`) are then refused with an `other` error naming the server's pid; read-only commands still work.

## Operations

### source_list

JSON: `{"op":"source_list"}`

CLI: `zeta source list`

### source_ensure

Check a band against the source and tune it if no job holds it. Takes no lease: an unheld source is tuned directly (a later `job_start` may retune it); a held source is never retuned, and the request either fits the current band (`Tuning`) or is a `Conflict`. Runs under the lease manager's per-source gate, so it never interleaves with a job's lease-and-tune.

- source: string; an id from `source list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"source_ensure","source":"<id>","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta source ensure --source <id> --center-hz 144800000 --bandwidth-hz 12500`

### job_start

- kind: string (JobKind: passthrough|spectrum|fm_demod|aprs_decode)
- source: string; an id from `source list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"job_start","kind":"passthrough","source":"<id>","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta job start --kind passthrough --source <id> --center-hz 144800000 --bandwidth-hz 12500`

### job_list

JSON: `{"op":"job_list"}`

CLI: `zeta job list`

### job_stop

- job: string; an id from `job list`

JSON: `{"op":"job_stop","job":"<id>"}`

CLI: `zeta job stop --job <id>`

### job_context

- job: string; an id from `job list`

JSON: `{"op":"job_context","job":"<id>"}`

CLI: `zeta job context --job <id>`

### iq_read

Save a selection under `state_dir/captures`: writes the whole ring bandwidth over the span as a SigMF pair; the selection band is recorded as an annotation. The capture is labelled with the ring's centre, and the selection band must lie inside the ring's band.

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"iq_read","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta iq read --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

### stream_list

JSON: `{"op":"stream_list"}`

CLI: `zeta stream list`

### graph_get

- job: string; an id from `job list`

JSON: `{"op":"graph_get","job":"<id>"}`

CLI: `zeta graph get --job <id>`

### graph_put

Replace a job's graph; restarts a running job under the same id with the new graph.

- job: string; an id from `job list`
- graph: object (Graph: id, nodes[Node: id, kind, params], edges[Edge: from, to, kind])

JSON: `{"op":"graph_put","job":"<id>","graph":{"id":"<id>","nodes":[{"id":"<node-id>","kind":"passthrough","params":{"kind":"iq"}}],"edges":[]}}`

CLI: `zeta graph put --job <id> --file graph.json`

Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed under Stage kinds below and by `schema`.

### knowledge_lookup

Band-plan allocations in `region` and known signatures overlapping the band, from every registered knowledge provider.

- region: string; free-text region code such as `AU`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"knowledge_lookup","region":"AU","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta knowledge lookup --region AU --center-hz 144800000 --bandwidth-hz 12500`

The built-in provider has signatures but no allocations, so an empty `allocations` list means no provider data, not nothing allocated.

### classify

Run every registered classifier over the selection's span of the source ring (no upstream features).

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"classify","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta classify --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

### schema

JSON: `{"op":"schema"}`

CLI: `zeta schema`

## Stage kinds

A graph node's `kind` names one of these; its `params` must match the listed fields.

### passthrough

- inputs: iq, audio, video, text, event, sink
- outputs: iq, audio, video, text, event, sink
- params.kind: string (StreamKind: iq|audio|video|text|event|sink)

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
