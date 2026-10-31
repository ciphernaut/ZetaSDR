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
daemon (with the `speech` source) in ZETA_UI_URL_NOSTT (for the "no daemon" scenarios).
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
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (9 sources)")
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
    # Widening re-evaluates the footprint.
    page.set_viewport_size(WIDE)
    page.wait_for_function("document.documentElement.dataset.footprint == 'wide'")
    expect(page.locator("#drawer-toggle")).to_be_hidden()


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
    assert health["ws_connections"] <= 2, f"duplicate sockets: {health}"
    page.fill("#cmd", '{"op":"source_list"}')
    page.press("#cmd", "Enter")
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (9 sources)")
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
    for source in ("tone", "aprs", "wfm", "speech", "retune", "scanner", "cb", "quiet"):
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
        expect(ribbon.first).to_contain_text("VK3ABC > APRS: >hi")
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
        expect(ribbon.first).to_contain_text("1234567: HELLO ZETA")
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
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (9 sources)")
    expect(page.locator("#side")).to_be_visible()


def scenario_transcript_hides_polls(page, url, secret):
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    page.wait_for_timeout(5_000)
    polls = page.locator("#transcript .line.op", has_text="job_list")
    expect(polls).to_have_count(0)
    page.fill("#cmd", '{"op":"job_list"}')
    page.press("#cmd", "Enter")
    typed = page.locator("#transcript .line.op", has_text="job_list → ok")
    expect(typed).to_have_count(1)
    # Later polls stay hidden and do not fold into the typed line (the typed op's own "ui"
    # record is hidden too: its one line comes from the /op reply).
    page.wait_for_timeout(2_500)
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


def open_menu_on_tone(page, url, secret):
    """Log in, show the tone and open the menu on a selection (see drag_menu)."""
    show_tone_waterfall(page, url, secret)
    # The layer-1 classifier needs about a second of a pure tone to call it a carrier
    # (shorter spans read the discriminator's edge transients as FM deviation).
    page.wait_for_function("() => window.zetaTest.wf().painted > 60", timeout=15_000)
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
    page.wait_for_function("() => window.zetaTest.wf().painted > 60", timeout=15_000)
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
    page.wait_for_function("() => window.zetaTest.wf().painted > 60", timeout=15_000)
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
    page.wait_for_function("() => window.zetaTest.wf().painted > 60", timeout=15_000)
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


def scenario_scan_of_a_sub_band_scans_just_that_band(page, url, secret):
    show_tone_waterfall(page, url, secret)
    page.wait_for_function("() => window.zetaTest.wf().painted > 60", timeout=15_000)
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
    cur = source_info(url, secret, "retune")["current"]["center_hz"]
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


