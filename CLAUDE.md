# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

ZetaSDR is a Rust software-defined radio node. It is driven through one typed operation set from
a CLI, HTTP/websocket, MCP, an LLM agent and a web page. `AGENTS.md` is the reference for
**operating** a node: the operations, the stage kinds and the field craft. This file is about
**developing** the node.

## Commands

The toolchain is pinned in `rust-toolchain.toml` (1.97). The gates CI runs (`.github/workflows/ci.yml`):

```bash
cargo fmt --check                    # in a worktree nested in the checkout: per crate, cargo fmt -p <crate> -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium
cargo test -p zeta --test ui -- --nocapture   # the web page end to end, about 9 min
cargo run -q -p zeta -- agents-md --check
cargo run -q -p zeta -- agents-md --skill --check
bash scripts/public-check.sh         # leak check; needs gitleaks and strings
bash scripts/check-doc-links.sh
```

### One test

`cargo test -p <crate> --test <file> -- --exact <name>`.

Some test targets declare `required-features` and are skipped without them:
- `zeta-core`'s `watch`, `identify` and `transcripts_log` need `--features test-fixtures`;
- `zeta-dsp`'s generators need `test-fixtures`;
- `zeta-agent`'s fake LLM needs `test-fake`.

For example: `cargo test -p zeta-core --features test-fixtures --test watch -- --exact a_lost_signal_stops_its_decoder`.

### The UI end to end

The test runs the first `python3` on `PATH` with Playwright: `uv venv && . .venv/bin/activate && uv pip install playwright && python -m playwright install chromium`.

When it can't run, it **passes** and prints `ui e2e skipped: <reason>`, so check for that line.

### Flaky in CI only

The hosted runners have 2 CPUs. Reproduce a CI-only failure with `taskset -c 0,1` and the test in a loop.

The failures so far have all been tests reading asynchronous state once, right after an operation:
- a lease a cancelled launch still holds;
- a context note a stage writes when it starts.

Poll for the end state (several suites have an `until` helper) instead of reading once.

### Build notes

- **Profile:** the dev profile optimises `zeta-dsp`, `rustfft`, `num-complex` and `nnnoiseless`, because the tests run sources in real time.
- **Disk:** `target/debug` grows to tens of GB. When the disk fills, delete it or old `target/debug/deps` files.
- **Shared target dir:** with one `CARGO_TARGET_DIR` shared between worktrees, cargo can serve a stale artifact of a path crate. Use a private `CARGO_TARGET_DIR` for gate runs, or `touch crates/*/src/lib.rs`.
- **Real SoapySDR hardware:** build with `RUSTFLAGS="--cfg soapy"` and `libsoapysdr-dev` installed. `crates/zeta-source-soapy-driver` links libSoapySDR and stays outside the workspace.

## Architecture

**One operation set.** `zeta-core/src/ops.rs` defines `Op`, and every operation is applied by
`Node::apply` (`zeta-core/src/node/mod.rs`). The transports only translate to and from `Op`:
- the CLI and `serve --listen`, in `crates/zeta`;
- `zeta-ws`, for HTTP and websocket;
- `zeta-mcp`, for MCP;
- `zeta-agent`, the LLM loop that calls the ops as tools;
- the page's static files, in `zeta-ui`.

A new operation or field goes into `ops.rs`. Its prose for agents goes into
`crates/zeta/src/agents_md.rs` (per-operation notes) or `docs/agents/field-craft.md`.

**`AGENTS.md` and `skills/zetasdr/SKILL.md` are generated.** Never edit them by hand.
Regenerate them with `cargo run -q -p zeta -- agents-md > AGENTS.md` and
`cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`. CI fails when they drift.

**Jobs, graphs and stages.**
- A job is a `kind` on a band of a source. `JobKind::expand` (`ops.rs`) turns a built-in kind into a `Graph` of stage nodes.
- A decoder mode is data, not code. It is a row in `modes/*.json`, embedded and expanded by `zeta-core/src/modes/`, plus the operator's own rows from `--modes-dir`.
- A row runs either an in-process graph or an external program through `zeta-ext` (dump1090, the rs1729 sonde decoders). `zeta modes check` tests rows against synthesised captures.
- Stages implement `Stage` (`zeta-core/src/stage.rs`); most live in `zeta-dsp`. They exchange `Frame`s over the `StreamBus` (`zeta-core/src/stream.rs`).
- External stages are plugin processes speaking the stdio protocol in `zeta-plugin`.
- `zeta-core/src/node/launch.rs` launches graphs. `node/watch.rs` and `node/identify.rs` are controller jobs that start and stop child jobs.

**Sources and the radio.**
- Sources implement `Port` (`zeta-core/src/port.rs`). There is one crate per kind: `zeta-source-file`, `-kiwi`, `-node` (another node's source), `-vrt`, `-soapy`.
- SoapySDR devices run in a respawned `zeta soapy-worker` child (`zeta-source-worker`), so a driver crash faults the source rather than the node. `node/recover.rs` relaunches the jobs that failed when the device comes back.
- `lease.rs` decides who may retune a source, under a per-source gate. Rigid jobs and pins hold a band. Elastic jobs (the page's spectrum) follow the capture. A request that doesn't fit is a `conflict` naming the holders, and `replace` stops them under the same gate.
- Channelised jobs share one polyphase channel bank per capture (`bank.rs`, `zeta-dsp/src/pfb.rs`), or filter directly when the bank can't serve them.

**State.**
- A node persists jobs, pins, gain, the station and captures in its state directory (`node/persist.rs`, `bursts.rs`) and resumes them on restart.
- `zeta serve` owns that directory through `serve.lock`; one-shot commands are then read-only.
- Every applied operation is appended to `ops.log`.

## Repository rules

- **This repository is public**, and the history is published through a sanitising export.
  - In tracked files and commit messages, use roles, never private host names, LAN addresses or real positions: "the field host" in prose, `field-host` in URLs and paths.
  - The demo station is Brisbane Airport (-27.3842, 153.1175, 4 m).
  - `scripts/public-check.sh` must be clean before anything lands on `main`.
- **Contributions arrive as issues and gists** (`CONTRIBUTING.md`). Pull requests are not merged, and CI runs on pushes only.
- **Design history:** each feature has a spec and a plan in `docs/superpowers/{specs,plans}`, referred to by plan number. `TODO.md` holds open work and ideas; `UAT.md` holds acceptance checks against live radios.
- **Private paths:** some paths, such as the field host's deploy helpers, never reach the public export. A commit that touches only them drops out of it.
