#!/usr/bin/env python3
"""Generate the ZetaSDR mudmap: the simplified wiring picture (SVG and a page around it).

Where gen.py draws every crate, this draws what an operator plugs in: the clients, the
node's host and what must sit on it, the services the node calls and the radios it reads,
each marked LOCAL (on the node's host), REMOTE (another host) or EITHER, each wire labelled
with its protocol and the flag that sets it up.

Edit the data below, run `python3 docs/architecture/mudmap.py`, commit the outputs.
"""
from __future__ import annotations
import html, pathlib, sys

HERE = pathlib.Path(__file__).parent
AS_OF = "2026-11-04 (plan26-zeta)"
FONT = "Inter, 'Segoe UI', Helvetica, Arial, sans-serif"
W, H = 1440, 700
WHERE = {
    "LOCAL": "#0f766e",   # must run on the node's host
    "REMOTE": "#1d4ed8",  # runs on another host
    "EITHER": "#7c3aed",  # the same host or another, by URL
}

def wrap(text: str, width_px: float, size: float) -> list[str]:
    maxc = max(8, int((width_px - 16) / (size * 0.53)))
    out: list[str] = []
    for para in text.split("\n"):
        line = ""
        for w in para.split():
            cand = (line + " " + w).strip()
            if len(cand) > maxc and line:
                out.append(line); line = w
            else:
                line = cand
        out.append(line)
    return out

def esc(s: str) -> str:
    return html.escape(s, quote=False)

parts: list[str] = []
def emit(s: str): parts.append(s)

def chip(x, y, where):
    w = len(where) * 6.6 + 12
    emit(f'<rect x="{x - w}" y="{y}" width="{w}" height="15" rx="7.5" fill="{WHERE[where]}"/>')
    emit(f'<text x="{x - w / 2}" y="{y + 11}" font-size="9" font-weight="700" letter-spacing="0.06em" text-anchor="middle" fill="#ffffff">{where}</text>')

