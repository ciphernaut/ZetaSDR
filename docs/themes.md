# Themes

A theme is a folder. The page offers every ready theme to `view set theme NAME`; `console` is
the default and the fallback. The built-in themes live under `themes/` in the repository;
yours go in the operator folder and can add a theme or replace a built-in one by using its `id`.

The six built-in themes:

| id | name | look |
|---|---|---|
| `console` | Console | black ground, an elbowed frame, pill bars with dark text and a condensed face (the default) |
| `plain` | Plain | dark slate panels, hairline borders, a monospace face and no frame |
| `cyberpunk` | Cyberpunk | near-black ground, magenta and cyan neon outlines, chamfered corners and a techno face |
| `terminal` | Terminal | phosphor green on black, boxed panes with titles in the border, a block cursor |
| `psytrance` | Psytrance | fluorescent pink, lime, orange and UV cyan on deep indigo, soft uneven panels with gradient borders |
| `vibecoder` | Vibecoder | the one light theme: white rounded cards on cool slate, an indigo-to-violet accent, the waterfall a dark card |

Browser floor: Chrome 120, Firefox 117, Safari 17.2 (the first versions with native CSS nesting
of bare type selectors, which the wrapper below relies on; the page already needs `color-mix()`).

## Where themes live

- Built in: `themes/<id>/` in the repository, embedded in the node.
- Operator folder: `--themes-dir PATH`, default `$XDG_CONFIG_HOME/zetasdr/themes`, else
  `~/.config/zetasdr/themes`; a missing directory is no error. It is read after the built-ins and
  belongs to the serving node (a usage error with `--node`).
- A folder that does not validate never stops the node. It is listed `broken` by
  `zeta themes list` (`state`, `reason`) and reported at startup as
  `zeta: theme <id> (<path>): <field>: <error>`. A broken folder that meant to replace a built-in
  theme leaves that theme in force.
- A stored theme that is no longer ready falls back to `console`; the transcript says so once.

## The folder

```
themes/<id>/
  theme.json      required
  style.css       optional
  <files>         optional: fonts and images that style.css or theme.json name
```

The folder name must equal `theme.json`'s `id`. The node serves a ready theme under
`/ui/themes/<id>/` without the bearer, with the page's content security policy, `nosniff` and
`Cache-Control: no-cache`. The served file types, by extension: `json`, `css`, `woff2`, `woff`,
`ttf`, `otf`, `png`, `svg`, `webp`, `jpg`, `jpeg`, `txt`, `md`; any other file is not served. A
theme without a `style.css` still serves one (its fonts, colour scheme, ribbon tilt and tokens).
The content security policy is the page's and is not widened for themes: a `url(https://...)`
is blocked by the browser and refused by the loader, and fonts load from the theme's own folder
only. The project's rule for the fonts of built-in themes is SIL OFL 1.1 with the licence text beside
them; the loader does not enforce it, so check the licence of any font you ship in your own.

## `theme.json`

Every field is required except `fonts`. Unknown fields are refused naming the field, `footprints`
among them (it is reserved, see Footprints).

