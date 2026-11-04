# Licensing

## The project

ZetaSDR is licensed under the Apache License 2.0 ([LICENSE](../LICENSE)). The reasons: it is
approved by the OSI, it carries an express patent grant (useful for DSP and codec code), and it
can be used from GPL-3 projects, so a GPL-3 program may build on ZetaSDR where the reverse is not
true of GPL-only code. Contributions are accepted on the same terms (inbound equals outbound, as
section 5 of the licence says).

## What is bundled

Taken from [NOTICE](../NOTICE), which is the authority. Rust dependencies are checked by
`cargo deny` against [deny.toml](../deny.toml): Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, ISC,
Unicode-3.0, Zlib, MPL-2.0, CC0-1.0, MIT-0, BSL-1.0 and CDLA-Permissive-2.0 are allowed; copyleft
(GPL, LGPL, AGPL) is not, and a dependency under one fails the CI licence check.

| Component | Path | Licence |
|---|---|---|
| iNTERCEPT signal catalogue (smittix) | `crates/zeta-dsp/data/signals.json` | Apache-2.0 |
| Leaflet 1.9.4 | `crates/zeta-ui/web/vendor/leaflet` | BSD-2-Clause |
| Antonio | `themes/console` | SIL OFL 1.1 |
| Orbitron, Share Tech Mono | `themes/cyberpunk` | SIL OFL 1.1 |
| Comfortaa, Nunito | `themes/psytrance` | SIL OFL 1.1 |
| JetBrains Mono | `themes/terminal`, `themes/vibecoder` | SIL OFL 1.1 |
| Inter | `themes/vibecoder` | SIL OFL 1.1 |

## External handlers

Some decoders are GPL programs: dump1090 and the rs1729 sonde decoders (`rs41mod`, `dfm09mod`,
`m20mod`). ZetaSDR runs them as separate processes when the operator has installed them. It never
links them and never ships them, and a missing one only makes its mode row `unavailable`. A new
decoder under the GPL follows the same rule: a program row that the operator installs, not code
in this repository. So does one under the LGPL, such as Codec 2 (LGPL-2.1) for a future M17 row:
it runs as an external handler. Linking it dynamically instead would bring the LGPL's obligations
(the library replaceable by the user, its source offered, the notice shipped) and needs `deny.toml`
changed first.

## To decide

These are not settled; each is marked so until the work exists.

- **FPGA gateware** (*to decide*): CERN-OHL-P (permissive) or CERN-OHL-S (strongly reciprocal).
  Any vendor IP the design uses would be listed with its own licence, not relicensed.
- **Models and datasets** (*to decide*): a base model under an OSI-open licence; released weights
  with a model card; a licence for the dataset; and the dataset to hold amateur transmissions
  only.