def box(x, y, w, h, title, body, where=None, size=10.5, strong=False):
    emit(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="7" fill="currentColor" fill-opacity="{0.10 if strong else 0.05}" stroke="currentColor" stroke-width="{1.8 if strong else 1.2}"/>')
    emit(f'<text x="{x + 9}" y="{y + 18}" font-size="{14 if strong else 12.5}" font-weight="700">{esc(title)}</text>')
    if where:
        chip(x + w - 7, y + 6, where)
    lines = wrap(body, w, size)
    need = 18 + 16 + len(lines) * (size + 2.5)
    if need > h + 2:
        print(f"warning: '{title}' needs {need:.0f} px, has {h}", file=sys.stderr)
    emit(f'<text x="{x + 9}" y="{y + 35}" font-size="{size}">')
    for i, ln in enumerate(lines):
        emit(f'<tspan x="{x + 9}" dy="{0 if i == 0 else size + 2.5}">{esc(ln)}</tspan>')
    emit('</text>')

def text(x, y, s, size=10, anchor="start", bold=False, op=0.85, italic=False):
    emit(f'<text x="{x}" y="{y}" font-size="{size}" text-anchor="{anchor}" opacity="{op}"'
         f'{" font-weight=\"700\"" if bold else ""}{" font-style=\"italic\"" if italic else ""}>{esc(s)}</text>')

def line(points, both=False, dash=False, end=True):
    d = "M " + " L ".join(f"{x} {y}" for x, y in points)
    emit(f'<path d="{d}" fill="none" stroke="currentColor" stroke-width="1.4"'
         f'{" stroke-dasharray=\"5 4\"" if dash else ""}'
         f'{" marker-end=\"url(#arrow)\"" if end else ""}{" marker-start=\"url(#arrow-start)\"" if both else ""}/>')

# --- header -----------------------------------------------------------------
text(18, 32, "ZetaSDR mudmap", size=22, bold=True, op=1)
text(222, 32, f"what plugs into a node, and where each piece may run; as of {AS_OF}", size=12, op=0.8)
lx = 18
for where, what in (("LOCAL", "on the node's host only"), ("REMOTE", "on another host"), ("EITHER", "same host or another, by URL")):
    chip(lx + len(where) * 6.6 + 12, 46, where)
    text(lx + len(where) * 6.6 + 18, 57, what, size=10.5)
    lx += len(where) * 6.6 + 30 + len(what) * 5.8
emit(f'<line x1="{lx + 10}" y1="53" x2="{lx + 50}" y2="53" stroke="currentColor" stroke-width="1.4" marker-end="url(#arrow)"/>')
text(lx + 56, 57, "who calls whom: the label is the wire, then the flag that sets it up", size=10.5)

# zones
ZY = 96
text(20, ZY, "CLIENTS: anywhere that reaches the node", size=10.5, bold=True, op=0.7)
text(380, ZY, "THE NODE'S HOST", size=10.5, bold=True, op=0.7)
text(1110, ZY, "OUTSIDE SERVICES AND REMOTE RADIOS", size=10.5, bold=True, op=0.7)

# --- clients ----------------------------------------------------------------
CL_X, CL_W = 20, 300
clients = [
    (130, 88, "Browser: the page", "http://NODE:PORT/ui/ with the secret: waterfall, jobs, map, the agent pane (POST /ask), the mic (POST /stt).", "EITHER"),
    (232, 112, "zeta CLI", "One-shot: opens the node in-process on its --state-dir (only when no serve owns it). --node URL (ZETA_NODE, ZETA_SECRET): the op runs on that node. Local-only verbs: ops tail, transcripts tail, modes check, soapy find.", "EITHER"),
    (358, 88, "zeta-mcp", "stdio MCP server for Claude Code or any MCP client, one tool per op; runs beside that client and calls the node with --node URL.", "EITHER"),
    (460, 100, "zeta ask \"REQUEST\"", "A tool-calling loop: talks to the LLM itself (--llm-url, --llm-model) and applies ops in-process or on --node URL. Stops and replace refused unless --allow-*.", "EITHER"),
    (574, 70, "Scripts, Python SDK", "examples/python (cb_watch.py, gain.py) over HTTP; sdk/python for plugins (run as children, below).", "EITHER"),
]
for y, h, t, b, w in clients:
    box(CL_X, y, CL_W, h, t, b, w)
BUS_X = 350
line([(BUS_X, 174), (BUS_X, 609)], end=False)
for y, h, *_ in clients:
    line([(CL_X + CL_W, y + h / 2), (BUS_X, y + h / 2)], end=False)
line([(BUS_X, 230), (400, 230)])
emit(f'<text x="{BUS_X - 6}" y="660" font-size="10" opacity="0.85" text-anchor="middle">HTTP + ws, bearer secret</text>')
emit(f'<text x="{BUS_X - 6}" y="673" font-size="10" opacity="0.85" text-anchor="middle">(or in-process: one-shot CLI, zeta ask)</text>')

# --- the node's host ---------------------------------------------------------
HX0, HX1, HY0, HY1 = 380, 1010, 124, 604
emit(f'<rect x="{HX0}" y="{HY0}" width="{HX1 - HX0}" height="{HY1 - HY0}" rx="10" fill="none" stroke="currentColor" stroke-width="1.2" stroke-dasharray="3 4" opacity="0.7"/>')
NX0, NX1, NY0, NY1 = 400, 990, 140, 296
box(NX0, NY0, NX1 - NX0, NY1 - NY0, "zeta serve: the node",
    "One process, one state dir (jobs, pins, gain, captures, station, ops.log, transcripts.log). Every client ends in the same 36 ops; jobs are graphs of stages on a leased band of a source. "
    "--listen ADDR:PORT with ZETA_SECRET (127.0.0.1 by default; a LAN address to reach it from elsewhere). Sources and plugins are flags, repeated on every start. "
    "It is also a server to other nodes: its sources become their --source-node and its /stt their --stt-node.", "LOCAL", size=11, strong=True)

kids = [
    (400, "zeta soapy-worker", "One child per --source-soapy ID=ARGS, spawned and respawned by the node; a driver crash faults the source, not the node.", "LOCAL"),
    (598, "Programs, plugins", "dump1090, rs41mod / dfm09mod / m20mod (handler report at startup); --plugin KIND=PATH (any language, sdk/python). stdio, cleared env.", "LOCAL"),
    (796, "Speech daemon", "local-voice (voiced) on the GPU: --stt-socket PATH (a Unix socket, so this host). Or --stt-node URL: another node's POST /stt does it.", "EITHER"),
]
KY, KH, KW = 346, 116, 194
for x, t, b, w in kids:
    box(x, KY, KW, KH, t, b, w)
for x, lab in ((400, "spawns; IQ back"), (598, "spawns; stdio"), (796, "wav in, text out")):
    line([(x + KW / 2, NY1), (x + KW / 2, KY)])
    text(x + KW / 2 + 6, NY1 + 32, lab, size=10)
RY, RH = 504, 84
box(400, RY, KW, RH, "HackRF, USRP B210", "USB on this host; libsoapysdr + driver modules; --cfg soapy build.", "LOCAL")
line([(400 + KW / 2, KY + KH), (400 + KW / 2, RY)])
text(400 + KW / 2 + 6, KY + KH + 26, "USB", size=10)
box(598, RY, KW, RH, "SigMF files", "--source-file ID=PATH: a recording as a source (tests, demos); captures and bursts land in the state dir.", "LOCAL")
box(796, RY, KW, RH, "Handlers on PATH", "apt install dump1090-fa, sonde-decoders: the operator installs them; zeta never fetches.", "LOCAL")

# --- right column ---------------------------------------------------------------
RX, RW = 1110, 310
services = [
    (128, 92, "LLM server", "Any OpenAI-compatible chat API (llama.cpp, vLLM, a gateway). Called by serve for POST /ask (--llm-url, --llm-model on serve) and by zeta ask from wherever it runs.", "EITHER"),
    (236, 84, "ACMA licence register", "acma-rrl-server, an MCP server over HTTP. --knowledge-acma URL on the serving node; needs station set for distances.", "EITHER"),
    (336, 70, "SondeHub", "Internet. sink_sondehub PUTs sonde telemetry only once a callsign is given.", "REMOTE"),
]
for y, h, t, b, w in services:
    box(RX, y, RW, h, t, b, w)
line([(NX1, 174), (RX, 174)])
text((NX1 + RX) / 2, 168, "HTTP chat + tools", size=10, anchor="middle")
line([(NX1, 278), (RX, 278)])
text((NX1 + RX) / 2, 272, "MCP over HTTP", size=10, anchor="middle")
line([(NX1, 250), (RX, 360)])
text(1104, 376, "HTTPS PUT", size=10, anchor="end")
# zeta ask's own wire to the LLM, over the top
line([(CL_X, 510), (10, 510), (10, 112), (RX + RW / 2, 112), (RX + RW / 2, 128)], dash=True)
text(700, 108, "zeta ask: its own HTTP chat + tools to the LLM (--llm-url), wherever it runs", size=10, anchor="middle", italic=True)

text(RX, 436, "REMOTE RADIOS (IQ in, tune out)", size=10.5, bold=True, op=0.7)
radios = [
    (446, 70, "KiwiSDR", "--source-kiwi ID=HOST:PORT: websocket over the internet; 0-30 MHz, 12 kHz, limited slots.", "REMOTE"),
    (526, 80, "Another ZetaSDR node", "--source-node ID=SRC@URL (secret ZETA_NODE_SECRET_<ID>): HTTP + ws, IQ as VRT; 250 kHz unless widened. A Pi at the antenna.", "REMOTE"),
    (616, 62, "VRT sender", "--source-vrt ID=udp://ADDR:PORT: VITA 49.2 over UDP, a LAN address.", "REMOTE"),
]
for y, h, t, b, w in radios:
    box(RX, y, RW, h, t, b, w)
    line([(NX1, 290), (RX, y + h / 2)], both=True)
text(1420, 694, "To use a USB radio on another host, run a node there and attach it with --source-node.", size=10, anchor="end", italic=True, op=0.8)

svg = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" '
       f'aria-label="ZetaSDR mudmap: clients on the left reach one node over HTTP; on the node\'s host sit the Soapy worker and USB radios, decoder programs, plugins and the speech daemon; to the right the LLM, the ACMA register, SondeHub and the remote radios." '
       f'font-family="{FONT}" fill="currentColor" style="color:#1b1f24">',
       '<defs>',
       '<marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="currentColor"/></marker>',
       '<marker id="arrow-start" viewBox="0 0 10 10" refX="1" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 10 0 L 0 5 L 10 10 z" fill="currentColor"/></marker>',
       '</defs>',
       f'<rect x="0" y="0" width="{W}" height="{H}" fill="#fbfbf9"/>'] + parts + ['</svg>']
