# ZetaSDR

ZetaSDR is an agentic RF runtime: a node that owns signal sources, keeps a live picture of the
spectrum and exposes "what is there, what is it, decode it" as typed operations that a human, a
script or an LLM can invoke. This branch is the core node (ports and leases, IQ ring, stream bus,
jobs, the operation set, a SigMF file source and the `zeta` CLI) and the DSP slice (channel
extraction, spectrum, NBFM demodulation, APRS decoding, layer-1 measurement and classifiers).

Design: [docs/superpowers/specs/2026-10-18-zetasdr-design.md](docs/superpowers/specs/2026-10-18-zetasdr-design.md).
Plans: [docs/superpowers/plans/2026-10-18-core-node.md](docs/superpowers/plans/2026-10-18-core-node.md),
[docs/superpowers/plans/2026-10-18-dsp-slice.md](docs/superpowers/plans/2026-10-18-dsp-slice.md).

## Tests

The toolchain is pinned by `rust-toolchain.toml` (1.97); install `rustfmt` and `clippy` for it
(`rustup component add rustfmt clippy --toolchain 1.97`), then:

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## CLI walkthrough

One-shot commands open the node, apply one operation, persist and exit; they do not resume jobs.
`zeta serve` resumes persisted jobs and keeps them running until Ctrl-C. A one-shot `iq read` or
`classify` waits (up to 5 s) for the source ring to cover the requested span. File sources play in
real time and loop. `--source-file` is not persisted: pass it on every invocation.

While `zeta serve` runs it holds `serve.lock` (its pid) in the state directory, and the mutating
one-shot commands (`job start`, `job stop`, `graph put`, `source ensure`) exit 2 with
`{"error": "other", "message": "zeta serve (pid N) owns this state dir"}`. A lock whose pid is no
longer alive is treated as stale. Liveness is checked through `/proc/<pid>`, so this is Linux only.

```bash
cargo build -q -p zeta
cargo run -q -p zeta-source-file --features test-fixtures --example gen_tone -- scratch/fx
A="--state-dir scratch/state --source-file tone=scratch/fx/tone.sigmf-meta"
./target/debug/zeta $A source list
./target/debug/zeta $A job start --kind passthrough --source tone --center-hz 100000000 --bandwidth-hz 48000
./target/debug/zeta $A job list
./target/debug/zeta $A iq read --source tone --start-ns 0 --end-ns 1000000000 --center-hz 100000000 --bandwidth-hz 48000
ls scratch/state/captures               # a .sigmf-meta / .sigmf-data pair
./target/debug/zeta $A job stop --job <id from job start>
./target/debug/zeta agents-md            # the agent guide, rendered from the operation schema
```

`AGENTS.md` is generated from the operation schema by `zeta agents-md`; `zeta agents-md --check`
fails if the committed file has drifted.

## DSP

Five job kinds, chosen with `job start --kind` (`zeta job start --help` lists them):

- `passthrough` copies the channel IQ through unchanged.
- `spectrum` emits one power-spectrum event per 100 ms frame (`size` 1024, `bin_hz`, `db` array).
- `fm_demod` channelises and demodulates narrowband FM to an audio stream (48 kHz, per-frame `peak`).
- `aprs_decode` demodulates AFSK 1200 and HDLC-deframes AX.25, emitting one event per CRC-valid packet.
- `measure` emits one layer-1 feature record per second of IQ (occupied band, SNR, burst, symbol rate, label).

`--then-tail N,MS` on `job start` starts the job and then tails up to N frames from its stream, waiting
up to MS ms; the same thing is available later as `zeta stream tail`. `zeta classify` runs every
classifier (layer 1 measurement, layer 2 signature and decode-to-identify) over a span of a source ring.
Outputs below are trimmed.

```bash
cargo build -q -p zeta
cargo run -q -p zeta-dsp --features test-fixtures --example gen_aprs -- scratch/fx
cargo run -q -p zeta-source-file --features test-fixtures --example gen_tone -- scratch/fx
Z=./target/debug/zeta
S="--state-dir scratch/state"

# APRS: one event, src VK3ABC (dst APRS, info ">hi", fcs_ok true)
$Z $S --source-file aprs=scratch/fx/aprs.sigmf-meta job start --kind aprs_decode --source aprs \
    --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000

# NBFM audio from the tone: "kind": "audio" frames, 4800 samples at 48000 Hz each. The first
# frame's peak is a filter start-up transient (1.07); later frames are 0.40 (between 0.3 and 1.0).
$Z $S --source-file tone=scratch/fx/tone.sigmf-meta job start --kind fm_demod --source tone \
    --center-hz 100000000 --bandwidth-hz 12500 --then-tail 5,3000

# spectrum: one event, "size": 1024, "bin_hz": 46.875, "db": [-180.99, -179.14, -181.65, ...]
$Z $S --source-file tone=scratch/fx/tone.sigmf-meta job start --kind spectrum --source tone \
    --center-hz 100000000 --bandwidth-hz 12500 --then-tail 1,3000

# measure on the tone: the tone is an unmodulated carrier, so label.name is "carrier"
# (confidence 0.7, snr_db 157.4, symbol_rate_baud null)
$Z $S --source-file tone=scratch/fx/tone.sigmf-meta job start --kind measure --source tone \
    --center-hz 100000000 --bandwidth-hz 12500 --then-tail 1,5000

# measure on the APRS fixture: symbol_rate_baud 1200 in both records. The first second is labelled
# "fsk" (audio tones 1125 Hz and 2203 Hz under fm), the next "fm" (rms deviation 894 Hz); the
# packet does not fill the whole window, so the label depends on the second measured.
$Z $S --source-file aprs=scratch/fx/aprs.sigmf-meta job start --kind measure --source aprs \
    --center-hz 144800000 --bandwidth-hz 12500 --then-tail 2,5000

# classify: layer 1 "fm" (0.6) plus layer 2 "APRS" (1.0, "1 AX.25 frames with valid FCS; first VK3ABC>APRS")
$Z $S --source-file aprs=scratch/fx/aprs.sigmf-meta classify --source aprs \
    --start-ns 0 --end-ns 2000000000 --center-hz 144800000 --bandwidth-hz 12500
```

