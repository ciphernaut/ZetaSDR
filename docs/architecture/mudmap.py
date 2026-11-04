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