| field | rule |
|---|---|
| `id` | `^[a-z][a-z0-9_-]{0,31}$`, equal to the folder name |
| `name` | shown in `themes list`; a string |
| `description` | one line |
| `color_scheme` | `dark` or `light`, set as `color-scheme` on `<html>` |
| `layout.rail` | `true` puts the job tags in the rail beside the axis (`<html data-rail="on">`), `false` on their outlines (`"off"`) |
| `layout.ribbon_tilt_deg` | number, -30 to 30 inclusive; the lean of transcript ribbons (the wrapper emits `--ribbon-tilt` from it, so `--ribbon-tilt` is not a token) |
| `layout.dock` | optional, `bottom` (when absent) or `right`: where the selected pane docks while the view's `dock` is `auto`, under the waterfall or map, or as a third section of the side column (`<html data-dock>`); any other value is refused naming `layout.dock` |
| `layout.map_tiles` | optional, one of `osm`, `osm-dark`, `opentopo`, `esri-street`, `esri-imagery` (keyless providers only; `osm-dark` is OpenStreetMap's tiles under a dark filter on the tile layer alone): the map's tile provider while the view's `map_tiles` is `auto` (`<html data-map-tiles>`); absent, a dark `color_scheme` gets `osm-dark` and a light one `osm`; any other value is refused naming `layout.map_tiles` |
| `tokens` | an object of custom properties from the list below; an unknown key is refused naming it |
| `ramp` | the waterfall's colour stops: 2 to 16 of `[t, [r, g, b]]`, `t` strictly ascending, the first exactly 0 and the last exactly 1, channels 0-255 |
| `fonts` | optional; each `{family, file, weight}` names a file in the folder and becomes an `@font-face` under the theme (`weight` is a CSS range such as `400 700`); use the family through `--font-ui` and `--font-mono`. `family` and `weight` may not contain a quote or the characters refused in token values |

```json
{
  "id": "terminal",
  "name": "Terminal",
  "description": "Phosphor green on black, boxed panes, a block cursor.",
  "color_scheme": "dark",
  "layout": { "rail": true, "ribbon_tilt_deg": 0 },
  "tokens": { "--bg": "#020602", "--text": "#7dff7a" },
  "ramp": [[0, [0, 6, 0]], [0.4, [0, 120, 30]], [0.8, [120, 255, 120]], [1, [235, 255, 235]]],
  "fonts": [{ "family": "Terminal Mono", "file": "Mono.woff2", "weight": "400 700" }]
}
```

### Token values

A token the theme leaves out takes the core stylesheet's value. A value is a CSS value string
with no length cap. It is refused when it contains `;`, `{`, `}` or `<`, or `url(` in any spelling
(case-insensitive, or with a CSS escape such as `u\72l(`); a raw NUL character; a stray `\`; or
an unclosed comment, string or bracket. URLs belong in `style.css`, relative to the folder.

### The tokens

Each token is a custom property the core stylesheet declares in `:root`.

- `--bg`: page ground.
- `--bar`: bars and panels.
- `--border`: hairlines.
- `--accent`: links, selection, the active job.
- `--text`: body text.
- `--dim`: captions and ids.
- `--error`: failures.
- `--ok`: open, running, speaking.
- `--user`: operator input and events.
- `--c-frame`: the console frame.
- `--c-source`: the source bar.
- `--c-action`: action buttons.
- `--c-channel-1`: per-channel colour 1, cycled by channel.
- `--c-channel-2`: per-channel colour 2.
- `--c-channel-3`: per-channel colour 3.
- `--c-channel-4`: per-channel colour 4.
- `--c-channel-5`: per-channel colour 5.
- `--c-channel-6`: per-channel colour 6.
- `--c-idle`: a quiet channel.
- `--c-speaking`: a channel being heard.
- `--wf-bg`: the waterfall canvas ground; any opaque CSS colour (`#hex`, `rgb()`, `hsl()`, a name, `color-mix()`); one the browser cannot parse falls back to `#000030`.
- `--wf-select`: the drag rectangle's colour; any opaque CSS colour, `#ffffff` when it does not parse.
- `--wf-select-alpha`: the drag rectangle's fill opacity.
- `--overlay-bg`: labels over the waterfall.
- `--tint-span`: a capture's cached region.
- `--tint-outline`: a job outline's fill.
- `--tint-speaking`: the fill of a speaking outline.
- `--label-bg`: map label ground.
- `--default-mark`: the default radio's dot on its tab.
- `--default-mark-size`: that dot's diameter.
- `--map-mark`: map position and track.
- `--map-sel`: map selection and home.
- `--map-dim`: map popup age.
- `--map-label-text`: map label text.
- `--scrim`: behind the graph view.
- `--shadow`: menus and the drawer.
- `--shadow-strong`: the graph panel.
- `--font-ui`: the interface face.
- `--font-mono`: the monospace face.
- `--rail-w`: the frequency-aligned rail's width.
- `--rail-thin-px`: the rail's thin form.
- `--radius-bar`: buttons and bars.
- `--radius-elbow`: the frame's elbows.
- `--axis-w`: the waterfall's frequency axis.
- `--side-w`: the side column.
- `--side-collapsed-w`: the side column folded.
- `--dock-share`: the docked pane's share of the side column while the dock is `right` (default 40%).
- `--bar-h-top`: the top bar's height.
- `--bar-h-bottom`: the command bar's height.
- `--bar-h`: the current bar height (defaults to `--bar-h-top`).

## `style.css`

At most 1 MiB (1 048 576 bytes); a larger file is refused as `style.css: <n> bytes, more than the
1 MiB limit`. Also refused:

- `@import`;
- a `url()` that is absolute (starts with `/`), has a scheme other than `data:`, contains a `..`
  segment, or names a file that is not in the theme folder. `data:` URLs pass untouched;
- `url(#fragment)`, which names no file;
- an unclosed comment, string or bracket, a closing bracket without its opener, a raw NUL.

The node serves the file wrapped so that it applies only while its theme is active:

1. one `@font-face` per entry of `theme.json`'s `fonts`;
2. the file's own top-level `@font-face` rules, unchanged;
3. the file's top-level `@keyframes` rules, hoisted out of the wrapper and renamed
   `<id>--<name>` (the `animation` and `animation-name` values that use them are renamed to
   match), so two themes cannot clash;
4. `[data-theme="<id>"] { ... }` holding the colour scheme, `--ribbon-tilt`, each token and the
   rest of the file, scoped by native CSS nesting.

`@font-face` and `@keyframes` are hoisted only at the file's top level: nested in `@media` they
stay inside the wrapper, where browsers drop them. `@property`, `@counter-style` and `@charset`
are not supported (they would end up inside the wrapper rule). An `animation` that names a
keyframe through `var()` is not renamed.

Write selectors as you would for the page; the wrapper adds the scope. A rule about `<html>`
itself (its `data-` attributes) must be written with `&`: a bare `html[...]` or `:root` inside the
wrapper matches nothing. The narrow footprint's rail, for example:

```css
&[data-footprint="narrow"] #rail { inline-size: 14px; }
```

`&[...]` adds the condition to `<html>` without a type selector. When the `html` type selector's
specificity matters (to win over a core rule written `html[...]`), write `html&[...]`:

```css
html&[data-footprint="narrow"] #rail { inline-size: 14px; }
```

Put every animation under `@media (prefers-reduced-motion: no-preference)`; a theme must be
still for a reader who asks for that.

## Footprints

`<html data-footprint>` is `wide` or `narrow` today. Do not assume there are only two: a theme
must render acceptably in a footprint it has no rules for (it falls back to the core's layout
for it), so a later footprint does not break it. `layout` in `theme.json` is footprint-independent
now; an optional `footprints` object that overrides `layout` per footprint is reserved, and the
loader refuses the key today.

## The hooks

These are what a theme may target; the project keeps them stable. Everything else in the markup
may change in any release. The built-in themes are the exception: they also use internal
selectors (`#bar .prompt`, `html[data-side]`, `.thin`, `.expanded`, `#wf-badge`, `#band`,
`.job .actions`, `#ctx`, `#ctx-outline`, `.ctx-item`) that are not promised stable and are
fixed along with the page when it changes; a theme of your own should keep to the hooks below. The
core's menu rules are written with ID selectors, so a theme that styles menus must write
`#ctx, #ctx-outline` (a bare `[role="menu"]` loses to them). An end-to-end scenario (`scenario_theme_hooks`) checks that each
selector below matches on a page with a running demodulator, a ribbon, a collapsed pane, a pane
docked right, the map and the graph view open, so renaming one fails the suite.

Regions:

- `#top`: the top bar. `.brand`: the product name in it.
- `.src-tab`: a source tab (a button); `.src-tab[aria-selected="true"]` is the selected one.
  Its parts are `.name`, `.range`, `.centre` and `.health`, and `.starting` (after `.range`)
  while the source's worker measures its link at a start: a note, not a fault, so keep it
  visible on every tab. `.map-tab` is the Map tab after the
  source tabs (also a `.src-tab`, with `.name` and a `.count`); while it is selected the shown
  source's tab carries `.kept`.
- `.src-tab.default`: the tab of the default radio for the agent (the shown radio under the
  `focus` and `focus-fallback` policies, none under `free-first` and `ask`), on the RF view and
  the Map alike. `.src-tab .default-mark`: the dot first in every source tab, shown only under
  `.src-tab.default`.
- `#src-menu`: the hover box under a source tab whose source has a worker (a fixed `div`,
  `hidden` otherwise), holding the source's own actions; its one entry is `.link-diag`, the
  "Run link diagnostics" button.
- `#rail`: the frequency-aligned rail beside the axis (present while `data-rail` is `on`).
- `.wf-axis`: the frequency axis. `#waterfall`: the waterfall region. `.wf-canvas`: its canvas.
- `#side`: the side column. `section.pane` is a pane in it (`#pane-transcript`, `#pane-jobs`);
  `.pane-head` is its header button; `section.pane[data-collapsed]` is a folded one.
  `section.pane.dock` (`#pane-dock`) is the third, between the two, present only while a pane is
  selected and the dock is `right`: its `.pane-head` names the pane (with a `.count` for
  Subjects: the subjects the running jobs hold, counted before merged groups fold, so it can be
  more than the table's rows) and its body is the pane host `#pane`; its share of the column is `--dock-share`.
- `#panes`: the pane strip (the tabs for the subjects and explore views).
- `#bar`: the command bar.

With only the binary and no checkout, fetch the starting files from a running node instead:
`/ui/themes/plain/theme.json` and `/ui/themes/plain/style.css` (no bearer needed). The
`style.css` comes back wrapped in the theme's scope, so start from `theme.json` and an empty
`style.css`. `#cmd`: its input. `#mic`: the microphone button.
- `#map`: the map, in the waterfall's place while the Map tab is selected. `#graph`: the graph
  view (a modal dialog).
- `[role="menu"]`: a menu (`#ctx`, the box menu, and `#ctx-outline`, the outline menu); its entries
  are `.ctx-item`.

Jobs:

- `.job`: a job row in the jobs pane. Its parts include `.job .kind` and `.job .band` (also
  `.source`, `.state`, `.id`). `.job[data-state="running"]` and `.job[data-state="failed"]` carry
  the job's state; a finished row also has `.finished`. A running scan's row also has
  `.sweep-ms` (the last sweep's time) and `select.scan-look` (the look: fast, averaged, long
  look, and coarse and fast where the source has a lossy depth).
- `.job-tag`: a job's tag in the rail (or on its outline). Its parts are `.job-tag .kind`,
  `.short`, `.job-tag .channel`, `.job-tag .centre`, a `.dot` and a `.snippet`.
- `.outline`: a job's band on the waterfall (`.outline-label` and `.outline-handle` inside).
- `.ov-ribbon`: a ribbon on the waterfall, with a protocol class (`.aprs`, `.transcript`, ...).

State hooks (classes and attributes the page toggles; style them, never depend on their timing):

- `.hot`: the job under the pointer, on `.job-tag`, `.outline` and `.job`; `.job-tag.hot` is the
  tag's.
- `.speaking`: a channel being heard, on `.job-tag`, `.outline` and `.job`;
  `.job-tag.speaking` is the tag's.
- `.job-tag .dot.open` and `.job-tag .dot.closed`: the squelch open or closed. A closed one is
  also the idle channel; its job row carries `.tag.squelch`. There is no `.idle` or
  `[data-squelch]` hook; use these.
- `.graph-node.selected`: the selected node in the graph view (`#subjects tr.subject.selected`
  and `#map .map-subject.selected` are the subjects table's and the map's).
- `.job[data-state="failed"]`: a failed job (there is no `.failed` class).

On `<html>`: `html[data-theme]` (the active theme's `id`), `html[data-rail]` (`on` or `off`, from
`layout.rail`), `html[data-footprint]` (`wide` or `narrow`), `html[data-main]` (`rf` or `map`:
what the main area shows, the waterfall or the map; the view's `main`) and `html[data-dock]`
(`bottom` or `right`: where the selected pane docks, the view's `dock` resolved through
`layout.dock`; set in every footprint, though the narrow one ignores it) and `html[data-map-tiles]` (the map's tile provider: the view's `map_tiles` resolved through `layout.map_tiles` and `color_scheme`). `#map .map-tile-note` is the map's notice that its tiles are not loading, naming another provider to try.

## Worked example

Copy `plain` to the operator folder, give it a new id and change three tokens and one rule:

```
mkdir -p ~/.config/zetasdr/themes
cp -r themes/plain ~/.config/zetasdr/themes/mine
```

In `~/.config/zetasdr/themes/mine/theme.json` set `"id": "mine"` and `"name": "Mine"`, and change
three tokens:

```json
"--accent": "#ffb454",
"--text": "#e8e2d4",
"--radius-bar": "8px"
```

Then put one rule in `mine/style.css` (the file is empty in `plain`):

```css
.job-tag.speaking { outline: 2px solid var(--accent); }
```

Restart the node with `--themes-dir ~/.config/zetasdr/themes` (or leave it at its default),
check that `zeta themes list` shows `mine` as `ready`, and run `view set theme mine` in the
command bar.