def scenario_view_set_show_reset(page, url, secret):
    page.set_viewport_size(WIDE)
    login(page, url, secret)
    expect(page.locator('.src-tab[data-id="tone"]')).to_be_visible()
    run_cmd(page, "view set follow free")
    expect(view_line(page, "follow = free")).to_have_text("follow = free")
    shown = view_show(page)
    for text in ("follow free", "zoom_out scan", "auto_scan_stop inside",
                 "finished_rows_s 60", "scanner auto", "plan auto", "map off", "home none"):
        assert text in shown, f"{text!r} not in view show: {shown!r}"
    assert shown.count("\n") == 7, f"view show: {shown!r}"
    run_cmd(page, "view set follow sideways")
    expect(view_line(page, "sideways", error=True)).to_contain_text("target, free")
    run_cmd(page, "view set finished_rows_s 5")
    expect(view_line(page, "finished_rows_s = 5")).to_be_visible()
    run_cmd(page, "view set finished_rows_s -1")
    expect(view_line(page, '"-1"', error=True)).to_be_visible()
    run_cmd(page, "view set nosuch 1")
    expect(view_line(page, "nosuch", error=True)).to_contain_text(
        "follow, zoom_out, auto_scan_stop, finished_rows_s, scanner, plan, map, home")
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
        cur = source_info(url, secret, "retune")["current"]
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
        cur = source_info(url, secret, "retune")["current"]["center_hz"]
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
        cur = source_info(url, secret, "retune")["current"]["center_hz"]
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
    cur = source_info(url, secret, "retune")["current"]["center_hz"]
    lo = start_job(url, secret, "passthrough", "retune", cur - 8_000, 4_000)
    hi = None
    try:
        hi = start_job(url, secret, "passthrough", "retune", cur + 8_000, 4_000)
        ol = outlines(page)
        expect(ol).to_have_count(2)
        for job in (lo, hi):
            el = page.locator(f'#waterfall .outline[data-key="{job}"]')
            expect(el).to_have_count(1)
            expect(el).to_contain_text("passthrough")
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
    cur = source_info(url, secret, "retune")["current"]["center_hz"]
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
        assert source_info(url, secret, "retune")["current"]["center_hz"] == body["center_hz"]
        expect(page.locator("#transcript .line.op", has_text=re.compile(
            rf"^job_retune job={b} .*replace=… → job {b}"))).to_have_count(1)
    finally:
        stop_job(url, secret, a)
        if b:
            stop_job(url, secret, b)


def move_retune_capture(url, secret):
    """Hold `retune` with a rigid passthrough on its capture, then move the capture 1 MHz with
    a rigid start that replaces it; (the new job's id, the old centre, the new centre)."""
    cur = source_info(url, secret, "retune")["current"]["center_hz"]
    held = start_job(url, secret, "passthrough", "retune", cur, 10_000)
    band = cur + 1_000_000 if cur + 1_000_000 < 299_000_000 else cur - 1_000_000
    r = http_json(url, secret, "/op", {"op": "job_start", "kind": "passthrough",
                                       "source": "retune", "center_hz": band,
                                       "bandwidth_hz": 10_000, "deviation_hz": None,
                                       "sideband": None, "elastic": False, "replace": [held]})
    assert r["result"] == "job", f"replacing start: {r}"
    return r["value"], cur, band


def capture_of(url, secret):
    c = source_info(url, secret, "retune")["current"]
    return c["center_hz"] - c["bandwidth_hz"] / 2, c["center_hz"] + c["bandwidth_hz"] / 2


def near(a, b, width):
    return abs(a - b) <= width * 0.01


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


def scenario_watched_jobs_are_capped(page, url, secret):
    # Six measure jobs: the four most recent are watched (one event socket each), the other
    # two rows show "—" and open nothing, so this tab holds at most ops + waterfall + 4.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    jobs = []
    try:
        for _ in range(6):
            jobs.append(start_job(url, secret, "measure", "tone", 100_000_000, 10_000))
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator('#jobs .job[data-state="running"]', has_text="measure")).to_have_count(6)
        for j in jobs[:2]:
            cell = page.locator(f'#jobs .job[data-id="{j}"] .events')
            expect(cell).to_have_text("—")
            expect(cell).to_have_attribute("title", "not watched (limit 4)")
        for j in jobs[2:]:
            cell = page.locator(f'#jobs .job[data-id="{j}"] .events')
            expect(cell).not_to_have_attribute("title", "not watched (limit 4)")
        # The four watches open (measure events can take seconds to come, so sockets are
        # counted rather than events), and no more than four.
        deadline = time.monotonic() + 5.0
        while ws_connections(url, secret) < 2 + 4 and time.monotonic() < deadline:
            time.sleep(0.2)
        counts = stable_ws_connections(url, secret)
        assert max(counts) <= 2 + 4, f"ws_connections {counts}"
        # Four newer fm_demods (squelch streams only) take no decoder's slot: they go
        # unwatched and the four measures keep theirs.
        for _ in range(4):
            jobs.append(start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500))
        page.evaluate("window.zetaTest.poll()")
        for j in jobs[6:]:
            expect(page.locator(f'#jobs .job[data-id="{j}"] .events')).to_have_attribute(
                "title", "not watched (limit 4)")
        for j in jobs[2:6]:
            cell = page.locator(f'#jobs .job[data-id="{j}"] .events')
            expect(cell).not_to_have_attribute("title", "not watched (limit 4)")
    finally:
        for j in jobs:
            stop_job(url, secret, j)


