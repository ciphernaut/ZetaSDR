# Plan 26: themes as folders (the built-ins and the operator's own, three new looks)

Design spec, 2026-11-03. Brainstormed in chat with the operator; answers marked "operator" are
theirs, the rest are the author's calls, listed in section 10. Plan 25 is the parked public release
(branch `worktree-public-release-plan`); this plan is numbered after it and does not depend on it.

Wording rule for this plan (operator): no franchise names, marks or licensed wording anywhere in the
code, docs, theme names or commit messages. The console theme's look is described by its parts: the
elbowed frame, the pill bars with dark text, the condensed face.

## 1. Intent and why

ZetaSDR will ship to other people, and they should be able to pick a look that suits them, and make
their own (operator). Today the page has two themes, `console` and `plain`, chosen with
`view set theme`. Their values live in `crates/zeta-ui/web/style.css`: every colour, font and size is
already a custom property, but about a hundred selectors are written for `[data-theme="console"]`
only, the rail is switched on by `theme === "console"` in `app.js`, and the waterfall's colour ramp is
a constant in `waterfall.js`.

The operator does not want recolours of the two current themes (operator). A theme must be able to
change the page's shape and texture, and none of the new themes may carry the console look: the
elbowed frame, the pill bars with dark text and the condensed face are the console's alone
(operator). What the themes share is the layout: where the regions sit, the rail beside the
frequency axis, ribbons riding the waterfall (operator: "layout, sure it will be similar").

So themes become data. A theme is a folder with a declarative `theme.json` and a `style.css`; the
built-in themes ship in that format inside the binary, and an operator's themes folder, read like
`--modes-dir`, adds or replaces them (operator). Three new built-ins ship with this plan: cyberpunk,
terminal and psytrance (operator chose these three from five; the looks are the author's,
section 6).

## 2. Goals, non-goals, anti-goals

### Goals

- **One mechanism.** Console, plain and the three new themes are theme folders embedded in the
  binary, loaded by the same loader as an operator's. An operator theme with a built-in's `id`
  replaces it, as a mode row does.
- **An operator themes folder** (operator): `--themes-dir PATH`, default
  `$XDG_CONFIG_HOME/zetasdr/themes`, else `~/.config/zetasdr/themes`; a missing directory is no
  error. Read once at startup.
- **Custom CSS** (operator): a theme's `style.css` can restyle anything the page's hooks expose. It
  is not limited to tokens.
- **A neutral core.** `style.css` in the page keeps layout and neutral defaults only. Everything that
  makes the console look like the console moves into the console theme's own `style.css`. Rebuilding
  console in the theme format is the proof the hooks are complete.
- **No visible change to console or plain.** After the move both render as they do on main today
  (checked by screenshot comparison, section 8).
- **Three new looks** that each change shape or texture as well as colour, none reading as the
  console (section 6).
- **A documented hooks contract** (section 5): the selectors and attributes a theme may rely on,
  kept stable across releases and pinned by a test.
- **Canvas parts are themeable** through tokens the JS reads (section 4.3): the waterfall ramp and
  ground, the axis, the selection, the ribbons.
- **Broken themes never stop the node**, reported and listed like broken mode rows.
- **Room for layout variants later** (operator: mobile and other device layouts are not built now,
  but the design must cater for them). The footprint (`<html data-footprint>`, today `wide` or
  `narrow`, chosen by viewport width in `app.js`) stays a separate axis from the theme: a theme
  styles every footprint, a footprint never picks a theme. Section 4.6 says what this plan does
  so a later plan can add footprints without changing the theme format.

### Non-goals

- A theme editor or picker UI beyond `view set theme` (the command line's completion lists the ready
  themes).
- Hot reload of the themes folder; a restart of the node picks up changes.
- Theming the map's tiles beyond what CSS can do to them (a theme's CSS may put a `filter` on the
  map's tile layer through its hook; the tiles stay OpenStreetMap's).
- JavaScript in themes. A theme is CSS, tokens, a font and images.
- Right-to-left and waterfall-direction policies (their `TODO.md` entries stay as they are).
- Moving `view set` policies to localStorage (its `TODO.md` entry stays).
- Steampunk and neon city (operator: cut).
- Mobile, tablet or other device-specific layouts (operator: later; section 4.6 keeps the slot).
- A node-level default theme (`--theme` or a marker in the themes folder). Operator: whoever wants a
  theme installs it into their own installation; the theme stays a per-tab view policy with console
  as the default, and remembering it across tabs is the existing localStorage entry in `TODO.md`.
- Sharing or distributing themes between installations.

### Anti-goals (failure even if it technically works)

