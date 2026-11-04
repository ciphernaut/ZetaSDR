#!/usr/bin/env python3
"""Generate the ZetaSDR architecture diagram (SVG) and the page around it.

Edit the data below, run `python3 docs/architecture/gen.py`, commit the outputs.
Solid boxes are shipped; amber dashed boxes are pencilled-in plans (TODO.md and
the plan series). Numbered discs point at the notes under the figure.
"""
from __future__ import annotations
import html, pathlib

HERE = pathlib.Path(__file__).parent
AS_OF = "2026-11-04, main a58389d, v0.1.0; plans 19.2 and 26 merged"
SHIPPED = "shipped (plans 1-24 and 26 merged; 19.3 and 25 not)"
PLANNED = "planned (plan 19.3, the rtl_433 row, the rest of the UI wave, TODO gists)"
AMBER = "#b7791f"
FONT = "Inter, 'Segoe UI', Helvetica, Arial, sans-serif"
W = 1660

# --- text helpers -----------------------------------------------------------
def wrap(text: str, width_px: float, size: float) -> list[str]:
    cw = size * 0.53
    maxc = max(8, int((width_px - 16) / cw))
    out: list[str] = []
    for para in text.split("\n"):
        words, line = para.split(), ""
        for w in words:
            cand = (line + " " + w).strip()
            if len(cand) > maxc and line:
                out.append(line); line = w
            else:
                line = cand
        out.append(line)
    return out

def esc(s: str) -> str:
    return html.escape(s, quote=False)

# --- drawing primitives -----------------------------------------------------
parts: list[str] = []
def emit(s: str): parts.append(s)