svg_text = "\n".join(svg)
(HERE / "mudmap.svg").write_text(svg_text)

# --- second figure: who runs the loop -------------------------------------------
# A configured LLM only proposes calls; zeta's own loop (zeta-agent) executes them behind
# the allowance gate. An external coding agent runs its own loop on its own model and
# reaches the node as any HTTP client does: no gate, only the guide's words.
parts.clear()
AW, AH = 1440, 664
text(18, 32, "How a model drives the node", size=22, bold=True, op=1)
text(330, 32, "who runs the loop (A: zeta, B: the agent), the doors in, what can refuse a call, and how the ops log records it", size=12, op=0.8)

def panel(x0, x1, title, sub):
    emit(f'<rect x="{x0}" y="58" width="{x1 - x0}" height="470" rx="10" fill="currentColor" fill-opacity="0.025"/>')
    text(x0 + 14, 80, title, size=13, bold=True, op=1)
    text(x0 + 14, 96, sub, size=10.5, op=0.75, italic=True)

L0, L1, R0, R1 = 20, 700, 740, 1420
panel(L0, L1, "A. A configured LLM: zeta runs the loop", "POST /ask (the page's agent pane) or zeta ask; the model only proposes calls")
panel(R0, R1, "B. An external coding agent (Claude Code): the agent runs the loop", "four doors: MCP, the CLI, raw HTTP, or handing the request to the node's own agent")

