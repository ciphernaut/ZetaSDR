# ZetaSDR plan series

The design (`docs/superpowers/specs/2026-10-18-zetasdr-design.md`) describes
six subsystems. Each gets its own plan; each plan ends with working,
testable software. Order is dependency order. Plan 1 is written; the rest
are written when their predecessor is done, so they can argue from real
interfaces rather than guessed ones.

| # | Plan | Delivers | Depends on |
|---|------|----------|------------|
| 1 | Core node | Cargo workspace, typed streams, port trait with leases, IQ ring buffer, SigMF file source, graph document, persistent job runtime, operation set with JSON schema and corrective errors, stage registry with multi-implementation selection, knowledge and classifier traits with stubs, `zeta` CLI, AGENTS.md seed generated from the schema. Test suite passes with no hardware. | none |
| 2 | DSP slice | Channeliser, NBFM demod to an audio stream, AFSK1200/APRS decode-to-identify to an event stream, layer-1 classical measurement producing feature records, CPU and GPU FFT implementations of one stage. **Delivered** on branch `dsp-slice`, 2026-10-19. GPU hardware equivalence (`gpu_matches_cpu_within_0_1_db`) is pending on the field host. | 1 |
| 3 | Transports and agent | Websocket+JSON server with shared secret, MCP server generated from the schema, OpenAI-compatible in-process agent adapter (thinking off, token budget, readiness sequence, prose-answer detection), skill and tool descriptions generated from AGENTS.md. **Delivered** on branch `transports-agent`, 2026-10-19. | 1 |
| 4 | Hardware and foreign sources | HackRF via SoapySDR plugin crate, KiwiSDR websocket client, external-process plugin SDK (Python) speaking the typed envelope. **Delivered** on branch `hardware-sources`, 2026-10-19. The `cfg(soapy)` driver build and clippy passed against SoapySDR 0.8.1 once `libsoapysdr-dev` was installed later the same day. Pending: the HackRF hardware test; GPU equivalence on the field host is still pending from plan 2. | 1, 2 |
| 5 | Federation | Node-to-node: VITA 49.2 for IQ, typed envelope for the rest, back-pressure, shared secret, Pi 5 build and deployment, remote node as a source. **Delivered** on branch `federation`, 2026-10-19. Pending: deployment on Pi hardware (no Pi is set up yet), the one-shot `--then-tail` finding against the B210, and releasing the remote job when the local job stops. | 3, 4 |
| 6 | UI | Wide renderer (waterfall, ribbons, context menu, agent pane, jobs), narrow renderer, graph view. All clients of the plan-3 websocket. **Delivered** on branch `ui`, 2026-10-19. Pending: the phone check of the narrow renderer, the microphone against a real `voiced`, the HackRF walk, and a live `/ask` answer from the field host. | 3 |
| 7 | Audio path and wideband FM | Rational resampler so every demod outputs 48 kHz audio; `wfm_demod`, `am_demod` and `ssb_demod` job kinds (`--sideband`); layer-1 rules for `wfm` (19 kHz pilot as evidence), `am` and `ssb`; iNTERCEPT signal catalogue (Apache-2.0, 594 entries) as knowledge provider and layer-2 classifier; UI decode entries and playback of any audio job. **Delivered** on branch `audio-wfm`, 2026-10-19. Pending: AM and SSB playback in a browser. | 2, 6 |
| 8 | Wideband | Polyphase filter bank shared by every job on a wide capture (a channel that fits one bin is bank-served, wider ones take the direct path); spectrum rows v2 (fixed `rows_per_s`, u8-quantised `q`, up to 8192 bins); UI band control (zoom and pan inside the capture render only, beyond it retune); `scan` job that time-shares a radio across a span wider than one capture and holds the lease on it; IQ ring sized from a memory budget (`--ring-budget-mb`); `wfm_demod` decimates before its audio filter. **Delivered** on branch `wideband`, 2026-10-20. Pending: the fix wave (sub-band scan clamp, overlays and scan redraw), the B210 at 24 Msps (its asserted `max_bandwidth_hz` is 16 MHz), and the bank's 20 % cost target (33 % measured at 16 Msps). | 2, 6, 7 |
| 9 | Decoders | Four decode job kinds: `pocsag_decode` (12.5 kHz FM channel, 512/1200/2400 baud detected automatically), `rds_decode` (200 kHz FM broadcast band; `pi`, `ps`, `rt`), `adsb_decode` (1 090 MHz, 2.4 MHz band, dump1090 run on the node's host through the new crate `zeta-ext`) and `acars_decode` (10 kHz AM channel, 2400 baud MSK); decode-to-identify in `classify` (APRS, POCSAG and ACARS on narrow bands, RDS on wide ones); `decoder` on catalogue signatures; UI entries; field craft for pagers, aircraft and FM data. **Delivered** on branch `decoders`, 2026-10-20. Pending: live acceptance on the field host. | 2, 7, 8 |
| 10 | Speech on a channel | Transcription of a radio channel: audio frames carry a channel count; stages `denoise` (RNNoise via `nnnoiseless`), `vad` (energy-over-floor voice gate) and `transcribe` (new crate `zeta-speech`, speech backends `SocketBackend` for a `local-voice`-protocol daemon and `NodeBackend` for another node's `/stt`, global flags `--stt-socket` and `--stt-node`); attached jobs (`stream_in` stage, `job_attach` op, `zeta job attach`) so any audio-producing job, demodulators now and digital voice decoders later, can be transcribed; UI entries and transcript ribbons; the source-separation slot proven with an external audio plugin (`examples/python/separate_stub.py`) swapped in with `graph put`; `docs/speech.md`. **Delivered** on branch `speech`, 2026-10-20. Pending: live acceptance on the field host (needs `local-voice` installed there, `docs/speech.md` section 1). | 2, 7, 9 |
| 11 | View policy | Resume restores the capture (widest first, built-in graphs re-expanded, `capture_rate_hz`); the ops log (`ops.log`, `zeta ops tail`); elastic leases (`job start --elastic`); atomic `replace` (`--replace` on `job start` and `source ensure`); a file-source `tune_range`; UI view policies (`follow`, `zoom_out`, `auto_scan_stop`, `finished_rows_s`, `scanner`) and `view set\|show\|reset\|clear`, automatic scans on a scanner source, a coarse scan layer, eased follow. **Delivered** on branch `worktree-view-policy`, 2026-10-20. Pending: live acceptance on the field host. | 3, 10 |
| 12 | Capture and transcripts | Resumed channels take the shared bank (the bank factory reaches `Node::open`; the channel's path in `job_context`, `bank` in `source_list`, lag lines by stage kind); rates the resampler accepts (a UHD ladder of exact master-clock divisors in the Soapy source, a snapping `Resampler`); the transcript log (`transcripts.log`, `zeta transcripts tail`); pinned captures (`source_pin`/`source_unpin`, `sources/<id>.json`, restored first on resume); live receive gain (`source_set_gain`, `source_list.gain`, persisted); UI pins and `source pin\|unpin\|gain` commands; `ppm=` correction and an automatic squelch in `fm_demod`/`am_demod`. Spec `docs/superpowers/specs/2026-10-21-capture-and-transcripts-design.md`, plan `docs/superpowers/plans/2026-10-21-capture-and-transcripts.md`. Delivered on branch `worktree-view-policy`, 2026-10-21. Pending: live acceptance on the field host. | 8, 10, 11 |

Plan 5 completes the first vertical slice (design section 8). Plan 6
satisfies the GUI half of slice item 4: the context menu's entries and the
MCP tools reach the same `Node::apply` code path, and the call shows in the
UI's transcript pane through the node's `ops` stream.

## Plan 3 status

Plan 3 is delivered on `transports-agent`: `zeta serve --listen` (authenticated HTTP and
websocket, `zeta-ws`), `zeta --node`/`--secret` (`zeta-client`), `zeta-mcp`, `zeta ask` with the
`zeta-agent` tool loop, and `AGENTS.md` plus `skills/zetasdr` generated by `zeta agents-md`. The
workspace gates passed on the branch (fmt, clippy, 186 tests passed, 1 ignored); after merging plan 2 they
pass with 261 passed, 1 ignored (264 passed with `--all-features`), and the by-hand slice ran against a live
`serve --listen` with a scripted fake model. The live-model test (`cargo test -p zeta-agent
--test live -- --ignored`) and a live `zeta ask` against `field-host:8080` (gemma-4-12b) both
failed on 2026-10-18 with the endpoint reachable but no chat completion within the 300 s attempt
bound. On 2026-10-19 the controller checked that 17.6 GiB of 24 GiB VRAM was free on the host, and a
direct 5-token `curl` POST to `/v1/chat/completions` returned no bytes within 20 s while
`/v1/models` answered and listed `gemma-4-12b`. Chat completions hang on the model server; the
cause is unknown and server-side. The live result is therefore unconfirmed and needs a rerun.
Deferred to later plans: browser websocket authentication and the operation log stream (plan 6),
binary audio and IQ over the websocket (plans 5 and 6), federation authentication (plan 5).
On 2026-10-19 in the evening the field host's chat completions answered again: the live test
`field_host_gemma_tunes_hackrf0` passed (1.2 s) and `zeta ask` against the B210 node answered correctly.

## Plan 4 status

Plan 4 is delivered on `hardware-sources`: rate-aware narrowband jobs (the narrowest exact channel
rate), explicit classifier bands, capability records with `remote` and `push`, a KiwiSDR source
(`zeta-source-kiwi`, `--source-kiwi`), a SoapySDR port (`zeta-source-soapy`, tested against a mock)
with its real driver in the workspace-excluded `zeta-source-soapy-driver` behind
`RUSTFLAGS="--cfg soapy"` (a cfg, not a feature, so no default or `--all-features` build compiles
`soapysdr`; `cargo tree -p zeta -e normal --all-features | grep -c soapysdr` prints 0), and
external-process stages (`zeta-plugin`, plugin protocol 1, `--plugin`) with a standard-library
Python SDK and `examples/python/gain.py`. The workspace gates passed on 2026-10-19: fmt, clippy
with `--all-targets --all-features -D warnings`, `cargo test --workspace` 393 passed, 2 ignored,
and with `--all-features` 396 passed, 2 ignored. By hand against the LAN Kiwi
(`kiwisdr.local:8073`, v1.837, 3 of 8 users before and after, so the slot was released):
`source list` showed `remote` true and `current` null; a 10 kHz `measure` job at 7.1 MHz produced
two records with finite `snr_db` (5.1 and 5.4 dB, labelled `noise`, reception is poor) and
`center_hz` 7 104 582 and 7 103 305; 20 kHz was refused `out_of_range` on `bandwidth_hz`, max
12000; the ignored live test passed (24 blocks in 1.29 s, RSSI -71 dBm). By hand, `gain.py` ran
inside a job of a `serve --listen` node after `graph put`: `stream list` showed the job's two iq
streams, each tailed three 4800-sample frames, and after `job stop` and stopping the server no
`gain.py` process remained. Later on 2026-10-19, with `libsoapysdr-dev` installed, `RUSTFLAGS="--cfg soapy" cargo build -p zeta`
and `cargo clippy -p zeta --all-targets -- -D warnings` passed against SoapySDR 0.8.1 (HackRF
module 0.3.4); `zeta soapy find` printed `[]` with no device attached and a `--source-soapy`
open with no device exited 1 with a clear error. With a HackRF One (r9, firmware v2.3.2) attached later that day, `zeta soapy find` listed it,
`source list` showed its capabilities (0-7.25 GHz, 20 MHz, ci8/ci16/cf32), and a `measure` job at
100.1 MHz with a 200 kHz band produced two events a second apart (no antenna: snr 2.8 dB, label
noise). A 1 MHz band in the debug build lagged continuously and produced no event in 8 s; the
release build is the one to use for wide bands on real hardware. With an antenna, the release
build measured a 1 MHz band at 1 Msps without lag, `measure` at 104.9 MHz showed a broadcast
station (11-20 dB snr, occupied bandwidth 28-98 kHz between runs) and `fm_demod` at 104.9 MHz
with 75 kHz deviation streamed audio frames. Layer-1 labels broadcast FM `unknown` by design: the
plan-2 `fm` rule is narrowband (band at most 25 kHz, envelope cv below 0.2), and the occupied-
bandwidth estimate varies with the FFT bin width. A wideband-FM rule and a steadier bandwidth
estimate are layer-1 follow-ups.

## Plan 5 status

Plan 5 is delivered on `federation`: VITA 49.2 signal-data and context packets (`zeta-vrt`, `ci16`
on the wire, FNV-1a stream ids, a context packet about once a second), a binary mode of the stream
websocket (`?format=binary`: VRT packets for IQ, the typed envelope for audio, text and events,
paced by `{"type":"credit","packets":N}` with skipped frames reported as `lagged`), a binary
subscription with credits in `zeta-client`, another node as a push source (`zeta-source-node`,
`--source-node ID=SOURCE@URL`, secret from `ZETA_NODE_SECRET_<ID>`, 250 kHz default cap widened by
`max_bandwidth_hz=`), VITA 49.2 over UDP as a push source (`zeta-source-vrt`, `--source-vrt`), and
a Pi 5 cross-build, qemu smoke test and systemd deployment (`deploy/pi/`). Discovery is deferred:
v1 nodes talk by URL. The workspace gates passed on 2026-10-19: fmt, clippy with `--all-targets
--all-features -D warnings`, `cargo test --workspace --all-features` 456 passed, 0 failed, 2
ignored (the live Kiwi and the field host model tests), `no_privileged_deps`, `zeta agents-md
--check` and `--skill --check`, and `deploy/pi/smoke.sh` (the aarch64 release binary under
qemu-aarch64). By hand on this box, node A served the tone fixture on 127.0.0.1:7979 and node B
federated it with `--source-node a=tone@http://127.0.0.1:7979` under `serve --listen
127.0.0.1:7980`: B's `source list` showed `a` with `remote` and `push` true and `max_bandwidth_hz`
48000 (the remote file's own limit, below the 250 kHz cap); a 12.5 kHz `measure` job on `a` at 100
MHz produced `carrier` records each second (snr 136.6 dB, as for the local tone), and A's `source
list` showed `tone` held by a `passthrough` job. `job stop` on B did not release it: A still showed
the holder 38 s later, because the port stays tuned until the next tune or exit; Ctrl-C on B
stopped the remote job before the next poll (under 10 ms), and the one-shot form (`job start ...
--then-tail 2,5000` with `--source-node`) returned two events and stopped its remote jobs. Each
federated job start created two `passthrough` jobs on A, the first stopped at once (fixed in the
fix wave: an identical re-tune reuses the remote job). On 2026-10-19 a second node on
field-host (x86, USRP B210 through the SoapySDR UHD module, built with `--cfg soapy`) served
`--source-soapy b210=driver=uhd,serial=31B92DD`; this box federated it with `--source-node
falc=b210@…` through an SSH tunnel because port 7979 is firewalled on that host. The federated
`source list` showed the B210 as a remote push source capped at 250 kHz; under a local `serve`, a
`measure` job at 104.9 MHz with a 200 kHz band produced events each second (snr 16-17 dB) and
`fm_demod` produced audio frames; the remote node showed the passthrough job as the lease holder. A
local `job stop` did not release it then (only a retune, the port's close or its drop did); since
the final-review fix wave the remote passthrough is released when the last local job on the source
stops, after a 2 s grace, and an identical re-tune reuses it, so a federated job start makes one
remote job. Open: the one-shot form (`job start … --then-tail`) streamed for 10 s but produced no
events in two runs, while the same job under `serve` did; it is recorded for the final review. A
federating `serve` stopped with SIGTERM now shuts down like Ctrl-C and stops its remote jobs (fix
wave). The HackRF step of the by-hand plan did not run (the HackRF was unplugged).

Later on 2026-10-19, with port 7979 opened on the field host's firewall, the two-host test ran directly over the
LAN with the merged `main`: one remote passthrough per federated job (the idempotent tune), no packet loss
or lag reported by the new counters, and `measure`, `spectrum` and `fm_demod` over the B210 all produced
output in both one-shot and `serve` form. The one-shot no-events finding turned out to be a cold-start
effect: in the first seconds after the remote node opens the B210, `measure` completes fewer or no 1 s
windows (one event instead of two in the first run after a restart, none in the earlier runs that also went
through an SSH tunnel with a debug build), and it is gone once the device has been streaming for a minute.
A settle period after a device starts streaming, or a more tolerant measure window, is the follow-up.

High-rate run over gigabit Ethernet (this box wired, the field host wired, release binaries both ends,
`max_bandwidth_hz=` raised explicitly): `spectrum` over the federated B210 at 1 Msps (610 frames in
10 s), 4 Msps (1000 frames in 3.3 s) and 16 Msps (10 000 frames over 10 s of signal through a local
`serve`, about 64 MB/s on the wire) all arrived with `missed` 0, no lag or loss counted, and every frame's
span accounted for. A request for 10 MHz with a 10 MHz cap was refused with `suggested` 16 000 000, because
the B210 ships 16 Msps for that request: the shipped-rate cap check from the fix wave working as intended.
Costs at 16 Msps: the local node ran at about 43 % of one core (VRT decode plus the spectrum FFT), the
remote at about 9 %, and both held about 2.1 GB resident, almost all of it the 10 s IQ ring. That ring
size is a follow-up for the Pi: `deploy/pi/zetasdr.service` caps memory at 1 GB, so a Pi node must keep a
shorter ring or refuse caps that would not fit.

## Plan 6 status

Plan 6 is delivered on `ui`: the node's `ops` event stream (one event per applied operation, from
any transport), browser websocket authentication by the `zeta.bearer.<base64url(secret)>`
subprotocol, `POST /ask` (`serve --llm-url/--llm-model`, the `zeta ask` agent run against the node
itself) and `POST /stt` (`--stt-socket`, the `local-voice` daemon), and the web app (`zeta-ui`,
plain JavaScript embedded in the binary and served without auth under `/ui/`): login with the
node secret, source tabs, a right-to-left waterfall fed by the UI's own `spectrum` job,
channel-pinned layer labels and decoded-text ribbons, the transcript fed by `ops` (successful
list polls hidden unless typed, errors always shown), the job list with live counters, the
six-entry context menu on a drag, audio playback of `fm_demod` jobs over the binary websocket, the
editable graph view, the microphone, and a narrow footprint below 700 px with the side pane in a
drawer and answers as ribbons. The workspace gates passed on 2026-10-19: fmt, clippy with
`--all-targets --all-features -D warnings`, `cargo test --workspace --all-features` 500 passed, 0
failed, 2 ignored (the live Kiwi and the field host model tests), `no_privileged_deps`, `zeta agents-md
--check` and `--skill --check`, `deploy/pi/smoke.sh` (PASS, on the aarch64 binary built for plan 5;
it was not rebuilt with the UI), and `cargo test -p zeta --test ui`: all 23 Playwright scenarios
passed in headless Chromium (62 s, no skip line). The first test run filled the disk (22 GB of
incremental cache under `target/debug`); deleting `target/debug/incremental` and running with
`CARGO_INCREMENTAL=0` got it through.

By hand on this box, headless Chromium against `serve --listen 127.0.0.1:7979` with the `tone` and
`aprs` fixtures and `--llm-url http://field-host:8080 --llm-model gemma-4-12b`: both tabs
showed with their tuning ranges; the waterfall painted on each; a drag on the tone opened the menu
with the six entries and the footer `center_hz=100000000 bandwidth_hz=9600 start_ns=… end_ns=…`
(clamped to the last 8 s when the span was longer); "What is this?" logged `classify selection=… →
ok (1 label)` and drew `L1 carrier 0.70` at 100 MHz (snr 114 dB); Decode › NBFM audio started an
`fm_demod` job whose ▶ played 30 frames in 3 s with none dropped; Decode › APRS on `aprs` drew the
`VK3ABC > APRS: >hi` ribbon 2 s later, repeating with a ×N count as the fixture loops; the graph
view of that job showed `channel → nbfm_demod → aprs_decode` with `iq` and `audio` edges and the
channel's params as JSON; the mic, with no `voiced` for this user (no `--stt-socket`), showed "no
speech daemon on this node" (the 503 from `/stt`); at 390×844 the page took the narrow footprint,
the waterfall filled the screen with the ribbon on it, and ☰ opened the transcript and jobs
drawer. The agent question "what sources do you have", typed on the command line, got no answer in
120 s; a direct `POST /ask` under `timeout 120` also returned nothing, and a 5-token `curl` to
the field host's `/v1/chat/completions` returned no bytes in 20 s while `/v1/models` listed
`gemma-4-12b`: chat completions on that server still hang, as on 2026-10-18 and 2026-10-19, so the
live `/ask` answer is unconfirmed. A `/ask` whose client goes away frees the one-run slot (the next
request was not refused 409). Two observations for follow-up: the transcript hides the agent's own
successful list calls too (`source_list` for exactly this question), because the `ops` stream does
not say which client sent an operation; and in the narrow footprint the tab labels drop the tuning
range.

Follow-ups from the final review (fix wave on `ui`). Each `ops` record now names its caller in
`transport`: `Node::apply_from(op, transport)` (`apply` is `"local"`), `"ui"` for `/op` with the
allow-listed header `X-Zeta-Client: ui`, `"http"` for any other `/op` (MCP included, it is an HTTP
client), `"agent"` for the calls of a `/ask` run (`LocalExecutor::tagged`). The transcript hides only
the UI's own successful list polls and prefixes other callers' records (`[agent]`, `[http]`), so the
agent's `source_list` now shows; a list op typed on the command line is shown from its `/op` reply
(ruling R6, replacing R5's typed counter). Errors on the `ops` stream are bounded like ops and results
(`{"error": <variant>, "truncated": true, "bytes": n}` over 8192 bytes). The overlays resolve event
streams with one `GET /streams` per poll and remember jobs without one (`fm_demod`), which had cost
about four lookups a second per job; at most four jobs per tab are watched (the rest show `—`), and a
stream refused for want of slots (503, probed after a failed upgrade) is noted once in the
transcript. Minors: the secret field is cleared after login; switching source stops the UI's own
spectrum job; closing the menu erases the selection; ribbons are capped at 200; `/ui/` files carry a
Content-Security-Policy (`blob:` is needed for the microphone's AudioWorklet) and `nosniff`; narrow
tabs show a compact centre (`144.8`); the e2e run has a 300 s deadline. The aarch64 binaries were
rebuilt with the UI (cross, release, built from 013ceea): `zeta` 23 148 104 bytes, sha256
`be0e943b…09e9c0`; `zeta-mcp` 10 836 096 bytes, `acff40ff…2eb2e0861`; `deploy/pi/smoke.sh` PASS.

Parked: stopped job records are never pruned (keep the last N; a core follow-up); speech-to-text
ribbons for channels (spec §7) are deferred; the `window.zetaTest` hooks used by the e2e scenarios
ship in the production bundle.

Pending: the phone check. Start the node on the LAN address,
`ZETA_SECRET=… zeta --state-dir … --source-file tone=… --source-file aprs=… serve --listen
<wired-ip>:7979` (wired; <wifi-ip> on WiFi), open `http://<wired-ip>:7979/ui/` on the phone,
paste the secret, and check the narrow renderer: waterfall full width, ☰ drawer, a drag opening the
menu by touch, Decode › APRS and its ribbon. The mic will not work there over plain HTTP (browsers
allow it only on a secure origin). Also pending: the microphone against a real `voiced`, the
HackRF walk (`--source-soapy` in the release `--cfg soapy` build, 104.9 MHz, Decode › NBFM audio;
the HackRF was unplugged), and a live `/ask` once the field host's chat completions answer.
The live `/ask` is no longer pending: on 2026-10-19 in the evening gemma-4-12b answered again, and the
UI's agent pane works live against the field host (calls in the transcript, then the answer).

## Plan 7 status

Plan 7 is delivered on `audio-wfm`: a streaming rational resampler (`Resampler`, polyphase L/M,
at least 63 taps) behind every demodulator, so all audio streams are 48 kHz mono in ±1 whatever
the channel rate; `wfm_demod` (75 kHz deviation, 50 µs de-emphasis by default, 15 kHz mono),
`am_demod` (envelope, slow AGC) and `ssb_demod` (Weaver form, `sideband` usb or lsb, `center_hz`
is the dial frequency) as stages and job kinds, with `sideband` a required-nullable op field like
`deviation_hz` and each refused on kinds that do not take it; layer-1 rules for `wfm` (60-250 kHz,
cv below 0.35, rms deviation above 5 kHz, a 19 kHz pilot at least 10 dB up raising 0.6 to 0.9),
`am` (carrier line) and `ssb` (one-sided, with the dial frequency in the evidence); the iNTERCEPT
`signals.json` (594 entries, Apache-2.0, `data/NOTICE`) as a knowledge provider and a layer-2
classifier scoring frequency 0.4, bandwidth 0.3 and modulation 0.3, with the bandwidth required for
entries wider than 50 kHz and ties to the narrowest range; the `Classifier` trait takes the band
centre; and in the UI, Decode entries for wideband FM, AM, USB and LSB, ▶ on any audio job, and
the top layer-2 name drawn beside the layer-1 label. `rustfft` and `num-complex` are built at
`opt-level = 3` in debug builds. The workspace gates passed on 2026-10-19: fmt, clippy with
`--all-targets --all-features -D warnings`, `cargo test --workspace --all-features` 547 passed, 0
failed, 2 ignored (the live Kiwi and the field host model tests) over 80 test binaries,
`no_privileged_deps`, `zeta agents-md --check` and `--skill --check`, and `cargo test -p zeta
--test ui`: all 30 Playwright scenarios passed in headless Chromium (125 s, no skip line; the two
classify scenarios take 19 s and 28 s of it). The `opt-level` override did not move the e2e time
(131.9 s before, 132.0 s after), so the FFT is not where that time goes.

By hand on 2026-10-19 against the field host's B210 (release `--cfg soapy` build of this branch, 104.9
MHz with a 1 MHz capture band), driven from this box with the debug `zeta --node`; one-shot
`--then-tail` is not supported with `--node`, so each job was started, its stream found with
`stream list`, tailed with `stream tail` and stopped. `wfm_demod` with a 200 kHz band streamed
audio frames at 48 000 Hz (786-787 samples, peaks 0.32, 0.50, 0.42). `measure` labelled the
station `wfm` at 0.9 with "19 kHz pilot +16.3 dB" (snr 18.8 dB, occupied 81-83 kHz, cv 0.34,
rms deviation about 17.5 kHz): the station is stereo. `classify` over 2 s spans inside the ring's
last 10 s, timed from fresh `measure` events, returned `L1 wfm` (0.9, pilot +16.1 to +16.7 dB),
`L2 FM Broadcast Radio` (1.0) and `L2 CBET` (0.7, "bandwidth off") on 19 of 20 spans; the first
span came back `unknown` (occupied 56 kHz, below the 60 kHz floor, and cv 0.356, above 0.35) and
so carried no layer-2 label. On the spans that passed, cv sat at 0.333-0.345, close under the 0.35 limit.
`knowledge lookup --region AU` at 104.9 MHz returned the catalogue signatures FM Broadcast Radio
(87.5-108 MHz, 250 kHz, WFM), CBET and HD Radio (FM), and no allocations. `am_demod` (10 kHz) and
`ssb_demod --sideband usb` (3 kHz) both started and streamed 48 kHz audio frames (peaks near
0.0002: a constant-envelope FM carrier carries no AM). Every job started was stopped; the node's
own spectrum job was left running.

Parked: `classify` measures the band twice (once for layer 1, once in the catalogue classifier);
broad catalogue entries such as CBET (88-108 MHz, 2.2 kHz) score 0.7 with the bandwidth off and
show up beside the right answer; `fm` does not map to AFSK, so APRS is named by the decoder, not
the catalogue; AM and SSB playback has not been tried in a browser; stereo and RDS are not built.
The wfm rule's cv limit has little margin on a real station (one span in 20 missed it); letting a
strong 19 kHz pilot carry a marginal band is a candidate fix. `knowledge lookup --region AU` returned no
allocation covering 104.9 MHz.

## Plan 8 status

Plan 8 is delivered on `wideband` (ff919fb..14f1df7 plus this docs commit): `PfbBank` in
`zeta-dsp` (2x-oversampled polyphase analysis, bins at least 750 kHz wide, 8..=256 of them,
Blackman prototype at 0.75 bin with 16·bins taps, at least 60 dB down in every bin whose span
excludes a tone) behind core's `ChannelBank`/`BankFactory`, one bank per source at 2 Msps or more on
its own thread; the `channel` stage draws from the nearest bin when the request fits one bin width
and the output rate divides the bin rate, and takes the direct path otherwise, with the same
semantics either way (`StageContext` passes the bank to every stage); spectrum rows v2 at a fixed
`rows_per_s`, quantised to u8 at 0.5 dB with `floor_db`, `size` up to 8192 and following the band;
the IQ ring sized from `--ring-budget-mb` (default a quarter of `MemTotal`, at most 4 GiB; 1..10 s,
recomputed on retune, released on shrink); `JobKind::Scan` (16 MHz steps at most, 250 ms dwell,
the lease on the whole span, `conflict` naming the scan for anything else on the source); and in
the UI, `#band`, pan and wheel zoom (render-only inside the capture, a retune beyond it) and the
stitched scan waterfall with "Scan this span". `wfm_demod` now decimates before its audio filter.

The workspace gates passed on 2026-10-20 at 14f1df7: fmt; clippy with `--all-targets
--all-features -D warnings`; `cargo test --workspace --all-features` 588 passed, 0 failed, 3 ignored
(live Kiwi, live the field host model, and the release-only PFB timing test) over 86 test binaries;
`zeta agents-md --check` and `--skill --check`; `cargo test -p zeta --test ui`: all 35 Playwright
scenarios passed in headless Chromium (144.6 s, no skip line). The release timing test
(`cargo test -p zeta-dsp --release --test pfb pfb_runs -- --include-ignored`) pushed 2 s of 24 Msps
through 32 bins in 1.39 s on this laptop: 0.694 of one core.

By hand on 2026-10-20 against the field host's B210 (release `--cfg soapy` build of 14f1df7, 12 cores),
driven from this box with the debug `zeta --node`. The B210 asserts `max_bandwidth_hz` 16 MHz:
the SoapySDR capability probe runs while UHD's automatic master clock sits at 16 MHz, so
`source ensure --bandwidth-hz 24000000` is refused `out_of_range` (max 16000000) although the
radio, on USB 3, can do more; the acceptance ran at 16 Msps. At 98 MHz / 16 MHz the node logged
`ring for b210: 10.00 s at 16000000 Hz (1280.0 MB)`; the spectrum job's rows had `size` 4096
(3906.25 Hz bins), `q` present, `rows_per_s` 20 and `start_ns` 50 ms apart. Four `wfm_demod`
jobs (100.5, 94.5, 101.3 and 93.7 MHz, 200 kHz each, bank-served) and a `measure` (95.3 MHz:
`wfm` 0.9, pilot +40.9 dB, snr 29 dB) ran together beside the spectrum job; each audio stream
gave 48 000 Hz frames with peaks above 0.05 (2 s medians 0.20, 0.46, 0.84, 0.16). A fifth
candidate, 102.5 MHz (40 dB above the floor), demodulated to peaks near 0.008 and was replaced:
a carrier with little or no program audio there, not investigated. CPU over 10 s from
`/proc/<pid>/task/*/stat`: the node 121 % of one core in all (`top` agreed, 120 %), of which the
bank thread `pfb-98000000` 33 %, the SoapySDR reader 9 %, the tokio workers 72 % for the six jobs,
and 7 % another node thread; with the spectrum job alone the node used 23 %. So each job is far
under one core (about 13 % per channel job on average), but the bank at 16 Msps costs 33 % of one
core, over the 20 % target (it would be about 50 % at 24 Msps). `scan` over 0-1 GHz started at
the B210's 42 MHz floor and stepped 56, 72, ... 1000 MHz: 61 distinct `center_hz` values, four rows
per step about 270 ms apart, one sweep in 19.2 s (two full sweeps seen in 60 s). During the scan
`job start --kind measure` and `--kind wfm_demod` on b210 were both refused `conflict` with the
scan as holder and the option "stop the scan holding the source"; `source list` showed the scan
as the only holder and `current` 712 MHz / 16 MHz; after the stop, no holders and `current` at
the last step (232 MHz / 16 MHz). Headless Chromium against the live UI during a second scan
switched to scan mode and painted the stitched view (84 columns in 25 s; `scratch/ui/scan-live.png`),
with the FM band bright near 100 MHz. The B210 was left at 104.9 MHz / 1 MHz with no jobs. Not
done: the `--ring-budget-mb 1024` restart (the node was not restarted); the sizing line above is
the default budget's. A UI tab open elsewhere kept restarting its own 1 MHz spectrum job on the
B210 and had to be stopped before the wide retune.

Parked: the scan runs its FFT inline on a worker (spawn_blocking it); the dwell does not skip
the PLL settling time at its start; each dwell copies 32 MB from the ring; a remote node's
`current` is cleared when idle; the bank's cost is over its 20 % target on the field host (above); the
overlays do not reposition on a view change; the scan view does not redraw a full sweep from the
ring rows on zoom; core does not clamp a scan step to a sub-band span (min(step, span), centre
inside the span) pending the fix wave; and the Soapy capability probe should read the rate range
after the master clock is raised (or assert the device's real maximum).

After the plan-8 fix wave (2026-10-20): the Soapy source probes a UHD device's sample-rate range at its
maximum clock, so the B210 reports 61.44 MHz with no device argument and a 24 MHz request becomes a 30.72
Msps capture (ring 10 s, 2.46 GB on the field host). With a spectrum job, four `wfm_demod` jobs and a `measure`
on that capture the node used about 173 % of a core: the bank thread 30 % (down from 50 % before the PFB
optimisations), the decoders 12-20 % each, audio clean. The plan's 20 % bank target holds at 16 Msps and is
missed by ten points at 30.72 Msps; `target-cpu`/multiversioning and a partial DFT for few bins are the
parked next steps. The scan, lease conflicts and stitched view were exercised live at 16 Msps earlier the
same night (0-1 GHz in 61 steps, 19.2 s per sweep). The plan's Conventions block says Hann for the PFB
prototype; the code uses Blackman (ruling R4) with a 0.75-bin cutoff and 16 × bins taps (ruling R5).

## Plan 9 status

Plan 9 is delivered on `decoders` (branch from main 857dbaa, tasks 1-10; the last implementation
commit before this docs commit is fb84e51). Not yet tried live: the live acceptance on the field host (below)
has not run, so nothing here was received off air.

What each decoder is:

- `pocsag_decode`: POCSAG from a 12.5 kHz FM channel; 512, 1200 and 2400 baud detected
  automatically (BitSync tracks 1 % rate error); one event per message with `address`, `function`,
  `kind`, `text`, `baud` and `protocol`.
- `rds_decode`: RDS from a 200 kHz broadcast band (19 kHz pilot PLL, 57 kHz band-pass, mixed
  to baseband); events `pi`, `ps`, `rt`. The first PI emits an event; a PI change clears PS and RT.
  The sensitivity floor is about 16-20 dB carrier to noise over 240 kHz, measured on synthetic
  fixtures only.
- `adsb_decode`: 1 090 MHz with at least 2 MHz of band (2.4 MHz asked for); the stage in `zeta-ext`
  feeds dump1090 (GPL, so an external process; `ZETA_DUMP1090` overrides the path) and parses its SBS
  output into events (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`,
  `track_deg`). The job fails naming dump1090 when the host has none.
- `acars_decode`: ACARS from a 10 kHz AM channel, 2400 baud MSK; one event per CRC-clean block
  (`reg`, `flight`, `label`, `text`, `crc_ok`).
- `classify` tries APRS, POCSAG and ACARS on narrow bands and RDS on wide ones; the RDS attempt
  uses a measured bandwidth threshold of 100 kHz (the fixture measures 138.5 kHz; ruling R5).
  `knowledge_lookup` signatures carry `decoder` (a table in the catalogue module).
- UI: decode menu entries and event ribbons for POCSAG, ACARS, RDS and ADS-B, and the ADS-B refusal
  on a narrow band.

Oracles, all installed on the dev host: multimon-ng decodes the POCSAG fixture (it needed the
modulation resampled 48 kHz to 22 050 Hz first); acarsdec decided the ACARS details the plan had
guessed: the tones are differential (2400 Hz keeps the bit, 1200 Hz inverts it), the CRC is
CRC-16/KERMIT (0x8408 reflected, init 0, low byte first, over the parity-inclusive bytes from mode
to ETX; vector 0x2189) and the pre-key is 128 bits (64 gave no sync); dump1090 (dump1090-fa 8.2-1)
decodes the synthetic DF17 fixture (ICAO 4840D6, callsign ZETA1). The oracle tests skip with a
printed line when the program is absent. The acarsdec oracle was flaky at about 50 % until its WAV
was padded with 2 s of silence (62ea5b5); 3 of 3 full-suite runs were then green.

Tests, as the task reports and ledger give them (the whole workspace at the branch head: 671 passed, 0 failed, 3 ignored, the 40 browser scenarios included): POCSAG 9 tests at task 1, RDS 12,
ACARS 8 plus 3 framer tests added in task 7, zeta-ext 2 unit plus 11 integration (the real dump1090
oracle among them), `zeta-core` ops 51, `zeta` cli 40, catalogue 10, and the UI suite 40 of 40
Playwright scenarios (160 s of the 300 s deadline, no skip line).
Task 10's README one-shot was run: `pocsag_decode` on the
`gen_pocsag` fixture at 148 637 500 Hz gives one event, address 1234567, function 3, `alpha`,
"HELLO ZETA", 1200 baud.

Parked (from the ledger): `bch_failures` is not reset per batch in the POCSAG decoder; no tests for
its BCH-failure and Tone branches; the POCSAG stage and `decode_pocsag_iq` duplicate the pipeline;
no test for a mid-run rate change in POCSAG or ACARS, or for `Lagged` in POCSAG, RDS or ACARS; 38 kHz stereo content is
untested against the RDS 57 kHz band-pass; an RDS PI change clears PS and RT without confirmation;
`write_all` to dump1090 can block if a live child stops reading (cancel still ends it); `free_port`
has a check-to-use race; the ADS-B clean-exit regression test could not reproduce the pre-fix race;
`parity_error_drops_the_block` also fails the CRC, so it does not isolate parity; UI ribbon
formatters print "null" or "undefined" for absent pocsag, acars and rds fields (partial events
only).

Live acceptance (to do, on the field host with dump1090, multimon-ng and acarsdec installed and the B210):
`rds_decode` on 104.9 MHz / 200 kHz (expect PS within 10 s), `pocsag_decode` on 148.6375 MHz /
12.5 kHz for 60 s, `acars_decode` on 131.550 MHz / 10 kHz for 120 s, `adsb_decode` on 1 090 MHz /
2.4 MHz for 60 s (a clean run without events is a pass for the plumbing only). Outcomes are not yet
recorded.

Live acceptance on the field host's B210 (2026-10-20, 06:45-07:05 AEST, merged main 6063a2d, dump1090 at
/usr/local/bin/dump1090 found at startup): `rds_decode` on 100.5 MHz / 200 kHz (the strongest station in a
20 MHz spectrum row, 56 dB above the floor) produced within 40 s the events PI `1361`, PS `ZINC 96 `, RT
`Wake up with Bilko & Jade`, PTY 11, from 2A then 0A groups. On 104.9 MHz (16.8 dB SNR, pilot +15.3 dB,
84 kHz wide), 107.1 MHz and 94.5 MHz no RDS event arrived in 30 s: the decoder's 16-20 dB CNR floor, or
no RDS carried; `classify` over the 104.9 MHz capture gave the layer-1 `wfm` label and the catalogue's
`FM Broadcast Radio`, no `RDS`. `pocsag_decode` on 148.6375 MHz / 12.5 kHz (60 s), `acars_decode` on
131.550 MHz / 10 kHz (120 s) and `adsb_decode` on 1090 MHz / 2.4 MHz (60 s) each ran without error
(captures at 120 kHz, 120 kHz and 2.4576 MHz; dump1090 started with the SC16 float path and exited cleanly
at the stop, no process left) but saw no events: plumbing pass, no signal verified at that hour with the
antenna in use. The radio was left at 104.9 MHz / 1 MHz with no jobs.


## Plan 10 status

Plan 10 is delivered on `speech` (tasks 1-8), fast-forward merged into `main` at effb67d on
2026-10-20, and tried live on the field host the same day (below).

What was built:

- Audio frames carry a channel count (interleaved; 1 today) on the bus, the plugin wire and the
  websocket.
- Stages: `denoise` (RNNoise through the pure-Rust `nnnoiseless`, BSD-3-Clause), `vad` (energy over a
  tracked noise floor, with onset look-back and hangover; emits `vad` events and the speech segments)
  and `transcribe` (crate `zeta-speech`: accumulates an utterance, downmixes and resamples to 16 kHz
  mono, encodes WAV, calls a `SpeechBackend`, emits one `transcript` event per utterance).
- Speech backends behind one trait: `SocketBackend` (the `local-voice` line protocol, `languages`
  probe limited to 2 s) and `NodeBackend` (another node's `POST /stt`, one hop only); `/stt`,
  `GET /stt/languages` and `?lang=` run on the same trait. Global flags `--stt-socket` and `--stt-node`
  (exclusive; ignored with `--node`).
- Attached jobs: the `stream_in` stage, the `job_attach` op and `zeta job attach`; `transcribe` is the
  first attachable kind (graph `stream_in -> denoise -> vad -> transcribe`).
- UI: transcribe entries, job attach and transcript ribbons.
- The separation slot: `examples/python/separate_stub.py` (an external audio-to-audio plugin) replaces
  `denoise` through `graph put`; `crates/zeta/tests/speech_chain.rs` proves it ends in a `transcript`.
  `examples/python/stt_daemon_stub.py` is the bring-your-own daemon.
- Docs: `docs/speech.md` (installing `local-voice` on a node's host, bring your own daemon, one daemon
  for several nodes), "Transcribing a channel" in field craft, README.

Final fix wave (after the whole-branch review):

- The voice gate's floor on digital silence (R15): a frame under -80 dB still counts as quiet and
  never raises the floor, but now lowers it to -80 dB, its minimum (and seeds an unseeded floor
  there). A chain that starts or restarts inside a burst therefore hears speech from the first gap
  on. The trade-off, documented on `Vad`: after a gap of exact zeros, -50 dB channel noise is
  forwarded for about 1 s plus the hangover until the floor re-adapts.
- The floor across a lag (R16): a lagged input no longer recreates the detector. The floor is kept;
  only the run in progress, its look-back and the stage's pending samples are dropped, and a run
  already announced is closed with one `speech: false` where its forwarded audio stopped.
- A deterministic speech fixture (R17): every test that builds the `speech` fixture uses one
  helper (`crates/zeta/tests/common/mod.rs` `speech_fixture`), which puts the 2 s pause before the
  3 s of bursts, so a chain starting with the file opens on silence. The retry in `speech_chain.rs`
  is gone: one `graph put`, one tail.
- Because the pause delays the first transcript by about 2.7 s of file time, the two `attach.rs`
  transcript windows were widened from 10 s to 20 s (the UI ribbon scenario's bound).
- Wider timing margins in the `zeta-speech` tests, and the `/stt` 503 text is now `serve has no
  speech backend (--stt-socket or --stt-node)`.

Gates at the branch head, `cargo test --workspace --all-features --no-fail-fast`:

- Run 1: 763 passed, 0 failed, 3 ignored.
- Run 2: 761 passed, 2 failed, 3 ignored. The failures were `attach`'s two transcript tests on
  their 10 s windows, since widened.
- Run 3, with the 20 s windows: 762 passed, 1 failed, 3 ignored. The failure was `zeta-dsp`
  `acars::tests::acarsdec_decodes_our_fixture`, an external-oracle test unrelated to this plan,
  which passed 3 of 3 when run alone.

Each run included `zeta` `cli` 43, `attach` 5 (ok in runs 1 and 3), `speech_chain` 1, and the UI
suite (`ui_scenarios_pass_in_headless_chromium`) ok in 171.65 s, 164.04 s and 171.61 s, with no
`ui e2e skipped` line.

Repeated runs, unloaded:

- `cargo test -p zeta --test speech_chain`: 5 of 5 ok.
- `cargo test -p zeta --test attach`: 3 of 3 ok before the window change and 3 of 3 after.

`cargo fmt -p` for each of the 17 members, `cargo clippy --workspace --all-targets --all-features --
-D warnings`, `zeta agents-md --check` and `--skill --check` are clean.

Observation for live acceptance: in the in-process attach chain, the whole chain occasionally starts
about 4 s late (seen in 1 of 5 unloaded probe runs: the `vad` and `transcript` events all arrive
about 4 s later than usual). Watch the time to the first `transcript` on air.

Live acceptance on the field host (2026-10-20, as the `sdr` user): `local-voice` installed per docs/speech.md
section 1 (clone from a git bundle under `~/zetasdr/local-voice`, `uv python install 3.13`, `uv sync`,
the Parakeet model copied, `voiced.service` as a user unit with linger; `languages` answers
`error:unknown command`, `file:` answers a transcript). The field host's own source tree is on branch
`wideband` with uncommitted work and no shared history with this repository, so the speech branch was
delivered as `~/zetasdr/scratch/zetasdr-speech.bundle` and built in a separate clone
(`~/zetasdr/speech-src`, `RUSTFLAGS="--cfg soapy"`, target `~/zetasdr/speech-target`); the node was
restarted from that binary with its usual flags plus `--stt-socket $XDG_RUNTIME_DIR/voiced.sock`
(the log shows `speech backend: socket:/run/user/1021/voiced.sock`). The running `spectrum` and
`adsb_decode` jobs were stopped to free the B210, then `job start --kind wfm_demod --source b210
--center-hz 100500000 --bandwidth-hz 200000` and `job attach --kind transcribe --job <id>` over
`--node`. Within 40 s of tailing, the attached job's two event streams carried 18 `vad` transitions
and 7 `transcript` events from the programme's audio, for example "There's nothing left inside"
(3480 ms), "Maybe you can get down." (2160 ms) and "The storm is really bottom outside." (5080 ms),
each `language` en, `backend` the socket. Observations: `stream tail --timeout-ms` is capped at
10 000, so a tail loop is needed; on compressed broadcast audio the voice gate fragments speech into
runs of 0.6-5 s ("Um", "Baby."), the floor-rise-rate effect the final review predicted, so a slower
floor rise or a minimum-tracking floor is the first thing to try next; the first transcript arrived
about 20 s after the attach (the ~4 s late start was not isolated on air). The old binary at
`~/zetasdr/target/release/zeta` (the `wideband` tree) is untouched for rollback.

## Plan 11 status

Plan 11 is delivered on `worktree-view-policy` (tasks 1-8 and the final-review fixes, base `main`
f7757cd), 2026-10-20. Live
acceptance on the field host is pending.

What was built:

- Resume restores the capture: each source is first held at the band bounding its rigid jobs
  (when the radio can capture it), then its jobs launch a scan first, then widest first, elastic
  after rigid; a built-in graph whose `capture_rate_hz` differs from the actual rate is
  re-expanded.
- The ops log (`ops.log`, rotated at 8 MiB, appends serialised; successful read-only ops without
  their result) and `zeta ops tail`.
- A rigid start inside the capture elastic jobs follow is leased there without a retune.
- Elastic leases (`job start --elastic`, `spectrum` only; `source_list.elastic`).
- Atomic `replace` (`--replace ID[,ID]` on `job start` and `source ensure`; the conflict option
  `retry with replace: [...]`).
- The file-source `tune_range` (`--source-file ID=PATH,tune_min_hz=N,tune_max_hz=M`).
- UI: the page's own job is elastic; view policies (`follow`, `zoom_out`, `auto_scan_stop`,
  `finished_rows_s`, `scanner`) and `view set|show|reset|clear`; finished rows age out; the conflict
  offers "stop <kind> <id> and retry"; automatic scans on a scanner source when zoomed past the
  capture; a coarse scan layer under the fine capture; rigid bands outlined; eased follow; the
  shown-source fallback scan stops on a zoom back in and the view returns.

Upgrade: both ends of a `--source-node` link must run this version together (`job_start` gained
the required `elastic` and `replace`, which an older node rejects, and the reverse). A rigid page
`spectrum` job persisted by an older version resumes rigid; the page reuses an elastic one first.

Gates at the branch head:

- `cargo test --workspace --all-features --no-fail-fast`: 811 passed, 0 failed, 3 ignored (106 `test
  result` lines), no `FAILED`, no `ui e2e skipped`. The UI suite
  (`ui_scenarios_pass_in_headless_chromium`, 64 scenarios) ran ok in 233.93 s in the final gate
  (233.04 s alone).
- `cargo fmt -p` for each workspace member (all but the excluded `zeta-source-soapy-driver`),
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `zeta agents-md --check`
  and `--skill --check` are clean.

Deferred:

- The shown-source fallback scan's span does not follow later zooms (in scan mode the view stays
  inside the scan; a zoom back in to a capturable band stops it).
- A replace that times out (10 s) can leave the port tuned with no holder.
- An ease started in a hidden tab keeps `wf.easing` true until the tab is visible, which pauses automatic scan decisions.
- A source that faults before its first band shows no coarse layer.
- `reconcile_rate` is redundant with resume's `fit_capture` and can restart a job once needlessly.
- The UI suite runs at about 234 s of its 300 s deadline.

## Plan 12 status

Plan 12 is delivered on `worktree-view-policy` (tasks 1-10, base plan 11), 2026-10-21. Live
acceptance on the field host is pending.

What was built:

- Task 1: resumed channels take the shared bank; the channel path is in `job_context`,
  `source_list.bank` shows `bin_hz` and `consumers`, lag lines name the stage kind.
- Task 2: the resampler is exact up to a reduced ratio of 2 048 and snaps within 1 000 ppm above it.
- Task 3: UHD sources ask for exact master-clock divisors; an actual rate below the band is retried
  once, then refused.
- Task 4: `transcripts.log` and `zeta transcripts tail [--lines N] [--job ID]`.
- Task 5: pinned captures (`source_pin`/`source_unpin`, `sources/<id>.json`), restored first on
  resume; conflicts offer `unpin source <id> (source_unpin)`.
- Task 6: live receive gain (`source_set_gain`, `source_list.gain`, persisted, `zeta source gain`).
- Task 7: the page: pins outlined and counted as holders, `source pin|unpin|gain` commands,
  "unpin and retry".
- Task 8: `ppm=N` in `--source-soapy` args.
- Task 9: an always-on automatic squelch in `fm_demod`/`am_demod` (`squelch_db`/`squelch` node
  params, `squelch` events, the page's `squelch` tag); no `job_start` field.
- Task 10: README, field-craft ("Holding a band", "Reading transcripts", the squelch line), this
  status.

Gates at the branch head:

- `cargo test --workspace --all-features --no-fail-fast`: 873 passed, 0 failed, 3 ignored (110 `test
  result` lines), no `FAILED`, no `ui e2e skipped`. The UI suite
  (`ui_scenarios_pass_in_headless_chromium`) ran ok in 234.32 s.
- `cargo fmt -p` for each workspace member, `rustfmt --edition 2024 --check` on the driver,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `zeta agents-md --check`,
  `--skill --check` and `cargo check` in the driver crate are clean.

Deferred:

- The resampler refusal text still names the old 100 000 limit though the threshold is 2 048.
- A conflict on a pinned source with no job holders still lists "stop one of the holders" (fix:
  only when holders is non-empty); the guide should say one-shot commands restore and apply pins
  too.
- Two Soapy gain tests have 50-100 ms timing margins (tighten); `set_gain` may hold the tune gate
  up to 2x `retune_timeout`.
- The squelch's floor window is 100 blocks, about 1 s live (10 ms blocks): tune at live
  acceptance. The guide's stage-params list omits `squelch`/`squelch_db`. The squelch tag shows
  only on watched rows.
- The driver crate's doc link still names the removed `split_gain`.
- The transcript tee's cancel-branch drain stops on a `Lagged` error; `ops.log` and
  `transcripts.log` writes are synchronous on runtime threads.
- Plan 11's leftovers are still open: the fallback-scan span does not follow later zooms; the UI
  suite runs near its deadline (234 s of 300 s).