MAX_WS = 16  # zeta-ws Limits::default().max_ws

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
    # With every stream slot taken (by another page), a new watch is refused with 503: one
    # error line in the transcript, and the watch opens once a slot frees.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    ops = [s for s in http_json(url, secret, "/streams")
           if s["provenance"]["source"] == "node"][0]["id"]
    proto = "zeta.bearer." + base64.urlsafe_b64encode(secret.encode()).decode().rstrip("=")
    hog = page.context.new_page()
    job = None
    try:
        hog.goto(url + "/ui/")
        free = MAX_WS - stable_ws_connections(url, secret)[-1]
        assert hog.evaluate(HOG_JS, [free, ops, proto]) == free, "hog sockets did not open"
        job = start_job(url, secret, "measure", "tone", 100_000_000, 10_000)
        page.evaluate("window.zetaTest.poll()")
        busy = page.locator("#transcript .line.note.error", has_text="out of stream slots")
        expect(busy).to_have_count(1)
        # Retries keep the normal backoff and are not noted again.
        page.wait_for_timeout(3_000)
        expect(busy).to_have_count(1)
        expect(busy.locator(".count")).to_have_text("")
        hog.evaluate("window.hog.forEach((ws) => ws.close())")
        # ops + waterfall + the watch, once the hog's sockets are gone and the backoff (at
        # most 8 s) has run out.
        deadline = time.monotonic() + 15.0
        counts = []
        while time.monotonic() < deadline:
            counts = stable_ws_connections(url, secret)
            if counts[-1] == 3 and len(set(counts)) == 1:
                break
        assert counts and counts[-1] == 3, f"the refused watch never opened: {counts}"
    finally:
        hog.close()
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
        expect(page.locator("#transcript .line.op", has_text=re.compile(
            r"^job_start kind=ssb_demod source=tone center_hz=\d+ "
            r"bandwidth_hz=\d+ sideband=usb → job ")).first).to_be_visible()
        jobs = [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
                if j["kind"] == "ssb_demod" and j["source"] == "tone"
                and j["state"]["status"] == "running"]
        assert len(jobs) == 1, f"ssb_demod jobs on tone: {jobs}"
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
        expect(el).to_contain_text("watch")
        expect(el.locator(".outline-handle")).to_have_count(0)
        assert "retunable" not in (el.get_attribute("class") or "")
    finally:
        for j in watches:
            stop_job(url, secret, j["id"])


def scenario_owned_jobs_are_hidden_from_the_jobs_pane(page, url, secret):
    select_source(page, url, secret, "tone")
    page.wait_for_function("() => window.zetaTest.wf().painted > 30", timeout=15_000)
    cur = source_info(url, secret, "tone")["current"]
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
    attached = None
    try:
        job_row(page, demod).locator("button.transcribe").click()
        attached = wait_attached(nostt, secret, demod)["id"]
        page.evaluate("window.zetaTest.poll()")
        row = page.locator(f'#jobs .job[data-id="{attached}"]')
        expect(row.locator(".state")).to_have_text("failed")
        expect(row).to_have_attribute("title", re.compile("speech backend"))
        expect(row.locator(".kind")).to_have_text("↳ transcribe")
    finally:
        for j in (attached, demod):
            if j:
                stop_job(nostt, secret, j)


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


# ---- channel plans (plan 13 Task 4) ---------------------------------------------------------
# `cb` is the tone recording labelled at 476.9 MHz, channel 20 of uhf_cb_au.

