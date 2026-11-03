# Console Theme Implementation Plan (plan 24)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The page gets a `console` theme (the default) that frames the spectrum in elbow-and-bar chrome, moves the job tags to a frequency-aligned rail beside the axis, lets decoded ribbons ride their band tilted and step up on collision, gives job rows two lines, makes the side panes collapsible and skins the graph modal; `view set theme plain` gives today's page back.

**Architecture:** One DOM, two skins. Every colour, radius, font and the ribbon tilt becomes a `:root` variable with a `[data-theme="console"]` override; `view.js` gains `theme` and `side` policies that `app.js` mirrors onto `<html data-theme>` and the side sections. `waterfall.js` creates `#rail` beside `.wf-axis`; `overlays.js` places the existing `.job-tag` elements there (or in the outline, in plain) and owns the ribbon collision rule behind two exported constants. `agent.js` keeps every job-row span and the CSS lays them on two lines. No node change.

**Tech Stack:** Vanilla ES modules and CSS in `crates/zeta-ui/web`, assets embedded by `crates/zeta-ui/src/lib.rs` `ASSETS`, Playwright scenarios in `crates/zeta-ui/tests/e2e/ui.py` run by `crates/zeta/tests/ui.rs` (`ZETA_UI_ONLY=a,b` selects scenarios), the Antonio font (SIL OFL 1.1).

**Spec:** `docs/superpowers/specs/2026-11-03-console-theme-design.md` (read it first; section numbers below refer to it). Where this plan departs from it, "Rulings on the spec" at the end says why. Those rulings bind.

## Global Constraints