def box(x, y, w, h, title, body="", planned=False, note=None, title_size=12.5, body_size=10.5, tag=None):
    stroke = AMBER if planned else "currentColor"
    dash = ' stroke-dasharray="6 4"' if planned else ""
    fill_op = "0.03" if planned else "0.07"
    emit(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6" fill="currentColor" fill-opacity="{fill_op}" stroke="{stroke}" stroke-width="{1.6 if planned else 1.2}"{dash}/>')
    ty = y + 16
    style = ' font-style="italic"' if planned else ""
    emit(f'<text x="{x+8}" y="{ty}" font-size="{title_size}" font-weight="700"{style}>{esc(title)}</text>')
    if planned:
        emit(f'<text x="{x+w-8}" y="{ty}" font-size="9" font-weight="700" text-anchor="end" fill="{AMBER}" letter-spacing="0.08em">{esc(tag or "PLANNED")}</text>')
    elif tag:
        emit(f'<text x="{x+w-8}" y="{ty}" font-size="9" text-anchor="end" opacity="0.7" letter-spacing="0.06em">{esc(tag)}</text>')
    if body:
        lines = wrap(body, w, body_size)
        op = ' opacity="0.85"' if planned else ""
        emit(f'<text x="{x+8}" y="{ty+15}" font-size="{body_size}"{op}>')
        for i, ln in enumerate(lines):
            dy = 0 if i == 0 else body_size + 2.5
            emit(f'<tspan x="{x+8}" dy="{dy}">{esc(ln)}</tspan>')
        emit('</text>')
    if note is not None:
        notes = note if isinstance(note, (list, tuple)) else [note]
        callout(x, y, notes[0])
        # further discs sit on the top edge just past the title, clear of its letters
        tx = x + 8 + len(title) * title_size * 0.6 + 14
        for k, n in enumerate(notes[1:]):
            callout(tx + 22 * k, y, n)
    return (x, y, w, h)

def need_h(title, body, w, body_size=10.5):
    n = len(wrap(body, w, body_size)) if body else 0
    return 16 + 15 + n * (body_size + 2.5) + 2

def row(y, items, xs, w, min_h=0):
    """items: list of dicts(title, body, planned, note, tag). Draws them at one height; returns that height."""
    h = max([min_h] + [need_h(i["title"], i.get("body", ""), w) for i in items])
    for x, i in zip(xs, items):
        box(x, y, w, h, i["title"], i.get("body", ""), planned=i.get("planned", False), note=i.get("note"), tag=i.get("tag"))
    return h

def callout(x, y, n, planned=False):
    # a numbered disc on the box's top-left corner
    emit(f'<circle cx="{x}" cy="{y}" r="9" fill="currentColor" fill-opacity="{0.9}"/>')
    emit(f'<text x="{x}" y="{y+3.6}" font-size="10.5" font-weight="700" text-anchor="middle" fill="#ffffff" stroke="none">{n}</text>')

def band(y, h, label, planned=False):
    emit(f'<rect x="112" y="{y}" width="{W-112-4}" height="{h}" rx="8" fill="currentColor" fill-opacity="0.025"/>')
    emit(f'<text x="18" y="{y+16}" font-size="10.5" font-weight="700" letter-spacing="0.12em" opacity="0.75">{esc(label)}</text>')

def arrow(x1, y1, x2, y2, label=None, planned=False, dash=False, anchor="middle", lx=None, ly=None, both=False):
    stroke = AMBER if planned else "currentColor"
    d = ' stroke-dasharray="5 4"' if (planned or dash) else ""
    ms = ' marker-start="url(#arrow-start)"' if both else ""
    emit(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{stroke}" stroke-width="1.3"{d} marker-end="url(#arrow)"{ms}/>')
    if label:
        lx = (x1 + x2) / 2 + 6 if lx is None else lx
        ly = (y1 + y2) / 2 + 3.5 if ly is None else ly
        emit(f'<text x="{lx}" y="{ly}" font-size="10" text-anchor="{anchor}" opacity="0.85">{esc(label)}</text>')

def label(x, y, text, size=10, anchor="start", bold=False, op=0.8):
    emit(f'<text x="{x}" y="{y}" font-size="{size}" text-anchor="{anchor}" opacity="{op}"{" font-weight=\"700\"" if bold else ""}>{esc(text)}</text>')

# --- layout -----------------------------------------------------------------
# Columns: content x=130..1350, outside-the-node column x=1382..1640
CX0, CX1 = 130, 1296
OX0, OX1 = 1404, 1640

# Title
emit(f'<text x="18" y="34" font-size="22" font-weight="800">ZetaSDR architecture</text>')
label(300, 34, f"as it stands {AS_OF}; planned work pencilled in amber", size=12, op=0.8)
# legend
emit(f'<rect x="18" y="48" width="26" height="12" rx="3" fill="currentColor" fill-opacity="0.07" stroke="currentColor" stroke-width="1.2"/>')
label(50, 58, SHIPPED, size=10.5)
emit(f'<rect x="350" y="48" width="26" height="12" rx="3" fill="currentColor" fill-opacity="0.03" stroke="{AMBER}" stroke-width="1.6" stroke-dasharray="6 4"/>')
label(382, 58, PLANNED, size=10.5)
callout(840, 54, "n"); label(854, 58, "see note n under the figure", size=10.5)
emit(f'<line x1="1010" y1="54" x2="1050" y2="54" stroke="currentColor" stroke-width="1.3" marker-end="url(#arrow)"/>'); label(1056, 58, "data or control flow (label says which)", size=10.5)

# ---- layout engine: measure first, then draw -------------------------------
def cols(n, gap, x0=CX0, x1=CX1):
    w = (x1 - x0 - (n - 1) * gap) / n
    return [x0 + i * (w + gap) for i in range(n)], w

def rowh(items, w, min_h=0):
    return max([min_h] + [need_h(i["title"], i.get("body", ""), w) for i in items])

def draw_row(y, items, xs, w, h):
    for x, i in zip(xs, items):
        box(x, y, w, h, i["title"], i.get("body", ""), planned=i.get("planned", False), note=i.get("note"), tag=i.get("tag"))

OW = OX1 - OX0 - 20
OX = OX0 + 10
PAD = 14
GAPY = 14

# ---- content ---------------------------------------------------------------
clients = [
 dict(title="zeta CLI", note=1, body="crate zeta. zeta <group> <verb> --flag: one-shot (opens the node in-process, applies one op, exits) or --node URL --secret against a serving node. zeta serve runs the node (--listen, --llm-*, --stt-*). Local-only: ops tail, transcripts tail, modes check, soapy find. --then-tail starts a job and tails it."),
 dict(title="Web UI", note=(10, 18, 20), body="crate zeta-ui: plain JS embedded in the binary, served at /ui/. Waterfall (fine capture + coarse scan layer, blank under a mask), ribbons, job list, graph view, map (vendored Leaflet), channel-plan axis, Ctrl-K command line, microphone to /stt. A boxed demod is fitted to the measured signal first; outlines drag to retune (job_retune) and have a menu (Fit, widths, Stop); job tags link to the rows. Its own spectrum job is elastic. Listen to all plays a mix; a speaking channel lights up. A pane strip under the waterfall holds Subjects (one row per aircraft, sonde or station; the map tracks each), Explore (burst captures: What is this, Download, Work the bits) and Bits (hex rows, sync hits, hypotheses with Try, the eye, the clock). Themes are folders (console default, plain, cyberpunk, terminal, psytrance, vibecoder, the operator's own) served at /ui/themes/<id>/."),
 dict(title="MCP server", body="crate zeta-mcp: zeta-mcp --node URL. One MCP tool per operation, generated from the schema; tool text from AGENTS.md."),
 dict(title="Python SDK and scripts", body="sdk/python zeta_plugin (plugin protocol), examples/python (cb_watch.py: elastic spectrum, demod + transcribe on active CB channels; gain.py; stt_daemon_stub.py; separate_stub.py), skills/zetasdr (the agent skill), deploy/pi (systemd unit, Pi build)."),
 dict(title="Agent", note=16, body="crate zeta-agent: zeta ask \"REQUEST\" and POST /ask. OpenAI-compatible tool-calling loop over the operation set; refuses stops and replace unless the run allows them; tools and guide generated from AGENTS.md; its calls appear on the ops stream as transport agent. Asked from the page (page_tools), it also has view_set for that tab's view settings, applied by the page, never by the node."),
]
client_labels = ["in-process or HTTP", "HTTP + ws", "HTTP", "HTTP", "HTTP, or in-process via /ask"]
llm = dict(title="LLM server", body="OpenAI-compatible chat API (e.g. llama.cpp or vLLM), on the node's host or another. The agent sends the tool list and the transcript; thinking off, token budget, prose-answer detection.")

ws = dict(title="zeta-ws: HTTP + WebSocket, bearer secret on every route", note=1, body="POST /op (one operation, 30 s cap) · GET /schema · GET /health (build, uptime, stages_running) · GET /streams · ws /stream/{id} (text frames; ?format=binary ships IQ as VRT) · GET /streams/{id}/tail · POST /ask (agent) · POST /stt, GET /stt/languages (speech) · GET /captures/<id>.sigmf-meta|-data · GET /agents-md · /ui/ and /ui/themes/<id>/ (no bearer for a ready theme's files). Routes: http.rs, stream.rs, ask.rs, stt.rs, ui.rs, auth.rs.")
client = dict(title="zeta-client", body="Typed client of a node: remote.rs (HTTP), ws.rs (streams), tools.rs and guide.rs (tool list and guide text shared by the CLI, MCP and agent). Local transport: a one-shot CLI calls Node::apply in-process; serve.lock keeps mutating one-shots off a served state dir.")
peers = dict(title="Other ZetaSDR nodes", note=7, body="Federation peers: a node is a source for another (--source-node), forwards speech (--stt-node, X-Zeta-Stt-Hop loop guard), and is driven with --node. One secret per node.")

core_a = [
 dict(title="Operation set", note=1, body="ops.rs: 36 typed ops (source_*, job_* with job_retune, jobs_stop, plan_start, mix_start, stream_*, graph_get/put/start, bits_start, iq_read, classify, knowledge_lookup, channel_plans, modes_*, themes_list, station_set/get/clear, capture_list/show/classify/delete, schema). JSON schema; every field required; corrective errors (field, range, suggested, conflict holders). CLI form and JSON form are the same op."),
 dict(title="Job runtime", note=(6, 10, 17), body="job.rs, node/launch.rs, retune.rs, persist.rs: a job is a graph on a source band; persisted, resumed by serve (scan first, then widest first), run generation so a stopped run cannot outlive its job, history of 20. job_retune relaunches a running job under its id on a new band, squelch, deviation and sideband carried. Attached jobs (transcribe) read another job's stream, hold no lease and follow their input through a retune or graph_put."),
 dict(title="Lease manager", note=2, body="lease.rs: one tuning per source. Rigid leases (decoders, identify, watch, scan), elastic followers (the page's spectrum), pins (a holder that is not a job). Per-source gate; replace stops named holders and tunes in one step; job_retune moves a lease under the gate; conflicts name the holders and the options."),
 dict(title="Graph document + stage registry", body="graph.rs, stage.rs: nodes (kind, params) and typed edges; 31 stage kinds with params_schema (burst_capture and the layer-4 chain among them); a job kind expands to a built-in graph (channel → demod → decoder); graph_get/put restarts under the same id; graph_start runs a graph with no radio (capture_in, stream_in or mix at its root) on the node source; re-expandable puts carry squelch, guard and width across capture changes."),
]
core_b = [
 dict(title="IQ ring", body="ring.rs: per-source IQ history under --ring-budget-mb (10 s narrow, down to 1 s at 16 Msps). iq_read saves a SigMF pair of the whole ring band; classify reads a span; older spans fail too_old."),
 dict(title="Channel bank", note=3, body="bank.rs over zeta-dsp PfbBank: at 2 Msps and up one polyphase filter bank per source splits the capture into 8-256 bins; a channel that fits a bin is bank-served, wider or odd-rate ones take the direct path. job_context shows channel.path; source_list shows bank.dropped."),
 dict(title="Streams and ports", body="stream.rs, port.rs, types.rs: typed streams (iq, audio, video, text, event, sink), frames with provenance (source, job, channel, start_ns, end_ns), the node's own ops stream, lagged/missed accounting, stream_tail. Port trait: a source's tune + push side."),
 dict(title="Capture and sources", note=(8, 9), body="capture.rs, node/sources.rs, recover.rs: source_ensure, pin/unpin, free, set_gain; capability records; faults (device lost, worker died, too_busy, remote gone); a healed source gets its gain, pin and failed jobs back. scan steps a span, odd sweeps a quarter step lower, each step's DC notch and edges masked on local hardware."),
]
core_c = [
 dict(title="Mode registry", note=4, body="modes/: rows from modes/*.json (aprs, pocsag, rds, acars, adsb, rs41, dfm, m20) then the operator's --modes-dir; a row names its band, modulation, decoder (graph or program), event schema, summary template, identify rule. States ready, broken, unavailable. zeta modes check runs each row's synthesised test."),
 dict(title="Knowledge", note=14, body="knowledge.rs: providers answer knowledge_lookup for a band: built-in seed signatures (e.g. the 404.025 MHz Unitywater link) the iNTERCEPT catalogue (594 signals, Apache-2.0) and, with --knowledge-acma, the ACMA licence register (acma provider over an MCP client: licensee, site, distance, power, decoded emission, reach per band class and station antenna); a signature's decoder names the mode row that proves it. station.json holds the node's position, region and antennas."),
 dict(title="Classify and bits", note=(5, 11), body="classify.rs: layer 1 labels the measured band (wfm, fm, am, ssb, fsk, carrier, noise, unknown); layer 2 scores every mode row, tries the best 5 as decode-to-identify (CRC/BCH pass = proof) and adds up to three catalogue names; layer 3 is an empty slot. Unknown stays unknown. bits_start builds the layer-4 chain from a capture's feature record and refuses rather than guesses a rate."),
 dict(title="Identify and watch", note=5, body="node/identify.rs, watch.rs: identify holds the band, measures 1 s, runs each fitting row as a child job for its dwell, publishes candidate events, leaves the proved decoder running. watch runs fft → detect over a pinned band and identifies each new signal, keeping the decoder in the capture or on a free source; optional SondeHub sink."),
 dict(title="Plans, logs, guide, themes", body="themes.rs: the theme registry (built-in folders under themes/, then --themes-dir; ready or broken; a theme's style.css wrapped and scoped, escaping urls refused), themes_list. plans.rs: channel plans (uhf_cb_au, 80 channels) naming channels and feeding the adjacent-channel guard. node/jsonlog.rs: ops.log and transcripts.log (8 MiB rotation). agents_md.rs: AGENTS.md generated from the schema plus docs/agents/field-craft.md, served at /agents-md."),
]

dsp = dict(title="zeta-dsp: in-process stages", body="channel (fine filter, wide_hz for the guard), pfb bank, resample (rational, snapping), fft (CPU; GPU via wgpu behind feature gpu, shaders/; notch_hz/edge_mask mark rows masked), measure (layer 1: occupied band, SNR, burst gating with duty/bursts/burst_ms/period_ms, OOK chip rate from run lengths, cyclostationary symbol rate, FSK/PSK order and tone spacing, label incl. psk and ook; ±25 kHz DC mask beside a local capture centre), nbfm/wfm/am/ssb demod to 48 kHz audio, squelch (auto floor + 6 dB) and adjacent-channel guard, afsk + ax25 → aprs_decode, pocsag, rds, acars (MSK), detect (runs of unmasked bins over the row median with a run-energy floor that persist), mix (sums audio jobs on a 20 ms clock for listening), the layer-4 chain: capture_in (replays a capture set, paced, looping) → channel/resample → fsk/psk/ook demod → clock_recover (M&M, Gardner, early-late) → slicer (2/4 levels, BPSK/QPSK/8PSK, differential) → correlate (sync hits) → bit_view (run lengths, frame period, sync and CRC candidates; CRC table ccitt, ibm, dnp, crc32, x25, kermit), denoise (RNNoise via nnnoiseless), vad, catalogue scoring, synth/fixtures (fsk/psk/ook families, RS41 frames, echo, snr, ppm impairments for modes check).")
ext = dict(title="zeta-ext: external decoder programs", note=4, body="external_decode runs a program row's command over stdio with IQ or demodulated audio (raw or wav fifo) and parses json_lines, sbs, nmea, kiss, text or multimon into events; adsb_decode (dump1090), rs41/dfm/m20 rows (rs1729: rs41mod, dfm09mod, m20mod); handler report at startup (ZETA_HANDLER_*).")
plug = dict(title="zeta-plugin", body="Stdio plugin protocol 1 and 2 (hello, run, typed frames; 2 adds video). --plugin KIND=PATH registers a stage kind probed at startup; cleared environment. The Python SDK implements it.")
children = dict(title="Child processes (spawned by the node, cleared env, stdio)", tag="OUTSIDE PROCESS", body="dump1090 · rs41mod / dfm09mod / m20mod · the operator's --plugin programs (any language; sdk/python). A crashed program fails its job with a reason naming it; a missing one leaves its row unavailable.")
speech = dict(title="zeta-speech", note=6, body="transcribe stage; backends SocketBackend (local-voice protocol on a Unix socket) and NodeBackend (another node's POST /stt); 16 kHz WAV; 4 slots, 30 s busy drop; attached chain stream_in → denoise → vad → transcribe.")
feeds = dict(title="Feeds out", note=13, body="sink_sondehub (zeta-ext): batches positioned sonde events into a PUT every batch_ms; sent/failed/last_error in job_context. The only sink stage today.")
daemon = dict(title="Speech daemon", body="local-voice (or any daemon speaking its protocol) at $XDG_RUNTIME_DIR/voiced.sock: file:<wav> answered ok:<text>; languages answered ok:en,de,... Runs on the node's host (a Unix socket), on its GPU when it has one.")
sondehub = dict(title="SondeHub", body="api.v2.sondehub.org/sondes/telemetry: PUT every batch_ms with the operator's callsign and position. Nothing uploads without them.")

sources = [
 dict(title="zeta-source-file", body="SigMF recording as a source; with tune_min/max it plays as a tunable radio (test and demo aid). Fixtures for the test suite."),
 dict(title="zeta-source-soapy", note=12, body="SoapySDR (HackRF, B210) via zeta-source-soapy-driver (cfg soapy) in a zeta soapy-worker child (zeta-source-worker) the node respawns: a crash or unplug faults the source, never the node. gain=, ppm=, clock guard, stats."),
 dict(title="zeta-source-kiwi", body="KiwiSDR over websocket: 0-30 MHz, 12 kHz per connection, password from env; too_busy fault when the slots are full."),
 dict(title="zeta-source-node", note=7, body="Another ZetaSDR node's source: each tune holds a passthrough job there and reads ws ?format=binary; 250 kHz cap unless widened; faults when the remote goes away."),
 dict(title="zeta-source-vrt", body="VITA 49.2 packets on UDP; the sender's context packet sets frequency and rate; a job outside it fails unsupported."),
 dict(title="zeta-vrt", body="VRT packet codec (context, data) and the typed envelope for non-IQ frames; used by the ws binary stream and the vrt source."),
]
hosts = dict(title="Decoder and speech hosts", body="dump1090-fa, sonde-decoders (rs1729), a future mbelib build: installed by the operator from the handler report's hint, never fetched by zeta.")
radios = [
 dict(title="SigMF files on disk", body="captures/ from iq_read, bursts/ (the burst capture store with its index and expiry), scratch fixtures"),
 dict(title="HackRF One, USRP B210", body="libsoapysdr + driver modules; 2 m / 70 cm antenna, discone"),
 dict(title="KiwiSDR receivers", body="public or private HF receivers, site-dependent"),
 dict(title="Remote ZetaSDR node", body="a Pi 5 at the antenna (deploy/pi), or a radio on another host"),
 dict(title="VRT sender", body="any VITA 49.2 UDP streamer on the LAN"),
 dict(title="(codec only)", body="no device of its own"),
]
planned_a = [
 dict(planned=True, note=15, title="UI wave: the rest", body="Web UI only. Plan 24 shipped the console theme, the rail, riding ribbons, two-line rows and collapsible panes; still pencilled in: a lane budget from the pixel height between channels, mode picking by band through knowledge_lookup with a search fallback, state-stream decoders such as ADS-B as one held line per subject."),
 dict(planned=True, note=11, title="Plan 19.3: frames and signatures", body="zeta-dsp and knowledge. frame_decode (preamble, length, NRZI/Manchester over the CRC table), thin Modbus RTU / DNP3 / IEC-101/104 rows, operator-accepted signature write-back so the next encounter is a layer-2 match. Open from 19.2: a real OOK remote (433.882 MHz) not yet recovered end to end by the bits chain; the live 404.025 MHz acceptance."),
 dict(planned=True, note=13, title="Decoder wave 2 (rows)", body="An rtl_433 program row for ISM remotes and sensors (spec draft 46c9c46). Rows and small stages on the plan-17 frameworks: M17 (Codec 2) and DMR (AMBE via mbelib, tools/install-vocoders.sh) as audio rows feeding transcribe; AIS (nmea parser, map), FLEX; whole-span POCSAG/APRS as a watch with decode_on capture; stereo wfm_demod with pilot level; DFM/M20 synthesised tests; APRS positions on the map."),
 dict(planned=True, note=13, title="Feeds and sinks", body="sink stage kinds for the standard feeds (APRS-IS, SBS/Beast, AIS NMEA, PSK Reporter, WSPRnet, KISS TNC); a generic sink: {kind, params} on the watch's detect node replacing the one-off sondehub field (shorthand kept); fixes: a sink-bearing decoder frozen as an operator graph, gated_stop cleared by a sink restart."),
]
planned_b = [
 dict(planned=True, note=5, title="Watch and identify fixes", body="Sonde rows before ACARS on a 12.5 kHz FSK box; a tone-pair criterion and a per-device spur list for the detector (the B210 noise recording is pending the operator). Re-centring, the median-referenced detector and the row-width slice shipped in plan 19.1."),
 dict(planned=True, note=14, title="Time, trunking, span", body="True block timestamps (HackRF drift bug) to unlock UTC-slotted modes (FT8/FT4, WSPR) and TDMA (DMR, TETRA); a trunking follower (P25, DMR Tier III, MPT1327) starting voice jobs from control-channel events inside a pin; decoders that find their own channels in a capture; wideband modes (DVB-T, DAB) on the GPU FFT, parked behind the HackRF-first set."),
 dict(planned=True, note=11, title="Packaging, release, layer 3", body="Plan 25, the public release, is parked on its branch until its rulings are confirmed. --run-dir (ZETA_RUN_DIR) for program-row fifos and snap/flatpak/AppImage install hints; the vocoder install script and its legal note; layer 3 (a learned classifier) stays an empty slot until layer-4 sessions produce labelled captures; per-footprint theme layouts and a node-level default theme."),
]

# ---- measure ---------------------------------------------------------------
xs1, bw1 = cols(5, 16)
h1 = rowh(clients + [llm], bw1); h1 = max(h1, need_h(llm["title"], llm["body"], OW))
Y1 = 84; H1 = h1 + 2 * PAD

ws_w = (CX1 - CX0) * 0.74; cl_x = CX0 + ws_w + 16; cl_w = CX1 - cl_x
h2 = max(need_h(ws["title"], ws["body"], ws_w), need_h(client["title"], client["body"], cl_w), need_h(peers["title"], peers["body"], OW))
Y2 = Y1 + H1 + GAPY; H2 = h2 + 2 * PAD

xa, cw4 = cols(4, 16, CX0 + 12, CX1 - 12)
xc, cw5 = cols(5, 11, CX0 + 12, CX1 - 12)
ha = rowh(core_a, cw4); hb = rowh(core_b, cw4); hc = rowh(core_c, cw5)
Y3 = Y2 + H2 + 28
core_top = Y3 + PAD
ra_y = core_top + 30; rb_y = ra_y + ha + GAPY; rc_y = rb_y + hb + GAPY
core_h = (rc_y + hc + 12) - core_top
H3 = core_h + 2 * PAD

Y4 = Y3 + H3 + 28
s_y = Y4 + PAD
dsp_w = 430; ext_x = CX0 + dsp_w + 16; ext_w = 290; plug_x = ext_x + ext_w + 14; plug_w = 160
right_x = plug_x + plug_w + 16; right_w = CX1 - right_x
h_ext = max(need_h(ext["title"], ext["body"], ext_w), need_h(plug["title"], plug["body"], plug_w))
h_child = need_h(children["title"], children["body"], plug_x + plug_w - ext_x)
h_speech = max(need_h(speech["title"], speech["body"], right_w), need_h(daemon["title"], daemon["body"], OW))
h_feeds = max(need_h(feeds["title"], feeds["body"], right_w), need_h(sondehub["title"], sondehub["body"], OW))
h_dsp = need_h(dsp["title"], dsp["body"], dsp_w)
col_mid = h_ext + 16 + h_child
col_right = h_speech + GAPY + h_feeds
h4 = max(h_dsp, col_mid, col_right)
H4 = h4 + 2 * PAD

Y5 = Y4 + H4 + 28
xs5, sw = cols(6, 16)
h5 = max(rowh(sources, sw), need_h(hosts["title"], hosts["body"], OW)); s5y = Y5 + PAD; H5 = h5 + 2 * PAD
Y6 = Y5 + H5 + GAPY
h6 = rowh(radios, sw); r6y = Y6 + PAD; H6 = h6 + 2 * PAD
Y7 = Y6 + H6 + 18
xp, pw = cols(4, 14, CX0, OX1)
hp1 = rowh(planned_a, pw); hp2 = rowh(planned_b, pw)
p_y = Y7 + 30; p2_y = p_y + hp1 + 12
H7 = (p2_y + hp2 + PAD) - Y7

# ---- draw: bands first -----------------------------------------------------
band(Y1, H1, "CLIENTS"); band(Y2, H2, "TRANSPORTS"); band(Y3, H3, "CORE NODE"); band(Y4, H4, "STAGES")
band(Y5, H5, "SOURCES"); band(Y6, H6, "RADIOS, FILES"); band(Y7, H7, "PLANNED")
emit(f'<rect x="{OX0}" y="{Y1}" width="{OX1-OX0}" height="{Y5+H5-Y1}" rx="8" fill="none" stroke="currentColor" stroke-width="1" stroke-dasharray="2 4" opacity="0.6"/>')
label(OX0 + 10, Y1 - 6, "OUTSIDE THE NODE: services it talks to", size=10.5, bold=True, op=0.75)

# clients
draw_row(Y1 + PAD, clients, xs1, bw1, h1)
box(OX, Y1 + PAD, OW, h1, llm["title"], llm["body"])
for i, lab in enumerate(client_labels):
    x = xs1[i] + bw1 / 2
    arrow(x, Y1 + PAD + h1, x, Y2 + PAD, lab, lx=x + 6, ly=Y2 + 6, anchor="start")
ay = Y1 + PAD + 40
arrow(xs1[4] + bw1, ay, OX, ay, "chat + tool calls", lx=(xs1[4] + bw1 + OX) / 2, ly=ay - 6)

# transports
box(CX0, Y2 + PAD, ws_w, h2, ws["title"], ws["body"], note=ws["note"])
box(cl_x, Y2 + PAD, cl_w, h2, client["title"], client["body"])
box(OX, Y2 + PAD, OW, h2, peers["title"], peers["body"], note=peers["note"])
ay = Y2 + PAD + 40
arrow(CX1, ay, OX, ay, "HTTP + ws, both ways", lx=(CX1 + OX) / 2, ly=ay - 6, both=True)

# core
emit(f'<rect x="{CX0}" y="{core_top}" width="{CX1-CX0}" height="{core_h}" rx="8" fill="none" stroke="currentColor" stroke-width="1.6"/>')
emit(f'<text x="{CX0+10}" y="{core_top+19}" font-size="13" font-weight="800">zeta-core: the node (one process, one state dir: jobs, captures, pins, gain, ops.log, transcripts.log)</text>')
label(CX1 - 10, core_top + 19, "every transport ends in Node::apply(op)", size=10.5, anchor="end")
mid = CX0 + 450
arrow(mid, Y2 + PAD + h2, mid, core_top, "Node::apply(op); results back; frames out on streams", lx=mid + 8, ly=Y3 + 4, anchor="start")
draw_row(ra_y, core_a, xa, cw4, ha); draw_row(rb_y, core_b, xa, cw4, hb); draw_row(rc_y, core_c, xc, cw5, hc)

# stages
arrow(mid, core_top + core_h, mid, s_y, "a job launches its graph; stages read and write typed streams", lx=mid + 8, ly=Y4 + 4, anchor="start")
box(CX0, s_y, dsp_w, h4, dsp["title"], dsp["body"])
box(ext_x, s_y, ext_w, h_ext, ext["title"], ext["body"], note=ext["note"])
box(plug_x, s_y, plug_w, h_ext, plug["title"], plug["body"])
box(ext_x, s_y + h_ext + 16, plug_x + plug_w - ext_x, h_child, children["title"], children["body"], tag=children["tag"])
arrow(ext_x + ext_w / 2, s_y + h_ext, ext_x + ext_w / 2, s_y + h_ext + 16, None)
arrow(plug_x + plug_w / 2, s_y + h_ext, plug_x + plug_w / 2, s_y + h_ext + 16, None)
box(right_x, s_y, right_w, h_speech, speech["title"], speech["body"], note=speech["note"])
box(OX, s_y, OW, h_speech, daemon["title"], daemon["body"])
ay = s_y + 40
arrow(CX1, ay, OX, ay, "wav in, text out", lx=(CX1 + OX) / 2, ly=ay - 6)
f_y = s_y + h_speech + GAPY
box(right_x, f_y, right_w, h_feeds, feeds["title"], feeds["body"], note=feeds["note"])
box(OX, f_y, OW, h_feeds, sondehub["title"], sondehub["body"])
ay = f_y + 40
arrow(CX1, ay, OX, ay, "HTTPS PUT", lx=(CX1 + OX) / 2, ly=ay - 6)

# sources
arrow(mid, s5y, mid, s_y + h4, "IQ blocks into the ring and the bank (Port push side); tune and lease the other way", lx=mid + 8, ly=Y5 + 4, anchor="start")
draw_row(s5y, sources, xs5, sw, h5)
box(OX, s5y, OW, h5, hosts["title"], hosts["body"])
draw_row(r6y, radios, xs5, sw, h6)
for i in range(5):
    x = xs5[i] + sw / 2
    arrow(x, r6y, x, s5y + h5, None)

# planned
emit(f'<text x="{CX0}" y="{Y7+18}" font-size="12" font-weight="800" font-style="italic" fill="{AMBER}">Pencilled in: where each planned piece lands (plan numbers from docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md, gists from TODO.md)</text>')
draw_row(p_y, planned_a, xp, pw, hp1); draw_row(p2_y, planned_b, xp, pw, hp2)

p2_h = hp2
H = p2_y + p2_h + 20

svg = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-label="ZetaSDR architecture: clients over transports over one core node that runs jobs as graphs of stages fed by sources, with external services beside it and planned work pencilled in." font-family="{FONT}" fill="currentColor" style="color:#1b1f24">',
       '<defs>',
       '<marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="currentColor"/></marker>',
       '<marker id="arrow-start" viewBox="0 0 10 10" refX="1" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 10 0 L 0 5 L 10 10 z" fill="currentColor"/></marker>',
       '</defs>',
       f'<rect x="0" y="0" width="{W}" height="{H}" fill="#fbfbf9"/>'] + parts + ['</svg>']
svg_text = "\n".join(svg)
(HERE / "zetasdr-architecture.svg").write_text(svg_text)

# --- the page ----------------------------------------------------------------
PLANNED_NOTES = {13, 14, 15}
NOTES = [
 (1, "One operation set, every transport", "The CLI, the JSON over HTTP, the MCP tools and the in-process agent all end in Node::apply with the same 36 ops; the CLI form and the JSON form are the same call. Every applied op is appended to ops.log with its caller's transport and published on the node's own ops stream, so the page's transcript pane shows what the agent did."),
 (2, "Leases, not locks", "A source holds one tuning. Decoders, identify, watch and scan hold rigid leases; the page's spectrum job is elastic and follows every retune; a pin is a rigid holder with no job. A request that does not fit is a conflict that names the holders; replace stops the named holders and tunes under the source's gate in one step. job_retune moves a running job's lease under the same gate: inside the capture it is granted without a tune; outside it the source is tuned, or the request is a conflict and the old lease is put back."),
 (3, "A wide capture is shared, not copied", "At 2 Msps and up one polyphase filter bank per source splits the capture into coarse bins; a channel that fits a bin reads that bin, so 80 UHF CB channels on one 1 MHz pin cost 80 fine filters at a few hundred kHz each instead of 80 full-rate filters. A channel wider than a bin, or on a slow source, takes the direct path with the same graph and provenance."),
 (4, "A decoder is a row, not code", "Plan 16 made modes table rows: modes/*.json built in, the operator's --modes-dir after. A graph row reuses in-process stages (APRS, POCSAG, RDS, ACARS); a program row runs an external decoder through external_decode over stdio (dump1090, rs1729) with IQ or demodulated audio and a declared parser. Each row carries its identify rule, summary template and a synthesised test for zeta modes check."),
 (5, "The identification ladder", "measure is layer 1 (occupied band, SNR, burst, symbol-rate line, a coarse label). classify is the quick layer 2: it scores every row against the layer-1 record, tries the best five as decode-to-identify (a CRC-clean frame is the proof) and names up to three catalogue signals. identify is the thorough form: a rigid lease, then each fitting row run as a child job for its dwell. watch automates identify over a band. Layer 3, a learned classifier, is an empty slot by design. Nothing matching is reported as unknown with the measured features, never guessed."),
 (6, "Attached jobs", "transcribe is never started on the radio: it attaches to a running demodulator, reads that job's audio stream and holds no lease. It follows its input through a job_retune or a graph_put restart, relaunched under its own id on the new stream, and ends when the input stops or fails for its own reason. Its chain is stream_in, denoise (RNNoise), vad, transcribe; the speech work happens in an external daemon or on another node's /stt."),
 (7, "Federation", "A node is a source for another node: each tune holds a passthrough job on the remote, whose source list shows who is using the radio, and IQ ships as VRT over the websocket's binary mode, narrowband by default. The same HTTP API forwards speech (--stt-node) and is what --node drives. It is not crash protection: one node owns a local HackRF directly, its driver in a worker child (note 12)."),
 (8, "Health and faults", "A source's fault (device lost, Kiwi too_busy, remote gone) fails its jobs with the fault as the reason and is cleared by the next successful tune. Soapy sources report measured rate, overflows and clock-guard restarts; the bank reports dropped blocks; /health reports live stage tasks so a leak is visible."),
 (9, "Plan 15: scan grid and masks", "The scan alternates its step grid between sweeps: the odd sweep sits a quarter step lower with at most one more step, so each grid's DC notch and edges fall a quarter of the way into a row of the other, clear of its mask (a half step put them inside the other grid's edge mask). On a local hardware receiver the scan's one fft node masks a 300 kHz centre notch and 0.10 of the bins at each edge; every other source gets none. Rows carry masked: [[lo, hi], ...], the row's floor ignores masked bins, detect skips them and the page paints nothing under them. job_context.scan reports alternate and mask; graph put on the fft node changes the mask. measure on a local receiver ignores the 25 kHz either side of the capture centre when the box holds the DC but is not centred on it. Live acceptance on the desktop HackRF is pending."),
 (10, "Plan 18: fit first, drag to refine", "A demodulator started from a box is fitted first: a measure of the box, then the measured centre with 1.25 times the occupied width, floored per kind and never wider than the box; an SSB demod moves its dial only and keeps its band. That is the operator's standing rule: no channel-grid assumptions. A running job's outline is dragged by its edges or its body and retuned on release through job_retune, the 23rd op: the lease moves under the source's gate and the job is relaunched under its id, squelch, deviation and sideband carried and the channel-plan guard derived again. The outline menu offers Fit to signal, the 2.5 to 25 kHz widths and Stop; job tags on the waterfall light up with their rows. Live acceptance on the desktop HackRF is pending."),
 (11, "Plan 19: layer 4", "19.1 (merged 2026-11-03) made layer 1 burst-aware, added the node's station, the ACMA register as a knowledge provider and the burst capture store with the Explore pane. 19.2 (merged 2026-11-04) added the vetted blocks an agent chains from a capture to bits: capture_in, resample, fsk/psk/ook demods, clock_recover, slicer, correlate and bit_view, started in one call by bits_start from the capture's features (it refuses rather than guesses a rate), or as any graph with no radio by graph_start; the Bits pane shows the hypotheses. The OOK run-length fit followed on the first real remote. 19.3 (planned) adds a framing decoder with standards-based serial parsers for clear traffic only and signature write-back; the labelled captures these sessions leave are the training data layer 3 would need."),
 (20, "Plan 26: themes as folders", "A theme is a folder (theme.json, an optional style.css, its fonts and images), built in under themes/ or the operator's --themes-dir, read like mode rows: ready or broken, a broken one never stopping the node and never displacing the built-in it meant to replace. The node wraps style.css (tokens and layout first, scoped by nesting, font faces hoisted, imports and escaping urls refused) and serves a ready theme's files under /ui/themes/<id>/ without the bearer; themes_list is the 36th op. Built in: console (the default), plain, cyberpunk, terminal, psytrance and vibecoder, the light one. Merged and deployed as plan26-zeta 2026-11-04; docs/themes.md holds the format."),
 (12, "Plan 20: Soapy worker", "A driver crash ends the process that loads it, so the driver runs in a zeta soapy-worker child the node spawns (crate zeta-source-worker). A crash or a pulled cable faults the source and fails its jobs with the fault; the node lives on, respawns the worker and, when the device is back, restores the gain and the pin and relaunches the jobs that failed with that fault under their own ids (zeta: source <id>: recovered: ...). source list shows worker {pid, restarts, since_ns}; --soapy-in-process opens the device in the node, for diagnosis only. Deployed on test_host 2026-10-31; acceptance items 1 and 2 passed."),
 (16, "Plan 21: requests too big for one call", "A local model makes one tool call per turn, so 160 operations never fit its budget. plan_start starts a kind on every channel of a plan, with an optional attach, under one lease decision: a clash names the holders and starts nothing, and a second run adds only what is missing. jobs_stop stops everything matching a source, kind or plan in one call. The agent's executor path refuses job_stop, jobs_stop and a non-empty replace unless the run carries an allowance (not_allowed goes back to the model; the words are the operator's: allow stops on, or a one-off button), because the guide's words alone did not hold. job_start refuses an identical running rigid job under the source's gate. Stage::readiness lets a stage such as transcribe say it cannot run, read by job_attach, plan_start and schema."),
 (17, "Plan 22: the mix", "With 80 channels running, a transmission is over before anyone finds its row. mix_start runs one job over any set of tuners: a stage on a 20 ms clock asks the node twice a second which jobs have audio, sums what arrives (a soft limit above 0.8; a closed squelch publishes nothing, so silence stays silent) and publishes who is talking as active events, which light rows and outlines through one socket. It holds no lease: its record's source is the placeholder node. A mute is a channel (source and centre within 1 kHz) in the mix's graph, so it outlives the job but not the mix. Transcription stays per demodulator; a transcribe on the mix output is the operator's option."),
 (18, "Plan 23: subjects", "A decoder's events are about things: a mode row names the event field identifying its subject (icao, a sonde's id, an APRS src), the table columns and units, and how the waterfall shows it (one line per job, or one ribbon per subject updated in place). The page merges each subject's events into one state: a Subjects tab beside the map (metres and km/h, climb in m/s, distance from home, dimmed after 60 s and dropped after 5 min), a marker turned to its heading with a track that keeps a point only when the subject has moved and thins its older half at the cap. No per-protocol code on the page. The agent may stop the jobs it started in the same run, and reads channel_plans before naming a plan."),
 (13, "Decoder wave 2 and feeds (planned)", "Voice rows (M17, DMR) feed transcribe unchanged; AIS and FLEX are parser rows; whole-span POCSAG/APRS reuses the watch; stereo lands on wfm_demod. Feeds out generalise sink_sondehub to a sink: {kind, params} on the watch and add the standard sinks (APRS-IS, SBS, AIS NMEA, PSK Reporter, WSPRnet, KISS)."),
 (14, "Knowledge and time (planned)", "Plan 19.1 added the knowledge provider over the ACMA licence register (it names the 404.025 MHz burst, Unitywater 16K0F1D, in seconds) and the node-level station it needs; a GPS fix for the station is a gist. True block timestamps unlock FT8/WSPR and TDMA; a trunking follower and self-channelising span decoders are the other open architecture notes."),
 (15, "The rest of the UI wave (planned)", "Plan 18 shipped the fit and the drag; plan 24 (deployed 2026-11-03) shipped the console theme behind view set theme: the rail of job tags beside the frequency axis, ribbons that ride their band and step up a lane, two-line job rows, idle plan channels folded into one group, collapsible chat and jobs panes (view set side). Still pencilled in: a lane budget from the pixel height available per band, mode picking by band through knowledge_lookup with a search fallback, and a held roster line per subject for state-stream decoders such as ADS-B. TODO.md holds the gists."),
]

notes_html = "\n".join(
    f'<li value="{n}"{" class=\"planned\"" if n in PLANNED_NOTES else ""}><b>{esc(t)}.</b> {esc(b)}</li>' for n, t, b in sorted(NOTES))

page = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ZetaSDR architecture</title>
<style>
:root {{ color-scheme: light dark; --fg: #1b1f24; --bg: #fbfbf9; --muted: #5a6270; --amber: {AMBER}; }}
@media (prefers-color-scheme: dark) {{ :root {{ --fg: #e6e8ec; --bg: #14171c; --muted: #9aa3b2; }} }}
html, body {{ margin: 0; background: var(--bg); color: var(--fg); font-family: {FONT}; line-height: 1.45; }}
main {{ max-width: 1680px; margin: 0 auto; padding: 20px 20px 48px; }}
h1 {{ font-size: 22px; margin: 0 0 4px; }}
p.lede {{ margin: 0 0 14px; color: var(--muted); font-size: 14px; }}
figure {{ margin: 0; }}
figure svg {{ max-width: 100%; height: auto; display: block; border: 1px solid color-mix(in srgb, var(--fg) 15%, transparent); border-radius: 8px; color: #1b1f24; }}
figcaption {{ font-size: 13px; color: var(--muted); margin: 8px 0 0; }}
details {{ margin: 6px 0 14px; font-size: 13px; color: var(--muted); }}
h2 {{ font-size: 16px; margin: 26px 0 8px; }}
ol.notes {{ padding-left: 26px; font-size: 14px; max-width: 1100px; }}
ol.notes li {{ margin: 0 0 10px; }}
ol.notes li.planned b {{ color: var(--amber); }}
code {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.92em; }}
.row {{ display: flex; gap: 14px; flex-wrap: wrap; align-items: center; font-size: 13px; color: var(--muted); margin: 0 0 10px; }}
.sw {{ display: inline-block; width: 22px; height: 11px; border-radius: 3px; vertical-align: middle; margin-right: 6px; border: 1.3px solid var(--fg); background: color-mix(in srgb, var(--fg) 7%, transparent); }}
.sw.planned {{ border: 1.6px dashed var(--amber); background: transparent; }}
</style>
</head>
<body>
<main>
<h1>ZetaSDR architecture</h1>
<p class="lede">As it stands on {AS_OF}, with the planned work pencilled in. One Rust workspace of 18 crates, plus the Soapy driver crate built on its own; the diagram groups them by the layer they belong to. For the short version, how the radios, the LLM, the ACMA register and the speech daemon plug in and which of them may live on another host, see the <a href="mudmap.html">mudmap</a>. Generated by <code>docs/architecture/gen.py</code>; the same picture is in <code>docs/architecture/zetasdr-architecture.svg</code>.</p>
<div class="row"><span><span class="sw"></span>{SHIPPED}</span><span><span class="sw planned"></span>{PLANNED}</span><span>numbered discs point at the notes below</span></div>
<figure>
{svg_text.replace('style="color:#1b1f24"', '')}
<figcaption>Clients call one operation set over one transport layer; the core node turns operations into leased jobs that run graphs of stages over IQ pushed in by the sources; services outside the process sit to the right. Amber dashed boxes are not built yet.</figcaption>
</figure>

<h2>Notes</h2>
<ol class="notes">
{notes_html}
</ol>

<h2>Reading it</h2>
<ol class="notes">
<li value="1"><b>A request top to bottom.</b> <code>zeta job start --kind rs41_decode ...</code> becomes a <code>job_start</code> op (CLI or JSON, same thing), crosses the transport, and in the node asks the lease manager for the band on the source. Granted, the job runtime expands the row's graph (<code>channel → nbfm_demod → external_decode</code>), the channel stage pulls IQ from the shared bank or the direct path, the external stage spawns <code>rs41mod</code> and parses its lines into events that carry <code>position</code> and <code>t_ns</code>, and those events go out as frames on a stream the page, the CLI tail, the agent and the SondeHub sink all read the same way.</li>
<li value="2"><b>What is deliberately absent.</b> No transmit path anywhere. No decoder guesses: a protocol is labelled only on a CRC-clean frame, and <em>unknown</em> is a first-class answer with measured features. No uploads without an operator's callsign and position. Nothing is fetched or built by the node: handlers, vocoders and speech models are the operator's to install.</li>
<li value="3"><b>Where the amber lands.</b> Most planned work is rows and stages, not new machinery: plans 16 and 17 were built so that the second decoder wave is table entries. The pieces that still change the architecture are the framing decoder and signature write-back (plan 19.3), the generic sink on the watch and true block timestamps; the Soapy worker (plan 20), the layer-4 bit chain (plan 19.2) and themes as folders (plan 26) have landed.</li>
</ol>
</main>
</body>
</html>
"""
(HERE / "index.html").write_text(page)
print("wrote", HERE / "zetasdr-architecture.svg", "and", HERE / "index.html", "height", H)
