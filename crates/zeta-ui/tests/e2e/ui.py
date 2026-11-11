"""End-to-end scenarios for the ZetaSDR web app, driven by Playwright (headless Chromium).

Usage: python3 ui.py <node-url> <secret> [scenario]

Runs every `scenario_*` function (or only those named, comma-separated, with or without the
`scenario_` prefix) against a running `zeta serve`, each in a fresh browser context. Prints `PASS <name>`
or `FAIL <name>: <reason>` (plus the browser console errors) and exits 1 on the first failure.
Run by `cargo test -p zeta --test ui`, which starts the node with the `tone`, `aprs`, `wfm`
(wideband FM, 1 kHz tone with pilot, 240 kHz at 104.9 MHz) and `pocsag` (a 1200-baud page
`HELLO ZETA` to address 1234567, 48 kHz at 148.6375 MHz) and `speech` (speech-like bursts,
wideband FM, 240 kHz at 104.7 MHz) fixtures, `retune` (the tone recording at 250 MHz,
tunable over 200-300 MHz: a tune moves its capture's centre), `scanner` (the tone recording
tunable over 50-150 MHz: the source automatic scans around `tone` pick), `cb` (the tone
recording at 476.9 MHz), `quiet` (low complex noise at 20 MHz, which `measure` labels `noise`:
a fitted demodulator finds no signal there), and a fake speech daemon (`--stt-socket`,
answering `what sources are there`), passes the node's
state directory in ZETA_UI_STATE_DIR, and the URL of a second node started without a speech
daemon (with the `speech` source) in ZETA_UI_URL_NOSTT (for the "no daemon" scenarios), and
the URL of a third node with the `cb` source whose `--llm-url` is a request-keyed fake model in
ZETA_UI_URL_LLM (for the `/ask` scenarios), and a node whose ring is a few seconds
(`--ring-budget-mb 1`) in ZETA_UI_URL_SHORTRING.
"""

import base64
import json
import os
import re
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

from playwright.sync_api import expect, sync_playwright

expect.set_options(timeout=10_000)

TIMEOUT_MS = 10_000
CHROMIUM_ARGS = [
    "--autoplay-policy=no-user-gesture-required",
    "--use-fake-device-for-media-stream",
    "--use-fake-ui-for-media-stream",
]
WIDE = {"width": 1280, "height": 800}
NARROW = {"width": 390, "height": 844}


# ---- helpers ---------------------------------------------------------------------------

def http_json(url, secret, path, body=None):
    """GET (or POST `body` as JSON to) `url + path` with the bearer; the decoded reply."""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url + path, data=data,
                                 headers={"Authorization": "Bearer " + secret,
                                          "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


# The demo station, Brisbane Airport: ui.rs `station_set`s the node to it at start, and a
# scenario that clears or moves the station puts it back.
DEMO_STATION = {"lat": -27.3842, "lon": 153.1175, "alt_m": 4.0}


def set_demo_station(url, secret):
    http_json(url, secret, "/op", {"op": "station_set", **DEMO_STATION, "region": "AU",
                                   "antennas": None})


def login(page, url, secret):
    page.goto(url + "/ui/")
    expect(page.locator("#login")).to_be_visible()
    page.fill("#secret", secret)
    page.click("#connect")
    expect(page.locator("#login")).to_be_hidden()


def footprint(page):
    return page.evaluate("document.documentElement.dataset.footprint")


# ---- scenarios -------------------------------------------------------------------------

# Must run first: it asserts the node has no websocket open at all, and the later scenarios
# open the ops stream. `scenarios()` keeps definition order, so keep this one at the top.
def scenario_wrong_secret_shows_error_and_starts_nothing(page, url, secret):
    page.goto(url + "/ui/")
    expect(page.locator("#login")).to_be_visible()
    page.fill("#secret", "nope")
    page.click("#connect")
    expect(page.locator("#login-error")).to_contain_text("secret rejected")
    expect(page.locator("#login")).to_be_visible()
    stored = page.evaluate("sessionStorage.getItem('zeta.secret')")
    assert stored is None, f"wrong secret was stored: {stored!r}"
    page.wait_for_timeout(500)
    health = http_json(url, secret, "/health")
    assert health["ws_connections"] == 0, f"websocket opened: {health}"
    jobs = http_json(url, secret, "/op", {"op": "job_list"})
    running = [j for j in jobs["value"] if j["state"]["status"] == "running"]
    assert running == [], f"jobs started: {running}"


def scenario_login_and_tabs(page, url, secret):
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    expect(page.locator('.src-tab[data-id="aprs"]')).to_be_visible()
    assert footprint(page) == "wide", f"footprint {footprint(page)!r}"
    expect(page.locator("#side")).to_be_visible()
    expect(page.locator("#cmd")).to_be_visible()
    # Selecting a tab marks it and survives a reload (secret and selection in sessionStorage).
    page.click('.src-tab[data-id="aprs"]')
    expect(page.locator('.src-tab[data-id="aprs"]')).to_have_attribute("aria-selected", "true")
    page.reload()
    expect(page.locator("#login")).to_be_hidden()
    expect(page.locator('.src-tab[data-id="aprs"]')).to_have_attribute("aria-selected", "true")
    # The secret is not left in the (hidden) login field.
    assert page.input_value("#secret") == "", "secret left in #secret"
    # Wide tabs show the tune range, not the compact centre.
    expect(page.locator('.src-tab[data-id="aprs"] .range')).to_be_visible()
    expect(page.locator('.src-tab[data-id="aprs"] .centre')).to_be_hidden()


def scenario_command_line_applies_op(page, url, secret):
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    page.keyboard.press("Control+k")
    expect(page.locator("#cmd")).to_be_focused()
    page.keyboard.type('{"op":"source_list"}')
    page.keyboard.press("Enter")
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (12 sources)")
    # An op error shows its name and field, marked as an error.
    page.fill("#cmd", json.dumps({"op": "job_start", "kind": "measure", "source": "tone",
                                  "center_hz": 1, "bandwidth_hz": 10000,
                                  "deviation_hz": None, "sideband": None,
                                  "elastic": False, "replace": []}))
    page.press("#cmd", "Enter")
    err = page.locator("#transcript .line.error", has_text="out_of_range")
    expect(err).to_contain_text("center_hz")
    # Free text goes to /ask; this node has no LLM, so its 503 message shows.
    page.fill("#cmd", "what is on 100 MHz?")
    page.press("#cmd", "Enter")
    expect(page.locator("#transcript .line.user", has_text="what is on 100 MHz?")).to_be_visible()
    expect(page.locator("#transcript .line.assistant.error")).to_contain_text("--llm-url")
    # A job started from the command line appears in the jobs list and can be stopped there.
    page.fill("#cmd", json.dumps({"op": "job_start", "kind": "measure", "source": "tone",
                                  "center_hz": 100_000_000, "bandwidth_hz": 10_000,
                                  "deviation_hz": None, "sideband": None,
                                  "elastic": False, "replace": []}))
    page.press("#cmd", "Enter")
    expect(page.locator("#transcript")).to_contain_text("job_start kind=measure source=tone")
    row = page.locator('#jobs .job[data-state="running"]', has_text="measure")
    expect(row).to_have_count(1)
    job = row.get_attribute("data-id")
    row.locator("button.stop").click()
    expect(page.locator("#transcript")).to_contain_text("job_stop")
    # The finished row stays listed (finished_rows_s), stopped and without a Stop button.
    row = page.locator(f'#jobs .job[data-id="{job}"]')
    expect(row.locator(".state")).to_have_text("stopped")
    expect(row.locator("button.stop")).to_have_count(0)


def scenario_narrow_hides_pane(page, url, secret):
    page.set_viewport_size(NARROW)
    login(page, url, secret)
    assert page.input_value("#secret") == "", "secret left in #secret"
    assert footprint(page) == "narrow", f"footprint {footprint(page)!r}"
    expect(page.locator("#side")).to_be_hidden()
    # Narrow tabs carry a compact centre instead of the range.
    expect(page.locator('.src-tab[data-id="aprs"] .centre')).to_have_text("144.8")
    expect(page.locator('.src-tab[data-id="aprs"] .range')).to_be_hidden()
    expect(page.locator("#drawer-toggle")).to_be_visible()
    page.click("#drawer-toggle")
    expect(page.locator("#side")).to_be_visible()
    # The drawer shows both panes, so their header bars are inert and read expanded.
    for name in ("transcript", "jobs"):
        head = page.locator(f"#side .pane.{name} .pane-head")
        expect(head).to_be_disabled()
        expect(head).to_have_attribute("aria-expanded", "true")
        expect(head).to_have_attribute("aria-controls", f"pane-{name}")
        expect(page.locator(f"section#pane-{name}.pane.{name}")).to_have_count(1)
    # Widening re-evaluates the footprint.
    page.set_viewport_size(WIDE)
    # (a function predicate polls without a string eval, which the page's CSP refuses)
    page.wait_for_function("() => document.documentElement.dataset.footprint === 'wide'")
    page.wait_for_function("document.documentElement.dataset.footprint == 'wide'")
    expect(page.locator("#drawer-toggle")).to_be_hidden()
    expect(page.locator("#side .pane.jobs .pane-head")).to_be_enabled()


def scenario_401_after_login_returns_to_login(page, url, secret):
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    # The node now rejects the secret the page holds (as after a restart with a new one).
    page.evaluate("window.zetaTest.setSecret('wrong'); window.zetaTest.poll()")
    expect(page.locator("#login")).to_be_visible()
    expect(page.locator("#login-error")).to_contain_text("secret rejected")
    stored = page.evaluate("sessionStorage.getItem('zeta.secret')")
    assert stored is None, f"rejected secret still stored: {stored!r}"
    # Logging in again works and starts one shell, not two.
    page.fill("#secret", secret)
    page.click("#connect")
    expect(page.locator("#login")).to_be_hidden()
    page.fill("#cmd", '{"op":"source_list"}')
    page.press("#cmd", "Enter")
    expect(page.locator("#transcript .line.user")).to_have_count(1)


def scenario_double_connect_starts_one_shell(page, url, secret):
    page.set_viewport_size(WIDE)
    page.goto(url + "/ui/")
    page.fill("#secret", secret)
    page.press("#secret", "Enter")
    page.press("#secret", "Enter")
    expect(page.locator("#login")).to_be_hidden()
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    page.wait_for_timeout(500)
    health = http_json(url, secret, "/health")
    assert health["ws_connections"] <= 3, f"duplicate sockets: {health}"
    page.fill("#cmd", '{"op":"source_list"}')
    page.press("#cmd", "Enter")
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (12 sources)")
    page.wait_for_timeout(300)
    expect(page.locator("#transcript .line", has_text="source_list → ok")).to_have_count(1)
    expect(page.locator("#transcript .line.user")).to_have_count(1)


def running_spectrum_jobs(url, secret, source):
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    return [j for j in jobs if j["kind"] == "spectrum" and j["source"] == source
            and j["state"]["status"] == "running"]


# Whether the right-most 10 columns of the waterfall canvas hold a pixel with G > 200.
BRIGHT_AT_RIGHT_JS = """() => {
  const c = document.querySelector('#waterfall canvas.wf-canvas');
  if (!c || c.width < 10 || c.height < 1) return false;
  const d = c.getContext('2d').getImageData(c.width - 10, 0, 10, c.height).data;
  for (let i = 0; i < d.length; i += 4) if (d[i + 1] > 200) return true;
  return false;
}"""


def fresh_spectrum_jobs(url, secret):
    """Start a waterfall scenario with no spectrum job left over from an earlier one, so the
    page starts (and owns) its own."""
    for source in ("tone", "aprs", "wfm", "speech", "retune", "scanner", "cb", "quiet", "video", "video2"):
        stop_spectrum_jobs(url, secret, source)


def show_tone_waterfall(page, url, secret):
    fresh_spectrum_jobs(url, secret)
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    page.click('.src-tab[data-id="tone"]')
    expect(page.locator('.src-tab[data-id="tone"]')).to_have_attribute("aria-selected", "true")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)


def wait_one_spectrum_job(url, secret, source, within_s=3.0):
    deadline = time.monotonic() + within_s
    while True:
        jobs = running_spectrum_jobs(url, secret, source)
        if len(jobs) == 1 or time.monotonic() > deadline:
            return jobs
        time.sleep(0.2)


def scenario_waterfall_paints(page, url, secret):
    show_tone_waterfall(page, url, secret)
    jobs = wait_one_spectrum_job(url, secret, "tone")
    assert len(jobs) == 1, f"spectrum jobs on tone: {jobs}"
    # The frequency axis: 5 ticks in MHz with 3 decimals, inside the tone's band.
    ticks = page.locator("#waterfall .wf-axis .tick").all_text_contents()
    assert len(ticks) == 5, f"ticks {ticks}"
    for t in ticks:
        v = float(t)
        assert 99.976 <= v <= 100.024 and t == f"{v:.3f}", f"tick {t!r} in {ticks}"


def scenario_waterfall_decodes_quantised_rows(page, url, secret):
    # The node sends spectrum event v2 (`q`, `floor_db`, `step_db`); the waterfall paints
    # them straight from the u8 levels, and still decodes an older node's `db` rows.
    show_tone_waterfall(page, url, secret)
    page.wait_for_function("() => window.zetaTest.lastRowFormat !== null", timeout=5_000)
    fmt = page.evaluate("window.zetaTest.lastRowFormat")
    assert fmt == "q", f"lastRowFormat {fmt!r}"
    rows = page.evaluate("""async () => {
      const m = await import('./waterfall.js');
      const q = m.decodeRow({size: 4, q: [0, 2, 255, 7], floor_db: -120, step_db: 0.5});
      const db = m.decodeRow({size: 2, db: [-90.5, -30]});
      return {qType: q.q.constructor.name, q: Array.from(q.q), floor: q.floorDb,
              step: q.stepDb, dbType: db.db.constructor.name, db: Array.from(db.db),
              none: m.decodeRow({label: {name: 'x'}})};
    }""")
    assert rows == {"qType": "Uint8Array", "q": [0, 2, 255, 7], "floor": -120, "step": 0.5,
                    "dbType": "Float32Array", "db": [-90.5, -30], "none": None}, rows


def stop_spectrum_jobs(url, secret, source):
    """Stop every running spectrum job on `source` through the API."""
    for j in running_spectrum_jobs(url, secret, source):
        http_json(url, secret, "/op", {"op": "job_stop", "job": j["id"]})
    assert running_spectrum_jobs(url, secret, source) == [], "spectrum jobs survive job_stop"


def wait_spectrum_jobs(url, secret, source, ok, within_s):
    """Poll the running spectrum jobs on `source` until `ok(jobs)` or `within_s` passes."""
    deadline = time.monotonic() + within_s
    while True:
        jobs = running_spectrum_jobs(url, secret, source)
        if ok(jobs) or time.monotonic() > deadline:
            return jobs
        time.sleep(0.2)


def scenario_ui_spectrum_job_is_reused_not_duplicated(page, url, secret):
    # A reload stops the page's own job on pagehide and starts exactly one fresh one: no
    # duplicate, and the stopped job is not picked up again.
    stop_spectrum_jobs(url, secret, "tone")
    show_tone_waterfall(page, url, secret)
    jobs = wait_one_spectrum_job(url, secret, "tone")
    assert len(jobs) == 1, f"spectrum jobs on tone before reload: {jobs}"
    old = jobs[0]["id"]
    page.reload()
    expect(page.locator("#login")).to_be_hidden()
    jobs = wait_spectrum_jobs(url, secret, "tone",
                              lambda js: len(js) == 1 and js[0]["id"] != old, 5.0)
    assert len(jobs) == 1 and jobs[0]["id"] != old, \
        f"spectrum jobs on tone after reload (old {old}): {jobs}"
    page.wait_for_timeout(1_000)
    jobs = running_spectrum_jobs(url, secret, "tone")
    assert len(jobs) == 1 and jobs[0]["id"] != old, f"spectrum jobs settle to: {jobs}"
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)


# Records on window.__badgeSeen whether #wf-badge was ever shown from now on.
WATCH_BADGE_JS = """() => {
  const b = document.querySelector('#wf-badge');
  window.__badgeSeen = !b.hidden;
  new MutationObserver(() => { if (!b.hidden) window.__badgeSeen = true; })
    .observe(b, {attributes: true, attributeFilter: ['hidden']});
}"""

WF_STATE_JS = "() => window.zetaTest.wf()"


def wait_waterfall_follows(page, pred_js, what):
    """Wait until the waterfall's job satisfies `pred_js` (a JS expression over `w`), then
    until it paints 3 more columns, the right edge is bright and #wf-badge is hidden."""
    page.wait_for_function(
        f"() => {{ const w = window.zetaTest.wf(); return !!w && ({pred_js}); }}", timeout=10_000)
    p1 = page.evaluate(WF_STATE_JS)["painted"]
    try:
        page.wait_for_function(
            f"""() => {{
              const w = window.zetaTest.wf();
              return w.painted > {p1} + 3 && !w.badge && ({BRIGHT_AT_RIGHT_JS})();
            }}""", timeout=10_000)
    except Exception as e:
        state = page.evaluate(WF_STATE_JS)
        raise AssertionError(f"{what}: waterfall not repainting: {state}") from e


def scenario_waterfall_reconnects_after_socket_close(page, url, secret):
    show_tone_waterfall(page, url, secret)
    page.wait_for_function("() => window.zetaTest.wf() && window.zetaTest.wf().jobId")
    old = page.evaluate(WF_STATE_JS)["jobId"]
    page.evaluate(WATCH_BADGE_JS)
    # Another client replaces the job: the waterfall follows the new one.
    http_json(url, secret, "/op", {"op": "job_stop", "job": old})
    new = http_json(url, secret, "/op", {"op": "job_start", "kind": "spectrum", "source": "tone",
                                         "center_hz": 100_000_000, "bandwidth_hz": 48_000,
                                         "deviation_hz": None, "sideband": None,
                                         "elastic": False, "replace": []})["value"]
    wait_waterfall_follows(page, f"w.jobId === {json.dumps(new)}", "after job_start")
    assert page.evaluate("window.__badgeSeen"), "#wf-badge never shown while down"
    jobs = running_spectrum_jobs(url, secret, "tone")
    assert [j["id"] for j in jobs] == [new], f"spectrum jobs on tone: {jobs}"
    # Nobody replaces it: the waterfall starts its own job within 10 s.
    page.evaluate(WATCH_BADGE_JS)
    http_json(url, secret, "/op", {"op": "job_stop", "job": new})
    wait_waterfall_follows(page, f"w.jobId && w.jobId !== {json.dumps(new)}", "own job")
    assert page.evaluate("window.__badgeSeen"), "#wf-badge never shown while down"
    own = page.evaluate(WF_STATE_JS)["jobId"]
    jobs = running_spectrum_jobs(url, secret, "tone")
    assert [j["id"] for j in jobs] == [own], f"spectrum jobs on tone (own {own}): {jobs}"
    ui_jobs = page.evaluate("JSON.parse(sessionStorage.getItem('zeta.uiJobs') || '[]')")
    assert ui_jobs == [own], f"zeta.uiJobs {ui_jobs} (own {own})"


def scenario_pagehide_stops_ui_jobs(page, url, secret):
    # Its own context, so the page closing is the only thing that can stop the job.
    stop_spectrum_jobs(url, secret, "tone")
    ctx = page.context.browser.new_context(viewport=WIDE)
    try:
        tab = ctx.new_page()
        show_tone_waterfall(tab, url, secret)
        jobs = wait_one_spectrum_job(url, secret, "tone")
        assert len(jobs) == 1, f"spectrum jobs on tone: {jobs}"
        # page.close(run_before_unload=True) runs the unload steps, so `pagehide` fires;
        # a plain ctx.close() in headless Chromium does not.
        tab.close(run_before_unload=True)
        jobs = wait_spectrum_jobs(url, secret, "tone", lambda js: js == [], 5.0)
        assert jobs == [], f"spectrum jobs on tone 5 s after the page closed: {jobs}"
    finally:
        ctx.close()


def scenario_source_switch_stops_own_spectrum_job(page, url, secret):
    show_tone_waterfall(page, url, secret)
    jobs = wait_one_spectrum_job(url, secret, "tone")
    assert len(jobs) == 1, f"spectrum jobs on tone: {jobs}"
    page.click('.src-tab[data-id="aprs"]')
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    jobs = wait_spectrum_jobs(url, secret, "tone", lambda js: js == [], 5.0)
    assert jobs == [], f"the page's own tone job survives the switch to aprs: {jobs}"
    ui_jobs = page.evaluate("JSON.parse(sessionStorage.getItem('zeta.uiJobs') || '[]')")
    aprs = running_spectrum_jobs(url, secret, "aprs")
    assert [j["id"] for j in aprs] == ui_jobs, f"zeta.uiJobs {ui_jobs}, aprs jobs {aprs}"


def scenario_selection_reports_band(page, url, secret):
    show_tone_waterfall(page, url, secret)
    box = page.locator("#waterfall canvas.wf-canvas").bounding_box()
    page.mouse.move(box["x"] + 300, box["y"] + 100)
    page.mouse.down()
    page.mouse.move(box["x"] + 350, box["y"] + 120, steps=4)
    page.mouse.move(box["x"] + 400, box["y"] + 140, steps=4)
    page.mouse.up()
    sel = page.evaluate("window.zetaLastSelection")
    assert sel, f"no selection: {sel!r}"
    for k in ("centerHz", "bandwidthHz", "startNs", "endNs"):
        assert isinstance(sel.get(k), int), f"{k} not an integer: {sel}"
    assert sel["bandwidthHz"] > 0, f"selection {sel}"
    assert 99_976_000 <= sel["centerHz"] <= 100_024_000, f"selection {sel}"
    assert sel["startNs"] <= sel["endNs"], f"selection {sel}"


# ---- overlays and transcript (Task 8) ----------------------------------------------------
# These run after the waterfall scenarios: they start decode jobs that the page watches (more
# websockets), and each stops its own jobs at the end so later runs start clean.

def start_job(url, secret, kind, source, center_hz, bandwidth_hz, deviation_hz=None):
    return http_json(url, secret, "/op", {"op": "job_start", "kind": kind, "source": source,
                                          "center_hz": center_hz, "bandwidth_hz": bandwidth_hz,
                                          "deviation_hz": deviation_hz, "sideband": None,
                                          "elastic": False, "replace": []})["value"]


def stop_job(url, secret, job):
    try:
        http_json(url, secret, "/op", {"op": "job_stop", "job": job})
    except Exception:  # noqa: BLE001 - best-effort cleanup
        pass


def select_source(page, url, secret, source):
    fresh_spectrum_jobs(url, secret)
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    page.click(f'.src-tab[data-id="{source}"]')
    expect(page.locator(f'.src-tab[data-id="{source}"]')).to_have_attribute(
        "aria-selected", "true")


def scenario_aprs_ribbon_appears(page, url, secret):
    select_source(page, url, secret, "aprs")
    job = start_job(url, secret, "aprs_decode", "aprs", 144_800_000, 12_500)
    try:
        page.evaluate("window.zetaTest.poll()")
        ribbon = page.locator("#waterfall .ov-ribbon", has_text="VK3ABC")
        expect(ribbon.first).to_be_visible(timeout=15_000)
        expect(page.locator("#waterfall .ov-ribbon.aprs")).to_have_count(1)
        expect(ribbon.first).to_have_text("aprs 1 stations, last VK3ABC")
        counter = page.locator(f'#jobs .job[data-id="{job}"] .events')
        page.wait_for_function(
            """(id) => {
              const e = document.querySelector(`#jobs .job[data-id="${id}"] .events`);
              return !!e && parseInt(e.textContent, 10) >= 1;
            }""", arg=job, timeout=5_000)
        expect(counter).to_be_visible()
        if os.environ.get("ZETA_UI_SHOTS"):
            page.screenshot(path=os.path.join(os.environ["ZETA_UI_SHOTS"], "overlays.png"))
    finally:
        stop_job(url, secret, job)


def scenario_pocsag_ribbon_appears(page, url, secret):
    select_source(page, url, secret, "pocsag")
    job = start_job(url, secret, "pocsag_decode", "pocsag", 148_637_500, 12_500)
    try:
        page.evaluate("window.zetaTest.poll()")
        ribbon = page.locator("#waterfall .ov-ribbon.pocsag", has_text="1234567")
        expect(ribbon.first).to_be_visible(timeout=15_000)
        expect(ribbon.first).to_have_text("pocsag 1 addresses, last 1234567")
        page.wait_for_function(
            """(id) => {
              const e = document.querySelector(`#jobs .job[data-id="${id}"] .events`);
              return !!e && parseInt(e.textContent, 10) >= 1;
            }""", arg=job, timeout=5_000)
    finally:
        stop_job(url, secret, job)


def scenario_measure_label_appears(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_job(url, secret, "measure", "tone", 100_000_000, 10_000)
    try:
        page.evaluate("window.zetaTest.poll()")
        label = page.locator("#waterfall .ov-label", has_text="carrier")
        expect(label).to_be_visible(timeout=5_000)
        text = label.text_content()
        assert text.startswith("L1 carrier "), f"label text {text!r}"
        conf = text.split()[-1]
        assert len(conf.split(".")[-1]) == 2, f"confidence not 2 dp: {text!r}"
        # One label per job: later events replace it.
        page.wait_for_timeout(2_000)
        expect(page.locator("#waterfall .ov-label")).to_have_count(1)
    finally:
        stop_job(url, secret, job)


def scenario_narrow_answer_is_ribbon(page, url, secret):
    page.set_viewport_size(NARROW)
    login(page, url, secret)
    assert footprint(page) == "narrow", f"footprint {footprint(page)!r}"
    expect(page.locator(".src-tab").first).to_be_visible()
    page.evaluate("window.zetaTest.ribbon('x')")
    ribbon = page.locator("#waterfall .ov-ribbon.agent")
    expect(ribbon).to_be_visible()
    expect(ribbon).to_have_text("x")
    # Ribbons are capped (the oldest go first).
    page.evaluate("for (let i = 0; i < 205; i++) window.zetaTest.ribbon('r' + i)")
    n = page.locator("#waterfall .ov-ribbon").count()
    assert n == 200, f"{n} ribbons after 206"
    expect(page.locator("#waterfall .ov-ribbon", has_text=re.compile(r"^x$"))).to_have_count(0)
    # The drawer still carries the transcript.
    page.click("#drawer-toggle")
    page.fill("#cmd", '{"op":"source_list"}')
    page.press("#cmd", "Enter")
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (12 sources)")
    expect(page.locator("#side")).to_be_visible()


def watch_job_list_records(page):
    """Count the `job_list` records the page's websockets receive (the ops stream's, one
    per poll) from now on; the list whose length is the count."""
    seen = []

    def on_ws(ws):
        ws.on("framereceived", lambda f: seen.append(1)
              if isinstance(f, str) and '"job_list"' in f else None)

    page.on("websocket", on_ws)
    return seen


def wait_job_list_records(page, seen, n, within_ms=10_000):
    """Wait until `seen` (watch_job_list_records) counts `n` records, then a beat for the
    page to draw the last."""
    waited = 0
    while len(seen) < n:
        assert waited < within_ms, f"{len(seen)} job_list records in {within_ms} ms, want {n}"
        page.wait_for_timeout(50)
        waited += 50
    page.wait_for_timeout(200)


def scenario_transcript_hides_polls(page, url, secret):
    page.set_viewport_size(WIDE)
    seen = watch_job_list_records(page)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    # The load's poll and the next (JOBS_POLL_MS) reached the page and drew nothing.
    wait_job_list_records(page, seen, 2)
    polls = page.locator("#transcript .line.op", has_text="job_list")
    expect(polls).to_have_count(0)
    page.fill("#cmd", '{"op":"job_list"}')
    page.press("#cmd", "Enter")
    typed = page.locator("#transcript .line.op", has_text="job_list → ok")
    expect(typed).to_have_count(1)
    # Later polls stay hidden and do not fold into the typed line (the typed op's own "ui"
    # record is hidden too: its one line comes from the /op reply).
    wait_job_list_records(page, seen, len(seen) + 2)
    expect(typed).to_have_count(1)
    expect(typed.locator(".count")).to_have_text("")
    # Another caller's poll shows, tagged with its transport (ruling R6).
    http_json(url, secret, "/op", {"op": "job_list"})
    other = page.locator("#transcript .line.op", has_text="[http] job_list → ok")
    expect(other).to_have_count(1)
    expect(typed).to_have_count(2)


# ---- context menu (Task 9) -----------------------------------------------------------------
# Each stops the jobs it starts.

# Whether the selection overlay canvas is fully transparent.
OVERLAY_BLANK_JS = """() => {
  const c = document.querySelector('#waterfall canvas.wf-overlay');
  const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
  for (let i = 3; i < d.length; i += 4) if (d[i] !== 0) return false;
  return true;
}"""


# Whether drag_menu's box (from 40 px to 20 px short of the canvas's right edge, the
# waterfall's own slotAt for each end) would span at least `s` seconds of signal. A fresh
# job's first rows can come faster than real time, so a count of painted columns does not
# say.
SPAN_AT_LEAST_JS = """(s) => {
  const w = window.zetaTest.waterfall();
  if (!w || !(w.columns > 0)) return false;
  const left = w.slotAt(40);
  const right = w.slotAt(w.canvas.clientWidth - 20);
  if (left === null || right === null || !w.ringStart[left]) return false;
  return w.ringEnd[right] - w.ringStart[left] >= s * 1e9;
}"""


def wait_span(page, s, timeout=15_000):
    """Wait until drag_menu's box would span `s` seconds of signal (SPAN_AT_LEAST_JS)."""
    page.wait_for_function(SPAN_AT_LEAST_JS, arg=s, timeout=timeout)


# The signal drag_menu's box spans before a menu that classifies or fits it opens, s:
# twice the second the layer-1 classifier needs (the 60 painted columns waited for before
# gave about 2.5 s).
MENU_SPAN_S = 2.0


def open_menu_on_tone(page, url, secret):
    """Log in, show the tone and open the menu on a selection (see drag_menu)."""
    show_tone_waterfall(page, url, secret)
    # The layer-1 classifier needs about a second of a pure tone to call it a carrier
    # (shorter spans read the discriminator's edge transients as FM deviation).
    wait_span(page, MENU_SPAN_S)
    return drag_menu(page)


def drag_menu(page, lo=0.40, hi=0.60):
    """Drag a selection over the band from fraction `lo` to `hi` of its height (by default
    the middle fifth: about 10 kHz around the tone, as the measure job uses) across every
    painted column; the menu opens at the pointer. Asserts the footer and a span of at least
    1 s. Returns the #ctx locator."""
    box = page.locator("#waterfall canvas.wf-canvas").bounding_box()
    x0, x1 = box["x"] + 40, box["x"] + box["width"] - 20
    y0, y1 = box["y"] + box["height"] * lo, box["y"] + box["height"] * hi
    page.mouse.move(x0, y0)
    page.mouse.down()
    page.mouse.move(x1, y1, steps=6)
    page.mouse.up()
    menu = page.locator("#ctx")
    expect(menu).to_be_visible()
    # A menu clamped to the viewport can open under the pointer, and a hovered SSB entry shows
    # its dial in the footer: leave the menu so the footer reads the selection.
    page.mouse.move(2, 2)
    footer = page.locator("#ctx-footer").text_content()
    m = re.search(r"center_hz=\d+ bandwidth_hz=\d+ start_ns=(\d+) end_ns=(\d+)", footer)
    assert m, f"footer {footer!r}"
    assert int(m.group(2)) - int(m.group(1)) >= 1_000_000_000, f"span under 1 s: {footer!r}"
    return menu


def scenario_what_is_this_classifies(page, url, secret):
    menu = open_menu_on_tone(page, url, secret)
    assert not page.evaluate(OVERLAY_BLANK_JS), "no selection rectangle drawn"
    # Escape closes it and erases the selection; a new drag opens it again.
    page.keyboard.press("Escape")
    expect(menu).to_be_hidden()
    assert page.evaluate(OVERLAY_BLANK_JS), "selection rectangle left after the menu closed"
    menu = drag_menu(page)
    page.click('#ctx .ctx-item[data-action="classify"]')
    expect(menu).to_be_hidden()
    expect(page.locator("#transcript .line.note", has_text="classify: running…")).to_be_visible()
    line = page.locator("#transcript .line.op", has_text=re.compile(r"^classify .*→ ok"))
    expect(line.first).to_be_visible(timeout=15_000)
    label = page.locator("#waterfall .ov-label", has_text="carrier")
    expect(label.first).to_be_visible()
    expect(page.locator("#transcript .line", has_text="carrier").first).to_be_visible()


def scenario_decode_starts_job_and_logs_op(page, url, secret):
    menu = open_menu_on_tone(page, url, secret)
    if os.environ.get("ZETA_UI_SHOTS"):
        page.screenshot(path=os.path.join(os.environ["ZETA_UI_SHOTS"], "menu.png"))
    page.click('#ctx .ctx-item[data-action="fm_demod"]')
    expect(menu).to_be_hidden()
    jobs = []
    try:
        expect(page.locator("#transcript .line.op", has_text="job_start kind=fm_demod")
               .first).to_be_visible()
        jobs = [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
                if j["kind"] == "fm_demod" and j["source"] == "tone"
                and j["state"]["status"] == "running"]
        assert len(jobs) == 1, f"fm_demod jobs on tone: {jobs}"
    finally:
        for j in jobs:
            stop_job(url, secret, j["id"])


def running_jobs(url, secret, kind, source):
    return [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
            if j["kind"] == kind and j["source"] == source
            and j["state"]["status"] == "running"]


def scenario_fit_first_lands_on_the_tone(page, url, secret):
    # The box is about 99.9952-100.0048 MHz (9.6 kHz); the tone is at 100.001 MHz. The NBFM
    # entry measures the box first (the menu stays open, its footer reading `measuring…`),
    # then starts on the measured centre, 1.25x the occupied width floored at 5 kHz and
    # capped at the box. The noise-free tone's occupied width is the window's leakage skirt
    # (measured 4781 Hz in a fresh measure job's first window, 7359 Hz after), so the width
    # is checked as fitted (at least the floor, under the box), not pinned. A second click
    # during the fit is ignored; Escape during a fit cancels its start.
    menu = open_menu_on_tone(page, url, secret)
    page.click('#ctx .ctx-item[data-action="fm_demod"]')
    # The entries are aria-disabled during the fit (Playwright would wait): force the click
    # a browser still delivers.
    expect(page.locator('#ctx .ctx-item[data-action="fm_demod"]')).to_have_attribute(
        "aria-disabled", "true")
    page.click('#ctx .ctx-item[data-action="fm_demod"]', force=True)
    jobs = []
    fitted = page.locator("#transcript .line.note", has_text=re.compile(r"^fitted fm_demod "))
    try:
        expect(page.locator("#ctx-footer")).to_have_text("measuring…")
        expect(menu).to_be_hidden(timeout=10_000)
        note = page.locator("#transcript .line.note", has_text=re.compile(
            r"^fitted fm_demod to 100\.00\d\d MHz, \d+\.\d kHz \(carrier\)$"))
        expect(note).to_have_count(1)
        jobs = running_jobs(url, secret, "fm_demod", "tone")
        assert len(jobs) == 1, f"fm_demod jobs on tone: {jobs}"
        t = jobs[0]["tuning"]
        assert abs(t["center_hz"] - 100_001_000) <= 100, f"tuning {t}"
        assert 5000 <= t["bandwidth_hz"] < 9600, f"tuning {t}"
        khz = re.search(r", (\d+\.\d) kHz", note.text_content()).group(1)
        assert khz == f"{t['bandwidth_hz'] / 1e3:.1f}", f"note {khz} kHz for tuning {t}"
        left = running_jobs(url, secret, "measure", "tone")
        assert not left, f"measure jobs left on tone: {left}"
        # A second box (dragged here, not with drag_menu: the menu may open with an SSB
        # entry under the pointer, whose hover rewrites the footer), then Escape mid-fit.
        box = page.locator("#waterfall canvas.wf-canvas").bounding_box()
        page.mouse.move(box["x"] + 40, box["y"] + box["height"] * 0.40)
        page.mouse.down()
        page.mouse.move(box["x"] + box["width"] - 20, box["y"] + box["height"] * 0.60, steps=6)
        page.mouse.up()
        expect(menu).to_be_visible()
        page.click('#ctx .ctx-item[data-action="fm_demod"]')
        expect(page.locator("#ctx-footer")).to_have_text("measuring…")
        page.keyboard.press("Escape")
        expect(menu).to_be_hidden()
        cancelled = page.locator("#transcript .line.note", has_text=re.compile(
            r"^fm_demod: fit cancelled$"))
        expect(cancelled).to_have_count(1, timeout=10_000)
        wait_until(lambda: not running_jobs(url, secret, "measure", "tone"), 5.0,
                   "the cancelled fit's measure job stopped")
        # By now a second fit from the double click would have started its job too.
        after = running_jobs(url, secret, "fm_demod", "tone")
        assert [j["id"] for j in after] == [jobs[0]["id"]], f"fm_demod jobs on tone: {after}"
        expect(fitted).to_have_count(1)
        expect(cancelled).to_have_count(1)
    finally:
        for j in running_jobs(url, secret, "fm_demod", "tone"):
            stop_job(url, secret, j["id"])


def scenario_fit_first_without_a_signal_starts_at_the_box(page, url, secret):
    # Noise only: the fit finds no signal and the job starts at the box as drawn.
    select_source(page, url, secret, "quiet")
    wait_span(page, MENU_SPAN_S)
    menu = drag_menu(page, 0.40, 0.60)
    footer = page.locator("#ctx-footer").text_content()
    m = re.search(r"center_hz=(\d+) bandwidth_hz=(\d+)", footer)
    c, bw = int(m.group(1)), int(m.group(2))
    page.click('#ctx .ctx-item[data-action="fm_demod"]')
    jobs = []
    try:
        expect(menu).to_be_hidden(timeout=10_000)
        expect(page.locator("#transcript .line.note", has_text=re.compile(
            r"^no signal found in 19\.99\d\d-20\.00\d\d MHz; "
            r"started fm_demod at the box as drawn$"))).to_have_count(1)
        wait_until(lambda: running_jobs(url, secret, "fm_demod", "quiet"), 5.0,
                   "a running fm_demod on quiet")
        jobs = running_jobs(url, secret, "fm_demod", "quiet")
        assert len(jobs) == 1, f"fm_demod jobs on quiet: {jobs}"
        assert jobs[0]["tuning"] == {"center_hz": c, "bandwidth_hz": bw}, \
            f"tuning {jobs[0]['tuning']} for footer {footer!r}"
    finally:
        for j in jobs:
            stop_job(url, secret, j["id"])


def scenario_save_sigmf(page, url, secret):
    menu = open_menu_on_tone(page, url, secret)
    # A click elsewhere closes it.
    page.mouse.click(5, 5)
    expect(menu).to_be_hidden()
    menu = drag_menu(page)
    page.click('#ctx .ctx-item[data-action="iq_read"]')
    expect(menu).to_be_hidden()
    line = page.locator("#transcript .line", has_text=re.compile(r"\.sigmf-meta$"))
    expect(line.first).to_be_visible(timeout=15_000)
    path = re.search(r"(/\S+\.sigmf-meta)$", line.first.text_content()).group(1)
    assert os.path.isfile(path), f"{path} does not exist"
    state = os.environ.get("ZETA_UI_STATE_DIR")
    assert state, "ZETA_UI_STATE_DIR not set"
    assert os.path.realpath(path).startswith(os.path.realpath(state) + os.sep), \
        f"{path} not under {state}"
    assert os.path.isfile(path[:-len(".sigmf-meta")] + ".sigmf-data"), f"no data beside {path}"


# ---- band control and scan view (plan 8, Task 6) -------------------------------------------
# Ruling R2: zoom/pan inside the capture only re-renders; leaving the capture checks the
# source's holders before any retune. Each stops the jobs it starts.

TICKS_JS = "() => [...document.querySelectorAll('#waterfall .wf-axis .tick')].map(t => t.textContent)"


def ticks(page):
    return page.evaluate(TICKS_JS)


def wait_ticks(page, top, bottom, what):
    """Wait until the axis' top and bottom ticks read `top` and `bottom` (MHz, 3 dp)."""
    try:
        page.wait_for_function(
            f"""() => {{
              const t = ({TICKS_JS})();
              return t.length === 5 && t[0] === {json.dumps(top)} && t[4] === {json.dumps(bottom)};
            }}""", timeout=10_000)
    except Exception as e:
        raise AssertionError(f"{what}: ticks {ticks(page)}, want {top}..{bottom}") from e


# The backing-pixel rows with G > 200 in the canvas column `arg` CSS px left of the right edge.
BRIGHT_ROWS_AT_JS = """(off) => {
  const c = document.querySelector('#waterfall canvas.wf-canvas');
  const dpr = c.width / c.clientWidth;
  const x = Math.max(0, c.width - 1 - Math.round(off * dpr));
  const d = c.getContext('2d').getImageData(x, 0, 1, c.height).data;
  const rows = [];
  for (let y = 0; y < c.height; y++) if (d[y * 4 + 1] > 200) rows.push(y);
  return rows;
}"""


def wheel_at_centre(page, delta_y):
    box = page.locator("#waterfall canvas.wf-canvas").bounding_box()
    page.mouse.move(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
    page.mouse.wheel(0, delta_y)


def ensure_lines(page):
    return page.locator("#transcript .line", has_text="source_ensure").count()


def scenario_zoom_inside_capture_is_render_only(page, url, secret):
    show_tone_waterfall(page, url, secret)
    # Columns painted 40 px from the right edge before the zoom (one column a pixel).
    page.wait_for_function("() => window.zetaTest.wf().painted > 45", timeout=15_000)
    wait_ticks(page, "100.024", "99.976", "before zooming")
    job = page.evaluate(WF_STATE_JS)["jobId"]
    # Wheel up at the centre: x2 in, about the pointer (the band's centre).
    wheel_at_centre(page, -100)
    wait_ticks(page, "100.012", "99.988", "after zooming in")
    assert ticks(page)[2] == "100.000", f"ticks {ticks(page)}"
    # The stored columns are drawn again on the narrower band: the tone (+1 kHz) shows in
    # columns painted before the zoom, its row at the new scale.
    page.wait_for_timeout(100)
    rows = page.evaluate(BRIGHT_ROWS_AT_JS, 40)
    assert rows, "no bright pixel 40 px from the right edge after the zoom"
    h = page.evaluate("document.querySelector('#waterfall canvas.wf-canvas').height")
    tone_y = h * (100_012_000 - 100_001_000) / 24_000
    assert min(rows) <= tone_y <= max(rows), f"tone row {tone_y:.0f} not in bright rows {rows[:3]}..{rows[-3:]}"
    expect(page.locator("#band")).to_be_visible()
    # Painting continues on the same job, with no operation issued.
    wait_waterfall_follows(page, f"w.jobId === {json.dumps(job)}", "after zooming in")
    page.wait_for_timeout(1_000)
    assert ensure_lines(page) == 0, "a source_ensure was issued for a zoom inside the capture"
    jobs = running_spectrum_jobs(url, secret, "tone")
    assert [j["id"] for j in jobs] == [job], f"spectrum jobs on tone (followed {job}): {jobs}"
    if os.environ.get("ZETA_UI_SHOTS"):
        page.screenshot(path=os.path.join(os.environ["ZETA_UI_SHOTS"], "zoom.png"))
    # A drag down the axis by a quarter of its height pans a quarter of the view up.
    axis = page.locator("#waterfall .wf-axis").bounding_box()
    x, y = axis["x"] + axis["width"] / 2, axis["y"] + axis["height"] / 2
    page.mouse.move(x, y)
    page.mouse.down()
    page.mouse.move(x, y + axis["height"] / 4, steps=4)
    page.mouse.up()
    wait_ticks(page, "100.018", "99.994", "after panning")
    # The band control: centre and width typed in, Apply.
    page.fill('#band input[name="center"]', "100.001")
    page.fill('#band input[name="width"]', "4")
    page.click("#band button")
    wait_ticks(page, "100.003", "99.999", "after Apply")
    assert ensure_lines(page) == 0, "a source_ensure was issued for a pan inside the capture"
    # Wheel down: back to the whole capture, still without an operation.
    page.fill('#band input[name="center"]', "100")
    page.fill('#band input[name="width"]', "24")
    page.click("#band button")
    wait_ticks(page, "100.012", "99.988", "after Apply")
    wheel_at_centre(page, 100)
    wait_ticks(page, "100.024", "99.976", "after zooming back out")
    page.wait_for_timeout(500)
    assert ensure_lines(page) == 0, "a source_ensure was issued for the capture's own band"


def scenario_zoom_beyond_capture_on_a_file_source_shows_out_of_range(page, url, secret):
    # Without the coarse layer (zoom_out none) a view past the capture asks the source.
    show_tone_waterfall(page, url, secret)
    run_cmd(page, "view set zoom_out none")
    expect(view_line(page, "zoom_out = none")).to_be_visible()
    page.wait_for_function("() => window.zetaTest.wf().painted > 5")
    wait_ticks(page, "100.024", "99.976", "before zooming")
    wheel_at_centre(page, 100)
    line = page.locator("#transcript .line", has_text="out_of_range")
    expect(line.first).to_be_visible()
    wait_ticks(page, "100.024", "99.976", "view not reverted")
    wait_waterfall_follows(page, "!!w.jobId", "after the refused retune")
    jobs = wait_one_spectrum_job(url, secret, "tone")
    assert len(jobs) == 1, f"spectrum jobs on tone: {jobs}"


def scenario_zoom_on_held_source_shows_conflict(page, url, secret):
    # A rigid job holds the shown source: zooming out never retunes it. Under zoom_out scan the
    # view leaves the capture and the coarse layer scans it on the scanner; under none the
    # view stays inside the capture with a conflict line.
    stop_scans(url, secret)
    fresh_spectrum_jobs(url, secret)
    job = start_job(url, secret, "measure", "wfm", 104_900_000, 10_000)
    try:
        select_source(page, url, secret, "wfm")
        page.wait_for_function("() => window.zetaTest.wf() && window.zetaTest.wf().painted > 5",
                               timeout=15_000)
        before = ticks(page)
        wheel_at_centre(page, 100)
        wait_ticks(page, "105.140", "104.660", "the view leaves the held capture")
        scans = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                           "an automatic scan on scanner beside the held wfm")
        assert page.evaluate(AUTO_SCAN_JS) == scans[0]["id"], scans
        wheel_at_centre(page, -100)
        wait_ticks(page, before[0], before[4], "back inside the held capture")
        wait_until(lambda: not running_scan_jobs(url, secret, "scanner"), 3.0,
                   "the automatic scan stopped")
        assert page.locator("#transcript .line", has_text="conflict: source wfm").count() == 0
        run_cmd(page, "view set zoom_out none")
        expect(view_line(page, "zoom_out = none")).to_be_visible()
        wheel_at_centre(page, 100)
        line = page.locator("#transcript .line", has_text="conflict: source wfm held by")
        expect(line.first).to_be_visible()
        expect(line.first).to_contain_text(job)
        wait_ticks(page, before[0], before[4], "view not reverted")
        page.wait_for_timeout(1_000)
        assert ensure_lines(page) == 0, "source_ensure issued on a source held by another job"
        assert running_scan_jobs(url, secret, "scanner") == [], "a scan under zoom_out none"
        jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
        mine = [j for j in jobs if j["id"] == job]
        assert mine and mine[0]["state"]["status"] == "running", f"measure job: {mine}"
    finally:
        stop_job(url, secret, job)
        stop_scans(url, secret)


def running_scan_jobs(url, secret, source):
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    return [j for j in jobs if j["kind"] == "scan" and j["source"] == source
            and j["state"]["status"] == "running"]


def stop_scans(url, secret):
    """Stop every running scan job, on any source, through the API."""
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    for j in jobs:
        if j["kind"] == "scan" and j["state"]["status"] == "running":
            stop_job(url, secret, j["id"])


# The id of the page's automatic scan (sessionStorage["zeta.autoScan"]), or null.
AUTO_SCAN_JS = "() => sessionStorage.getItem('zeta.autoScan')"


def scenario_scan_entry_starts_a_scan_job(page, url, secret):
    show_tone_waterfall(page, url, secret)
    wait_span(page, 1.5)  # drag_menu asserts at least 1 s
    # The whole height: the file source's own band, which a scan covers in one step.
    menu = drag_menu(page, 0.0, 1.05)
    expect(page.locator('#ctx .ctx-item[data-action="scan"]')).to_have_text("Scan this span")
    page.click('#ctx .ctx-item[data-action="scan"]')
    expect(menu).to_be_hidden()
    scans = []
    try:
        deadline = time.monotonic() + 5.0
        while not scans and time.monotonic() < deadline:
            scans = running_scan_jobs(url, secret, "tone")
            time.sleep(0.2)
        assert len(scans) == 1, f"scan jobs on tone: {scans}"
        page.wait_for_function("() => window.zetaTest.waterfallMode === 'scan'", timeout=10_000)
        wait_waterfall_follows(page, f"w.jobId === {json.dumps(scans[0]['id'])}", "scan mode")
        assert running_spectrum_jobs(url, secret, "tone") == [], "a spectrum job runs beside the scan"
        if os.environ.get("ZETA_UI_SHOTS"):
            page.screenshot(path=os.path.join(os.environ["ZETA_UI_SHOTS"], "scan.png"))
    finally:
        for j in scans:
            stop_job(url, secret, j["id"])
    # The scan gone, the waterfall goes back to its own spectrum job.
    page.wait_for_function("() => window.zetaTest.waterfallMode === 'spectrum'", timeout=10_000)
    wait_waterfall_follows(page, "!!w.jobId", "back to spectrum")


def scenario_scan_entry_on_a_self_sweeping_source_asks_the_node(page, url, secret):
    # A source whose `capabilities.sweep` holds the span sweeps for itself beside a held IQ
    # channel, so the page asks the node instead of refusing. The e2e node's recording does not
    # sweep: the node's own conflict is what the transcript shows, not the page's.
    posted = []

    def route(r):
        body = r.request.post_data or ""
        if '"job_start"' in body and '"scan"' in body:
            posted.append(body)
        if '"source_list"' not in body:
            return r.continue_()
        resp = r.fetch()
        data = resp.json()
        for src in data.get("value") or []:
            if src.get("id") == "tone":
                src.setdefault("capabilities", {})["sweep"] = {
                    "min_hz": 0, "max_hz": 1e12, "bin_hz_min": 1,
                    "rows_per_s_max": 23, "calibrated": True}
        return r.fulfill(response=resp, body=json.dumps(data))

    stop_scans(url, secret)
    show_tone_waterfall(page, url, secret)
    wait_span(page, 1.5)  # drag_menu asserts at least 1 s
    job = start_job(url, secret, "measure", "tone", 100_000_000, 10_000)
    try:
        # Without the stub the page refuses on its own and posts nothing.
        drag_menu(page, 0.0, 1.05)
        page.click('#ctx .ctx-item[data-action="scan"]')
        # The page's line ends at the job id; the node's names the job's kind and a hint.
        refusal = page.locator("#transcript .line", has_text=re.compile(r"^conflict: source tone held by \S+$"))
        expect(refusal.first).to_be_visible(timeout=10_000)
        assert not posted, f"a scan was posted without a sweep capability: {posted}"
        before = refusal.count()
        page.route("**/op", route)
        drag_menu(page, 0.0, 1.05)
        page.click('#ctx .ctx-item[data-action="scan"]')
        deadline = time.monotonic() + 10.0
        while not posted and time.monotonic() < deadline:
            page.wait_for_timeout(100)
        assert posted, "the page never posted a scan job_start"
        page.wait_for_timeout(1_000)
        assert refusal.count() == before, "the page refused on its own"
        expect(page.locator("#transcript .line", has_text="and retry").first).to_be_visible(
            timeout=10_000)
    finally:
        page.unroute("**/op", route)
        for j in running_scan_jobs(url, secret, "tone"):
            stop_job(url, secret, j["id"])
        stop_job(url, secret, job)


def scenario_scan_looks_and_sweep_time(page, url, secret):
    show_tone_waterfall(page, url, secret)
    wait_span(page, 1.5)  # drag_menu asserts at least 1 s
    drag_menu(page, 0.0, 1.05)
    page.click('#ctx .ctx-item[data-action="scan"]')
    scans = []
    try:
        scans = wait_until(lambda: running_scan_jobs(url, secret, "tone"), 5.0, "a scan job on tone")
        job = scans[0]["id"]
        row = page.locator(f'#jobs .job[data-job="{job}"]')
        select = row.locator("select.scan-look")
        expect(select).to_be_visible(timeout=10_000)
        # A file source lists no lossy depth: no coarse option.
        assert select.locator('option[value="coarse"]').count() == 0, "coarse offered on a file source"

        def scan_context():
            r = http_json(url, secret, "/op", {"op": "job_context", "job": job})["value"]
            return (r or {}).get("scan") or {}

        for look, dwell, average in (("averaged", 20, 16), ("long", 250, 1), ("fast", 20, 1)):
            select.select_option(look)

            def looks_like(dwell=dwell, average=average):
                c = scan_context()
                asked_avg = c.get("average_asked", c.get("average"))
                return c.get("dwell_ms_asked", c.get("dwell_ms")) == dwell and asked_avg == average and c

            wait_until(looks_like, 15.0, f"the scan's context shows {look} ({dwell} ms x {average})")
            # The row follows the stored node on the next refresh.
            expect(select).to_have_value(look, timeout=10_000)
            expect(row.locator(".sweep-ms")).to_have_text(
                re.compile(r"^\d+(\.\d)? (ms|s)$"), timeout=20_000)
        # The page's own per-poll reads stay out of the transcript (the test's own calls show as [http]).
        time.sleep(5.0)  # more than two polls
        noise = page.evaluate(
            "() => [...document.querySelectorAll('#transcript .line')].map((l) => l.textContent)"
            ".filter((t) => /job_context|graph_get/.test(t) && !t.startsWith('['))")
        assert not noise, f"the transcript shows the page's own reads: {noise}"
        # A focused select is not rebuilt under the pointer by the polls.
        handle = select.element_handle()
        select.focus()
        time.sleep(5.0)
        same = page.evaluate(
            "(el) => document.activeElement === el && el.isConnected", handle)
        assert same, "the focused look select was replaced or lost its focus"
    finally:
        for j in running_scan_jobs(url, secret, "tone") or scans:
            stop_job(url, secret, j["id"])


def scenario_scan_look_on_a_source_with_depths(page, url, secret):
    # The e2e nodes run file sources only: the page is told that `tone` has a lossless 16-bit
    # and a lossy 8-bit depth, and a scan's context is answered here with `bits: 16` (the depth
    # in force) while its stored node sets no bits, as on a source with depths.
    st = {"graph_get": 0, "context": 0}

    def route(r):
        body = r.request.post_data or ""
        if '"graph_get"' in body:
            st["graph_get"] += 1
            return r.continue_()
        if '"job_context"' in body:
            st["context"] += 1
            ctx = {"scan": {"dwell_ms": 85, "dwell_ms_asked": 20, "average": 1, "bits": 16,
                            "timing": {"sweep_ms": 100 + st["context"]}}}
            return r.fulfill(status=200, content_type="application/json",
                             body=json.dumps({"result": "context", "value": ctx}))
        if '"source_list"' not in body:
            return r.continue_()
        resp = r.fetch()
        data = resp.json()
        for src in data.get("value") or []:
            if src.get("id") == "tone":
                src.setdefault("capabilities", {})["depths"] = [
                    {"bits": 16, "lossless": True, "max_bandwidth_hz": {"measured": 20_480_000}},
                    {"bits": 8, "lossless": False, "max_bandwidth_hz": {"measured": 20_480_000}},
                ]
        return r.fulfill(response=resp, body=json.dumps(data))

    show_tone_waterfall(page, url, secret)
    page.route("**/op", route)
    wait_span(page, 1.5)
    drag_menu(page, 0.0, 1.05)
    page.click('#ctx .ctx-item[data-action="scan"]')
    scans = []
    try:
        # Route handlers run only while Playwright waits: poll through the page.
        for _ in range(50):
            scans = running_scan_jobs(url, secret, "tone")
            if scans:
                break
            page.wait_for_timeout(100)
        assert scans, "a scan job on tone within 5 s"
        row = page.locator(f'#jobs .job[data-job="{scans[0]["id"]}"]')
        select = row.locator("select.scan-look")
        expect(select).to_be_visible(timeout=10_000)
        expect(select.locator('option[value="coarse"]')).to_have_count(1)
        expect(select.locator('option[value="coarse"]')).to_have_attribute("title", re.compile("^8-bit samples"))
        # Route handlers run while Playwright waits: wait in the page for several polls.
        page.wait_for_timeout(7_000)
        assert st["context"] >= 3, f"job_context polled {st['context']} times"
        assert st["graph_get"] <= 1, f"graph_get re-read {st['graph_get']} times in the steady state"
        # A focused select keeps its place and the row's sweep time still follows the polls.
        select.focus()
        sweep = row.locator(".sweep-ms")
        first = sweep.text_content()
        page.wait_for_function(
            "([id, t]) => document.querySelector(`#jobs .job[data-job=\"${id}\"] .sweep-ms`)"
            ".textContent !== t", arg=[scans[0]["id"], first], timeout=8_000)
        assert page.evaluate("() => document.activeElement.className") == "scan-look"
        # A choice blurs it.
        select.select_option("long")
        page.wait_for_timeout(500)
        assert page.evaluate("() => document.activeElement.className") != "scan-look", \
            "the select kept the focus after a choice"
    finally:
        page.unroute("**/op", route)
        for j in running_scan_jobs(url, secret, "tone") or scans:
            stop_job(url, secret, j["id"])


def scenario_scan_of_a_sub_band_scans_just_that_band(page, url, secret):
    show_tone_waterfall(page, url, secret)
    wait_span(page, 1.5)  # drag_menu asserts at least 1 s
    # The upper fifth of the capture, off centre: one step of that width, kept inside the
    # recording (a capture-wide step centred there would leave it and fail the scan).
    drag_menu(page, 0.10, 0.30)
    footer = page.locator("#ctx-footer").text_content()
    bw = int(re.search(r"bandwidth_hz=(\d+)", footer).group(1))
    page.click('#ctx .ctx-item[data-action="scan"]')
    scans = []
    try:
        deadline = time.monotonic() + 5.0
        while not scans and time.monotonic() < deadline:
            scans = running_scan_jobs(url, secret, "tone")
            time.sleep(0.2)
        assert len(scans) == 1, f"scan jobs on tone: {scans}"
        assert scans[0]["tuning"]["bandwidth_hz"] == bw, f"scan tuning {scans[0]['tuning']}, drag {bw}"
        wait_waterfall_follows(page, f"w.jobId === {json.dumps(scans[0]['id'])}", "scan mode")
        before = page.evaluate("window.zetaTest.wf().painted")
        page.wait_for_function(f"() => window.zetaTest.wf().painted > {before + 3}", timeout=10_000)
        # Its rows cover the capture the file source answers with: the band widens to them.
        wait_ticks(page, "100.024", "99.976", "scan band widened to its rows")
        now = running_scan_jobs(url, secret, "tone")
        assert [j["id"] for j in now] == [scans[0]["id"]], f"the scan stopped: {now}"
    finally:
        for j in scans:
            stop_job(url, secret, j["id"])
    page.wait_for_function("() => window.zetaTest.waterfallMode === 'spectrum'", timeout=10_000)


def scenario_overlays_follow_a_zoom(page, url, secret):
    show_tone_waterfall(page, url, secret)
    page.wait_for_function("() => window.zetaTest.wf().painted > 5", timeout=15_000)
    wait_ticks(page, "100.024", "99.976", "before zooming")
    # A label 12 kHz above the centre: a quarter of the way down from the top.
    top = page.evaluate("""() => {
      const el = window.zetaTest.label(100012000, "probe");
      return parseFloat(el.style.top) + el.offsetHeight / 2;
    }""")
    h = page.evaluate("document.querySelector('#waterfall canvas.wf-canvas').clientHeight")
    assert abs(top - h / 4) < 2, f"label at {top}, want {h / 4}"
    # A wheel notch x2 in about the centre, and the label read in the same task: it is at the
    # top edge at once, not only when the next column scrolls the overlays.
    moved = page.evaluate("""() => {
      const o = document.querySelector('#waterfall canvas.wf-overlay');
      const r = o.getBoundingClientRect();
      o.dispatchEvent(new WheelEvent('wheel', {deltaY: -100, deltaMode: 0, bubbles: true,
        cancelable: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2}));
      const el = document.querySelector('.ov-label');
      return parseFloat(el.style.top) + el.offsetHeight / 2;
    }""")
    assert abs(moved) < 2, f"label at {moved} right after the zoom, want 0"
    wait_ticks(page, "100.012", "99.988", "after zooming in")


# ---- view policy: the page yields (plan 11 Task 5) ------------------------------------------
# `retune` is the tone recording at 250 MHz, tunable over 200-300 MHz: a tune moves its
# capture's centre.

def job_record(url, secret, job):
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    return next((j for j in jobs if j["id"] == job), None)


def source_info(url, secret, source):
    sources = http_json(url, secret, "/op", {"op": "source_list"})["value"]
    return next(s for s in sources if s["id"] == source)


def run_cmd(page, text):
    page.fill("#cmd", text)
    page.press("#cmd", "Enter")


def view_line(page, text, error=False):
    """The newest note line containing `text` (`.error` ones with `error`); the ops stream
    keeps adding lines, so the newest line overall may be another."""
    cls = "#transcript .line.note.error" if error else "#transcript .line.note:not(.error)"
    return page.locator(cls, has_text=text).last


def view_show(page):
    """Run `view show` and return its text once a new answer line is there."""
    before = page.locator("#transcript .line.note", has_text="(allowed").count()
    run_cmd(page, "view show")
    lines = page.locator("#transcript .line.note", has_text="(allowed")
    expect(lines).to_have_count(before + 1)
    return lines.last.text_content()


def current_capture(url, secret, source, within_s=10.0):
    """`source`'s current capture (source_list's `current`), waiting for one: right after a
    source is shown, the page's spectrum job may not have tuned it yet."""
    return wait_until(lambda: source_info(url, secret, source)["current"], within_s,
                      f"a capture on {source}")


def wait_until(pred, within_s, what, step_s=0.2):
    """Poll `pred()` until it returns a truthy value or `within_s` passes; that value."""
    deadline = time.monotonic() + within_s
    while True:
        v = pred()
        if v or time.monotonic() > deadline:
            assert v, f"{what} within {within_s} s"
            return v
        time.sleep(step_s)


def scenario_page_job_is_elastic_and_a_rigid_start_needs_no_replace(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    page.wait_for_function("() => window.zetaTest.wf() && window.zetaTest.wf().jobId")
    own = page.evaluate(WF_STATE_JS)["jobId"]
    rec = job_record(url, secret, own)
    assert rec and rec["kind"] == "spectrum" and rec["elastic"] is True, f"page job {rec}"
    # Its row is listed, marked elastic, with nothing but Stop.
    row = job_row(page, own)
    expect(row.locator(".kind")).to_have_text("spectrum elastic")
    expect(row.locator("button")).to_have_count(1)
    expect(row.locator("button.stop")).to_have_count(1)
    cur = current_capture(url, secret, "retune")["center_hz"]
    band = cur + 1_000_000 if cur + 1_000_000 < 299_000_000 else cur - 1_000_000
    r = http_json(url, secret, "/op", {"op": "job_start", "kind": "passthrough",
                                       "source": "retune", "center_hz": band,
                                       "bandwidth_hz": 10_000, "deviation_hz": None,
                                       "sideband": None, "elastic": False, "replace": []})
    assert r["result"] == "job", f"rigid start over the page's job: {r}"
    rigid = r["value"]
    try:
        def followed():
            src = source_info(url, secret, "retune")
            rec = job_record(url, secret, own)
            ok = (src["current"]["center_hz"] == band and rec
                  and rec["state"]["status"] == "running" and rec["tuning"] == src["current"])
            return (src, rec) if ok else None
        src, rec = wait_until(followed, 5.0, "the page's job follows the retune")
        assert src["holders"] == [rigid], f"holders {src['holders']}"
        assert own in src["elastic"], f"elastic {src['elastic']}"
        assert page.evaluate(WF_STATE_JS)["jobId"] == own, page.evaluate(WF_STATE_JS)
        # It keeps painting the new capture under the same job id.
        wait_waterfall_follows(page, f"w.jobId === {json.dumps(own)}", "after the relaunch")
        mid = f"{band / 1e6:.3f}"
        page.wait_for_function(
            f"() => ({TICKS_JS})()[2] === {json.dumps(mid)}", timeout=10_000)
    finally:
        stop_job(url, secret, rigid)


def scenario_stopping_the_page_job_starts_a_fresh_elastic_one(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    own = page.evaluate(WF_STATE_JS)["jobId"]
    job_row(page, own).locator("button.stop").click()
    wait_waterfall_follows(page, f"w.jobId && w.jobId !== {json.dumps(own)}", "fresh own job")
    fresh = page.evaluate(WF_STATE_JS)["jobId"]
    rec = job_record(url, secret, fresh)
    assert rec["elastic"] is True and rec["state"]["status"] == "running", f"fresh job {rec}"
    jobs = running_spectrum_jobs(url, secret, "retune")
    assert [j["id"] for j in jobs] == [fresh], f"spectrum jobs on retune: {jobs}"


def scenario_waterfall_reuses_an_elastic_spectrum_job_first(page, url, secret):
    # A rigid spectrum job (an older page's, persisted before elastic jobs) listed first on the
    # source: the waterfall reuses the elastic one beside it.
    fresh_spectrum_jobs(url, secret)
    rigid = start_job(url, secret, "spectrum", "aprs", 144_800_000, 48_000)
    try:
        elastic = http_json(url, secret, "/op", {
            "op": "job_start", "kind": "spectrum", "source": "aprs", "center_hz": 144_800_000,
            "bandwidth_hz": 48_000, "deviation_hz": None, "sideband": None, "elastic": True,
            "replace": []})["value"]
        page.set_viewport_size(WIDE)
        login(page, url, secret)
        page.click('.src-tab[data-id="aprs"]')
        page.wait_for_function(
            f"() => window.zetaTest.wf().jobId === {json.dumps(elastic)}", timeout=5_000)
    finally:
        stop_job(url, secret, rigid)
        fresh_spectrum_jobs(url, secret)


def scenario_theme_switch(page, url, secret):
    select_source(page, url, secret, "tone")
    expect(page.locator("html")).to_have_attribute("data-theme", "console")
    run_cmd(page, "view set theme plain")
    expect(view_line(page, "theme = plain")).to_be_visible()
    expect(page.locator("html")).to_have_attribute("data-theme", "plain")
    stored = page.evaluate("sessionStorage.getItem('zeta.view')")
    assert '"theme":"plain"' in stored, stored
    run_cmd(page, "view set theme neon")
    neon = view_line(page, "neon", error=True)
    for theme in ("console", "plain", "terminal", "fixture_theme"):
        expect(neon).to_contain_text(theme)
    run_cmd(page, "view reset")
    expect(page.locator("html")).to_have_attribute("data-theme", "console")
    # The mic ends the command bar with the elbow's cap; a finished row reads in the theme's text.
    radii = page.evaluate("(() => { const s = getComputedStyle(document.getElementById('mic'));"
                          " return [s.borderStartStartRadius, s.borderEndEndRadius]; })()")
    assert radii == ["14px", "22px"], radii
    colours = page.evaluate("(() => { const r = document.createElement('div');"
                            " r.className = 'job finished'; r.dataset.colour = '1';"
                            " document.getElementById('jobs').append(r);"
                            " const c = getComputedStyle(r).color; r.remove();"
                            " const t = document.createElement('span'); t.style.color = 'var(--text)';"
                            " document.body.append(t); const want = getComputedStyle(t).color;"
                            " t.remove(); return [c, want]; })()")
    assert colours[0] == colours[1], colours
    # Live jobs' tags move between the rail (console) and their outlines (plain), the squelch
    # dot with them, none left behind.
    job = start_job(url, secret, "passthrough", "tone", 100_000_000, 10_000)
    demod = None
    try:
        # Off the tone, so its squelch stays closed.
        demod = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500, deviation_hz=5000)
        page.evaluate("window.zetaTest.poll()")
        in_outline = page.locator(f'#waterfall .outline[data-key="{job}"] .job-tag')
        in_rail = page.locator(f'#rail .job-tag[data-job="{job}"]')
        dot_rail = page.locator(f'#rail .job-tag[data-job="{demod}"] .dot.closed')
        dot_outline = page.locator(f'#waterfall .outline[data-key="{demod}"] .job-tag .dot.closed')
        expect(in_rail).to_have_count(1)
        expect(dot_rail).to_have_count(1, timeout=5_000)
        run_cmd(page, "view set theme plain")
        expect(page.locator("html")).to_have_attribute("data-theme", "plain")
        expect(in_outline).to_have_count(1)
        expect(in_outline.locator(".kind")).to_have_text("passthrough")
        expect(dot_outline).to_have_count(1)
        expect(page.locator("#rail")).to_be_hidden()
        expect(page.locator("#rail .job-tag")).to_have_count(0)
        run_cmd(page, "view set theme console")
        expect(page.locator("html")).to_have_attribute("data-theme", "console")
        expect(in_rail).to_have_count(1)
        expect(in_outline).to_have_count(0)
        expect(in_rail.locator(".kind")).to_have_text("passthrough")
        expect(dot_rail).to_have_count(1)
        expect(page.locator("#waterfall .outline .job-tag")).to_have_count(0)
        for j in (job, demod):
            expect(page.locator(f'#waterfall .job-tag[data-job="{j}"]')).to_have_count(1)
    finally:
        for j in (job, demod):
            if j:
                stop_job(url, secret, j)


# Job ids differ from run to run, and so does each job's colour (overlays.js channelColour
# sums the id): the shots write every id, and its 6-character short form, as one fixed id and
# give every coloured element colour 1, so two runs of the same stylesheet match. A
# MutationObserver keeps it so through the page's own re-renders (it runs before each paint).
SHOT_STEADY_JS = """() => {
  if (window.__shotSteady) return;
  const ULID = /01[0-9A-HJKMNP-TV-Z]{24}/g;
  const FIXED = "01AAAAAAAAAAAAAAAAAAAAAAAA";
  const app = document.getElementById("app");
  const ids = new Set();
  const steady = () => {
    for (const el of app.querySelectorAll("[data-job], [data-key], [data-id]")) {
      for (const v of [el.dataset.job, el.dataset.key, el.dataset.id]) {
        if (v && /^01[0-9A-HJKMNP-TV-Z]{24}$/.test(v)) ids.add(v);
      }
    }
    const walker = document.createTreeWalker(app, NodeFilter.SHOW_TEXT);
    const texts = [];
    while (walker.nextNode()) texts.push(walker.currentNode);
    for (const t of texts) for (const m of t.data.match(ULID) || []) ids.add(m);
    const shorts = [...ids].map((id) => id.slice(-6));
    for (const t of texts) {
      let s = t.data.replace(ULID, FIXED);
      for (const short of shorts) s = s.split(short).join(FIXED.slice(-6));
      if (s !== t.data) t.data = s;
    }
    for (const el of app.querySelectorAll("[data-colour]")) {
      if (el.dataset.colour !== "1") el.dataset.colour = "1";
    }
  };
  steady();
  window.__shotSteady = new MutationObserver(steady);
  window.__shotSteady.observe(app, {subtree: true, childList: true, characterData: true,
                                    attributes: true, attributeFilter: ["data-colour"]});
}"""


def scenario_theme_shots(page, url, secret):
    """Screenshots of console and plain, wide and narrow, into ZETA_UI_SHOTS (nothing without
    it), for shotdiff.py to compare across a stylesheet move. The waterfall canvas, the status
    line and the live source numbers are masked: they change from run to run."""
    shots = os.environ.get("ZETA_UI_SHOTS")
    if not shots:
        return
    select_source(page, url, secret, "tone")
    demod = None
    try:
        # Off the tone, so its squelch stays closed and the tag reads the same every run.
        demod = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500, deviation_hz=5000)
        # The page's own spectrum job and the demodulator both listed, so every run shows the
        # same rows.
        spectrum = wait_one_spectrum_job(url, secret, "tone", within_s=10.0)
        assert len(spectrum) == 1, spectrum
        job_row(page, spectrum[0]["id"])
        job_row(page, demod)
        for theme in ("console", "plain"):
            for name, size in (("wide", WIDE), ("narrow", NARROW)):
                page.set_viewport_size(size)
                page.wait_for_function(
                    f"() => document.documentElement.dataset.footprint === '{name}'")
                run_cmd(page, f"view set theme {theme}")
                expect(page.locator("html")).to_have_attribute("data-theme", theme)
                expect(page.locator(f'#waterfall .job-tag[data-job="{demod}"] .dot.closed')
                       ).to_have_count(1, timeout=5_000)
                page.evaluate("document.activeElement && document.activeElement.blur()")
                page.wait_for_timeout(500)
                page.evaluate(SHOT_STEADY_JS)
                page.screenshot(
                    path=os.path.join(shots, f"{theme}-{name}.png"), animations="disabled",
                    mask=[page.locator(".wf-canvas"), page.locator("#status"),
                          page.locator(".src-tab .health")])
    finally:
        if demod:
            stop_job(url, secret, demod)


# The built-in themes added in plan 26 (Tasks 8-11), each checked by scenario_new_themes.
NEW_THEMES = ["cyberpunk", "terminal", "psytrance", "vibecoder"]

# The motion a theme may start, provoked and read in one synchronous step (a jobs poll would
# re-sync `.speaking` and cancel a hand-made glitch, and a short animation would finish while
# a test waited): the speaking state on a job tag, keyboard focus on a button and focus in the
# command line, reading document.getAnimations() after each (it flushes style, so transitions
# and animations have started). The names of what runs: CSS animations by animationName, CSS
# transitions as "transition:<property>", anything else by its constructor.
MOTION_PROBE_JS = """(job) => {
  const seen = new Set();
  const read = () => {
    for (const a of document.getAnimations()) {
      seen.add(a.animationName || (a.transitionProperty ? "transition:" + a.transitionProperty
                                                        : a.constructor.name));
    }
  };
  if (document.activeElement) document.activeElement.blur();
  getComputedStyle(document.body).color;
  const tag = document.querySelector(`#waterfall .job-tag[data-job="${job}"]`);
  tag.classList.add("speaking");
  read();
  document.getElementById("listen-all").focus({focusVisible: true});
  read();
  document.getElementById("cmd").focus();
  read();
  tag.classList.remove("speaking");
  document.activeElement.blur();
  return [...seen].sort();
}"""


# Colour helpers for the contrast checks. `rgba(c)` normalises any CSS colour the browser
# computes (rgb(), color(srgb ...) from a color-mix, ...) to [r, g, b, a] by painting it on a
# 1x1 canvas (a an alpha 0-1; a colour the canvas cannot parse reads as transparent).
# `ground(el)` is the [r, g, b] an element's text actually sits on: its own background colour
# and its ancestors', from the element up to the first opaque one, composited; a layer under
# 0.05 alpha counts as transparent. A stack that never reaches an opaque layer is composited
# over the canvas ground (the background of <html>, else <body>, else white), as the browser
# paints it. An element painted only with a background image (a gradient) has an unknown
# colour, so the walk goes on to the solid colour below it. `settle()` finishes the CSS
# transitions in flight, so a check reads the colours a state settles on, not a frame of a
# fade into them.
COLOUR_HELPERS_JS = """(() => {
  const x = document.createElement("canvas").getContext("2d", {willReadFrequently: true});
  const rgba = (c) => {
    x.clearRect(0, 0, 1, 1);
    x.fillStyle = "rgba(0, 0, 0, 0)";
    x.fillStyle = c;
    x.fillRect(0, 0, 1, 1);
    const d = x.getImageData(0, 0, 1, 1).data;
    return [d[0], d[1], d[2], Math.round(d[3] / 255 * 100) / 100];
  };
  const canvas = () => {
    for (const e of [document.documentElement, document.body]) {
      const c = rgba(getComputedStyle(e).backgroundColor);
      if (c[3] >= 0.95) return c.slice(0, 3);
    }
    return [255, 255, 255];
  };
  const ground = (el) => {
    const layers = [];
    for (let e = el; e; e = e.parentElement) {
      const c = rgba(getComputedStyle(e).backgroundColor);
      if (c[3] < 0.05) continue;
      layers.push(c);
      if (c[3] >= 0.95) break;
    }
    let out = layers.length && layers[layers.length - 1][3] >= 0.95
      ? layers.pop().slice(0, 3) : canvas();
    while (layers.length) {
      const [r, g, b, a] = layers.pop();
      out = [r, g, b].map((v, i) => Math.round(v * a + out[i] * (1 - a)));
    }
    return out;
  };
  const settle = () => {
    for (const a of document.getAnimations()) {
      if (a instanceof CSSTransition) a.finish();
    }
  };
  return {rgba, ground, settle};
})()"""

# A keyboard-focused unselected source tab: [its name's colour, the ground it sits on].
FOCUSED_TAB_JS = """() => {
  const {rgba, ground, settle} = """ + COLOUR_HELPERS_JS + """;
  const tab = document.querySelector('.src-tab:not([aria-selected="true"])');
  tab.focus({focusVisible: true});
  settle();
  const name = tab.querySelector(".name");
  const out = [rgba(getComputedStyle(name).color).slice(0, 3), ground(name)];
  tab.blur();
  return out;
}"""

# The hovered action button in a job's (hot) row: its text, the ground it sits on, its own
# background colour ([r, g, b, a]) and the row's ground.
HOT_ACTION_JS = """(job) => {
  const {rgba, ground, settle} = """ + COLOUR_HELPERS_JS + """;
  settle();
  const row = document.querySelector(`#jobs .job[data-id="${job}"]`);
  const button = row.querySelector(".actions button");
  return {text: rgba(getComputedStyle(button).color).slice(0, 3), ground: ground(button),
          own: rgba(getComputedStyle(button).backgroundColor), row: ground(row)};
}"""


def visible_controls(page, ids):
    """The ids among `ids` whose centre is not hit by the element itself or a child (covered
    or off screen)."""
    return page.evaluate("""(ids) => ids.filter((id) => {
      const el = document.getElementById(id);
      const r = el.getBoundingClientRect();
      if (!r.width || !r.height) return true;
      const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      return !(hit && el.contains(hit));
    })""", ids)


def scenario_new_themes(page, url, secret):
    """Each new built-in theme applies; it starts motion for a reader who has not asked for
    less (the probe can see it) and none for one who has, transitions included; its Listen to
    all (wide) and drawer toggle (narrow) stay visible and clickable; and, with ZETA_UI_SHOTS,
    it is shot wide and narrow as scenario_theme_shots does, the waterfall left unmasked so
    its ramp can be judged."""
    shots = os.environ.get("ZETA_UI_SHOTS")
    select_source(page, url, secret, "tone")
    demod = None
    still = page.context.browser.new_context(viewport=WIDE, reduced_motion="reduce")
    try:
        # Off the tone, so its squelch stays closed and the tag reads the same every run.
        demod = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500, deviation_hz=5000)
        job_row(page, demod)
        calm = still.new_page()
        login(calm, url, secret)
        calm.click('.src-tab[data-id="tone"]')
        for theme in NEW_THEMES:
            for p in (page, calm):
                run_cmd(p, f"view set theme {theme}")
                expect(p.locator("html")).to_have_attribute("data-theme", theme)
                p.evaluate("window.zetaTest.poll()")
                expect(p.locator(f'#waterfall .job-tag[data-job="{demod}"]')
                       ).to_have_count(1, timeout=5_000)
            moving = page.evaluate(MOTION_PROBE_JS, demod)
            assert moving, f"{theme}: the motion probe saw nothing on a normal-motion page"
            still_running = calm.evaluate(MOTION_PROBE_JS, demod)
            assert still_running == [], \
                f"{theme}: motion under reduced motion: {still_running} (normal: {moving})"
            # Focus and hover never draw text on its own colour: a keyboard-focused unselected
            # source tab's name and a hovered action in the hot job row, each against the
            # ground it actually sits on (its own and its ancestors' backgrounds, composited).
            # A hovered action with a ground of its own also stands out of the hot row's.
            focused = page.evaluate(FOCUSED_TAB_JS)
            assert focused[0] != focused[1], f"{theme}: focused tab's name on its own colour: {focused}"
            job_row(page, demod).locator(".actions button").first.hover()
            expect(job_row(page, demod)).to_have_class(re.compile(r"\bhot\b"))
            hovered = page.evaluate(HOT_ACTION_JS, demod)
            assert hovered["text"] != hovered["ground"], \
                f"{theme}: hovered action in the hot row: {hovered}"
            if hovered["own"][3] >= 0.05:
                assert hovered["own"][:3] != hovered["row"], \
                    f"{theme}: hovered action melts into the hot row: {hovered}"
            page.mouse.move(0, 0)
            for name, size in (("wide", WIDE), ("narrow", NARROW)):
                page.set_viewport_size(size)
                page.wait_for_function(
                    f"() => document.documentElement.dataset.footprint === '{name}'")
                controls = ["listen-all"] if name == "wide" else ["listen-all", "drawer-toggle"]
                expect(page.locator("#listen-all")).to_be_visible()
                hidden = visible_controls(page, controls)
                assert hidden == [], f"{theme} {name}: covered or off screen: {hidden}"
                if not shots:
                    continue
                expect(page.locator(f'#waterfall .job-tag[data-job="{demod}"] .dot.closed')
                       ).to_have_count(1, timeout=5_000)
                page.evaluate("document.activeElement && document.activeElement.blur()")
                # A ribbon in the job's colour, and time for the waterfall to fill a little.
                page.evaluate(f"window.zetaTest.ribbon(100015000, 'copy, signal clear', "
                              f"'transcript', null, '{demod}')")
                page.wait_for_timeout(2_500)
                page.evaluate(SHOT_STEADY_JS)
                page.screenshot(
                    path=os.path.join(shots, f"{theme}-{name}.png"), animations="disabled",
                    mask=[page.locator("#status"), page.locator(".src-tab .health")])
            page.set_viewport_size(WIDE)
    finally:
        still.close()
        run_cmd(page, "view reset")
        if demod:
            stop_job(url, secret, demod)


# The right-most 10 columns of the waterfall hold the fixture theme's ramp (black to magenta):
# no pixel greener than it is red, and the tone well on the red side.
MAGENTA_AT_RIGHT_JS = """() => {
  const c = document.querySelector('#waterfall canvas.wf-canvas');
  if (!c || c.width < 10 || c.height < 1) return false;
  const d = c.getContext('2d').getImageData(c.width - 10, 0, 10, c.height).data;
  let best = 0;
  for (let i = 0; i < d.length; i += 4) {
    if (d[i + 1] > d[i]) return false;
    best = Math.max(best, d[i] - d[i + 1]);
  }
  return best > 100;
}"""


# Whether the left-most backing-pixel column of the waterfall canvas (no data scrolled to it
# yet) holds the ground colour `arg` = [r, g, b].
GROUND_AT_LEFT_JS = """(want) => {
  const c = document.querySelector('#waterfall canvas.wf-canvas');
  const d = c.getContext('2d').getImageData(0, 0, 1, c.height).data;
  for (let i = 0; i < d.length; i += 4) {
    if (d[i] === want[0] && d[i + 1] === want[1] && d[i + 2] === want[2]) return true;
  }
  return false;
}"""


def scenario_operator_theme(page, url, secret):
    """An operator theme from --themes-dir (ui.rs fake_themes) applies its tokens, its layout
    (no rail) and its waterfall ramp; a broken folder's id is not offered."""
    select_source(page, url, secret, "tone")
    try:
        run_cmd(page, "view set theme fixture_theme")
        expect(view_line(page, "theme = fixture_theme")).to_be_visible()
        html = page.locator("html")
        expect(html).to_have_attribute("data-theme", "fixture_theme")
        expect(html).to_have_attribute("data-rail", "off")
        page.wait_for_function(
            "() => getComputedStyle(document.documentElement)"
            ".getPropertyValue('--accent').trim() === '#ff00ff'")
        page.wait_for_function(MAGENTA_AT_RIGHT_JS, timeout=5_000)
        # --wf-bg in any CSS colour paints the canvas ground; a value that is no colour falls
        # back to #000030, not to the previous theme's ground.
        run_cmd(page, "view set theme hsl_bg")
        expect(html).to_have_attribute("data-theme", "hsl_bg")
        page.wait_for_function(GROUND_AT_LEFT_JS, arg=[0, 128, 0], timeout=5_000)
        run_cmd(page, "view set theme bad_bg")
        expect(html).to_have_attribute("data-theme", "bad_bg")
        page.wait_for_function(GROUND_AT_LEFT_JS, arg=[0, 0, 48], timeout=5_000)
        run_cmd(page, "view set theme console@broken")
        expect(view_line(page, "console@broken", error=True)).to_be_visible()
        run_cmd(page, "view set theme neon")
        expect(view_line(page, "neon", error=True)).to_contain_text("fixture_theme")
        expect(html).to_have_attribute("data-theme", "bad_bg")
    finally:
        run_cmd(page, "view reset")


def scenario_theme_gone_falls_back(page, url, secret):
    """A stored theme that is no longer there falls back to console, saying so once."""
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    page.evaluate("sessionStorage.setItem('zeta.view', JSON.stringify({theme: 'gone'}))")
    page.reload()
    expect(page.locator("#login")).to_be_hidden()
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    line = page.locator("#transcript .line", has_text="theme gone is not available; using console")
    expect(line).to_have_count(1)
    html = page.locator("html")
    expect(html).to_have_attribute("data-theme", "console")
    expect(html).to_have_attribute("data-rail", "on")
    page.wait_for_function(
        "() => !(sessionStorage.getItem('zeta.view') || '').includes('gone')")
    # The themes list has answered too (fixture_theme is offered), and said nothing more.
    page.wait_for_function("() => window.zetaTest.themeIds().includes('fixture_theme')")
    expect(line).to_have_count(1)
    run_cmd(page, "view set theme plain")
    expect(html).to_have_attribute("data-theme", "plain")
    run_cmd(page, "view reset")


def scenario_theme_switch_live(page, url, secret):
    """A switch console -> plain -> console moves live tags without losing their state, and
    an open graph view redraws in the new theme."""
    select_source(page, url, secret, "tone")
    demod = None
    try:
        # Off the tone, so its squelch stays closed.
        demod = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500, deviation_hz=5000)
        row = job_row(page, demod)
        row.dispatch_event("pointerenter")
        in_rail = page.locator(f'#rail .job-tag[data-job="{demod}"]')
        in_outline = page.locator(f'#waterfall .outline[data-key="{demod}"] .job-tag')
        expect(in_rail.locator(".dot.closed")).to_have_count(1, timeout=5_000)
        expect(in_rail).to_have_class(re.compile(r"\bhot\b"))
        in_rail.evaluate("t => t.classList.add('speaking')")
        html = page.locator("html")
        for theme, tag in (("plain", in_outline), ("console", in_rail)):
            run_cmd(page, f"view set theme {theme}")
            expect(html).to_have_attribute("data-theme", theme)
            expect(tag).to_have_count(1)
            expect(tag).to_have_class(re.compile(r"\bhot\b"))
            expect(tag).to_have_class(re.compile(r"\bspeaking\b"))
            expect(tag.locator(".dot.closed")).to_have_count(1)
        modal = open_graph(page, demod)
        rect = modal.locator('svg .graph-node[data-kind="nbfm_demod"] rect')
        expect(rect).to_have_count(1)
        look = "r => [getComputedStyle(r).fill, r.getAttribute('rx')]"
        console_look = rect.evaluate(look)
        run_cmd(page, "view set theme plain")
        expect(html).to_have_attribute("data-theme", "plain")
        page.wait_for_function(
            "() => document.querySelector("
            "'#graph svg .graph-node[data-kind=\"nbfm_demod\"] rect')?.getAttribute('rx') === '3'")
        plain_look = rect.evaluate(look)
        assert plain_look[0] != console_look[0], (console_look, plain_look)
        run_cmd(page, "view set theme console")
        expect(html).to_have_attribute("data-theme", "console")
        page.wait_for_function(
            "() => document.querySelector("
            "'#graph svg .graph-node[data-kind=\"nbfm_demod\"] rect')?.getAttribute('rx') === '14'")
        assert rect.evaluate(look) == console_look, (console_look, rect.evaluate(look))
        page.keyboard.press("Escape")
    finally:
        run_cmd(page, "view reset")
        if demod:
            stop_job(url, secret, demod)


# The selectors docs/themes.md promises a theme may target (spec 5). Each must match on a page
# with a running demodulator, a ribbon, a collapsed pane, a pane docked right, the map and the
# graph view open, and each must appear verbatim in the docs.
THEME_HOOKS = [
    "#top", ".brand", ".src-tab", '.src-tab[aria-selected="true"]', "#rail", ".wf-axis",
    "#waterfall", ".wf-canvas", "#side", "section.pane", ".pane-head",
    "section.pane[data-collapsed]", "#panes", "#bar", "#cmd", "#mic", "#map", "#graph",
    '[role="menu"]', ".ctx-item", ".job", ".job .kind", ".job .band", '.job[data-state="running"]',
    ".job-tag", ".job-tag .kind", ".job-tag .channel", ".job-tag .centre", ".ov-ribbon",
    ".outline", ".graph-node.selected", ".job-tag.hot", ".job-tag.speaking",
    ".job-tag .dot.closed", "html[data-theme]", "html[data-rail]", "html[data-footprint]",
    ".map-tab", "html[data-main]", "html[data-dock]", "section.pane.dock",
    ".src-tab.default", ".src-tab .default-mark",
    "html[data-map-tiles]", "#map .map-tile-note",
]


def scenario_theme_hooks(page, url, secret):
    """Every hook the theme docs list exists in the live page and is named in docs/themes.md."""
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.dirname(os.path.abspath(__file__))))))
    with open(os.path.join(root, "docs", "themes.md"), encoding="utf-8") as f:
        docs = f.read()
    missing = [h for h in THEME_HOOKS if f"`{h}`" not in docs]
    assert not missing, f"docs/themes.md does not list: {missing}"
    page.set_viewport_size(WIDE)
    select_source(page, url, secret, "tone")
    demod = None
    try:
        # Off the tone, so its squelch stays closed (the idle state hook).
        demod = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500, deviation_hz=5000)
        row = job_row(page, demod)
        page.evaluate("window.zetaTest.ribbon('hooks')")
        run_cmd(page, "view set main map")
        expect(page.locator("#map")).to_be_visible()
        run_cmd(page, "view set dock right")
        run_cmd(page, "view set pane explore")
        expect(page.locator("#side section.pane.dock")).to_have_count(1)
        page.click("#pane-transcript .pane-head")
        tag = page.locator(f'#rail .job-tag[data-job="{demod}"]')
        expect(tag.locator(".dot.closed")).to_have_count(1, timeout=5_000)
        open_graph(page, demod)
        page.locator("#graph .graph-node").first.click()
        row.dispatch_event("pointerenter")
        expect(tag).to_have_class(re.compile(r"\bhot\b"))
        tag.evaluate("t => t.classList.add('speaking')")
        for hook in THEME_HOOKS:
            expect(page.locator(hook).first, hook).to_be_attached()
        page.keyboard.press("Escape")
    finally:
        run_cmd(page, "view reset")
        if demod:
            stop_job(url, secret, demod)


def scenario_source_bar_shows_live_numbers(page, url, secret):
    select_source(page, url, secret, "tone")
    tab = page.locator('.src-tab[aria-selected="true"]')
    health = tab.locator(".health")
    expect(health.locator('.v[data-field="hold"]')).to_have_text(re.compile(r"^\d+$"))
    expect(health.locator('.v[data-field="bank"]')).to_have_text(re.compile(r"^(direct|\d+)$"))
    expect(health.locator('.v[data-field="gain"]')).to_have_text(
        re.compile(r"^(auto|-?\d+(\.\d+)? dB|—)$"))
    assert "hold" in (tab.get_attribute("title") or "")
    run_cmd(page, "view set theme plain")
    expect(health).to_be_hidden()


def scenario_view_set_show_reset(page, url, secret):
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    run_cmd(page, "view set follow free")
    expect(view_line(page, "follow = free")).to_have_text("follow = free")
    shown = view_show(page)
    for text in ("follow free", "zoom_out scan", "auto_scan_stop inside",
                 "finished_rows_s 60", "scanner auto", "plan auto", "main rf", "map auto",
                 "home none",
                 "subjects auto", "subject_dim_s 60", "subject_drop_s 300",
                 "origin_fade_s 300", "pane none", "theme console", "side both", "dock auto",
                 "map_tiles auto", "radio_policy focus", "radio_prefer none", "video smooth"):
        assert text in shown, f"{text!r} not in view show: {shown!r}"
    assert shown.count("\n") == 20, f"view show: {shown!r}"
    run_cmd(page, "view set follow sideways")
    expect(view_line(page, "sideways", error=True)).to_contain_text("target, free")
    run_cmd(page, "view set finished_rows_s 5")
    expect(view_line(page, "finished_rows_s = 5")).to_be_visible()
    run_cmd(page, "view set finished_rows_s -1")
    expect(view_line(page, '"-1"', error=True)).to_be_visible()
    run_cmd(page, "view set nosuch 1")
    expect(view_line(page, "nosuch", error=True)).to_contain_text(
        "follow, zoom_out, auto_scan_stop, finished_rows_s, scanner, plan, main, map, home, "
        "subjects, subject_dim_s, subject_drop_s")
    run_cmd(page, "view reset")
    expect(view_line(page, "view policies reset")).to_be_visible()
    shown = view_show(page)
    assert "follow target" in shown and "finished_rows_s 60" in shown, shown
    # Nothing went to /ask.
    expect(page.locator("#transcript .line.assistant")).to_have_count(0)
    # Ruling R24: other text starting "view " is a question for the agent, not a view command.
    run_cmd(page, "view the strongest signal")
    expect(page.locator("#transcript .line.assistant")).to_have_count(1)
    expect(page.locator("#transcript .line", has_text='view: expected')).to_have_count(0)
    # The values live in sessionStorage and survive a reload.
    run_cmd(page, "view set scanner aprs")
    expect(view_line(page, "scanner = aprs")).to_be_visible()
    page.reload()
    expect(page.locator("#login")).to_be_hidden()
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    assert "scanner aprs" in view_show(page)
    stored = page.evaluate("JSON.parse(sessionStorage.getItem('zeta.view'))")
    assert stored.get("scanner") == "aprs", f"zeta.view {stored}"


def scenario_finished_rows_age_out(page, url, secret):
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set finished_rows_s 1")
    expect(view_line(page, "finished_rows_s = 1")).to_be_visible()
    job = start_job(url, secret, "passthrough", "tone", 100_000_000, 10_000)
    job_row(page, job)
    stop_job(url, secret, job)
    page.evaluate("window.zetaTest.poll()")
    row = page.locator(f'#jobs .job[data-id="{job}"]')
    expect(row.locator(".reason")).to_have_text("stopped")
    expect(row.locator("button.stop")).to_have_count(0)
    expect(row).to_have_count(0, timeout=3_000)
    # 0: a stopped job is never listed.
    run_cmd(page, "view set finished_rows_s 0")
    job = start_job(url, secret, "passthrough", "tone", 100_000_000, 10_000)
    stop_job(url, secret, job)
    for _ in range(5):
        page.evaluate("window.zetaTest.poll()")
        page.wait_for_timeout(300)
        expect(page.locator(f'#jobs .job[data-id="{job}"]')).to_have_count(0)
    # view clear drops the finished rows at once.
    run_cmd(page, "view set finished_rows_s 60")
    job = start_job(url, secret, "passthrough", "tone", 100_000_000, 10_000)
    stop_job(url, secret, job)
    page.evaluate("window.zetaTest.poll()")
    expect(page.locator(f'#jobs .job[data-id="{job}"] .reason')).to_have_text("stopped")
    run_cmd(page, "view clear")
    expect(page.locator(f'#jobs .job[data-id="{job}"]')).to_have_count(0)
    page.evaluate("window.zetaTest.poll()")
    page.wait_for_timeout(300)
    expect(page.locator(f'#jobs .job[data-id="{job}"]')).to_have_count(0)


def scenario_conflict_offers_replace(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    a = start_job(url, secret, "passthrough", "retune", 250_000_000, 10_000)
    new = None
    try:
        op = {"op": "job_start", "kind": "passthrough", "source": "retune",
              "center_hz": 251_000_000, "bandwidth_hz": 10_000, "deviation_hz": None,
              "sideband": None, "elastic": False, "replace": []}
        run_cmd(page, json.dumps(op))
        line = page.locator("#transcript .line.error",
                            has_text=re.compile(rf"conflict.*held by passthrough {a} @ 250\.000"))
        expect(line).to_have_count(1)
        button = line.locator("button", has_text=f"stop passthrough {a[-6:]} and retry")
        expect(button).to_have_count(1)
        button.click()

        def handed_over():
            jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
            mine = next(j for j in jobs if j["id"] == a)
            news = [j for j in jobs if j["kind"] == "passthrough" and j["source"] == "retune"
                    and j["state"]["status"] == "running"
                    and j["tuning"]["center_hz"] == 251_000_000]
            return news[0]["id"] if mine["state"]["status"] == "stopped" and news else None
        new = wait_until(handed_over, 5.0, "A stopped and the new job running")
        cur = current_capture(url, secret, "retune")
        assert cur["center_hz"] == 251_000_000, f"current {cur}"
        expect(page.locator("#transcript .line.op",
                            has_text=re.compile(rf"replace=… → job {new}"))).to_have_count(1)
    finally:
        stop_job(url, secret, a)
        if new:
            stop_job(url, secret, new)


# ---- pins and gain from the command line (plan 12 Task 7) ------------------------------------

def try_op(url, secret, body):
    """POST `body` to /op; the decoded reply, or the decoded error JSON of a 4xx answer."""
    try:
        return http_json(url, secret, "/op", body)
    except urllib.error.HTTPError as e:
        return json.loads(e.read())


def scenario_source_pin_unpin_and_gain_commands(page, url, secret):
    select_source(page, url, secret, "retune")
    tab = page.locator('.src-tab[data-id="retune"]')
    cur = wait_until(lambda: source_info(url, secret, "retune")["current"], 5.0,
                     "retune has a capture")
    other = 260_000_000 if abs(cur["center_hz"] - 260_000_000) > 2_000_000 else 240_000_000
    job = None
    try:
        run_cmd(page, "source pin")
        expect(page.locator("#transcript .line.note:not(.error)", has_text=(
            f"pinned retune at {cur['center_hz']} Hz, {cur['bandwidth_hz']} Hz"))).to_have_count(1)
        assert source_info(url, secret, "retune")["pin"] is not None, "no pin after source pin"
        page.evaluate("window.zetaTest.poll()")
        expect(tab.locator(".tag.pin")).to_have_count(1)
        expect(page.locator('#waterfall .outline[data-key="pin"]')).to_be_visible()
        expect(page.locator('#waterfall .outline[data-key="pin"] .outline-label')
               ).to_have_text("pin")
        expect(page.locator('#waterfall .outline[data-key="pin"] .job-tag')).to_have_count(0)
        expect(page.locator('#waterfall .outline[data-key="pin"] .outline-handle')
               ).to_have_count(0)
        expect(page.locator('#waterfall .outline[data-key="capture"]')).to_have_count(0)
        # A rigid start elsewhere: the pin holds the source, and nobody else does.
        r = try_op(url, secret, {"op": "job_start", "kind": "passthrough", "source": "retune",
                                 "center_hz": other, "bandwidth_hz": 10_000,
                                 "deviation_hz": None, "sideband": None, "elastic": False,
                                 "replace": []})
        assert r.get("error") == "conflict", f"job_start on the pinned source: {r}"
        line = page.locator("#transcript .line.error", has_text="held by nobody").filter(
            has_text="unpin source retune (source_unpin)")
        expect(line).to_have_count(1)
        button = line.locator("button", has_text="unpin retune and retry")
        expect(button).to_have_count(1)
        button.click()

        def started():
            jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
            run = [j for j in jobs if j["kind"] == "passthrough" and j["source"] == "retune"
                   and j["state"]["status"] == "running" and j["tuning"]["center_hz"] == other]
            return run[0]["id"] if run else None
        job = wait_until(started, 3.0, "the retried job running")
        assert source_info(url, secret, "retune")["pin"] is None, "still pinned after unpin"
        expect(tab.locator(".tag.pin")).to_have_count(0, timeout=3_000)
        # A recording has no adjustable gain: the op's error text.
        run_cmd(page, "source gain 20")
        expect(page.locator("#transcript .line.note.error",
                            has_text="source has no adjustable gain")).to_have_count(1)
        run_cmd(page, "source pin 999")
        expect(page.locator("#transcript .line.note.error",
                            has_text="source pin: expected <center_hz> <bandwidth_hz>")
               ).to_have_count(1)
        # Any other text starting "source " is a question for the agent.
        run_cmd(page, "source of the noise")
        expect(page.locator("#transcript .line.assistant")).to_have_count(1)
    finally:
        if job:
            stop_job(url, secret, job)
        try_op(url, secret, {"op": "source_unpin", "source": "retune"})


# ---- automatic scans on the scanner source (plan 11 Task 6) ---------------------------------
# A view past the fine capture starts the page's own scan on a free source (`scanner`, the only
# free source whose range holds 100 MHz); a scan the page did not start is never touched.

def zoom_tone_out(page, url, secret):
    """Show `tone` and zoom x2 out past its capture (the caller stopped stray scans)."""
    show_tone_waterfall(page, url, secret)
    page.wait_for_function("() => window.zetaTest.wf().painted > 5")
    wait_ticks(page, "100.024", "99.976", "before zooming")
    wheel_at_centre(page, 100)
    wait_ticks(page, "100.048", "99.952", "the view leaves the capture")


def zoom_tone_in(page):
    wheel_at_centre(page, -100)
    wait_ticks(page, "100.024", "99.976", "back inside the capture")


def stopped(url, secret, job):
    rec = job_record(url, secret, job)
    return rec is not None and rec["state"]["status"] != "running"


def scenario_zoom_out_starts_and_stops_an_auto_scan(page, url, secret):
    stop_scans(url, secret)
    try:
        zoom_tone_out(page, url, secret)
        scans = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                           "an automatic scan on scanner")
        assert len(scans) == 1, f"scans on scanner: {scans}"
        own = scans[0]
        bw = own["tuning"]["bandwidth_hz"]
        assert abs(bw - 96_000) <= 96_000 * 0.05, f"scan tuning {own['tuning']}, view 96 kHz"
        assert abs(own["tuning"]["center_hz"] - 100_000_000) <= 2_000, own["tuning"]
        assert page.evaluate(AUTO_SCAN_JS) == own["id"], page.evaluate(AUTO_SCAN_JS)
        # The view left the capture without a retune: the shown source keeps its own job.
        assert ensure_lines(page) == 0, "a source_ensure was issued for the coarse view"
        assert running_scan_jobs(url, secret, "tone") == [], "a scan on the shown source"
        # The coarse layer follows the scan's frames.
        page.wait_for_function(
            f"() => window.zetaTest.coarse().scanJobId === {json.dumps(own['id'])}"
            " && window.zetaTest.coarse().rows > 0", timeout=5_000)
        zoom_tone_in(page)
        wait_until(lambda: stopped(url, secret, own["id"]), 3.0, "the automatic scan stopped")
        assert running_scan_jobs(url, secret, "scanner") == [], "a scan still runs on scanner"
    finally:
        stop_scans(url, secret)


def scenario_manual_scan_is_left_alone(page, url, secret):
    stop_scans(url, secret)
    manual = start_job(url, secret, "scan", "scanner", 100_000_000, 96_000)
    try:
        zoom_tone_out(page, url, secret)
        # It is shown (followed read-only), and nothing else starts.
        page.wait_for_function(
            f"() => window.zetaTest.coarse().followed === {json.dumps(manual)}", timeout=5_000)
        page.wait_for_timeout(1_000)
        assert [j["id"] for j in running_scan_jobs(url, secret, "scanner")] == [manual]
        assert running_scan_jobs(url, secret, "tone") == [], "a scan on the shown source"
        assert page.evaluate(AUTO_SCAN_JS) is None, page.evaluate(AUTO_SCAN_JS)
        zoom_tone_in(page)
        page.wait_for_timeout(1_000)
        rec = job_record(url, secret, manual)
        assert rec["state"]["status"] == "running", f"manual scan after zooming in: {rec}"
    finally:
        stop_job(url, secret, manual)
        stop_scans(url, secret)


def scenario_scanner_policy_pins_source(page, url, secret):
    try:
        # Pinned to aprs (whose range is nowhere near the view): no scan, and auto's choice
        # (scanner) is not used instead.
        stop_scans(url, secret)
        show_tone_waterfall(page, url, secret)
        run_cmd(page, "view set scanner aprs")
        expect(view_line(page, "scanner = aprs")).to_be_visible()
        page.wait_for_function("() => window.zetaTest.wf().painted > 5")
        wheel_at_centre(page, 100)
        wait_ticks(page, "100.048", "99.952", "the view leaves the capture")
        expect(page.locator("#transcript .line", has_text="no free source can scan")).to_have_count(1)
        page.wait_for_timeout(700)
        assert running_scan_jobs(url, secret, "scanner") == [], "auto's scanner used under a pin"
        # Pinned to scanner: the view, still outside, is scanned there.
        run_cmd(page, "view set scanner scanner")
        scans = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                           "a scan on the pinned scanner")
        assert page.evaluate(AUTO_SCAN_JS) == scans[0]["id"]
        zoom_tone_in(page)
        wait_until(lambda: stopped(url, secret, scans[0]["id"]), 3.0, "the scan stopped")
        # zoom_out none: zooming out starts nothing.
        run_cmd(page, "view set zoom_out none")
        expect(view_line(page, "zoom_out = none")).to_be_visible()
        wheel_at_centre(page, 100)
        expect(page.locator("#transcript .line", has_text="out_of_range").first).to_be_visible()
        page.wait_for_timeout(1_000)
        assert running_scan_jobs(url, secret, "scanner") == [], "a scan under zoom_out none"
    finally:
        stop_scans(url, secret)


def scenario_own_scan_row_is_marked(page, url, secret):
    stop_scans(url, secret)
    manual = start_job(url, secret, "scan", "retune", 250_000_000, 96_000)
    try:
        zoom_tone_out(page, url, secret)
        own = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                         "an automatic scan on scanner")[0]["id"]
        expect(job_row(page, own).locator(".kind")).to_have_text("scan auto")
        manual_row = job_row(page, manual)
        expect(manual_row).to_have_attribute("data-state", "running")
        expect(manual_row.locator(".kind")).to_have_text("scan")
    finally:
        stop_job(url, secret, manual)
        stop_scans(url, secret)


def scenario_zoom_out_beyond_scanner_range_starts_nothing(page, url, secret):
    try:
        stop_scans(url, secret)
        show_tone_waterfall(page, url, secret)
        page.wait_for_function("() => window.zetaTest.wf().painted > 5")
        # 174-176 MHz: above scanner's 50-150 MHz, below retune's 200-300 MHz, far from tone.
        page.fill('#band input[name="center"]', "175")
        page.fill('#band input[name="width"]', "2000")
        page.click("#band button")
        wait_ticks(page, "176.000", "174.000", "the view far above the capture")
        line = page.locator("#transcript .line", has_text="no free source can scan")
        expect(line).to_have_count(1)
        expect(line).to_contain_text("174.000-176.000 MHz")
        page.wait_for_timeout(1_500)
        expect(line).to_have_count(1)
        jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
        scans = [j for j in jobs if j["kind"] == "scan" and j["state"]["status"] == "running"]
        assert scans == [], f"scans started: {scans}"
        assert page.evaluate(AUTO_SCAN_JS) is None
        assert ensure_lines(page) == 0, "a source_ensure was issued for the coarse view"
        # The coarse layer stays empty: nothing followed, no frame handed on.
        coarse = page.evaluate("window.zetaTest.coarse()")
        assert coarse["followed"] is None and coarse["rows"] == 0, f"coarse layer {coarse}"
    finally:
        stop_scans(url, secret)


def scenario_failed_auto_scan_stop_is_retried(page, url, secret):
    # A job_stop of the page's own scan that fails keeps the scan the page's own: the id stays
    # in zeta.autoScan and the stop is retried until it lands.
    stop_scans(url, secret)
    failing = {"on": True, "seen": 0}

    def route(r):
        body = r.request.post_data or ""
        if failing["on"] and '"job_stop"' in body:
            failing["seen"] += 1
            r.abort()
        else:
            r.continue_()
    try:
        zoom_tone_out(page, url, secret)
        own = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                         "an automatic scan on scanner")[0]["id"]
        page.route("**/op", route)
        zoom_tone_in(page)
        # Waited on through the page: the sync API runs route handlers only inside its calls.
        for _ in range(30):
            if failing["seen"]:
                break
            page.wait_for_timeout(100)
        assert failing["seen"], "the page did not try to stop its scan within 3 s"
        page.wait_for_timeout(500)
        assert job_record(url, secret, own)["state"]["status"] == "running"
        assert page.evaluate(AUTO_SCAN_JS) == own, "the own scan's id was dropped on a failed stop"
        assert page.evaluate("window.zetaTest.coarse().scanJobId") == own
        tries = failing["seen"]
        failing["on"] = False
        page.wait_for_function("() => sessionStorage.getItem('zeta.autoScan') === null",
                               timeout=5_000)
        assert stopped(url, secret, own), f"own scan after the retried stop: {job_record(url, secret, own)}"
        assert tries >= 1, tries
        assert running_scan_jobs(url, secret, "scanner") == [], "a scan still runs on scanner"
    finally:
        failing["on"] = False
        stop_scans(url, secret)


def scenario_auto_scan_is_followed_after_failed_stream_lookups(page, url, secret):
    # While GET /streams fails the coarse layer cannot find its scan's stream: once its lookups
    # give up it follows nothing, and a later decision follows the scan when /streams answers.
    stop_scans(url, secret)
    try:
        show_tone_waterfall(page, url, secret)
        page.wait_for_function("() => window.zetaTest.wf().painted > 5")
        page.route("**/streams", lambda r: r.abort())
        wheel_at_centre(page, 100)
        own = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                         "an automatic scan on scanner")[0]["id"]
        page.wait_for_function(
            f"() => window.zetaTest.coarse().scanJobId === {json.dumps(own)}"
            " && window.zetaTest.coarse().followed === null", timeout=10_000)
        page.unroute("**/streams")
        page.wait_for_function(
            f"() => window.zetaTest.coarse().followed === {json.dumps(own)}"
            " && window.zetaTest.coarse().rows > 0", timeout=10_000)
    finally:
        page.unroute("**/streams")
        stop_scans(url, secret)


def scenario_capturable_view_on_unheld_source_retunes_under_zoom_out_scan(page, url, secret):
    # Ruling R6: under the default zoom_out scan, a view past the capture that the unheld shown
    # source can capture (no wider than max_bandwidth_hz, inside its tune range) retunes it
    # with source_ensure; the coarse layer starts nothing.
    stop_scans(url, secret)
    try:
        select_source(page, url, secret, "retune")
        page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
        cur = current_capture(url, secret, "retune")["center_hz"]
        want = cur + 100_000 if cur + 100_000 < 299_000_000 else cur - 100_000
        page.fill('#band input[name="center"]', f"{want / 1e6:.6f}")
        page.fill('#band input[name="width"]', "24")
        page.click("#band button")
        expect(page.locator("#transcript .line", has_text="source_ensure").first).to_be_visible()
        wait_until(lambda: source_info(url, secret, "retune")["current"]["center_hz"] == want,
                   5.0, f"retune's capture moved to {want}")
        mid = f"{want / 1e6:.3f}"
        page.wait_for_function(f"() => ({TICKS_JS})()[2] === {json.dumps(mid)}", timeout=10_000)
        page.wait_for_timeout(1_000)
        jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
        scans = [j for j in jobs if j["kind"] == "scan" and j["state"]["status"] == "running"]
        assert scans == [], f"scans started: {scans}"
        assert page.evaluate("window.zetaTest.coarse().scanJobId") is None
        assert page.evaluate(AUTO_SCAN_JS) is None
    finally:
        stop_scans(url, secret)


def set_band(page, center_hz, width_hz):
    page.fill('#band input[name="center"]', f"{center_hz / 1e6:.6f}")
    page.fill('#band input[name="width"]', f"{width_hz / 1e3:.3f}")
    page.click("#band button")


def scenario_zoom_in_stops_the_shown_source_scan(page, url, secret):
    # Ruling R23: `retune` is the only source whose range holds its view, so a view wider than
    # its 48 kHz max bandwidth is scanned on retune itself (the shown-source fallback). Zoomed
    # back in to a band it can capture, that scan stops and the page's elastic job returns.
    stop_scans(url, secret)
    try:
        select_source(page, url, secret, "retune")
        page.wait_for_function("() => window.zetaTest.wf().painted > 5", timeout=10_000)
        cur = current_capture(url, secret, "retune")["center_hz"]
        set_band(page, cur, 96_000)
        own = wait_until(lambda: running_scan_jobs(url, secret, "retune"), 3.0,
                         "the page's scan on retune")[0]["id"]
        assert page.evaluate(AUTO_SCAN_JS) == own, page.evaluate(AUTO_SCAN_JS)
        page.wait_for_function(
            f"() => window.zetaTest.waterfallMode === 'scan'"
            f" && window.zetaTest.wf().jobId === {json.dumps(own)}", timeout=5_000)
        set_band(page, cur, 40_000)
        wait_until(lambda: stopped(url, secret, own), 3.0, "the shown-source scan stopped")
        jobs = wait_until(lambda: [j for j in running_spectrum_jobs(url, secret, "retune")
                                   if j["elastic"]], 5.0, "an elastic spectrum job on retune")
        page.wait_for_function(
            f"() => window.zetaTest.waterfallMode === 'spectrum'"
            f" && window.zetaTest.wf().jobId === {json.dumps(jobs[0]['id'])}", timeout=5_000)
        w = page.evaluate("window.zetaTest.wf()")
        assert w["band"]["lo"] <= w["view"]["lo"] and w["view"]["hi"] <= w["band"]["hi"], w
        assert abs((w["view"]["lo"] + w["view"]["hi"]) / 2 - cur) <= 1_000, (w, cur)
        assert abs(w["view"]["hi"] - w["view"]["lo"] - 40_000) <= 1_000, w
        assert page.evaluate(AUTO_SCAN_JS) is None
    finally:
        stop_scans(url, secret)


# ---- two layers, outlines, easing (plan 11 Task 7) -----------------------------------------
# The coarse scan's rows are painted across the whole view under the fine capture; every rigid
# holder's band is outlined with its kind (the capture itself with none); under `follow
# target` the view eases to a new capture, under `free` it stays.

# Non-background pixels (BG #000030) in the top quarter of the canvas's right-most column.
TOP_QUARTER_LIT_JS = """() => {
  const c = document.querySelector('#waterfall canvas.wf-canvas');
  if (!c || c.width < 2 || c.height < 8) return 0;
  const d = c.getContext('2d').getImageData(c.width - 1, 0, 1, Math.floor(c.height / 4)).data;
  let n = 0;
  for (let i = 0; i < d.length; i += 4) if (d[i] !== 0 || d[i + 1] !== 0 || d[i + 2] !== 48) n++;
  return n;
}"""


# Paints the newest masked coarse row whose notch (the masked range holding the row's middle
# bin) lies inside the view alone into a blank buffer and counts the background pixel rows
# under its notch and the lit ones elsewhere in its band; then redraws and counts the
# background rows under that notch in the replayed coarse column (both parities fill it).
MASK_PROBE_JS = """() => {
  const wf = window.zetaTest.waterfall();
  const s = wf.shown();
  const notchOf = (r) => (r.masked || []).find(([lo, hi]) => lo <= r.q.length / 2 && r.q.length / 2 < hi);
  const inView = (r) => {
    const n = notchOf(r);
    if (!n) return false;
    const perBin = (r.hi - r.lo) / r.q.length;
    return r.lo + n[0] * perBin >= s.lo && r.lo + n[1] * perBin <= s.hi;
  };
  const rows = wf.coarseRows.map((r) => r.row);
  const row = [...rows].reverse().find(inView);
  if (!row) return null;
  const h = wf.coarseColumn.height;
  const cw = wf.coarseColumn.width;
  const yOf = (hz) => Math.round(((s.hi - hz) / (s.hi - s.lo)) * h);
  const isBg = (b, y) => b.data[y * cw * 4] === 0 && b.data[y * cw * 4 + 1] === 0 && b.data[y * cw * 4 + 2] === 48;
  const count = (b, y0, y1, bg) => {
    let n = 0;
    for (let y = Math.max(0, y0); y < Math.min(h, y1); y++) if (isBg(b, y) === bg) n++;
    return n;
  };
  const perBin = (row.hi - row.lo) / row.q.length;
  const [lo, hi] = notchOf(row);
  // One px inside each edge, and inside the canvas.
  const notchTop = Math.max(0, yOf(row.lo + hi * perBin) + 1);
  const notchBottom = Math.min(h, yOf(row.lo + lo * perBin) - 1);
  const buf = wf.ctx.createImageData(cw, h);
  wf.blankBuffer(buf);
  wf.fillColumn(row, false, buf);
  const notchRows = notchBottom - notchTop;
  const notchBgAlone = count(buf, notchTop, notchBottom, true);
  const restLit = count(buf, yOf(row.hi) + 1, notchTop - 1, false) + count(buf, notchBottom + 1, yOf(row.lo) - 1, false);
  wf.redraw();
  const notchBgRedraw = count(wf.coarseColumn, notchTop, notchBottom, true);
  return { masked: row.masked, notchRows, notchBgAlone, restLit, notchBgRedraw };
}"""


def scenario_coarse_layer_paints_outside_the_capture(page, url, secret):
    stop_scans(url, secret)
    try:
        zoom_tone_out(page, url, secret)
        # The view's top quarter (100.024-100.048 MHz) lies past the 48 kHz capture: only the
        # coarse layer paints there.
        page.wait_for_function("() => window.zetaTest.wf().coarsePainted > 0", timeout=5_000)
        p0 = page.evaluate(WF_STATE_JS)["painted"]
        page.wait_for_function(f"() => window.zetaTest.wf().painted > {p0} + 5", timeout=5_000)
        page.wait_for_function(f"() => ({TOP_QUARTER_LIT_JS})() > 0", timeout=5_000)
        # Still the coarse view: the axis spans it, not the capture.
        assert ticks(page)[0] == "100.048" and ticks(page)[-1] == "99.952", ticks(page)
    finally:
        stop_scans(url, secret)


def scenario_coarse_layer_moves_on_when_the_fine_stream_stops(page, url, secret):
    # The fine job stops and cannot be restarted (its job_start aborted): the coarse layer
    # goes on painting columns of its own across the zoomed-out view.
    stop_scans(url, secret)
    blocked = {"seen": 0}

    def route(r):
        body = r.request.post_data or ""
        if '"job_start"' in body and '"kind":"spectrum"' in body:
            blocked["seen"] += 1
            r.abort()
        else:
            r.continue_()
    try:
        zoom_tone_out(page, url, secret)
        page.wait_for_function("() => window.zetaTest.wf().coarsePainted > 0", timeout=5_000)
        page.route("**/op", route)
        stop_job(url, secret, page.evaluate(WF_STATE_JS)["jobId"])
        page.wait_for_function("() => window.zetaTest.wf().sourceId === null", timeout=10_000)
        assert blocked["seen"], "the waterfall did not try to restart its job"
        w0 = page.evaluate(WF_STATE_JS)
        page.wait_for_function(
            f"() => window.zetaTest.wf().coarsePainted > {w0['coarsePainted']} + 10",
            timeout=5_000)
        w1 = page.evaluate(WF_STATE_JS)
        assert w1["painted"] == w0["painted"], f"fine columns painted: {w0} -> {w1}"
        assert ticks(page)[0] == "100.048" and ticks(page)[-1] == "99.952", ticks(page)
        page.wait_for_function(f"() => ({TOP_QUARTER_LIT_JS})() > 0", timeout=5_000)
    finally:
        page.unroute("**/op")
        stop_scans(url, secret)


def scenario_coarse_layer_leaves_masked_bins_unpainted(page, url, secret):
    # A graph put of a 12 kHz notch and a 0.1 edge mask on the page's own scan (48 kHz steps:
    # four notches fit, so the notch is in force; the grid alternates, so the edge mask is):
    # the notch's pixel rows are not painted by the row that masks them, and a redraw,
    # replaying both parities, fills them from the sweep shifted by a quarter step.
    stop_scans(url, secret)
    try:
        zoom_tone_out(page, url, secret)
        own = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                         "an automatic scan on scanner")[0]["id"]
        page.wait_for_function("() => window.zetaTest.wf().coarsePainted > 0", timeout=5_000)
        graph = http_json(url, secret, "/op", {"op": "graph_get", "job": own})["value"]
        fft = [n for n in graph["nodes"] if n["kind"] == "fft"]
        assert len(fft) == 1, f"scan graph {graph}"
        fft[0]["params"]["notch_hz"] = 12_000
        fft[0]["params"]["edge_mask"] = 0.1
        http_json(url, secret, "/op", {"op": "graph_put", "job": own, "graph": graph})
        ctx = http_json(url, secret, "/op", {"op": "job_context", "job": own})["value"]["scan"]
        assert ctx["mask"] == {"notch_hz": 12_000, "edge_mask": 0.1}, ctx
        assert ctx["alternate"]["shift_hz"] == 12_000, ctx
        assert ctx["alternate"]["steps_shifted"] == ctx["steps"] + 1, ctx
        # Two sweeps of masked rows (about 5 rows per 250 ms step, 5 steps).
        page.wait_for_function("() => window.zetaTest.wf().coarseMasked >= 20", timeout=10_000)
        probe = page.evaluate(MASK_PROBE_JS)
        assert probe and probe["masked"] == [[0, 102], [384, 641], [922, 1024]], probe
        assert probe["notchRows"] > 10, probe
        assert probe["notchBgAlone"] == probe["notchRows"], probe
        assert probe["restLit"] > 0, probe
        assert probe["notchBgRedraw"] < probe["notchRows"] / 2, probe
    finally:
        stop_scans(url, secret)


def outlines(page):
    page.evaluate("window.zetaTest.poll()")
    return page.locator("#waterfall .outline")


def scenario_rigid_bands_are_outlined(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    # No rigid holder: the capture itself is outlined.
    expect(outlines(page)).to_have_count(1)
    expect(outlines(page).first).to_contain_text("capture")
    cur = current_capture(url, secret, "retune")["center_hz"]
    lo = start_job(url, secret, "passthrough", "retune", cur - 8_000, 4_000)
    hi = None
    try:
        hi = start_job(url, secret, "passthrough", "retune", cur + 8_000, 4_000)
        ol = outlines(page)
        expect(ol).to_have_count(2)
        for job in (lo, hi):
            el = page.locator(f'#waterfall .outline[data-key="{job}"]')
            expect(el).to_have_count(1)
            expect(page.locator(f'#waterfall .job-tag[data-job="{job}"] .kind')
                   ).to_have_text("passthrough")
        top = {j: page.locator(f'#waterfall .outline[data-key="{j}"]').bounding_box()["y"]
               for j in (lo, hi)}
        assert top[hi] < top[lo], f"the higher band is not drawn above the lower one: {top}"
        stop_job(url, secret, hi)
        expect(outlines(page)).to_have_count(1)
        expect(page.locator(f'#waterfall .outline[data-key="{lo}"]')).to_have_count(1)
    finally:
        stop_job(url, secret, lo)
        if hi:
            stop_job(url, secret, hi)


def scenario_tags_show_kind_id_and_link_rows(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = current_capture(url, secret, "retune")["center_hz"]
    a = b = None
    hot = re.compile(r"\bhot\b")
    try:
        a = start_job(url, secret, "passthrough", "retune", cur, 10_000)
        # Off the tone, so its squelch stays closed.
        b = start_job(url, secret, "fm_demod", "retune", cur + 15_000, 12_500, deviation_hz=5000)
        page.evaluate("window.zetaTest.poll()")
        ta = page.locator(f'#waterfall .job-tag[data-job="{a}"]')
        tb = page.locator(f'#waterfall .job-tag[data-job="{b}"]')
        expect(ta.locator(".kind")).to_have_text("passthrough")
        expect(ta.locator(".short")).to_have_text(a[-6:])
        expect(ta.locator(".dot")).to_have_count(0)
        expect(tb.locator(".dot.closed")).to_have_count(1, timeout=5_000)
        row_a = job_row(page, a)
        expect(row_a.locator(".band")).to_have_text(f"{cur / 1e6:.4f} MHz, 10.0 kHz")
        ta.hover()
        expect(page.locator(f'#jobs .job[data-id="{a}"]')).to_have_class(hot)
        page.evaluate("window.zetaTest.poll()")
        page.wait_for_timeout(300)
        expect(page.locator(f'#jobs .job[data-id="{a}"]')).to_have_class(hot)  # survives a re-render
        page.mouse.move(5, 5)
        expect(page.locator(f'#jobs .job[data-id="{a}"]')).not_to_have_class(hot)
        job_row(page, b).hover()
        expect(tb).to_have_class(hot)
        expect(page.locator(f'#waterfall .outline[data-key="{b}"]')).to_have_class(hot)
    finally:
        for j in (a, b):
            if j:
                stop_job(url, secret, j)



# ---- the rail (plan 24 Task 3) ----------------------------------------------------------------
# In the console theme job tags sit in #rail beside the axis, at their outline's centre; where
# they crowd they collapse to thin segments and the hot or speaking one keeps the space.

def scenario_rail_tags_align(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    capture = current_capture(url, secret, "retune")
    cur = capture["center_hz"]
    a = start_job(url, secret, "passthrough", "retune", cur - 8_000, 4_000)
    b = start_job(url, secret, "passthrough", "retune", cur + 8_000, 4_000)
    c = None
    try:
        page.evaluate("window.zetaTest.poll()")
        rail = page.locator("#waterfall #rail")
        expect(rail).to_be_visible()
        for job in (a, b):
            tag = page.locator(f'#rail .job-tag[data-job="{job}"]')
            ol = page.locator(f'#waterfall .outline[data-key="{job}"]')
            expect(tag).to_have_count(1)
            tb, ob = tag.bounding_box(), ol.bounding_box()
            assert abs((tb["y"] + tb["height"] / 2) - (ob["y"] + ob["height"] / 2)) <= 2, (tb, ob)
            expect(tag.locator(".centre")).to_have_text(re.compile(r"^\d+\.\d{3}$"))
            assert tag.get_attribute("data-colour") == ol.get_attribute("data-colour")
        page.locator(f'#rail .job-tag[data-job="{a}"]').hover()
        expect(page.locator(f'#waterfall .outline[data-key="{a}"]')).to_have_class(re.compile(r"\bhot\b"))
        expect(job_row(page, a)).to_have_class(re.compile(r"\bhot\b"))
        # A job outside the shown band has a hidden tag, not one pinned at the rail's edge. It
        # sits inside the capture (a and b hold the source) but outside the 20 kHz shown.
        set_band(page, cur, 20_000)
        c_hz = cur + capture["bandwidth_hz"] // 2 - 4_000
        assert c_hz - 2_000 > cur + 10_000, f"no room outside the shown band: {capture}"
        c = start_job(url, secret, "passthrough", "retune", c_hz, 4_000)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#rail .job-tag[data-job="{c}"]')).to_have_count(1)
        expect(page.locator(f'#rail .job-tag[data-job="{c}"]')).to_be_hidden()
    finally:
        for j in (a, b, c):
            if j:
                stop_job(url, secret, j)


def scenario_rail_collapses_crowd(page, url, secret):
    # `cb` plays its recording's own 48 kHz only (476.876-476.924 MHz). Six narrow jobs packed so
    # every centre lies within one full tag's height of the first (so each overlaps the one full
    # tag), yet over half a thin segment apart (so a thin one's middle is not under the next).
    select_source(page, url, secret, "cb")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    base = 476_895_000
    jobs = [start_job(url, secret, "passthrough", "cb", base, 100)]
    try:
        page.evaluate("window.zetaTest.poll()")
        first = page.locator(f'#rail .job-tag[data-job="{jobs[0]}"]')
        expect(first).to_be_visible()
        th = first.bounding_box()["height"]
        view = page.evaluate("window.zetaTest.wf().view")
        h = page.evaluate("document.querySelector('#waterfall canvas.wf-canvas').clientHeight")
        # Five steps under one tag height, each well over half a 5 px thin segment (the
        # pointer lands on whole pixels, so the next segment must clear the middle by more).
        step_px = 0.9 * th / 5
        assert step_px > 3.2, f"tags {th} px high leave no room for six thin segments"
        step_hz = round(step_px * (view["hi"] - view["lo"]) / h)
        for i in range(1, 6):
            jobs.append(start_job(url, secret, "passthrough", "cb", base + i * step_hz, 100))
        page.evaluate("window.zetaTest.poll()")
        tags = page.locator("#rail .job-tag")
        expect(tags).to_have_count(6)
        expect(page.locator("#rail .job-tag.thin")).to_have_count(5)
        for t in page.locator("#rail .job-tag.thin").all():  # named: channel or kind, centre
            title = t.get_attribute("title")
            assert title and title == t.get_attribute("aria-label"), (title, t.get_attribute("aria-label"))
            assert re.fullmatch(r"\S+ 476\.\d{3}", title), title
        full = page.locator("#rail .job-tag:not(.thin)")
        expect(full).to_have_count(1)
        boxes = [t.bounding_box() for t in tags.all()]
        mids = [bx["y"] + bx["height"] / 2 for bx in boxes]
        full_h = full.bounding_box()["height"]
        assert max(mids) - min(mids) < full_h, f"the crowd spans {max(mids) - min(mids)} px, a tag {full_h}"
        other = page.locator(f'#rail .job-tag[data-job="{jobs[3]}"]')
        other.hover()
        expect(other).not_to_have_class(re.compile(r"\bthin\b"))
        expect(page.locator("#rail .job-tag:not(.thin)")).to_have_count(1)
    finally:
        for j in jobs:
            stop_job(url, secret, j)



# ---- the side column (plan 24 Task 5) --------------------------------------------------------
# The chat log and the jobs pane fold on their header bars (view `side`), job rows sit on two
# lines with no sideways scroll, and idle plan channels fold into one group row.

def scenario_side_folds(page, url, secret):
    select_source(page, url, secret, "tone")
    before = page.locator("canvas.wf-canvas").bounding_box()["width"]
    page.locator(".pane.jobs .pane-head").click()
    expect(page.locator("section.pane.jobs")).to_have_attribute("data-collapsed", "")
    assert view_value(page, "side") == "side transcript"
    page.locator(".pane.transcript .pane-head").click()
    assert view_value(page, "side") == "side none"
    page.wait_for_function(  # the waterfall follows its resize
        "w => document.querySelector('canvas.wf-canvas').getBoundingClientRect().width - w >= 200",
        arg=before, timeout=3_000)
    after = page.locator("canvas.wf-canvas").bounding_box()["width"]
    assert after - before >= 200, (before, after)
    run_cmd(page, "view set side both")
    expect(page.locator("section.pane[data-collapsed]")).to_have_count(0)
    expect(page.locator(".pane.jobs h2 .count")).to_have_text(re.compile(r"^\d+$"))


def scenario_job_row_two_lines(page, url, secret):
    select_source(page, url, secret, "tone")
    cur = current_capture(url, secret, "tone")["center_hz"]
    job = start_job(url, secret, "fm_demod", "tone", cur, 12_500, deviation_hz=5000)
    try:
        row = job_row(page, job)
        kind, band, ident = (row.locator(s).bounding_box() for s in (".kind", ".band", ".id"))
        assert band["y"] > kind["y"] + kind["height"] / 2, (kind, band)
        assert ident["y"] > kind["y"] + kind["height"] / 2, (kind, ident)
        sw, cw = page.evaluate("el => [el.scrollWidth, el.clientWidth]", row.element_handle())
        assert sw == cw, (sw, cw)
        expect(row.locator(".band")).to_have_text(f"{cur / 1e6:.4f} MHz, 12.5 kHz")
        assert row.locator(".kind").get_attribute("dir") == "auto"
    finally:
        stop_job(url, secret, job)


def scenario_job_rows_fold_idle_plan_channels(page, url, secret):
    # `cb` plays its own 48 kHz only (476.876-476.924 MHz), which holds three uhf_cb_au channels
    # (ch20, ch59, ch60): three passthrough jobs on each, told apart by their widths so no start
    # is a duplicate.
    select_source(page, url, secret, "cb")
    plans = http_json(url, secret, "/op", {"op": "channel_plans"})["value"]["plans"]
    plan = next(p for p in plans if p["id"] == "uhf_cb_au")
    inside = [ch for ch in plan["channels"] if 476_880_000 <= ch["center_hz"] <= 476_920_000]
    assert len(inside) == 3, inside
    jobs = []
    try:
        for i in range(9):
            ch = inside[i % 3]
            jobs.append(start_job(url, secret, "passthrough", "cb", ch["center_hz"],
                                  1_000 * (1 + i // 3)))
        page.evaluate("window.zetaTest.foldIdle(0)")  # idle threshold 0 ms for the test
        page.evaluate("window.zetaTest.poll()")
        group = page.locator('#jobs details.job-group[data-plan="uhf_cb_au"]')
        expect(group).to_have_count(1)
        expect(group.locator("summary")).to_have_text(re.compile(r"^9 idle passthrough on uhf_cb_au"))
        assert group.get_attribute("open") is not None
        for j in jobs:
            expect(group.locator(f'.job[data-id="{j}"]')).to_have_count(1)
        stop_job(url, secret, jobs[0])
        stop_job(url, secret, jobs[1])
        page.evaluate("window.zetaTest.poll()")
        expect(group).to_have_count(0)
    finally:
        for j in jobs[2:]:
            stop_job(url, secret, j)


def scenario_feed_lag_reseeds_squelch_dots(page, url, secret):
    # A quiet fm_demod (closed) is tagged. A lag forgets the dot at once (seed reads held so
    # the gap is observable) and the read brings it back; a reconnect re-seeds again; a read
    # in flight across a second reseed is discarded as stale.
    quiet = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500)
    try:
        select_source(page, url, secret, "tone")
        page.evaluate("window.zetaTest.poll()")
        tag = job_row(page, quiet).locator(".kind .tag.squelch")
        expect(tag).to_be_visible(timeout=5_000)
        assert "closed" in (tag.get_attribute("title") or "")
        r0 = page.evaluate("window.zetaTest.reseeds()")
        page.evaluate("window.zetaTest.holdSeeds(true)")
        page.evaluate("window.zetaTest.feedLag()")
        assert page.evaluate("window.zetaTest.reseeds()") == r0 + 1
        expect(job_row(page, quiet).locator(".kind .tag.squelch")).to_have_count(0)
        page.evaluate("window.zetaTest.holdSeeds(false)")
        tag = job_row(page, quiet).locator(".kind .tag.squelch")
        expect(tag).to_be_visible(timeout=5_000)
        assert "closed" in (tag.get_attribute("title") or "")
        # A reconnect: the socket reopens and the feed re-seeds.
        before = stable_ws_connections(url, secret)[-1]
        page.evaluate("window.zetaTest.feedReconnect()")
        wait_until(lambda: page.evaluate("window.zetaTest.reseeds()") >= r0 + 2, 10.0,
                   "the reopened feed re-seeds")
        assert stable_ws_connections(url, secret)[-1] == before
        expect(job_row(page, quiet).locator(".kind .tag.squelch")).to_be_visible(timeout=5_000)
        # A read in flight across a second reseed answers into a hole: discarded.
        s0 = page.evaluate("window.zetaTest.staleSeedsDiscarded()")
        page.evaluate("window.zetaTest.holdSeeds(true)")
        page.evaluate("window.zetaTest.feedLag()")
        page.evaluate("window.zetaTest.feedLag()")
        page.evaluate("window.zetaTest.holdSeeds(false)")
        wait_until(lambda: page.evaluate("window.zetaTest.staleSeedsDiscarded()") > s0, 5.0,
                   "the pre-hole read is discarded")
        expect(job_row(page, quiet).locator(".kind .tag.squelch")).to_be_visible(timeout=5_000)
    finally:
        page.evaluate("window.zetaTest.holdSeeds(false)")
        stop_job(url, secret, quiet)


def scenario_job_rows_fold_only_on_evidence(page, url, secret):
    # Every job on the shown source is watched (the feed), so each fm_demod's own squelch is
    # the evidence: eighteen fm_demod on cb's three plan channels (six widths each), the tone
    # holds ch20 open and its guard keeps ch59 and ch60 closed. The twelve quiet rows fold
    # without a mix; a mix keeps them folded and ch20's talking rows out of the group.
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    plans = http_json(url, secret, "/op", {"op": "channel_plans"})["value"]["plans"]
    plan = next(p for p in plans if p["id"] == "uhf_cb_au")
    inside = [ch for ch in plan["channels"] if 476_880_000 <= ch["center_hz"] <= 476_920_000]
    assert len(inside) == 3, inside
    ch20 = next(ch["center_hz"] for ch in inside if ch["number"] == 20)
    jobs = []
    try:
        for i in range(18):
            ch = inside[i % 3]
            jobs.append((start_job(url, secret, "fm_demod", "cb", ch["center_hz"],
                                   5_000 + 1_000 * (i // 3), deviation_hz=2500),
                         ch["center_hz"]))
        page.evaluate("window.zetaTest.foldIdle(0)")
        group = page.locator('#jobs details.job-group[data-plan="uhf_cb_au"]')
        for _ in range(3):  # the watched ones' squelch states arrive
            page.evaluate("window.zetaTest.poll()")
            page.wait_for_timeout(700)
        quiet = [j for j, hz in jobs if hz != ch20]
        talking = [j for j, hz in jobs if hz == ch20]
        assert len(quiet) == 12, quiet

        def grouped():
            page.evaluate("window.zetaTest.poll()")
            if group.count() == 1:
                return True
            page.wait_for_timeout(500)
            return False

        # No mix: the quiet rows fold on their own squelch.
        wait_until(grouped, 10.0, "the quiet rows fold without a mix")
        for j in quiet:
            expect(group.locator(f'.job[data-id="{j}"]')).to_have_count(1)
        for j in talking:
            expect(group.locator(f'.job[data-id="{j}"]')).to_have_count(0)
        # A mix over every source: the group stays, ch20's speaking rows are not in it.
        http_mix_start(url, secret, None)
        for j in talking:
            expect(page.locator(f'#jobs .job[data-id="{j}"].speaking')).to_have_count(
                1, timeout=8_000)
        expect(group).to_have_count(1)
        for j in quiet:
            expect(group.locator(f'.job[data-id="{j}"]')).to_have_count(1)
        for j in talking:
            expect(group.locator(f'.job[data-id="{j}"]')).to_have_count(0)
        expect(group.locator("summary")).to_have_text(
            re.compile(r"^\d+ idle fm_demod on uhf_cb_au"))
        # A newer mix on another source is the one the page watches: the group stays, its
        # evidence is the rows' own squelch, not the mix.
        other = http_mix_start(url, secret, ["tone"])
        page.evaluate("window.zetaTest.poll()")
        wait_until(lambda: page.evaluate("window.zetaMix.job") == other, 5.0,
                   "the newest mix is the one watched")
        page.evaluate("window.zetaTest.poll()")
        expect(group).to_have_count(1)
        for j in quiet:
            expect(group.locator(f'.job[data-id="{j}"]')).to_have_count(1)
    finally:
        stop_mixes(url, secret)
        for j, _ in jobs:
            stop_job(url, secret, j)

# ---- draggable outlines (plan 18 Task 6) -----------------------------------------------------
# A running rigid job the node can retune is dragged by its outline: an edge handle resizes,
# the body moves, release sends `job_retune`. `retune` shows its 48 kHz capture.

def is_op(name):
    def pred(r):
        if r.method != "POST" or not r.url.endswith("/op"):
            return False
        try:
            body = json.loads(r.post_data or "")
        except ValueError:
            return False
        return body.get("op") == name
    return pred


def outline_box(page, job):
    return page.locator(f'#waterfall .outline[data-key="{job}"]').bounding_box()


def show_retune(page, url, secret):
    """Show `retune`; (its capture's centre, Hz per CSS pixel of the waterfall)."""
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = current_capture(url, secret, "retune")["center_hz"]
    hzpp = 48_000 / page.locator("#waterfall canvas.wf-canvas").bounding_box()["height"]
    return cur, hzpp


def centre_of(box):
    return box["x"] + box["width"] / 2, box["y"] + box["height"] / 2


def scenario_drag_handle_narrows_the_band_and_clamps_at_1_khz(page, url, secret):
    cur, hzpp = show_retune(page, url, secret)
    job = start_job(url, secret, "passthrough", "retune", cur, 10_000)
    try:
        ol = page.locator(f'#waterfall .outline[data-key="{job}"]')
        page.evaluate("window.zetaTest.poll()")
        expect(ol.locator(".outline-handle")).to_have_count(2)
        ghost = page.locator("#waterfall .outline-ghost")
        readout = page.locator("#waterfall .outline-readout")
        x, y = centre_of(ol.locator('.outline-handle[data-edge="hi"]').bounding_box())
        with page.expect_request(is_op("job_retune")) as req:
            page.mouse.move(x, y)
            page.mouse.down()
            page.mouse.move(x, y + 20, steps=5)
            expect(ghost).to_have_count(1)
            expect(readout).to_contain_text("release to retune")
            top = ol.bounding_box()["y"]
            page.evaluate("window.zetaTest.poll()")
            page.wait_for_timeout(300)
            assert abs(ol.bounding_box()["y"] - top) < 1, "a poll mid-drag moved the outline"
            page.mouse.up()
        body = json.loads(req.value.post_data)
        assert body["job"] == job and body["replace"] == [], f"job_retune {body}"
        assert abs(body["bandwidth_hz"] - (10_000 - 20 * hzpp)) <= 1.5 * hzpp, f"{body}"
        assert abs(body["center_hz"] - (cur - 10 * hzpp)) <= 1.5 * hzpp, f"{body}"
        expect(ghost).to_have_count(0)
        expect(readout).to_have_count(0)
        want = {"center_hz": body["center_hz"], "bandwidth_hz": body["bandwidth_hz"]}
        wait_until(lambda: job_record(url, secret, job)["tuning"] == want, 5.0,
                   "the record follows")
        expect(page.locator("#transcript .line.op", has_text=re.compile(
            rf"^job_retune job={job} .*→ job {job}"))).to_have_count(1)
        # Clamp: drag the hi handle down past the lo edge.
        page.evaluate("window.zetaTest.poll()")
        h = ol.bounding_box()["height"]
        x, y = centre_of(ol.locator('.outline-handle[data-edge="hi"]').bounding_box())
        with page.expect_request(is_op("job_retune")) as req2:
            page.mouse.move(x, y)
            page.mouse.down()
            page.mouse.move(x, y + h + 40, steps=5)
            expect(readout).to_contain_text("1.0 kHz")
            page.mouse.up()
        body2 = json.loads(req2.value.post_data)
        assert body2["bandwidth_hz"] == 1000, f"clamped job_retune {body2}"
    finally:
        stop_job(url, secret, job)


def scenario_drag_body_moves_the_centre_and_a_short_drag_is_a_click(page, url, secret):
    cur, hzpp = show_retune(page, url, secret)
    job = start_job(url, secret, "passthrough", "retune", cur, 10_000)
    try:
        ol = page.locator(f'#waterfall .outline[data-key="{job}"]')
        page.evaluate("window.zetaTest.poll()")
        expect(ol.locator(".outline-handle")).to_have_count(2)
        ghost = page.locator("#waterfall .outline-ghost")
        x, y = centre_of(outline_box(page, job))
        with page.expect_request(is_op("job_retune")) as req:
            page.mouse.move(x, y)
            page.mouse.down()
            page.mouse.move(x, y + 30, steps=5)
            page.mouse.up()
        body = json.loads(req.value.post_data)
        assert body["bandwidth_hz"] == 10_000, f"job_retune {body}"
        assert abs(body["center_hz"] - (cur - 30 * hzpp)) <= 1.5 * hzpp, f"{body}"
        wait_until(lambda: job_record(url, secret, job)["tuning"]["center_hz"]
                   == body["center_hz"], 5.0, "the record follows")
        page.evaluate("window.zetaTest.poll()")
        # A 2 px drag sends nothing and leaves the outline where it is.
        seen = []
        page.on("request", lambda r: seen.append(r) if is_op("job_retune")(r) else None)
        top = outline_box(page, job)["y"]
        x, y = centre_of(outline_box(page, job))
        page.mouse.move(x, y)
        page.mouse.down()
        page.mouse.move(x, y + 2, steps=2)
        page.mouse.up()
        page.wait_for_timeout(500)
        assert seen == [], f"a 2 px drag sent {[r.post_data for r in seen]}"
        expect(ghost).to_have_count(0)
        assert abs(outline_box(page, job)["y"] - top) < 1, "a 2 px drag moved the outline"
        # A pointercancel mid-drag snaps back and sends nothing.
        page.mouse.move(x, y)
        page.mouse.down()
        page.mouse.move(x, y + 25, steps=5)
        expect(ghost).to_have_count(1)
        ol.dispatch_event("pointercancel")
        page.wait_for_timeout(300)
        assert seen == [], f"a cancelled drag sent {[r.post_data for r in seen]}"
        expect(ghost).to_have_count(0)
        assert abs(outline_box(page, job)["y"] - top) < 1, "a cancelled drag did not snap back"
        page.mouse.up()
        page.wait_for_timeout(200)
        assert seen == [], f"the release after a cancel sent {[r.post_data for r in seen]}"
        # A band a few px tall: the handles sit outside it, so the body still moves it.
        small = start_job(url, secret, "passthrough", "retune", cur + 12_000, 1000)
        try:
            page.evaluate("window.zetaTest.poll()")
            sol = page.locator(f'#waterfall .outline[data-key="{small}"]')
            expect(sol.locator(".outline-handle")).to_have_count(2)
            expect(sol).to_have_class(re.compile(r"\bhandles-out\b"))
            sb = outline_box(page, small)
            hi = sol.locator('.outline-handle[data-edge="hi"]').bounding_box()
            lo = sol.locator('.outline-handle[data-edge="lo"]').bounding_box()
            assert hi["y"] + hi["height"] <= sb["y"] + 1, f"hi handle {hi} over the band {sb}"
            assert lo["y"] >= sb["y"] + sb["height"] - 1, f"lo handle {lo} over the band {sb}"
            x, y = centre_of(sb)
            with page.expect_request(is_op("job_retune")) as req:
                page.mouse.move(x, y)
                page.mouse.down()
                page.mouse.move(x, y + 20, steps=4)
                page.mouse.up()
            body = json.loads(req.value.post_data)
            assert body["job"] == small and body["bandwidth_hz"] == 1000, f"body drag {body}"
            assert body["center_hz"] < cur + 12_000, f"body drag {body}"
            # A passthrough's outline menu offers Stop only (no fit, no presets).
            page.evaluate("window.zetaTest.poll()")
            sol.click(button="right")
            menu = page.locator("#ctx-outline")
            expect(menu).to_be_visible()
            got = menu.locator(".ctx-item:visible").evaluate_all(
                "els => els.map(e => e.dataset.action)")
            assert got == ["stop"], f"passthrough outline menu {got}"
            page.keyboard.press("Escape")
            expect(menu).to_be_hidden()
        finally:
            stop_job(url, secret, small)
    finally:
        stop_job(url, secret, job)


def scenario_drag_outside_the_capture_conflicts_snaps_back_and_the_retry_moves_it(
        page, url, secret):
    cur, _ = show_retune(page, url, secret)
    a = start_job(url, secret, "passthrough", "retune", cur, 10_000)
    b = None
    try:
        b = start_job(url, secret, "passthrough", "retune", cur + 8_000, 4_000)
        page.evaluate("window.zetaTest.poll()")
        ol = page.locator(f'#waterfall .outline[data-key="{b}"]')
        expect(ol.locator(".outline-handle")).to_have_count(2)
        ob = outline_box(page, b)
        top = ob["y"]
        h = page.locator("#waterfall canvas.wf-canvas").bounding_box()["height"]
        x, y = centre_of(ob)
        page.mouse.move(x, y)
        page.mouse.down()
        page.mouse.move(x, y - h - 20, steps=8)  # pointer capture keeps the drag
        page.mouse.up()
        at = re.escape(f"{cur / 1e6:.3f}")
        line = page.locator("#transcript .line.error",
                            has_text=re.compile(rf"conflict.*held by passthrough {a} @ {at}"))
        expect(line).to_have_count(1)
        wait_until(lambda: abs(outline_box(page, b)["y"] - top) < 1, 3.0, "snap back")
        assert job_record(url, secret, b)["tuning"]["center_hz"] == cur + 8_000
        button = line.locator("button", has_text=f"stop passthrough {a[-6:]} and retry")
        expect(button).to_have_count(1)
        with page.expect_request(is_op("job_retune")) as req:
            button.click()
        body = json.loads(req.value.post_data)
        assert body["replace"] == [a], f"retry {body}"

        def moved():
            ra, rb = job_record(url, secret, a), job_record(url, secret, b)
            return (ra["state"]["status"] == "stopped"
                    and rb["tuning"]["center_hz"] == body["center_hz"])
        wait_until(moved, 5.0, "A stopped, B moved")
        assert current_capture(url, secret, "retune")["center_hz"] == body["center_hz"]
        expect(page.locator("#transcript .line.op", has_text=re.compile(
            rf"^job_retune job={b} .*replace=… → job {b}"))).to_have_count(1)
    finally:
        stop_job(url, secret, a)
        if b:
            stop_job(url, secret, b)


def move_retune_capture(url, secret):
    """Hold `retune` with a rigid passthrough on its capture, then move the capture 1 MHz with
    a rigid start that replaces it; (the new job's id, the old centre, the new centre)."""
    cur = current_capture(url, secret, "retune")["center_hz"]
    held = start_job(url, secret, "passthrough", "retune", cur, 10_000)
    band = cur + 1_000_000 if cur + 1_000_000 < 299_000_000 else cur - 1_000_000
    r = http_json(url, secret, "/op", {"op": "job_start", "kind": "passthrough",
                                       "source": "retune", "center_hz": band,
                                       "bandwidth_hz": 10_000, "deviation_hz": None,
                                       "sideband": None, "elastic": False, "replace": [held]})
    assert r["result"] == "job", f"replacing start: {r}"
    return r["value"], cur, band


def capture_of(url, secret):
    c = current_capture(url, secret, "retune")
    return c["center_hz"] - c["bandwidth_hz"] / 2, c["center_hz"] + c["bandwidth_hz"] / 2


def near(a, b, width):
    return abs(a - b) <= width * 0.01


# ---- the outline menu (plan 18 Task 7) -------------------------------------------------------
# Right-click on a retunable outline (wide) or a long press on its label (narrow) opens
# #ctx-outline: Fit to signal, width presets about the centre, Stop.

def scenario_outline_menu_presets_fit_and_stop(page, url, secret):
    cur, _ = show_retune(page, url, secret)
    job = start_job(url, secret, "fm_demod", "retune", cur, 20_000, deviation_hz=5000)
    try:
        ol = page.locator(f'#waterfall .outline[data-key="{job}"]')
        page.evaluate("window.zetaTest.poll()")
        expect(ol.locator(".outline-handle")).to_have_count(2)
        ol.click(button="right")
        menu = page.locator("#ctx-outline")
        expect(menu).to_be_visible()
        got = menu.locator(".ctx-item").evaluate_all(
            "els => els.filter(e => e.offsetParent !== null)"
            ".map(e => [e.dataset.action, e.textContent])")
        assert got == [["fit", "Fit to signal"], ["w2500", "2.5 kHz"], ["w5000", "5 kHz"],
                       ["w6250", "6.25 kHz"], ["w12500", "12.5 kHz"], ["w25000", "25 kHz"],
                       ["denoise", "Denoise on"], ["stop", "Stop"]], f"entries {got}"
        expect(page.locator("#ctx-outline-footer")).to_contain_text(
            f"job={job} center_hz={cur} bandwidth_hz=20000")
        with page.expect_request(is_op("job_retune")) as req:
            page.click('#ctx-outline .ctx-item[data-action="w12500"]')
        assert json.loads(req.value.post_data) == {
            "op": "job_retune", "job": job, "center_hz": cur, "bandwidth_hz": 12500,
            "replace": []}, f"preset {req.value.post_data}"
        expect(menu).to_be_hidden()
        wait_until(lambda: job_record(url, secret, job)["tuning"]["bandwidth_hz"] == 12500,
                   5.0, "preset applied")
        page.evaluate("window.zetaTest.poll()")
        # Denoise: a graph_put of the job's own graph with `denoise: true` on its demodulator
        # (the node keeps the job re-expandable), the entry reading "Denoise off" afterwards.
        ol.click(button="right")
        with page.expect_request(is_op("graph_put")) as req:
            page.click('#ctx-outline .ctx-item[data-action="denoise"]')
        body = json.loads(req.value.post_data)
        demod = [n for n in body["graph"]["nodes"] if n["kind"] == "nbfm_demod"]
        assert body["job"] == job and len(demod) == 1 and demod[0]["params"]["denoise"] is True, \
            f"denoise put {body}"
        expect(menu).to_be_hidden()
        expect(page.locator("#transcript .line.note", has_text="fm_demod: denoise on")).to_have_count(1)
        wait_until(lambda: [n for n in job_record(url, secret, job)["graph"]["nodes"]
                            if n["kind"] == "nbfm_demod"][0]["params"]["denoise"] is True,
                   5.0, "denoise applied")
        wait_until(lambda: job_record(url, secret, job)["state"]["status"] == "running",
                   5.0, "running again")
        assert job_record(url, secret, job)["operator_graph"] is False
        page.evaluate("window.zetaTest.poll()")
        ol.click(button="right")
        expect(menu.locator('.ctx-item[data-action="denoise"]')).to_have_text("Denoise off")
        page.keyboard.press("Escape")
        expect(menu).to_be_hidden()
        page.evaluate("window.zetaTest.poll()")
        # Fit: the tone sits at cur + 1000 Hz.
        ol.click(button="right")
        with page.expect_request(is_op("job_retune")) as req:
            page.click('#ctx-outline .ctx-item[data-action="fit"]')
        body = json.loads(req.value.post_data)
        assert body["job"] == job and body["replace"] == [], f"fit {body}"
        assert abs(body["center_hz"] - (cur + 1000)) <= 100, f"fit centre {body}"
        # Measured on the retune fixture's tone through an fm_demod band of 12.5 kHz: a fresh
        # measure's occupied width x 1.25 gave 7090 Hz (labelled `fm`), not the 5000 Hz floor,
        # so the width is asserted as a range (floor 5000, cap the job's 12500) and the note
        # must report the sent width.
        assert 5000 <= body["bandwidth_hz"] <= 12500, f"fit width {body}"
        kHz = f"{body['bandwidth_hz'] / 1000:.1f}"
        mhz = f"{body['center_hz'] / 1e6:.4f}"
        expect(page.locator("#transcript .line.note", has_text=re.compile(
            rf"^fitted fm_demod to {re.escape(mhz)} MHz, {re.escape(kHz)} kHz \((carrier|fm)\)$"
        ))).to_have_count(1)
        wait_until(lambda: job_record(url, secret, job)["tuning"]["bandwidth_hz"]
                   == body["bandwidth_hz"], 5.0, "fit applied")
        page.evaluate("window.zetaTest.poll()")
        ol.click(button="right")
        page.click('#ctx-outline .ctx-item[data-action="stop"]')
        wait_until(lambda: job_record(url, secret, job)["state"]["status"] == "stopped",
                   5.0, "stopped from the menu")
    finally:
        stop_job(url, secret, job)


def scenario_narrow_long_press_opens_the_outline_menu(page, url, secret):
    fresh_spectrum_jobs(url, secret)
    page.set_viewport_size(NARROW)
    login(page, url, secret)
    page.click('.src-tab[data-id="retune"]')
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = current_capture(url, secret, "retune")["center_hz"]
    job = start_job(url, secret, "passthrough", "retune", cur, 10_000)
    try:
        page.evaluate("window.zetaTest.poll()")
        ol = page.locator(f'#waterfall .outline[data-key="{job}"]')
        expect(ol).to_have_count(1)
        expect(ol.locator(".outline-handle")).to_have_count(0)
        cx, cy = centre_of(page.locator(f'#waterfall .job-tag[data-job="{job}"]').bounding_box())
        page.mouse.move(cx, cy)
        page.mouse.down()
        page.wait_for_timeout(700)
        page.mouse.up()
        expect(page.locator("#ctx-outline")).to_be_visible()
        page.keyboard.press("Escape")
        expect(page.locator("#ctx-outline")).to_be_hidden()
        # A tapped segment reads as the console tag does: the channel or the kind, the squelch
        # dot (one is put in here: this job has no squelch) and the centre, nothing else,
        # floated one rail width in.
        tag = page.locator(f'#rail .job-tag[data-job="{job}"]')
        page.evaluate("t => { t.classList.add('expanded');"
                      " const d = document.createElement('span'); d.className = 'dot closed';"
                      " t.prepend(d); }", tag.element_handle())
        expect(tag.locator(".short")).to_be_hidden()
        expect(tag.locator(".centre")).to_be_visible()
        named = page.evaluate("t => ['kind', 'channel'].filter(c =>"
                              " getComputedStyle(t.querySelector('.' + c)).display !== 'none')"
                              ".length", tag.element_handle())
        assert named == 1, f"{named} of kind and channel shown"
        dot = tag.locator(".dot").bounding_box()
        assert dot and dot["width"] >= 6 and dot["height"] >= 6, dot
        rail_x = page.locator("#rail").bounding_box()["x"]
        rail_w = page.evaluate("document.getElementById('rail').getBoundingClientRect().width")
        assert abs(tag.bounding_box()["x"] - rail_x - rail_w) < 1.5, (tag.bounding_box(), rail_x, rail_w)
    finally:
        stop_job(url, secret, job)


def scenario_follow_target_eases_on_a_capture_change(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    job, _, band = move_retune_capture(url, secret)
    try:
        page.wait_for_function("() => window.zetaTest.wf().easing", polling="raf", timeout=2_000)
        page.wait_for_function("() => !window.zetaTest.wf().easing", timeout=2_000)
        lo, hi = capture_of(url, secret)
        assert near((lo + hi) / 2, band, hi - lo), f"capture {lo}-{hi}, wanted {band}"
        v = page.evaluate(WF_STATE_JS)["view"]
        assert near(v["lo"], lo, hi - lo) and near(v["hi"], hi, hi - lo), \
            f"view {v} after easing, capture {lo}-{hi}"
    finally:
        stop_job(url, secret, job)


def scenario_follow_free_holds_the_view(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    run_cmd(page, "view set follow free")
    expect(view_line(page, "follow = free")).to_have_text("follow = free")
    before = page.evaluate(WF_STATE_JS)["view"]
    page.evaluate("window.__eased = false; setInterval(() => {"
                  " if (window.zetaTest.wf().easing) window.__eased = true; }, 10)")
    job, _, band = move_retune_capture(url, secret)
    try:
        # The fine layer follows the new capture; the view does not.
        page.wait_for_function(
            f"() => {{ const b = window.zetaTest.wf().band;"
            f" return !!b && Math.abs((b.lo + b.hi) / 2 - {band}) < 1000; }}", timeout=5_000)
        page.wait_for_timeout(2_000)
        after = page.evaluate(WF_STATE_JS)["view"]
        assert after == before, f"view moved under follow free: {before} -> {after}"
        assert page.evaluate("window.__eased") is False, "the view eased under follow free"
    finally:
        stop_job(url, secret, job)


# ---- graph view and audio (Task 10) --------------------------------------------------------
# Last: they open more websockets (the audio socket) and each stops the job it starts.

def job_row(page, job):
    """Force a jobs poll and return the job's row once it shows."""
    page.evaluate("window.zetaTest.poll()")
    row = page.locator(f'#jobs .job[data-id="{job}"]')
    expect(row).to_be_visible()
    return row


def open_graph(page, job):
    job_row(page, job).locator("button.graph").click()
    modal = page.locator("#graph")
    expect(modal).to_be_visible()
    return modal


def stream_of(url, secret, job, kind):
    for s in http_json(url, secret, "/streams"):
        if s["kind"] == kind and s["provenance"].get("job") == job:
            return s["id"]
    return None


def scenario_graph_headline(page, url, secret):
    select_source(page, url, secret, "tone")
    cur = current_capture(url, secret, "tone")["center_hz"]
    job = start_job(url, secret, "fm_demod", "tone", cur, 12_500, deviation_hz=5000)
    try:
        open_graph(page, job)
        node = page.locator('#graph svg .graph-node[data-kind="channel"]')
        expect(node.locator("text.graph-node-headline")).to_contain_text("center_hz")
        expect(node.locator("text.graph-node-headline")).to_contain_text(str(cur))
        rx = page.evaluate("el => el.querySelector('rect').getAttribute('rx')",
                           node.element_handle())
        assert rx not in (None, "0"), rx
    finally:
        page.keyboard.press("Escape")
        stop_job(url, secret, job)


def scenario_graph_view_renders_nodes(page, url, secret):
    select_source(page, url, secret, "aprs")
    job = start_job(url, secret, "aprs_decode", "aprs", 144_800_000, 12_500)
    try:
        modal = open_graph(page, job)
        nodes = modal.locator("svg .graph-node")
        page.wait_for_function(
            "() => document.querySelectorAll('#graph svg .graph-node').length >= 3")
        edges = modal.locator("svg .graph-edge")
        assert edges.count() >= 2, f"{edges.count()} edges"
        kinds = [nodes.nth(i).get_attribute("data-kind") for i in range(nodes.count())]
        assert "channel" in kinds, f"node kinds {kinds}"
        # Every edge is labelled with its stream kind.
        labels = modal.locator("svg .graph-edge text").all_text_contents()
        assert sorted(labels) == ["audio", "iq"], f"edge labels {labels}"
        # Columns follow the edges: channel left of nbfm_demod left of aprs_decode.
        xs = page.evaluate("""() => Object.fromEntries([...document.querySelectorAll(
            '#graph svg .graph-node')].map((g) => [g.dataset.kind,
            g.querySelector('rect').x.baseVal.value]))""")
        assert xs["channel"] < xs["nbfm_demod"] < xs["aprs_decode"], f"columns {xs}"
        if os.environ.get("ZETA_UI_SHOTS"):
            modal.locator('svg .graph-node[data-kind="channel"]').click()
            page.screenshot(path=os.path.join(os.environ["ZETA_UI_SHOTS"], "graph.png"))
        page.keyboard.press("Escape")
        expect(modal).to_be_hidden()
    finally:
        stop_job(url, secret, job)


def scenario_graph_edit_puts(page, url, secret):
    select_source(page, url, secret, "aprs")
    job = start_job(url, secret, "aprs_decode", "aprs", 144_800_000, 12_500)
    try:
        before = stream_of(url, secret, job, "audio")
        assert before, "no audio stream for the aprs_decode job"
        modal = open_graph(page, job)
        modal.locator('svg .graph-node[data-kind="channel"]').click()
        params = modal.locator("#graph-params")
        expect(params).to_be_visible()
        current = json.loads(params.input_value())
        assert "out_rate_hz" in current, f"channel params {current}"
        # A rejected edit shows the node's corrective message in the modal.
        params.fill(json.dumps({**current, "out_rate_hz": "x"}))
        modal.locator("#graph-apply").click()
        expect(modal.locator(".graph-error")).to_contain_text("integer")
        params.fill("{")
        modal.locator("#graph-apply").click()
        expect(modal.locator(".graph-error")).to_contain_text("not valid JSON")
        # A good one (a real change: bandwidth 12500 -> 12000) is put, restarts the job (new
        # stream ids) and re-renders.
        assert current.get("bandwidth_hz") == 12_500, f"channel params {current}"
        params.fill(json.dumps({**current, "bandwidth_hz": 12_000}, indent=2))
        modal.locator("#graph-apply").click()
        expect(modal.locator(".graph-status")).to_contain_text("applied")
        expect(modal.locator(".graph-error")).to_have_text("")
        expect(modal.locator("svg .graph-node")).to_have_count(3)
        graph = http_json(url, secret, "/op", {"op": "graph_get", "job": job})["value"]
        channel = [n for n in graph["nodes"] if n["kind"] == "channel"]
        assert channel and channel[0]["params"]["bandwidth_hz"] == 12_000, f"graph {graph}"
        after = stream_of(url, secret, job, "audio")
        assert after and after != before, f"audio stream {before} -> {after}: job not restarted"
        ok = page.locator("#transcript .line.op", has_text=re.compile(r"^graph_put .*→ ok"))
        expect(ok).to_have_count(1)
        page.keyboard.press("Escape")
        expect(modal).to_be_hidden()
    finally:
        stop_job(url, secret, job)


def ws_connections(url, secret):
    return http_json(url, secret, "/health")["ws_connections"]


def stable_ws_connections(url, secret, within_s=5.0, samples=5):
    """`ws_connections` read until `samples` consecutive reads 0.2 s apart agree (sockets
    of earlier contexts may still be closing), or `within_s` passes; the last reads."""
    deadline = time.monotonic() + within_s
    reads = []
    while True:
        reads = (reads + [ws_connections(url, secret)])[-samples:]
        if (len(reads) == samples and len(set(reads)) == 1) or time.monotonic() > deadline:
            return reads
        time.sleep(0.2)


def scenario_audio_play_opens_binary_socket(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_job(url, secret, "fm_demod", "tone", 100_000_000, 10_000, deviation_hz=5000)
    try:
        row = job_row(page, job)
        play = row.locator("button.play")
        expect(play).to_have_text("▶")
        page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
        # Sockets of earlier contexts may still be closing: compare settled counts.
        idle = stable_ws_connections(url, secret)[-1]
        play.click()
        page.wait_for_function(
            "() => window.zetaAudio && window.zetaAudio.framesPlayed > 0", timeout=3_000)
        playing = stable_ws_connections(url, secret)[-1]
        assert playing >= 2 and playing > idle, f"ws_connections {idle} -> {playing}"
        # The ▶ survives the 2 s jobs re-render as ■ while playing.
        page.evaluate("window.zetaTest.poll()")
        expect(row.locator("button.play")).to_have_text("■")
        row.locator("button.play").click()
        expect(row.locator("button.play")).to_have_text("▶")
        after = stable_ws_connections(url, secret)
        assert after[-1] < playing, f"audio socket still open after ■: {playing} -> {after}"
        assert page.evaluate("window.zetaAudio.playing") is None, "still playing after ■"
    finally:
        stop_job(url, secret, job)


def scenario_fm_demod_does_not_poll_streams(page, url, secret):
    # An fm_demod job's event stream (its squelch events) is looked up once and watched, not
    # looked up on every poll.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    page.wait_for_timeout(500)
    seen = []
    page.on("request", lambda r: seen.append(r.url)
            if r.method == "GET" and r.url.split("?")[0].endswith("/streams") else None)
    job = start_job(url, secret, "fm_demod", "tone", 100_000_000, 10_000, deviation_hz=5000)
    try:
        job_row(page, job)
        page.wait_for_timeout(6_000)
        assert len(seen) <= 3, f"{len(seen)} GET /streams in 6 s: {seen}"
    finally:
        stop_job(url, secret, job)


def frames_in(url, secret, stream, within_ms, max_frames=5):
    """The frames `stream_tail` returns from `stream` within `within_ms`."""
    r = http_json(url, secret, "/op", {"op": "stream_tail", "stream": stream,
                                       "max_frames": max_frames, "timeout_ms": within_ms})
    return r["value"]["frames"]


def scenario_quiet_channel_row_is_tagged_squelch(page, url, secret):
    # Inside the tone recording (99.976-100.024 MHz) but off the tone: the squelch closes, the
    # row is tagged and the audio stream is silent. On the tone: open, untagged, audio flows.
    # The page opens only after the quiet job's single `open: false` event: the tag comes
    # from the gate latched in its job_context, not the (missed) live event.
    quiet = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500)
    loud = None
    try:
        loud = start_job(url, secret, "fm_demod", "tone", 100_000_000, 12_500)
        deadline = time.monotonic() + 5.0
        while True:
            ctx = http_json(url, secret, "/op", {"op": "job_context", "job": quiet})["value"]
            if (ctx.get("squelch") or {}).get("open") is False:
                break
            assert time.monotonic() < deadline, f"no latched squelch in {ctx}"
            time.sleep(0.1)
        select_source(page, url, secret, "tone")
        page.evaluate("window.zetaTest.poll()")
        tag = job_row(page, quiet).locator(".kind .tag.squelch")
        expect(tag).to_be_visible(timeout=3_000)
        expect(tag).to_have_text("squelch")
        assert "closed" in (tag.get_attribute("title") or ""), tag.get_attribute("title")
        audio = stream_of(url, secret, quiet, "audio")
        assert audio, "no audio stream for the quiet fm_demod job"
        got = frames_in(url, secret, audio, 1_000)
        assert got == [], f"{len(got)} audio frames from a squelched channel"
        audio = stream_of(url, secret, loud, "audio")
        assert audio, "no audio stream for the fm_demod job on the tone"
        assert len(frames_in(url, secret, audio, 1_000, max_frames=2)) == 2, \
            "no audio from the fm_demod job on the tone"
        expect(job_row(page, loud).locator(".tag.squelch")).to_have_count(0)
        # The squelch events are not decoded events: neither row counts them.
        for j in (quiet, loud):
            expect(job_row(page, j).locator(".events")).to_have_text("")
    finally:
        for j in (loud, quiet):
            if j:
                stop_job(url, secret, j)


def scenario_every_job_on_the_source_is_watched(page, url, secret):
    # Six measure jobs and four fm_demods on tone: every row counts or is tagged, no row
    # says "not watched", and the page holds ops + waterfall + feed = 3 sockets throughout.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    jobs = []
    try:
        for i in range(6):
            jobs.append(start_job(url, secret, "measure", "tone", 100_000_000, 10_000 + 100 * i))
        for i in range(4):
            jobs.append(start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500 + 100 * i))
        page.evaluate("window.zetaTest.poll()")
        # The page's own (elastic) spectrum job is listed too.
        expect(page.locator('#jobs .job[data-state="running"]').filter(
            has_not_text="spectrum")).to_have_count(10)
        for j in jobs[:6]:
            cell = page.locator(f'#jobs .job[data-id="{j}"] .events')
            expect(cell).not_to_have_text("—")
            page.wait_for_function(
                "(id) => { const e = document.querySelector(`#jobs .job[data-id=\"${id}\"] .events`);"
                " return !!e && parseInt(e.textContent, 10) > 0; }", arg=j, timeout=5_000)
        for j in jobs[6:]:
            expect(job_row(page, j).locator(".kind .tag.squelch")).to_be_visible(timeout=5_000)
        expect(page.locator('#jobs .job .events[title^="not watched"]')).to_have_count(0)
        counts = stable_ws_connections(url, secret)
        assert max(counts) <= 3, f"ws_connections {counts}"
    finally:
        for j in jobs:
            stop_job(url, secret, j)


def scenario_frame_before_poll_is_drawn_after_it(page, url, secret):
    # A measure job started between polls publishes its first event before the page knows
    # the job: the frame is held and counted once the poll lands, not lost.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    page.evaluate("window.zetaTest.pausePolling(true)")
    job = None
    try:
        job = start_job(url, secret, "measure", "tone", 100_000_000, 10_000)
        # The first measure event lands within about a second; wait for two.
        page.wait_for_timeout(2_500)
        assert page.evaluate("window.zetaTest.pendingFrames()") >= 1
        # A poll that started before the job did lands: the frame survives it.
        page.evaluate("window.zetaTest.staleSync()")
        assert page.evaluate("window.zetaTest.pendingFrames()") >= 1
        page.evaluate("window.zetaTest.pausePolling(false)")
        page.evaluate("window.zetaTest.poll()")
        page.wait_for_function(
            "(id) => { const e = document.querySelector(`#jobs .job[data-id=\"${id}\"] .events`);"
            " return !!e && parseInt(e.textContent, 10) >= 1; }", arg=job, timeout=5_000)
        assert page.evaluate("window.zetaTest.pendingFrames()") == 0
    finally:
        page.evaluate("window.zetaTest.pausePolling(false)")
        if job:
            stop_job(url, secret, job)


MAX_WS = 64  # zeta-ws Limits::default().max_ws

HOG_JS = """async ([n, id, proto]) => {
  window.hog = [];
  const opened = [];
  for (let i = 0; i < n; i++) {
    const ws = new WebSocket(location.origin.replace(/^http/, 'ws') + '/stream/' + id,
                             ['zeta', proto]);
    window.hog.push(ws);
    opened.push(new Promise((ok) => { ws.onopen = () => ok(true); ws.onclose = () => ok(false); }));
  }
  return (await Promise.all(opened)).filter(Boolean).length;
}"""


def scenario_ws_busy_is_noted_once(page, url, secret):
    # With every stream slot taken (by another page), the shown source's feed is refused with
    # 503: one error line in the transcript, and the feed opens once a slot frees. The feed is
    # held back over the switch (zetaTest.hideFeeds) and let go once the pool is full: a
    # switch frees the old source's sockets first, so it would take a freed slot.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    page.evaluate("window.zetaTest.hideFeeds(true)")
    page.click('.src-tab[data-id="retune"]')
    expect(page.locator('.src-tab[data-id="retune"]')).to_have_attribute("aria-selected", "true")
    ops = [s for s in http_json(url, secret, "/streams")
           if s["provenance"]["source"] == "node" and s["provenance"]["job"] is None][0]["id"]
    proto = "zeta.bearer." + base64.urlsafe_b64encode(secret.encode()).decode().rstrip("=")
    hog = page.context.new_page()
    try:
        hog.goto(url + "/ui/")
        # ops + waterfall here, no feed yet; plus whatever the hog page holds.
        free = MAX_WS - stable_ws_connections(url, secret)[-1]
        assert hog.evaluate(HOG_JS, [free, ops, proto]) == free, "hog sockets did not open"
        page.evaluate("window.zetaTest.hideFeeds(false)")
        page.evaluate("window.zetaTest.poll()")
        busy = page.locator("#transcript .line.note.error", has_text="out of stream slots")
        expect(busy).to_have_count(1)
        # Retries keep the normal backoff and are not noted again.
        page.wait_for_timeout(3_000)
        expect(busy).to_have_count(1)
        expect(busy.locator(".count")).to_have_text("")
        hog.evaluate("window.hog.forEach((ws) => ws.close())")
        # ops + waterfall + the feed, once the hog's sockets are gone and the backoff (at
        # most 8 s) has run out.
        deadline = time.monotonic() + 15.0
        counts = []
        while time.monotonic() < deadline:
            counts = stable_ws_connections(url, secret)
            if counts[-1] == 3 and len(set(counts)) == 1:
                break
        assert counts and counts[-1] == 3, f"the refused feed never opened: {counts}"
    finally:
        hog.close()


def scenario_squelch_dots_are_not_starved_by_squelch_off_demods(page, url, secret):
    # Five fm_demods with their squelch off answer job_context with no gate; they must not
    # hold the seed slots (SEED_IN_FLIGHT, 4) poll after poll: the normal fm_demod started
    # last is tagged within a few seconds.
    jobs = []
    try:
        for i in range(5):
            j = start_job(url, secret, "fm_demod", "tone", 100_015_000, 13_000 + 100 * i)
            jobs.append(j)
            graph = http_json(url, secret, "/op", {"op": "graph_get", "job": j})["value"]
            demod = [n for n in graph["nodes"] if n["kind"] == "nbfm_demod"]
            assert len(demod) == 1, f"fm_demod graph {graph}"
            demod[0]["params"]["squelch"] = False
            http_json(url, secret, "/op", {"op": "graph_put", "job": j, "graph": graph})
        last = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500)
        jobs.append(last)
        select_source(page, url, secret, "tone")
        page.evaluate("window.zetaTest.poll()")
        expect(job_row(page, last).locator(".kind .tag.squelch")).to_be_visible(timeout=5_000)
    finally:
        for j in jobs:
            stop_job(url, secret, j)


def scenario_no_feed_is_noted_once(page, url, secret):
    # A node without a feed for the shown source (an older node; stubbed here with
    # zetaTest.hideFeeds) is noted once; the outlines still draw, and once the feed is listed
    # the next sync opens it (no second note) and the page learns the job's squelch gate.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    page.evaluate("window.zetaTest.hideFeeds(true)")
    page.click('.src-tab[data-id="retune"]')
    expect(page.locator('.src-tab[data-id="retune"]')).to_have_attribute("aria-selected", "true")
    note = page.locator("#transcript .line.note.error",
                        has_text="no event feed for retune; job events are not shown")
    expect(note).to_have_count(1)
    job = None
    try:
        job = start_job(url, secret, "fm_demod", "retune", 250_015_000, 12_500)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#waterfall .outline[data-key="{job}"]')).to_be_visible()
        page.evaluate("window.zetaTest.poll()")
        page.wait_for_timeout(500)
        expect(note).to_have_count(1)
        page.evaluate("window.zetaTest.hideFeeds(false)")
        page.evaluate("window.zetaTest.poll()")
        page.evaluate("window.zetaTest.poll()")
        # The tag shows only while the gate is closed, and whether the recording holds a signal
        # in this band depends on where earlier scenarios left the source: the page knowing the
        # gate is the proof the feed was opened and the job seeded.
        page.wait_for_function(
            f"() => window.zetaTest.squelchJobs().includes('{job}')", timeout=5_000)
        expect(note).to_have_count(1)
    finally:
        if job:
            stop_job(url, secret, job)


def scenario_ssb_entry_tunes_the_dial(page, url, secret):
    # Ruling R6: SSB (USB) tunes the carrier 1.5 kHz below the selection's centre, 6 kHz wide,
    # and the footer shows that dial frequency while the entry is focused.
    open_menu_on_tone(page, url, secret)
    footer = page.locator("#ctx-footer").text_content()
    centre = int(re.search(r"center_hz=(\d+)", footer).group(1))
    page.focus('#ctx .ctx-item[data-action="ssb_usb"]')
    expect(page.locator("#ctx-footer")).to_contain_text(
        f"center_hz={centre - 1500} bandwidth_hz=6000 sideband=usb")
    page.focus('#ctx .ctx-item[data-action="ssb_lsb"]')
    expect(page.locator("#ctx-footer")).to_contain_text(
        f"center_hz={centre + 1500} bandwidth_hz=6000 sideband=lsb")
    page.click('#ctx .ctx-item[data-action="ssb_usb"]')
    jobs = []
    try:
        # The fit moves the dial only: the band stays the entry's 6000 Hz (a 3000 Hz band
        # about the dial would cut the voice above 1.5 kHz).
        expect(page.locator("#transcript .line.op", has_text=re.compile(
            r"^job_start kind=ssb_demod source=tone center_hz=\d+ "
            r"bandwidth_hz=6000 sideband=usb → job ")).first).to_be_visible()
        jobs = [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
                if j["kind"] == "ssb_demod" and j["source"] == "tone"
                and j["state"]["status"] == "running"]
        assert len(jobs) == 1, f"ssb_demod jobs on tone: {jobs}"
        assert jobs[0]["tuning"]["bandwidth_hz"] == 6000, f"ssb_demod tuning {jobs[0]}"
        expect(job_row(page, jobs[0]["id"]).locator("button.play")).to_have_text("▶")
    finally:
        for j in jobs:
            stop_job(url, secret, j["id"])


TRANSCRIBE_ENTRIES = [
    ("wfm_transcribe", "Transcribe › FM broadcast"),
    ("fm_transcribe", "Transcribe › NBFM voice"),
    ("am_transcribe", "Transcribe › AM"),
    ("ssb_usb_transcribe", "Transcribe › SSB (USB)"),
    ("ssb_lsb_transcribe", "Transcribe › SSB (LSB)"),
]

NEW_DECODE_ENTRIES = [
    ("pocsag_decode", "Decode › POCSAG"),
    ("rds_decode", "Decode › RDS (FM station data)"),
    ("adsb_decode", "Decode › ADS-B (dump1090)"),
    ("acars_decode", "Decode › ACARS"),
]


def scenario_decode_menu_lists_new_kinds(page, url, secret):
    menu = open_menu_on_tone(page, url, secret)
    items = menu.locator(".ctx-item").evaluate_all(
        "els => els.map(e => [e.dataset.action, e.textContent])")
    actions = [a for a, _ in items]
    assert "aprs_decode" in actions, f"menu entries {items}"
    at = actions.index("aprs_decode")
    got = [tuple(i) for i in items[at + 1:at + 1 + len(NEW_DECODE_ENTRIES)]]
    assert got == NEW_DECODE_ENTRIES, f"entries after APRS: {got}; menu {items}"
    page.keyboard.press("Escape")
    expect(menu).to_be_hidden()


def scenario_identify_from_a_box_shows_candidates(page, url, secret):
    # Identify on the POCSAG page: the box (widened to 12.5 kHz) sits on a POCSAG centre, so
    # pocsag_decode is tried first and its proof shows as a candidate ribbon; the proved
    # child keeps running and is stopped here.
    select_source(page, url, secret, "pocsag")
    page.wait_for_function("() => window.zetaTest.wf().painted > 30", timeout=15_000)
    menu = drag_menu(page)
    try:
        page.click('#ctx .ctx-item[data-action="identify"]')
        expect(menu).to_be_hidden()
        ribbon = page.locator("#waterfall .ov-ribbon.candidate", has_text="pocsag_decode: proved")
        expect(ribbon.first).to_be_visible(timeout=10_000)
        # The verdict event is kept as a transcript line, not only a passing ribbon.
        expect(page.locator("#transcript .line.note", has_text="identify ").filter(
            has_text=re.compile(r"POCSAG at 148\.\d{4} MHz: "))).to_have_count(1, timeout=10_000)
        # The suite's `fake_pos` row (ui.rs) is too wide for this box: never tried here.
        cands = page.locator("#waterfall .ov-ribbon.candidate").all_text_contents()
        tried = [c for c in cands if c.startswith("fake_pos:") and "skipped_band" not in c]
        assert not tried, f"fake_pos tried by identify: {cands}"
    finally:
        for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]:
            if (j["source"] == "pocsag" and j["kind"] in ("identify", "pocsag_decode")
                    and j["state"]["status"] == "running"):
                stop_job(url, secret, j["id"])


def running_watch_jobs(url, secret, source):
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    return [j for j in jobs if j["kind"] == "watch" and j["source"] == source
            and j["state"]["status"] == "running"]


def scenario_watch_entry_starts_a_watch_and_outlines_its_band(page, url, secret):
    menu = open_menu_on_tone(page, url, secret)
    expect(page.locator('#ctx .ctx-item[data-action="watch"]')).to_have_text("Watch this band")
    footer = page.locator("#ctx-footer").text_content()
    m = re.search(r"center_hz=(\d+) bandwidth_hz=(\d+)", footer)
    page.click('#ctx .ctx-item[data-action="watch"]')
    expect(menu).to_be_hidden()
    watches = []
    try:
        wait_until(lambda: running_watch_jobs(url, secret, "tone"), 5.0, "a running watch")
        watches = running_watch_jobs(url, secret, "tone")
        assert len(watches) == 1, f"watch jobs on tone: {watches}"
        t = watches[0]["tuning"]
        assert (t["center_hz"], t["bandwidth_hz"]) == (int(m.group(1)), int(m.group(2))), \
            f"tuning {t} for footer {footer!r}"
        el = page.locator(f'#waterfall .outline[data-key="{watches[0]["id"]}"]')
        page.evaluate("window.zetaTest.poll()")
        expect(el).to_have_count(1)
        expect(page.locator(f'#waterfall .job-tag[data-job="{watches[0]["id"]}"] .kind')
               ).to_have_text("watch")
        expect(el.locator(".outline-handle")).to_have_count(0)
        assert "retunable" not in (el.get_attribute("class") or "")
    finally:
        for j in watches:
            stop_job(url, secret, j["id"])


def scenario_owned_jobs_are_hidden_from_the_jobs_pane(page, url, secret):
    select_source(page, url, secret, "tone")
    page.wait_for_function("() => window.zetaTest.wf().painted > 30", timeout=15_000)
    cur = current_capture(url, secret, "tone")
    watch = start_job(url, secret, "watch", "tone", cur["center_hz"], cur["bandwidth_hz"])
    try:
        graph = http_json(url, secret, "/op", {"op": "graph_get", "job": watch})["value"]
        detect = [n for n in graph["nodes"] if n["kind"] == "detect"]
        assert len(detect) == 1, f"detect nodes: {graph['nodes']}"
        detect[0]["params"].update({"width_hz": [0, 48000], "persist_ms": 500, "gone_ms": 60000,
                                      "dc_mask_hz": 0})
        http_json(url, secret, "/op", {"op": "graph_put", "job": watch, "graph": graph})

        def owned():
            jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
            return [j for j in jobs if j.get("owner") == watch
                    and j["state"]["status"] == "running"]

        wait_until(owned, 20.0, "a running job owned by the watch")
        child = owned()[0]["id"]
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#jobs .job[data-id="{watch}"]')).to_have_count(1)
        expect(page.locator(f'#jobs .job[data-id="{child}"]')).to_have_count(0)
        expect(page.locator(f'#waterfall .outline[data-key="{child}"]')).to_have_count(0)
        expect(page.locator("#waterfall .ov-mark")).not_to_have_count(0)
    finally:
        stop_job(url, secret, watch)
        for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]:
            if j.get("owner") == watch and j["state"]["status"] == "running":
                stop_job(url, secret, j["id"])


def detect_params(url, secret, job):
    """The params of `job`'s `detect` node (graph_get)."""
    graph = http_json(url, secret, "/op", {"op": "graph_get", "job": job})["value"]
    detect = [n for n in graph["nodes"] if n["kind"] == "detect"]
    return detect[0]["params"] if len(detect) == 1 else None


def scenario_decode_across_entry_starts_a_watch_with_modes(page, url, secret):
    # Plan 31 spec 4.7, 7.3: the box menu's "Decode across this band…" lists the ready rows
    # that fit the box; one click starts a watch with `modes` on its `detect` node, whose
    # decoder folds under the watch's row and whose subjects merge into one table per watch.
    select_source(page, url, secret, "pocsag")
    wait_span(page, 1.2)
    menu = drag_menu(page, 0.25, 0.75)
    entry = page.locator('#ctx .ctx-item[data-action="decode_across"]')
    expect(entry).to_have_text("Decode across this band…")
    sub = page.locator('#ctx .ctx-sub[data-for="decode_across"]')
    expect(sub).to_be_hidden()
    entry.click()
    item = sub.locator('.ctx-item[data-mode="pocsag_decode"]')
    expect(item).to_have_text("Decode POCSAG pagers across this band")
    # A ready row wider than the box (RDS's 200 kHz) is not offered.
    expect(sub.locator('.ctx-item[data-mode="rds_decode"]')).to_have_count(0)
    item.click()
    expect(menu).to_be_hidden()
    watch = None
    try:
        def detected():
            ws = running_watch_jobs(url, secret, "pocsag")
            if len(ws) != 1:
                return None
            p = detect_params(url, secret, ws[0]["id"])
            return ws[0]["id"] if p and p.get("modes") == ["pocsag_decode"] else None

        watch = wait_until(detected, 10.0, "a watch on pocsag with modes [pocsag_decode]")
        p = detect_params(url, secret, watch)
        assert p["persist_ms"] == 500 and p["width_hz"] == [6250, 25000], p
        expect(page.locator("#transcript .line", has_text="graph_put")).not_to_have_count(0)
        # The fixture's page sits on the capture centre, which `detect` masks (a real radio's DC
        # spike), and its clean FSK reads as two sub-kHz tones under the 0.5x width floor: the
        # test seeds the row's known channel (148.6375 MHz) instead of waiting for a detection.
        graph = http_json(url, secret, "/op", {"op": "graph_get", "job": watch})["value"]
        next(n for n in graph["nodes"] if n["kind"] == "detect")["params"]["seed_known"] = True
        http_json(url, secret, "/op", {"op": "graph_put", "job": watch, "graph": graph})

        def decoder():
            jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
            w = next((j for j in jobs if j["id"] == watch), None)
            kept = [d["job"] for d in (((w or {}).get("context") or {}).get("watch") or {})
                    .get("decoding", [])]
            return next((j["id"] for j in jobs if j["kind"] == "pocsag_decode"
                         and j["source"] == "pocsag" and j["state"]["status"] == "running"
                         and j["id"] in kept), None)

        dec = wait_until(decoder, 20.0, "a pocsag_decode the watch keeps")
        page.evaluate("window.zetaTest.poll()")
        group = page.locator(f'#jobs .job-group[data-plan="{watch}"][data-kind="watch"]')
        expect(group).to_have_count(1)
        expect(group.locator(f'.job[data-id="{dec}"]')).to_have_count(1)
        expect(group.locator("> summary")).to_have_text(
            re.compile(r"pocsag_decode × 1 decoding, 0 waiting"))
        expect(group.locator(f'> summary .job[data-id="{watch}"]')).to_have_count(1)
        open_subjects(page)
        sec = page.locator(f'#subjects section.subject-job[data-job="{watch}"]')
        expect(sec).to_have_count(1)
        expect(sec.locator("h3")).to_have_text(f"watch {watch[-6:]} · addresses")
        expect(sec.locator('th[data-col="channel"]')).to_have_count(1)
        row = sec.locator('tr.subject[data-key="1234567"]')
        expect(row).to_have_count(1, timeout=20_000)
        expect(row.locator('td[data-col="channel"]')).to_have_text("148.6375")
        expect(page.locator(f'#subjects section.subject-job[data-job="{dec}"]')).to_have_count(0)
    finally:
        for w in running_watch_jobs(url, secret, "pocsag"):
            stop_job(url, secret, w["id"])
        wait_until(lambda: not running_jobs(url, secret, "pocsag_decode", "pocsag"), 10.0,
                   "no pocsag_decode left after the watch stopped")


def set_running(url, secret, set_id):
    """`set_id`'s running instance from `set_list` (null when it does not run)."""
    sets = http_json(url, secret, "/op", {"op": "set_list"})["value"]
    return next(s for s in sets if s["id"] == set_id)["running"]


def scenario_set_menu_starts_a_set_and_groups_its_jobs(page, url, secret):
    # Plan 31 spec 5.5, 7.3: "Start set…" opens a role → source picker and sends set_start;
    # the set's jobs fold under one group titled by the set's name, the members on the shown
    # source listed and the others counted; the group's Stop is set_stop after a confirm.
    menu = open_menu_on_tone(page, url, secret)
    entry = page.locator('#ctx .ctx-item[data-action="set"]')
    expect(entry).to_have_text("Start set…")
    entry.click()
    expect(menu).to_be_hidden()
    form = page.locator("#ctx-set")
    expect(form).to_be_visible()
    form.locator("select.set-pick").select_option("two_sources")
    expect(form.locator('select[data-role="a"]')).to_have_value("tone")
    expect(form.locator('select[data-role="b"]')).to_have_value("")
    form.locator('select[data-role="b"]').select_option("cb")
    try:
        with page.expect_request(lambda r: r.method == "POST" and r.url.endswith("/op")
                                 and (r.post_data or "").find('"set_start"') >= 0) as req:
            form.locator("button.start").click()
        body = json.loads(req.value.post_data)
        assert body == {"op": "set_start", "set": "two_sources",
                        "radios": {"a": "tone", "b": "cb"}, "members": None,
                        "replace": []}, f"set_start {body}"
        expect(form).to_be_hidden()

        def running():
            r = set_running(url, secret, "two_sources")
            ms = (r or {}).get("members", [])
            return r if len(ms) == 2 and all(m["state"] == "running" for m in ms) else None

        inst = wait_until(running, 10.0, "two_sources running with 2 members")
        assert inst["radios"] == {"a": "tone", "b": "cb"}, inst
        job_of = {m["id"]: m["job"] for m in inst["members"]}
        line = page.locator("#transcript .line.op", has_text="set_start")
        expect(line.first).to_contain_text(
            "ok (2 started, 0 existing, 0 refused, 0 warnings)")
        page.evaluate("window.zetaTest.poll()")
        group = page.locator('#jobs .job-group[data-plan="two_sources"][data-kind="set"]')
        expect(group).to_have_count(1)
        expect(group.locator("> summary .group-text")).to_have_text(
            re.compile(r"Two sources \(UI tests\) · 1 on tone, \+1 on cb"))
        expect(group.locator(f'.job[data-id="{job_of["m"]}"]')).to_have_count(1)
        expect(group.locator(f'.job[data-id="{job_of["cb20"]}"]')).to_have_count(0)
        expect(page.locator(f'#jobs .job[data-id="{job_of["cb20"]}"]')).to_have_count(0)
        # On cb the same group lists the CB channel and counts the measure.
        page.click('.src-tab[data-id="cb"]')
        expect(page.locator('.src-tab[data-id="cb"]')).to_have_attribute("aria-selected", "true")
        page.evaluate("window.zetaTest.poll()")
        group = page.locator('#jobs .job-group[data-plan="two_sources"][data-kind="set"]')
        expect(group.locator("> summary .group-text")).to_have_text(
            re.compile(r"Two sources \(UI tests\) · 1 on cb, \+1 on tone"))
        expect(group.locator(f'.job[data-id="{job_of["cb20"]}"]')).to_have_count(1)
        expect(group.locator(f'.job[data-id="{job_of["m"]}"]')).to_have_count(0)
        # The group's Stop asks first, then sends set_stop.
        stop = group.locator("> summary button.stop")
        expect(stop).to_have_text("Stop")
        stop.click()
        expect(stop).to_have_text("Stop set?")
        assert set_running(url, secret, "two_sources") is not None, "stopped on one click"
        stop.click()
        wait_until(lambda: set_running(url, secret, "two_sources") is None, 10.0,
                   "two_sources no longer running")
        wait_until(lambda: not running_jobs(url, secret, "measure", "tone")
                   and not running_jobs(url, secret, "fm_demod", "cb"), 10.0,
                   "no measure on tone and no fm_demod on cb after set_stop")
        expect(page.locator("#transcript .line.op", has_text="set_stop").first).to_contain_text(
            "ok (2 stopped, 0 ended, 0 failed)")
    finally:
        try:
            http_json(url, secret, "/op", {"op": "set_stop", "set": "two_sources"})
        except Exception:  # noqa: BLE001 - best-effort cleanup
            pass


def scenario_adsb_entry_asks_for_2400_khz(page, url, secret):
    # ADS-B always asks for 2.4 MHz, whatever was selected; the 48 kHz tone cannot serve it,
    # so the node refuses with out_of_range naming bandwidth_hz and the transcript says so.
    open_menu_on_tone(page, url, secret)
    def is_adsb_start(r):
        if r.method != "POST" or not r.url.endswith("/op"):
            return False
        try:
            body = json.loads(r.post_data or "")
        except ValueError:
            return False
        return body.get("op") == "job_start" and body.get("kind") == "adsb_decode"
    try:
        with page.expect_request(is_adsb_start) as req:
            page.click('#ctx .ctx-item[data-action="adsb_decode"]')
        body = json.loads(req.value.post_data)
        assert body["bandwidth_hz"] == 2_400_000, f"job_start {body}"
        # The op's own head says bandwidth_hz=2400000: match the error after the arrow.
        line = page.locator("#transcript .line", has_text="job_start kind=adsb_decode")
        expect(line.first).to_contain_text(
            re.compile(r"bandwidth_hz=2400000 .*→ out_of_range\b.*\bbandwidth_hz\b"))
        expect(line.first).to_have_class(re.compile(r"\berror\b"))
    finally:
        for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]:
            if j["kind"] == "adsb_decode" and j["state"]["status"] == "running":
                stop_job(url, secret, j["id"])


def open_menu_on_wfm(page, url, secret):
    """Show the `wfm` source and open the menu on a selection over most of its band (the
    middle 80 %, about 190 kHz) and every painted column: about 1.5 s, at least 1 s (a
    classification of 240 kHz IQ takes about 15 s a second of span in a debug build)."""
    select_source(page, url, secret, "wfm")
    page.wait_for_function("() => window.zetaTest.wf().painted > 55", timeout=15_000)
    return drag_menu(page, 0.10, 0.90)


def scenario_wfm_decode_plays_audio(page, url, secret):
    menu = open_menu_on_wfm(page, url, secret)
    page.click('#ctx .ctx-item[data-action="wfm_demod"]')
    expect(menu).to_be_hidden()
    jobs = []
    try:
        expect(page.locator("#transcript .line.op",
                            has_text=re.compile(r"^job_start kind=wfm_demod source=wfm .*"
                                                r"deviation_hz=75000 → job ")).first).to_be_visible()
        jobs = [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
                if j["kind"] == "wfm_demod" and j["source"] == "wfm"
                and j["state"]["status"] == "running"]
        assert len(jobs) == 1, f"wfm_demod jobs on wfm: {jobs}"
        row = job_row(page, jobs[0]["id"])
        play = row.locator("button.play")
        expect(play).to_have_text("▶")
        play.click()
        # The first frames of a demod on this 240 kHz source take 4-7 s in a debug build.
        page.wait_for_function(
            "() => window.zetaAudio && window.zetaAudio.framesPlayed > 0", timeout=15_000)
        rate = page.evaluate("window.zetaAudio.lastRate")
        assert rate == 48_000, f"audio header sample_rate_hz {rate!r}"
        page.evaluate("window.zetaTest.poll()")
        expect(row.locator("button.play")).to_have_text("■")
        row.locator("button.play").click()
        expect(row.locator("button.play")).to_have_text("▶")
    finally:
        for j in jobs:
            stop_job(url, secret, j["id"])


def scenario_what_is_this_names_fm_broadcast(page, url, secret):
    menu = open_menu_on_wfm(page, url, secret)
    page.click('#ctx .ctx-item[data-action="classify"]')
    expect(menu).to_be_hidden()
    line = page.locator("#transcript .line.op", has_text=re.compile(r"^classify .*→ ok"))
    expect(line.first).to_be_visible(timeout=45_000)
    expect(line.first).to_contain_text("L1 wfm ")
    expect(line.first).to_contain_text("L2 FM Broadcast Radio ")
    l1 = page.locator("#waterfall .ov-label", has_text=re.compile(r"^L1 wfm "))
    l2 = page.locator("#waterfall .ov-label", has_text=re.compile(r"^L2 FM Broadcast Radio "))
    expect(l1).to_have_count(1)
    expect(l2).to_have_count(1)
    expect(l1).to_be_visible()
    expect(l2).to_be_visible()
    # Only the top layer-2 name is drawn, one line under the layer-1 label.
    expect(page.locator('#waterfall .ov-label[data-layer="2"]')).to_have_count(1)
    b1, b2 = l1.bounding_box(), l2.bounding_box()
    assert b1["y"] + b1["height"] <= b2["y"] + 1 and b2["y"] - b1["y"] < 2 * b1["height"] + 4, \
        f"L2 label not one line under L1: {b1} {b2}"
    expect(page.locator("#transcript .line.note", has_text=re.compile(r"^L1 wfm ")).first) \
        .to_be_visible()
    expect(page.locator("#transcript .line.note",
                        has_text=re.compile(r"^L2 FM Broadcast Radio \d\.\d\d")).first).to_be_visible()
    if os.environ.get("ZETA_UI_SHOTS"):
        page.screenshot(path=os.path.join(os.environ["ZETA_UI_SHOTS"], "wfm.png"))


# ---- transcription (speech Task 7) -------------------------------------------------------
# The `speech` source: speech-like bursts, wideband FM, 240 kHz at 104.7 MHz, looping with a
# 2 s pause. The fake daemon hears SPOKEN in every segment.

SPOKEN = "what sources are there"
SPEECH_HZ = 104_700_000
TRANSCRIPT_WITHIN_MS = 20_000


def attached_jobs(url, secret, demod):
    return [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
            if j.get("input") == demod]


def wait_attached(url, secret, demod, within_s=10.0):
    """The one job attached to `demod`, once job_list shows it."""
    deadline = time.monotonic() + within_s
    while True:
        got = attached_jobs(url, secret, demod)
        if got or time.monotonic() > deadline:
            assert len(got) == 1, f"jobs attached to {demod}: {got}"
            return got[0]
        time.sleep(0.2)


def assert_attached_row(page, demod, attached):
    """The attached job's row reads `↳ transcribe`, sits right under its input's row and
    has no play (or Transcribe) button."""
    row = job_row(page, attached)
    expect(row).to_have_class(re.compile(r"\battached\b"))
    expect(row.locator(".kind")).to_have_text("↳ transcribe")
    expect(row.locator("button.play")).to_have_count(0)
    expect(row.locator("button.transcribe")).to_have_count(0)
    under = page.evaluate(
        """(id) => { const r = document.querySelector(`#jobs .job[data-id="${id}"]`);
                     const p = r && r.previousElementSibling;
                     return p ? p.dataset.id : null; }""", attached)
    assert under == demod, f"attached row under {under!r}, want its input {demod!r}"
    return row


def scenario_transcript_ribbon_appears(page, url, secret):
    select_source(page, url, secret, "speech")
    page.wait_for_function("() => window.zetaTest.wf().painted > 55", timeout=15_000)
    menu = drag_menu(page, 0.10, 0.90)
    items = menu.locator(".ctx-item").evaluate_all(
        "els => els.map(e => [e.dataset.action, e.textContent])")
    for entry in TRANSCRIBE_ENTRIES:
        assert list(entry) in items, f"{entry} not in the menu: {items}"
    page.click('#ctx .ctx-item[data-action="wfm_transcribe"]')
    clicked = time.monotonic()
    expect(menu).to_be_hidden()
    demod = None
    attached = None
    try:
        expect(page.locator("#transcript .line.op",
                            has_text=re.compile(r"^job_start kind=wfm_demod source=speech .*"
                                                r"deviation_hz=75000 → job ")).first).to_be_visible()
        demods = [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
                  if j["kind"] == "wfm_demod" and j["source"] == "speech"
                  and j["state"]["status"] == "running"]
        assert len(demods) == 1, f"wfm_demod jobs on speech: {demods}"
        demod = demods[0]["id"]
        job = wait_attached(url, secret, demod)
        attached = job["id"]
        assert job["kind"] == "transcribe", f"attached job {job}"
        assert job["tuning"] == demods[0]["tuning"], f"tuning {job['tuning']} vs {demods[0]}"
        expect(page.locator("#transcript .line.op",
                            has_text=re.compile(r"^job_attach kind=transcribe job=" + demod
                                                + r" .*→ job ")).first).to_be_visible()
        assert_attached_row(page, demod, attached)
        ribbon = page.locator("#waterfall .ov-ribbon.transcript", has_text=SPOKEN)
        left_ms = max(1, int(TRANSCRIPT_WITHIN_MS - (time.monotonic() - clicked) * 1000))
        expect(ribbon.first).to_be_visible(timeout=left_ms)
        expect(page.locator(f'#waterfall .job-tag[data-job="{demod}"] .snippet')
               ).to_contain_text(SPOKEN[:20])
        title = ribbon.first.get_attribute("title") or ""
        assert re.search(r"\ben\b", title) and re.search(r"\d+ ms", title), \
            f"ribbon title {title!r} lacks language and duration"
        # The vad transitions are counted in the attached row, never drawn: the count runs
        # ahead of the transcripts (an onset before each), and no ribbon carries one.
        page.wait_for_function(
            """(id) => {
              const e = document.querySelector(`#jobs .job[data-id="${id}"] .events`);
              const shown = document.querySelectorAll('#waterfall .ov-ribbon.transcript').length;
              return !!e && parseInt(e.textContent, 10) > shown;
            }""", arg=attached, timeout=5_000)
        expect(page.locator("#waterfall .ov-ribbon", has_text='"vad"')).to_have_count(0)
        if os.environ.get("ZETA_UI_SHOTS"):
            page.screenshot(path=os.path.join(os.environ["ZETA_UI_SHOTS"], "transcript.png"))
        # Stopping the demodulator ends the attached job: both rows finish.
        job_row(page, demod).locator("button.stop").click()
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#jobs .job[data-id="{demod}"] .state')).to_have_text("stopped")
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#jobs .job[data-id="{attached}"] .state')).to_have_text("stopped")
    finally:
        for j in (attached, demod):
            if j:
                stop_job(url, secret, j)


def scenario_job_row_transcribe_button(page, url, secret):
    select_source(page, url, secret, "speech")
    demod = start_job(url, secret, "wfm_demod", "speech", SPEECH_HZ, 200_000,
                      deviation_hz=75_000)
    attached = None
    try:
        row = job_row(page, demod)
        button = row.locator("button.transcribe")
        expect(button).to_have_text("Transcribe")
        button.click()
        attached = wait_attached(url, secret, demod)["id"]
        assert_attached_row(page, demod, attached)
        expect(page.locator("#transcript .line.op",
                            has_text=re.compile(r"^job_attach kind=transcribe job=" + demod
                                                + r" .*→ job ")).first).to_be_visible()
    finally:
        for j in (attached, demod):
            if j:
                stop_job(url, secret, j)


def scenario_transcribe_without_backend_fails_visibly(page, url, secret):
    nostt = os.environ.get("ZETA_UI_URL_NOSTT")
    assert nostt, "ZETA_UI_URL_NOSTT (a node without --stt-socket) is not set"
    nostt = nostt.rstrip("/")
    page.set_viewport_size(WIDE)
    login(page, nostt, secret)
    page.click('.src-tab[data-id="speech"]')
    demod = start_job(nostt, secret, "wfm_demod", "speech", SPEECH_HZ, 200_000,
                      deviation_hz=75_000)
    try:
        # job_attach checks the stage's readiness (7e08b62): refused at once, no job made.
        job_row(page, demod).locator("button.transcribe").click()
        expect(page.locator("#transcript .line.error", has_text="speech backend")
               ).not_to_have_count(0)
        assert attached_jobs(nostt, secret, demod) == [], "no attached job on a node without STT"
    finally:
        stop_job(nostt, secret, demod)


# ---- voice (Task 11) -----------------------------------------------------------------------


def record_and_stop(page, hold_ms):
    """Click #mic, check it records (red dot) for `hold_ms`, then click it again."""
    mic = page.locator("#mic")
    expect(mic).to_be_enabled()
    mic.click()
    expect(mic).to_have_class(re.compile(r"\brecording\b"))
    page.wait_for_timeout(hold_ms)
    mic.click()
    expect(mic).not_to_have_class(re.compile(r"\brecording\b"))


def start_node(build, state_dir, meta, secret, port=0):
    """Start `zeta serve` (ZETA_BIN) on 127.0.0.1:`port` with build id `build`; (proc, url)."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("ZETA_")}
    env["ZETA_BUILD_ID"] = build
    proc = subprocess.Popen(
        [os.environ["ZETA_BIN"], "--state-dir", state_dir, "--source-file", f"tone={meta}",
         "serve", "--listen", f"127.0.0.1:{port}", "--secret", secret],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    deadline = time.monotonic() + 20
    while True:
        line = proc.stderr.readline()
        if not line or time.monotonic() > deadline:
            proc.kill()
            raise AssertionError(f"node with build {build!r} did not report a listen address")
        m = re.search(r"listening on 127\.0\.0\.1:(\d+)", line)
        if m:
            break
    threading.Thread(target=lambda: [None for _ in proc.stderr], daemon=True).start()
    return proc, f"http://127.0.0.1:{m.group(1)}"


def stop_node(proc):
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def scenario_page_reloads_when_the_node_is_rebuilt(page, url, secret):
    """A node restarted under an open page with another build id: the page reloads itself
    (a single-page app otherwise keeps the old code running against the new node)."""
    meta = os.environ["ZETA_UI_REBUILD_META"]
    state_dir = os.path.join(os.path.dirname(os.environ["ZETA_UI_STATE_DIR"]), "rebuild-state")
    proc, node = start_node("test-build-a", state_dir, meta, secret)
    try:
        login(page, node, secret)
        assert http_json(node, secret, "/health")["build"] == "test-build-a"
        page.evaluate("window.__beforeRebuild = 1")
        port = node.rsplit(":", 1)[1]
        stop_node(proc)
        proc, node2 = start_node("test-build-b", state_dir, meta, secret, port=port)
        assert node2 == node, f"restarted on {node2}, wanted {node}"
        # The page notices the new build on its next node check and reloads: the marker set on
        # the old document is gone, and the stored secret logs the new document in again.
        page.wait_for_function("() => window.__beforeRebuild === undefined", timeout=20_000)
        expect(page.locator("#login")).to_be_hidden(timeout=10_000)
        expect(page.locator("#app")).to_be_visible()
        assert page.evaluate("window.zetaTest.build()") == "test-build-b"
    finally:
        stop_node(proc)


def scenario_mic_round_trip(page, url, secret):
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    is_post = lambda path: lambda r: r.method == "POST" and r.url.endswith(path)  # noqa: E731
    with page.expect_request(is_post("/stt")) as stt_req, \
            page.expect_request(is_post("/ask")) as ask_req:
        record_and_stop(page, 1000)
    # The upload is a 16 kHz mono s16le WAV with a 44-byte header, about 1 s long.
    wav = stt_req.value.post_data_buffer
    assert stt_req.value.headers.get("content-type") == "audio/wav", stt_req.value.headers
    assert wav[:4] == b"RIFF" and wav[8:16] == b"WAVEfmt ", f"header {wav[:16]!r}"
    fmt, channels, rate, byte_rate, align, bits = struct.unpack("<HHIIHH", wav[20:36])
    assert (fmt, channels, rate, byte_rate, align, bits) == (1, 1, 16_000, 32_000, 2, 16), \
        f"fmt {(fmt, channels, rate, byte_rate, align, bits)}"
    assert wav[36:40] == b"data", f"data tag {wav[36:40]!r}"
    data_len = struct.unpack("<I", wav[40:44])[0]
    assert data_len == len(wav) - 44 and struct.unpack("<I", wav[4:8])[0] == 36 + data_len
    seconds = data_len / 32_000
    assert 0.5 <= seconds <= 3.0, f"recorded {seconds:.2f} s for a 1 s hold"
    samples = struct.unpack(f"<{data_len // 2}h", wav[44:44 + data_len - data_len % 2])
    peak = max((abs(x) for x in samples), default=0)
    assert peak > 1000, f"the fake microphone's tone recorded as silence (peak {peak})"
    # The transcript went through the command line (it is in its history) to /ask.
    assert json.loads(ask_req.value.post_data)["request"] == SPOKEN, ask_req.value.post_data
    expect(page.locator("#transcript .line.user", has_text=SPOKEN)).to_be_visible()
    expect(page.locator("#transcript .line.assistant.error")).to_contain_text("--llm-url")
    page.focus("#cmd")
    page.keyboard.press("ArrowUp")
    expect(page.locator("#cmd")).to_have_value(SPOKEN)
    expect(page.locator("#mic-status")).to_have_text("")
    expect(page.locator("#mic")).to_be_enabled()


def scenario_mic_without_daemon_shows_message(page, url, secret):
    nostt = os.environ.get("ZETA_UI_URL_NOSTT")
    assert nostt, "ZETA_UI_URL_NOSTT (a node without --stt-socket) is not set"
    page.set_viewport_size(WIDE)
    login(page, nostt.rstrip("/"), secret)
    asks = []
    page.on("request", lambda r: asks.append(r.url) if r.url.endswith("/ask") else None)
    with page.expect_response(lambda r: r.url.endswith("/stt")) as stt_resp:
        record_and_stop(page, 500)
    assert stt_resp.value.status == 503, f"/stt answered {stt_resp.value.status}"
    expect(page.locator("#mic-status")).to_have_text("no speech daemon on this node")
    # The button stays usable for a retry, and nothing went to /ask.
    expect(page.locator("#mic")).to_be_enabled()
    page.wait_for_timeout(300)
    assert not asks, f"/ask requested without a transcript: {asks}"
    record_and_stop(page, 300)
    expect(page.locator("#mic-status")).to_have_text("no speech daemon on this node")


# ---- bulk operations (plan 21 Task 8) --------------------------------------------------------
# The typed `plan start` and `allow stops`, and the duplicate error's text, on the main server
# (no model). `cb` holds ch20, ch59 and ch60 of uhf_cb_au in its 48 kHz capture.

def running_kind(url, secret, kind, source):
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    return [j["id"] for j in jobs if j["kind"] == kind and j["source"] == source
            and j["state"]["status"] == "running"]


def scenario_plan_start_typed_renders_counts_and_refusals(page, url, secret):
    select_source(page, url, secret, "cb")
    started = []
    try:
        run_cmd(page, "plan start uhf_cb_au fm_demod ch 20,59")
        expect(page.locator("#transcript .line.op", has_text="plan_start").filter(
            has_text="ok (2 started, 0 existing, 0 refused)")).not_to_have_count(0)
        started = running_kind(url, secret, "fm_demod", "cb")
        assert len(started) == 2, f"two fm_demod jobs running: {started}"
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator('#jobs .job[data-state="running"]', has_text="fm_demod")
               ).to_have_count(2)
        run_cmd(page, "plan start uhf_cb_au fm_demod ch 20,59")
        expect(page.locator("#transcript .line.op", has_text="plan_start").filter(
            has_text="ok (0 started, 2 existing, 0 refused)")).not_to_have_count(0)
        run_cmd(page, "plan start uhf_cb_au fm_demod ch 1")
        expect(page.locator("#transcript .line.error", has_text="plan_start")).not_to_have_count(0)
        assert running_kind(url, secret, "fm_demod", "cb") == started, "nothing partial"
    finally:
        for j in running_kind(url, secret, "fm_demod", "cb"):
            stop_job(url, secret, j)


def scenario_allow_stops_toggle_is_noted(page, url, secret):
    select_source(page, url, secret, "cb")
    run_cmd(page, "allow stops on")
    expect(page.locator("#transcript .line.note:not(.error)", has_text="allow stops: on")
           ).to_have_count(1)
    run_cmd(page, "allow stops off")
    expect(page.locator("#transcript .line.note:not(.error)", has_text="allow stops: off")
           ).to_have_count(1)
    run_cmd(page, "allow deletes on")
    expect(page.locator("#transcript .line.note:not(.error)", has_text="allow deletes: on")
           ).to_have_count(1)
    run_cmd(page, "allow deletes off")
    expect(page.locator("#transcript .line.note:not(.error)", has_text="allow deletes: off")
           ).to_have_count(1)


def scenario_duplicate_start_renders_the_job(page, url, secret):
    select_source(page, url, secret, "tone")
    op = ('{"op":"job_start","kind":"measure","source":"tone","center_hz":100000000,'
          '"bandwidth_hz":20000,"deviation_hz":null,"sideband":null,"elastic":false,'
          '"replace":[]}')
    try:
        run_cmd(page, op)
        expect(page.locator("#transcript .line.op", has_text="measure").filter(
            has_text="→ job ")).not_to_have_count(0)
        run_cmd(page, op)
        expect(page.locator("#transcript .line.error", has_text="duplicate of job")
               ).not_to_have_count(0)
    finally:
        for j in running_kind(url, secret, "measure", "tone"):
            stop_job(url, secret, j)


# ---- the agent behind /ask (plan 21 Task 9) -------------------------------------------------
# Against ZETA_UI_URL_LLM: a node with only `tone` and `cb` whose `--llm-url` is ui.rs's
# `zeta-fake-llm`, answering by request: "think" lists the sources (with reasoning and content)
# then answers "one source"; "stop the measure" calls job_stop on 01NOPE then answers "I was not
# allowed to stop it."; "every CB channel" lists the sources, plan_starts fm_demod on ch20 and
# ch59 of uhf_cb_au on `cb`, then answers "Started 2 channels."; "switch to the plain theme"
# calls the page-only view_set {key: theme, value: plain}, then answers "Switched to plain.";
# "radio fallback probe" job_starts a measure on `tone` (center 100 MHz, 12.5 kHz), then
# answers "Started it on tone."; the box menu's "what is it?" answers "A tone.".

def llm_url():
    url = os.environ.get("ZETA_UI_URL_LLM")
    assert url, "ZETA_UI_URL_LLM (a node with --llm-url on the fake model) is not set"
    return url.rstrip("/")


NO_ALLOW = {"stop_jobs": False, "replace": False, "delete_captures": False,
            "accept_signatures": False}


def capture_asks(page):
    """The JSON bodies of the page's POST /ask requests, appended as they are sent."""
    bodies = []
    page.on("request", lambda r: bodies.append(json.loads(r.post_data))
            if r.url.endswith("/ask") and r.method == "POST" else None)
    return bodies


def scenario_ask_sends_32_turns_and_the_allowance(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    asks = capture_asks(page)
    run_cmd(page, "think")
    answer = page.locator("#transcript .line.assistant", has_text="one source")
    expect(answer).to_have_count(1)
    assert len(asks) == 1, f"one /ask request: {asks}"
    assert asks[0]["max_turns"] == 32, asks[0]
    assert asks[0]["allow"] == NO_ALLOW, asks[0]
    assert asks[0]["context"] == {"focus": "cb", "main": "rf", "radio_policy": "focus",
                                  "radio_prefer": []}, asks[0]
    expect(answer).not_to_contain_text("[may stop jobs]")
    turn = page.locator("#transcript details.turn", has_text="turn 1 (1 call)")
    expect(turn).to_have_count(1)
    expect(turn.locator("summary")).to_have_text("turn 1 (1 call)")
    expect(turn).to_contain_text("I should list the sources first.")


def scenario_not_allowed_shows_the_unlock_and_reruns_with_the_allowance(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    asks = capture_asks(page)
    run_cmd(page, "stop the measure")
    expect(page.locator("#transcript .line.assistant",
                        has_text="I was not allowed to stop it.")).to_have_count(1)
    # A turn with only a tool call (no reasoning or content: thinking off) still shows,
    # with the call and its refusal.
    turn = page.locator("#transcript details.turn", has_text="turn 1 (1 call)").first
    expect(turn).to_contain_text("→ job_stop")
    expect(turn).to_contain_text("error:")
    note = page.locator("#transcript .line.note",
                        has_text="the agent was not allowed to stop or replace jobs")
    expect(note).to_have_count(1)
    button = note.get_by_role("button", name="allow stops and ask again")
    expect(button).to_have_count(1)
    button.click()
    expect(page.locator("#transcript .line.assistant", has_text="[may stop jobs]")
           ).to_have_count(1)
    assert len(asks) == 2, f"two /ask requests: {asks}"
    assert asks[0]["allow"]["stop_jobs"] is False, asks[0]
    assert asks[1]["allow"]["stop_jobs"] is True, asks[1]
    assert asks[1]["request"] == "stop the measure", asks[1]
    assert asks[1]["allow"] == {"stop_jobs": True, "replace": True, "delete_captures": False,
                                "accept_signatures": False}, \
        asks[1]
    # The note's button disables once clicked, and the unlock was for that request only:
    # the session's `allow stops` switch is still off for the next plain ask.
    expect(button).to_be_disabled()
    run_cmd(page, "think")
    answer = page.locator("#transcript .line.assistant", has_text="one source")
    expect(answer).to_have_count(1)
    expect(answer).not_to_contain_text("[may stop jobs]")
    assert len(asks) == 3, f"three /ask requests: {asks}"
    assert asks[2]["allow"] == NO_ALLOW, asks[2]


def scenario_not_allowed_delete_offers_the_delete_unlock(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    asks = capture_asks(page)
    run_cmd(page, "delete the capture")
    expect(page.locator("#transcript .line.assistant",
                        has_text="I was not allowed to delete it.")).to_have_count(1)
    note = page.locator("#transcript .line.note",
                        has_text="the agent was not allowed to delete captures")
    expect(note).to_have_count(1)
    button = note.get_by_role("button", name="allow deletes and ask again")
    expect(button).to_have_count(1)
    button.click()
    expect(page.locator("#transcript .line.assistant", has_text="[may delete captures]")
           ).to_have_count(1)
    assert len(asks) == 2, f"two /ask requests: {asks}"
    assert asks[0]["allow"] == NO_ALLOW, asks[0]
    assert asks[1]["allow"] == {"stop_jobs": False, "replace": False, "delete_captures": True,
                                "accept_signatures": False}, \
        asks[1]
    # The switch: every following ask carries delete_captures until turned off.
    run_cmd(page, "allow deletes on")
    run_cmd(page, "think")
    expect(page.locator("#transcript .line.assistant", has_text="one source")).to_have_count(1)
    assert len(asks) == 3, f"three /ask requests: {asks}"
    assert asks[2]["allow"] == {"stop_jobs": False, "replace": False, "delete_captures": True,
                                "accept_signatures": False}, \
        asks[2]


def scenario_ask_switches_the_theme_with_a_job_running(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    # A running job on the source must not stand in the way of a view setting.
    job = start_job(url, secret, "measure", "cb", 476_900_000, 10_000)
    try:
        page.wait_for_function(
            "() => document.documentElement.dataset.theme === 'console'", timeout=5_000)
        asks = capture_asks(page)
        run_cmd(page, "switch to the plain theme")
        expect(page.locator("#transcript .line.assistant", has_text="Switched to plain.")
               ).to_have_count(1)
        page.wait_for_function(
            "() => document.documentElement.dataset.theme === 'plain'", timeout=5_000)
        expect(page.locator('html[data-theme="plain"]')).to_have_count(1)
        expect(view_line(page, "theme = plain")).to_be_visible()
        assert len(asks) == 1, f"one /ask request: {asks}"
        assert asks[0]["page_tools"] is True, asks[0]
        assert job_record(url, secret, job)["state"]["status"] == "running", "the job kept running"
    finally:
        stop_job(url, secret, job)


def scenario_ask_fans_out_through_plan_start(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    try:
        run_cmd(page, "an FM decoder on every CB channel")
        expect(page.locator("#transcript .line.assistant", has_text="Started 2 channels.")
               ).to_have_count(1)
        expect(page.locator("#transcript .line", has_text="[agent] ").filter(
            has_text="plan_start").filter(has_text="ok (2 started")).not_to_have_count(0)
        started = running_kind(url, secret, "fm_demod", "cb")
        assert len(started) == 2, f"two fm_demod jobs running: {started}"
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator('#jobs .job[data-state="running"]', has_text="fm_demod")
               ).to_have_count(2)
    finally:
        for j in running_kind(url, secret, "fm_demod", "cb"):
            stop_job(url, secret, j)
    assert not running_kind(url, secret, "fm_demod", "cb"), "the fm_demod jobs stopped"


def scenario_stop_all_is_one_operation_and_keeps_the_spectrum(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    measure = lambda: start_job(url, secret, "measure", "cb", 476_900_000, 10_000)
    jobs = lambda kind: running_kind(url, secret, kind, "cb")
    try:
        # Through the agent: refused until stops are allowed, then one call.
        measure()
        run_cmd(page, "stop all jobs")
        expect(page.locator("#transcript .line.note",
                            has_text="the agent was not allowed to stop or replace jobs")
               ).to_have_count(1)
        assert len(jobs("measure")) == 1, "a refused jobs_stop stopped nothing"
        run_cmd(page, "allow stops on")
        run_cmd(page, "stop all jobs")
        expect(page.locator("#transcript .line", has_text="[agent] ").filter(
            has_text="jobs_stop").filter(has_text="1 stopped, 0 failed")).to_have_count(1)
        assert not jobs("measure"), "the agent's jobs_stop stopped the job"
        # Typed: the exact words, no model.
        measure()
        run_cmd(page, "stop all")
        expect(page.locator("#transcript .line.op", has_text="jobs_stop").filter(
            has_text="1 stopped, 0 failed")).to_have_count(2)
        assert not jobs("measure"), "the typed stop all stopped the job"
        assert jobs("spectrum"), "the page's elastic spectrum keeps running"
    finally:
        for j in jobs("measure"):
            stop_job(url, secret, j)


# ---- channel plans (plan 13 Task 4) ---------------------------------------------------------
# `cb` is the tone recording labelled at 476.9 MHz, channel 20 of uhf_cb_au.

def scenario_channel_plan_labels(page, url, secret):
    select_source(page, url, secret, "cb")
    job = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    try:
        row = job_row(page, job)
        expect(row.locator(".tag.ch")).to_have_text("ch20", timeout=3_000)
        label = page.locator(f'#waterfall .job-tag[data-job="{job}"] .channel')
        expect(label).to_have_text("ch20")
        set_band(page, 476_900_000, 40_000)
        ticks = page.locator("#waterfall .wf-axis .tick.ch")
        expect(ticks.first).to_be_visible()
        texts = ticks.all_text_contents()
        for n in ("20", "59", "60"):
            assert n in texts, f"channel {n} not on the axis: {texts}"
        run_cmd(page, "view set plan none")
        expect(view_line(page, "plan = none")).to_be_visible()
        expect(job_row(page, job).locator(".tag.ch")).to_have_count(0)
        expect(ticks).to_have_count(0)
        expect(label).to_have_text("")
        run_cmd(page, "view set plan bogus")
        expect(view_line(page, "auto, none, uhf_cb_au", error=True)).to_be_visible()
        run_cmd(page, "view reset")
        expect(job_row(page, job).locator(".tag.ch")).to_have_text("ch20")
        expect(label).to_have_text("ch20")
    finally:
        stop_job(url, secret, job)


# ---- the audio mix (plan 22 Task 5) -----------------------------------------------------
# `cb` is the tone at ch20, which holds a demodulator's squelch open. A mix is a job on the
# node's placeholder source ("node") with no band; Listen to all plays one.

def mix_jobs(url, secret):
    return running_jobs(url, secret, "mix", "node")


def stop_mixes(url, secret):
    for j in mix_jobs(url, secret):
        stop_job(url, secret, j["id"])


def http_mix_start(url, secret, sources):
    return http_json(url, secret, "/op", {"op": "mix_start", "sources": sources})["value"]


def scenario_listen_to_all_starts_one_mix_and_plays(page, url, secret):
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    demod = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    try:
        button = page.locator("#listen-all")
        expect(button).to_have_attribute("aria-pressed", "false")
        button.click()
        page.wait_for_function(
            "() => window.zetaAudio && window.zetaAudio.framesPlayed > 0", timeout=5_000)
        mixes = mix_jobs(url, secret)
        assert len(mixes) == 1, f"one mix running: {mixes}"
        mix = mixes[0]["id"]
        assert page.evaluate("window.zetaAudio.playing") == mix, "the mix is what plays"
        expect(button).to_have_attribute("aria-pressed", "true")
        expect(button).to_have_text("Stop listening")
        row = job_row(page, mix)
        expect(row.locator(".band")).to_have_count(0)
        button.click()
        wait_until(lambda: page.evaluate("window.zetaAudio.playing") is None, 3.0, "stopped")
        expect(button).to_have_attribute("aria-pressed", "false")
        expect(button).to_have_text("Listen to all")
        assert [j["id"] for j in mix_jobs(url, secret)] == [mix], "the mix is still running"
        button.click()
        page.wait_for_function(
            "(m) => window.zetaAudio.playing === m && window.zetaAudio.framesPlayed > 0",
            arg=mix, timeout=5_000)
        assert [j["id"] for j in mix_jobs(url, secret)] == [mix], "the same mix, only one"
        # The ops feed is not the mix's stream: the node lists streams by id, the ops stream
        # first, so list the job's streams first (as a node that started the mix before its
        # ops stream would) and reload with the mix running: the ops subscription resolves
        # among them, and another caller's op must still show.
        def job_streams_first(route):
            listed = json.loads(route.fetch().body())
            listed.sort(key=lambda i: 0 if (i.get("provenance") or {}).get("job") else 1)
            route.fulfill(json=listed)

        page.route("**/streams", job_streams_first)
        page.reload()
        expect(page.locator("#login")).to_be_hidden()
        page.wait_for_timeout(1_000)
        probe = start_job(url, secret, "measure", "cb", 476_900_000, 10_000)
        stop_job(url, secret, probe)
        expect(page.locator("#transcript .line.op", has_text="[http] job_start kind=measure")
               .first).to_be_visible(timeout=5_000)
    finally:
        stop_job(url, secret, demod)
        stop_mixes(url, secret)


def scenario_mix_lights_the_speaker_and_names_the_last_heard(page, url, secret):
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    demod = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    try:
        row = job_row(page, demod)
        expect(row.locator(".tag.ch")).to_have_text("ch20", timeout=3_000)
        idle = stable_ws_connections(url, secret)[-1]
        first = http_mix_start(url, secret, None)
        page.evaluate("window.zetaTest.poll()")
        settled = stable_ws_connections(url, secret)[-1]
        assert settled == idle + 1, f"one socket on the mix: {idle} -> {settled}"
        newest = http_mix_start(url, secret, ["cb"])
        assert newest != first, "a different set is another mix"
        page.evaluate("window.zetaTest.poll()")
        wait_until(lambda: page.evaluate("window.zetaMix.job") == newest, 5.0,
                   "the newest mix is the one watched")
        again = stable_ws_connections(url, secret)[-1]
        assert again == idle + 1, f"still one socket with two mixes: {idle} -> {again}"
        speaking = page.locator(f'#jobs .job[data-id="{demod}"].speaking')
        expect(speaking).to_be_visible(timeout=5_000)
        expect(page.locator("#waterfall .outline.speaking")).to_have_count(1)
        assert "hot" not in (row.get_attribute("class") or "").split(), "speaking is not hot"
        now = page.locator(f'#jobs .job[data-id="{newest}"] .mix-now')
        expect(now).to_have_text("ch20 now", timeout=5_000)
        stop_job(url, secret, demod)
        wait_until(lambda: page.evaluate("window.zetaMix.active.length") == 0, 4.0,
                   "nothing active after the demodulator stopped")
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(".speaking")).to_have_count(0)
        expect(now).to_have_text(re.compile(r"^last ch20 \d+ s ago$"), timeout=3_000)
        page.reload()
        expect(page.locator("#login")).to_be_hidden()
        reseeded = page.locator(f'#jobs .job[data-id="{newest}"] .mix-now')
        expect(reseeded).to_have_text(re.compile(r"^last ch20 \d+ s ago$"), timeout=8_000)
    finally:
        stop_job(url, secret, demod)
        stop_mixes(url, secret)


def scenario_mix_active_before_the_socket_opens_still_lights(page, url, secret):
    # The mix says `active` only when it changes, so the seed must read after the node has
    # subscribed the socket: a change between an earlier read and the subscription reaches
    # neither. Sockets are held back here, so the demodulator starts talking in that gap.
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    demod = None
    page.evaluate("""() => {
      const Real = window.WebSocket;
      window.zetaHeldSockets = { Real, open: [] };
      window.WebSocket = function (url, protocols) {
        const held = { close() { if (held.real) held.real.close(); else held.closed = true; } };
        window.zetaHeldSockets.open.push(() => {
          if (held.closed) return;
          const ws = new Real(url, protocols);
          held.real = ws;
          for (const on of ["onopen", "onmessage", "onclose"]) {
            ws[on] = (ev) => held[on] && held[on](ev);
          }
        });
        return held;
      };
    }""")
    release = """() => {
      window.WebSocket = window.zetaHeldSockets.Real;
      for (const open of window.zetaHeldSockets.open) open();
      window.zetaHeldSockets.open = [];
    }"""
    try:
        mix = http_mix_start(url, secret, ["cb"])
        page.evaluate("window.zetaTest.poll()")
        wait_until(lambda: page.evaluate("window.zetaHeldSockets.open.length") > 0, 5.0,
                   "the mix's socket was asked for")
        page.wait_for_timeout(300)  # whatever the page reads now, it reads an idle mix
        demod = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
        wait_until(lambda: http_json(url, secret, "/op", {"op": "job_context", "job": mix})
                   ["value"]["mix"]["active"], 5.0, "the mix hears the demodulator")
        page.evaluate(release)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#jobs .job[data-id="{demod}"].speaking')).to_be_visible(
            timeout=5_000)
    finally:
        page.evaluate(release)
        if demod:
            stop_job(url, secret, demod)
        stop_mixes(url, secret)


def scenario_stopping_the_mix_ends_playback_and_lights(page, url, secret):
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    demod = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    try:
        job_row(page, demod)
        before = stable_ws_connections(url, secret)[-1]
        button = page.locator("#listen-all")
        button.click()
        page.wait_for_function(
            "() => window.zetaAudio && window.zetaAudio.framesPlayed > 0", timeout=5_000)
        mix = mix_jobs(url, secret)[0]["id"]
        expect(page.locator(f'#jobs .job[data-id="{demod}"].speaking')).to_be_visible(
            timeout=5_000)
        job_row(page, mix).locator("button.stop").click()
        wait_until(lambda: page.evaluate("window.zetaAudio.playing") is None, 4.0,
                   "playback ended")
        expect(button).to_have_attribute("aria-pressed", "false")
        expect(page.locator(".speaking")).to_have_count(0, timeout=4_000)
        after = stable_ws_connections(url, secret)[-1]
        assert after == before, f"sockets back to {before}: {after}"
        # A typed `mix cb` plays a mix of that source; other text starting `mix` is the agent's.
        run_cmd(page, "mix cb")
        page.wait_for_function(
            "() => window.zetaAudio.playing !== null && window.zetaAudio.framesPlayed > 0",
            timeout=5_000)
        typed = mix_jobs(url, secret)
        assert len(typed) == 1, f"one mix: {typed}"
        graph = http_json(url, secret, "/op", {"op": "graph_get", "job": typed[0]["id"]})["value"]
        assert graph["nodes"][0]["params"]["sources"] == ["cb"], f"graph {graph}"
        run_cmd(page, "mix the channels please")
        page.wait_for_timeout(1_500)
        assert [j["id"] for j in mix_jobs(url, secret)] == [typed[0]["id"]], "no new mix"
    finally:
        stop_job(url, secret, demod)
        stop_mixes(url, secret)



def mix_graph_muted(url, secret, mix):
    graph = http_json(url, secret, "/op", {"op": "graph_get", "job": mix})["value"]
    return [n for n in graph["nodes"] if n["kind"] == "mix"][0]["params"]["muted"]


def frames_played(page):
    return page.evaluate("window.zetaAudio.framesPlayed")


def transcribes_on(url, secret, job):
    return [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
            if j["kind"] == "transcribe" and j["input"] == job]


def job_status(url, secret, job):
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    return [j for j in jobs if j["id"] == job][0]["state"]["status"]


def attached_stream(url, secret, job):
    """The stream an attached job's `stream_in` reads."""
    graph = http_json(url, secret, "/op", {"op": "graph_get", "job": job})["value"]
    return [n for n in graph["nodes"] if n["kind"] == "stream_in"][0]["params"]["stream"]


def scenario_mute_in_mix_from_row_and_outline(page, url, secret):
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    demod = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    other = start_job(url, secret, "fm_demod", "tone", 100_000_000, 10_000, deviation_hz=5000)
    jobs = [demod, other]
    try:
        run_cmd(page, "mix cb")
        page.wait_for_function(
            "() => window.zetaAudio.playing !== null && window.zetaAudio.framesPlayed > 0",
            timeout=5_000)
        mix = mix_jobs(url, secret)[0]["id"]
        # A demodulator on a source the mix does not cover has no Mute.
        expect(job_row(page, other).locator("button.mute")).to_have_count(0)
        # The mix's row takes the ordinary Transcribe button, off until pressed.
        mrow = job_row(page, mix)
        assert not transcribes_on(url, secret, mix), "no transcript attached by default"
        mrow.locator("button.transcribe").click()
        attached = wait_until(lambda: transcribes_on(url, secret, mix), 5.0,
                              "a transcribe attached to the mix")[0]["id"]
        jobs.append(attached)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#jobs .job.attached[data-id="{attached}"]')).to_be_visible()
        read_before = attached_stream(url, secret, attached)
        row = job_row(page, demod)
        expect(row).to_have_class(re.compile(r"\bspeaking\b"), timeout=5_000)
        mute = row.locator("button.mute")
        expect(mute).to_have_text("Mute in mix")
        mute.click()
        wait_until(lambda: mix_graph_muted(url, secret, mix) ==
                   [{"source": "cb", "center_hz": 476_900_000}], 5.0, "muted in the graph")
        assert [j["id"] for j in mix_jobs(url, secret)] == [mix], "the same mix"
        # The mute's restart gives the mix a new audio stream; the transcribe follows it under
        # its own id.
        wait_until(lambda: attached_stream(url, secret, attached) != read_before, 5.0,
                   "the transcribe relaunched on the mix's new stream")
        assert [j["id"] for j in transcribes_on(url, secret, mix)] == [attached], "the same one"
        assert job_status(url, secret, attached) == "running", "the transcribe runs"
        page.evaluate("window.zetaTest.poll()")
        expect(job_row(page, demod)).not_to_have_class(re.compile(r"\bspeaking\b"),
                                                       timeout=3_000)
        assert running_kind(url, secret, "fm_demod", "cb") == [demod], "the demodulator runs"
        expect(job_row(page, demod).locator("button.mute")).to_have_text("Unmute in mix")
        assert page.evaluate("window.zetaAudio.playing") == mix, "still playing the mix"
        page.wait_for_timeout(1_500)
        before = frames_played(page)
        page.wait_for_timeout(1_000)
        assert frames_played(page) == before, "no audio frames while the channel is muted"
        # The mute is the channel's: a demodulator restarted at the same band is still muted.
        stop_job(url, secret, demod)
        demod = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
        jobs.append(demod)
        row = job_row(page, demod)
        expect(row.locator("button.mute")).to_have_text("Unmute in mix", timeout=5_000)
        page.wait_for_timeout(1_500)
        assert page.evaluate("window.zetaMix.active.length") == 0, "still muted after restart"
        # From the outline menu.
        page.evaluate("window.zetaTest.poll()")
        page.locator(f'#waterfall .outline[data-key="{demod}"]').click(button="right")
        item = page.locator('#ctx-outline .ctx-item[data-action="mute"]')
        expect(item).to_have_text("Unmute in mix")
        item.click()
        wait_until(lambda: mix_graph_muted(url, secret, mix) == [], 5.0, "unmuted in the graph")
        expect(page.locator("#transcript .line.note", has_text="fm_demod: unmuted in mix")
               ).to_have_count(1)
        page.evaluate("window.zetaTest.poll()")
        expect(job_row(page, demod)).to_have_class(re.compile(r"\bspeaking\b"), timeout=5_000)
        before = frames_played(page)
        wait_until(lambda: frames_played(page) > before, 5.0, "audio frames again")
    finally:
        for j in jobs:
            stop_job(url, secret, j)
        stop_mixes(url, secret)


def scenario_mutes_in_quick_succession_keep_each_other(page, url, secret):
    """Each mute reads the mix's graph when it runs, one at a time: neither a second click before
    a poll nor another client's mute is undone by a list the page cached."""
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    a = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    b = start_job(url, secret, "fm_demod", "tone", 100_000_000, 10_000, deviation_hz=5000)
    try:
        mix = http_json(url, secret, "/op", {"op": "mix_start", "sources": None})["value"]
        page.evaluate("window.zetaTest.poll()")
        expect(job_row(page, a).locator("button.mute")).to_have_text("Mute in mix")
        expect(job_row(page, b).locator("button.mute")).to_have_text("Mute in mix")
        # Another client's mute, which the page's list has not seen.
        theirs = {"source": "tone", "center_hz": 90_000_000}
        graph = http_json(url, secret, "/op", {"op": "graph_get", "job": mix})["value"]
        [n for n in graph["nodes"] if n["kind"] == "mix"][0]["params"]["muted"] = [theirs]
        http_json(url, secret, "/op", {"op": "graph_put", "job": mix, "graph": graph})
        page.evaluate(
            """(ids) => ids.forEach((id) =>
                 document.querySelector(`#jobs .job[data-id="${id}"] button.mute`).click())""",
            [a, b])
        want = [theirs, {"source": "cb", "center_hz": 476_900_000},
                {"source": "tone", "center_hz": 100_000_000}]
        wait_until(lambda: mix_graph_muted(url, secret, mix) == want, 5.0, "both in the graph")
        page.wait_for_timeout(500)
        assert mix_graph_muted(url, secret, mix) == want, "neither undone"
        page.evaluate("window.zetaTest.poll()")
        expect(job_row(page, a).locator("button.mute")).to_have_text("Unmute in mix")
        expect(job_row(page, b).locator("button.mute")).to_have_text("Unmute in mix")
    finally:
        stop_job(url, secret, a)
        stop_job(url, secret, b)
        stop_mixes(url, secret)


def scenario_audio_comes_back_from_a_suspended_context(page, url, secret):
    """As a phone's lock screen does: the context stops while frames keep arriving. Coming back
    it runs again, and the audio queued meanwhile is cut rather than heard late."""
    stop_mixes(url, secret)
    select_source(page, url, secret, "cb")
    demod = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    try:
        page.evaluate(
            """() => {
                 const Real = window.AudioContext;
                 window.AudioContext = class extends Real {
                   constructor(...a) { super(...a); window.__ctx = this; }
                 };
               }""")
        run_cmd(page, "mix cb")
        page.wait_for_function(
            "() => window.zetaAudio.playing !== null && window.zetaAudio.framesPlayed > 0",
            timeout=5_000)
        # The node's own statechange answer: a suspend is resumed.
        page.evaluate("window.__ctx.suspend()")
        page.wait_for_function("() => window.__ctx.state === 'running'", timeout=3_000)
        # Locked: a resume does nothing until the page is visible again.
        page.evaluate("window.__ctx.resume = async () => {}; window.__ctx.suspend()")
        page.wait_for_function("() => window.__ctx.state === 'suspended'", timeout=3_000)
        page.wait_for_function("() => window.zetaAudio.framesDropped > 0", timeout=10_000)
        page.evaluate(
            """() => {
                 delete window.__ctx.resume;
                 document.dispatchEvent(new Event("visibilitychange"));
               }""")
        page.wait_for_function("() => window.__ctx.state === 'running'", timeout=3_000)
        before = frames_played(page)
        wait_until(lambda: frames_played(page) > before, 5.0, "frames again")
        assert page.evaluate("window.zetaAudio.playing") is not None, "still playing"
    finally:
        stop_job(url, secret, demod)
        stop_mixes(url, secret)


# ---- the radio policy (plan 33 Task 4) ------------------------------------------------------
# Against ZETA_UI_URL_LLM (`tone` and `cb`): "radio fallback probe" job_starts a measure on
# `tone` while `cb` is shown. `radio_policy` and `radio_prefer` persist in `localStorage`
# (RADIO_PREFS_KEY) across the tabs of one browser; the shown radio stays per tab.

RADIO_PREFS_KEY = "zeta.radio_prefs"

# Every Storage throws (the page's init script): the page boots with the defaults.
THROWING_STORAGE_JS = """
Storage.prototype.getItem = function () { throw new Error('storage unavailable'); };
Storage.prototype.setItem = function () { throw new Error('storage unavailable'); };
"""


# Only the radio prefs' writes throw (reads work): a held value must not be re-read away.
THROWING_SET_JS = """
const realSet = Storage.prototype.setItem;
Storage.prototype.setItem = function (k, v) {
  if (k === 'zeta.radio_prefs') throw new Error('storage full');
  return realSet.call(this, k, v);
};
"""


def scenario_radio_default_mark_and_policy(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    tone_measures = lambda: running_kind(url, secret, "measure", "tone")
    others = []
    try:
        asks = capture_asks(page)
        # The default radio's mark (spec 4.9): the shown radio's tab under `focus`.
        dflt = page.locator(".src-tab.default")
        expect(dflt).to_have_count(1)
        expect(dflt).to_have_attribute("data-id", "cb")
        expect(dflt).to_have_attribute(
            "title", re.compile(r"default radio for the agent \(policy focus\)"))
        expect(page.locator(".src-tab.default .default-mark")).to_be_visible()
        expect(page.locator('.src-tab[data-id="tone"] .default-mark')).to_be_hidden()
        # Another tab selected: the mark moves with it.
        page.click('.src-tab[data-id="tone"]')
        expect(page.locator('.src-tab[data-id="tone"]')).to_have_class(re.compile(r"\bdefault\b"))
        expect(dflt).to_have_count(1)
        page.click('.src-tab[data-id="cb"]')
        expect(dflt).to_have_attribute("data-id", "cb")
        # `free-first` and `ask` have no default radio; `focus-fallback` has the shown one.
        for pol, n in (("free-first", 0), ("ask", 0), ("focus-fallback", 1)):
            run_cmd(page, f"view set radio_policy {pol}")
            expect(view_line(page, f"radio_policy = {pol}")).to_be_visible()
            expect(dflt).to_have_count(n)
        expect(dflt).to_have_attribute(
            "title", re.compile(r"default radio for the agent \(policy focus-fallback\)"))
        run_cmd(page, "view set radio_policy focus")
        expect(view_line(page, "radio_policy = focus")).to_be_visible()
        expect(dflt).to_have_attribute(
            "title", re.compile(r"default radio for the agent \(policy focus\)"))
        # The Map tab keeps `cb` as the focus; the default `focus` refuses another radio,
        # as the call's result in its turn, with no unlock button and nothing started.
        page.locator("#tabs .src-tab.map-tab").click()
        page.wait_for_function("() => document.documentElement.dataset.main === 'map'",
                               timeout=5_000)
        kept = page.locator('.src-tab[data-id="cb"]')
        expect(kept).to_have_class(re.compile(r"\bkept\b"))
        expect(kept).to_have_class(re.compile(r"\bdefault\b"))
        run_cmd(page, "radio fallback probe")
        answer = page.locator("#transcript .line.assistant", has_text="Started it on tone.")
        expect(answer).to_have_count(1, timeout=10_000)
        assert len(asks) == 1, f"one /ask request: {asks}"
        assert asks[0]["context"] == {"focus": "cb", "main": "map", "radio_policy": "focus",
                                      "radio_prefer": []}, asks[0]
        turn = page.locator("#transcript details.turn", has_text="→ job_start").last
        expect(turn).to_contain_text("radio policy focus")
        # A refused run is not a run: no "ran on" note.
        expect(page.locator("#transcript .line.note", has_text="ran on tone")).to_have_count(0)
        expect(page.locator("#transcript button", has_text="allow")).to_have_count(0)
        for _ in range(5):
            assert not tone_measures(), "the focus policy started nothing on tone"
            time.sleep(0.2)
        # `focus-fallback`: the run on `tone` is noted after the answer, and the measure runs.
        run_cmd(page, "view set radio_policy focus-fallback")
        expect(view_line(page, "radio_policy = focus-fallback")).to_be_visible()
        run_cmd(page, "radio fallback probe")
        expect(answer).to_have_count(2, timeout=10_000)
        note = page.locator(
            "#transcript .line.note",
            has_text="ran on tone (job_start), not the default radio cb (focus-fallback)")
        expect(note).to_have_count(1)
        assert asks[1]["context"]["radio_policy"] == "focus-fallback", asks[1]
        started = wait_until(tone_measures, 5.0, "a measure on tone")
        assert len(started) == 1, f"one measure on tone: {started}"
        # The page's note helper: a dropped focus, and one line per off-default source.
        notes = page.evaluate("""() => window.zetaTest.selectionNotes({
            radio: null, policy: 'focus', note: 'focus gone is not a source',
            off_default: []})""")
        assert notes == ["focus gone is not a source; no default radio"], notes
        notes = page.evaluate("""() => window.zetaTest.selectionNotes({
            radio: null, policy: 'free-first', note: 'focus cb is faulted: usb gone',
            off_default: []})""")
        assert notes == [], notes
        notes = page.evaluate("""() => window.zetaTest.selectionNotes({
            radio: 'cb', policy: 'focus-fallback', note: null, off_default: [
              {tool: 'job_start', source: 'tone'}, {tool: 'source_pin', source: 'tone'},
              {tool: 'job_start', source: 'tone'}, {tool: 'job_start', source: 'hackrf'}]})""")
        assert notes == [
            "ran on tone (job_start, source_pin), not the default radio cb (focus-fallback)",
            "ran on hackrf (job_start), not the default radio cb (focus-fallback)"], notes
        # A focus that is not a source, posted without the page: the run's note says so.
        r = http_json(url, secret, "/ask", {"request": "think", "context": {"focus": "gone"}})
        assert r["selection"]["note"] == "focus gone is not a source", r["selection"]
        # The box menu's question names its radio.
        page.click('.src-tab[data-id="cb"]')
        page.wait_for_function("() => document.documentElement.dataset.main === 'rf'",
                               timeout=5_000)
        wait_span(page, MENU_SPAN_S)
        drag_menu(page)
        page.click('#ctx .ctx-item[data-action="ask"]')
        expect(page.locator("#transcript .line.assistant", has_text="A tone.")
               ).to_have_count(1, timeout=10_000)
        expect(page.locator("#transcript .line.user", has_text="what is it?")
               ).to_contain_text("on cb,")
        # `radio_prefer`: no repeats; a list is posted as ids.
        run_cmd(page, "view set radio_prefer cb,cb")
        expect(view_line(page, '"cb,cb" is not allowed', error=True)).to_contain_text(
            "none, a comma-separated list of source ids")
        run_cmd(page, "view set radio_prefer tone,cb")
        expect(view_line(page, "radio_prefer = tone,cb")).to_be_visible()
        n = len(asks)
        run_cmd(page, "think")
        wait_until(lambda: len(asks) > n, 5.0, "an /ask request")
        assert asks[n]["context"]["radio_prefer"] == ["tone", "cb"], asks[n]
        # Persistence: a second tab of the same browser reads both and posts them; its shown
        # radio is its own; a reload keeps them; `view reset` there clears both.
        second = page.context.new_page()
        others.append(second)
        second.set_viewport_size(WIDE)
        login(second, url, secret)
        second.click('.src-tab[data-id="tone"]')
        expect(second.locator('.src-tab[data-id="tone"]')).to_have_attribute(
            "aria-selected", "true")
        assert view_value(second, "radio_policy") == "radio_policy focus-fallback"
        assert view_value(second, "radio_prefer") == "radio_prefer tone,cb"
        # The first page hears the storage event: its default mark goes with the policy.
        expect(dflt).to_have_count(1)
        run_cmd(second, "view set radio_policy free-first")
        expect(view_line(second, "radio_policy = free-first")).to_be_visible()
        expect(dflt).to_have_count(0, timeout=5_000)
        run_cmd(second, "view set radio_policy focus-fallback")
        expect(view_line(second, "radio_policy = focus-fallback")).to_be_visible()
        expect(dflt).to_have_count(1, timeout=5_000)
        asks2 = capture_asks(second)
        run_cmd(second, "think")
        expect(second.locator("#transcript .line.assistant", has_text="one source")
               ).to_have_count(1, timeout=10_000)
        assert asks2[0]["context"] == {"focus": "tone", "main": "rf",
                                       "radio_policy": "focus-fallback",
                                       "radio_prefer": ["tone", "cb"]}, asks2[0]
        assert page.evaluate("window.zetaTest.shown()") == "cb", "the first tab still shows cb"
        second.reload()
        expect(second.locator('.src-tab[data-id="tone"]')).to_be_visible()
        assert view_value(second, "radio_policy") == "radio_policy focus-fallback"
        assert view_value(second, "radio_prefer") == "radio_prefer tone,cb"
        run_cmd(second, "view reset")
        expect(view_line(second, "view policies reset")).to_be_visible()
        assert view_value(second, "radio_policy") == "radio_policy focus"
        assert view_value(second, "radio_prefer") == "radio_prefer none"
        stored = second.evaluate(f"localStorage.getItem('{RADIO_PREFS_KEY}')")
        assert stored is None, f"view reset left {stored!r}"
        # Storage that throws: the defaults, and a set holds for the page load.
        run_cmd(page, "view set radio_policy free-first")
        expect(view_line(page, "radio_policy = free-first")).to_be_visible()
        third = page.context.new_page()
        others.append(third)
        third.add_init_script(THROWING_STORAGE_JS)
        third.set_viewport_size(WIDE)
        login(third, url, secret)
        expect(third.locator('.src-tab[data-id="cb"]')).to_be_visible()
        assert view_value(third, "radio_policy") == "radio_policy focus"
        run_cmd(third, "view set radio_policy ask")
        expect(view_line(third, "radio_policy = ask")).to_be_visible()
        assert view_value(third, "radio_policy") == "radio_policy ask"
        # Only a write that throws (reads work): the value still holds for the page load.
        fourth = page.context.new_page()
        others.append(fourth)
        fourth.add_init_script(THROWING_SET_JS)
        fourth.set_viewport_size(WIDE)
        login(fourth, url, secret)
        expect(fourth.locator('.src-tab[data-id="cb"]')).to_be_visible()
        run_cmd(fourth, "view set radio_policy ask")
        expect(view_line(fourth, "radio_policy = ask")).to_be_visible()
        assert view_value(fourth, "radio_policy") == "radio_policy ask"
    finally:
        for p in others:
            p.close()
        run_cmd(page, "view reset")
        page.evaluate(f"localStorage.removeItem('{RADIO_PREFS_KEY}')")
        for j in tone_measures():
            stop_job(url, secret, j)
    assert not tone_measures(), "the measure on tone stopped"


# ---- the map (plan 17 Task 9) -----------------------------------------------------------
# The node's `fake_pos` mode row (ui.rs) runs zeta-ext's `fake_lines`, which prints two fixes
# of sonde S1 (-27.40,153.10 then -27.41,153.11) 2 s into the job. The map's tiles come from
# the tile providers' hosts (map.js TILE_PROVIDERS), which every scenario aborts (main routes
# them for each context, and the map scenarios again): real tiles are never fetched, and the
# map must still draw.

TILE_HOSTS = re.compile(
    r"^https://([a-z0-9-]+\.)?(tile\.openstreetmap\.org"
    r"|tile\.opentopomap\.org|server\.arcgisonline\.com)/")


def page_errors(page):
    """Collects the page's console errors and uncaught exceptions from now on, except the
    load failures of the aborted map tiles (Chromium reports each as a console error)."""
    seen = []

    def on_console(m):
        if m.type != "error":
            return
        where = (m.location or {}).get("url", "")
        if TILE_HOSTS.search(where):
            return
        seen.append(f"{m.text} ({where})")

    page.on("console", on_console)
    page.on("pageerror", lambda e: seen.append(f"pageerror: {e}"))
    return seen


def start_fake_pos(page, url, secret):
    """Start `fake_pos` on the tone source at the row's own 40 kHz width (ui.rs shapes the row
    so no identify, watch or classify sweep tries it) and have the page watch it at once (its
    two fixes come 2 s in); the job id."""
    job = start_job(url, secret, "fake_pos", "tone", 100_000_000, 40_000)
    job_row(page, job)
    return job


def scenario_map_marker_draws_without_tiles(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    expect(page.locator("#map")).to_be_hidden()
    run_cmd(page, "view set main map")
    expect(view_line(page, "main = map")).to_be_visible()
    mp = page.locator("#map")
    expect(mp).to_be_visible()
    expect(mp.locator(".leaflet-control-attribution")).to_contain_text(
        "© OpenStreetMap contributors")
    job = start_fake_pos(page, url, secret)
    try:
        expect(mp.locator(".leaflet-interactive")).to_have_count(1, timeout=10_000)
        track = mp.locator("path.map-track")
        expect(track).to_have_count(1)
        # Both fixes drawn: the track's SVG path is a move and one line segment.
        page.wait_for_function(
            """() => {
              const d = document.querySelector('#map path.map-track')?.getAttribute('d') || '';
              return (d.match(/[ML]/g) || []).length === 2;
            }""", timeout=5_000)
        # The popup is the last summary and its age.
        mp.locator(".leaflet-interactive").click()
        popup = mp.locator(".leaflet-popup-content")
        expect(popup).to_contain_text("S1 -27.41,153.11")
        expect(popup).to_contain_text(re.compile(r"\d+ s ago"))
        # Off hides the pane; the job's marker goes with the job.
        stop_job(url, secret, job)
        page.evaluate("window.zetaTest.poll()")
        expect(mp.locator(".leaflet-interactive")).to_have_count(0)
        expect(track).to_have_count(0)
        run_cmd(page, "view set main rf")
        expect(mp).to_be_hidden()
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


# map.js TILE_PROVIDERS, as the brief tables them: id -> (label, url, attribution).
TILE_TABLE = {
    "osm": ("OpenStreetMap", "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            "© OpenStreetMap contributors"),
    "osm-dark": ("OpenStreetMap dark", "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
                 "© OpenStreetMap contributors"),
    "opentopo": ("OpenTopoMap", "https://{s}.tile.opentopomap.org/{z}/{x}/{y}.png",
                 "© OpenStreetMap contributors, SRTM | © OpenTopoMap (CC-BY-SA)"),
}


def scenario_map_tiles(page, url, secret):
    """Plan 32 Task 11b: the tile layer follows the view's `map_tiles` (auto: OpenStreetMap
    darkened by a filter on its tile layer alone under the dark console theme), the layers
    control sets it, and tiles that fail say so."""
    page.route(TILE_HOSTS, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    mp = page.locator("#map")
    layer = lambda: page.evaluate("() => window.zetaTest.tileLayer()")  # noqa: E731

    def expect_layer(tid):
        page.wait_for_function(
            f"() => window.zetaTest.tileLayer()?.id === {json.dumps(tid)}", timeout=5_000)
        got = layer()
        label, turl, attribution = TILE_TABLE[tid]
        assert got == {"id": tid, "url": turl, "attribution": attribution}, got
        expect(page.locator("html")).to_have_attribute("data-map-tiles", tid, timeout=2_000)
        expect(mp.locator(".leaflet-tile-pane > .leaflet-layer")).to_have_count(1, timeout=2_000)
        # The aborted tiles: a note naming the provider and another one to try.
        note = mp.locator(".map-tile-note")
        expect(note).to_be_visible(timeout=5_000)
        expect(note).to_contain_text(f"map tiles from {label} are not loading; try another "
                                     "(view set map_tiles ")
        return note

    def filters():
        # The tile layer's CSS filter, and those of the panes that must stay unfiltered.
        return page.evaluate("""() => {
          const f = (sel) => {
            const el = document.querySelector('#map ' + sel);
            return el ? getComputedStyle(el).filter : null;
          };
          return {layer: f('.leaflet-tile-pane > .leaflet-layer'), tilePane: f('.leaflet-tile-pane'),
                  overlay: f('.leaflet-overlay-pane'), marker: f('.leaflet-marker-pane'),
                  tooltip: f('.leaflet-tooltip-pane'), map: f('')};
        }""")

    try:
        run_cmd(page, "view set main map")
        expect(mp).to_be_visible(timeout=5_000)
        note = expect_layer("osm-dark")
        # The same tiles as osm, so the note offers a provider on another server.
        assert "view set map_tiles osm)" not in note.text_content(), note.text_content()
        expect(mp.locator(".leaflet-control-attribution")).to_contain_text(
            "© OpenStreetMap contributors")
        got = filters()
        assert "invert" in (got["layer"] or ""), got
        for pane in ("tilePane", "overlay", "marker", "tooltip", "map"):
            assert got[pane] == "none", got
        run_cmd(page, "view set map_tiles osm")
        expect(view_line(page, "map_tiles = osm")).to_be_visible(timeout=2_000)
        note = expect_layer("osm")
        assert "view set map_tiles osm)" not in note.text_content(), note.text_content()
        assert filters()["layer"] == "none", filters()
        # The layers control lists the providers by label; choosing one sets the policy.
        ctl = mp.locator(".leaflet-control-layers")
        ctl.hover(timeout=2_000)
        labels = ctl.locator(".leaflet-control-layers-base label")
        expect(labels).to_have_count(5, timeout=2_000)
        labels.filter(has_text="OpenTopoMap").click(timeout=2_000)
        expect_layer("opentopo")
        shown = view_show(page)
        assert "map_tiles opentopo" in shown, shown
        run_cmd(page, "view set map_tiles sideways")
        expect(view_line(page, "sideways", error=True)).to_contain_text(
            "auto, osm, osm-dark, opentopo, esri-street, esri-imagery", timeout=2_000)
        run_cmd(page, "view reset")
        expect(view_line(page, "view policies reset")).to_be_visible(timeout=2_000)
        shown = view_show(page)
        assert "map_tiles auto" in shown, shown
        run_cmd(page, "view set main map")
        page.wait_for_function(
            "() => window.zetaTest.tileLayer()?.id === 'osm-dark'", timeout=5_000)
        # Ruling M: `auto` under a light theme (vibecoder, color_scheme light) is OpenStreetMap.
        run_cmd(page, "view set theme vibecoder")
        expect(page.locator("html")).to_have_attribute("data-map-tiles", "osm", timeout=5_000)
        page.wait_for_function("() => window.zetaTest.tileLayer()?.id === 'osm'", timeout=5_000)
        assert not errors, f"console errors: {errors}"
    finally:
        run_cmd(page, "view reset")


def scenario_map_follows_a_positioned_job_on_another_source(page, url, secret):
    # Spec 7.2: every running positioned job gets a marker, not only the shown source's (the
    # two-radio form: the page shows the watching radio, the decoder runs on the other).
    page.route(TILE_HOSTS, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "aprs")
    run_cmd(page, "view set main map")
    mp = page.locator("#map")
    expect(mp).to_be_visible()
    job = start_fake_pos(page, url, secret)
    try:
        expect(mp.locator(".leaflet-interactive")).to_have_count(1, timeout=10_000)
        page.wait_for_function(
            """() => {
              const d = document.querySelector('#map path.map-track')?.getAttribute('d') || '';
              return (d.match(/[ML]/g) || []).length === 2;
            }""", timeout=5_000)
        # Map only: the other source's job draws no ribbon on the shown waterfall.
        expect(page.locator(".ov-ribbon.fake_pos")).to_have_count(0)
        stop_job(url, secret, job)
        page.evaluate("window.zetaTest.poll()")
        expect(mp.locator(".leaflet-interactive")).to_have_count(0)
        run_cmd(page, "view set main rf")
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_map_is_a_source_tab(page, url, secret):
    # Spec 4.4: the Map is a tab of the sources strip; `main map` swaps it for the waterfall in
    # #main and keeps the shown source (its job and axis), `map on|off` is its alias.
    page.route(TILE_HOSTS, lambda r: r.abort())
    select_source(page, url, secret, "tone")
    page.set_viewport_size(WIDE)
    main_is = "() => document.documentElement.dataset.main === '%s'"
    try:
        tab = page.locator("#tabs .src-tab.map-tab")
        expect(tab).to_have_count(1)
        expect(page.locator("#map-toggle")).to_have_count(0)
        # The waterfall settled first: its spectrum job painting, the axis drawn.
        page.wait_for_function(
            """() => {
              const wf = window.zetaTest.wf();
              const t = document.querySelectorAll('#waterfall .wf-axis .tick');
              return !!wf && !!wf.jobId && wf.painted > 0 && t.length === 5;
            }""", timeout=10_000)
        before = ticks(page)
        job0 = page.evaluate("() => window.zetaTest.wf().jobId")
        tab.click()
        page.wait_for_function(main_is % "map", timeout=5_000)
        expect(page.locator("#map")).to_be_visible()
        expect(page.locator("#waterfall")).to_be_hidden()
        shown_id = page.evaluate("() => window.zetaTest.shown()")
        assert shown_id == "tone", shown_id
        expect(tab).to_have_attribute("aria-selected", "true")
        tone = page.locator('#tabs .src-tab[data-id="tone"]')
        expect(tone).to_have_class(re.compile(r"\bkept\b"))
        expect(tone).to_have_attribute("aria-selected", "false")
        shown = view_show(page)
        assert "main map" in shown and "map auto" in shown, shown
        # A jobs poll and the source_list after it (another source's jobs gone, syncTabs run)
        # keep the tab and the map shown.
        stop_spectrum_jobs(url, secret, "retune")
        with page.expect_response(
                lambda r: is_op("source_list")(r.request), timeout=10_000) as resp:
            page.evaluate("() => window.zetaTest.poll()")
        resp.value.finished()
        page.evaluate("() => new Promise((r) => requestAnimationFrame(() => r()))")
        expect(tab).to_have_count(1)
        expect(tab).to_have_attribute("aria-selected", "true")
        expect(tone).to_have_class(re.compile(r"\bkept\b"))
        main = page.evaluate("() => document.documentElement.dataset.main")
        assert main == "map", f"after a source_list: main {main}"
        assert "main map" in view_show(page)
        tone.click()
        page.wait_for_function(main_is % "rf", timeout=5_000)
        expect(tone).not_to_have_class(re.compile(r"\bkept\b"))
        after = ticks(page)
        assert after == before, f"ticks {before} then {after}"
        job1 = page.evaluate("() => window.zetaTest.wf().jobId")
        assert job1 == job0, f"spectrum job {job0} then {job1}"
        run_cmd(page, "view set map on")
        page.wait_for_function(main_is % "map", timeout=5_000)
        run_cmd(page, "view set map off")
        page.wait_for_function(main_is % "rf", timeout=5_000)
        # The box menu goes with the waterfall (as a source switch drops it). Switched from
        # the command line: a click on the Map tab would close it as any press outside does.
        page.wait_for_function("() => window.zetaTest.wf().painted > 40", timeout=10_000)
        menu = drag_menu(page)
        run_cmd(page, "view set main map")
        page.wait_for_function(main_is % "map", timeout=5_000)
        expect(menu).to_be_hidden()
        run_cmd(page, "view set main rf")
        page.wait_for_function(main_is % "rf", timeout=5_000)
        # A stored `map on` from before `main` still shows the map after a reload.
        page.evaluate("() => sessionStorage.setItem('zeta.view', JSON.stringify({map: 'on'}))")
        page.reload()
        page.wait_for_function(main_is % "map", timeout=10_000)
        expect(page.locator("#map")).to_be_visible()
        run_cmd(page, "view reset")
        page.wait_for_function(main_is % "rf", timeout=5_000)
        page.set_viewport_size(NARROW)
        tab.click()
        page.wait_for_function(main_is % "map", timeout=5_000)
        expect(page.locator("#map")).to_be_visible()
        # A logout with the map shown leaves no map box over the login.
        page.evaluate("() => { window.zetaTest.setSecret('wrong'); window.zetaTest.poll(); }")
        expect(page.locator("#login")).to_be_visible()
        page.wait_for_function(
            "() => getComputedStyle(document.getElementById('map')).display === 'none'",
            timeout=5_000)
        page.fill("#secret", secret)
        page.click("#connect")
        expect(page.locator("#login")).to_be_hidden()
    finally:
        run_cmd(page, "view reset")
        page.set_viewport_size(WIDE)


def scenario_home_gives_distance_and_bearing(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    errors = page_errors(page)
    # No station, so `home none` leaves the row without a distance (the page reads it at load).
    http_json(url, secret, "/op", {"op": "station_clear"})
    try:
        home_gives_distance_and_bearing(page, url, secret, errors)
    finally:
        set_demo_station(url, secret)


def home_gives_distance_and_bearing(page, url, secret, errors):
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set home -27.50,153.00")
    expect(view_line(page, "home = -27.5,153")).to_be_visible()
    run_cmd(page, "view set home 91,0")
    expect(view_line(page, '"91,0"', error=True)).to_be_visible()
    db = page.evaluate(
        """() => import('./map.js').then((m) =>
             m.distanceBearing({lat: -27.5, lon: 153.0}, {lat: -27.4, lon: 153.1}))""")
    assert abs(db["km"] - 15.0) <= 0.3, f"distanceBearing {db}"
    assert abs(db["deg"] - 42) <= 2, f"distanceBearing {db}"
    job = start_fake_pos(page, url, secret)
    try:
        pos = page.locator(f'#jobs .job[data-id="{job}"] .pos')
        expect(pos).to_have_text(re.compile(r"^\d+(\.\d+)? km, \d+°$"), timeout=10_000)
        # The last fix, -27.41,153.11 from -27.50,153.00: 14.8 km at 47°.
        expect(pos).to_have_text("14.8 km, 47°")
        run_cmd(page, "view set home none")
        expect(pos).to_have_count(0)
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_station_is_the_default_home(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    errors = page_errors(page)
    http_json(url, secret, "/op", {"op": "station_set", "lat": -27.5, "lon": 153.0,
                                   "alt_m": 10.0, "region": "AU", "antennas": None})
    job = None
    try:
        # Read once at page load: no `view set home` needed.
        select_source(page, url, secret, "tone")
        page.wait_for_function(
            "() => { const h = window.zetaTest.home(); return !!h && h.lat === -27.5"
            " && h.lon === 153; }", timeout=10_000)
        assert view_value(page, "home") == "home none", view_value(page, "home")
        job = start_fake_pos(page, url, secret)
        pos = page.locator(f'#jobs .job[data-id="{job}"] .pos')
        expect(pos).to_have_text("14.8 km, 47°", timeout=10_000)
        # The view's home takes precedence.
        run_cmd(page, "view set home -27.41,153.11")
        assert page.evaluate("window.zetaTest.home()") == {"lat": -27.41, "lon": 153.11}
        run_cmd(page, "view set home none")
        assert not errors, f"console errors: {errors}"
    finally:
        if job:
            stop_job(url, secret, job)
        set_demo_station(url, secret)


# ---- lit sites (plan 32 Task 9) ---------------------------------------------------------
# The node's register stub (ui.rs fake_register_rows) answers two transmitters on the tone's
# band, 16K0F1D at Example Hill 9 km north-west of the demo station and 12K5A3E at Example
# Ridge 20 km east, and a receiver; the node's station is the demo station.

def lit_sites(page):
    """The lit sites on the map as (agree, confirmed) pairs."""
    return page.locator("#map path.map-site").evaluate_all(
        "els => els.map(e => [e.dataset.agree, e.dataset.confirmed !== undefined])")


def hover_site_of(page, licensee):
    """Hover each lit site until its tooltip names `licensee`; that site's locator."""
    site = page.locator("#map path.map-site")
    tip = page.locator("#map .leaflet-tooltip")
    for i in range(site.count()):
        site.nth(i).hover()
        if licensee in (tip.text_content() or ""):
            return site.nth(i)
    raise AssertionError(f"no lit site of {licensee}")


def origins_of(url, secret, job, source=None, center_hz=None, bandwidth_hz=None):
    """`origin_lookup` by `job` (or, with `job` None, the band of `source`), measured as FM
    (the fake_pos row states no modulation): the 16K0F1D row fits, the 12K5A3E row
    disagrees. The answer."""
    return http_json(url, secret, "/op", {"op": "origin_lookup", "job": job, "source": source,
                                          "center_hz": center_hz, "bandwidth_hz": bandwidth_hz,
                                          "modulation": ["fm"]})["value"]


def scenario_origins_light_sites(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    select_source(page, url, secret, "tone")
    page.set_viewport_size(WIDE)
    # At the tone's centre, the row's own 40 kHz (its `min_hz`; a narrower band is refused).
    job = start_fake_pos(page, url, secret)
    other = None
    badge = page.locator("#tabs .map-tab .count")
    try:
        # Lit while the map was never shown (no Leaflet map yet): the badge counts them and
        # the map draws them on its first show.
        job_row(page, job).locator("button.origins").click()   # the job form; the page follows the feed
        expect(badge).to_have_text("2", timeout=10_000)
        assert page.evaluate("() => window.zetaTest.litKeys()") == [job]
        # A node annotation, not a decode (ruling N): a transcript line, no ribbon.
        expect(page.locator("#transcript .line.note", has_text="2 sites on")).to_have_count(1)
        expect(page.locator("#waterfall .ov-ribbon", has_text="sites on")).to_have_count(0)
        page.locator("#tabs .src-tab.map-tab").click()
        expect(page.locator("#map path.map-site")).to_have_count(2, timeout=10_000)
        # The job form reads the job's own modulation (the fake_pos row states none): unknown.
        agree = sorted(a for a, _ in lit_sites(page))
        assert agree == ["unknown", "unknown"], agree
        expect(page.locator("#map path.map-bearing")).to_have_count(2)
        # Brightness is the score times the freshness, per site.
        bright = page.locator("#map path.map-site").evaluate_all(
            "els => els.map((e) => Number(e.style.getPropertyValue('--site-brightness')))")
        assert len(bright) == 2 and min(bright) > 0, bright
        tip = page.locator("#map .leaflet-tooltip")
        site = page.locator("#map path.map-site")
        hill = hover_site_of(page, "Example Water Authority")
        expect(tip).to_contain_text("Example Water Authority")
        expect(tip).to_contain_text("16K0F1D")
        expect(tip).to_contain_text(re.compile(r"0\.\d+ unknown"))
        hill.click()
        pop =page.locator("#map .leaflet-popup-content")
        expect(pop).to_contain_text("licence 0000001")
        expect(pop.locator("button")).to_have_count(0)
        run_cmd(page, "view set origin_fade_s 2")
        # fake_pos sent its last event 2 s in.
        expect(page.locator("#map path.map-site")).to_have_count(0, timeout=8_000)
        expect(badge).to_have_text("")
        # A long fade again, so what follows is the job's stop and not the fade.
        run_cmd(page, "view set origin_fade_s 300")
        job_row(page, job).locator("button.origins").click()
        expect(page.locator("#map path.map-site")).to_have_count(2, timeout=10_000)
        page.locator('#tabs .src-tab[data-id="tone"]').click()
        stop_job(url, secret, job)
        page.evaluate("() => window.zetaTest.poll()")
        # Review focus 3: the set goes with the job, at once.
        page.wait_for_function("() => window.zetaTest.litKeys().length === 0", timeout=3_000)
        expect(badge).to_have_text("", timeout=1_000)
        page.locator("#tabs .src-tab.map-tab").click()
        expect(page.locator("#map path.map-site")).to_have_count(0)
        # A job the overlays do not follow (an fm_demod on a source the page does not show,
        # not positioned): Origins lights its sites from the op's answer and notes them.
        other = start_job(url, secret, "fm_demod", "tone", 100_000_000, 12_500)
        page.locator('#tabs .src-tab[data-id="quiet"]').click()
        job_row(page, other).locator("button.origins").click()
        page.wait_for_function("(j) => window.zetaTest.litKeys().includes(j)", arg=other,
                               timeout=10_000)
        expect(page.locator("#transcript .line.note", has_text=re.compile(
            rf"fm_demod {other[-6:]}: \d+ sites? on 100 MHz"))).to_have_count(1, timeout=5_000)
    finally:
        run_cmd(page, "view reset")
        stop_job(url, secret, job)
        if other:
            stop_job(url, secret, other)


def scenario_origins_from_the_menu_and_bits(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    clear_captures(url, secret)
    stop_bits_jobs(url, secret)
    try:
        # A labelled box: What is this labels the FM tone (layer 1: fm); its lit set is
        # dropped so only the menu's own answer is on the map.
        menu = open_menu_on_tone(page, url, secret)
        page.click('#ctx .ctx-item[data-action="classify"]')
        expect(page.locator("#transcript .line.op", has_text=re.compile(r"^classify .*→ ok"))
               .first).to_be_visible(timeout=15_000)
        for k in page.evaluate("() => window.zetaTest.litKeys()"):
            page.evaluate("(k) => window.zetaTest.lightOrigin(k, {origins: []})", k)
        assert page.evaluate("() => window.zetaTest.litKeys()") == []
        # The box menu's entry: the band form, the label's modulation named.
        drag_menu(page)
        page.click('#ctx .ctx-item[data-action="origins"]')
        expect(menu).to_be_hidden()
        expect(page.locator("#tabs .map-tab .count")).to_have_text("2", timeout=10_000)
        keys = page.evaluate("() => window.zetaTest.litKeys()")
        assert len(keys) == 1 and keys[0].startswith("band:"), keys
        # Spec 4.8: the entry says what it found in the transcript, as the node words it.
        expect(page.locator("#transcript .line.note", has_text=re.compile(
            r"^2 sites on [\d.]+ MHz: Example "))).to_have_count(1, timeout=5_000)
        page.locator("#tabs .src-tab.map-tab").click()
        expect(page.locator("#map path.map-site")).to_have_count(2, timeout=10_000)
        # The tone's label is `carrier`, which neither stub transmitter (FM, AM) fits: the
        # label reached the node (a null modulation would read `unknown`).
        agree = sorted(a for a, _ in lit_sites(page))
        assert agree == ["disagrees", "disagrees"], agree
        # The drawing of a fit, from the node's answer for FM (the fitting site is brighter,
        # the disagreeing one a dashed ring, one bearing).
        page.evaluate("() => window.zetaTest.lightOrigin(window.zetaTest.litKeys()[0], {origins: []})")
        fm = origins_of(url, secret, None, "tone", 100_000_000, 9_600)
        page.evaluate("(a) => window.zetaTest.lightOrigin('band:fm', a)", fm)
        expect(page.locator("#map path.map-site")).to_have_count(2)
        agree = sorted(a for a, _ in lit_sites(page))
        assert agree == ["disagrees", "fits"], agree
        expect(page.locator("#map path.map-bearing")).to_have_count(1)
        marks =page.locator("#map path.map-site").evaluate_all(
            """els => Object.fromEntries(els.map((e) => [e.dataset.agree,
                 [Number(e.style.getPropertyValue('--site-brightness')),
                  e.getAttribute('stroke-dasharray')]]))""")
        assert marks["fits"][0] > marks["disagrees"][0] > 0, marks
        assert marks["disagrees"][1] and not marks["fits"][1], marks
        # A confirmed origin (a heard callsign) wears the ring.
        page.evaluate("""() => window.zetaTest.lightOrigin('band:ring', {
            t_ns: Date.now() * 1e6, origins: [{score: 1, agree: 'fits',
              confirmed: {field: 'callsign', value: 'XX4EX'},
              allocation: {lat: -27.45, lon: 153.0, licensee: 'Example Water Authority',
                           site: 'Example Hill'}}]})""")
        expect(page.locator("#map path.map-site[data-confirmed]")).to_have_count(1)
        page.locator('#tabs .src-tab[data-id="tone"]').click()
        # The Bits header's button: a bits job on a tone capture asks by its job.
        cap = start_job(url, secret, "capture", "tone", 100_001_000, 6_000)
        two_windows(url, secret, cap)
        wait_until(lambda: len(capture_ops(url, secret, "capture_list", signal=None)) == 2,
                   30, "two captures")
        newest = capture_ops(url, secret, "capture_list", signal=None)[0]["id"]
        http_json(url, secret, "/op", {"op": "bits_start", "capture": newest,
                                       "modulation": "fsk", "symbol_rate_baud": 4800,
                                       "order": 2})
        page.evaluate("() => window.zetaTest.poll()")
        page.locator('#panes .pane-tab[data-pane="bits"]').click()
        page.locator('#bits button[data-action="origins"]').click()
        expect(page.locator("#transcript .line.note", has_text="bits ")).to_contain_text(
            "sites on", timeout=10_000)
        page.locator("#tabs .src-tab.map-tab").click()
        expect(page.locator("#map path.map-site")).not_to_have_count(0, timeout=10_000)
    finally:
        run_cmd(page, "view reset")
        stop_bits_jobs(url, secret)
        clear_captures(url, secret)
        # The capture job ended on its own, so the node may have proposed a signature of
        # it (plan 19.3): one left proposed keeps the Explore tab eligible after this.
        clear_signatures(url, secret)


def scenario_no_station_lights_without_bearings(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    http_json(url, secret, "/op", {"op": "station_clear"})
    job = None
    try:
        select_source(page, url, secret, "tone")
        job = start_fake_pos(page, url, secret)
        answer = origins_of(url, secret, job)
        assert answer["from"] is None and answer["from_note"] == "no station set", answer
        page.locator("#tabs .src-tab.map-tab").click()
        expect(page.locator("#map path.map-site")).to_have_count(2, timeout=10_000)
        expect(page.locator("#map path.map-bearing")).to_have_count(0)
        note = page.locator("#map .map-note")
        expect(note).to_be_visible()
        expect(note).to_contain_text("no station set")
        page.locator('#map path.map-site[data-agree="fits"]').hover()
        tip = page.locator("#map .leaflet-tooltip")
        expect(tip).to_contain_text("Example Water Authority")
        assert " km" not in tip.text_content(), tip.text_content()
    finally:
        run_cmd(page, "view reset")
        if job:
            stop_job(url, secret, job)
        set_demo_station(url, secret)


def scenario_classify_lights_sites(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    try:
        menu = open_menu_on_tone(page, url, secret)
        page.click('#ctx .ctx-item[data-action="classify"]')
        expect(menu).to_be_hidden()
        line = page.locator("#transcript .line.op", has_text=re.compile(r"^classify .*→ ok"))
        expect(line.first).to_be_visible(timeout=15_000)
        expect(page.locator("#tabs .map-tab .count")).to_have_text("2", timeout=5_000)
        keys = page.evaluate("() => window.zetaTest.litKeys()")
        assert len(keys) == 1 and keys[0].startswith("classify:"), keys
        page.locator("#tabs .src-tab.map-tab").click()
        expect(page.locator("#map path.map-site")).to_have_count(2, timeout=10_000)
        expect(page.locator("#map path.map-bearing")).to_have_count(
            sum(1 for a, _ in lit_sites(page) if a != "disagrees"))
        run_cmd(page, "view set origin_fade_s 2")
        expect(page.locator("#map path.map-site")).to_have_count(0, timeout=6_000)
        assert page.evaluate("() => window.zetaTest.litKeys()") == []
    finally:
        run_cmd(page, "view reset")


# ---- heard sites (plan 35 Task 4) -------------------------------------------------------
# The node's `fake_vdl2` row (ui.rs) names its ground stations (`subject.station`): its lines,
# 2 s into the job, are three aircraft frames addressed (`dst`) to the ground station 1A2B3C
# and 1A2B3C's own uplink, once (fake lines are written whole, never repeated here). On the
# tone's band, which the register stub answers, the job's origin answer lights both stub
# transmitters as heard, graded `from`.

def scenario_heard_sites(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    page.set_viewport_size(WIDE)
    job = start_subject_job(page, url, secret, "fake_vdl2", bandwidth_hz=42_000)
    # A row without `station` beside it, so the page's station gate is seen to hold.
    plain = start_subject_job(page, url, secret, "fake_subjects")
    sites = page.locator("#map path.map-site")
    heard = page.locator('#map path.map-site[data-heard="from"]')
    try:
        # The node's tally first, so the lookup answers `heard` already.
        def tallied():
            ctx = http_json(url, secret, "/op", {"op": "job_context", "job": job})["value"]
            s = (ctx.get("stations") or {}).get("1A2B3C")
            return s if s and s["from"] == 1 and s["to"] == 3 else None
        wait_until(tallied, 10, "1A2B3C heard once, addressed three times")
        # The page timed the station frames of the station row, and none of the other row,
        # whose events it saw too (each job's subjects all merged).
        for j in (job, plain):
            page.wait_for_function("(j) => window.zetaSubjects.size(j) === 3", arg=j,
                                   timeout=10_000)
        last = "(j) => window.zetaTest.lastStationEvent(j)"
        assert page.evaluate(last, job) is not None, "no station frame timed on fake_vdl2"
        assert page.evaluate(last, plain) is None, "a station frame timed on fake_subjects"
        job_row(page, job).locator("button.origins").click()
        expect(page.locator("#tabs .map-tab .count")).to_have_text("2", timeout=10_000)
        page.locator("#tabs .src-tab.map-tab").click()
        expect(sites).to_have_count(2, timeout=10_000)
        expect(heard).to_have_count(2)
        expect(page.locator("#map path.map-bearing")).to_have_count(2)
        # A heard site paints at freshness alone: brighter than its score.
        answer = http_json(url, secret, "/op", {"op": "job_context", "job": job})["value"]["origins"]
        scores = [o["score"] for o in answer["origins"]]
        assert all(o["heard"]["grade"] == "from" for o in answer["origins"]), answer
        assert max(scores) < 0.9, scores
        bright = sites.evaluate_all(
            "els => els.map((e) => Number(e.style.getPropertyValue('--site-brightness')))")
        assert min(bright) >= 0.9, bright
        # The hover names the grade, the popup each station's counts.
        tip = page.locator("#map .leaflet-tooltip")
        hill = hover_site_of(page, "Example Water Authority")
        hill_tip = page.locator("#map .leaflet-tooltip", has_text="Example Water Authority")
        expect(hill_tip).to_contain_text("ground station heard, 1 frame")
        # The frames' time is the tone recording's clock, no age on the page's: none given.
        expect(hill_tip).not_to_contain_text("min ago")
        hill.click()
        pop = page.locator("#map .leaflet-popup-content")
        expect(pop).to_contain_text("1A2B3C: heard 1, addressed by 3")
        page.keyboard.press("Escape")
        # The `to` grade on a disagreeing site: still filled, still a bearing, its own mark,
        # never the confirmed ring.
        page.evaluate("""() => window.zetaTest.lightOrigin('band:heard-to', {
            t_ns: Date.now() * 1e6, from: {kind: 'station', lat: -27.3842, lon: 153.1175},
            stations: {'1A2B3C': {from: 0, to: 2, first_ns: Date.now() * 1e6,
                                  last_ns: Date.now() * 1e6}},
            origins: [{score: 0.2, agree: 'disagrees', confirmed: null,
              heard: {last_ns: Date.now() * 1e6, grade: 'to'},
              allocation: {lat: -27.30, lon: 153.30, licensee: 'Example Airways',
                           site: 'Example Tower'}}]})""")
        to = page.locator('#map path.map-site[data-heard="to"]')
        expect(to).to_have_count(1)
        expect(page.locator("#map path.map-bearing")).to_have_count(3)
        assert to.get_attribute("data-confirmed") is None
        assert to.evaluate("(e) => getComputedStyle(e).fill") != "none"
        to.hover()
        expect(page.locator("#map .leaflet-tooltip", has_text="Example Airways")).to_contain_text(
            "ground station addressed by 2 frames, last 0 min ago")
        page.evaluate("() => window.zetaTest.lightOrigin('band:heard-to', {origins: []})")
        expect(to).to_have_count(0)
        # No station frame comes again: lit afresh, the site fades on the view's fade.
        run_cmd(page, "view set origin_fade_s 2")
        job_row(page, job).locator("button.origins").click()
        page.wait_for_function(
            """() => {
              const e = document.querySelector('#map path.map-site[data-heard="from"]');
              if (!e) return false;
              const b = Number(e.style.getPropertyValue('--site-brightness'));
              return b > 0 && b <= 0.5;
            }""", timeout=3_000)
        # The Subjects table marks the ground station's row and no aircraft's.
        open_subjects(page)
        expect(page.locator('#subjects tr.subject.station[data-key="1A2B3C"]')).to_have_count(
            1, timeout=10_000)
        expect(page.locator('#subjects tr.subject[data-key="7C0000"]')).to_have_count(1)
        expect(page.locator('#subjects tr.subject.station[data-key="7C0000"]')).to_have_count(0)
        expect(page.locator("#subjects tr.subject.station")).to_have_count(1)
        assert not errors, f"console errors: {errors}"
    finally:
        run_cmd(page, "view reset")
        stop_job(url, secret, job)
        stop_job(url, secret, plain)


# ---- subjects (plan 23 Task 2) ----------------------------------------------------------
# The node's `fake_subjects` row prints three aircraft in separate messages, `fake_fold` two
# sondes (ui.rs), each 2 s into the job.

def start_subject_job(page, url, secret, kind, source="tone", bandwidth_hz=40_000):
    """Start a subject fixture at a width of its own (identical bands are `duplicate`), its row
    waited for, so the page subscribes before the lines come; the job id."""
    job = start_job(url, secret, kind, source, 100_000_000, bandwidth_hz)
    job_row(page, job)
    return job


def subject_row(page, key):
    return page.locator(f'#subjects tr.subject[data-key="{key}"]')


def subject_keys(page):
    return page.locator("#subjects tr.subject").evaluate_all("els => els.map((e) => e.dataset.key)")


def subject_counts(page, job):
    return page.evaluate(
        "(job) => Object.fromEntries(window.zetaSubjects.list(job).map((s) => [s.key, s.count]))",
        job)


def scenario_subjects_store_merge_units_and_fill(page, url, secret):
    login(page, url, secret)
    r = page.evaluate(
        """async () => {
          const { mountSubjects, fillSummary, subjectLabel, subjectSummary, subjectValues } =
            await import('./subjects.js');
          const { metricText } = await import('./units.js');
          const fill = {
            aprs: fillSummary("{src} > {dst}: {n} {ok}",
                              {src: "VK3ABC", dst: "APRS", n: 7, ok: true}),
            nulls: fillSummary("{z} {absent} {o}", {z: null, o: {a: 1}}),
            rds1: fillSummary("{ps?pi} · {rt?}", {ps: "ZETA FM", pi: null, rt: null}),
            rds2: fillSummary("{ps?pi} · {rt?}", {pi: "2AB1", rt: "hello"}),
            adsb: fillSummary("{icao} {callsign?} {alt?} {lat?},{lon?}",
                              {icao: "7C6B2E", lat: -37.8}),
            keep: fillSummary("{a}{b?}", {a: "x:", b: null}),
            lat: fillSummary("{id} {lat?:.4}", {id: "S1", lat: -27.41234}),
            noLat: fillSummary("{id} {lat?:.4}", {id: "S1"}),
            plus: fillSummary("{n:+.2}", {n: 3}),
            text: fillSummary("{id:.1}", {id: "S1"}),
            notFormat: fillSummary("{a:x}", {a: 1}),
          };
          const metric = [
            [21000, "ft"], [421, "kt"], [245, "deg"], [5, "m_s"], [0, "ft"], [1200.4, "m"],
            [90, "km_h"], ["x", null], ["x", "ft"], [null, "ft"], [undefined, null],
            [-0.4, "m_s"], [-0.04, "m_s"], [359.6, "deg"], [0, "deg"],
          ].map(([v, u]) => metricText(v, u));
          const row = {
            protocol: "k",
            event: {summary: "{icao} {callsign?} {altitude_ft?}"},
            subject: {key: "icao", name: "aircraft", waterfall: "summary", heading: null,
                      label: "callsign",
                      columns: [{field: "callsign", label: "Callsign", unit: null},
                                {field: "altitude_ft", label: "Altitude", unit: "ft"}]},
          };
          const api = {op: async (o) => o.op === "modes_list"
            ? {value: {modes: [{id: "k"}, {id: "plain"}]}}
            : {value: {mode: {row: o.id === "k" ? row : {protocol: "p", subject: null}}}}};
          const view = {get: (k) => (k === "subject_dim_s" ? 60 : 300), onChange() {}};
          const store = mountSubjects(api, view);
          const before = store.rowOf("k") === undefined && store.rowOf("nosuch") === undefined;
          const learned = await store.lookup(["k", "plain", "nosuch"]);
          const job = {id: "J", kind: "k"};
          store.event(job, {icao: "A", callsign: "QFA1"});
          const s = store.event(job, {icao: "A", callsign: null, altitude_ft: 0});
          store.event(job, {icao: "A", callsign: "   "});
          store.event(job, {icao: "A", position: {lat: 1, lon: 2, alt_m: 100}});
          const fix = store.event(job, {icao: "A", position: {lat: 3, lon: 4, alt_m: null}});
          const proto = store.event(job, {icao: "__proto__", callsign: "P"});
          const out = {
            fill, metric, learned, before,
            noRow: store.rowOf("nosuch"), again: await store.lookup(["k"]),
            callsign: s.state.callsign, altitude: s.state.altitude_ft,
            nullProto: Object.getPrototypeOf(s.state) === null,
            count: fix.count, pos: fix.position,
            summary: subjectSummary(s), label: subjectLabel(s), values: subjectValues(s),
            labelOfKey: subjectLabel(store.event(job, {icao: "B"})),
            polluted: ({}).callsign !== undefined,
            protoKey: proto && proto.key, listed: store.list("J").length,
            lastHeard: store.lastHeard("J").key,
            blank: store.event(job, {icao: ""}),
            blanks: store.event(job, {icao: "  "}),
            absent: store.event(job, {callsign: "X"}),
            nullKey: store.event(job, {icao: null}),
            owned: store.event({id: "O", kind: "k", owner: "W"}, {icao: "A"}),
            unrowed: store.event({id: "P", kind: "plain"}, {icao: "A"}),
            looked: store.subjectOf(job, {icao: "A"}).count,
            lookedBlank: store.subjectOf(job, {icao: " "}),
            held: store.held("J", "A"), heldAny: store.held("J", null),
            heldNone: store.held("nosuch", null),
            selectedFirst: store.selected(),
          };
          const events = [];
          store.onSelect((sel) => events.push(sel));
          const drops = [];
          store.onDrop((j, k) => drops.push([j, k]));
          store.select("J", "A");
          out.selected = store.selected();
          store.sync([{id: "J", state: {status: "stopped"}}]);
          out.afterSync = store.list("J").length;
          out.lastAfterSync = store.lastHeard("J");
          out.dropped = drops.length;
          out.selectedAfter = store.selected();
          out.selectEvents = events;
          // A watch's merged section (plan 31 spec 4.7) goes with the watch, not its decoders.
          const { mountSubjectsPane } = await import('./subjects.js');
          const el = document.createElement("div");
          document.body.append(el);
          let jobsNow = [];
          const pane = mountSubjectsPane(el, store, view, {jobs: () => jobsNow});
          const watch = (decoding, status = "running") => ({
            id: "W0123456", kind: "watch", state: {status},
            context: {watch: {decoding: decoding.map((j) => ({job: j, mode: "k"}))}}});
          const dec = {id: "D", kind: "k", state: {status: "running"},
                       tuning: {center_hz: 148637500, bandwidth_hz: 12500}};
          const look = () => {
            pane.render();
            const sec = el.querySelector('section.subject-job[data-job="W0123456"]');
            return sec && {
              h3: sec.querySelector("h3").textContent,
              channel: sec.querySelector('tr.subject[data-key="Z"] td[data-col="channel"]')
                ?.textContent ?? null,
              decoderSection: !!el.querySelector('section.subject-job[data-job="D"]'),
            };
          };
          jobsNow = [watch(["D"]), dec];
          store.sync(jobsNow);
          store.event(dec, {icao: "Z", callsign: "Q"});
          out.watchLive = look();
          jobsNow = [watch([]), {...dec, state: {status: "stopped"}}];
          store.sync(jobsNow);
          out.decoderGone = look();
          out.decoderGoneEligible = pane.eligible();
          jobsNow = [watch([])];
          store.sync(jobsNow);
          out.decoderAged = look();
          jobsNow = [watch([], "stopped")];
          store.sync(jobsNow);
          out.watchStopped = look();
          out.watchStoppedSize = store.size("W0123456");
          pane.destroy();
          el.remove();
          store.destroy();
          out.windowStore = window.zetaSubjects;
          return out;
        }""")
    f = r["fill"]
    assert f["aprs"] == "VK3ABC > APRS: 7 true", f
    assert f["nulls"] == "null null {\"a\":1}", f
    assert f["rds1"] == "ZETA FM", f
    assert f["rds2"] == "2AB1 · hello", f
    assert f["adsb"] == "7C6B2E -37.8", f
    assert f["keep"] == "x:", f
    assert f["lat"] == "S1 -27.4123", f
    assert f["noLat"] == "S1", f
    assert f["plus"] == "+3.00", f
    assert f["text"] == "S1", f
    assert f["notFormat"] == "null", f
    assert r["metric"] == ["6401 m", "780 km/h", "245°", "+5.0 m/s", "0 m", "1200 m", "90 km/h",
                           "x", "x", "", "", "-0.4 m/s", "+0.0 m/s", "0°", "0°"], r["metric"]
    assert r["learned"] is True and r["again"] is False, r
    assert r["before"] is True, r  # undefined: not yet known
    assert r["noRow"] is None, r
    assert r["callsign"] == "QFA1", r  # a null never erases a value
    assert r["altitude"] == 0, r  # and 0 is a value
    assert r["nullProto"] is True, r
    assert r["count"] == 5, r
    assert r["pos"] == {"lat": 3, "lon": 4, "alt_m": 100}, r
    assert r["summary"] == "A QFA1 0", r
    assert r["label"] == "QFA1" and r["labelOfKey"] == "B", r
    assert r["values"][0]["label"] == "Callsign" and r["values"][0]["text"] == "QFA1", r
    assert r["values"][1]["text"] == "0 m", r
    assert r["polluted"] is False and r["protoKey"] == "__proto__", r
    assert r["listed"] == 3 and r["lastHeard"] == "B", r
    for name in ("blank", "blanks", "absent", "nullKey", "owned", "unrowed", "lookedBlank"):
        assert r[name] is None, f"{name}: {r[name]}"
    assert r["looked"] == 5, r  # subjectOf neither merges nor counts
    assert r["held"] is True and r["heldAny"] is True and r["heldNone"] is False, r
    assert r["selectedFirst"] is None, r
    # A job no watch keeps is its own group (plan 31 spec 4.7).
    assert r["selected"] == {"job": "J", "group": "J", "key": "A"}, r
    assert r["afterSync"] == 0 and r["lastAfterSync"] is None and r["dropped"] == 3, r
    assert r["selectedAfter"] is None, r
    assert r["selectEvents"][-1] is None, r
    assert r["windowStore"] is None, r
    live = {"h3": "watch 123456 · aircraft", "channel": "148.6375", "decoderSection": False}
    assert r["watchLive"] == live, r["watchLive"]
    # The decoder lost (stopped, then gone from the list) while the watch runs: the section
    # stays, its Channel the decoder's last centre, and the tab stays eligible.
    assert r["decoderGone"] == live, r["decoderGone"]
    assert r["decoderGoneEligible"] is True, r
    assert r["decoderAged"] == live, r["decoderAged"]
    # The watch stopped: its subjects and section go.
    assert r["watchStopped"] is None and r["watchStoppedSize"] == 0, r


def scenario_subjects_table_merges_three_aircraft(page, url, secret):
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        rows = page.locator("#subjects tr.subject")
        expect(rows).to_have_count(3, timeout=10_000)
        section = page.locator("#subjects section.subject-job")
        expect(section).to_have_count(1)
        expect(section.locator("h3")).to_have_text("fake_subjects tone, 3 aircraft")
        # Every line in, never more than three rows.
        page.wait_for_function(
            "(job) => window.zetaSubjects.list(job).reduce((n, s) => n + s.count, 0) === 8",
            arg=job, timeout=5_000)
        expect(rows).to_have_count(3)
        a = subject_row(page, "7C0001")
        for text in ("QFA1", "6401 m", "780 km/h", "245°", "-27.41, 153.11"):
            expect(a).to_contain_text(text)
        expect(a.locator('td[data-col="count"]')).to_have_text("5")
        expect(a.locator('td[data-col="last_seen"]')).to_have_text(re.compile(r"^\d+ s$"))
        expect(subject_row(page, "7C0002")).to_contain_text("0 m")
        # Decoder text is shown as text.
        c = subject_row(page, "7C0003").locator('td[data-col="callsign"]')
        expect(c).to_have_text("<b>JST3</b>")
        expect(c).to_have_attribute("title", "<b>JST3</b>")
        expect(page.locator("#subjects b")).to_have_count(0)
        assert subject_keys(page)[0] == "7C0001", subject_keys(page)
        assert page.evaluate("(job) => window.zetaSubjects.list(job).length", job) == 3
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_subjects_tab_appears_only_with_a_subject_job(page, url, secret):
    select_source(page, url, secret, "tone")
    toggle = pane_tab(page)
    pane = page.locator("#subjects")
    expect(toggle).to_be_hidden()
    expect(pane).to_be_hidden()
    pos = start_fake_pos(page, url, secret)
    try:
        page.evaluate("window.zetaTest.poll()")
        page.wait_for_function("() => window.zetaSubjects.rowOf('fake_pos') !== undefined")
        expect(toggle).to_be_hidden()
        expect(pane).to_be_hidden()
    finally:
        stop_job(url, secret, pos)
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        # Eligible: the tab shows; the pane waits for it to be chosen.
        expect(toggle).to_be_visible(timeout=10_000)
        expect(pane).to_be_hidden()
        toggle.click()
        expect(pane).to_be_visible()
        assert page.evaluate("document.documentElement.dataset.strip") == "subjects"
        wf = page.locator("#waterfall").bounding_box()
        box = page.locator("#pane").bounding_box()
        assert abs(box["y"] - (wf["y"] + wf["height"])) <= 2, f"{wf} {box}"
        run_cmd(page, "view set subjects off")
        expect(pane).to_be_hidden()
        expect(toggle).to_be_visible()
        assert page.evaluate("document.documentElement.dataset.strip") is None
        toggle.click()
        expect(pane).to_be_visible()
        assert "pane subjects" in view_show(page)
        # With the map shown too, the map takes the top and the table the bottom.
        page.route(TILE_HOSTS, lambda r: r.abort())
        run_cmd(page, "view set main map")
        expect(page.locator("#map")).to_be_visible()
        mp = page.locator("#map").bounding_box()
        box = pane.bounding_box()
        assert mp["y"] + mp["height"] <= box["y"] + 2, f"{mp} {box}"
        run_cmd(page, "view set main rf")
    finally:
        stop_job(url, secret, job)
    page.evaluate("window.zetaTest.poll()")
    run_cmd(page, "view set subjects auto")
    run_cmd(page, "view set pane none")
    expect(pane).to_be_hidden()
    expect(toggle).to_be_hidden()
    run_cmd(page, "view set subjects on")
    expect(pane).to_be_visible()
    expect(toggle).to_be_visible()
    expect(page.locator("#subjects section.subject-job")).to_have_count(0)


def scenario_subjects_table_has_padding_and_scrolls_sideways(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    select_source(page, url, secret, "tone")
    page.set_viewport_size(WIDE)
    run_cmd(page, "view set main map")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        pane = page.locator("#subjects").bounding_box()
        first = page.locator("#subjects tbody tr.subject td").first.bounding_box()
        assert first["x"] - pane["x"] >= 4, f"{first} {pane}"
        last = page.locator("#subjects thead th").last
        sec = page.locator("#subjects section.subject-job")
        assert sec.evaluate("(e) => e.scrollWidth >= e.clientWidth") is True
        last.scroll_into_view_if_needed()
        lb, pb = last.bounding_box(), page.locator("#subjects").bounding_box()
        assert lb["x"] >= pb["x"] and lb["x"] + lb["width"] <= pb["x"] + pb["width"] + 1, f"{lb} {pb}"
        # The key stays in view while the rest is scrolled in.
        fb = page.locator("#subjects thead th").first.bounding_box()
        assert fb["x"] - pb["x"] >= 4, f"{fb} {pb}"
        run_cmd(page, "view set main rf")
    finally:
        stop_job(url, secret, job)


def scenario_subjects_dim_and_drop(page, url, secret):
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set subject_dim_s 0")
    expect(view_line(page, "subject_dim_s", error=True)).to_contain_text(
        "a positive integer of seconds")
    run_cmd(page, "view set subject_dim_s 1")
    expect(view_line(page, "subject_dim_s = 1")).to_be_visible()
    run_cmd(page, "view set subject_drop_s 4")
    expect(view_line(page, "subject_drop_s = 4")).to_be_visible()
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        rows = page.locator("#subjects tr.subject")
        expect(rows).to_have_count(3, timeout=10_000)
        expect(page.locator("#subjects tr.subject.dim")).to_have_count(3, timeout=3_000)
        expect(rows).to_have_count(0, timeout=8_000)
    finally:
        stop_job(url, secret, job)


def scenario_subjects_distance_and_sort(page, url, secret):
    # No station, so the table has no distance column until `view set home` (the page reads
    # the station at load).
    http_json(url, secret, "/op", {"op": "station_clear"})
    try:
        subjects_distance_and_sort(page, url, secret)
    finally:
        set_demo_station(url, secret)


def subjects_distance_and_sort(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        expect(page.locator('#subjects th[data-col="distance"]')).to_have_count(0)
        run_cmd(page, "view set home -27.50,153.00")
        expect(page.locator('#subjects th[data-col="distance"]')).to_have_count(1)
        dist = lambda key: subject_row(page, key).locator('td[data-col="distance"]')
        expect(dist("7C0001")).to_have_text("14.8 km, 47°")
        expect(dist("7C0003")).to_have_text("")
        icao = page.locator('#subjects th[data-col="icao"]')
        icao.click()
        assert subject_keys(page) == ["7C0001", "7C0002", "7C0003"], subject_keys(page)
        icao.click()
        assert subject_keys(page) == ["7C0003", "7C0002", "7C0001"], subject_keys(page)
        # Only 7C0001 has a track: nulls last, whichever way it is sorted.
        track = page.locator('#subjects th[data-col="track_deg"]')
        track.click()
        assert subject_keys(page)[0] == "7C0001", subject_keys(page)
        track.click()
        assert subject_keys(page)[0] == "7C0001", subject_keys(page)
    finally:
        stop_job(url, secret, job)


def scenario_subjects_narrow_shows_three_columns(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    page.set_viewport_size(NARROW)  # its row is in the drawer from here
    try:
        toggle = pane_tab(page)
        expect(toggle).to_be_visible(timeout=10_000)
        pane = page.locator("#subjects")
        expect(pane).to_be_hidden()
        toggle.click()
        expect(pane).to_be_visible()
        assert page.evaluate("document.documentElement.dataset.pane") == "subjects"
        box = pane.bounding_box()
        assert box["x"] == 0 and box["width"] >= NARROW["width"] - 2, box
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        ths = page.locator("#subjects th")
        expect(ths).to_have_count(3)
        assert ths.all_text_contents() == ["icao", "Altitude", "Last seen"], ths.all_text_contents()
        toggle.click()
        expect(pane).to_be_hidden()
        assert page.evaluate("document.documentElement.dataset.pane") is None
    finally:
        stop_job(url, secret, job)


def scenario_subjects_survive_a_source_switch(page, url, secret):
    select_source(page, url, secret, "tone")
    a = start_subject_job(page, url, secret, "fake_subjects", bandwidth_hz=40_000)
    b = start_subject_job(page, url, secret, "fake_fold", bandwidth_hz=41_000)
    c = None
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(5, timeout=10_000)
        page.wait_for_function(
            """([a, b]) => [a, b].every((j) => window.zetaSubjects.list(j)
                 .reduce((n, s) => n + s.count, 0) === (j === a ? 8 : 5))""",
            arg=[a, b], timeout=5_000)
        before = [subject_counts(page, a), subject_counts(page, b)]
        assert before[1] == {"S1": 2, "S2": 3}, before
        for source in ("aprs", "tone"):
            page.click(f'.src-tab[data-id="{source}"]')
            page.evaluate("window.zetaTest.poll()")
            page.wait_for_timeout(1500)
            expect(page.locator("#subjects tr.subject")).to_have_count(5)
            after = [subject_counts(page, a), subject_counts(page, b)]
            assert after == before, f"on {source}: {before} -> {after}"
        # A stopped job's section goes at the next poll; the other's stays.
        stop_job(url, secret, a)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator("#subjects section.subject-job")).to_have_count(1)
        expect(page.locator("#subjects tr.subject")).to_have_count(2)
        stop_job(url, secret, b)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator("#subjects section.subject-job")).to_have_count(0)
        # A keyed row with no `position` on a source the page does not show is followed too.
        page.click('.src-tab[data-id="aprs"]')
        expect(page.locator('.src-tab[data-id="aprs"]')).to_have_attribute(
            "aria-selected", "true")
        c = start_subject_job(page, url, secret, "fake_fold", bandwidth_hz=42_000)
        run_cmd(page, "view set subjects on")
        expect(subject_row(page, "S1")).to_have_count(1, timeout=10_000)
        expect(subject_row(page, "S2")).to_have_count(1)
    finally:
        for job in (a, b, c):
            if job:
                stop_job(url, secret, job)


def scenario_subjects_name_the_jobs_not_watched(page, url, secret):
    # Every job on the shown source is watched (its feed): the cap in play is MAX_POSITIONED,
    # on the positioned jobs of the other sources, so the five run on `retune`.
    select_source(page, url, secret, "tone")
    jobs = []
    try:
        for bw in (40_000, 41_000, 42_000, 43_000, 44_000):
            jobs.append(start_job(url, secret, "fake_subjects", "retune", 250_000_000, bw))
        job_row(page, jobs[-1])
        open_subjects(page)
        sections = page.locator("#subjects section.subject-job")
        expect(sections).to_have_count(5, timeout=10_000)
        marked = page.locator("#subjects section.subject-job .not-watched")
        expect(marked).to_have_count(1)
        expect(marked).to_have_text("not watched (limit 4)")
        oldest = page.locator(f'#subjects section.subject-job[data-job="{jobs[0]}"]')
        expect(oldest.locator(".not-watched")).to_have_count(1)
        for job in jobs[1:]:
            rows = page.locator(f'#subjects section.subject-job[data-job="{job}"] tr.subject')
            expect(rows).to_have_count(3, timeout=10_000)
    finally:
        for job in jobs:
            stop_job(url, secret, job)


def scenario_subjects_merge_across_rows(page, url, secret):
    # Two links to the same aircraft (plan 30 spec 4.8): fake_acars keyed by `reg`, fake_vdl2
    # by `icao` with a `reg`, both `merge` and both `aircraft`, so one table. fake_vdl2's ground
    # station 1A2B3C carries VH-ZET's `reg` from its uplink but matches `merge_skip`: a row of
    # its own.
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    a = start_subject_job(page, url, secret, "fake_acars", bandwidth_hz=41_000)
    v = start_subject_job(page, url, secret, "fake_vdl2", bandwidth_hz=42_000)
    try:
        open_subjects(page)
        merged = page.locator("#subjects section.subject-merged")
        expect(merged).to_have_count(1, timeout=10_000)
        expect(page.locator("#subjects section.subject-job")).to_have_count(0)
        expect(merged.locator("h3")).to_contain_text("aircraft, 4", timeout=10_000)
        zet = merged.locator('tr.subject:has(td[data-col="reg"]:text-is("VH-ZET"))')
        expect(zet).to_have_count(1)
        expect(zet.locator('td[data-col="via"]')).to_have_text("fake_acars, fake_vdl2")
        expect(zet.locator('td[data-col="count"]')).to_have_text("4")
        # VH-ZET, VH-ABC, 7C0001 and the station 1A2B3C
        expect(merged.locator("tr.subject")).to_have_count(4)
        station = merged.locator('tr.subject[data-key="1A2B3C"]')
        expect(station).to_have_count(1)
        expect(station.locator('td[data-col="via"]')).to_have_text("fake_vdl2")
        expect(station.locator('td[data-col="reg"]')).to_have_text("1A2B3C")
        # A header sorts the merged table as any other: by count ascending, VH-ZET last.
        merged.locator('th[data-col="count"]').click()
        last = merged.locator("tr.subject").last.locator('td[data-col="reg"]')
        expect(last).to_have_text("VH-ZET")
        zet.dispatch_event("pointerdown", {"button": 0})
        expect(zet).to_have_class(re.compile(r"\bselected\b"))
        assert not errors, f"console errors: {errors}"
    finally:
        http_json(url, secret, "/op", {"op": "job_stop", "job": v})
    expect(page.locator("#subjects section.subject-merged")).to_have_count(0)
    expect(page.locator('#subjects section.subject-job[data-job="%s"]' % a)).to_have_count(1)
    http_json(url, secret, "/op", {"op": "job_stop", "job": a})


# ---- the waterfall's one line (plan 23 Task 3) -----------------------------------------

def scenario_summary_line_replaces_ribbons_per_aircraft(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        ribbons = page.locator("#waterfall .ov-ribbon.fake_subjects")
        expect(ribbons).to_have_text("fake_subjects 3 aircraft, last QFA1", timeout=15_000)
        page.wait_for_timeout(3_000)
        expect(ribbons).to_have_count(1)
        expect(ribbons).to_have_text("fake_subjects 3 aircraft, last QFA1")
        for icao in ("7C0001", "7C0003"):
            expect(page.locator("#waterfall .ov-ribbon", has_text=icao)).to_have_count(0)
        expect(page.locator("#waterfall b")).to_have_count(0)
        assert ribbons.evaluate("e => e.style.right") == "0px"
    finally:
        stop_job(url, secret, job)


def scenario_fold_gives_one_ribbon_per_subject(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_fold")
    try:
        ribbons = page.locator("#waterfall .ov-ribbon.fake_fold")
        expect(ribbons).to_have_count(2, timeout=15_000)
        s1 = ribbons.filter(has_text="S1")
        s2 = ribbons.filter(has_text="S2")
        expect(s1).to_contain_text("×2", timeout=5_000)
        expect(s2).to_contain_text("×3", timeout=5_000)
        expect(ribbons).to_have_count(2)
    finally:
        stop_job(url, secret, job)


def scenario_unkeyed_row_keeps_separate_ribbons(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_fake_pos(page, url, secret)
    try:
        ribbons = page.locator("#waterfall .ov-ribbon.fake_pos")
        expect(ribbons).to_have_count(2, timeout=15_000)
        expect(ribbons.filter(has_text="S1 -27.40,153.10")).to_have_count(1)
        expect(ribbons.filter(has_text="S1 -27.41,153.11")).to_have_count(1)
    finally:
        stop_job(url, secret, job)


def scenario_silent_ribbon_moves_off_the_edge(page, url, secret):
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set subject_dim_s 1")
    expect(view_line(page, "subject_dim_s = 1")).to_be_visible()
    job = start_subject_job(page, url, secret, "fake_fold")
    try:
        ribbons = page.locator("#waterfall .ov-ribbon.fake_fold")
        expect(ribbons).to_have_count(2, timeout=15_000)
        page.wait_for_function(
            """() => {
              const els = [...document.querySelectorAll('#waterfall .ov-ribbon.fake_fold')];
              return els.length > 0 && els.every((e) => parseFloat(e.style.right) > 0);
            }""", timeout=10_000)
    finally:
        stop_job(url, secret, job)


def scenario_slow_row_lookup_draws_no_ribbon_per_message(page, url, secret):
    def route(r):
        if "modes_show" in (r.request.post_data or ""):
            page.wait_for_timeout(500)
        r.continue_()
    page.route("**/op", route)
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        most = 0
        for _ in range(40):
            most = max(most, page.locator("#waterfall .ov-ribbon.fake_subjects").count())
            page.wait_for_timeout(100)
        assert most <= 1, f"{most} ribbons at once while the row was looked up"
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
    finally:
        stop_job(url, secret, job)


def scenario_keyed_ribbons_with_equal_text_stay_two(page, url, secret):
    select_source(page, url, secret, "tone")
    r = page.evaluate(
        """async () => {
          const t = window.zetaTest;
          t.keyedRibbon("a", 100e6, "same", "kr", 1);
          t.keyedRibbon("b", 100e6, "same", "kr", 1);
          t.keyedRibbon("b", 100e6, "same", "kr", 2);
          t.keyedRibbon("a", 100e6, "same", "kr", 3);
          await new Promise((done) => setTimeout(done, 700));
          return [...document.querySelectorAll('#waterfall .ov-ribbon.kr .count')]
            .map((e) => e.textContent.trim());
        }""")
    assert r == ["×3", "×2"], r


# ---- riding ribbons (plan 24 Task 4) -----------------------------------------------------

def scenario_ribbon_rides_band(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = current_capture(url, secret, "retune")["center_hz"]
    job = start_job(url, secret, "passthrough", "retune", cur, 10_000)
    try:
        page.evaluate("window.zetaTest.poll()")
        ol = page.locator(f'#waterfall .outline[data-key="{job}"]').bounding_box()
        page.evaluate(f"window.zetaTest.ribbon({cur}, 'first chunk', 'transcript')")
        first = page.locator("#waterfall .ov-ribbon.transcript", has_text="first chunk")
        fb = first.bounding_box()
        assert abs((fb["y"] + fb["height"] / 2) - (ol["y"] + ol["height"] / 2)) <= 2, (fb, ol)
        tilt = page.evaluate("getComputedStyle(document.documentElement).getPropertyValue('--ribbon-tilt').trim()")
        assert tilt == "-15deg", tilt
        page.evaluate(f"window.zetaTest.ribbon({cur}, 'second chunk', 'transcript')")
        second = page.locator("#waterfall .ov-ribbon.transcript", has_text="second chunk")
        page.wait_for_timeout(300)
        top1 = page.evaluate("el => parseFloat(el.style.top)", first.element_handle())
        top2 = page.evaluate("el => parseFloat(el.style.top)", second.element_handle())
        h = page.evaluate("el => el.offsetHeight", second.element_handle())
        assert abs((top2 - top1) - (h + 2)) <= 1, (top1, top2, h)
        # The ribbons' z-index stays inside the overlay layer (menus, badge and drawer above).
        iso = page.evaluate("getComputedStyle(document.querySelector('#waterfall .ov-layer')).isolation")
        assert iso == "isolate", iso
        # A job's ribbon takes the job's colour wherever it sits: here outside the job's band.
        colour = page.locator(f'#waterfall .outline[data-key="{job}"]').get_attribute("data-colour")
        assert colour, "outline without a colour"
        page.evaluate(
            f"window.zetaTest.ribbon({cur + 20_000}, 'job chunk', 'transcript', null, {json.dumps(job)})")
        expect(page.locator("#waterfall .ov-ribbon.transcript", has_text="job chunk")
               ).to_have_attribute("data-colour", colour)
        # A keyed ribbon (a subject) never steps: a second one with the same key replaces in place.
        page.evaluate(f"window.zetaTest.ribbon({cur + 3_000}, 'VH-A 1', 'kr', 'VH-A')")
        page.evaluate(f"window.zetaTest.ribbon({cur + 3_000}, 'VH-A 2', 'kr', 'VH-A')")
        expect(page.locator("#waterfall .ov-ribbon.kr")).to_have_count(1)
    finally:
        stop_job(url, secret, job)


def scenario_ribbon_clips_at_other_band(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = current_capture(url, secret, "retune")["center_hz"]
    set_band(page, cur, 40_000)
    lo = start_job(url, secret, "passthrough", "retune", cur - 2_000, 1_000)
    hi = start_job(url, secret, "passthrough", "retune", cur + 2_000, 1_000)
    try:
        page.evaluate("window.zetaTest.poll()")
        for i in range(4):
            page.evaluate(f"window.zetaTest.ribbon({cur - 2_000}, 'lo {i}', 'transcript')")
        page.wait_for_timeout(300)
        hi_y = page.locator(f'#waterfall .outline[data-key="{hi}"]').bounding_box()
        hi_c = hi_y["y"] + hi_y["height"] / 2
        for el in page.locator("#waterfall .ov-ribbon.transcript").element_handles():
            b = el.bounding_box()
            assert not (b["y"] <= hi_c <= b["y"] + b["height"]), f"ribbon crossed the other band: {b} {hi_c}"
    finally:
        stop_job(url, secret, lo)
        stop_job(url, secret, hi)


def scenario_dir_auto(page, url, secret):
    select_source(page, url, secret, "tone")
    cur = current_capture(url, secret, "tone")["center_hz"]
    page.evaluate(f"window.zetaTest.ribbon({cur}, 'مرحبا بالعالم', 'transcript')")
    rib = page.locator("#waterfall .ov-ribbon.transcript", has_text="مرحبا")
    assert rib.get_attribute("dir") == "auto"
    assert page.evaluate("el => getComputedStyle(el).direction", rib.element_handle()) == "rtl"
    run_cmd(page, "view get theme")
    line = page.locator("#transcript .line").last
    assert line.locator("span").first.get_attribute("dir") == "auto"


def scenario_protocol_text_has_no_adsb_case(page, url, secret):
    login(page, url, secret)
    r = page.evaluate(
        """async () => {
          const { protocolText } = await import('./overlays.js');
          return [protocolText({protocol: "adsb", icao: "X"}),
                  protocolText({protocol: "acars", reg: "R", label: "H1", text: "t"})];
        }""")
    assert r == [None, "R H1: t"], r


# ---- runner ----------------------------------------------------------------------------

# ---- the map, per subject (plan 23 Task 4) ----------------------------------------------

def map_with_subjects(page, url, secret):
    """The map on, its tiles aborted, and `fake_subjects` running on the tone source with both
    of its fixed aircraft drawn; the job id."""
    page.route(TILE_HOSTS, lambda r: r.abort())
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set main map")
    expect(page.locator("#map")).to_be_visible()
    job = start_subject_job(page, url, secret, "fake_subjects")
    expect(page.locator("#map .map-subject")).to_have_count(2, timeout=10_000)
    return job


def map_subject(page, text):
    return page.locator("#map .map-subject").filter(has_text=text)


def scenario_map_draws_one_marker_per_subject(page, url, secret):
    errors = page_errors(page)
    job = map_with_subjects(page, url, secret)
    try:
        expect(page.locator("#map path.map-track")).to_have_count(2)
        expect(page.locator("#map path.map-pos")).to_have_count(0)
        # Every line in: 7C0001's two fixes are a move and one line segment.
        page.wait_for_function(
            "(job) => window.zetaSubjects.list(job).reduce((n, s) => n + s.count, 0) === 8",
            arg=job, timeout=5_000)
        page.wait_for_function(
            """() => [...document.querySelectorAll('#map path.map-track')].some(
                 (e) => (e.getAttribute('d').match(/[ML]/g) || []).length === 2)""",
            timeout=5_000)
        # The heading and speed came in a message with no fix, after the last fix.
        qfa = map_subject(page, "QFA1")
        expect(qfa.locator(".label")).to_have_text("QFA1 6401 m")
        transform = qfa.locator(".arrow").evaluate("(e) => e.style.transform")
        assert "rotate(245deg)" in transform, transform
        voz = map_subject(page, "VOZ2")
        expect(voz.locator(".label")).to_have_text("VOZ2 0 m")
        assert voz.locator(".arrow").evaluate("(e) => e.style.transform") == ""
        expect(voz.locator(".arrow.dot")).to_have_count(1)
        # The popup names the subject and lists its values.
        qfa.click()
        popup = page.locator("#map .leaflet-popup-content")
        expect(popup).to_contain_text("QFA1")
        expect(popup).to_contain_text("780 km/h")
        # Two subjects: the table has them, the job's row has no distance.
        run_cmd(page, "view set home -27.50,153.00")
        expect(page.locator(f'#jobs .job[data-id="{job}"]')).to_be_visible()
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#jobs .job[data-id="{job}"] .pos')).to_have_count(0)
        # Decoder text stays text, in the marker and in its popup.
        page.evaluate(
            """async () => {
              const { state } = await import('./app.js');
              state.map.map.closePopup();
            }""")
        page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const job = {id, kind: 'fake_subjects'};
              const p = {icao: '7C0004', callsign: '<b>HOSTILE</b>',
                         position: {lat: -27.35, lon: 153.15, alt_m: null}};
              state.subjects.event(job, p);
              state.map.event(job, p);
            }""", job)
        hostile = map_subject(page, "HOSTILE")
        expect(hostile.locator(".label")).to_have_text("<b>HOSTILE</b>")
        hostile.click()
        # The fading popup of QFA1 may still be there.
        expect(popup.last).to_contain_text("<b>HOSTILE</b>")
        expect(page.locator("#map b")).to_have_count(0)
        # The job's markers and tracks go with it.
        stop_job(url, secret, job)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator("#map .map-subject")).to_have_count(0)
        expect(page.locator("#map path.map-track")).to_have_count(0)
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_map_tracks_an_aprs_station(page, url, secret):
    # Plan 29 Task 6: the APRS row maps a decoded position (the fixture's packet is
    # "!2723.05S/15307.05E>ZetaSDR", Brisbane Airport), so the station lands on the map and in
    # the Subjects table.
    page.route(TILE_HOSTS, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "aprs")
    run_cmd(page, "view set main map")
    mp = page.locator("#map")
    expect(mp).to_be_visible()
    job = start_job(url, secret, "aprs_decode", "aprs", 144_800_000, 12_500)
    try:
        job_row(page, job)
        station = map_subject(page, "VK3ABC")
        expect(station).to_have_count(1, timeout=15_000)
        station.click()
        expect(mp.locator(".leaflet-popup-content")).to_contain_text("VK3ABC")
        open_subjects(page)
        row = subject_row(page, "VK3ABC")
        expect(row).to_be_visible()
        expect(row.locator('td[data-col="position"]')).to_contain_text("-27.38, 153.12")
        # The audio-row rule (Task 4): a row whose outputs list audio counts as an audio job.
        audio = page.evaluate(
            """() => import('./agent.js').then((m) => [
                 m.isAudioJob({kind: 'm17_decode'}, () => ({outputs: ['event', 'audio']})),
                 m.isAudioJob({kind: 'aprs_decode'}, () => ({outputs: ['event']})),
                 m.isAudioJob({kind: 'fm_demod'}, () => null)])""")
        assert audio == [True, False, True], f"isAudioJob {audio}"
        # The store the jobs list asks (`audioKind`) carries the live row's outputs.
        live = page.evaluate(
            """async () => {
                 const { state } = await import('./app.js');
                 await state.subjects.lookup(['aprs_decode', 'm17_decode']);
                 const m = await import('./agent.js');
                 const rowOf = (k) => state.subjects.rowOf(k);
                 return [rowOf('aprs_decode')?.outputs ?? null,
                         m.isAudioJob({kind: 'aprs_decode'}, rowOf),
                         rowOf('m17_decode')?.outputs ?? null,
                         m.isAudioJob({kind: 'm17_decode'}, rowOf)];
               }""")
        assert live == [["event"], False, ["event", "audio"], True], f"live row outputs {live}"
        run_cmd(page, "view set main rf")
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)
        page.evaluate("window.zetaTest.poll()")
        run_cmd(page, "view set subjects auto")
        run_cmd(page, "view set pane none")


def scenario_a_keyed_job_with_one_subject_keeps_its_row_distance(page, url, secret):
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set home -27.50,153.00")
    expect(view_line(page, "home = -27.5,153")).to_be_visible()
    r = page.evaluate(
        """async () => {
          const { state } = await import('./app.js');
          // lookup() returns at once while the page's own lookup is in flight: poll
          // until the row is known.
          for (let i = 0; !state.subjects.rows.has('fake_subjects'); i += 1) {
            if (i > 200) throw new Error('the fake_subjects row was never looked up');
            await state.subjects.lookup(['fake_subjects']);
            await new Promise((r) => setTimeout(r, 50));
          }
          // In one tick: a jobs poll drops the subjects of a job that is not running.
          const job = {id: 'stub', kind: 'fake_subjects'};
          const fix = (icao, lat, lon) => {
            const p = {icao, position: {lat, lon, alt_m: null}};
            state.subjects.event(job, p);
            state.map.event(job, p);
          };
          fix('AAAAAA', -27.41, 153.11);
          const one = state.map.rowText('stub');
          fix('BBBBBB', -27.30, 153.20);
          const two = state.map.rowText('stub');
          state.subjects.drop('stub', 'AAAAAA');
          const back = state.map.rowText('stub');
          state.subjects.drop('stub', 'BBBBBB');
          return {one, two, back, none: state.map.rowText('stub')};
        }""")
    assert r["one"] == "14.8 km, 47°", r
    assert r["two"] is None, r
    assert r["back"] is not None and r["back"] != r["one"], r
    assert r["none"] is None, r


def scenario_a_subject_track_thins_at_its_cap(page, url, secret):
    select_source(page, url, secret, "tone")
    r = page.evaluate(
        """async () => {
          const { state } = await import('./app.js');
          const { SUBJECT_TRACK_POINTS, TRACK_POINTS } = await import('./map.js');
          // lookup() returns at once while the page's own lookup is in flight: poll
          // until the row is known.
          for (let i = 0; !state.subjects.rows.has('fake_subjects'); i += 1) {
            if (i > 200) throw new Error('the fake_subjects row was never looked up');
            await state.subjects.lookup(['fake_subjects']);
            await new Promise((r) => setTimeout(r, 50));
          }
          const job = {id: 'stub', kind: 'fake_subjects'};
          let most = 0;
          const fix = (lat, lon) => {
            const p = {icao: 'AAAAAA', position: {lat, lon, alt_m: null}};
            state.subjects.event(job, p);
            state.map.event(job, p);
          };
          let t = null;
          for (let i = 0; i < 150; i += 1) {
            fix(-27 - i / 1000, 153);
            t = state.map.tracks.get('stub\\u0000AAAAAA');
            most = Math.max(most, t.points.length);
          }
          const out = {most, first: t.points[0], last: t.points[t.points.length - 1],
                       job: t.job, key: t.key, cap: SUBJECT_TRACK_POINTS, unkeyed: TRACK_POINTS,
                       entries: state.map.tracks.size};
          // An event with no fix touches no geometry.
          const points = t.points;
          state.subjects.event(job, {icao: 'AAAAAA', callsign: 'QFA1'});
          state.map.event(job, {icao: 'AAAAAA', callsign: 'QFA1'});
          out.untouched = t.points === points;
          state.subjects.drop('stub', 'AAAAAA');
          return out;
        }""")
    assert r["cap"] == 100 and r["unkeyed"] == 3600, r
    assert r["most"] <= 100, r
    assert r["first"] == [-27, 153], r
    assert r["last"] == [-27.149, 153], r
    assert r["untouched"] is True, r
    assert (r["job"], r["key"], r["entries"]) == ("stub", "AAAAAA", 1), r


def scenario_track_thinning_functions(page, url, secret):
    login(page, url, secret)
    r = page.evaluate(
        """async () => {
          const { addToTrack, distanceM, drawnPoints, thinTrack } = await import('./map.js');
          // 5000 moving fixes through a track capped at 100.
          let kept = [];
          let most = 0;
          const opts = {cap: 100, minM: 0};
          for (let i = 0; i < 5000; i += 1) {
            kept = addToTrack(kept, {lat: i / 1000, lon: 0, alt_m: null, heading: null, n: i}, opts);
            most = Math.max(most, kept.length);
          }
          const ns = kept.map((p) => p.n);
          let tail = 1;
          while (tail < ns.length && ns[ns.length - 1 - tail] === ns[ns.length - tail] - 1) tail += 1;
          // A stationary subject keeps one point, and the line still ends at the latest fix.
          let still = [];
          const here = {lat: -27, lon: 153, alt_m: 100, heading: null};
          for (let i = 0; i < 50; i += 1) still = addToTrack(still, {...here}, {cap: 100, minM: 500});
          // Distance counts altitude; an unknown altitude counts for nothing.
          const up = {lat: -27, lon: 153, alt_m: 1100, heading: null};
          const blind = {lat: -27, lon: 153, alt_m: null, heading: null};
          const climbed = addToTrack([here], up, {cap: 100, minM: 500}).length;
          const unknown = addToTrack([here], blind, {cap: 100, minM: 500}).length;
          // A turn past 10 degrees joins however near; across north counts the short way.
          const at = (heading) => ({lat: -27, lon: 153, alt_m: null, heading});
          const turn = (a, b) => addToTrack([at(a)], at(b), {cap: 100, minM: 500}).length;
          const latest = {lat: -27.1, lon: 153, alt_m: 100, heading: null};
          return {
            most, first: ns[0], last: ns[ns.length - 1], tail, count: ns.length,
            still: still.length,
            metres: distanceM(here, up), blindMetres: distanceM(up, blind),
            climbed, unknown,
            turns: [turn(350, 5), turn(350, 1), turn(10, 20), turn(10, 21), turn(null, 90),
                    turn(90, null)],
            drawn: drawnPoints(still, latest),
            drawnStill: drawnPoints(still, here),
            thin4: thinTrack([0, 1, 2, 3]),
            thin9: thinTrack([0, 1, 2, 3, 4, 5, 6, 7, 8]),
          };
        }""")
    assert r["most"] <= 100, r
    assert r["first"] == 0 and r["last"] == 4999, r
    assert r["tail"] >= 50, r
    assert r["count"] >= 50, r
    assert r["still"] == 1, r
    assert abs(r["metres"] - 1000) < 0.001 and r["blindMetres"] == 0, r
    assert r["climbed"] == 2 and r["unknown"] == 1, r
    assert r["turns"] == [2, 2, 1, 2, 1, 1], r  # 15, 11, not 10, 11; unknowns never
    assert r["drawn"] == [[-27, 153], [-27.1, 153]], r
    assert r["drawnStill"] == [[-27, 153]], r
    assert r["thin4"] == [0, 2, 3], r
    assert r["thin9"] == [0, 2, 4, 5, 6, 7, 8], r


def scenario_table_and_map_select_each_other(page, url, secret):
    errors = page_errors(page)
    job = map_with_subjects(page, url, secret)
    try:
        open_subjects(page)
        selected = page.locator("#map .map-subject.selected")
        expect(selected).to_have_count(0)
        subject_row(page, "7C0001").click()
        expect(subject_row(page, "7C0001")).to_have_class(re.compile(r"\bselected\b"))
        expect(selected).to_have_count(1)
        expect(selected).to_contain_text("QFA1")
        # A subject with no fix selects in the table and marks no marker.
        subject_row(page, "7C0003").click()
        expect(selected).to_have_count(0)
        subject_row(page, "7C0001").click()
        expect(selected).to_have_count(1)
        # A selection off-screen pans to it; one on-screen does not.
        moved = page.evaluate(
            """async () => {
              const { state } = await import('./app.js');
              const m = state.map.map;
              const at = () => { const c = m.getCenter(); return [c.lat, c.lng]; };
              const before = at();
              const job = state.subjects.selected().job;
              state.subjects.select(job, '7C0002');
              const on = at();
              m.setView([40, 0], 10, {animate: false});
              const away = m.getBounds().contains([-27.41, 153.11]);
              state.subjects.select(job, '7C0001');
              const shown = m.getBounds().contains([-27.41, 153.11]);
              return {same: on[0] === before[0] && on[1] === before[1], away, shown};
            }""")
        assert moved == {"same": True, "away": False, "shown": True}, moved
        page.evaluate(
            """async () => {
              const { state } = await import('./app.js');
              state.map.map.setView([-27.35, 153.15], 10, {animate: false});
            }""")
        # A marker click selects its row, in view, and only that marker.
        map_subject(page, "VOZ2").click()
        row = subject_row(page, "7C0002")
        expect(row).to_have_class(re.compile(r"\bselected\b"))
        expect(row).to_be_in_viewport()
        expect(selected).to_have_count(1)
        expect(selected).to_contain_text("VOZ2")
        expect(page.locator("#subjects tr.subject.selected")).to_have_count(1)
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_selected_row_scrolls_into_view(page, url, secret):
    # A short pane, so the last row starts below its fold.
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const pane = document.querySelector('#subjects');
              pane.style.height = '60px';
              pane.style.bottom = 'auto';
              pane.scrollTop = 0;
              const rows = [...pane.querySelectorAll('tr.subject')];
              const tr = rows[rows.length - 1];
              const before = tr.getBoundingClientRect().bottom > pane.getBoundingClientRect().bottom;
              state.subjects.select(id, tr.dataset.key);
              const b = tr.getBoundingClientRect();
              const p = pane.getBoundingClientRect();
              return {before, after: b.top >= p.top - 1 && b.bottom <= p.bottom + 1};
            }""", job)
        assert r == {"before": True, "after": True}, r
    finally:
        stop_job(url, secret, job)


def scenario_map_follows_dim_and_drop(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set subject_dim_s 1")
    expect(view_line(page, "subject_dim_s = 1")).to_be_visible()
    run_cmd(page, "view set subject_drop_s 4")
    expect(view_line(page, "subject_drop_s = 4")).to_be_visible()
    run_cmd(page, "view set main map")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        expect(page.locator("#map .map-subject")).to_have_count(2, timeout=10_000)
        page.evaluate("(job) => window.zetaSubjects.select(job, '7C0001')", job)
        assert page.evaluate("window.zetaSubjects.selected().key") == "7C0001"
        expect(page.locator("#map .map-subject.dim")).to_have_count(2, timeout=3_000)
        expect(page.locator("#map .map-subject")).to_have_count(0, timeout=8_000)
        expect(page.locator("#map path.map-track")).to_have_count(0)
        assert page.evaluate("window.zetaSubjects.selected()") is None
    finally:
        stop_job(url, secret, job)


def scenario_a_dimmed_subject_relights(page, url, secret):
    page.route(TILE_HOSTS, lambda r: r.abort())
    select_source(page, url, secret, "tone")
    run_cmd(page, "view set subject_dim_s 1")
    expect(view_line(page, "subject_dim_s = 1")).to_be_visible()
    run_cmd(page, "view set main map")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        expect(page.locator("#map .map-subject.dim")).to_have_count(2, timeout=10_000)
        # Heard again: at once, not on the next tick.
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const job = {id, kind: 'fake_subjects'};
              const p = {icao: '7C0001', ground_speed_kt: 400};
              const s = state.subjects.event(job, p);
              state.map.event(job, p);
              const t = state.map.tracks.get(`${id}\\u0000${s.key}`);
              return {store: s.dim, marker: t.box.classList.contains('dim')};
            }""", job)
        assert r == {"store": False, "marker": False}, r
        # A longer dim time un-dims the rest on the next tick.
        expect(page.locator("#map .map-subject.dim")).to_have_count(2, timeout=3_000)
        run_cmd(page, "view set subject_dim_s 60")
        expect(page.locator("#map .map-subject.dim")).to_have_count(0, timeout=3_000)
        expect(page.locator("#subjects tr.subject.dim")).to_have_count(0)
    finally:
        stop_job(url, secret, job)


def scenario_mark_dim_runs_from_the_tick_only(page, url, secret):
    job = map_with_subjects(page, url, secret)
    try:
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              let calls = 0;
              const real = state.map.markDim;
              state.map.markDim = function () { calls += 1; return real.call(this); };
              const job = {id, kind: 'fake_subjects'};
              for (let i = 0; i < 5; i += 1) state.subjects.event(job, {icao: '7C0001'});
              const merged = calls;
              state.subjects.tick();
              delete state.map.markDim;
              return {merged, ticked: calls};
            }""", job)
        assert r == {"merged": 0, "ticked": 1}, r
    finally:
        stop_job(url, secret, job)


def scenario_identify_trials_do_not_flap_the_subjects_pane(page, url, secret):
    select_source(page, url, secret, "tone")
    pane = page.locator("#subjects")
    expect(pane).to_be_hidden()
    identify = start_job(url, secret, "identify", "tone", 100_000_000, 12_500)
    try:
        # A real trial of a row with a table: its identify names it, and it has no owner.
        seen = None
        for _ in range(240):
            page.evaluate("window.zetaTest.poll()")
            jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
            me = next(j for j in jobs if j["id"] == identify)
            if me["state"]["status"] != "running":
                break
            current = me["context"].get("identify", {}).get("current")
            child = next((j for j in jobs if j["id"] == current
                          and j["state"]["status"] == "running"), None)
            has_table = child is not None and page.evaluate(
                "(k) => window.zetaSubjects.rowOf(k)?.subject?.columns.length > 0", child["kind"])
            if has_table:
                seen = child
                assert child.get("owner") is None, child
                page.evaluate("window.zetaTest.poll()")
                page.wait_for_timeout(300)
                assert pane.is_hidden(), child
                assert page.evaluate("document.documentElement.dataset.strip") is None
                expect(pane_tab(page)).to_be_hidden()
                break
            page.wait_for_timeout(250)
        assert seen is not None, "no trial of a row with a table ran"
    finally:
        stop_job(url, secret, identify)
    # The store: a trial's events are not a subject; once its identify ended the job is
    # ordinary (a proved decoder), and its events are.
    r = page.evaluate(
        """async () => {
          const { state } = await import('./app.js');
          const { mountSubjects, mountSubjectsPane } = await import('./subjects.js');
          // lookup() returns at once while the page's own lookup is in flight: poll
          // until the row is known.
          for (let i = 0; !state.subjects.rows.has('fake_subjects'); i += 1) {
            if (i > 200) throw new Error('the fake_subjects row was never looked up');
            await state.subjects.lookup(['fake_subjects']);
            await new Promise((r) => setTimeout(r, 50));
          }
          const api = {op: async () => ({value: {modes: []}})};
          const store = mountSubjects(api, state.view);
          store.rows.set('fake_subjects', state.subjects.rowOf('fake_subjects'));
          const child = {id: 'c', kind: 'fake_subjects', source: 't', state: {status: 'running'}};
          const parent = {id: 'p', kind: 'identify', state: {status: 'running'},
                          context: {identify: {current: 'c'}}};
          let jobs = [parent, child];
          const pane = mountSubjectsPane(document.createElement('div'), store, state.view,
                                         {jobs: () => jobs});
          store.sync(jobs);
          const out = {};
          out.trial = [store.event(child, {icao: 'A'}), store.list('c').length, pane.eligible()];
          parent.state = {status: 'stopped'};
          store.sync(jobs);
          out.proved = [!!store.event(child, {icao: 'A'}), store.list('c').length, pane.eligible()];
          pane.destroy();
          store.destroy();
          window.zetaSubjects = state.subjects;
          return out;
        }""")
    assert r == {"trial": [None, 0, False], "proved": [True, 1, True]}, r


def scenario_two_thousand_messages_cost_little(page, url, secret):
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        expect(page.locator("#waterfall .ov-ribbon.fake_subjects")).to_have_count(1)
        page.wait_for_function(
            "(job) => window.zetaSubjects.list(job).reduce((n, s) => n + s.count, 0) === 8",
            arg=job, timeout=5_000)
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const store = window.zetaSubjects;
              const job = {id, kind: 'fake_subjects'};
              let measured = 0, applied = 0, copied = 0, drawn = 0;
              const width = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetWidth');
              Object.defineProperty(HTMLElement.prototype, 'offsetWidth', {
                configurable: true,
                get() { measured += 1; return width.get.call(this); },
              });
              const list = store.list;
              store.list = function (j) { copied += 1; return list.call(this, j); };
              const render = state.subjectsPane.render;
              state.subjectsPane.render = function () { drawn += 1; return render.call(this); };
              const hidden = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'hidden');
              const pane = document.querySelector('#subjects');
              Object.defineProperty(pane, 'hidden', {
                configurable: true,
                get() { return hidden.get.call(this); },
                set(v) { applied += 1; hidden.set.call(this, v); },
              });
              const t0 = performance.now();
              for (let i = 0; i < 2000; i += 1) {
                const p = {icao: `B${i % 50}`, altitude_ft: i, summary: `B${i % 50} ${i}`};
                store.event(job, p);
                state.ov.subjectRibbon(job, p);
              }
              const ms = performance.now() - t0;
              const out = {ms, measured, copied, applied, drawn};
              Object.defineProperty(HTMLElement.prototype, 'offsetWidth', width);
              delete store.list;
              delete state.subjectsPane.render;
              delete pane.hidden;
              const b = store.jobs.get(id).get('B49');
              out.merged = [store.jobs.get(id).size, b.count, b.state.altitude_ft];
              return out;
            }""", job)
        assert r["merged"] == [53, 40, 1999], r
        assert r["measured"] == 0, r
        assert r["copied"] == 0, r
        assert r["applied"] == 0, r
        assert r["drawn"] == 0, r
        assert r["ms"] < 1000, r
        # Settled within a second: the ribbon says so and the table has them all.
        expect(page.locator("#waterfall .ov-ribbon.fake_subjects")).to_have_text(
            "fake_subjects 53 aircraft, last B49", timeout=1_000)
        expect(page.locator("#subjects tr.subject")).to_have_count(53, timeout=1_000)
        expect(subject_row(page, "B49").locator('td[data-col="count"]')).to_have_text(
            "40", timeout=1_000)
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_a_hidden_subjects_pane_does_not_draw(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        run_cmd(page, "view set subjects off")
        expect(page.locator("#subjects")).to_be_hidden()
        # Hidden: nothing drawn. Shown: drawn at once, no timer waited for.
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const job = {id, kind: 'fake_subjects'};
              state.subjects.event(job, {icao: 'C1', altitude_ft: 1000});
              state.subjectsPane.render();
              const hidden = !!document.querySelector('#subjects tr.subject[data-key="C1"]');
              state.view.set('subjects', 'on');
              return {hidden, shown: !!document.querySelector('#subjects tr.subject[data-key="C1"]')};
            }""", job)
        assert r == {"hidden": False, "shown": True}, r
    finally:
        stop_job(url, secret, job)


def scenario_a_subjects_fix_redraws_the_map_at_most_twice_a_second(page, url, secret):
    job = map_with_subjects(page, url, secret)
    try:
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              let drawn = 0;
              const real = state.map.drawSubject;
              state.map.drawSubject = function (t) { drawn += 1; return real.call(this, t); };
              const job = {id, kind: 'fake_subjects'};
              for (let i = 0; i < 200; i += 1) {
                const p = {icao: '7C0001', position: {lat: -27 - i / 1000, lon: 153, alt_m: null}};
                state.subjects.event(job, p);
                state.map.event(job, p);
              }
              const burst = drawn;
              await new Promise((done) => setTimeout(done, 700));
              delete state.map.drawSubject;
              const t = state.map.tracks.get(`${id}\\u00007C0001`);
              const at = t.marker.getLatLng();
              return {burst, after: drawn, last: [at.lat, at.lng]};
            }""", job)
        assert r["burst"] <= 1 and r["after"] <= 3, r
        assert r["last"] == [-27.199, 153], r
    finally:
        stop_job(url, secret, job)


def scenario_a_row_holds_still_under_the_pointer(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        page.wait_for_function(
            "(job) => window.zetaSubjects.list(job).reduce((n, s) => n + s.count, 0) === 8",
            arg=job, timeout=5_000)
        # One 10 s step holds them, so the key orders them; across a step's edge a later key
        # could lead, so the step is made certain.
        page.evaluate(
            """(id) => {
              const store = window.zetaSubjects;
              const now = Date.now();
              for (const s of store.list(id)) s.last_seen = now;
              store.changed();
            }""", job)
        page.evaluate("async () => (await import('./app.js')).state.subjectsPane.render()")
        assert subject_keys(page) == ["7C0001", "7C0002", "7C0003"], subject_keys(page)
        later = """(id) => {
          const store = window.zetaSubjects;
          store.jobs.get(id).get('7C0003').last_seen = Date.now() + 30000;
          store.changed();
        }"""
        render = "async () => (await import('./app.js')).state.subjectsPane.render()"
        subject_row(page, "7C0002").hover()
        page.evaluate(later, job)
        page.evaluate(render)
        assert subject_keys(page) == ["7C0001", "7C0002", "7C0003"], subject_keys(page)
        # A press selects, on the row that was under it.
        box = subject_row(page, "7C0002").bounding_box()
        page.mouse.move(box["x"] + 5, box["y"] + box["height"] / 2)
        page.mouse.down()
        assert page.evaluate("window.zetaSubjects.selected().key") == "7C0002"
        page.mouse.up()
        # A header click sorts as asked, under the pointer, and then that order holds.
        page.locator('#subjects th[data-col="icao"]').click()
        page.locator('#subjects th[data-col="icao"]').click()
        assert subject_keys(page) == ["7C0003", "7C0002", "7C0001"], subject_keys(page)
        page.evaluate(
            """(id) => {
              window.zetaSubjects.event({id, kind: 'fake_subjects'}, {icao: '7C0000'});
            }""", job)
        page.evaluate(render)
        assert subject_keys(page) == ["7C0003", "7C0002", "7C0001", "7C0000"], subject_keys(page)
        # Off the table, the default order would have moved 7C0003 first anyway: the chosen
        # sort resumes and keeps the new row where the sort puts it.
        pane_tab(page).hover()
        page.wait_for_timeout(100)
        page.evaluate(render)
        assert subject_keys(page) == ["7C0003", "7C0002", "7C0001", "7C0000"], subject_keys(page)
        page.locator('#subjects th[data-col="icao"]').click()
        pane_tab(page).hover()
        page.evaluate(render)
        assert subject_keys(page) == ["7C0000", "7C0001", "7C0002", "7C0003"], subject_keys(page)
    finally:
        stop_job(url, secret, job)


def scenario_a_subject_key_is_trimmed_and_scalar(page, url, secret):
    select_source(page, url, secret, "tone")
    r = page.evaluate(
        """async () => {
          const { state } = await import('./app.js');
          const { metricText } = await import('./units.js');
          // lookup() returns at once while the page's own lookup is in flight: poll
          // until the row is known.
          for (let i = 0; !state.subjects.rows.has('fake_subjects'); i += 1) {
            if (i > 200) throw new Error('the fake_subjects row was never looked up');
            await state.subjects.lookup(['fake_subjects']);
            await new Promise((r) => setTimeout(r, 50));
          }
          const store = state.subjects;
          const job = {id: 'stub2', kind: 'fake_subjects'};
          const key = (icao) => store.event(job, {icao})?.key ?? null;
          const out = {
            trimmed: key('  AB12 '), number: key(7), blank: key('   '),
            object: key({a: 1}), array: key(['x']),
            text: metricText({a: 1}, null), list: metricText([1, 2], null),
          };
          store.drop('stub2', 'AB12');
          store.drop('stub2', '7');
          return out;
        }""")
    assert r == {"trimmed": "AB12", "number": "7", "blank": None, "object": None, "array": None,
                 "text": '{"a":1}', "list": "[1,2]"}, r


def scenario_selected_marker_rises(page, url, secret):
    job = map_with_subjects(page, url, secret)
    try:
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const t = state.map.tracks.get(`${id}\\u00007C0001`);
              const before = t.marker.options.zIndexOffset;
              state.subjects.select(id, '7C0001');
              const on = t.marker.options.zIndexOffset;
              state.subjects.select(id, '7C0002');
              return {before, on, off: t.marker.options.zIndexOffset};
            }""", job)
        assert r["before"] == 0 and r["on"] > 0 and r["off"] == 0, r
    finally:
        stop_job(url, secret, job)


def scenario_subject_jobs_show_with_no_subjects_yet(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_job(url, secret, "fake_subjects", "tone", 100_000_000, 40_000)
    try:
        open_subjects(page)
        sec = page.locator(f'#subjects section.subject-job[data-job="{job}"]')
        expect(sec.locator("h3")).to_have_text(re.compile(r"^fake_subjects tone, \d aircraft$"))
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const pane = state.subjectsPane;
              const real = pane.jobs;
              const all = real();
              // A job the store has nothing for: its section says 0, with no table.
              const stub = {...all.find((j) => j.id === id), id: 'stub-empty'};
              pane.jobs = () => [...all, stub];
              pane.render();
              const empty = document.querySelector(
                '#subjects section.subject-job[data-job="stub-empty"]');
              const text = empty.querySelector('h3').textContent;
              const tableHidden = empty.querySelector('table').hidden;
              // A not-watched section goes after the watched ones, though listed first.
              const watching = pane.watching;
              pane.watching = (j) => j !== id;
              pane.render();
              const order = [...document.querySelectorAll('#subjects section.subject-job')]
                .map((s) => s.dataset.job);
              pane.watching = watching;
              pane.jobs = real;
              pane.render();
              return {text, tableHidden, order};
            }""", job)
        assert r["text"] == "fake_subjects tone, 0 aircraft", r
        assert r["tableHidden"] is True, r
        assert r["order"][-1] == job and "stub-empty" in r["order"], r
    finally:
        stop_job(url, secret, job)


def scenario_a_chosen_sort_survives_its_section_going(page, url, secret):
    select_source(page, url, secret, "tone")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        open_subjects(page)
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        page.locator(f'#subjects section.subject-job[data-job="{job}"] th[data-col="count"]').click()
        r = page.evaluate(
            """async (id) => {
              const { state } = await import('./app.js');
              const pane = state.subjectsPane;
              const real = pane.jobs;
              const all = real();
              const listed = pane.sorts.get(id);
              pane.jobs = () => all.map((j) => (j.id === id
                ? {...j, state: {status: 'stopped'}} : j));
              pane.render();
              const gone = !document.querySelector(
                `#subjects section.subject-job[data-job="${id}"]`);
              pane.jobs = real;
              pane.render();
              return {listed, gone, after: pane.sorts.get(id)};
            }""", job)
        assert r["gone"] is True, r
        assert r["listed"] == {"col": "count", "dir": 1}, r
        assert r["after"] == r["listed"], r
    finally:
        stop_job(url, secret, job)


# ---- the pane tab strip (plan 19 Task 17) -----------------------------------------------

def pane_tab(page, pane="subjects"):
    return page.locator(f'#panes .pane-tab[data-pane="{pane}"]')


def open_subjects(page):
    """Select the Subjects tab once it shows (a subject job running), unless it is selected."""
    tab = pane_tab(page)
    expect(tab).to_be_visible(timeout=10_000)
    if tab.get_attribute("aria-selected") != "true":
        tab.click()
    expect(page.locator("#subjects")).to_be_visible()


def view_value(page, key):
    """The `<key> <value>` part of `view show`'s line for `key`."""
    for line in view_show(page).splitlines():
        if line.startswith(f"{key} "):
            return line.split(" (allowed")[0]
    raise AssertionError(f"no {key} in view show")


def scenario_panes_show_one_at_a_time(page, url, secret):
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    strip = page.locator('#top > nav#panes[role="tablist"][aria-label="Panes"]')
    expect(strip).to_have_count(1)
    assert page.evaluate(
        "document.querySelector('#tabs').nextElementSibling.id") == "panes"
    tab = pane_tab(page)
    pane = page.locator("#pane")
    expect(tab).to_be_hidden()
    expect(pane).to_be_hidden()
    expect(page.locator("#subjects-toggle")).to_have_count(0)
    run_cmd(page, "view set pane bits:x")
    expect(view_line(page, '"bits:x"', error=True)).to_contain_text("none, subjects, explore")
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        # Eligible: the tab shows, nothing is selected.
        expect(tab).to_be_visible(timeout=10_000)
        expect(tab).to_have_attribute("role", "tab")
        expect(tab).to_have_attribute("aria-selected", "false")
        expect(pane).to_be_hidden()
        tab.click()
        expect(pane).to_be_visible()
        expect(page.locator("#subjects")).to_be_visible()
        expect(tab).to_have_attribute("aria-selected", "true")
        assert view_value(page, "pane") == "pane subjects"
        expect(page.locator("#subjects tr.subject")).to_have_count(3, timeout=10_000)
        # Under the waterfall, the width of #main, at most 40 % of its height.
        main = page.locator("#main").bounding_box()
        wf = page.locator("#waterfall").bounding_box()
        box = pane.bounding_box()
        assert abs(box["x"] - main["x"]) <= 1 and abs(box["width"] - main["width"]) <= 1, box
        assert box["height"] <= 0.4 * main["height"] + 1, f"{box} {main}"
        assert abs(box["y"] + box["height"] - (main["y"] + main["height"])) <= 1, f"{box} {main}"
        assert wf["y"] + wf["height"] <= box["y"] + 1, f"{wf} {box}"
        assert abs(wf["width"] - main["width"]) <= 1, f"{wf} {main}"
        # A second click deselects.
        tab.click()
        expect(pane).to_be_hidden()
        expect(tab).to_have_attribute("aria-selected", "false")
        expect(tab).to_be_visible()
        assert view_value(page, "pane") == "pane none"
        wf = page.locator("#waterfall").bounding_box()
        assert abs(wf["height"] - main["height"]) <= 1, f"{wf} {main}"
        # `view set pane` and the `subjects` alias.
        run_cmd(page, "view set pane subjects")
        expect(pane).to_be_visible()
        run_cmd(page, "view set pane none")
        expect(pane).to_be_hidden()
        run_cmd(page, "view set subjects on")
        expect(pane).to_be_visible()
        assert view_value(page, "pane") == "pane subjects"
        run_cmd(page, "view set subjects off")
        expect(pane).to_be_hidden()
        assert view_value(page, "pane") == "pane none"
        run_cmd(page, "view set subjects auto")
        expect(pane).to_be_hidden()
        expect(tab).to_be_visible()
        # The map is the main area's: with the pane open it keeps the top.
        page.route(TILE_HOSTS, lambda r: r.abort())
        tab.click()
        run_cmd(page, "view set main map")
        expect(page.locator("#map")).to_be_visible()
        mp = page.locator("#map").bounding_box()
        box = pane.bounding_box()
        assert mp["y"] + mp["height"] <= box["y"] + 1, f"{mp} {box}"
        assert page.locator("#panes .pane-tab:visible").count() == 1
        run_cmd(page, "view set main rf")
        # The selection is the view's, kept over a reload.
        page.reload()
        expect(pane).to_be_visible(timeout=10_000)
        expect(pane_tab(page)).to_have_attribute("aria-selected", "true")
    finally:
        stop_job(url, secret, job)
    # Selected, the tab stays when its pane is no longer eligible.
    page.evaluate("window.zetaTest.poll()")
    expect(pane_tab(page)).to_be_visible()
    pane_tab(page).click()
    expect(pane_tab(page)).to_be_hidden()
    assert not errors, f"console errors: {errors}"


def wait_pane_parent(page, parent_id):
    """Wait until the pane host #pane is a child of the element `parent_id`."""
    page.wait_for_function(
        "(id) => document.querySelector('#pane')?.parentElement?.id === id",
        arg=parent_id, timeout=5_000)


def scenario_dock_right(page, url, secret):
    """`view set dock right` moves #pane into the side column as a third folding section above
    the Jobs pane, the waterfall keeping #main's height, whether the dock or the pane comes
    first; `bottom` moves it back and a return to `right` comes back unfolded; `auto` follows
    the theme's `layout.dock`; the narrow footprint ignores the dock."""
    select_source(page, url, secret, "tone")
    page.set_viewport_size(WIDE)
    job = start_subject_job(page, url, secret, "fake_subjects")
    try:
        run_cmd(page, "view set dock right")  # the dock first, the pane after
        open_subjects(page)
        sec = page.locator("#side section.pane.dock")
        expect(sec).to_have_count(1)
        expect(sec.locator(".pane-head")).to_contain_text("Subjects")
        wait_pane_parent(page, "pane-dock")
        order = page.evaluate(
            "() => [...document.querySelectorAll('#side > section')].map(s => s.className)")
        assert order[1].endswith("dock") and "jobs" in order[2], order
        wf, main = page.locator("#waterfall").bounding_box(), page.locator("#main").bounding_box()
        assert abs(wf["height"] - main["height"]) <= 2, (wf, main)
        # The table reads in the column: the section has room and its rows show.
        assert sec.bounding_box()["height"] >= 0.25 * main["height"], sec.bounding_box()
        expect(page.locator("#pane-dock #subjects tr.subject").first).to_be_visible(timeout=10_000)
        expect(sec.locator(".pane-head .count")).to_have_text(re.compile(r"^[1-9]\d*$"))
        head = sec.locator(".pane-head")
        head.click()
        expect(sec).to_have_attribute("data-collapsed", "")
        expect(head).to_have_attribute("aria-expanded", "false")
        head.click()
        page.wait_for_function(
            "() => !document.querySelector('#pane-dock').hasAttribute('data-collapsed')",
            timeout=5_000)
        # Folded, then bottom: #pane goes back under the main view and the section goes.
        head.click()
        expect(sec).to_have_attribute("data-collapsed", "")
        run_cmd(page, "view set dock bottom")
        wait_pane_parent(page, "main")
        expect(page.locator("#side section.pane.dock")).to_have_count(0)
        # The pane first, the dock after: the selected pane goes into the column, unfolded.
        run_cmd(page, "view set dock right")
        wait_pane_parent(page, "pane-dock")
        expect(sec).to_have_count(1)
        expect(sec).not_to_have_attribute("data-collapsed", "")
        expect(sec.locator(".pane-head")).to_have_attribute("aria-expanded", "true")
        expect(page.locator("#pane-dock #subjects")).to_be_visible()
        run_cmd(page, "view set dock bottom")
        wait_pane_parent(page, "main")
        expect(page.locator("#side section.pane.dock")).to_have_count(0)
        run_cmd(page, "view set dock auto")
        run_cmd(page, "view set theme fixture_dock_right")
        page.wait_for_function("() => document.documentElement.dataset.dock === 'right'",
                               timeout=5_000)
        wait_pane_parent(page, "pane-dock")
        run_cmd(page, "view set theme console")
        page.wait_for_function("() => document.documentElement.dataset.dock === 'bottom'",
                               timeout=5_000)
        wait_pane_parent(page, "main")
        page.set_viewport_size(NARROW)
        run_cmd(page, "view set dock right")
        page.wait_for_function("() => document.documentElement.dataset.dock === 'right'",
                               timeout=5_000)
        wait_pane_parent(page, "main")
        expect(page.locator("#side section.pane.dock")).to_have_count(0)
    finally:
        run_cmd(page, "view reset")
        page.set_viewport_size(WIDE)
        stop_job(url, secret, job)


def capture_ops(url, secret, op, **kw):
    return http_json(url, secret, "/op", {"op": op, **kw}).get("value")


def clear_captures(url, secret):
    for c in capture_ops(url, secret, "capture_list", signal=None):
        capture_ops(url, secret, "capture_delete", capture=c["id"])


# The windows two_windows writes, ms: long enough for a count, a bracket or the fsk
# fixture's frames (about 0.23 s each). A capture whose content is classified or measured
# for its keying asks for MEASURED_WINDOW_MS, the stage's default.
WINDOW_MS = 1_000
MEASURED_WINDOW_MS = 2_000


def two_windows(url, secret, job, window_ms=WINDOW_MS):
    """graph_put the capture job's burst_capture node to two whole windows of `window_ms`
    (the tone has no bursts, so the defaults would wait out their timeout)."""
    g = capture_ops(url, secret, "graph_get", job=job)
    for n in g["nodes"]:
        if n["kind"] == "burst_capture":
            n["params"].update({"bursts": 2, "whole": True, "window_ms": window_ms})
    http_json(url, secret, "/op", {"op": "graph_put", "job": job, "graph": g})


def scenario_capture_bursts_from_the_box_menu_and_explore_lists_it(page, url, secret):
    clear_captures(url, secret)
    menu = open_menu_on_tone(page, url, secret)
    page.click('#ctx .ctx-item[data-action="capture"]')
    expect(page.locator("#transcript .line.note", has_text="fitted capture to").first).to_be_visible(timeout=20000)
    # The menu's job: the running one (job_list keeps earlier scenarios' ended jobs).
    job = next(j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
               if j["kind"] == "capture" and j["state"]["status"] == "running")["id"]
    two_windows(url, secret, job)
    expect(page.locator('#panes .pane-tab[data-pane="explore"]')).to_be_visible(timeout=30000)
    expect(page.locator("#explore tr.capture")).to_have_count(2, timeout=30000)
    # Ended, so no window of it lands in a later scenario.
    wait_until(lambda: job_status(url, secret, job) != "running", 10, "the capture job ended")


def scenario_explore_what_is_this_and_delete(page, url, secret):
    # Self-seeding: its own two windows on the tone, so it passes alone.
    clear_captures(url, secret)
    job = start_job(url, secret, "capture", "tone", 100_001_000, 6_000)
    # Classified below (a carrier).
    two_windows(url, secret, job, MEASURED_WINDOW_MS)
    wait_until(lambda: len(capture_ops(url, secret, "capture_list", signal=None)) == 2,
               30, "two captures")
    login(page, url, secret)
    tab = page.locator('#panes .pane-tab[data-pane="explore"]')
    expect(tab).to_be_visible(timeout=20000)
    # Captures written before this load never take the pane by themselves (spec 5.7: only
    # a capture that appears while the page runs selects it, once).
    page.wait_for_timeout(2500)
    assert view_value(page, "pane") != "pane explore", view_value(page, "pane")
    tab.click()
    expect(page.locator("#explore tr.capture")).to_have_count(2, timeout=20000)
    row = '#explore tr.capture >> nth=0 >> '
    page.click(row + 'button[data-action="classify"]')
    expect(page.locator("#transcript .line.note", has_text="carrier").first).to_be_visible(timeout=20000)
    page.click(row + 'button[data-action="delete"]')
    expect(page.locator(row + 'button[data-action="delete"]')).to_have_text("sure?")
    page.click(row + 'button[data-action="delete"]')
    expect(page.locator("#explore tr.capture")).to_have_count(1)
    # The stored pane survives a reload with captures present.
    run_cmd(page, "view set pane none")
    page.reload()
    expect(tab).to_be_visible(timeout=20000)
    page.wait_for_timeout(2500)
    assert view_value(page, "pane") == "pane none", view_value(page, "pane")
    clear_captures(url, secret)


def scenario_explore_delete_all_removes_every_capture(page, url, secret):
    clear_captures(url, secret)
    job = start_job(url, secret, "capture", "tone", 100_001_000, 6_000)
    two_windows(url, secret, job)
    wait_until(lambda: len(capture_ops(url, secret, "capture_list", signal=None)) == 2,
               30, "two captures")
    login(page, url, secret)
    page.locator('#panes .pane-tab[data-pane="explore"]').click()
    expect(page.locator("#explore tr.capture")).to_have_count(2, timeout=20000)
    all_btn = page.locator('#explore button[data-action="delete-all"]')
    all_btn.click()
    expect(all_btn).to_have_text("sure?")
    assert len(capture_ops(url, secret, "capture_list", signal=None)) == 2, "one click deletes nothing"
    all_btn.click()
    expect(page.locator("#transcript .line.note", has_text="deleted 2 captures")).to_have_count(1)
    expect(page.locator("#explore tr.capture")).to_have_count(0)
    assert capture_ops(url, secret, "capture_list", signal=None) == [], "the store is empty"


def scenario_jobs_clear_all_stops_and_drops_the_rows(page, url, secret):
    url = llm_url()
    select_source(page, url, secret, "cb")
    jobs = lambda kind: running_kind(url, secret, kind, "cb")
    job = start_job(url, secret, "measure", "cb", 476_900_000, 10_000)
    try:
        row = page.locator(f'#jobs .job[data-job="{job}"]')
        expect(row).to_have_count(1, timeout=10000)
        btn = page.locator("#jobs-clear")
        btn.click()
        expect(btn).to_have_text("sure?")
        assert jobs("measure") == [job], "one click stops nothing"
        btn.click()
        expect(page.locator("#transcript .line.op", has_text="jobs_stop").filter(
            has_text="1 stopped, 0 failed")).to_have_count(1)
        assert not jobs("measure"), "Clear all stopped the job"
        assert jobs("spectrum"), "the page's elastic spectrum keeps running"
        # Its finished row is dropped at once, not after finished_rows_s.
        expect(row).to_have_count(0)
        expect(btn).to_have_text("Clear all")
    finally:
        for j in jobs("measure"):
            stop_job(url, secret, j)


def scenario_a_capture_draws_a_bracket_while_on_screen(page, url, secret):
    clear_captures(url, secret)
    # The page first, so its columns cover the capture's time.
    show_tone_waterfall(page, url, secret)
    job = start_job(url, secret, "capture", "tone", 100_001_000, 6_000)
    two_windows(url, secret, job)
    wait_until(lambda: len(capture_ops(url, secret, "capture_list", signal=None)) == 2,
               30, "two captures")
    span = page.locator("#waterfall .ov-span")
    expect(span.first).to_be_visible(timeout=20000)
    ids = {c["id"][-6:] for c in capture_ops(url, secret, "capture_list", signal=None)}
    assert span.first.text_content() in ids, (span.first.text_content(), ids)
    box = span.first.bounding_box()
    canvas = page.locator("#waterfall canvas.wf-canvas").bounding_box()
    assert box["width"] >= 2 and box["height"] >= 2, box
    assert box["x"] + box["width"] <= canvas["x"] + canvas["width"] + 1, (box, canvas)
    clear_captures(url, secret)
    expect(page.locator("#waterfall .ov-span")).to_have_count(0, timeout=20000)


def scenario_too_old_classify_reissues_on_the_latest_span(page, url, secret):
    short = os.environ.get("ZETA_UI_URL_SHORTRING")
    assert short, "ZETA_UI_URL_SHORTRING (a node with a short ring) is not set"
    fresh_spectrum_jobs(short, secret)
    page.set_viewport_size(WIDE)
    login(page, short, secret)
    page.click('.src-tab[data-id="tone"]')
    expect(page.locator('.src-tab[data-id="tone"]')).to_have_attribute("aria-selected", "true")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    # The ring holds a few seconds (at most 4); the page keeps more columns than that.
    wait_span(page, 6.5, timeout=30_000)
    drag_menu(page)
    footer = page.locator("#ctx-footer").text_content()
    m = re.search(r" ring (\d+\.\d) s", footer)
    assert m, f"no ring depth in the footer {footer!r}"
    assert 1.0 <= float(m.group(1)) <= 4.0, footer
    # The box starts well before the ring's earliest sample.
    span = re.search(r"start_ns=(\d+) end_ns=(\d+)", footer)
    span_s = (int(span.group(2)) - int(span.group(1))) / 1e9
    assert span_s >= float(m.group(1)) + 2.0, f"box of {span_s:.1f} s on a ring of {footer!r}"
    page.click('#ctx .ctx-item[data-action="classify"]')
    expect(page.locator("#transcript .line.note", has_text="buffer moved on; classified the last")
           .first).to_be_visible(timeout=20000)
    expect(page.locator("#transcript .line.note", has_text="tried ").first).to_be_visible(timeout=20000)


def stop_bits_jobs(url, secret):
    for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]:
        if j["kind"] == "bits" and j["state"]["status"] == "running":
            stop_job(url, secret, j["id"])


def scenario_work_the_bits_shows_a_stream_and_a_sync_hit(page, url, secret):
    # Self-seeding: two windows of the fsk fixture's framed 2-FSK (sync 0x2dd4, 1120 bits).
    clear_captures(url, secret)
    stop_bits_jobs(url, secret)
    job = start_job(url, secret, "capture", "fsk", 433_920_000, 12_000)
    two_windows(url, secret, job)
    wait_until(lambda: len(capture_ops(url, secret, "capture_list", signal=None)) == 2,
               30, "two captures")
    # `measure` finds no keying line on this whole-window capture (its features carry no
    # symbol rate), so the row's rate is typed; a measured one is prefilled.
    newest = capture_ops(url, secret, "capture_list", signal=None)[0]
    login(page, url, secret)
    page.locator('#panes .pane-tab[data-pane="explore"]').click()
    expect(page.locator("#explore tr.capture")).to_have_count(2, timeout=20000)
    row = '#explore tr.capture >> nth=0 >> '
    rate = newest["features"].get("symbol_rate_baud")
    if rate:
        expect(page.locator(row + "input.bits-rate")).to_have_value(str(round(rate)))
    else:
        page.fill(row + "input.bits-rate", "4800")
    page.click(row + 'button[data-action="bits"]')
    expect(page.locator("#transcript .line.note", has_text="started bits").first).to_be_visible(timeout=20000)
    expect(page.locator('#panes .pane-tab[data-pane="bits"]')).to_have_attribute(
        "aria-selected", "true", timeout=20000)
    expect(page.locator("#bits .bits-row").first).to_be_visible(timeout=20000)
    # The byte under the pointer reads in binary.
    page.locator("#bits .bits-row .byte").first.hover()
    expect(page.locator("#bits-binary")).to_have_text(re.compile(r"^[01]{8}"), timeout=5000)
    expect(page.locator("#bits-clock")).to_contain_text("Bd", timeout=20000)
    expect(page.locator("#bits-hyps li", has_text="2dd4").first).to_be_visible(timeout=40000)
    page.click('#bits-hyps li:has-text("2dd4") button[data-action="try"]')
    expect(page.locator("#bits .sync").first).to_be_visible(timeout=30000)
    # The correlator sits before bit_view: the hypotheses keep coming after the relaunch.
    expect(page.locator("#bits-hyps")).to_have_attribute("data-events", re.compile(r"^[1-9]"),
                                                         timeout=20000)
    expect(page.locator("#bits-hyps li", has_text="2dd4").first).to_be_visible()
    graph = capture_ops(url, secret, "graph_get", job=[
        j["id"] for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
        if j["kind"] == "bits" and j["state"]["status"] == "running"][0])
    edges = {(e["from"], e["to"]) for e in graph["edges"]}
    assert ("slicer", "correlate") in edges and ("correlate", "bit_view") in edges, edges
    # The job's row offers the pane.
    expect(page.locator("#jobs .job button.bits").first).to_be_visible(timeout=10000)
    stop_bits_jobs(url, secret)
    clear_captures(url, secret)


# The fsk fixture's framing (`crates/zeta/tests/ui.rs`'s `fsk_fixture`): the sync FSK_SYNC
# 0x2dd4, then FSK_BODY_BYTES (136) bytes of body and the body's 2-byte CRC-16-CCITT, so
# (136 + 2) × 8 = 1104 bits after the sync.
FSK_FRAMING = {"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 1104},
               "crc": "crc16-ccitt"}
# A framing with a CRC for the `pocsag` capture: its sync codeword, then one batch (16
# codewords). The node checks a hand framing's shape only (the `frame_decode` params), not
# against the capture, and POCSAG's BCH is not a `frame_decode` CRC: CRC-16-CCITT stands in.
POCSAG_FRAMING = {"sync": {"hex": "7cd215d8", "bits": 32}, "length": {"fixed_bits": 512},
                  "crc": "crc16-ccitt"}


def propose(url, secret, capture, framing=None, name=None):
    """`signature_propose` by hand from `capture`; the record."""
    return capture_ops(url, secret, "signature_propose", capture=capture, name=name,
                       framing=framing)


def clear_signatures(url, secret):
    """Reject every proposed signature (an accepted one stays: there is no unaccept)."""
    for s in capture_ops(url, secret, "signature_list", state="proposed"):
        capture_ops(url, secret, "signature_reject", id=s["id"])


def two_captures_of(url, secret, source, center_hz, bandwidth_hz, window_ms=WINDOW_MS):
    """A capture job's two whole windows (of `window_ms`) on `source`; its captures, newest
    first."""
    job = start_job(url, secret, "capture", source, center_hz, bandwidth_hz)
    two_windows(url, secret, job, window_ms)
    mine = lambda: [c for c in capture_ops(url, secret, "capture_list", signal=None)
                    if c["job"] == job]
    wait_until(lambda: len(mine()) == 2, 30, f"two captures on {source}")
    return mine()


# Whether the input `sel` matched when `window.__field` was taken is gone from the page
# (the pane's poll redrew it) and the one there now has the focus.
FIELD_REDRAWN_JS = """(sel) => {
  const now = document.querySelector(sel);
  return !!now && !document.contains(window.__field) && document.activeElement === now;
}"""


def type_across_a_poll(page, sel, keys):
    """Type `keys` into `sel` (already focused) slowly enough that the pane's poll redraws
    the row at least once meanwhile (checked)."""
    page.evaluate("(sel) => { window.__field = document.querySelector(sel); }", sel)
    page.locator(sel).press_sequentially(keys, delay=EXPLORE_TYPE_DELAY_MS)
    page.wait_for_function(FIELD_REDRAWN_JS, arg=sel, timeout=5000)


# The delay between keystrokes: sixteen of them (2.4 s) outlast EXPLORE_POLL_MS (2 s).
EXPLORE_TYPE_DELAY_MS = 150


def scenario_typing_survives_the_explore_poll(page, url, secret):
    clear_captures(url, secret)
    clear_signatures(url, secret)
    capture = two_captures_of(url, secret, "fsk", 433_920_000, 12_000)[0]
    sig = propose(url, secret, capture["id"])
    login(page, url, secret)
    page.locator('#panes .pane-tab[data-pane="explore"]').click()
    name = f'#explore tr.signature[data-id="{sig["id"]}"] input.sig-name'
    expect(page.locator(name)).to_have_count(1, timeout=20000)
    # A name typed into the middle: the focus and the caret come back after each redraw.
    page.fill(name, "abcdef")
    page.press(name, "End")
    for _ in range(3):
        page.press(name, "ArrowLeft")
    type_across_a_poll(page, name, "0123456789012345")
    expect(page.locator(name)).to_have_value("abc0123456789012345def")
    # The rate field keeps the focus too.
    rate = f'#explore tr.capture[data-id="{capture["id"]}"] input.bits-rate'
    page.fill(rate, "")
    page.focus(rate)
    type_across_a_poll(page, rate, "1234567890123456")
    expect(page.locator(rate)).to_have_value("1234567890123456")
    clear_signatures(url, secret)
    clear_captures(url, secret)


# Defined after every scenario that counts captures: its accepts pin their captures, which
# `clear_captures` cannot delete, and `scenarios()` runs them in definition order.
def scenario_proposal_accepted_from_the_page(page, url, secret):
    clear_captures(url, secret)
    clear_signatures(url, secret)
    newest = two_captures_of(url, secret, "fsk", 433_920_000, 12_000)[0]
    sig = propose(url, secret, newest["id"], framing=FSK_FRAMING)
    other = propose(url, secret, newest["id"])
    login(page, url, secret)
    page.locator('#panes .pane-tab[data-pane="explore"]').click()
    # The capture job ended on its own, so the node may add its own features-only proposal
    # at any moment: the rows counted are the ones proposed here, by id.
    rows = page.locator(f'#explore tr.signature[data-id="{sig["id"]}"], '
                        f'#explore tr.signature[data-id="{other["id"]}"]')
    expect(rows).to_have_count(2, timeout=20000)
    row = f'#explore tr.signature[data-id="{sig["id"]}"] >> '
    expect(page.locator(row + "td.confidence")).to_have_text("0.60")
    expect(page.locator(row + "td.state")).to_have_text("proposed")
    expect(page.locator(row + "td.evidence")).to_contain_text("framing given by hand")
    page.fill(row + "input.sig-name", "fsk test link")
    page.click(row + 'button[data-action="accept"]')
    expect(page.locator("#transcript .line.note",
                        has_text=f"accepted fsk test link ({sig['id'][-6:]})")).to_have_count(1)
    # `measure` reads no symbol rate off direct 2-FSK today, so this one writes no learned
    # row (`accepted`); one that read a rate would add ` · learned <row>`.
    expect(page.locator(row + "td.state")).to_have_text(re.compile(r"^accepted"))
    expect(page.locator(row + "input.sig-name")).to_have_attribute("readonly", "")
    expect(page.locator(row + 'button[data-action="accept"]')).to_have_count(0)
    accepted = capture_ops(url, secret, "signature_show", id=sig["id"])
    assert accepted["state"] == "accepted" and accepted["name"] == "fsk test link", accepted
    assert all(c["pinned"] for c in capture_ops(url, secret, "capture_list", signal=None)), \
        "captures pinned"
    other_row = f'#explore tr.signature[data-id="{other["id"]}"] >> '
    page.click(other_row + 'button[data-action="reject"]')
    expect(page.locator(other_row + 'button[data-action="reject"]')).to_have_text("sure?")
    page.click(other_row + 'button[data-action="reject"]')
    expect(rows).to_have_count(1, timeout=20000)
    assert capture_ops(url, secret, "signature_show", id=other["id"])["state"] == "rejected"
    # A framing with a CRC and a measured symbol rate (the pocsag capture's keying line,
    # 1200 Bd) writes `learned_<id>.json` to the node's modes directory on Accept. Its
    # classify proves pocsag_decode, so the node proposes nothing of its own here.
    capture = two_captures_of(url, secret, "pocsag", 148_637_500, 12_500,
                              MEASURED_WINDOW_MS)[0]
    assert capture["features"].get("symbol_rate_baud"), capture["features"]
    framed = propose(url, secret, capture["id"], framing=POCSAG_FRAMING)
    learned = f'#explore tr.signature[data-id="{framed["id"]}"] >> '
    expect(page.locator(learned + "td.state")).to_have_text("proposed", timeout=20000)
    page.click(learned + 'button[data-action="accept"]')
    expect(page.locator("#transcript .line.note",
                        has_text=f"accepted {framed['name']} ({framed['id'][-6:]})")
           ).to_have_count(1)
    row_id = f"learned_{framed['id'].lower()}"
    expect(page.locator(learned + "td.state")).to_contain_text("learned", timeout=20000)
    expect(page.locator(learned + "td.state")).to_have_text(f"accepted · learned {row_id}")
    rec = capture_ops(url, secret, "signature_show", id=framed["id"])
    assert rec["decoder"] == row_id and rec["name"] == framed["name"], rec
    # The pinned captures stay in the store for the rest of the run (unpin is not an op).


def scenario_allow_signatures_switch(page, url, secret):
    login(page, url, secret)
    run_cmd(page, "allow signatures on")
    expect(page.locator("#transcript .line.note:not(.error)",
                        has_text="allow signatures: on")).to_have_count(1)
    run_cmd(page, "allow signatures off")
    expect(page.locator("#transcript .line.note:not(.error)",
                        has_text="allow signatures: off")).to_have_count(1)


# ---- video (plan 28.2) -----------------------------------------------------------------
# The `video` and `video2` sources each loop one synthesised PAL recording (ui.rs
# `video_fixture`, 8 MHz at 100 MHz); a video_decode needs a couple of seconds to lock, and the
# page polls the jobs every 2 s, so every check below polls for its end state.

VIDEO_CENTER_HZ = 100_000_000
VIDEO_BAND_HZ = 7_680_000

# Whether the centre of the thumbnail canvas `sel` has a bar (a pixel brighter than 20).
THUMB_PIXEL_JS = """(sel) => {
  const c = document.querySelector(sel);
  if (!c || !c.width || !c.height) return false;
  const d = c.getContext('2d').getImageData(c.width >> 1, c.height >> 1, 1, 1).data;
  return Math.max(d[0], d[1], d[2]) > 20;
}"""


def start_video(url, secret, source):
    return start_job(url, secret, "video_decode", source, VIDEO_CENTER_HZ, VIDEO_BAND_HZ)


def stop_videos(url, secret):
    for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]:
        if j["kind"] == "video_decode":
            stop_job(url, secret, j["id"])


def open_video_pane(page, job):
    """Click the tag's thumbnail (once it locks) and wait for the full view."""
    thumb = page.locator(f'#waterfall .job-tag[data-job="{job}"] canvas.video-thumb')
    page.evaluate("window.zetaTest.poll()")
    expect(thumb).to_be_visible(timeout=10_000)
    expect(thumb).not_to_have_class(re.compile("nolock"), timeout=10_000)
    thumb.click()
    expect(page.locator("html")).to_have_attribute("data-full", "video")


def scenario_video_thumbnail_rides_the_tag(page, url, secret):
    errors = page_errors(page)
    select_source(page, url, secret, "video")
    stop_videos(url, secret)
    job = start_video(url, secret, "video")
    try:
        page.evaluate("window.zetaTest.poll()")
        tag = page.locator(f'#waterfall .job-tag[data-job="{job}"]')
        thumb = tag.locator("canvas.video-thumb")
        expect(thumb).to_be_visible(timeout=10_000)
        expect(thumb).not_to_have_class(re.compile("nolock"), timeout=10_000)
        expect(tag.locator(".snippet")).to_contain_text("locked", timeout=10_000)
        page.wait_for_function(THUMB_PIXEL_JS, arg=f'.job-tag[data-job="{job}"] canvas.video-thumb',
                               timeout=10_000)
        assert not errors, f"page errors {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_video_pane_fills_main_and_switches_profile(page, url, secret):
    errors = page_errors(page)
    select_source(page, url, secret, "video")
    stop_videos(url, secret)
    job = start_video(url, secret, "video")
    try:
        open_video_pane(page, job)
        root = page.locator("html")
        expect(root).to_have_attribute("data-strip", "video")
        pane, main = page.locator("#pane").bounding_box(), page.locator("#main").bounding_box()
        for k in ("x", "y", "width", "height"):
            assert abs(pane[k] - main[k]) <= 1, f"#pane {pane} vs #main {main}"
        assert page.evaluate("getComputedStyle(document.getElementById('waterfall')).visibility") \
            == "hidden"
        status = page.locator("#video .video-status .text")
        expect(status).to_contain_text("locked", timeout=10_000)
        expect(status).to_contain_text("smooth", timeout=10_000)
        run_cmd(page, "view set video sharp")
        expect(status).to_contain_text("sharp", timeout=10_000)
        assert view_value(page, "video") == "video sharp"
        assert not errors, f"page errors {errors}"
    finally:
        try:
            stop_job(url, secret, job)
        finally:
            run_cmd(page, "view set video smooth")


def scenario_video_pane_returns_the_waterfall_when_left(page, url, secret):
    errors = page_errors(page)
    select_source(page, url, secret, "video")
    stop_videos(url, secret)
    job = start_video(url, secret, "video")
    try:
        open_video_pane(page, job)
        # Shown: the main picture's socket (the tag's is paused).
        page.wait_for_function("() => window.zetaVideo.sockets_main === 1", timeout=10_000)
        pane_tab(page, "video").click()
        expect(page.locator("html")).not_to_have_attribute("data-full", "video")
        expect(page.locator("#waterfall")).to_be_visible()
        # Left: the main socket closes and the tag's thumbnail is the one socket left.
        page.wait_for_function(
            "() => window.zetaVideo.sockets_main === 0 && window.zetaVideo.sockets_thumb === 1",
            timeout=10_000)
        assert page.evaluate("getComputedStyle(document.getElementById('waterfall')).visibility") \
            != "hidden"
        assert not errors, f"page errors {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_video_feed_stopping_falls_back_to_the_next(page, url, secret):
    errors = page_errors(page)
    select_source(page, url, secret, "video")
    stop_videos(url, secret)
    first = start_video(url, secret, "video")
    second = start_video(url, secret, "video2")
    try:
        open_video_pane(page, first)
        expect(page.locator("#video .video-strip canvas")).to_have_count(2, timeout=10_000)
        main = page.locator("#video canvas.video-main")
        expect(main).to_have_attribute("data-job", first)
        # Before each stop, wait for a picture the selected feed's own main socket has drawn:
        # a stop before that socket's handshake makes the browser log a 404 the page cannot
        # suppress (no operator is that quick). Each feed has its own main canvas, blank until
        # its first frame.
        drawn = lambda job: page.wait_for_function(  # noqa: E731
            THUMB_PIXEL_JS, arg=f'#video canvas.video-main[data-job="{job}"]', timeout=10_000)
        drawn(first)
        stop_job(url, secret, first)
        page.evaluate("window.zetaTest.poll()")
        status = page.locator("#video .video-status .text")
        expect(status).to_contain_text("video2 ·", timeout=8_000)
        expect(main).to_have_attribute("data-job", second, timeout=8_000)
        drawn(second)
        stop_job(url, secret, second)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator("html")).not_to_have_attribute("data-strip", "video", timeout=8_000)
        expect(pane_tab(page, "video")).to_be_hidden(timeout=8_000)
        assert not errors, f"page errors {errors}"
    finally:
        stop_videos(url, secret)


def scenario_decode_video_greys_when_the_box_is_too_wide(page, url, secret):
    # The waterfall shows at most the capture's own width, so a real box is never wider than
    # the file's cap (asserted: its rate, 8 MHz); the page is told a 4 MHz cap instead.
    cap = 4_000_000

    def route(r):
        if '"source_list"' not in (r.request.post_data or ""):
            return r.continue_()
        resp = r.fetch()
        data = resp.json()
        for src in data.get("value") or []:
            src.setdefault("capabilities", {})["max_bandwidth_hz"] = {"asserted": cap}
        return r.fulfill(response=resp, body=json.dumps(data))

    def box_hz():
        footer = page.locator("#ctx-footer").text_content()
        return int(re.search(r"bandwidth_hz=(\d+)", footer).group(1))

    page.route("**/op", route)
    try:
        select_source(page, url, secret, "video")
        wait_span(page, MENU_SPAN_S)
        item = page.locator('#ctx .ctx-item[data-action="video"]')
        drag_menu(page, 0.02, 0.98)
        assert box_hz() > cap, f"wide box {box_hz()} Hz"
        expect(item).to_have_attribute("aria-disabled", "true", timeout=10_000)
        title = item.get_attribute("title")
        assert title.startswith("insufficient bandwidth") and "(measured at open)" not in title, \
            title
        page.keyboard.press("Escape")
        drag_menu(page, 0.40, 0.60)
        assert box_hz() < cap, f"narrow box {box_hz()} Hz"
        expect(item).not_to_have_attribute("aria-disabled", "true")
        assert item.get_attribute("title") is None
        page.keyboard.press("Escape")
    finally:
        page.unroute("**/op", route)


def scenario_a_measuring_source_shows_its_notice(page, url, secret):
    # The e2e nodes run file sources only (a worker needs a soapy build), so the page is
    # told that `tone`'s worker is measuring its link while `measuring` is on.
    measuring = {"on": True}
    worker = {"pid": 1, "restarts": 1, "since_ns": None, "transport": None,
              "transport_dropped": 0,
              "starting": {"what": "measuring", "detail": "12-bit at 30720000 Hz",
                           "step": 2, "of": 6, "eta_ms": 30000}}
    notice = "measuring the link: 12-bit at 30720000 Hz, 2 of 6, up to 30 s"

    def route(r):
        if '"source_list"' not in (r.request.post_data or "") or not measuring["on"]:
            return r.continue_()
        resp = r.fetch()
        data = resp.json()
        for src in data.get("value") or []:
            if src.get("id") == "tone":
                src["worker"] = worker
        return r.fulfill(response=resp, body=json.dumps(data))

    tab = '.src-tab[data-id="tone"]'
    note = page.locator(f"{tab} .starting")
    themes = sorted(os.listdir(os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))), "themes")))
    page.route("**/op", route)
    try:
        select_source(page, url, secret, "tone")
        page.evaluate("window.zetaTest.poll()")
        expect(note).to_be_visible(timeout=10_000)
        expect(note).to_have_text(notice)
        assert notice in page.get_attribute(tab, "title"), page.get_attribute(tab, "title")
        # Not a fault: the tab is not marked as one.
        assert "fault" not in (page.get_attribute(tab, "class") or "").split()
        # Shown in every theme, on the selected tab and on another's.
        for theme in themes:
            run_cmd(page, f"view set theme {theme}")
            expect(page.locator("html")).to_have_attribute("data-theme", theme)
            for selected in ("tone", "aprs"):
                page.click(f'.src-tab[data-id="{selected}"]')
                expect(page.locator(f'.src-tab[data-id="{selected}"]')).to_have_attribute(
                    "aria-selected", "true")
                expect(note).to_be_visible()
                assert page.evaluate(
                    "(sel) => document.querySelector(sel).offsetParent !== null",
                    f"{tab} .starting"), (theme, selected)
        measuring["on"] = False
        page.evaluate("window.zetaTest.poll()")
        expect(note).to_have_count(0, timeout=10_000)
        assert notice not in page.get_attribute(tab, "title")
    finally:
        page.unroute("**/op", route)
        run_cmd(page, "view reset")


def scenario_run_link_diagnostics(page, url, secret):
    # As in the scenario above, the e2e nodes run file sources only: the page is told that
    # `tone` has a worker (its latest hello measured one 16-bit depth at 20.48 Msps), and
    # `source_measure` is answered here as zeta-ws answers an `ok` and a `conflict`.
    since = 1_000_000
    depth = {"bits": 16, "lossless": True, "max_bandwidth_hz": {"measured": 20_480_000}}
    st = {
        "worker": {"pid": 1, "restarts": 1, "since_ns": since, "transport": None,
                   "transport_dropped": 0, "starting": None, "attachment": "usb:2-1.4@5000",
                   "link_measure": {"max_bandwidth_hz": {"measured": 20_480_000},
                              "depths": [depth], "at_speed": "5000"}},
        "answer": None,
        "sent": [],
    }

    def route(r):
        body = r.request.post_data or ""
        if '"source_measure"' in body:
            st["sent"].append(json.loads(body))
            if st["answer"] is None:
                return r.fulfill(status=200, content_type="application/json",
                                 body=json.dumps({"result": "ok"}))
            return r.fulfill(status=422, content_type="application/json",
                             body=json.dumps(st["answer"]))
        if '"source_list"' not in body:
            return r.continue_()
        resp = r.fetch()
        data = resp.json()
        for src in data.get("value") or []:
            if src.get("id") == "tone":
                src["worker"] = st["worker"]
        return r.fulfill(response=resp, body=json.dumps(data))

    def sent(n):
        # Route handlers run while Playwright waits, so wait in the page.
        for _ in range(100):
            if len(st["sent"]) >= n:
                return st["sent"][n - 1]
            page.wait_for_timeout(100)
        raise AssertionError(f"source_measure #{n} never sent: {st['sent']}")

    tab = '.src-tab[data-id="tone"]'
    button = page.locator("#src-menu .link-diag")
    job = None
    page.route("**/op", route)
    try:
        select_source(page, url, secret, "tone")
        # The entry shows under the tab of a source with a worker (once a poll says so,
        # the pointer already on the tab), and only there.
        page.hover(tab)
        page.evaluate("window.zetaTest.poll()")
        expect(button).to_be_visible(timeout=5_000)
        expect(button).to_have_text("Run link diagnostics")
        expect(button).to_have_attribute(
            "title", "measures the rates this radio's link sustains: about a minute, "
            "the host will be busy")
        page.hover('.src-tab[data-id="aprs"]')
        expect(button).to_be_hidden(timeout=5_000)
        page.hover(tab)
        expect(button).to_be_visible(timeout=5_000)
        button.click()
        op = sent(1)
        assert op == {"op": "source_measure", "source": "tone", "replace": []}, op
        expect(page.locator("#transcript")).to_contain_text(
            "measuring tone again: about a minute, the host will be busy", timeout=5_000)
        # The respawn measures: the notice shows, and no result yet.
        st["worker"] = dict(st["worker"], pid=None, since_ns=None, link_measure=None,
                            starting={"what": "measuring", "detail": "as asked: 16-bit at "
                                      "30720000 Hz", "step": 1, "of": 6, "eta_ms": 36000})
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f"{tab} .starting")).to_be_visible(timeout=10_000)
        result = "link diagnostics tone: 16-bit 20.48 → 30.72 Msps; in force now, the record is set once a run"
        expect(page.locator("#transcript")).not_to_contain_text("link diagnostics tone")
        # Its hello: a later since_ns, no start, the new figure.
        st["worker"] = dict(st["worker"], pid=2, since_ns=since + 1, starting=None,
                            link_measure={"max_bandwidth_hz": {"measured": 30_720_000},
                                    "depths": [dict(depth, max_bandwidth_hz={
                                        "measured": 30_720_000})],
                                    "at_speed": "5000"})
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator("#transcript .line", has_text=result)).to_have_count(
            1, timeout=10_000)
        expect(page.locator(f"{tab} .starting")).to_have_count(0)
        # A conflict offers the usual stop and retry, the retry naming the holder.
        job = start_job(url, secret, "passthrough", "tone", 100_000_000, 10_000)
        st["answer"] = {"error": "conflict", "source": "tone", "holders": [job],
                        "current": {"center_hz": 100_000_000, "bandwidth_hz": 48_000},
                        "options": []}
        page.evaluate("window.zetaTest.poll()")
        page.hover(tab)
        expect(button).to_be_visible(timeout=5_000)
        button.click()
        sent(2)
        line = page.locator("#transcript .line.error",
                            has_text=re.compile(rf"conflict: source tone held by passthrough {job}"))
        expect(line).to_have_count(1, timeout=10_000)
        retry = line.locator("button", has_text=f"stop passthrough {job[-6:]} and retry")
        expect(retry).to_have_count(1)
        st["answer"] = None
        retry.click()
        op = sent(3)
        assert op == {"op": "source_measure", "source": "tone", "replace": [job]}, op
    finally:
        page.unroute("**/op", route)
        if job:
            stop_job(url, secret, job)


def scenarios():
    return [(name[len("scenario_"):], fn) for name, fn in globals().items()
            if name.startswith("scenario_") and callable(fn)]


# With `ZETA_UI_SHARD=<i>/<n>`, ui.rs runs the scenarios in n shards at once, each against
# its own set of nodes: shard i runs the i-th contiguous block of `scenarios()` (definition
# order), the blocks starting at these scenarios. A block's first scenario runs on a fresh
# node, so it must pass alone (as with `ZETA_UI_ONLY`). Rebalance from the `PASS <name>
# (<s> s)` times so each block takes about the same time.
SHARD_STARTS = {
    1: [],
    2: ["listen_to_all_starts_one_mix_and_plays"],
    3: ["fm_demod_does_not_poll_streams", "subjects_distance_and_sort"],
}


def shard(todo, spec):
    """The block of `todo` that shard `spec` (`<i>/<n>`, 0-based i) runs (SHARD_STARTS)."""
    i, n = (int(x) for x in spec.split("/"))
    names = [name for name, _ in todo]
    starts = SHARD_STARTS.get(n)
    assert starts is not None and 0 <= i < n, \
        f"no shard {spec}: SHARD_STARTS has {list(SHARD_STARTS)}"
    cuts = [0] + [names.index(s) for s in starts] + [len(todo)]
    assert cuts == sorted(cuts), f"SHARD_STARTS[{n}] out of definition order: {starts}"
    return todo[cuts[i]:cuts[i + 1]]


# Collects the page's Content-Security-Policy violations (checked after every scenario).
CSP_WATCH_JS = """
window.__cspViolations = [];
document.addEventListener('securitypolicyviolation', (e) => {
  window.__cspViolations.push(`${e.violatedDirective} ${e.blockedURI}`);
});
"""


class LineTimes:
    """With `ZETA_UI_PROFILE=<file>`: the wall time each line of this file took, per scenario.

    A line's time runs from its `line` event to the next event in its frame, so a helper's
    line counts inside the line that called it too. `report` appends the slowest lines."""

    def __init__(self, path):
        self.out = open(path, "a")
        self.acc, self.at = {}, {}

    def _local(self, frame, event, _arg):
        now = time.monotonic()
        if event in ("line", "return"):
            prev = self.at.pop(id(frame), None)
            if prev is not None:
                key = (frame.f_code.co_name, prev[0])
                self.acc[key] = self.acc.get(key, 0.0) + now - prev[1]
            if event == "line":
                self.at[id(frame)] = (frame.f_lineno, now)
        return self._local

    def _global(self, frame, _event, _arg):
        return self._local if frame.f_code.co_filename == __file__ else None

    def start(self):
        self.acc.clear()
        self.at.clear()
        sys.settrace(self._global)

    def report(self, name, took_s):
        sys.settrace(None)
        self.out.write(f"== {name} {took_s:.1f} s\n")
        for (fn, line), dt in sorted(self.acc.items(), key=lambda kv: -kv[1])[:10]:
            if dt >= 0.25:
                self.out.write(f"  {dt:6.2f} {fn}:{line}\n")
        self.out.flush()


def main(argv):
    if len(argv) < 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    url, secret = argv[1].rstrip("/"), argv[2]
    todo = scenarios()
    if len(argv) > 3:
        want = {w.strip().removeprefix("scenario_") for w in argv[3].split(",")}
        todo = [(n, f) for n, f in todo if n in want]
        if len(todo) != len(want):
            print(f"no scenario {argv[3]!r}; known: {[n for n, _ in scenarios()]}",
                  file=sys.stderr)
            return 2
    if os.environ.get("ZETA_UI_SHARD"):
        todo = shard(todo, os.environ["ZETA_UI_SHARD"])
    profile = LineTimes(os.environ["ZETA_UI_PROFILE"]) if os.environ.get("ZETA_UI_PROFILE") \
        else None
    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=True, args=CHROMIUM_ARGS)
        try:
            for name, fn in todo:
                ctx = browser.new_context(viewport=WIDE)
                ctx.set_default_timeout(TIMEOUT_MS)
                ctx.add_init_script(CSP_WATCH_JS)
                ctx.route(TILE_HOSTS, lambda r: r.abort())
                page = ctx.new_page()
                console = []
                page.on("console", lambda m: console.append(f"{m.type}: {m.text}")
                        if m.type == "error" else None)
                page.on("pageerror", lambda e: console.append(f"pageerror: {e}"))
                started = time.monotonic()
                try:
                    if profile:
                        profile.start()
                    try:
                        fn(page, url, secret)
                    finally:
                        if profile:
                            profile.report(name, time.monotonic() - started)
                    if not page.is_closed() and page.url.startswith(url):
                        seen = page.evaluate("window.__cspViolations || []")
                        assert not seen, f"Content-Security-Policy violations: {seen}"
                except Exception as e:  # noqa: BLE001 - report any failure with its scenario
                    print(f"FAIL {name}: {type(e).__name__}: {e}")
                    for line in console:
                        print(f"  console {line}")
                    return 1
                finally:
                    ctx.close()
                print(f"PASS {name} ({time.monotonic() - started:.1f} s)")
        finally:
            browser.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

