# Public Release Preparation Plan (plan 25)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The ZetaSDR tree can be published as a public repository: nothing in it names the operator's hosts, home position or private services, every bundled third-party file is credited, a stranger can build and test it from the README alone, CI gates every push, and a script exports the tree as a fresh one-commit history ready to push.

**Architecture:** Four kinds of change, in order. (1) A leak gate (`scripts/public-check.sh`) that fails on private names and precise coordinates, so every later task proves itself against it. (2) Removal and scrubbing: private ops files leave the tree, hostnames become roles, the home position becomes a public demo station. (3) Release furniture: notices, crate metadata, community files, CI. (4) An export script that builds the public history from a scrubbed `HEAD` and re-runs every gate in the export, so the private repository and its 616 commits stay as they are.

**Tech Stack:** Rust workspace (19 crates, edition 2024, Apache-2.0), bash, `gitleaks` 8 (installed at `~/.local/bin/gitleaks`), `cargo-deny`, GitHub Actions, Playwright for the UI e2e.

**Spec:** none; this plan is its own spec. The audit it rests on (2026-11-03, `main` at 8215ccd) is summarised in "Audit findings" below.

## Audit findings (2026-11-03)

- **Private hosts:** the field host's name in 51 tracked files (130 hits under `docs/superpowers` alone; code: `crates/zeta-agent/tests/live.rs:13-16`, comments in `crates/zeta-dsp/src/mix.rs:33`, `crates/zeta-dsp/tests/detect_noise.rs:2-11`, `crates/zeta-dsp/tests/pfb.rs:199`); the ACMA host's `.local` name in 5 files, `.mcp.json` among them (its `/mcp` URL).
- **Home position:** `UAT.md:285` gives the station to six decimals; history also carries five other precise points near it. Coarse points near it appear in `AGENTS.md:197` (generated), `crates/zeta-core/tests/station.rs:32,47`, `crates/zeta-ext/src/acma.rs:671-673`.
- **Operator-only files:** `deploy/<field-host>/` (4 scripts), `UAT.md` (acceptance log with the station), `scratch/` (already ignored; `scratch/live/record-noise.sh` exists only in history).
- **Secrets:** `gitleaks git` over all history finds one hit, a redacted placeholder in `docs/superpowers/plans/2026-10-19-ui.md:70` (false positive). Test secrets (`s3cret`) are fixtures. No keys, no private-key blocks.
- **Identity:** every commit is `Sage Grigull <ciphernaut@proton.me>`.
- **Third-party content:** `crates/zeta-dsp/data/signals.json` (iNTERCEPT, Apache-2.0, has `NOTICE`), `crates/zeta-ui/web/vendor/leaflet` (BSD-2, has `LICENSE`), `crates/zeta-ui/web/vendor/antonio` (SIL OFL 1.1, has `OFL.txt`). GPL decoders (dump1090, rs1729) run as external programs, never linked. No top-level `NOTICE`.
- **Missing:** `.github/`, `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.md`, `repository`/`homepage`/`readme`/`keywords` in the crates' `Cargo.toml`, a dependency licence check.
- **Re-audit (2026-11-04, `main` at 04e50bb, 88 commits later):** the field host's name in 53 tracked files (238 hits; new code comments in `crates/zeta-dsp/src/bit_view.rs` and `crates/zeta-dsp/tests/layer1.rs`), the ACMA host's name in 6; the coordinates as before plus `crates/zeta/tests/cli.rs:2870` (a synthesised sonde fix near the old home) and the register fixture sites in `crates/zeta-ext/tests/acma.rs:59-64` (public register data; they stay unless a distance to the old home sits beside them). `gitleaks git` over the whole history: still the one false positive. `docs/architecture` was already made host-free on `main` (b4ade27, 04e50bb). `README.md` is 882 lines of reference.
- **Size:** `.git` is 32 MB; the largest blob is `signals.json` (794 KB). Size is no obstacle.

## Rulings to confirm (defaults the plan takes)

These are the operator's calls; the plan proceeds on the default unless told otherwise.

