# Security

## Reporting a vulnerability

Report it privately through GitHub's private vulnerability reporting: on
<https://github.com/ciphernaut/ZetaSDR>, open **Security**, then **Report a vulnerability**. Please
do not open a public issue for it. Say what an attacker can reach and do, with the version or
commit and the steps to reproduce. You will get an answer as soon as the maintainer can give one;
this is a one-person project, so allow a few days.

The same private channel is the contact for anything under the
[Code of Conduct](CODE_OF_CONDUCT.md) that should not be public.

## What the node defends, and what it does not

ZetaSDR is built for a trusted local network. These are the boundaries it keeps, and so the
places where a weakness would matter:

- **The node secret.** Every HTTP route and websocket of `zeta serve --listen` needs the bearer
  secret (`ZETA_SECRET`, or `--secret`, which is visible in `ps`); only the page's static files
  under `/` and `/ui/` are served without it. A secret shorter than 12 characters draws a warning.
  The API is plain HTTP: the secret crosses the network in the clear, so use it only on a network
  you trust, or put the node behind a tunnel or a TLS proxy of your own.
- **LAN only.** `--listen PORT` binds 127.0.0.1, and `--listen ADDR:PORT` refuses an address that
  is not loopback, private, link-local, CGNAT (100.64.0.0/10) or unspecified. `--source-vrt`
  accepts the same addresses and is unauthenticated by design (VITA 49.2 has no authentication):
  anyone who can reach its port can feed it samples.
- **Programs it starts.** Plugins (`--plugin`) and decoder programs (dump1090, the rs1729 sonde
  decoders) run with a cleared environment: only `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`,
  `TERM` and `PYTHONPATH` pass, plus the variables you name with `ZETA_PLUGIN_ENV_<NAME>`, and
  never `ZETA_SECRET`. A plugin path must be absolute (no `PATH` lookup, no shell), and its whole
  process group is killed when its stage ends. The SoapySDR worker inherits the node's environment
  minus every `ZETA_*` variable whose name holds `SECRET`, `PASSWORD` or `KEY`.
- **Secrets in output.** KiwiSDR passwords and the secrets of other nodes (`ZETA_NODE_SECRET_<ID>`,
  `ZETA_STT_NODE_SECRET`) are never printed.

Out of scope: an attacker who already holds the node secret can do anything an operator can
(start and stop jobs, retune radios, run the agent if the node has one, read and delete captures).
Choose a long secret and keep it private.
