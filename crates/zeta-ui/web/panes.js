// panes.js: the pane tab strip beside the source tabs (plan 19 spec 5.7).
//
// One tab per registered pane (Subjects; Explore and the Bits panes register later), one pane
// shown at a time in #pane under the waterfall, none when none is selected. The selection is
// the view's `pane` policy (view.js): `none` or a registered pane's id.
//
// Exports:
//   paneIds() -> [id]   the ids registered with the mounted strip (view.js checks `view set
//       pane` against them; [] before one is mounted)
//   mountPanes(el, view, {footprint, host, root}) -> Panes
//       el         the strip (<nav id="panes" role="tablist">)
//       host       the pane area (#pane), shown while a pane is
//       footprint  () -> "wide" | "narrow"
//     panes.register(id, label, {eligible, el, onShow, onHide})
//         a tab `button.pane-tab[role=tab][data-pane=<id>]` named `label`; `eligible()` says
//         whether the tab shows unselected, `el` is the pane's element in #pane (shown only
//         while it is the pane on screen), `onShow()` runs when it comes on screen and
//         `onHide()` just before it goes. A registered id may be registered again (replaced).
//     panes.unregister(id)
//     panes.select(id | null)    the view's `pane` set to `id` (`none` for null)
//     panes.current() -> id | null   the selected pane (null when none or not registered)
//     panes.render()   tabs, #pane and the panes' elements from the view and eligibility; run
//         on a view change, a jobs poll, a footprint change
//     panes.destroy()
//
// A tab shows while its pane is eligible or selected, `aria-selected` on the selected one. A
// click on the selected tab deselects (`pane none`). <html data-strip="<id>"> is set while a
// pane is selected in the wide footprint (style.css gives it the bottom of #main). In the
// narrow footprint a pane is shown in the main area's place (over the waterfall or the map)
// only while <html data-pane> names it: a click on its tab selects it and sets data-pane, a
// click on the one shown clears both.

let live = null;

export function paneIds() {
  return live ? [...live.panes.keys()] : [];
}

export function mountPanes(el, view, opts = {}) {
  const panes = new Panes(el, view, opts);
  live = panes;
  return panes;
}

class Panes {
  constructor(el, view, { footprint = () => "wide", host = null, root = document.documentElement } = {}) {
    this.el = el;
    this.view = view;
    this.footprint = footprint;
    this.host = host;
    this.root = root;
    // id -> {tab, eligible, el, onShow, onHide, shown}
    this.panes = new Map();
    this.onClick = (ev) => {
      const tab = ev.target.closest("button.pane-tab");
      if (tab) this.click(tab.dataset.pane);
    };
    el.addEventListener("click", this.onClick);
  }

  register(id, label, { eligible = () => false, el = null, onShow = null, onHide = null } = {}) {
    this.unregister(id);
    const tab = document.createElement("button");
    tab.type = "button";
    tab.className = "pane-tab";
    tab.setAttribute("role", "tab");
    tab.dataset.pane = id;
    tab.textContent = label;
    tab.hidden = true;
    tab.setAttribute("aria-selected", "false");
    this.el.append(tab);
    this.panes.set(id, { tab, eligible, el, onShow, onHide, shown: false });
    this.render();
  }

  unregister(id) {
    const p = this.panes.get(id);
    if (!p) return;
    if (p.shown && p.onHide) p.onHide();
    if (p.el) p.el.hidden = true;
    p.tab.remove();
    this.panes.delete(id);
  }

  current() {
    const id = this.view.get("pane");
    return this.panes.has(id) ? id : null;
  }

  select(id) {
    this.view.set("pane", id || "none");
  }

  click(id) {
    const narrow = this.footprint() === "narrow";
    if (narrow) {
      // data-pane first, so the render the view's change runs sees it.
      if (this.root.dataset.pane === id) {
        delete this.root.dataset.pane;
        this.select(null);
      } else {
        this.root.dataset.pane = id;
        this.select(id);
      }
    } else {
      this.select(this.current() === id ? null : id);
    }
    this.render();
  }

  render() {
    const sel = this.current();
    const narrow = this.footprint() === "narrow";
    const root = this.root;
    // Narrow: a pane tab in the waterfall's place that is no longer selected goes.
    if (narrow && this.panes.has(root.dataset.pane) && root.dataset.pane !== sel) {
      delete root.dataset.pane;
    }
    if (sel && !narrow) root.dataset.strip = sel;
    else delete root.dataset.strip;
    const onScreen = narrow ? (sel && root.dataset.pane === sel ? sel : null) : sel;
    const showing = [];
    for (const [id, p] of this.panes) {
      const selected = id === sel;
      p.tab.hidden = !(selected || p.eligible());
      if (p.tab.getAttribute("aria-selected") !== String(selected)) {
        p.tab.setAttribute("aria-selected", String(selected));
      }
      const shown = id === onScreen;
      if (!shown && p.shown && p.onHide) p.onHide();
      if (p.el) p.el.hidden = !selected;
      if (shown && !p.shown) showing.push(p);
      p.shown = shown;
    }
    if (this.host) this.host.hidden = !onScreen;
    for (const p of showing) if (p.onShow) p.onShow();
  }

  destroy() {
    for (const id of [...this.panes.keys()]) this.unregister(id);
    this.el.removeEventListener("click", this.onClick);
    if (this.host) this.host.hidden = true;
    delete this.root.dataset.strip;
    if (live === this) live = null;
  }
}