1. **History: superseded 2026-11-05 by the operator: keep the full history, sanitised (Task 10); the one-commit export stays as `--squash`.** Original default: a fresh root commit (Task 7), not `git filter-repo`. The home position, hostnames and 130 plan-doc mentions are spread over 616 commits; rewriting them is error-prone and a missed blob is public forever. The private repository keeps the full history. Alternative: filter-repo with the same denylist, then gitleaks over the result.
2. **Design history stays public, scrubbed:** `docs/superpowers/{specs,plans}`, `TODO.md`, `RECIPES.md` and `docs/architecture` are kept with hosts replaced by roles. Alternative: drop `docs/superpowers` from the public tree.
3. **Leave the public tree:** `deploy/<field-host>/`, `UAT.md`, `.mcp.json`. They stay tracked on the private `main` and are dropped from the export by the operator's exclude file (Global Constraints; Tasks 1, 2, 7).
4. **No crates.io publish** in this plan: every crate gets `publish = false`. A publish plan comes later if wanted.
5. **Public identity:** the export commit is authored `Sage Grigull <ciphernaut@proton.me>`, as the history already is. Change `PUBLIC_AUTHOR` in Task 7 if another identity is wanted.
6. **Host: GitHub**, owner `ciphernaut` (as `github.com/ciphernaut/acma-local`), so `https://github.com/ciphernaut/ZetaSDR`; CI on GitHub Actions, private vulnerability reporting for `SECURITY.md`.
7. **Demo station:** the public examples and tests use Brisbane Airport, `-27.3842, 153.1175`, 4 m (a public landmark already named in the field craft for sonde launches).
8. **Contributions (operator, 2026-11-04): feedback and gists only at this stage, no pull requests.** Feedback and ideas arrive as GitHub issues from two forms, *Feedback* (a report: what was tried, what happened) and *Gist* (an idea or a change sketched in prose, optionally linking a GitHub gist with a patch); the maintainer writes the code. `CONTRIBUTING.md` says so first, a pull-request template points a PR's author at the Gist form, and CI runs on push only. Discussions stay off until asked for.
9. **Documentation: the README becomes a front page.** It keeps what a newcomer reads first (what it is, status, quick start without a radio, with a radio, where the docs are, legal, feedback); the reference sections move unchanged in substance to `docs/guide.md`, and `docs/README.md` indexes the docs.

## Global Constraints