- A new theme that reads as the console: elbows, pill-shaped bars with dark text on coloured fills,
  the Antonio face, the console's warm palette on black.
- A new theme that is the plain theme with other colours.
- A theme able to load anything from outside the node, or run script.
- A theme that makes a control unreadable or unreachable at the default zoom: contrast under WCAG AA
  for body text, or an animation that ignores `prefers-reduced-motion`.
- Console or plain changing appearance as a side effect of the move.

## 3. Constraints

- The page is served by `crates/zeta-ws/src/ui.rs` with
  `default-src 'self'; script-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: https://tile.openstreetmap.org; ...`.
  Theme files are served from the node's own origin, so they need no CSP change, and a theme's
  `url(https://...)` is blocked by the browser. Keep it that way: do not widen the CSP for themes.
- Built-in assets are embedded with `include_bytes!` in `crates/zeta-ui/src/lib.rs`, as the Antonio
  font is. The new themes' fonts are vendored the same way with their licence text beside them
  (`vendor/<font>/OFL.txt`); only SIL OFL or equally permissive fonts.
- The project rules apply to `theme.json`: every field is required unless marked optional here, and
  unknown fields are rejected.
- `--themes-dir` belongs to the serving node: with `--node` it is a usage error (exit 1), like
  `--modes-dir`.
- Browser floor (author's call): Chrome 120, Firefox 117, Safari 17.2, the first versions with
  native CSS nesting of bare type selectors. The page already needs `color-mix()` (Chrome 111,
  Firefox 113, Safari 16.2), so this moves the floor by months, not years. Stated in the README and
  `docs/themes.md`.
- The e2e suite (`crates/zeta-ui/tests/e2e/ui.py`) uses `neon` as its example of an unknown theme;
  no theme in this plan is called `neon`, so that assertion stays.

## 4. The theme format

### 4.1 The folder

```
themes/<id>/
  theme.json      required
  style.css       optional
  <files>         optional: fonts (woff2, ttf, otf) and images (png, svg, webp) style.css names
```

The folder name must equal `theme.json`'s `id`. `id` is lowercase `[a-z][a-z0-9_-]{0,31}`.

### 4.2 `theme.json`

```json
{
  "id": "terminal",
  "name": "Terminal",
  "description": "Phosphor green on black, boxed panes, a block cursor.",
  "color_scheme": "dark",
  "layout": { "rail": true, "ribbon_tilt_deg": 0 },
  "tokens": { "--bg": "#020602", "--text": "#7dff7a", "...": "..." },
  "ramp": [[0, [0, 6, 0]], [0.4, [0, 120, 30]], [0.8, [120, 255, 120]], [1, [235, 255, 235]]],
  "fonts": [{ "family": "Terminal Mono", "file": "Mono.woff2", "weight": "400 700" }]
}
```

- `name`: shown in `view set theme`'s completion and in `themes list`. `description`: one line.
- `color_scheme`: `dark` or `light`, set as `color-scheme` on `<html>`.
- `layout`: the switches the JS honours. `rail` turns the rail beside the axis on or off (replacing
  the `theme === "console"` test in `app.js:1004`). `ribbon_tilt_deg`, -30..=30, the ribbons' lean.
  The builder may add switches the JS needs (section 9), each documented with the hooks.
- `tokens`: values for the documented custom properties (section 4.3). A key outside the documented
  list is refused naming it. A value is a CSS value string of at most 200 characters; one containing
  `;`, `{`, `}`, `<` or `url(` is refused (URLs belong in `style.css`, relative to the folder).
  A token the theme leaves out takes the neutral core's value.
- `ramp`: the waterfall's colour stops, 2-16 of `[t, [r, g, b]]`, `t` ascending from 0 to 1 exactly,
  channels 0-255. Replaces `RAMP_STOPS` in `waterfall.js`.
- `fonts`: optional; each names a file in the folder, becomes an `@font-face` under the theme, and
  is used through the `--font-ui`/`--font-mono` tokens.

### 4.3 Tokens

The documented list is today's custom properties in `style.css`'s `:root` (colours, the channel
colours, `--wf-bg`, `--wf-select`, tints, map colours, fonts, radii, bar heights, `--axis-w`,
`--side-w`, `--rail-w`) plus the canvas values the builder lifts out of the JS: every colour, font
and line width `waterfall.js` and `overlays.js` draw with that is hard-coded today (axis text and
ticks, grid, the DC marker, outlines, ribbon ground and text). The list lives in `docs/themes.md`
with one line per token saying what it paints; the loader checks `tokens` against it.

### 4.4 `style.css`

At most 256 KiB. The node serves it wrapped so that it applies only while its theme is active: the
author's call is native CSS nesting, `[data-theme="<id>"] { <file> }`, with the file's
`@font-face` and `@keyframes` rules hoisted out of the wrapper (keyframe names prefixed with the
theme id so two themes cannot clash). `@import` is refused. `url()` resolves inside the theme's
folder; a path leaving it is refused.

The wrapper changes what a theme's CSS selects: a rule on `<html>` itself (its `data-`
attributes) must be written with `&`, for example `&[data-footprint="narrow"] #rail { ... }`; a
bare `html[...]` or `:root` selector inside the wrapper matches nothing. The console's narrow
rules (`style.css` around line 873) are rewritten that way when they move. `docs/themes.md` says
so with an example. Lifting `@font-face` and `@keyframes` out, renaming keyframes and their
`animation` references, and refusing `@import` and escaping `url()` needs a small CSS tokeniser in
the node (comments, strings, blocks), not a full parser; a crate is the builder's choice.