- Theme id `console`; the other is `plain`; `console` is the default (spec 4.1). Switch: `view set theme console|plain`, mirrored as `<html data-theme="…">`.
- Anti-goals (spec 2): none of the franchise's names or marks anywhere in code, CSS, docs, settings or the page. The minimum word list is the set of words commit `22db49e` removed from `TODO.md`; `git grep -i` for them must be empty on every commit of this plan. Theme and class names are generic (`console`, `bar`, `elbow`, `rail`, `cap`).
- Palette (author's values, spec 10.10; documented in CSS with one comment per role): frame `#f08c3c`, source bar `#b79ae6`, action `#f3e3a0`, channel 1-6 `#6aa8e0 #f3e3a0 #b79ae6 #6fcfc4 #f7b98a #f08c3c`, idle `#3a3d48`, speaking `#7ee787`, error `#e05a5a`, console ground `#000`, console text `#e9e4d8`, console dim `#8a8f9a`, waterfall canvas `#000030` in both themes. Black text on every coloured bar.
- Geometry: `--radius-bar` 14px / 3px (console / plain), `--radius-elbow` 22px / 0, `--rail-w` 96px / 0, `--rail-thin-px` 5, `--bar-h` 34px top and 38px bottom as today, `--ribbon-tilt` -15deg / 0deg, `--side-collapsed-w` 44px.
- Ribbon collision constants (spec 4.5): `RIBBON_STEP_DIR = -1`, `RIBBON_STEP_MS = 160`, lane = ribbon height + `RIBBON_GAP_PX` (2), `MAX_LANES` 8 unchanged.
- Jobs fold (spec 4.7): `JOBS_FOLD_MIN = 8`, `JOBS_FOLD_IDLE_MS = 60_000`, group `details.job-group[data-plan]` open by default, rows stay in the DOM.
- Every element the e2e selects keeps its id, classes, data attributes and text (spec 3). Exactly four existing assertions change, all in Task 3 and Task 5: `ui.py:1734`, `:2565`, `:2112` (tag through the outline → `#waterfall .job-tag[data-job=…]`) and `ui.py:1128` (`view show` newline count 11 → 13). Any other existing scenario that fails is a bug in the build.
- Every side-bound CSS property written or touched by this plan is logical (`inset-inline-start`, `margin-inline-end`, `padding-inline-*`, `border-start-start-radius`, `text-align: start|end`). Every ribbon, `#transcript .line` body and job-row text span gets `dir="auto"`.
- All DOM from data uses `textContent`. No external fetch at runtime; the font is embedded.
- Gates per task: `cargo fmt -p zeta-ui -- --check` (and `-p zeta` when `ui.rs` is touched; per-member, the nested worktree breaks `--all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test -p zeta-ui`, and the task's scenarios via `ZETA_UI_ONLY=<names> cargo test -p zeta --test ui`. The final task runs the whole suite and reports its wall-clock time against `DEADLINE` (600 s; 352-440 s today).
- Temp files under the job's tmp directory, never `/tmp`. Python via `python3 -I` except Playwright.
- Commits on `worktree-plan24-console-theme`, one per task, with the message each task names, each ending with a blank line and the session attribution line the harness gives. No merge and no deploy in this plan.

## Review Focus

1. **A tag whose outline is off-screen.** A job above or below the shown band must not pin its rail tag at the rail's edge over another tag; it is hidden, as its outline is (`o.el.hidden`). Pinned in Task 3 (`scenario_rail_tags_align`, the third job).
2. **A ribbon on a band with no outline** (an `agent` ribbon, a `classify` label's hz, a keyed `fake_pos` ribbon): it still centres on `yFor(hz)` and never steps a keyed ribbon. Pinned in Task 4 (`scenario_ribbon_rides_band`, the keyed assertion).
3. **Right-to-left text in a chunk.** An Arabic transcript renders right to left inside its ribbon and its chat line while the ribbon stays anchored by its centre. Pinned in Task 4 (`scenario_dir_auto`).
4. **Theme switch with jobs running.** Switching to `plain` must move every live tag back into its outline and switching back must move it to the rail, with `hot`/`speaking` and the squelch dot intact, not leave orphans. Pinned in Task 3 (`scenario_theme_switch`).
5. **A folded job that changes.** A job leaving the idle set (stopped, squelch opened, an event arrived) must leave the group within one poll, and the group must disappear under `JOBS_FOLD_MIN`; the three exits share one code path. Pinned in Task 5 (`scenario_job_rows_fold_idle_plan_channels`, the stop path).

---

### Task 1: The theme layer and the font (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/view.js:89-170` (POLICIES)
- Modify: `crates/zeta-ui/web/app.js:798-802` (view.onChange), and the boot path that applies `data-map` (`app.js:870-892`) for the pattern
- Modify: `crates/zeta-ui/web/style.css` (whole file: `:root`, the literals, a `[data-theme="console"]` block)
- Modify: `crates/zeta-ui/web/waterfall.js:189, 937, 955, 1480, 1585-1586`
- Create: `crates/zeta-ui/web/vendor/antonio/Antonio[wght].ttf` (or `.woff2`), `crates/zeta-ui/web/vendor/antonio/OFL.txt`
- Modify: `crates/zeta-ui/src/lib.rs:4-120` (ASSETS), `:152-190` (embed test)
- Test: `crates/zeta-ui/tests/e2e/ui.py` (new `scenario_theme_switch`, the `view show` assertion at `:1128`)

**Interfaces:**
- Produces: view policy `theme` (`values: ["console", "plain"]`, `default_: "console"`); `<html data-theme>` always set (never absent); CSS variables named in Global Constraints plus `--c-frame`, `--c-source`, `--c-action`, `--c-channel-1..6`, `--c-idle`, `--c-speaking`, `--wf-bg`, `--wf-select`, `--font-ui`, `--ribbon-tilt`, `--rail-w`, `--radius-bar`, `--radius-elbow`; `waterfall.js` export `themeColour(name) -> string` reading `getComputedStyle(document.documentElement).getPropertyValue(name).trim()`.
- Later tasks read these names; they do not add `:root` colours of their own.

- [ ] **Step 1: Write the failing scenario**

```python
def scenario_theme_switch(page, url, secret):
    select_source(page, url, secret, "tone")
    expect(page.locator("html")).to_have_attribute("data-theme", "console")
    run_cmd(page, "view set theme plain")
    expect(view_line(page, "theme = plain")).to_be_visible()
    expect(page.locator("html")).to_have_attribute("data-theme", "plain")
    stored = page.evaluate("sessionStorage.getItem('zeta.view')")
    assert '"theme":"plain"' in stored, stored
    run_cmd(page, "view set theme neon")
    expect(view_line(page, "neon", error=True)).to_contain_text("console, plain")
    run_cmd(page, "view reset")
    expect(page.locator("html")).to_have_attribute("data-theme", "console")
```

Also change `ui.py:1128` to `assert shown.count("\n") == 12` and add `"theme console"` to the tuple above it. (Task 5 raises both again for `side`.)

- [ ] **Step 2: Run it to verify it fails**

Run: `ZETA_UI_ONLY=theme_switch,view_set_show_reset cargo test -p zeta --test ui` (`view_set_show_reset` is the scenario holding `:1128`).
Expected: FAIL, `data-theme` absent.

- [ ] **Step 3: Add the policy and mirror it**

`view.js`: append `{ key: "theme", values: ["console", "plain"], default_: "console", describe: "console, plain" }` to `POLICIES`. `app.js`: a function `applyTheme()` that sets `document.documentElement.dataset.theme = state.view.get("theme")`, called at boot beside `applyMapPane()` and from the `onChange` handler on `key === "theme"`.

- [ ] **Step 4: Reorganise `style.css` into variables**

Move every hex and `rgba()` literal outside `:root` into a variable (a translucent form becomes `color-mix(in srgb, var(--x) 78%, transparent)` or its own variable). `:root` holds the plain values (today's) plus the new roles with plain values that leave the plain look unchanged (`--ribbon-tilt: 0deg`, `--rail-w: 0px`, `--radius-bar: 3px`, `--radius-elbow: 0`, `--font-ui` today's monospace stack, `--c-channel-1..6` all `var(--accent)`, `--wf-bg: #000030`, `--wf-select: #ffffff`). A `[data-theme="console"]` block restates the roles with the Global Constraints palette, `--font-ui: "Antonio", "DejaVu Sans Condensed", "Roboto Condensed", "Arial Narrow", sans-serif`, `--ribbon-tilt: -15deg`, `--rail-w: 96px`, the radii, and `body { background: var(--bg); font-family: var(--font-ui); }`. Every label role (bar captions, `.pane h2`, `.job .kind`, `.job-tag .kind`, `.job-tag .channel`) gets `text-transform: uppercase` under console only; decoded text, `.line` bodies, params and `.band` stay as sent. Numeric cells (`.band`, `.centre`, `.src-tab .range`, `.src-tab .centre`, `#subjects td`) get `font-variant-numeric: tabular-nums`.

- [ ] **Step 5: Canvas colours from the theme**

`waterfall.js`: export `themeColour(name)`; replace `BG` with a `let` refreshed by `refreshThemeColours()` that parses `--wf-bg` (hex → `[r,g,b]`) and reads `--wf-select` for `drawDrag()`; call it in the constructor and from `app.js` on a `theme` change (`state.wf.refreshThemeColours()`), then `redraw()`. The colormap stop at line 223 stays literal.

- [ ] **Step 6: Bundle the font**

Fetch `ofl/antonio/Antonio[wght].ttf` and `ofl/antonio/OFL.txt` from `https://github.com/google/fonts` (raw, main branch) into `web/vendor/antonio/`. If `woff2_compress` is on the host, ship the `.woff2` (MIME `font/woff2`), else the `.ttf` (MIME `font/ttf`). Add both files to `ASSETS` with a comment naming the licence, and to the embed test's list. `@font-face { font-family: "Antonio"; src: url("vendor/antonio/<file>") format("<woff2|truetype>"); font-weight: 400 700; font-display: swap; }` in `style.css`. If the fetch is impossible offline, skip the files, keep the fallback stack, and say so in the commit message and the final report.

- [ ] **Step 7: Run the gates**

Run: `cargo fmt -p zeta-ui -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test -p zeta-ui && ZETA_UI_ONLY=theme_switch,view_set_show_reset,login_and_tabs,graph_view_renders_nodes cargo test -p zeta --test ui`
Expected: all PASS; `git grep -i` for the word list empty.

- [ ] **Step 8: Commit**

```bash
git add crates/zeta-ui
git commit -m "UI: a theme layer (view set theme console|plain, console default): every colour, radius, font and the ribbon tilt a variable; the waterfall canvas reads its colours from the theme; Antonio (OFL) bundled"
```

---

### Task 2: The frame (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/index.html:21-45`
- Modify: `crates/zeta-ui/web/app.js:290-330` (`decorateTab`, `renderTabs`)
- Modify: `crates/zeta-ui/web/style.css` (`#app`, `#top`, `.brand`, `.src-tab`, `#panes`, `#listen-all`, `#map-toggle`, `#drawer-toggle`, `#bar`, `#main`)
- Test: `crates/zeta-ui/tests/e2e/ui.py` (new `scenario_source_bar_shows_live_numbers`)

**Interfaces:**
- Consumes: the variables of Task 1.
- Produces: inside `.src-tab[aria-selected="true"]` a `span.health` holding pairs `span.k` / `span.v` with `data-field` in `pin|rate|ovf|gain|hold|bank|fault`; the same text in the tab's `title` in both themes (plain hides `.health` by CSS).

- [ ] **Step 1: Write the failing scenario**

```python
def scenario_source_bar_shows_live_numbers(page, url, secret):
    select_source(page, url, secret, "tone")
    tab = page.locator('.src-tab[aria-selected="true"]')
    health = tab.locator(".health")
    expect(health.locator('.v[data-field="hold"]')).to_have_text(re.compile(r"^\d+$"))
    expect(health.locator('.v[data-field="bank"]')).to_have_text(re.compile(r"^(direct|\d+)$"))
    expect(health.locator('.v[data-field="gain"]')).to_have_text(re.compile(r"^(auto|-?\d+(\.\d+)? dB|—)$"))
    assert "hold" in (tab.get_attribute("title") or "")
    run_cmd(page, "view set theme plain")
    expect(health).to_be_hidden()
```

- [ ] **Step 2: Run it to verify it fails**

Run: `ZETA_UI_ONLY=source_bar_shows_live_numbers cargo test -p zeta --test ui`
Expected: FAIL, no `.health`.

- [ ] **Step 3: Render the health pairs**

`app.js` `decorateTab(tab, s)`: build or update `span.health` from the `source_list` record: `pin` → `s.pin ? (s.pin.center_hz/1e6).toFixed(3) : "—"`, `rate` → `s.stats?.measured_rate_hz ?? "—"` (thousands-separated with a thin space), `ovf` → `s.stats?.overflows ?? "—"`, `gain` → `s.gain ? (s.gain.db == null ? "auto" : `${s.gain.db} dB`) : "—"`, `hold` → `s.holders.length`, `bank` → `s.bank ? s.bank.bin_hz : "direct"`, `fault` only when `s.fault` (text `fault`). Put the same pairs, space-joined, in `tab.title` after today's text. `textContent` only.

- [ ] **Step 4: The chrome in CSS**

Under `[data-theme="console"]`: `#top` and `#bar` are black with a `gap` of 6px; `.brand` is the elbow (`background: var(--c-frame); color: #000; border-start-start-radius: var(--radius-elbow); padding-inline: 10px 8px; display:flex; align-items:flex-end;`), `.src-tab` a bar (`background: var(--c-source); border-radius: var(--radius-bar); color: #000;`, selected one `flex: 1`, others `flex: 0 0 auto`), `.pane-tab`, `#map-toggle`, `#listen-all`, `#drawer-toggle` bars in `--c-action` with the last visible one end-capped (`border-start-end-radius`/`border-end-end-radius` on `:last-of-type`), `#bar .prompt` the lower elbow (`border-end-start-radius: var(--radius-elbow)`), `#cmd` a dark bar, `#mic` a peach bar. `#main` gets `padding-inline-start: 12px` painted `var(--c-frame)` as the spine between the elbows (a `::before` pseudo on `#main`), and `#waterfall { inset-inline-start: 12px }` under console. Plain: no change in rendering (verify by screenshot diff of `plain` before/after is not required; eyeball at 1280 px).

- [ ] **Step 5: Run the gates**

Run: the Task 1 gate command with `ZETA_UI_ONLY=source_bar_shows_live_numbers,login_and_tabs,narrow_hides_pane,theme_switch`.
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/zeta-ui
git commit -m "UI: the console frame: elbows top-left and bottom-left, bar segments with end caps, the selected source's bar carrying pin, rate, overflows, gain, holders and bank from source_list"
```

---

### Task 3: The rail (Opus)

**Files:**
- Modify: `crates/zeta-ui/web/waterfall.js:456-469` (mount: create `#rail` before the axis), `:909-915` (expose nothing new; `yForHz` exists)
- Modify: `crates/zeta-ui/web/overlays.js:176-186` (constants), `:283-300` (constructor: `this.rail`), `:985-1085` (`tag`, `untag`, `refreshTag`, `layoutTags`), `:1087-1110` (`setHot`, `setSpeaking` unchanged in behaviour), `:1352-1364` (`layout` calls `layoutTags`)
- Modify: `crates/zeta-ui/web/app.js` (after `mountWaterfall`: `state.ov.setRail(state.wf.rail)`; on `theme` change: `state.ov.setRail(theme === "console" ? state.wf.rail : null)`)
- Modify: `crates/zeta-ui/web/agent.js:400-460` (row start border colour from `channelColour`)
- Modify: `crates/zeta-ui/web/style.css` (`#waterfall`, `#rail`, `.job-tag`, `.outline`, `.ov-ribbon` colour hooks)
- Modify: `crates/zeta-ui/tests/e2e/ui.py:1734, 2565, 2112` (re-point three tag lookups), new `scenario_rail_tags_align`, `scenario_rail_collapses_crowd`, and extend `scenario_theme_switch`

**Interfaces:**
- Consumes: Task 1's variables and `data-theme`.
- Produces: `overlays.js` exports `channelColour(jobId) -> 1..6` (stable: sum of char codes mod 6, plus 1) and the tag/outline/ribbon carry `data-colour="<n>"` which CSS maps to `--c-channel-n`; `Overlays.setRail(el|null)` moves every existing tag between rail and outline at once; `waterfall.js` `Waterfall.rail` is the `div#rail` element; a new `span.centre` in every tag (text `476.625` style, three decimals, from `job.tuning.center_hz`).

- [ ] **Step 1: Re-point the three outline-scoped lookups**

`ui.py:1734` and `:2565`: `expect(page.locator(f'#waterfall .job-tag[data-job="{job}"] .kind')).to_have_text(...)` (keep the outline count assertions). `ui.py:2112`: `centre_of(page.locator(f'#waterfall .job-tag[data-job="{job}"]').bounding_box())`. Run the three scenarios to confirm they still pass before the rail exists.

- [ ] **Step 2: Write the failing scenarios**

```python
def scenario_rail_tags_align(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = source_info(url, secret, "retune")["current"]["center_hz"]
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
        # A job outside the shown band has a hidden outline and a hidden tag, not one pinned at the edge.
        set_band(page, cur, 20_000)
        c = start_job(url, secret, "passthrough", "retune", cur + 60_000, 4_000)
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator(f'#rail .job-tag[data-job="{c}"]')).to_be_hidden()
    finally:
        for j in (a, b, c):
            if j:
                stop_job(url, secret, j)


def scenario_rail_collapses_crowd(page, url, secret):
    select_source(page, url, secret, "cb")
    jobs = [start_job(url, secret, "passthrough", "cb", 476_900_000 + i * 12_500, 10_000) for i in range(6)]
    try:
        set_band(page, 476_931_250, 1_000_000)
        page.evaluate("window.zetaTest.poll()")
        tags = page.locator("#rail .job-tag")
        expect(tags).to_have_count(6)
        expect(page.locator("#rail .job-tag.thin")).to_have_count(5)
        full = page.locator("#rail .job-tag:not(.thin)")
        expect(full).to_have_count(1)
        other = page.locator(f'#rail .job-tag[data-job="{jobs[3]}"]')
        other.hover()
        expect(other).not_to_have_class(re.compile(r"\bthin\b"))
        expect(page.locator("#rail .job-tag:not(.thin)")).to_have_count(1)
    finally:
        for j in jobs:
            stop_job(url, secret, j)
```

Extend `scenario_theme_switch`: with a `passthrough` job running on `tone`, after `view set theme plain` expect `#waterfall .outline[data-key=job] .job-tag` count 1 and `#rail` hidden; after `view set theme console` expect `#rail .job-tag[data-job=job]` count 1 and the outline's count 0; the tag's `.kind` text is `passthrough` in both.

- [ ] **Step 3: Run them to verify they fail**

Run: `ZETA_UI_ONLY=rail_tags_align,rail_collapses_crowd,theme_switch cargo test -p zeta --test ui`
Expected: FAIL, no `#rail`.

- [ ] **Step 4: Create the rail in `waterfall.js`**

In the constructor: `this.rail = document.createElement("div"); this.rail.id = "rail";` and `el.replaceChildren(this.rail, this.axis, this.canvas, this.overlay, this.badge, this.bandForm)`. CSS: `#waterfall #rail { position: absolute; inset-inline-start: 0; top: 0; bottom: 0; width: var(--rail-w); overflow: hidden; }`, `#waterfall .wf-axis { inset-inline-start: var(--rail-w); }`, canvas, overlay and `.ov-layer` `inset-inline-start: calc(var(--rail-w) + var(--axis-w)); width: calc(100% - var(--rail-w) - var(--axis-w));`. Under plain `--rail-w` is 0 and `#rail { display: none }`.

- [ ] **Step 5: Tags into the rail in `overlays.js`**

`setRail(el)`: store `this.rail = el`; for every outline with a tag, move `o.tag` to `el` (or back into `o.el` when `null`) and call `layoutTags()`. In `tag(o, key)`: append to `this.rail ?? o.el`; add `o.parts.centre = span("centre")` after `channel`; set `el.dataset.colour = o.el.dataset.colour = channelColour(key)`. In `refreshTag`: `parts.centre.textContent = job.tuning ? (job.tuning.center_hz / 1e6).toFixed(3) : ""`. `layoutTags()` when `this.rail`: `top = yFor(centre) - th/2` clamped to `[0, railHeight - th]`, hidden when `o.el.hidden`; sort by top; walk once: a tag overlapping the previous visible full tag becomes `thin` (height `--rail-thin-px`, `top = yFor(centre) - thin/2`) unless it is `hot` or `speaking`, in which case the previous one becomes `thin` instead; the previous outline-stacking branch stays for `this.rail == null`. `hoverTag` and `setSpeaking` call `layoutTags()` after toggling the class. Pointer handlers already attach to `el`; add `el.addEventListener("pointerenter"...)` nothing new. Export `channelColour`.

- [ ] **Step 6: Colour hooks**

CSS under console: `.job-tag[data-colour="1"] { background: var(--c-channel-1); color: #000; }` for 1..6 (plain keeps today's rule); `#waterfall .outline[data-colour="n"] { border-color: var(--c-channel-n) }`; `.job-tag.thin { height: var(--rail-thin-px); padding: 0; border-radius: 0 3px 3px 0; } .job-tag.thin > * { display: none }`; `.job-tag .short, .job-tag .snippet { display: none }` under console; `.job-tag:has(.channel:not(:empty)) .kind { display: none }` under console. `agent.js` `jobRow`: `row.dataset.colour = channelColour(job.id)`; CSS `#jobs .job[data-colour="n"] { border-inline-start: 3px solid var(--c-channel-n) }`.

- [ ] **Step 7: Wire `app.js`**

After `mountWaterfall`: `state.ov.setRail(state.view.get("theme") === "console" ? state.wf.rail : null)`; in the `onChange` handler the same on `key === "theme"`.

- [ ] **Step 8: Run the gates**

Run: the gate with `ZETA_UI_ONLY=rail_tags_align,rail_collapses_crowd,theme_switch,tags_show_kind_id_and_link_rows,the scenarios owning ui.py:1734, :2565 and :2112 (`grep -n 'def scenario_' ui.py | awk -F: '$1<N' | tail -1` for each),narrow_hides_pane` plus every scenario whose name contains `tag`, `outline` or `retune` (`grep -o 'def scenario_[a-z_]*\(tag\|outline\|retune\)[a-z_]*' ui.py`).
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/zeta-ui
git commit -m "UI: the rail: job tags leave the outlines for a frequency-aligned column beside the axis in the console theme, collapsing to thin segments where they crowd, the hot or speaking one winning the space; outline, tag and row share a per-job colour"
```

---

### Task 4: Riding ribbons (Opus)

**Files:**
- Modify: `crates/zeta-ui/web/overlays.js:176-186` (constants block), `:768-820` (`ribbon`, `makeRibbon`, `place`), `:1352-1364` (`layout`), `:1370-1385` (`scroll`)
- Modify: `crates/zeta-ui/web/app.js:1184` (`window.zetaTest`: add `ribbon(hz, text, cls)`)
- Modify: `crates/zeta-ui/web/agent.js:231` (`Transcript` line body `dir="auto"`)
- Modify: `crates/zeta-ui/web/style.css` (`.ov-ribbon` transform, transition, colours)
- Modify: `TODO.md` ("UI: staircase transcript ribbons on the waterfall" rewritten)
- Test: `crates/zeta-ui/tests/e2e/ui.py` (new `scenario_ribbon_rides_band`, `scenario_ribbon_clips_at_other_band`, `scenario_dir_auto`)

**Interfaces:**
- Consumes: Task 3's `data-colour` on outlines; `--ribbon-tilt`.
- Produces: `overlays.js` exports `RIBBON_STEP_DIR` and `RIBBON_STEP_MS`; each ribbon record gains `lane` (integer, 0 at the band); `window.zetaTest.ribbon(hz, text, cls)` returns the element; ribbons of a job's band carry `data-colour`.

- [ ] **Step 1: Write the failing scenarios**

```python
def scenario_ribbon_rides_band(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = source_info(url, secret, "retune")["current"]["center_hz"]
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
        # A keyed ribbon (a subject) never steps: a second one with the same key replaces in place.
        page.evaluate(f"window.zetaTest.ribbon({cur + 3_000}, 'VH-A 1', 'kr', 'VH-A')")
        page.evaluate(f"window.zetaTest.ribbon({cur + 3_000}, 'VH-A 2', 'kr', 'VH-A')")
        expect(page.locator("#waterfall .ov-ribbon.kr")).to_have_count(1)
    finally:
        stop_job(url, secret, job)


def scenario_ribbon_clips_at_other_band(page, url, secret):
    select_source(page, url, secret, "retune")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=10_000)
    cur = source_info(url, secret, "retune")["current"]["center_hz"]
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
    cur = source_info(url, secret, "tone")["current"]["center_hz"]
    page.evaluate(f"window.zetaTest.ribbon({cur}, 'مرحبا بالعالم', 'transcript')")
    rib = page.locator("#waterfall .ov-ribbon.transcript", has_text="مرحبا")
    assert rib.get_attribute("dir") == "auto"
    assert page.evaluate("el => getComputedStyle(el).direction", rib.element_handle()) == "rtl"
    run_cmd(page, "view get theme")
    line = page.locator("#transcript .line").last
    assert line.locator("span").first.get_attribute("dir") == "auto"
```

- [ ] **Step 2: Run them to verify they fail**

Run: `ZETA_UI_ONLY=ribbon_rides_band,ribbon_clips_at_other_band,dir_auto cargo test -p zeta --test ui`
Expected: FAIL, `zetaTest.ribbon` undefined.

- [ ] **Step 3: The test hook**

`app.js` `window.zetaTest`: `ribbon: (hz, text, cls, key) => key ? state.ov.keyedRibbon(hz, text, cls, key) : state.ov.ribbon(hz, text, cls)` (use the existing keyed-ribbon entry point at `overlays.js:716-746` under whatever name it has; if it is not a method, add `keyedRibbon(hz, text, cls, key)` delegating to it).

- [ ] **Step 4: The collision rule**

`overlays.js` constants block gets the exact comment and the two exports from spec 4.5. `makeRibbon`: `el.dir = "auto"`; `el.dataset.colour` from the outline whose band contains `hz` when one exists (search `this.outlines` for `lo <= hz <= hi`); `r.lane = 0`. `place(r)` (a new ribbon): `top = base`; collect `hits` = ribbons overlapping `r` at the right edge on this band (`o.right < r.w` and vertical overlap); for each hit that is not keyed, `o.lane += 1` toward `RIBBON_STEP_DIR` unless that would exceed `MAX_LANES` or put `o`'s box across another outline's centre line (`yFor(other.hz)` for every outline whose band is not `r`'s) in which case `o` keeps its lane and gets class `clipped`; then set every moved ribbon's `top = yFor(o.hz) - o.h/2 + o.lane * RIBBON_STEP_DIR * (o.h + RIBBON_GAP_PX)`. Newer ribbons get `z-index` from their index (`r.el.style.zIndex = String(this.ribbons.length)`). `layout()`: compute `top` from `lane` the same way (replacing `dy`), then, for each ribbon with `lane > 0` whose lane below is free of overlap at its `right`, `lane -= 1` (one step per `layout` call, which runs per column). `scroll` unchanged. CSS: `.ov-ribbon { transform: translateY(-50%) rotate(var(--ribbon-tilt)); transform-origin: center; transition: top var(--ribbon-step-ms, 160ms) ease-out; }` with `--ribbon-step-ms` set from `RIBBON_STEP_MS` on the layer element at construction (`this.layer.style.setProperty("--ribbon-step-ms", `${RIBBON_STEP_MS}ms`)`). Because `top` is now the centre line, `place` and `layout` set `top = yFor(hz) + lane…` without the `- h/2` (the transform does it) and the e2e's `style.top` difference is still `h + 2`.

- [ ] **Step 5: Colours and the chat log**

CSS under console: `#waterfall .ov-ribbon[data-colour="n"] { background: var(--c-channel-n); color: #000; border: 0; }` for 1..6; `.ov-ribbon.transcript-error, .ov-ribbon[data-dropped] { background: var(--error) }`; `.ov-ribbon.agent { background: var(--user); color: #000 }`; plain keeps the per-protocol rules. `agent.js` `Transcript` line body `span` gets `dir = "auto"`.

- [ ] **Step 6: Rewrite the TODO gist**

Replace the body of "UI: staircase transcript ribbons on the waterfall" with what remains: a lane budget from the pixel height between bands per footprint (2, 4, 8 lanes), the staircase sharing lanes with keyed rows, and a note that plan 24 shipped the step-up rule behind `RIBBON_STEP_DIR`/`RIBBON_STEP_MS`.

- [ ] **Step 7: Run the gates**

Run: the gate with `ZETA_UI_ONLY=ribbon_rides_band,ribbon_clips_at_other_band,dir_auto` plus every scenario whose name contains `ribbon`, `transcri` or `subjects_waterfall` (`grep -o 'def scenario_[a-z_]*\(ribbon\|transcri\|fold\)[a-z_]*' ui.py`).
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/zeta-ui TODO.md
git commit -m "UI: ribbons ride their band: centred on the line, tilted by the theme, a new chunk taking the line and the older ones stepping up a lane (RIBBON_STEP_DIR, RIBBON_STEP_MS) and falling back as it clears; dir=auto on every chunk and chat line"
```

---

### Task 5: The side column: collapsible panes, two-line jobs, the idle fold (Opus)

**Files:**
- Modify: `crates/zeta-ui/web/index.html:31-40`
- Modify: `crates/zeta-ui/web/view.js` (POLICIES: `side`)
- Modify: `crates/zeta-ui/web/app.js` (`applySide()` beside `applyTheme()`; the Jobs count)
- Modify: `crates/zeta-ui/web/agent.js:400-470` (`jobRow`: `dir="auto"` on text spans; `startJobs`: the fold group, the count)
- Modify: `crates/zeta-ui/web/style.css` (`#side`, `.pane`, `.pane h2`, `.job`, `.job-group`)
- Modify: `crates/zeta-ui/tests/e2e/ui.py:1128` (count 12 → 13, `"side both"` in the tuple), new `scenario_side_folds`, `scenario_job_row_two_lines`, `scenario_job_rows_fold_idle_plan_channels`

**Interfaces:**
- Consumes: Task 3's `channelColour` / `data-colour` on rows.
- Produces: view policy `side` (`values: ["both", "transcript", "jobs", "none"]`, `default_: "both"`); `section.pane[data-collapsed]`; `button.pane-head[aria-expanded]` inside each `h2`; `.pane.jobs h2 .count`; `details.job-group[data-plan]` with `summary` text `<n> idle <kind> on <plan>` (plus `, <m> transcribe` when attached ones are inside).

- [ ] **Step 1: Write the failing scenarios**

```python
def scenario_side_folds(page, url, secret):
    select_source(page, url, secret, "tone")
    before = page.locator("canvas.wf-canvas").bounding_box()["width"]
    page.locator(".pane.jobs .pane-head").click()
    expect(page.locator("section.pane.jobs")).to_have_attribute("data-collapsed", "")
    assert view_value(page, "side") == "transcript"
    page.locator(".pane.transcript .pane-head").click()
    assert view_value(page, "side") == "none"
    after = page.locator("canvas.wf-canvas").bounding_box()["width"]
    assert after - before >= 200, (before, after)
    run_cmd(page, "view set side both")
    expect(page.locator("section.pane[data-collapsed]")).to_have_count(0)
    expect(page.locator(".pane.jobs h2 .count")).to_have_text(re.compile(r"^\d+$"))


def scenario_job_row_two_lines(page, url, secret):
    select_source(page, url, secret, "tone")
    cur = source_info(url, secret, "tone")["current"]["center_hz"]
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
    select_source(page, url, secret, "cb")
    plans = http_json(url, secret, "/op", {"op": "channel_plans"})["value"]
    plan = next(p for p in plans if p["id"] == "uhf_cb_au")
    jobs = [start_job(url, secret, "passthrough", "cb", ch["center_hz"], 12_500) for ch in plan["channels"][:9]]
    try:
        page.evaluate("window.zetaTest.foldIdle(0)")  # idle threshold 0 ms for the test
        page.evaluate("window.zetaTest.poll()")
        group = page.locator('#jobs details.job-group[data-plan="uhf_cb_au"]')
        expect(group).to_have_count(1)
        expect(group.locator("summary")).to_have_text(re.compile(r"^9 idle passthrough on uhf_cb_au"))
        assert group.get_attribute("open") is not None
        for j in jobs:
            expect(group.locator(f'.job[data-id="{j}"]')).to_have_count(1)
        stop_job(url, secret, jobs[0]); stop_job(url, secret, jobs[1])
        page.evaluate("window.zetaTest.poll()")
        expect(group).to_have_count(0)
    finally:
        for j in jobs[2:]:
            stop_job(url, secret, j)
```

(`passthrough` has no squelch; the fold rule treats a kind without squelch as closed. The `zetaTest.foldIdle(ms)` hook overrides `JOBS_FOLD_IDLE_MS` for the page's lifetime.)

- [ ] **Step 2: Run them to verify they fail**

Run: `ZETA_UI_ONLY=side_folds,job_row_two_lines,job_rows_fold_idle_plan_channels cargo test -p zeta --test ui`
Expected: FAIL.

- [ ] **Step 3: The `side` policy and the headers**

`view.js`: `{ key: "side", values: ["both", "transcript", "jobs", "none"], default_: "both", describe: "both, transcript, jobs, none" }`. `index.html`: each `h2` wraps `<button class="pane-head" type="button" aria-expanded="true">Transcript</button>` (and `Jobs <span class="count"></span>`). `app.js` `applySide()`: from the value, set or remove `data-collapsed=""` on `.pane.transcript` and `.pane.jobs`, `aria-expanded`, and `document.documentElement.dataset.side = value`; the buttons compute the next value (`both`→ click jobs → `transcript`; `transcript` → click transcript → `none`; etc.) and call `state.view.set("side", next)`. CSS: `section.pane[data-collapsed] > :not(h2) { display: none }`, `html[data-side="none"] #app { grid-template-columns: minmax(0,1fr) var(--side-collapsed-w) }`, the other pane `flex: 1` when its sibling is collapsed. Narrow footprint: `.pane-head { pointer-events: none }` under `html[data-footprint="narrow"]`.

- [ ] **Step 4: Two lines**

CSS (both themes): `.job { display: grid; grid-template-columns: 1fr auto; grid-template-areas: "l1 actions" "l2 actions"; column-gap: 8px; overflow: hidden; }` with `.kind, .state { grid-area: l1 }` … implemented as two wrapper-free lines by `grid-row` assignments: `.kind, .state { grid-row: 1 }`, `.band, .source, .events, .mix-now, .pos, .reason, .id { grid-row: 2 }`, `.actions { grid-area: actions; align-self: center }`. Since CSS grid places each span in its own cell, use `display: flex; flex-wrap: wrap` instead if the builder finds grid rows clumsy: line 1 = `.kind .state`, then a forced break (`.state::after { content: ""; flex-basis: 100% }`), line 2 = the rest with `.id { margin-inline-start: auto }`. Either way: `.job > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0 }`, `.job .id { flex: 0 0 auto }`. `agent.js` `jobRow`: every text span `span.dir = "auto"`, `span.title = text` for `.band`, `.source`, `.events`, `.reason`. Console: `.job { background: var(--c-channel-n) via [data-colour]; color: #000; border-radius: 0 var(--radius-bar) var(--radius-bar) 0 }`, `.job.finished { background: var(--c-idle); color: #c8c8c8 }`.

- [ ] **Step 5: The fold and the count**

`agent.js` `startJobs` render: after building rows, group running rows whose `job.tuning` lies on a plan channel (`channelLabel(...)` non-null), whose squelch is closed or whose kind has none, and whose last event age (the `.events` counter's last change, tracked in a `Map<jobId, lastEventMs>` the renderer already has or now keeps) exceeds `JOBS_FOLD_IDLE_MS`; attached rows (`job.input`) of a folded job fold with it. When a plan's group has `>= JOBS_FOLD_MIN` rows, wrap them in `details.job-group[data-plan]` (`open` on first creation; the operator's toggle stored in `sessionStorage["zeta.jobfold.<plan>"]`), `summary.textContent = `${n} idle ${kind} on ${plan}` + (m ? `, ${m} transcribe` : "")`. Set `.pane.jobs h2 .count` to the running-job count. `window.zetaTest.foldIdle = (ms) => { foldIdleMs = ms }`.

- [ ] **Step 6: Raise the `view show` assertion**

`ui.py:1128` → `== 13`; add `"side both"` to the tuple.

- [ ] **Step 7: Run the gates**

Run: the gate with `ZETA_UI_ONLY=side_folds,job_row_two_lines,job_rows_fold_idle_plan_channels,view_set_show_reset,tags_show_kind_id_and_link_rows` plus every scenario whose name contains `job` or `plan_start` or `mix`.
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/zeta-ui
git commit -m "UI: the side column: the chat log and the jobs fold on their header bars (view set side), job rows on two lines with no sideways scroll and a live count, idle plan channels folded into one group row"
```

---

### Task 6: The graph modal skinned (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/graph.js:236-262` (node drawing: the headline line, `rx`)
- Modify: `crates/zeta-ui/web/style.css:466-515`
- Test: `crates/zeta-ui/tests/e2e/ui.py` (new `scenario_graph_headline`)

**Interfaces:**
- Produces: `text.graph-node-headline` under each node's kind; `HEADLINE_PARAMS` in `graph.js`: `{ channel: ["center_hz", "bandwidth_hz"], nbfm_demod: ["deviation_hz", "squelch_db"], wfm_demod: ["deviation_hz"], am_demod: ["squelch_db"], ssb_demod: ["sideband"], transcribe: ["language"], fft: ["size", "window"], detect: ["threshold_db", "energy_db"], measure: ["bandwidth_hz"], burst_capture: ["bursts", "window_ms"], sink_sondehub: ["callsign"], mix: ["sources"], stream_in: ["job"], external_decode: ["mode"], passthrough: ["kind"] }`; family map in CSS by `data-kind`.

- [ ] **Step 1: Write the failing scenario**

```python
def scenario_graph_headline(page, url, secret):
    select_source(page, url, secret, "tone")
    cur = source_info(url, secret, "tone")["current"]["center_hz"]
    job = start_job(url, secret, "fm_demod", "tone", cur, 12_500, deviation_hz=5000)
    try:
        job_row(page, job).locator(".graph").click()
        node = page.locator('#graph svg .graph-node[data-kind="channel"]')
        expect(node.locator("text.graph-node-headline")).to_contain_text("center_hz")
        expect(node.locator("text.graph-node-headline")).to_contain_text(str(cur))
        rx = page.evaluate("el => el.querySelector('rect').getAttribute('rx')", node.element_handle())
        assert rx not in (None, "0"), rx
    finally:
        page.keyboard.press("Escape")
        stop_job(url, secret, job)
```

- [ ] **Step 2: Run it to verify it fails**

Run: `ZETA_UI_ONLY=graph_headline cargo test -p zeta --test ui`
Expected: FAIL, no headline.

- [ ] **Step 3: The headline and the skin**

`graph.js`: `NODE_H` 46 → 58; after the `nid` text, `svgEl("text", { x: p.x + 10, y: p.y + 50, class: "graph-node-headline" }, headline(n))` where `headline(n)` joins up to two `key value` pairs from `HEADLINE_PARAMS[n.kind]` present in `n.params` (numbers as sent, strings as sent, arrays as `[n]`), truncated to 26 characters with `…`; `rx` from `getComputedStyle(document.documentElement).getPropertyValue("--radius-bar")` parsed, else 4. CSS: `#graph .graph-node rect` fill by family: `[data-kind="channel"], [data-kind="fft"], [data-kind="measure"], [data-kind="detect"]` → `var(--c-channel-1)`; the four demods → `var(--c-frame)`; `denoise, vad, mix, stream_in` → `var(--c-channel-3)`; `aprs_decode, pocsag_decode, rds_decode, acars_decode, adsb_decode, external_decode, transcribe, burst_capture` → `var(--c-action)`; `sink_sondehub` → `var(--c-channel-4)`; `passthrough` and the default → `var(--c-idle)`; all under `[data-theme="console"]` with `text { fill: #000 }` and `text.nid, text.graph-node-headline { fill: rgba(0,0,0,.7) }`; the two `ui-monospace` rules → `var(--font-ui)`; edges `stroke: var(--c-frame)`, edge text `fill: var(--c-channel-5)`. Plain: the fills stay `var(--bar)`.

- [ ] **Step 4: Run the gates**

Run: the gate with `ZETA_UI_ONLY=graph_headline,graph_view_renders_nodes,graph_edit_puts`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui
git commit -m "UI: the graph modal skinned: nodes as rounded bars coloured by stage family with a params headline under the kind; click-to-edit unchanged"
```

---

### Task 7: The narrow footprint, the whole suite, TODO and UAT (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/style.css` (narrow footprint block, `:400-420`)
- Modify: `crates/zeta-ui/web/overlays.js` (narrow: a tapped thin tag expands for 3 s)
- Modify: `TODO.md`, `UAT.md`
- Test: the whole `ui.py`

- [ ] **Step 1: Narrow footprint**

CSS under `html[data-footprint="narrow"][data-theme="console"]`: `--rail-w: 14px`, `.job-tag { height: var(--rail-thin-px) } .job-tag > * { display: none }` unless `.job-tag.expanded`, which is `position: absolute; inset-inline-start: 14px; z-index: 6; height: auto; width: max-content` over the axis with its children shown. `overlays.js`: on `pointerdown` of a tag when `document.documentElement.dataset.footprint === "narrow"`, add `expanded` for 3 s (`RAIL_EXPAND_MS = 3000`), without blocking the long press. Run `ZETA_UI_ONLY=narrow_hides_pane,narrow_answer_is_ribbon,narrow_long_press_opens_the_outline_menu,subjects_narrow_shows_three_columns cargo test -p zeta --test ui`.

- [ ] **Step 2: The whole suite, timed**

Run: `time cargo test -p zeta --test ui 2>&1 | tail -30`
Expected: PASS; note the wall-clock seconds. If over 520 s, trim waits in the nine new scenarios first (`wait_for_timeout(300)` → poll `style.top` with `wait_for_function`), rerun, and report the number either way.

- [ ] **Step 3: Franchise words**

Run: `git grep -n -i -e <word1> -e <word2> -e <word3> -- . ':!docs/superpowers/plans/2026-11-03-console-theme.md'` with the words commit `22db49e` removed from `TODO.md` (`git show 22db49e -- TODO.md | grep '^-'` shows them).
Expected: no output.

- [ ] **Step 4: TODO and UAT**

`TODO.md`: remove "UI: console restyle" and "UI: the job panel hides the id behind a horizontal scroll" (evidence: Tasks 1-6's scenarios); add gists "UI: `view set dir ltr|rtl|auto`" and "UI: `view set time_flow left|right`" (spec 4.9 mechanics), "UI: view policies in localStorage" (the theme and side would survive a new tab), "UI: a lane budget per footprint for ribbons" if not already in the rewritten staircase gist; re-point "UI: graph builder" wording if an entry names the old modal. `UAT.md`: append a "Plan 24: the console theme" section with the eight operator checks of spec 6 as unticked boxes, naming the deploy binary `plan24-zeta` and the two `view set theme` commands.

- [ ] **Step 5: Gates and commit**

Run: `cargo fmt -p zeta-ui -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test -p zeta-ui`
Expected: PASS.

```bash
git add crates/zeta-ui TODO.md UAT.md
git commit -m "UI: the narrow footprint's rail (thin segments, a tap expands one); the whole ui suite green under the console theme at <N> s; TODO tidied (restyle and job-panel gists shipped, dir and time_flow gists added); UAT plan 24 checks listed"
```

---

## Rulings on the spec

1. **Spec 5 says nine new scenarios; this plan has eleven** (`source_bar_shows_live_numbers` and `job_rows_fold_idle_plan_channels` added) because the source bar and the fold are behaviour the operator will see and nothing else pins them. The budget line in Task 7 covers them.
2. **Spec 4.5's `top` is the ribbon's centre line here**, not its top edge, since the transform does the `-50%`; the e2e's step assertion is on `style.top` differences, which are the same.
3. **Spec 4.7's fold rule treats a kind without a squelch as closed** so `passthrough` and the decoders fold when idle; the spec only named `fm_demod`.
4. **Spec 4.3's frame spine is `#main` padding**, not a rail grid column, per the builder check (the rail lives in `#waterfall`).
5. **The font is the variable TTF when `woff2_compress` is absent**; the spec assumed two woff2 files.