- Work on branch `worktree-public-release-plan` in its worktree; one commit per task with the message the task names, each ending with the session attribution line. No push, no merge, no deploy: the operator does those (Task 8).
- The private denylist never enters the tree: `scripts/public-check.sh` reads it from the file named by `ZETA_PUBLIC_DENYLIST` (default `$XDG_CONFIG_HOME/zetasdr/public-denylist`, else `~/.config/zetasdr/public-denylist`), one extended regex per line, `#` comments. Without the file the script still runs the built-in checks and prints `public-check: no private denylist; built-in checks only`.
- Private-only paths stay tracked on the private `main` (ruling 3) and never reach the export: `scripts/public-check.sh` skips, and `scripts/export-public.sh` deletes after `git archive`, every path matched by the globs in the file named by `ZETA_PUBLIC_EXCLUDE` (default `$XDG_CONFIG_HOME/zetasdr/public-exclude`, else `~/.config/zetasdr/public-exclude`; one `git ls-files` pathspec glob per line, `#` comments), so the list of private paths is not in the tree either. The operator's file holds `deploy/<field-host>/**`, `UAT.md` and `.mcp.json`. The export then runs the check with no exclude file, so a missing exclusion fails there.
- Built-in checks (public, in the script): latitude/longitude pairs with 4 or more decimals (`-?[0-9]{1,3}\.[0-9]{4,}, ?-?[0-9]{1,3}\.[0-9]{4,}`) outside an allowlist file `scripts/public-allow.txt` (`path:regex` lines; the demo station and synthesised test values go there), any `*.local` hostname with a port (`[a-z0-9-]+\.local:[0-9]+`), and `gitleaks dir` with `.gitleaks.toml`.
- Hosts become roles, consistently: the field host's name → `the field host` in prose, `field-host` in URLs and paths (`http://field-host:7979`); the ACMA host's `.local` name and port → `acma-host:3110`. Hardware names (B210, HackRF) stay: they are the facts that make the measurements reproducible.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: edit their sources (`docs/agents/*.md`, op docs in code) and regenerate with `cargo run -p zeta -- agents-md`; `zeta agents-md --check` must pass.
- Gates per task: `bash scripts/public-check.sh` (from Task 1 on; it may list findings only in files a later task owns, and the task's report names them), `cargo fmt -p <touched crates> -- --check` (per member: the nested worktree breaks `--all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`. The UI e2e (`cargo test -p zeta --test ui`) only in Tasks 5 and 7.
- Temp files under the job's tmp directory, never `/tmp`.

## Review Focus

1. **A leak in a file the gate does not scan.** Binary and generated files (`docs/architecture/zetasdr-architecture.svg`, `index.html`, `.png` screenshots) carry text too. The gate scans every tracked file `git ls-files` lists, binaries via `strings`. Pinned in Task 1 (`svg_and_binary_are_scanned`).
2. **The denylist leaking itself.** A failure message must print the file and line of the hit, never the pattern that matched. Pinned in Task 1 (`message_omits_pattern`).
3. **A scrub that breaks a test's arithmetic.** Moving the station moves every expected distance and bearing; recomputed, not edited by hand. Pinned in Task 3 (the `acma.rs` haversine/bearing test against an independent value).
4. **An export that differs from what was checked.** The export must come from `git archive` of a committed `HEAD` (no untracked or ignored file, no `target/`), and every gate runs again inside the export. Pinned in Task 7 (`export_has_no_ignored_files`, gates in the export).
5. **Live tests that now point nowhere.** `crates/zeta-agent/tests/live.rs` must skip with a reason when `ZETA_LIVE_LLM_URL` is unset, not fail or hang on a dead default. Pinned in Task 2.

---

### Task 1: The leak gate (Sonnet)

**Files:**
- Create: `scripts/public-check.sh`, `scripts/public-allow.txt`, `.gitleaks.toml`, `scripts/tests/public-check.sh`

**Interfaces:**
- Produces: `bash scripts/public-check.sh [ROOT]` (default the repo root): exit 0 and `public-check: clean (<n> files)`; exit 1 with one line per hit, `public-check: <path>:<line>: <check>` where `<check>` is `denylist`, `coordinates`, `local-host` or `gitleaks:<rule>`. Every later task runs it.

- [ ] **Step 1: Write the failing tests** in `scripts/tests/public-check.sh` (plain bash, builds fixture repos in `$CLAUDE_JOB_DIR/tmp` or `mktemp -d` under `${TMPDIR}`): `clean_tree_passes`; `denylist_hit_fails` (denylist `secret-host`, file containing `ssh secret-host` → exit 1, line `a.md:1: denylist`); `message_omits_pattern` (that output does not contain `secret-host`); `coordinates_fail_unless_allowed` (`-27.384212, 153.117501` fails; the same line allowed in `public-allow.txt` passes); `local_host_with_port_fails` (`http://box.local:8080`); `svg_and_binary_are_scanned` (hit inside an `.svg` and inside a `.png` built with `printf`); `no_denylist_still_runs` (message and exit 0 on a clean tree); `gitleaks_hit_fails` (a fake AWS key string `AKIA` + 16 caps); `excluded_paths_are_skipped` (a denylist hit in `private/x.sh` with `private/**` in the exclude file passes; the same tree without the exclude file fails). The test script builds every leak-shaped string (coordinates, `AKIA…`, `.local:` hosts) at run time by concatenation, so the gate does not flag its own tests. Exit non-zero on the first failing case, print `ok <name>` per pass.
- [ ] **Step 2: Run** `bash scripts/tests/public-check.sh`. Expected: fails (script missing).
- [ ] **Step 3: Implement `scripts/public-check.sh`**: `git -C ROOT ls-files` for the file list, `grep -nE` per pattern (text) and `strings | grep` for files `grep -I` calls binary; gitleaks as `gitleaks dir --no-banner --redact -c .gitleaks.toml -r <tmp json> ROOT`, mapped to the output format with `python3 -I`. `.gitleaks.toml` extends the default config and allowlists `docs/superpowers/plans/2026-10-19-ui.md` line 70's placeholder by regex and the `s3cret` fixtures by path.
- [ ] **Step 4: Run** the test script. Expected: every `ok`. Then run `bash scripts/public-check.sh` on the repo with the operator's denylist and record the hit count per file in the task report (expected: dozens, owned by Tasks 2-3).
- [ ] **Step 5: Commit** `release: a public-check gate for private names, precise coordinates and secrets`.

### Task 2: Operator-only files stay private, the live test needs no private host (Sonnet)

**Files:**
- Create: `.mcp.json.example` (`{"mcpServers": {"acma": {"type": "http", "url": "http://acma-host:3110/mcp"}}}`), `deploy/README.md`, the operator's `~/.config/zetasdr/public-exclude` (outside the tree: `deploy/<field-host>/**`, `UAT.md`, `.mcp.json`)
- Modify: `crates/zeta-agent/tests/live.rs:13-16`, every public doc line that links or cites the private paths (`git grep -n 'deploy/<field-host>\|UAT\.md\|\.mcp\.json'` outside them and outside `docs/superpowers`)

**Interfaces:**
- Consumes: Task 1's gate and its `ZETA_PUBLIC_EXCLUDE` handling.
- Produces: the private files untouched and still tracked on the private `main` (ruling 3); `bash scripts/public-check.sh` lists no hit inside them because the exclude file skips them.

- [ ] **Step 1:** Write the operator's exclude file if it is missing; `bash scripts/public-check.sh` no longer lists `deploy/<field-host>/`, `UAT.md` or `.mcp.json`.
- [ ] **Step 2:** `live.rs`: the test reads `ZETA_LIVE_LLM_URL` and `ZETA_LIVE_LLM_MODEL`; unset, it prints `skipped: set ZETA_LIVE_LLM_URL` and returns (keep `#[ignore]`, reason `needs an LLM at ZETA_LIVE_LLM_URL`); rename the fn `live_llm_tunes_a_source`. Run `cargo test -p zeta-agent --test live -- --ignored` with the variable unset: passes, prints the skip.
- [ ] **Step 3:** `deploy/README.md`: `deploy/pi` is the supported example; a host's own scripts are the operator's and are not published. Re-point the public doc links found above (README, RECIPES, docs/) at `deploy/README.md` or drop them; acceptance prose that cited `UAT.md` cites nothing. `.mcp.json.example` is what a public reader copies to `.mcp.json`.
- [ ] **Step 4:** Gates; `public-check` shows no hit in a file this task touched.
- [ ] **Step 5: Commit** `release: the live LLM test reads its URL from the environment, an MCP config example, deploy/README.md, and no public doc links the operator's private files`.

### Task 3: Scrub hosts and the home position (Opus: many files, arithmetic in tests)

**Files:**
- Modify: `crates/zeta-core/tests/station.rs:32,47`, `crates/zeta/tests/cli.rs:2870` (allowlist it as synthesised, or move it to the demo station), `crates/zeta-dsp/src/bit_view.rs`, `crates/zeta-dsp/tests/layer1.rs`, `crates/zeta-ext/src/acma.rs:665-680`, `crates/zeta-core/src/modes/summary.rs:267-268` (allowlist instead if it is a synthesised value), `crates/zeta-dsp/src/mix.rs:33`, `crates/zeta-dsp/tests/detect_noise.rs:1-15`, `crates/zeta-dsp/tests/pfb.rs:199`, the op doc for `station_set` (source of `AGENTS.md:197`), `docs/agents/*.md`, `docs/speech.md`, `README.md`, `RECIPES.md`, `TODO.md`, `docs/architecture/{gen.py,index.html,zetasdr-architecture.svg}`, `docs/superpowers/**`; regenerated `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Modify: `scripts/public-allow.txt` (the demo station, synthesised test coordinates)
- Also found by the 2026-11-04 denylist: `crates/zeta-ext/tests/sink.rs`, `crates/zeta-ui/web/view.js`, `crates/zeta-ui/tests/e2e/ui.py`, `crates/zeta/tests/ui.rs`, `crates/zeta/src/agents_md.rs`, the field-craft example's register line (a site and its distance from the old home: give a synthetic site and its distance from the demo station, recomputed), and a LAN address in `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md:729` (→ `<host-ip>`). The run's denylist is `~/.config/zetasdr/public-denylist` (case-insensitive); the task ends with `public-check` clean with it and with the exclude file.

**Interfaces:**
- Consumes: Task 1's gate and the host→role mapping in Global Constraints.

- [ ] **Step 1: Coordinates in code.** `station.rs` uses `-27.3842, 153.1175, 4.0`. `acma.rs`'s distance/bearing test keeps a site but measures from the demo station; compute the expected `haversine_km` and `bearing_deg` with an independent formula (`python3 -I -c` with `math`, spherical earth R = 6371.0088 km as the code uses; check its constant) and assert to the test's existing tolerance. Run `cargo test -p zeta-core --test station` and `cargo test -p zeta-ext acma`: pass.
- [ ] **Step 2: Hosts in code comments and docs**, scripted then read: `git grep -lE '<the private host names>'` → `sed` per the mapping, then read every changed hunk (`git diff --word-diff`) and fix grammar ("on <host>'s B210" → "on the field host's B210"). The `station set` CLI example becomes `--lat -27.38 --lon 153.12 --alt-m 4`. `docs/architecture/gen.py` is edited and run to regenerate the svg and html, not the outputs by hand.
- [ ] **Step 3: Precise coordinates in prose** (`docs/superpowers/**`, `TODO.md`, `RECIPES.md`): replace each with the demo station or `<station>`; a transmitter site from the public register may stay only if its distance to the old home is not given beside it (else replace both).
- [ ] **Step 4:** Regenerate: `cargo run -p zeta -- agents-md`, then `cargo run -p zeta -- agents-md --check` passes; the skill regenerated by the same path the plan 24 docs commit used.
- [ ] **Step 5:** `bash scripts/public-check.sh` with the operator's denylist: `clean`. Full gates.
- [ ] **Step 6: Commit** `release: hosts become roles and the station a public demo position, in code, docs and the design history`.

### Task 4: Notices, crate metadata and the licence check (Sonnet)

**Files:**
- Create: `NOTICE`, `deny.toml`
- Modify: `Cargo.toml` (`[workspace.package]`: `repository`, `homepage`, `readme = "README.md"`, `authors`), every `crates/*/Cargo.toml` (`repository.workspace = true`, `homepage.workspace = true`, `publish = false`, `keywords`, `categories` where it helps: `["sdr", "radio", "dsp"]`, `["command-line-utilities"]` for `zeta`)

**Interfaces:**
- Produces: `cargo deny check licenses bans sources` passing; CI (Task 6) runs it.

- [ ] **Step 1: `NOTICE`**: ZetaSDR copyright line (`Copyright 2026 Sage Grigull`), then one section per bundled third party with path, licence and copyright: `crates/zeta-dsp/data/signals.json` (iNTERCEPT, Apache-2.0, from its `NOTICE`), `crates/zeta-ui/web/vendor/leaflet` (Leaflet 1.9.4, BSD-2-Clause), `crates/zeta-ui/web/vendor/antonio` (Antonio, SIL OFL 1.1); and a paragraph that dump1090 and the rs1729 decoders are GPL programs run as separate processes, not distributed or linked.
- [ ] **Step 2: `deny.toml`**: allow `Apache-2.0`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-3.0`, `Zlib`, `MPL-2.0`, `CC0-1.0` and whatever `cargo deny check licenses` reports as in use that is permissive (add with a comment naming the crate); deny copyleft; `sources` allow crates.io only. Install `cargo-deny` if missing (`cargo install cargo-deny --locked`) and name it in the report. Run: passes. A GPL/LGPL/AGPL dependency found here stops the task and is reported, not allowlisted.
- [ ] **Step 3:** Metadata as listed; the repository URL is `https://github.com/<OWNER>/ZetaSDR` with `<OWNER>` taken from the operator (ruling 6); until given, use the placeholder and list it in the report. `cargo metadata --format-version 1 >/dev/null` succeeds; `cargo package -p zeta-core --no-verify --allow-dirty --list` shows `README.md` resolved.
- [ ] **Step 4:** Gates. **Commit** `release: NOTICE for the bundled data, map and font, crate metadata, and a cargo-deny licence check`.

### Task 5: Polished documentation and the feedback-only community files (Opus: reader's-eye writing over 2 000 lines)

**Files:**
- Modify: `README.md` (rewritten as the front page), `docs/agents/*.md` and op docs only where a link moves
- Create: `docs/guide.md` (the README's reference sections from `## Tests` on, moved), `docs/README.md` (an index of every doc: the guide, `AGENTS.md`, `RECIPES.md`, `docs/speech.md`, `docs/themes.md`, `docs/architecture`, and the design history under `docs/superpowers` with one line on what it is), `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.md`, `.github/ISSUE_TEMPLATE/feedback.yml`, `.github/ISSUE_TEMPLATE/gist.yml`, `.github/ISSUE_TEMPLATE/config.yml`, `.github/pull_request_template.md`, `scripts/check-doc-links.sh`, `scripts/tests/check-doc-links.sh`

**Interfaces:**
- Consumes: Tasks 1-4 (the gate, the scrubbed tree, `NOTICE`).
- Produces: `bash scripts/check-doc-links.sh [ROOT]` (plain bash + `python3 -I`): every relative Markdown link and `#anchor` in tracked `*.md` outside `docs/superpowers` resolves; exit 1 listing `path:line: <link>`, else `doc-links: clean (<n> links)`. CI (Task 6) runs it.

- [ ] **Step 1: The front page.** `README.md` at most ~200 lines: what ZetaSDR is (an agentic SDR runtime: a node, a typed operation set, a web page, MCP and an LLM agent) and what it is for, in one paragraph and the architecture figure; **Status** (`0.1.0`, Linux, pre-1.0: operations may change; feedback wanted, contributions as gists); **Quick start without a radio** (build `cargo build --release -p zeta`, run against a recording the tests already use (find the fixture `crates/zeta/tests` uses; if none is small enough to name, use what the test harness synthesises and say so), open the page, a first `job start`; a first `zeta ask` only if it needs no private service); **With a radio** (`--source-soapy` with `RUSTFLAGS="--cfg soapy"`, `libsoapysdr-dev`; a KiwiSDR as the no-hardware alternative); **Documentation** (link to `docs/README.md` and the guide's main sections); **Legal** (the field-craft paragraph on interception law, short: public protocols vs third-party content, the operator's responsibility, SondeHub uploads only with a callsign; the licence and `NOTICE`); **Feedback and contributions** (three lines pointing at `CONTRIBUTING.md`).
- [ ] **Step 2: The guide.** Move everything from `## Tests` on into `docs/guide.md` under a title and a table of contents; fix relative links for the new depth; read it once as a stranger and fix only what a stranger trips on: a plan number as the only name of a feature, internal shorthand, a forward reference with no link. Substance and examples stay; this is not a rewrite. Re-point every link into the old README sections (`git grep -n 'README.md#'`, op docs included); regenerate `AGENTS.md` and the skill if a source changed (`agents-md --check` passes).
- [ ] **Step 3: Community files.** `CONTRIBUTING.md` opens with the rule: *at this stage ZetaSDR takes feedback and gists, not pull requests*; how to send each (the two issue forms); what makes a useful one (version or commit, source kind, the command and its JSON output, what was expected); that a gist may link a GitHub gist with a patch, which the maintainer may adopt, credited in the commit (`Suggested-by:`); that anything sent is offered under Apache-2.0 (inbound = outbound). Then, for whoever builds it locally, the gates (per-member `cargo fmt`, clippy `-D warnings`, `cargo test --workspace`, the UI e2e and its Playwright setup, `zeta agents-md --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`). `.github/ISSUE_TEMPLATE/feedback.yml` and `gist.yml`: issue forms with required fields (what was tried and what happened / the idea and why), an optional version field, `labels: [feedback]` / `[gist]`. `config.yml`: `blank_issues_enabled: false`, a contact link to `SECURITY.md` for vulnerabilities. `.github/pull_request_template.md`: thanks; pull requests are not merged at this stage; please open a Gist issue linking this diff. `SECURITY.md`: report via GitHub private vulnerability reporting; scope notes (the node secret, `--listen` refuses non-LAN addresses, plugins and handlers run with a cleared environment). `CODE_OF_CONDUCT.md`: Contributor Covenant 2.1 by reference, with the contact method of `SECURITY.md`. `CHANGELOG.md`: `## 0.1.0 - <release date>` with a ten-line summary of the shipped features (features, not plan numbers).
- [ ] **Step 4: Links.** Write `scripts/tests/check-doc-links.sh` first (`ok_link`, `broken_file`, `broken_anchor` with GitHub's anchor slug rules), then `scripts/check-doc-links.sh`; run it on the tree: clean.
- [ ] **Step 5:** In a fresh clone of the worktree under the job's tmp dir, follow the README's quick start verbatim: every command succeeds and the page answers. Run the UI e2e once (`cargo test -p zeta --test ui`): passes.
- [ ] **Step 6:** Gates. **Commit** `release: the README as a front page, the reference as docs/guide.md, a docs index, feedback-and-gists contributing, security, conduct and changelog`.

### Task 6: CI (Sonnet)

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/dependabot.yml`

**Interfaces:**
- Consumes: Task 1's script, Task 4's `deny.toml`.

- [ ] **Step 1: `ci.yml`**, on `push` and `workflow_dispatch` (no `pull_request`: ruling 8), `ubuntu-24.04`, toolchain from `rust-toolchain.toml`, `Swatinem/rust-cache` pinned by commit SHA: jobs `fmt` (per member: loop over `crates/*` except `zeta-source-soapy-driver`), `clippy` (`--workspace --all-targets --all-features -D warnings`), `test` (`cargo test --workspace`, excluding the UI e2e via whatever filter `ui.rs` honours; if none, `--exclude`-free with `ZETA_UI_SKIP` added to `ui.rs` in this task with a test), `ui` (Playwright install, `cargo test -p zeta --test ui`, `timeout-minutes: 20`), `agents-md` (`zeta agents-md --check`), `deny` (`EmbarkStudios/cargo-deny-action` pinned), `public-check` (`gitleaks` pinned, `bash scripts/public-check.sh`; no denylist in CI, so built-in checks only), `doc-links` (`bash scripts/check-doc-links.sh`). Every third-party action pinned to a full commit SHA with the version in a comment. `permissions: contents: read`.
- [ ] **Step 2: `dependabot.yml`**: cargo and github-actions, weekly.
- [ ] **Step 3:** Validate: `actionlint` if available (install with `go install` or the release binary and name it), else `python3 -I -c 'import yaml,sys; yaml.safe_load(open(sys.argv[1]))'`; run each job's command locally and record pass/fail and wall-clock time in the report. The workflow first runs for real after Task 8's push.
- [ ] **Step 4: Commit** `release: CI for fmt, clippy, tests, the UI e2e, agents-md, licences and the leak gate`.

### Task 7: The public export (Opus: last line of defence)

**Files:**
- Create: `scripts/export-public.sh`, `scripts/tests/export-public.sh`

**Interfaces:**
- Consumes: Tasks 1-6 committed.
- Produces: `bash scripts/export-public.sh DEST [REF]` (`REF` default `HEAD`): refuses a dirty tree or an existing non-empty `DEST`; `git archive REF | tar -x -C DEST`, then delete the exclude file's paths from `DEST`; `git init -b main`; one commit `ZetaSDR 0.1.0` authored and committed as `PUBLIC_AUTHOR` (default `Sage Grigull <ciphernaut@proton.me>`, overridable by env) with `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` set to the release date; tag `v0.1.0`; then in `DEST`: `public-check.sh` (with the denylist), `gitleaks git` over the new history, `cargo build --workspace`, `cargo test --workspace`; prints `export: ready at DEST (<sha>)` or the first failing gate and exits 1. Never adds a remote, never pushes.

- [ ] **Step 1: Failing tests** in `scripts/tests/export-public.sh` (on a tiny fixture repo, the cargo gates skipped by `EXPORT_SKIP_CARGO=1`): `refuses_dirty_tree`, `refuses_nonempty_dest`, `export_has_one_commit_and_tag`, `export_has_no_ignored_files` (a file ignored in the fixture is absent in `DEST`), `export_fails_on_leak` (a denylist hit committed in the fixture → exit 1 naming `public-check`), `export_drops_excluded_paths` (a path in the exclude file is tracked in the fixture and absent in `DEST`; the check inside `DEST` runs with no exclude file), `author_is_public_author`, `no_remote`.
- [ ] **Step 2:** Run: fail. **Step 3:** Implement. **Step 4:** Run: every `ok`.
- [ ] **Step 5: The real dry run**: `bash scripts/export-public.sh $CLAUDE_JOB_DIR/tmp/zetasdr-public` on the branch tip. Expected `export: ready`. Report the commit SHA, the tree's file count and size, and `du -sh DEST/.git`.
- [ ] **Step 6: Commit** `release: export-public.sh builds the one-commit public history and re-runs every gate in it`.

### Task 9: Grant-ready project furniture (Sonnet)

Added 2026-11-05 at the operator's request: an ARDC grant proposal is about a month out, and reviewers will read the public repository.

**Files:**
- Create: `ROADMAP.md`, `GOVERNANCE.md`, `docs/licensing.md`, `CITATION.cff`
- Modify: `CONTRIBUTING.md` (one line on when pull requests open), `README.md` (amateur use first; links to ROADMAP, GOVERNANCE, licensing), `docs/README.md` (index the new docs), every `sigint`/`SIGINT` occurrence in code identifiers, strings and docs outside `docs/superpowers` (rename to neutral layer-3 terms, e.g. `sigint-slot` → `layer3-slot`), regenerated `AGENTS.md` / `skills/zetasdr/SKILL.md` if a source changed

- [ ] **Step 1: `ROADMAP.md`.** Short and public-facing, distinct from `TODO.md` (the working gist list, linked). Sections: *Now* (0.x: what is being worked on, from `TODO.md`'s open items, no dates promised); *Seeking funding*, each a few lines with what exists today (paths) and what the track would add: amateur digital and voice modes (FT8/WSPR, CW, M17/Codec 2, satellite AX.25); a multi-stream FPGA source over VITA 49.2 (`crates/zeta-source-vrt` today takes one stream); a layer-3 IQ classifier with an amateur-only labelled dataset (the `classify` ladder's empty slot; labels from CRC-proven decodes); an agent evaluation suite; *Not planned* (recovering the content of third-party links). No budget figures.
- [ ] **Step 2: `GOVERNANCE.md`.** Who maintains it (one maintainer today, Sage Grigull), how decisions are made (the maintainer decides; design notes in `docs/superpowers`), how feedback and gists are handled, how someone becomes a maintainer, and the bus-factor plan (the code is Apache-2.0 and the design history public, so a fork can continue). `CONTRIBUTING.md`: add "Pull requests will open at 0.2.0." beside the gists rule.
- [ ] **Step 3: `docs/licensing.md`.** The project's licence (Apache-2.0, why: OSI-approved, patent grant, usable from GPL-3 projects); a table of everything bundled (from `NOTICE` and `deny.toml`: component, path, licence); the external-handler rule (GPL programs such as dump1090 and the rs1729 decoders run as separate processes the user installs, never linked or shipped; new GPL decoders follow the same rule); placeholders, marked *to decide*, for FPGA gateware (CERN-OHL-P or -S; vendor IP listed, not relicensed) and models and datasets (an OSI-open base model licence, released weights with a model card, a dataset licence, amateur transmissions only).
- [ ] **Step 4: `CITATION.cff`** (CFF 1.2.0: title, version 0.1.0, authors, licence Apache-2.0, repository-code `https://github.com/ciphernaut/ZetaSDR`, abstract one sentence; no DOI yet). Validate with `uvx cffconvert --validate` if it runs, else a YAML parse.
- [ ] **Step 5: Wording.** Rename `sigint`/`SIGINT` outside `docs/superpowers` (`git grep -in sigint`); keep tests passing (a renamed classifier id may be asserted in tests and docs). README: the *what it is for* paragraph and the decoder list lead with amateur use (APRS on 2 m, radiosondes, ADS-B), pagers and CB after.
- [ ] **Step 6:** Gates (public-check, doc-links, agents-md checks, fmt, clippy, workspace tests). **Commit** `release: a roadmap, governance, a licensing note and a citation file; layer-3 wording; amateur use first`.

### Task 10: The sanitised full-history export (Opus: rewrites history, last line of defence)

Ruling 1 changed by the operator on 2026-11-05: keep the full history, sanitised thoroughly. The one-commit export of Task 7 stays as a fallback (`--squash`).

**Files:**
- Modify: `scripts/export-public.sh`, `scripts/tests/export-public.sh`, `.github/workflows/ci.yml` (run the export's tests), `docs/superpowers/plans/2026-11-03-public-release.md` (Task 8 for the history export)
- Outside the tree (operator's, never committed): `~/.config/zetasdr/public-replacements`, a `git filter-repo --replace-text` expressions file (`literal==>replacement`, `regex:...==>replacement`), drafted by this task from the denylist and the scrub's mapping, and reported as a count of rules only

**Interfaces:**
- `bash scripts/export-public.sh DEST [REF]` (default: history mode): `git clone --no-local --single-branch` of the source repo into DEST at REF's branch, every remote removed, then `git filter-repo` in DEST with `--refs` limited to that branch: `--invert-paths --paths-from-file` (the exclude file's globs as `glob:` lines, plus any private path that existed only in history: `scratch/`, earlier names of the deploy scripts; found by listing every path in history), `--replace-text` (the replacements file) and `--replace-message` (the same file), `--mailmap` only if history holds another identity. Then tag `v0.1.0` on the tip (annotated, `PUBLIC_AUTHOR`, `PUBLIC_DATE`; commit dates are left as they were). `--squash` keeps Task 7's behaviour.
- Gates in DEST, all required, each named on failure, exit 1: (1) **every blob in history** (`git rev-list --objects --all` → `git cat-file --batch`, binaries through `strings`) checked against the denylist, the built-in coordinate and local-host checks (the final allowlist applies by path) — reported as `history: <commit7>:<path>: <check>`; (2) every commit message and author/committer field against the denylist; (3) `gitleaks git` over the whole rewritten history; (4) no path in any commit matches the exclude globs; (5) the tip's tree equals the source REF's tree with the excluded paths removed (`git diff --stat` empty), so the rewrite changed nothing in today's files; (6) the Task 7 tip gates (public-check with no exclude file, doc-links, fmt, clippy, build, tests, agents-md; cargo-deny when present). The final line lists any skipped gate: `export: ready at DEST (<sha>, <n> commits); skipped: <gates>`.
- Never touches the source repository's refs, objects, config or working tree (the clone is `--no-local`; filter-repo runs only in DEST); never adds a remote; never pushes.

- [ ] **Step 1:** Install filter-repo: `uv tool install git-filter-repo` (name the version). Draft the replacements file from the denylist and the Task 3 mapping (hosts → roles, home coordinates and nearby points → the demo station, suburbs → `Example Suburb`, LAN addresses → `<wired-ip>`/`<wifi-ip>`), with `chmod 600`.
- [ ] **Step 2: Failing tests** in `scripts/tests/export-public.sh` (fixture repo with a leak in an OLD commit's file, an old commit message and an old deleted private file): `history_leak_in_old_blob_is_rewritten`, `history_leak_in_message_is_rewritten`, `history_private_path_is_dropped`, `history_unreplaced_leak_fails` (a leak with no replacement rule → exit 1 naming `history`), `history_tip_tree_matches_source`, `source_repo_untouched` (refs, `git count-objects`, config and status of the source identical before and after), `squash_mode_still_works`; plus the Task 7 review minors: skipped gates listed in the final line, doc-links required, unguarded steps named (`archive`, `drop`), a gitleaks hit fails the export, an ignored `__pycache__` removed while another ignored stray fails `clean-tree`, DEST-inside-source refused before `mkdir`, the empty-dir prune run from inside DEST.
- [ ] **Step 3:** Run: fail. **Step 4:** Implement. **Step 5:** Run: every `ok`. Add `bash scripts/tests/export-public.sh` to CI (it needs filter-repo: install with `uv tool install` pinned).
- [ ] **Step 6: The real dry run** to `$CLAUDE_JOB_DIR/tmp/zetasdr-public-history` from the branch tip. Iterate on the replacements file (never on the tree) until `export: ready`. Report: commit count before and after, the tip SHA, the number of replacement rules and of blobs and messages rewritten (counts, not contents), the history gate counts, `du -sh DEST/.git`, and a sample of 5 rewritten commit subjects chosen at random (they contain no private text by then).
- [ ] **Step 7:** Rewrite Task 8 below for the history export. **Commit** `release: export-public.sh keeps the full history, rewritten by filter-repo in a fresh clone and checked blob by blob`.

### Task 8: Publish (operator, by hand)

Not for an agent: publishing is outward-facing and irreversible.

- [ ] Merge `worktree-public-release` to the private `main` (fast-forward), so the private repo keeps the gate and scripts.
- [ ] Re-run `bash scripts/export-public.sh <dest>` from `main`.
- [ ] Create the GitHub repository (empty, no README), enable private vulnerability reporting and Dependabot alerts, turn the wiki and Discussions off, create the `feedback` and `gist` labels, and (if the setting is offered) limit pull requests to collaborators; set the `repository` URL placeholder from Task 4 if it was left (then re-export).
- [ ] In `<dest>`: `git remote add origin git@github.com:<OWNER>/ZetaSDR.git && git push -u origin main --tags`.
- [ ] Watch the first CI run; protect `main` (require the CI jobs) once it is green.
- [ ] Later releases: keep developing privately and export each tag on top of the public history (a follow-up gist: `export-public.sh --onto <public clone>` committing the diff as one release commit), or move development to the public repo outright.