### 4.5 Loading, state and reporting

- The built-ins are read first, then `--themes-dir`. A folder that does not parse or validate is
  `broken`: logged at startup as `zeta: theme <id> (<path>): <field>: <error>`, listed with
  `state: broken` and `reason`, never stopping the node. A broken folder meant to replace a built-in
  leaves the built-in in force.
- A new read-only op, `themes_list` (CLI `zeta themes list`), answers each theme's `id`, `name`,
  `description`, `builtin`, `replaces_builtin`, `file`, `state` (`ready` or `broken`) and `reason`,
  sorted by id, following `modes_list`.
- The node serves a ready theme's files under `/ui/themes/<id>/` (`theme.json`, the wrapped
  `style.css`, its fonts and images; anything else 404), in the router of `crates/zeta-ws/src/ui.rs`.
  They are part of the page, so they are served without the bearer and with the page's CSP, as the
  page's own assets under `/ui/` are.
- The page fetches `themes_list` at load, offers every `ready` theme to `view set theme`, injects the
  active theme's `style.css` and tokens, applies `layout`, and rebuilds the waterfall ramp and canvas
  colours on a switch (`refreshThemeColours` grows to cover them). An open graph view redraws on a
  switch (fixing the plan 24 nit).
- A stored theme that is no longer ready falls back to `console`, the default, and the transcript
  says so once.

### 4.6 Footprints, now and later

- `data-footprint` is in the hooks contract (section 5). A theme targets a footprint with
  `&[data-footprint="<name>"]` in its `style.css`; the built-ins carry rules for `narrow`.
- A theme must render acceptably in a footprint it has no rules for (it falls back to the core's
  layout for it), so a later footprint (`phone`, `tablet`, a kiosk) does not break existing themes.
  Themes and the docs must not assume there are only two footprints.
- `layout` in `theme.json` is footprint-independent in this plan. The reserved extension is an
  optional `footprints` object, `{"narrow": {"rail": false}}`, overriding `layout` keys per
  footprint; the loader refuses the key today (unknown fields are rejected) and a later plan adds
  it without changing anything else. The page applies `layout` through one function that a later
  plan can make footprint-aware.
- How a footprint is chosen (width, `?footprint=`, a future `view set footprint`) stays in `app.js`
  and out of the theme format.

## 5. The hooks contract

`docs/themes.md` lists what a theme may target and the project keeps stable: the regions (top bar,
brand, source tabs, rail, frequency axis, waterfall, side column and its panes, pane strip, command
bar, map, graph view, menus), the job row and job tag parts, ribbons, outlines, and the state
attributes (selected, speaking, hot, idle, squelch open, failed). Where today's markup has no stable
hook for one of these, the builder adds a class or `data-` attribute. Everything not in the list may
change in any release. An e2e scenario asserts every listed hook exists on a page with a running job,
a ribbon and an open pane, so renaming one fails the suite.

## 6. The three new looks (author's call)

Each must pass the anti-goals: not the console's frame, bars or face, not plain recoloured, AA
contrast for body text, every animation off under `prefers-reduced-motion`. Each has its own font
pair (OFL) chosen by the builder within the direction below.

### 6.1 Cyberpunk (`cyberpunk`)

Near-black ground with a faint blue cast; magenta and electric cyan as the two signal colours, acid
yellow for warnings. Shape: chamfered corners (one corner cut at 45 degrees with `clip-path`) on
panes, buttons and job tags instead of rounding; thin 1 px neon outlines with a soft glow, never solid
coloured fills behind text. Texture: a faint scanline overlay on the bars, a brief glitch offset
(two-colour split) on a job tag when it starts speaking. Type: a squared, wide techno display face
for labels and headings, a monospace for numbers and decoded text. Waterfall ramp: deep violet,
magenta, cyan, white. Rail on, ribbons lean slightly.