# A
box(40, 110, 640, 54, "Operator", "types a request in the page's agent pane, or runs zeta ask \"REQUEST\" (stdin works: a voice transcript piped in).")
box(40, 196, 350, 150, "zeta-agent: the tool loop",
    "Runs inside zeta serve for POST /ask, inside the zeta process for zeta ask. Builds the tool list and guide from the schema and AGENTS.md, sends them with the transcript, executes each call it gets back, appends the result, repeats up to max_turns (1-32, default 8). Results trimmed for a small model (job_list without graphs, the newest 20 captures).", "EITHER", size=10.2)
box(480, 196, 200, 150, "LLM server",
    "OpenAI-compatible (field-host:8080, Gemma 4 12B). Sees text only: never the node, the secret or the radios. Answers one tool call per turn, or the final prose.", "EITHER", size=10.2)
line([(390, 236), (480, 236)]); text(435, 229, "tools, transcript", size=9.5, anchor="middle")
line([(480, 300), (390, 300)]); text(435, 315, "one tool call", size=9.5, anchor="middle")
line([(215, 164), (215, 196)])
box(40, 376, 640, 96, "The allowance gate (in the loop, before the node)",
    "job_stop, jobs_stop, a non-empty replace and capture_delete come back to the model as not_allowed unless the run allows them (allow.stop_jobs / replace / delete_captures, zeta ask --allow-stop / --allow-replace / --allow-delete); jobs this run started itself may be stopped. Code enforces it, not the prompt.", size=10.2)
line([(215, 346), (215, 376)])

