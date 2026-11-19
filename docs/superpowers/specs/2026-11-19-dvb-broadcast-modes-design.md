# DVB broadcast modes: DVB-T2 and first-generation DVB-S

Design doc. From the operator, 2026-11-19 ("park the [Wireshark] spec and make
another one for expanding other dvb modes"; scope chosen "DVB-S (first-gen) + T2",
satellite measurement-only — the DVB-S2 GSE/IP decapsulation boundary unchanged).

Prior art in the repo: plan 39 (`modes/dvbt_decode.json`, the external `dvbt-rx`
wrapping SDRangel's `modemdvb`), and the two TODO gists "DVB-T: picture, sound and
services" and "GEO DVB-S2 reception and measurement".

## Intent and why

The node reads DVB-T signalling but nothing else in the DVB family. Two modes are the
next broadcast steps, and both are the strongest signals in their bands:

- **DVB-T2** — the second-generation terrestrial standard. A T2 multiplex is common on
  the same discone the DVB-T row already uses, but `dvbt-rx` does not look for T2, so a
  T2 multiplex reads `locked: false` today. Reading its signalling is a small extension
  of the existing wrapper and row.
- **First-generation DVB-S** — satellite broadcast television/radio to MPEG-TS
  (EN 300 421, QPSK). It is a different front end (an L-band dish + LNB, shared with the
  deferred DVB-S2 work) and a different external program, but it is broadcast content in
  the same category as DVB-T picture/sound: public services in a transport stream, not
  the shared-transponder IP traffic the DVB-S2 boundary is about.

This keeps the one established pattern: a decoder mode is a JSON row that runs an
operator-built external program over IQ and reports what the signal says about itself.

## Goals

- **DVB-T2:** a locked T2 multiplex reports its signalling — system (DVB-T/DVB-T2), FFT
  size, guard interval, the physical-layer pipes (PLPs) with each one's constellation
  and code rate, and the cell id — instead of `locked: false`. Same discone front end,
  same identify/classify shape as DVB-T.
- **DVB-S:** a new row **measures** a DVB-S carrier — lock, symbol rate, code rate,
  roll-off, and MER/Es-No as events — and, as a second step, **lists its MPEG-TS
  services** (PAT/SDT), mirroring the DVB-T signalling→service-list progression. Needs
  the L-band dish/LNB.
- Both reuse the program-row model and identify's `as_asked`/decoded-query path. The
  register emission mapping needs no new work: `modes/score.rs::designator_fits` already
  maps `ofdm` (T2, like DVB-T) and `psk` (DVB-S QPSK → designators G and D). DVB-S's
  `modulation: ["psk"]` fits as-is.

## Non-goals

- **No DVB-S2/S2X.** The operator deferred it; the GEO DVB-S2 measurement gist stands
  on its own.
- **No picture or sound.** When the DVB-T picture/sound path is built (its own gist), a
  DVB-S transport stream reuses it; this spec stops at the service list.
- **No new operation.** These are rows plus the existing `job_start`/`identify`; no `Op`
  is added.

## Anti-goals (failure even if it works)

- **Any IP reassembly from a satellite carrier.** DVB-S can carry MPE data broadcasting;
  decapsulating MPE and reassembling IP flows is the same boundary the DVB-S2 gist draws
  ("GEO DVB-S2 reception and measurement") and stays unbuilt. The DVB-S row stops at the
  MPEG-TS service tables (the broadcast content), exactly as the DVB-T service-list step
  does. "First-gen DVB-S" here means broadcast TV/radio, not a data-broadcast carrier's
  payload.
- **A T2 row that silently needs a dish, or a DVB-S row offered in the terrestrial
  band.** Each row's `band.range_hz` keeps identify/classify from trying it where it
  cannot be (DVB-T2 in the broadcast TV bands; DVB-S in the satellite L-band IF).

## Constraints

- **One operation set / generated prose.** New rows are JSON under `modes/`; their notes
  and any new event-field prose go through `crates/zeta/src/agents_md.rs`, regenerated,
  not hand-edited. `AGENTS.md`/`SKILL.md` drift fails CI.
- **GPL external handlers are allowed** (`docs/licensing.md`, "External handlers":
  dump1090, rs1729, and `dvbt-rx` itself). A new GPL program (DecSAT, SatDump or
  gr-dvbs2rx) the operator builds and installs is the same model: never linked, never in
  this repo, named by a `ZETA_HANDLER_<PROGRAM>` override in the unit (per
  [[zetasdr-dvbt-direction]] bookkeeping).
- **Wide/binary at real rates needs a release build** (the DVB-T lesson); a debug node
  cannot keep an 8 MHz T2 channel or a wide DVB-S symbol rate.
- **L-band front end is a source/hardware concern, not a decoder one** — the dish + LNB
  downconverts to an IF the B210 takes, shared with the DVB-S2 gist. This spec assumes
  the IF reaches a source; it does not design the dish.
- **Public repo:** no real transmissions, positions or frequencies in fixtures/notes;
  demo station Brisbane Airport; `public-check.sh` clean before `main`.

## Approaches considered

**DVB-T2 — the program and the row:**

1. *Extend `dvbt-rx` to try modemdvb's `dvbt2receiver`, one row reporting `system`*
   (**chosen**). The wrapper already owns the terrestrial acquisition; it tries the T1
   receiver, then the T2 receiver, and reports which locked. One row ("DVB-T/T2
   terrestrial television") avoids two near-identical rows and the duplicate identify
   dwell, matching the operator's "one mechanism, not N variants" preference
   ([[feedback-generic-mechanisms-over-variants]]).
2. *A separate `dvbt2-rx` program and `dvbt2_decode` row.* Rejected: duplicates the
   wrapper, the fixture and the row for a sibling standard the same library already
   receives.

**DVB-S — a wrapper, and which engine it drives.** The builder check (2026-11-19)
corrected a wrong assumption: **none of the candidate programs is a drop-in handler.**
DVB-S, like DVB-T, needs a small operator-built wrapper — call it `dvbs-rx` — that reads
IQ on stdin and prints `json_lines` measurements on stdout, because:

- *DecSAT* has no such mode. Its CLI is file-in/file-out (`analyse`, `scan`, `decode`
  writes `.ts`/`.csv`) and `decsat-server`'s smart mode reports over its own network
  protocol, not stdout. But its engine crates (`decsat-engine`, `decsat-modem`) do blind
  **DVB-S → MPEG-TS (EN 300 421)**, Viterbi + RS, blind symbol rate/roll-off — Rust,
  GPL-3, in hand. A `dvbs-rx` built on those crates is the cleanest engine (**my pick**).
- *gr-dvbs2rx / GNU Radio* and *SatDump* can take stdin cf32 but still need a wrapper for
  machine-readable status (SatDump's is an HTTP port, the SatDump gist's open problem);
  they are the alternatives if the operator prefers not to depend on DecSAT's crates.
- *modemdvb* (the DVB-T wrapper's library) does **not** do DVB-S, so there is no
  extending `dvbt-rx` into it.

So the DVB-S decision is which engine `dvbs-rx` wraps, not which program to call. This is
a materially bigger piece than DVB-T2: a new GPL wrapper to build, test, publish and
version, on the `dvbt-rx` precedent (unpublished local repo, named by
`ZETA_HANDLER_DVBS_RX` in the unit). The node side is unchanged — stdin cf32, exactly
like `dvbt_decode`.

**DVB-S measurement vs service list — one row or two:** one row, two build steps
(measurement first, then the MPEG-TS service list), mirroring how the DVB-T row went
signalling-first then service-list. Rejected the two-row split for the same reason as
DVB-T2.

## The design

### Part 1 — DVB-T2 (extend `dvbt-rx`, no new front end)

`dvbt-rx` gains a T2 acquisition: after the DVB-T receiver fails to lock, it runs
modemdvb's `dvbt2receiver` on the same band. The `dvbt_decode` row is **extended, not
replaced**:

- A new event field `system` (`"dvbt"` | `"dvbt2"`), and T2-only fields nullable on a
  DVB-T lock: `fft` (1k–32k), `plps` (an array, each `{ id, constellation, code_rate,
  rotated }`), `t2_profile` (base/lite), reusing `guard`, `cell_id`, `offset_hz`,
  `clock_ppm`, `snr_db`, `ripple_db`, `resyncs`.
- The row label stays DVB-T-family for the register; `system` distinguishes the lock.
  `locked: false` now means neither T1 nor T2 locked at any width.
- Identify/classify unchanged (`as_asked`, `range_hz` the broadcast TV bands); the
  decoded-query comparison already maps `ofdm` to the OFDM emission letters.
- **Acquisition within the dwell (builder gap):** the wrapper now runs two receivers, so
  it must reach a T2 lock inside the row's 8 s identify dwell. A DVB-T2 signal carries a
  **P1 preamble symbol** identifying it as T2 before full acquisition, so the wrapper
  should branch on P1 rather than run the whole DVB-T width sweep and only then try T2.
  If P1 detection will not fit the dwell, the dwell is widened for this row (builder's
  measurement on a real/synthetic T2 capture).

Fixture: `modemdvb` has a T2 modulator path, so `dvbt-rx` can write a short synthetic T2
multiplex for the row's `test` the way the DVB-T fixture gist proposes, under the few-MB
budget.

### Part 2 — DVB-S (new row + new external program + L-band front end)

A new `dvbs_decode` row, `family: "tv"`, `decoder.kind: "program"`, modelled on
`dvbt_decode`:

```jsonc
{
  "id": "dvbs_decode",
  "name": "DVB-S satellite television (measurement)",
  "family": "tv",
  "band": { "bandwidth_hz": 30000000, "centers_hz": [], "range_hz": <L-band IF>, "min_hz": 1000000 },
  "modulation": ["psk"],
  "label": "DVB-S",
  "protocol": "dvbs",
  "decoder": {
    "kind": "program",
    "programs": ["dvbs-rx"],           // operator-built wrapper; engine = DecSAT crates (my pick)
    "args": [ ... symbol-rate hint, blind ... ],
    "input": { "stream": "iq", "format": "cf32", "rate_hz": <2–4× Rs>, "via": "stdin" },
    "output": { "via": "stdout", "port": null, "parser": "json_lines" },
    "install": { "debian": null, "url": "<dvbs-rx repo, unpublished like dvbt-rx>" }
  },
  "event": { "schema": { ... locked, lock, symbol_rate_baud, code_rate, roll_off,
                          mer_db, esno_db, offset_hz, inverted, services?, network_id? ... },
             "summary": "{lock} · {symbol_rate_baud?} {code_rate?} MER {mer_db?}" },
  "identify": { "proof": "field", "field": "locked", "min_events": 1,
                "dwell_ms": 8000, "as_asked": true }
}
```

- **Step 1, measurement:** lock, symbol rate, code rate, roll-off, MER/Es-No, carrier
  offset, spectral inversion — the within-boundary half, no TS parsing. Proves the
  program and the front end.
- **Step 2, service list:** the MPEG-TS PAT/SDT read into `services` and `network_id`
  events (broadcast content), the same shape the DVB-T service-list step defines. No MPE,
  no IP.
- **Front end:** the job runs on a source fed the dish/LNB L-band IF. The row's
  `range_hz` is the IF span, so identify never offers DVB-S in a terrestrial band and
  vice versa. The dish is out of scope (a later source concern, shared with DVB-S2).

#### No new node mechanism

An earlier draft proposed a `via: "udp"` socket feed, because DecSAT-the-application
only takes network IQ. With `dvbs-rx` built as a purpose wrapper, that dissolves: the
wrapper reads cf32 on stdin like `dvbt-rx`, so the DVB-S row uses `via: "stdin"` and the
node's program-row launcher is unchanged. The cost that would have been a node mechanism
moves into the wrapper, where it belongs.

### Shared bookkeeping (both parts, from [[zetasdr-dvbt-direction]])

A new embedded row (DVB-S) means updating `EMBEDDED` in `zeta-core/src/modes/mod.rs`, the
row-name lists in `zeta-core/tests/modes.rs`, `zeta/tests/cli.rs` (plus a
`ZETA_HANDLER_DVBS_RX` override), `zeta/tests/modes_check.rs`, and the order test in
`modes/score.rs`. A program row with no centres needs a `range_hz` or it takes a classify
slot everywhere. Schema fields ending `_hz`/`_baud` are integers. DVB-T2 extends an
existing row, so it touches the event schema and `agents_md.rs` prose but adds no row.

## What's left to the builder

- The exact T2 signalling fields modemdvb's `dvbt2receiver` exposes, and whether the
  wrapper reports per-PLP data for all PLPs or only the one it decodes.
- The DVB-S row's `args` (symbol-rate hint vs fully blind) and `rate_hz` policy (2–4× the
  expected symbol rate) — read from the chosen program's interface.
- The L-band IF `range_hz` numbers, once the dish/LNB LO is known (a source concern; use a
  placeholder span until then).
- The DVB-S fixture: a short synthetic DVB-S carrier (DecSAT has a modulator /
  `decsat synth`/`scene`; or SatDump's), within the fixture budget.
- Whether DecSAT's CLI or `decsat-server` is the handler invoked, and the precise
  socket/argument form for the `via: udp` feed.

## Decisions (operator, 2026-11-19)

1. **DVB-S wrapper and engine — DECIDED: build it.** DVB-S means building `dvbs-rx`, a
   GPL wrapper (IQ-in on stdin → `json_lines` out), on DecSAT's `decsat-engine` /
   `decsat-modem` crates (Rust, GPL, blind DVB-S→TS, in hand). This is the bulk of the
   DVB-S effort; the node side stays `via: stdin`, no new node mechanism. (Alternatives
   gr-dvbs2rx / SatDump were offered and not taken.)
2. **Landing shape — DECIDED: two plans, T2 first.** DVB-T2 (a small extension of
   `dvbt-rx` and the existing row, no hardware) lands first and is live-testable on the
   discone now; DVB-S (new wrapper + row + L-band dish) is a second plan, gated on the
   dish.
