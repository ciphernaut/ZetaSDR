# ZetaSDR

ZetaSDR is an agentic RF runtime: a node that owns signal sources, keeps a live picture of the
spectrum and exposes "what is there, what is it, decode it" as typed operations that a human, a
script or an LLM can invoke. This branch is the core node: ports and leases, IQ ring, stream bus,
jobs, the operation set, a SigMF file source and the `zeta` CLI.

Design: [docs/superpowers/specs/2026-10-18-zetasdr-design.md](docs/superpowers/specs/2026-10-18-zetasdr-design.md).
Plan: [docs/superpowers/plans/2026-10-18-core-node.md](docs/superpowers/plans/2026-10-18-core-node.md).

## Tests

The toolchain is pinned by `rust-toolchain.toml` (1.97); install `rustfmt` and `clippy` for it
(`rustup component add rustfmt clippy --toolchain 1.97`), then:

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## CLI walkthrough

One-shot commands open the node, apply one operation, persist and exit; they do not resume jobs.
`zeta serve` resumes persisted jobs and keeps them running until Ctrl-C. A one-shot `iq read` or
`classify` waits (up to 5 s) for the source ring to cover the requested span.

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