# B
box(760, 110, 640, 54, "Operator", "asks in a terminal or IDE session; approves the agent's tool calls by its own permission settings.")
box(760, 196, 350, 150, "Claude Code (or any MCP-capable agent)",
    "Its own loop on its own host (on the field host as claude, or the desktop). Reads AGENTS.md as the MCP server's instructions or the skills/zetasdr skill. Gets full results, chains as many calls as it likes, and can also edit code, build, deploy and read logs: things no node op can do.", "EITHER", size=10.2)
box(1200, 196, 200, 150, "Its model",
    "Anthropic's API, called by the agent. The node never talks to it and never sees its prompts.", "REMOTE", size=10.2)
line([(1110, 236), (1200, 236)]); text(1155, 229, "prompt", size=9.5, anchor="middle")
line([(1200, 300), (1110, 300)]); text(1155, 315, "tool calls", size=9.5, anchor="middle")
line([(935, 164), (935, 196)])
DY, DH, DW = 376, 134, 148
doors = [
    (760, "1. zeta-mcp", "stdio MCP server beside the agent: one tool per op from GET /schema, AGENTS.md as its instructions.", "POST /op: http"),
    (924, "2. zeta CLI", "zeta --node URL with ZETA_SECRET, or z.sh over ssh. Without --node a one-shot opens the node in-process, only while no serve owns the state dir.", "POST /op: http (one-shot: local)"),
    (1088, "3. Raw HTTP + ws", "curl, cb_watch.py, any language: POST /op, GET /schema, /agents-md, /streams/{id}/tail, ws /stream/{id}. The page is one such client.", "POST /op: http (ui by header)"),
    (1252, "4. POST /ask", "Hand the request to the node's own agent: path A from outside, its LLM, its turn budget and the gate (allow in the body).", "runs path A: agent"),
]
for x, t, b, _ in doors:
    box(x, DY, DW, DH, t, b, size=9.8)
line([(834, 346), (834, DY)]); line([(998, 346), (998, DY)])
line([(1100, 346), (1100, 361), (1326, 361), (1326, DY)]); line([(1162, 361), (1162, DY)])

# shared node
box(20, 556, 1400, 92, "zeta serve: Node::apply(op), the same 36 ops for every route",
    "A arrives in-process: POST /ask logs ops as transport agent; zeta ask logs local (one-shot) or http (with --node), not agent. "
    "B's doors 1-3 arrive over HTTP /op and log http (ui when the caller sends X-Zeta-Client: ui, a label it chooses, not a credential): the log cannot tell Claude Code from curl, nor which request caused a call. Door 4 runs path A inside the node, gate included. "
    "Over HTTP the bearer is the whole of the authority: no gate refuses a job_stop or replace sent through doors 1-3. The guide's rules (stop only what the request names or you started) bind B only as instructions it follows, and the operator's approval of its tool calls.", "LOCAL", size=10.5, strong=True)
line([(215, 472), (215, 556)]); text(223, 520, "in-process executor (HTTP with zeta ask --node)", size=10)
for x, *_, lab in doors:
    line([(x + DW / 2, DY + DH), (x + DW / 2, 556)])
    text(x + DW / 2 + 5, DY + DH + 26, lab, size=9.2)

asvg = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {AW} {AH}" width="{AW}" height="{AH}" role="img" '
        f'aria-label="How a model drives the node: a configured LLM proposes calls that zeta-agent executes behind an allowance gate; an external coding agent runs its own loop and reaches the node through zeta-mcp, the CLI or raw HTTP with no gate, or hands the request to the node\'s own agent through POST /ask." '
        f'font-family="{FONT}" fill="currentColor" style="color:#1b1f24">'] + svg[1:5] + \
       [f'<rect x="0" y="0" width="{AW}" height="{AH}" fill="#fbfbf9"/>'] + parts + ['</svg>']
agents_svg = "\n".join(asvg)
(HERE / "mudmap-agents.svg").write_text(agents_svg)