### 6.2 Terminal (`terminal`)

A text-mode screen: black ground, phosphor green text, amber for warnings, inverse video (green
ground, black text) for selection instead of colour fills. Shape: square corners everywhere; panes
drawn as boxes with a single-line border and a title set into the top edge (`[ JOBS ]`); bars are
text rows, not blocks. Texture: CRT scanlines and a slight text glow, a blinking block cursor in the
command bar. Type: one monospace face throughout, no uppercase transforms beyond pane titles. Waterfall
ramp: black through dark and bright green to near-white. Rail on, ribbons flat (tilt 0).

### 6.3 Psytrance (`psytrance`)

Blacklight at a festival: deep indigo ground, fluorescent pink, lime, orange and UV cyan as the
channel colours. Shape: soft organic panels with generous, uneven radii (not pills, and never dark text
on a bright fill), borders drawn as gradients through the channel colours. Texture: a slow hue drift
on the brand and the speaking glow, a faint radial pattern in the side column's ground kept under the
contrast floor. Type: a rounded geometric face for labels, a clean sans for decoded text. Waterfall
ramp: a rainbow ramp from indigo to white, checked for readable weak signals (the low end must stay
dark and distinct from the floor). Rail on, ribbons lean the other way from cyberpunk.

## 7. Moving console and plain

- `style.css` keeps layout, neutral defaults (plain's values) and the token defaults. Every
  `[data-theme="console"]` rule, the Antonio `@font-face`, the frame spine, the pill bars, the
  uppercase label transforms, the graph node fills, moves to `themes/console/style.css`, with its
  tokens in `themes/console/theme.json`.
- Plain becomes `themes/plain/` with an empty or near-empty `style.css`.
- The built-in theme folders live in the source tree at `crates/zeta-ui/themes/<id>/` (author's
  call) and are embedded at build time.
- The initial `data-theme="console"` in `index.html` stays as the no-JS default. The one-frame flash
  of console when another theme is stored is in scope only if it falls out of the loading work.

## 8. Testing

- Rust: the loader's validation, one test per refusal (unknown token key, bad value characters, bad
  ramp, `@import`, path escape, id mismatch, oversized CSS), built-in replacement, a broken
  replacement keeping the built-in, `--themes-dir` with `--node` refused.
- E2E: `view set theme` for each built-in; an operator theme from a fixture folder appears and
  applies; a broken one is listed and not offered; the hooks scenario (section 5); a switch rebuilds
  the waterfall ramp (sample a canvas pixel); `prefers-reduced-motion` stops the new themes'
  animations.
- Screenshot comparison of console and plain before and after the move on the same fixture, at the
  e2e suite's desktop and narrow sizes; any difference is a bug or is explained in the plan's report.
- Contrast: a check (script or test) that each built-in's `--text`, `--dim` and decoded-text colours
  meet AA against their grounds.

## 9. Left to the builder

- The exact token list (section 4.3) after auditing the JS for hard-coded drawing values.
- Further `layout` switches, if a look needs the JS to change behaviour (each documented).
- How the page injects tokens and CSS (a `<link>` per theme or one swapped element), within the CSP.
- The font choices within each look's direction, OFL only.
- Whether `themes_list` also reaches the agent's tool list (it is harmless; the default is yes, as
  every op is).
- Docs: `docs/themes.md` (format, tokens, hooks, a worked example copying `terminal`), the README,
  `AGENTS.md` and its generated skill for the new flag and op, `RECIPES.md`, `UAT.md`, `TODO.md`.

## 10. The author's calls (check these)

1. The plan number 26 and the folder layout `crates/zeta-ui/themes/<id>/`.
2. The three looks in section 6, including the theme ids `cyberpunk`, `terminal`, `psytrance` and
   the name "Terminal" (operator).
3. Themes are read once at startup; no hot reload.
4. Tokens may not contain `url(`; images and fonts are referenced from `style.css` only.
5. Scoping `style.css` with native CSS nesting, hoisting `@font-face` and `@keyframes`.
6. Theme files served without the bearer, like the page's own assets.
7. A stored theme that is no longer ready falls back to console.
8. A new op `themes_list` rather than an HTTP-only listing.
9. A 256 KiB cap on `style.css` and a 200-character cap on a token value.
10. The no-visible-change rule for console and plain, checked by screenshots.
11. The browser floor Chrome 120 / Firefox 117 / Safari 17.2 (section 3), so the node wraps CSS
    instead of rewriting selectors.
12. The reserved `footprints` key in `theme.json` for later per-footprint layout (section 4.6).
