"""End-to-end scenarios for the ZetaSDR web app, driven by Playwright (headless Chromium).

Usage: python3 ui.py <node-url> <secret> [scenario]

Runs every `scenario_*` function (or only those named, comma-separated, with or without the
`scenario_` prefix) against a running `zeta serve`, each in a fresh browser context. Prints `PASS <name>`
or `FAIL <name>: <reason>` (plus the browser console errors) and exits 1 on the first failure.
Run by `cargo test -p zeta --test ui`, which starts the node with the `tone`, `aprs` and `wfm`
(wideband FM, 1 kHz tone with pilot, 240 kHz at 104.9 MHz) fixtures and a fake speech daemon (`--stt-socket`, answering `what sources are there`), passes the node's
state directory in ZETA_UI_STATE_DIR, and the URL of a second node started without a speech
daemon in ZETA_UI_URL_NOSTT (for the "no daemon" scenario).
"""

import base64
import json
import os
import re
import struct
import sys
import time
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
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (3 sources)")
    # An op error shows its name and field, marked as an error.
    page.fill("#cmd", json.dumps({"op": "job_start", "kind": "measure", "source": "tone",
                                  "center_hz": 1, "bandwidth_hz": 10000,
                                  "deviation_hz": None, "sideband": None}))
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
                                  "deviation_hz": None, "sideband": None}))
    page.press("#cmd", "Enter")
    expect(page.locator("#transcript")).to_contain_text("job_start kind=measure source=tone")
    row = page.locator("#jobs .job", has_text="measure")
    expect(row).to_contain_text("running")
    row.locator("button.stop").click()
    expect(page.locator("#transcript")).to_contain_text("job_stop")
    expect(row).to_have_count(0)


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
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (3 sources)")
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
    for source in ("tone", "aprs", "wfm"):
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
                                         "deviation_hz": None, "sideband": None})["value"]
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
                                          "deviation_hz": deviation_hz, "sideband": None})["value"]


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
    expect(page.locator("#transcript")).to_contain_text("source_list → ok (3 sources)")
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
    show_tone_waterfall(page, url, secret)
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
    fresh_spectrum_jobs(url, secret)
    job = start_job(url, secret, "measure", "wfm", 104_900_000, 10_000)
    try:
        select_source(page, url, secret, "wfm")
        page.wait_for_function("() => window.zetaTest.wf() && window.zetaTest.wf().painted > 5",
                               timeout=15_000)
        before = ticks(page)
        wheel_at_centre(page, 100)
        line = page.locator("#transcript .line", has_text="conflict: source wfm held by")
        expect(line.first).to_be_visible()
        expect(line.first).to_contain_text(job)
        wait_ticks(page, before[0], before[4], "view not reverted")
        page.wait_for_timeout(1_000)
        assert ensure_lines(page) == 0, "source_ensure issued on a source held by another job"
        jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
        mine = [j for j in jobs if j["id"] == job]
        assert mine and mine[0]["state"]["status"] == "running", f"measure job: {mine}"
    finally:
        stop_job(url, secret, job)


def running_scan_jobs(url, secret, source):
    jobs = http_json(url, secret, "/op", {"op": "job_list"})["value"]
    return [j for j in jobs if j["kind"] == "scan" and j["source"] == source
            and j["state"]["status"] == "running"]


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
    # An fm_demod job has no event stream: it is looked up once, not on every poll.
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
        expect(page.locator("#jobs .job", has_text="measure")).to_have_count(6)
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
            rf"^job_start kind=ssb_demod source=tone center_hz={centre - 1500} "
            r"bandwidth_hz=6000 sideband=usb → job ")).first).to_be_visible()
        jobs = [j for j in http_json(url, secret, "/op", {"op": "job_list"})["value"]
                if j["kind"] == "ssb_demod" and j["source"] == "tone"
                and j["state"]["status"] == "running"]
        assert len(jobs) == 1, f"ssb_demod jobs on tone: {jobs}"
        expect(job_row(page, jobs[0]["id"]).locator("button.play")).to_have_text("▶")
    finally:
        for j in jobs:
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


# ---- voice (Task 11) -----------------------------------------------------------------------

SPOKEN = "what sources are there"


def record_and_stop(page, hold_ms):
    """Click #mic, check it records (red dot) for `hold_ms`, then click it again."""
    mic = page.locator("#mic")
    expect(mic).to_be_enabled()
    mic.click()
    expect(mic).to_have_class(re.compile(r"\brecording\b"))
    page.wait_for_timeout(hold_ms)
    mic.click()
    expect(mic).not_to_have_class(re.compile(r"\brecording\b"))


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