AGENT_TABLE = [
    ("Who runs the tool loop", "zeta-agent, in the node (<code>/ask</code>) or the <code>zeta ask</code> process", "the coding agent, on its own host"),
    ("Which model", "the one at <code>--llm-url</code> (any OpenAI-compatible server)", "the agent's own (Anthropic's API for Claude Code)"),
    ("What the model sees", "the tool list, the guide and trimmed results; never the node or the secret", "full results, the repo, the shell: whatever the agent's permissions allow"),
    ("How calls reach the node", "in-process executor (HTTP when <code>zeta ask --node</code>)", "<code>zeta-mcp</code>, the CLI or raw HTTP/ws: <code>POST /op</code> with the bearer; or <code>POST /ask</code>, which hands the request to path A"),
    ("What can refuse a stop or replace", "the allowance gate in code: <code>not_allowed</code> unless the run allows it", "through <code>/op</code>, nothing in the node: the guide's rules and the operator's approval of tool calls; through <code>/ask</code>, the gate"),
    ("Budget", "<code>max_turns</code> 1-32 (default 8), one call per turn; bulk ops (<code>plan_start</code>, <code>jobs_stop</code>) exist for it", "the agent's own; it can make hundreds of calls"),
    ("ops log <code>transport</code>", "<code>agent</code> (<code>/ask</code>); <code>local</code> or <code>http</code> (<code>zeta ask</code>)", "<code>http</code>, the same as any script (<code>ui</code> if it sends <code>X-Zeta-Client: ui</code>; <code>local</code> for a one-shot CLI; <code>agent</code> via <code>/ask</code>)"),
    ("Beyond the node", "nothing: only the operations", "code, builds, deploys, logs, other hosts"),
]
agent_rows = "\n".join(f"<tr><td>{a}</td><td>{b}</td><td>{c}</td></tr>" for a, b, c in AGENT_TABLE)