### GPU

The FFT stage has a GPU implementation behind the `gpu` cargo feature (wgpu compute shader;
`cargo build -p zeta --features gpu`, tests with `cargo test -p zeta-dsp --features gpu`). With
the feature on, the node adds the GPU backend only when a hardware adapter is found, and the
scheduler then prefers it; without one the CPU FFT runs, so a missing GPU never fails a job.
`gpu_matches_cpu_within_0_1_db` must be run on a GPU host to count as verification (hardware equivalence is pending on the field host): elsewhere it prints `skipped: no adapter` and passes. Software adapters (lavapipe, llvmpipe) are ignored unless `ZETA_GPU_ALLOW_SOFTWARE=1`;
`software_adapter_allowed_by_env` sets it, so a GPU-less box with Mesa still runs the GPU path. A
GPU failure at run time is logged to stderr and that spectrum is computed on the CPU.

## Transports

Every transport is a thin adapter over the same `Node::apply`, so the CLI, HTTP, MCP and the
in-process agent run the same operations. Nothing is exposed without a shared secret.

`zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) also serves
an authenticated HTTP and websocket API. It needs a bearer secret, from `--secret` or
`ZETA_SECRET` (prefer the variable: `--secret` is visible in `ps`; secrets under 12 characters
draw a warning). Every request, `/health` included, must carry `Authorization: Bearer <secret>`,
else it gets 401.

| Route | |
|---|---|
| `GET /health` | status, version, uptime, websocket connections |
| `POST /op` | apply one operation (`{"op": ...}` JSON) |
| `GET /schema` | the operation schema |
| `GET /streams` | live streams |
| `GET /agents-md` | the node's agent guide |
| `GET /stream/{id}` | websocket of one stream |
| `GET /streams/{id}/tail?max_frames=N&timeout_ms=T` | up to N recent frames as a JSON array |

The websocket sends text messages tagged by `type`: `{"type":"frame", ...}` carries a `WsFrame`
(`stream`, `kind`, `source`, `job`, `channel`, `start_ns`, `end_ns` and a `payload` summary; sample
data does not cross the wire), `{"type":"lagged","missed":N}` reports dropped frames and
`{"type":"closed","reason":...}` ends the stream.

`zeta serve --listen` runs in the foreground, so run the commands that follow in a second terminal.

```bash
A="--state-dir scratch/state3 --source-file tone=scratch/fx/tone.sigmf-meta"
ZETA_SECRET=correct-horse-battery ./target/debug/zeta $A serve --listen 127.0.0.1:7979

export ZETA_NODE=http://127.0.0.1:7979 ZETA_SECRET=correct-horse-battery
./target/debug/zeta job start --kind passthrough --source tone --center-hz 100000000 --bandwidth-hz 48000
./target/debug/zeta job list        # applied in the serving node; the job keeps running
curl -s -H "Authorization: Bearer $ZETA_SECRET" $ZETA_NODE/health
```

`--node URL --secret SECRET` (or `ZETA_NODE` and `ZETA_SECRET`) sends any CLI operation to a
running node instead of opening a local one. This is the way to change a node that `zeta serve`
owns. `--state-dir` and `--source-file` belong to the serving node and are rejected.

`zeta-mcp` is a stdio MCP server with one tool per operation, generated from the schema, with
`AGENTS.md` as its instructions. Register it with Claude Code:

```bash
claude mcp add zeta -e ZETA_SECRET=… -- zeta-mcp --node http://127.0.0.1:7979
```

This needs `zeta-mcp` on `PATH` (for example `cargo install --path crates/zeta-mcp`), or give the
full path to `target/debug/zeta-mcp` in its place.

To give Claude Code the field guide as a skill, install `skills/zetasdr` (generated by
`zeta agents-md --skill`) into `~/.claude/skills/` or a project's `.claude/skills/`.

`zeta ask` lets a local model satisfy a request by calling the operations, against any
OpenAI-compatible endpoint (thinking off, token budget per turn, bounded turns, readiness wait):

```bash
./target/debug/zeta ask "what sources are there?" --llm-url http://127.0.0.1:8080 --llm-model gemma-4-12b
```

`zeta ask` reads its request from stdin, so a `local-voice` transcript can be piped in.
