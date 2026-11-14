// selection.js: the page's one selection (plan 37 spec 3.5), shared by the map, the Subjects
// pane and the waterfall. app.js makes one per login and hands it to the subjects store
// (subjects.js), the map (map.js) and the Subjects pane; nothing else holds selection state.
//
// A selection is null or one of:
//   {kind: "subject", job, group, key}   a subject of the store: `job` the job it was first
//       heard on, `group` its group's id (a watch's for a kept decoder), `key` its key
//   {kind: "site", provider, siteId, name, lat, lon, set, holders}   a register site lit on the
//       map: `provider` and `siteId` its best origin's allocation's (`siteId` null from a
//       provider without ids: such a site is selected and marked, never asked about), `name`
//       the allocation's `site`, `lat`/`lon` the marker's position, `set` the key of the lit
//       set it was clicked in, `holders` the distinct licensees of the site's origins
//
// Exports:
//   Selection
//     sel.get() -> selection | null
//     sel.set(selection | null)   replaces it and calls every onChange callback, even when the
//         new one equals the old (a subject selected again pans the map to it)
//     sel.onChange(cb(selection | null))
//   isSite(sel) -> bool   a site selection
//   sameSite(a, b) -> bool   both site selections of one site: provider, id and position equal

export class Selection {
  constructor() {
    this.sel = null;
    this.cbs = [];
  }

  get() {
    return this.sel;
  }

  set(sel) {
    this.sel = sel ?? null;
    for (const cb of this.cbs) cb(this.sel);
  }

  onChange(cb) {
    this.cbs.push(cb);
  }
}

export function isSite(sel) {
  return !!sel && sel.kind === "site";
}

export function sameSite(a, b) {
  return (
    isSite(a) &&
    isSite(b) &&
    a.provider === b.provider &&
    a.siteId === b.siteId &&
    a.lat === b.lat &&
    a.lon === b.lon
  );
}