def scenario_channel_plan_labels(page, url, secret):
    select_source(page, url, secret, "cb")
    job = start_job(url, secret, "fm_demod", "cb", 476_900_000, 12_500)
    try:
        row = job_row(page, job)
        expect(row.locator(".tag.ch")).to_have_text("ch20", timeout=3_000)
        label = page.locator(f'#waterfall .outline[data-key="{job}"] .outline-label')
        expect(label).to_contain_text("ch20 fm_demod")
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
        expect(label).to_have_text("fm_demod")
        run_cmd(page, "view set plan bogus")
        expect(view_line(page, "auto, none, uhf_cb_au", error=True)).to_be_visible()
        run_cmd(page, "view reset")
        expect(job_row(page, job).locator(".tag.ch")).to_have_text("ch20")
        expect(label).to_contain_text("ch20 fm_demod")
    finally:
        stop_job(url, secret, job)


# ---- the map (plan 17 Task 9) -----------------------------------------------------------
# The node's `fake_pos` mode row (ui.rs) runs zeta-ext's `fake_lines`, which prints two fixes
# of sonde S1 (-27.40,153.10 then -27.41,153.11) 2 s into the job. The map's tiles come from
# tile.openstreetmap.org, which every map scenario aborts: the map must still draw.

OSM_TILES = "**/tile.openstreetmap.org/**"


def page_errors(page):
    """Collects the page's console errors and uncaught exceptions from now on, except the
    load failures of the aborted map tiles (Chromium reports each as a console error)."""
    seen = []

    def on_console(m):
        if m.type != "error":
            return
        where = (m.location or {}).get("url", "")
        if "tile.openstreetmap.org" in where:
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
    page.route(OSM_TILES, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "tone")
    expect(page.locator("#map")).to_be_hidden()
    run_cmd(page, "view set map on")
    expect(view_line(page, "map = on")).to_be_visible()
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
        run_cmd(page, "view set map off")
        expect(mp).to_be_hidden()
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_map_follows_a_positioned_job_on_another_source(page, url, secret):
    # Spec 7.2: every running positioned job gets a marker, not only the shown source's (the
    # two-radio form: the page shows the watching radio, the decoder runs on the other).
    page.route(OSM_TILES, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "aprs")
    run_cmd(page, "view set map on")
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
        run_cmd(page, "view set map off")
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


def scenario_home_gives_distance_and_bearing(page, url, secret):
    page.route(OSM_TILES, lambda r: r.abort())
    errors = page_errors(page)
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
        # The last fix, -27.41,153.11 from -27.50,153.00: 14.8 km at 48°.
        expect(pos).to_have_text("14.8 km, 47°")
        run_cmd(page, "view set home none")
        expect(pos).to_have_count(0)
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)


# ---- runner ----------------------------------------------------------------------------

def scenarios():
    return [(name[len("scenario_"):], fn) for name, fn in globals().items()
            if name.startswith("scenario_") and callable(fn)]


# Collects the page's Content-Security-Policy violations (checked after every scenario).
CSP_WATCH_JS = """
window.__cspViolations = [];
document.addEventListener('securitypolicyviolation', (e) => {
  window.__cspViolations.push(`${e.violatedDirective} ${e.blockedURI}`);
});
"""


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
    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=True, args=CHROMIUM_ARGS)
        try:
            for name, fn in todo:
                ctx = browser.new_context(viewport=WIDE)
                ctx.set_default_timeout(TIMEOUT_MS)
                ctx.add_init_script(CSP_WATCH_JS)
                page = ctx.new_page()
                console = []
                page.on("console", lambda m: console.append(f"{m.type}: {m.text}")
                        if m.type == "error" else None)
                page.on("pageerror", lambda e: console.append(f"pageerror: {e}"))
                started = time.monotonic()
                try:
                    fn(page, url, secret)
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
