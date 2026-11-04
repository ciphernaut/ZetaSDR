# Contributing to ZetaSDR

At this stage ZetaSDR takes feedback and gists, not pull requests.

The project is young and still changing shape, so the most useful thing you can send is an account
of what you tried and what happened, or an idea with the reason behind it. Code is welcome too, as
a gist the maintainer can read and adopt; pull requests are not merged for now. Thank you for
taking the time either way.

## Sending feedback

Open an issue with the **Feedback** form: what you tried, what happened, and what you expected.
A useful report has:

- the version or commit (`zeta --version`, or `git rev-parse --short HEAD`);
- the source kind (a recording, a SoapySDR device and which one, a KiwiSDR, another node);
- the exact command or operation, and the JSON it printed (errors carry the offending `field` and
  the acceptable range, which usually says most of what is wrong);
- what you expected instead, and anything from the node's stderr that looks related.

Leave out anything private: secrets, your exact position, host names you would rather not publish.

## Sending a gist

Open an issue with the **Gist** form for an idea: what it is and why it would help. A gist may
link a [GitHub gist](https://gist.github.com) holding a patch, a mode row, a theme, a plugin or a
recipe. The maintainer may adopt it, in whole or in part, in a commit of their own that credits you
with a `Suggested-by:` line. A gist that is not adopted is still read, and often shapes what is.

## Licence of what you send

ZetaSDR is licensed under the Apache License 2.0. Anything you send (an issue, a gist, a patch) is
offered under the same licence, as section 5 of the licence describes: inbound equals outbound.
Send only what you have the right to offer on those terms.

## Building and checking locally

For your own changes, or a patch you want to offer as a gist, these are the checks the project
runs. The toolchain is pinned in `rust-toolchain.toml`; add `rustfmt` and `clippy` to it.

```bash
cargo fmt --check                    # in a nested worktree, per member: cargo fmt -p <crate> -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p zeta --test ui         # the web page end to end, about 8 minutes
cargo run -p zeta -- agents-md --check
cargo run -p zeta -- agents-md --skill --check
bash scripts/public-check.sh         # needs gitleaks on PATH and strings (binutils)
bash scripts/check-doc-links.sh
```

The UI test drives the page with Playwright and headless Chromium. Install Playwright for the
`python3` on your `PATH` and its browser: `python3 -m pip install --user playwright`, then
`python3 -m playwright install chromium` (where the system Python refuses `pip install`, use a
virtual environment and keep it active when you run the test). Without Playwright the test prints
`ui e2e skipped: <reason>` and passes, so check for that line.

`AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: edit the operation docs in the code or
[docs/agents/field-craft.md](docs/agents/field-craft.md), then regenerate them with
`cargo run -q -p zeta -- agents-md > AGENTS.md` and
`cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`. The [guide](docs/guide.md#building-and-testing)
has notes on build profiles and disk use.

## Conduct and security

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md). Report a vulnerability
privately as [SECURITY.md](SECURITY.md) describes, not in a public issue.
