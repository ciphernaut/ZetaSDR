# Governance

## Who maintains ZetaSDR

One maintainer today: Sage Grigull ([ciphernaut](https://github.com/ciphernaut)). The maintainer
owns the repository, the releases and the decisions below.

## How decisions are made

The maintainer decides. Larger changes are written down first as a design note in
[docs/superpowers](docs/superpowers/) (a specification and an implementation plan), so the reasons
are public and a later change can be judged against them. Those notes are kept as written; the
guide and [AGENTS.md](AGENTS.md) describe the current behaviour.

## Feedback and gists

At this stage ZetaSDR takes feedback and gists, not pull requests (see
[CONTRIBUTING.md](CONTRIBUTING.md)). An issue opened with the Feedback or Gist form is read by the
maintainer. A gist may be adopted whole or in part, in a commit of the maintainer's own that
credits the author with a `Suggested-by:` line; one that is not adopted is still read. Open work
the maintainer has taken up is in [TODO.md](TODO.md) and [ROADMAP.md](ROADMAP.md). Security reports
go privately, as [SECURITY.md](SECURITY.md) describes. Everyone follows the
[Code of Conduct](CODE_OF_CONDUCT.md).

Pull requests open at 0.2.0. Until then the process above is the whole process.

## Becoming a maintainer

There is no fixed route yet. A person who has sent several adopted gists, and later pull requests,
that show they understand the design and the project's rules (one unit per field, unknown reported
as unknown, jobs belong to the operator) can be invited by the maintainer to review and then to
merge. Maintainers are added by the existing maintainer, in a public note in this file.

## If the maintainer is unavailable

The code is Apache-2.0 ([docs/licensing.md](docs/licensing.md)) and the design history is public,
so anyone can fork and continue the project without asking. The build, the checks and the
generated documents are all in the repository, and a release needs no private service. If a
second maintainer exists by then, they continue the project under this file; if not, a fork is the
plan.