# --- the page ------------------------------------------------------------------
# (piece, wire, how it is wired, where)
TABLE = [
    ("Browser page", "HTTP + ws to the node, bearer secret", "open <code>/ui/</code> on the node's <code>--listen</code> address", "EITHER"),
    ("zeta CLI", "in-process, or HTTP to <code>--node URL</code>", "one-shot on the node's <code>--state-dir</code>, else <code>ZETA_NODE</code> + <code>ZETA_SECRET</code>; <code>ops tail</code>, <code>transcripts tail</code>, <code>modes check</code> and <code>soapy find</code> are local only", "EITHER"),
    ("zeta-mcp", "stdio to the MCP client, HTTP to the node", "<code>claude mcp add zeta -e ZETA_SECRET=… -- zeta-mcp --node URL</code>", "EITHER"),
    ("zeta ask", "HTTP to the LLM; in-process or HTTP to the node", "<code>--llm-url</code>, <code>--llm-model</code>; <code>--node URL</code> to drive a served node", "EITHER"),
    ("Agent in the node (<code>POST /ask</code>)", "the node calls the LLM over HTTP", "<code>zeta serve --listen … --llm-url URL --llm-model NAME</code>", "EITHER"),
    ("LLM server", "OpenAI-compatible chat API", "a URL: localhost or another host", "EITHER"),
    ("ACMA register", "MCP over HTTP (the node is the MCP client)", "<code>--knowledge-acma URL[,reach_km=N]</code> on the serving node, plus <code>station set</code>", "EITHER"),
    ("Speech daemon", "Unix socket", "<code>--stt-socket PATH</code> (default <code>$XDG_RUNTIME_DIR/voiced.sock</code>)", "LOCAL"),
    ("Speech on another node", "HTTP <code>POST /stt</code>", "<code>--stt-node URL</code>, secret in <code>ZETA_STT_NODE_SECRET</code>", "REMOTE"),
    ("HackRF, B210 (SoapySDR)", "USB, through a <code>zeta soapy-worker</code> child", "<code>--source-soapy ID=driver=…,gain=N,ppm=N</code>; build with <code>--cfg soapy</code>", "LOCAL"),
    ("SigMF recording", "file", "<code>--source-file ID=PATH.sigmf-meta[,tune_min_hz=…,tune_max_hz=…]</code>", "LOCAL"),
    ("KiwiSDR", "websocket over the internet", "<code>--source-kiwi ID=HOST:PORT</code>, password in <code>ZETA_KIWI_PASSWORD_&lt;ID&gt;</code>", "REMOTE"),
    ("Another node's radio", "HTTP + ws, IQ as VRT", "<code>--source-node ID=SRC@URL[,max_bandwidth_hz=N]</code>, secret in <code>ZETA_NODE_SECRET_&lt;ID&gt;</code>", "REMOTE"),
    ("VRT sender", "VITA 49.2 over UDP", "<code>--source-vrt ID=udp://ADDR:PORT</code> (a LAN address)", "REMOTE"),
    ("Decoder programs", "stdio child, cleared env", "installed on <code>PATH</code> (or <code>ZETA_HANDLER_&lt;PROGRAM&gt;</code>); the startup report names what is missing", "LOCAL"),
    ("Plugins", "stdio child, cleared env", "<code>--plugin KIND=PATH</code>; <code>ZETA_PLUGIN_ENV_&lt;NAME&gt;</code> passes a variable", "LOCAL"),
    ("Operator modes, themes", "files", "<code>--modes-dir</code>, <code>--themes-dir</code>", "LOCAL"),
    ("SondeHub", "HTTPS PUT, outbound", "<code>sondehub: {callsign}</code> on a watch, or a <code>sink_sondehub</code> node", "REMOTE"),
]
STATION = [
    ("the field host, user <code>sdr</code>", "<code>zeta serve --listen 0.0.0.0:7979</code> as the user unit <code>zeta.service</code>, binary <code>~/zetasdr/plan26-zeta</code>, state <code>~/zetasdr/state</code>"),
    ("Radios", "<code>--source-soapy b210=driver=uhd,…,gain=65,ppm=2.7</code> and <code>--source-soapy hackrf=driver=hackrf,gain=45</code>: both on the field host's USB"),
    ("LLM", "<code>--llm-url http://127.0.0.1:8080 --llm-model gemma-4-12b</code>: the field host's OpenAI-compatible gateway (user <code>vllm</code>) in front of llama.cpp serving Gemma 4 12B on the 4090"),
    ("Speech", "<code>--stt-socket</code> to the <code>sdr</code> user's runtime-dir socket: local-voice (<code>voiced</code>) on the same host and GPU"),
    ("ACMA register", "<code>--knowledge-acma http://acma-host:3110</code>: another host on the LAN"),
    ("Clients", "the page from the desktop browser; <code>deploy/field-host/z.sh</code> runs the CLI on the <code>sdr</code> side over ssh, with the secret sourced there"),
]
rows = "\n".join(f'<tr><td>{p}</td><td>{w}</td><td>{h}</td><td><span class="chip {where.lower()}">{where}</span></td></tr>' for p, w, h, where in TABLE)
station = "\n".join(f"<tr><td>{a}</td><td>{b}</td></tr>" for a, b in STATION)

page = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ZetaSDR mudmap</title>
<style>
:root {{ color-scheme: light dark; --fg: #1b1f24; --bg: #fbfbf9; --muted: #5a6270; --line: #d9dcd6; }}
@media (prefers-color-scheme: dark) {{ :root {{ --fg: #e6e8ec; --bg: #14171c; --muted: #9aa3b2; --line: #2c323b; }} }}
html, body {{ margin: 0; background: var(--bg); color: var(--fg); font-family: {FONT}; line-height: 1.45; }}
main {{ max-width: 1460px; margin: 0 auto; padding: 20px 16px 48px; }}
h1 {{ font-size: 22px; margin: 0 0 4px; }}
h2 {{ font-size: 16px; margin: 28px 0 8px; }}
p {{ max-width: 1000px; }}
p.lede {{ margin: 0 0 14px; color: var(--muted); font-size: 14px; }}
figure {{ margin: 0; overflow-x: auto; }}
figure svg {{ min-width: 900px; max-width: 100%; height: auto; display: block; border: 1px solid var(--line); border-radius: 8px; color: #1b1f24; }}
table {{ border-collapse: collapse; font-size: 13.5px; width: 100%; max-width: 1200px; }}
th, td {{ text-align: left; vertical-align: top; padding: 6px 10px; border-bottom: 1px solid var(--line); }}
th {{ font-size: 12px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }}
.wrap {{ overflow-x: auto; }}
code {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.9em; }}
.chip {{ display: inline-block; padding: 1px 8px; border-radius: 9px; color: #fff; font-size: 11px; font-weight: 700; letter-spacing: 0.05em; }}
.chip.local {{ background: {WHERE["LOCAL"]}; }} .chip.remote {{ background: {WHERE["REMOTE"]}; }} .chip.either {{ background: {WHERE["EITHER"]}; }}
ul {{ max-width: 1000px; }}
</style>
</head>
<body>
<main>
<h1>ZetaSDR mudmap</h1>
<p class="lede">The short version of the <a href="index.html">architecture map</a>: what plugs into a node, over which wire, set up by which flag, and whether it has to sit on the node's host. As of {AS_OF}. Generated by <code>docs/architecture/mudmap.py</code>.</p>
<figure>
{svg_text.replace('style="color:#1b1f24"', '')}
</figure>

<h2>Local, remote or either</h2>
<ul>
<li><b>Local</b>: it has to be on the node's host. That means anything the node opens directly: a USB radio (through its worker child), a Unix socket, a child process, a file.</li>
<li><b>Remote</b>: it is on another host by nature (KiwiSDR, a VRT sender, another node, SondeHub). The node is the client.</li>
<li><b>Either</b>: it is reached by URL, so localhost and another host work the same way. All the clients, the LLM and the ACMA register are in this group.</li>
</ul>
<p>Two rules cover most cases. A USB radio on another machine is attached by running a node on that machine and adding it here with <code>--source-node</code>. Speech that needs a GPU elsewhere goes through <code>--stt-node</code> to the node that has the GPU. Every flag that wires something in (<code>--source-*</code>, <code>--plugin</code>, <code>--knowledge-acma</code>, <code>--stt-*</code>, <code>--llm-*</code>, <code>--modes-dir</code>, <code>--themes-dir</code>) belongs to the serving node and must be repeated on every start. A client using <code>--node</code> passes none of them.</p>
<p>There are two MCP directions. <code>zeta-mcp</code> makes the node an MCP <em>server</em> for Claude Code and similar clients. With <code>--knowledge-acma</code>, the node is an MCP <em>client</em> of the register. There are also two agent paths. <code>zeta ask</code> runs the tool loop wherever it is started. <code>POST /ask</code> (the page's agent pane) runs it inside the node, so the node's own <code>--llm-url</code> must reach the LLM.</p>

<h2>How a model drives the node: who runs the loop, and the doors in</h2>
<p>A configured LLM never touches the node. It answers with tool calls, and zeta's own loop executes each one behind a gate that code enforces. An external coding agent such as Claude Code is a client with the secret. It runs its own loop, calls the node over HTTP the way any script does, and is held only by the guide's rules and the operator's approval of its tool calls.</p>
<figure>
{agents_svg.replace('style="color:#1b1f24"', '')}
</figure>
<div class="wrap"><table>
<thead><tr><th></th><th>A. Configured LLM (<code>POST /ask</code>, <code>zeta ask</code>)</th><th>B. External coding agent (<code>zeta-mcp</code>, CLI)</th></tr></thead>
<tbody>
{agent_rows}
</tbody>
</table></div>

<h2>Each piece</h2>
<div class="wrap"><table>
<thead><tr><th>Piece</th><th>Wire</th><th>How it is wired</th><th>Where</th></tr></thead>
<tbody>
{rows}
</tbody>
</table></div>

<h2>This station</h2>
<p>How the home node is wired today (from the running unit's command line, 2026-11-04):</p>
<div class="wrap"><table>
<tbody>
{station}
</tbody>
</table></div>
</main>
</body>
</html>
"""
(HERE / "mudmap.html").write_text(page)
print("wrote", HERE / "mudmap.svg", "and", HERE / "mudmap.html")
