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
- Both reuse the program-row model, identify's `as_asked`/decoded-query path, and the
  register emission-designator mapping (`ofdm` → D/V/W/X) the DVB-T row established.

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

**DVB-S — the external program:**

1. *DecSAT* (**chosen**). Rust, GPL-3, already does **DVB-S → MPEG-TS (EN 300 421)** with
   code rate, rotation and inversion found blind (Viterbi + RS), and a blind
   symbol-rate/roll-off "Identify". We reviewed it 2026-11-19. One wrinkle: its live IQ
   input is **network IQ** (`udp://`, `tcp://`, `listen://`), not stdin — so the DVB-S row
   needs a socket feed, not the `via: stdin` the DVB-T row uses (see "the input wrinkle").
   Best blind DVB-S decode of the candidates and the one already in hand.
2. *SatDump DVB-S pipeline.* Fits the existing `via: stdin` cf32 feed with no node change,
   but SatDump is image/product-oriented and its DVB-S→TS path is less its focus; its
   status comes from an HTTP port, not stdout (the SatDump gist's open problem).
3. *gr-dvbs2rx / GNU Radio flowgraph.* Reads stdin cf32, flexible, but must be assembled
   and validated here.
4. *Extend `dvbt-rx`.* Rejected: DVB-S is **not** `modemdvb` (which is DVB-T/T2); it is a
   different SDRangel component (the DATV/leandvb path), so this is not a free extension
   of the existing wrapper.

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
    "programs": ["decsat"],            // or satdump / dvbs2rx; see approaches
    "args": [ ... symbol-rate hint, blind ... ],
    "input": { "stream": "iq", "format": "cf32", "rate_hz": <2–4× Rs>, "via": "udp" },  // the wrinkle
    "output": { "via": "stdout", "port": null, "parser": "json_lines" },
    "install": { "debian": null, "url": "https://github.com/CasualArclamp/DecSAT" }
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

#### The input wrinkle (the one node change DVB-S may need)

The program-row `input.via` is `stdin` today. DecSAT's live IQ input is a UDP/TCP socket,
not stdin. Two ways:

- **Add `via: "udp"` (chosen with DecSAT):** the node binds/sends cf32 to a local socket
  the program reads. A small, reusable addition to the program-row launcher
  (`zeta-ext`/launch), useful later for the DVB-S2 measure row and any socket-only
  program. This is the one genuinely new node mechanism in the spec.
- **Keep `via: stdin` and pick a stdin program** (SatDump or gr-dvbs2rx): no node change,
  at the cost of DecSAT's blind DVB-S decode. If the operator prefers no new node
  mechanism, this is the fallback and the program choice flips with it.

### Shared bookkeeping (both parts, from [[zetasdr-dvbt-direction]])

A new embedded row (DVB-S) means updating `EMBEDDED` in `zeta-core/src/modes/mod.rs`, the
row-name lists in `zeta-core/tests/modes.rs`, `zeta/tests/cli.rs` (plus a
`ZETA_HANDLER_DECSAT` override), `zeta/tests/modes_check.rs`, and the order test in
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

## Open decisions for the operator

Carried in the handover, not pre-settled:

1. **The DVB-S external program** — DecSAT (best blind DVB-S, needs the new `via: udp`
   node mechanism) vs a stdin program (SatDump/gr-dvbs2rx, no node change, weaker DVB-S).
   My pick is DecSAT; it decides whether the spec adds a node mechanism.
2. **Landing shape** — DVB-T2 and DVB-S are independent (different program, front end and
   risk); I propose two plans, T2 first (small, no hardware), DVB-S second (needs the
   dish). Confirmable.
