# ZetaSDR documentation

Start with the [README](../README.md): what ZetaSDR is, a quick start without a radio, and how to
attach one.

## Using a node

- [The guide](guide.md): the reference for running a node. Building and testing, a CLI
  walkthrough with worked examples, the DSP, speech, sources, decoder modes, exploring unknown
  signals, external stages (plugins), the transports (HTTP, websocket, MCP, the agent), the web
  page, wideband capture and scanning, and federation.
- [AGENTS.md](../AGENTS.md): every operation with its fields, JSON and CLI forms, the stage kinds
  and the field craft (where to look, how to decode, how to fit a demodulator to a signal, what to
  do with an unknown). Written for an agent driving the node and just as useful to a person. It is
  generated from the operation schema and [agents/field-craft.md](agents/field-craft.md) by
  `zeta agents-md`; a node also serves it at `GET /agents-md`. The same text, as a Claude Code
  skill, is [skills/zetasdr/SKILL.md](../skills/zetasdr/SKILL.md).
- [RECIPES.md](../RECIPES.md): named job setups (airband data links as a set, UHF CB listeners with transcription, a radiosonde watch, your own theme) as the
  operations to send, in order.
- [speech.md](speech.md): transcribing a channel. Installing a speech daemon on a node's host,
  writing your own, sharing one between nodes, and source separation.
- [themes.md](themes.md): the web page's themes, and how to write one.
- [deploy/README.md](../deploy/README.md): running a node as a service, with the Raspberry Pi
  example in [deploy/pi](../deploy/pi/README.md).

## How it is built

- [architecture/mudmap.html](architecture/mudmap.html): what plugs into a node and where each
  piece may run (the figure on the front page).
- [architecture/index.html](architecture/index.html): every crate, what it holds and how they
  depend on each other.
- [ROADMAP.md](../ROADMAP.md): where the project is going, and the tracks that need funding.
- [GOVERNANCE.md](../GOVERNANCE.md): who maintains it and how decisions are made.
- [licensing.md](licensing.md): the project's licence, what is bundled and how GPL decoders are
  kept apart.
- [TODO.md](../TODO.md): open work and ideas not yet scheduled.
- [CHANGELOG.md](../CHANGELOG.md): what each release contains.

## Design history

[superpowers](superpowers/) holds the design specifications (`specs/`) and implementation plans
(`plans/`) the project was built from, one pair per feature, starting with
[the design](superpowers/specs/2026-10-18-zetasdr-design.md) and
[the plan series](superpowers/plans/2026-10-18-zetasdr-plan-series.md), which lists every plan with
its status and what was proved live. They are kept as written at the time: later work supersedes
some of what they say, and the guide and `AGENTS.md` describe the current behaviour. They refer to
features by plan number.

## Contributing

[CONTRIBUTING.md](../CONTRIBUTING.md) (feedback and gists, and the checks for a local build),
[SECURITY.md](../SECURITY.md) and [CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md).
