// map.js: the map pane (plan 17 spec 7.2): where the decoded events that carry a `position`
// ({lat, lon, alt_m}, from a mode row's `position` map) were heard from.
//
// Exports:
//   TRACK_POINTS   fixes kept per job track (the oldest dropped first)
//   SUBJECT_TRACK_POINTS   the points of a subject's track when its row names no
//                  `subject.track_points`
//   OSM_TILES, OSM_ATTRIBUTION   the only tile layer: OpenStreetMap's, with its attribution;
//                  a tile that fails (offline, or the e2e suite aborting the host) leaves its
//                  square blank
//   distanceBearing(home, pos) -> {km, deg}
//       Great-circle distance (haversine, mean Earth radius) and initial bearing (degrees
//       clockwise from true north, 0 <= deg < 360) from `home` to `pos`, both {lat, lon} in
//       decimal degrees.
//   positionOf(payload) -> {lat, lon, alt_m} | null   an event's `position`, when it has finite
//       lat and lon
//   distanceM(a, b) -> metres between two {lat, lon, alt_m}, the altitudes counted where both
//       are known
//   addToTrack(kept, fix, {cap, minM}) -> kept points after `fix` ({lat, lon, alt_m, heading},
//       unknowns null): it joins when at least `minM` from the last kept point, or when its
//       heading has turned more than TURN_DEG from that point's; at `cap` the result is thinned,
//       so a track never holds `cap` points after this call
//   thinTrack(points) -> points less every other one of the older half, the first kept, the
//       newer half whole: a track keeps its start and old detail coarsens with age
//   drawnPoints(kept, latest) -> [[lat, lon]]   the line: the kept points, ending at the latest
//       fix (`cap` at most, as `kept` is under it)
//   mountMap(el, api, view, {jobs, subjects}) -> MapPane
//       `el` (#map) is shown while view.get("main") is "map" and hidden otherwise (view.onChange;
//       `invalidateSize` on each show).
//       Leaflet (vendor/leaflet, served by the node) is loaded the first time the map shows,
//       so a page that never shows it never loads it. `jobs()` returns the last `job_list`
//       result, `subjects` the store (subjects.js). `api` is the node's client (unused so far:
//       every fix arrives by event).
//   MapPane:
//     map.event(job, payload)   one event of a job the overlays follow (overlays.js onEvent:
//                               the selected source's watched jobs and the positioned jobs on
//                               every other source): one with a `position` is that job's
//                               newest fix, whichever source the page shows, or the newest
//                               of its subject's when the store has merged the event into
//                               one (call the store first). Fixes are kept while the map is
//                               hidden, so the track is whole when it shows.
//     map.sync()                after each jobs poll: the tracks of jobs no longer running
//                               (stopped, failed, forgotten) are dropped with their marker
//     map.rowText(jobId) -> "<km, 1 dp> km, <deg rounded>°" | null
//                               the job's last fix from the home (view.js homeOf: view `home`,
//                               else the node's station; null without a home or
//                               a fix), for its jobs pane row (agent.js). A job with subjects
//                               answers for its one subject on the map; with more it is null,
//                               since the table has them.
//     map.destroy()
//   Drawn, once Leaflet is in: one L.circleMarker (class `map-pos`) at each job's newest fix,
//   whose popup is the last event's `summary` and its age (`12 s ago`, from when this page
//   received it), and one non-interactive L.polyline (class `map-track`) through its fixes;
//   a non-interactive circle marker (class `map-home`) at the home (homeOf). The view is centred
//   once, on the first fix or on home (whichever comes first), at zoom FIRST_ZOOM, and never
//   moved again: after that the user pans.
//   A subject with a fix is drawn instead as a track under `${job}\u0000${key}`: one L.marker
//   holding `div.map-subject` (classes `dim`, `selected`) with `span.arrow` turned to the row's
//   heading (class `dot` while none is known) and `span.label`, the subject's label and altitude,
//   and one polyline as above through the fixes addToTrack kept, by the row's
//   `subject.track_points` and `track_min_m`. The marker is always at the latest fix, redrawn at
//   most twice a second per subject. An event
//   with no fix refreshes the marker's face and leaves the geometry alone. Its popup names the
//   subject and lists its values; a click selects it in the store. The store's selection marks
//   the marker and raises it over the others, panning only when it is off the map; each of the
//   store's ticks dims it with its subject, being heard again lights it, and a drop removes it.

import { subjectLabel, subjectValues } from "./subjects.js";
import { metricText } from "./units.js";
import { homeOf } from "./view.js";

const SUBJECT_DRAW_MS = 500;
export const TRACK_POINTS = 3600;
export const SUBJECT_TRACK_POINTS = 100;
const TURN_DEG = 10;
// Over any marker a screen's height of latitude would otherwise put in front.
const SELECTED_Z = 10000;
export const OSM_TILES = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
export const OSM_ATTRIBUTION =
  '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors';
const FIRST_ZOOM = 10;
const LEAFLET_JS = "vendor/leaflet/leaflet.js";
const LEAFLET_CSS = "vendor/leaflet/leaflet.css";
const EARTH_KM = 6371.0088;

const rad = (d) => (d * Math.PI) / 180;
const deg = (r) => (r * 180) / Math.PI;

export function distanceBearing(home, pos) {
  const p1 = rad(home.lat);
  const p2 = rad(pos.lat);
  const dp = p2 - p1;
  const dl = rad(pos.lon - home.lon);
  const h = Math.sin(dp / 2) ** 2 + Math.cos(p1) * Math.cos(p2) * Math.sin(dl / 2) ** 2;
  const km = 2 * EARTH_KM * Math.asin(Math.min(1, Math.sqrt(h)));
  const y = Math.sin(dl) * Math.cos(p2);
  const x = Math.cos(p1) * Math.sin(p2) - Math.sin(p1) * Math.cos(p2) * Math.cos(dl);
  return { km, deg: (deg(Math.atan2(y, x)) + 360) % 360 };
}

export function positionOf(p) {
  const pos = p && typeof p === "object" ? p.position : null;
  if (!pos || typeof pos !== "object") return null;
  const { lat, lon } = pos;
  if (!Number.isFinite(lat) || !Number.isFinite(lon)) return null;
  return { lat, lon, alt_m: Number.isFinite(pos.alt_m) ? pos.alt_m : null };
}

export function distanceM(a, b) {
  const flat = distanceBearing(a, b).km * 1000;
  const known = Number.isFinite(a.alt_m) && Number.isFinite(b.alt_m);
  return known ? Math.hypot(flat, a.alt_m - b.alt_m) : flat;
}

// The angle between two headings, 0-180 degrees; 0 while either is unknown.
function turned(a, b) {
  if (!Number.isFinite(a) || !Number.isFinite(b)) return 0;
  return Math.abs((((a - b) % 360) + 540) % 360 - 180);
}

export function thinTrack(points) {
  const older = Math.floor(points.length / 2);
  return points.filter((_, i) => i >= older || i % 2 === 0);
}

export function addToTrack(kept, fix, { cap, minM }) {
  const last = kept[kept.length - 1];
  if (last && distanceM(last, fix) < minM && turned(last.heading, fix.heading) <= TURN_DEG) {
    return kept;
  }
  const next = [...kept, fix];
  return next.length >= cap ? thinTrack(next) : next;
}

export function drawnPoints(kept, latest) {
  const line = kept.map((p) => [p.lat, p.lon]);
  const last = kept[kept.length - 1];
  if (!last || last.lat !== latest.lat || last.lon !== latest.lon) {
    line.push([latest.lat, latest.lon]);
  }
  return line;
}

// Leaflet's stylesheet and script, added once; resolves to `L`.
let leaflet = null;

function loadLeaflet() {
  if (window.L) return Promise.resolve(window.L);
  if (!leaflet) {
    leaflet = new Promise((resolve, reject) => {
      const css = document.createElement("link");
      css.rel = "stylesheet";
      css.href = LEAFLET_CSS;
      const js = document.createElement("script");
      js.src = LEAFLET_JS;
      let loaded = 0;
      const done = () => {
        loaded += 1;
        if (loaded === 2) resolve(window.L);
      };
      const fail = () => {
        leaflet = null;
        css.remove();
        js.remove();
        reject(new Error("the map library did not load"));
      };
      css.onload = done;
      js.onload = done;
      css.onerror = fail;
      js.onerror = fail;
      document.head.append(css, js);
    });
  }
  return leaflet;
}

function ageText(ms) {
  return `${Math.max(0, Math.round(ms / 1000))} s ago`;
}

// A subject's popup, as it stands when opened: its name, its values and its age.
function subjectPopup(s) {
  const box = document.createElement("div");
  const label = subjectLabel(s);
  const name = document.createElement("div");
  name.textContent = label === s.key ? label : `${label} (${s.key})`;
  box.append(name);
  for (const v of subjectValues(s)) {
    if (v.text === "") continue;
    const line = document.createElement("div");
    line.textContent = `${v.label}: ${v.text}`;
    box.append(line);
  }
  const age = document.createElement("div");
  age.className = "age";
  age.textContent = ageText(Date.now() - s.last_seen);
  box.append(age);
  return box;
}

export function mountMap(el, api, view, opts = {}) {
  return new MapPane(el, api, view, opts);
}

class MapPane {
  constructor(el, api, view, { jobs = () => [], subjects = null } = {}) {
    this.el = el;
    this.api = api;
    this.view = view;
    this.jobs = jobs;
    this.subjects = subjects;
    // job id -> {points: [[lat, lon]], summary, at, marker, line}
    // `${job id}\u0000${key}` -> {job, key, subject, kept, points (the line, as drawnPoints),
    //   marker, line, box, arrow, label}
    this.tracks = new Map();
    this.map = null; // the Leaflet map, once loaded and shown
    this.homeMarker = null;
    this.centred = false;
    this.loading = false;
    this.destroyed = false;
    this.ro = new ResizeObserver(() => this.map?.invalidateSize());
    this.ro.observe(el);
    view.onChange((key) => {
      if (key === "main") this.show();
      else if (key === "home") this.drawHome();
    });
    subjects?.onTick(() => this.markDim());
    subjects?.onSelect((sel) => this.markSelected(sel));
    subjects?.onDrop((jobId, key) => this.dropSubject(jobId, key));
    this.show();
  }

  show() {
    if (this.destroyed) return;
    const on = this.view.get("main") === "map";
    this.el.hidden = !on;
    if (!on) return;
    if (this.map) {
      this.map.invalidateSize();
      return;
    }
    if (this.loading) return;
    this.loading = true;
    loadLeaflet()
      .then((L) => this.create(L))
      .catch((e) => {
        this.el.textContent = e.message;
      })
      .finally(() => {
        this.loading = false;
      });
  }

  create(L) {
    if (this.destroyed || this.map) return;
    this.L = L;
    this.map = L.map(this.el, { worldCopyJump: true });
    L.tileLayer(OSM_TILES, { attribution: OSM_ATTRIBUTION, maxZoom: 19 }).addTo(this.map);
    const first = this.tracks.values().next().value;
    const home = homeOf(this.view);
    if (first) this.centre(first.points[0]);
    else if (home) this.centre([home.lat, home.lon]);
    else this.map.setView([0, 0], 2);
    this.drawHome();
    for (const id of this.tracks.keys()) this.draw(id);
  }

  centre(latlng) {
    if (this.centred || !this.map) return;
    this.centred = true;
    this.map.setView(latlng, FIRST_ZOOM);
  }

  drawHome() {
    if (!this.map) return;
    const home = homeOf(this.view);
    if (!home) {
      this.homeMarker?.remove();
      this.homeMarker = null;
      return;
    }
    const at = [home.lat, home.lon];
    if (this.homeMarker) this.homeMarker.setLatLng(at);
    else {
      this.homeMarker = this.L.circleMarker(at, {
        radius: 5,
        className: "map-home",
        interactive: false,
      }).addTo(this.map);
    }
    this.centre(at);
  }

  event(job, p) {
    if (this.destroyed) return;
    const subject = this.subjects?.subjectOf(job, p);
    if (subject) {
      this.subjectEvent(subject, p);
      return;
    }
    const pos = positionOf(p);
    if (!pos) return;
    let t = this.tracks.get(job.id);
    if (!t) {
      t = { points: [], summary: "", at: 0, marker: null, line: null };
      this.tracks.set(job.id, t);
    }
    t.points.push([pos.lat, pos.lon]);
    if (t.points.length > TRACK_POINTS) t.points.splice(0, t.points.length - TRACK_POINTS);
    t.summary = typeof p.summary === "string" ? p.summary : "";
    t.at = Date.now();
    this.draw(job.id);
  }

  subjectEvent(s, p) {
    const id = `${s.job}\u0000${s.key}`;
    let t = this.tracks.get(id);
    const pos = positionOf(p);
    if (!pos) {
      if (t?.marker) {
        // Being heard lights it at once; what it says waits for the draw.
        t.box.classList.toggle("dim", s.dim);
        this.scheduleDraw(id, t);
      }
      return;
    }
    if (!t) {
      t = { job: s.job, key: s.key, subject: s, kept: [], points: [], marker: null, line: null };
      this.tracks.set(id, t);
    }
    const sub = s.row.subject;
    const heading = sub.heading ? s.state[sub.heading] : null;
    const fix = { ...pos, heading: Number.isFinite(heading) ? heading : null };
    t.kept = addToTrack(t.kept, fix, {
      cap: sub.track_points ?? SUBJECT_TRACK_POINTS,
      minM: sub.track_min_m ?? 0,
    });
    t.points = drawnPoints(t.kept, fix);
    t.moved = true;
    t.box?.classList.toggle("dim", s.dim);
    this.scheduleDraw(id, t);
  }

  // A subject's marker and line at most every SUBJECT_DRAW_MS: the first message at once, the
  // last of a burst from a timer. Only a fix moves them; other messages redraw the face.
  scheduleDraw(id, t) {
    if (t.drawTimer !== undefined) return;
    const flush = () => {
      t.drawTimer = undefined;
      if (this.destroyed || this.tracks.get(id) !== t) return;
      t.drawnAt = Date.now();
      if (t.moved || !t.marker) {
        t.moved = false;
        this.draw(id);
      } else {
        this.drawFace(t);
      }
    };
    const wait = (t.drawnAt ?? 0) + SUBJECT_DRAW_MS - Date.now();
    if (wait <= 0) flush();
    else t.drawTimer = setTimeout(flush, wait);
  }

  drawSubject(t) {
    const last = t.points[t.points.length - 1];
    if (!t.marker) {
      t.box = document.createElement("div");
      t.box.className = "map-subject";
      t.arrow = document.createElement("span");
      t.label = document.createElement("span");
      t.label.className = "label";
      t.box.append(t.arrow, t.label);
      t.line = this.L.polyline(t.points, { className: "map-track", interactive: false }).addTo(
        this.map,
      );
      // No iconSize: `div.map-subject` sizes itself, its arrow on the fix (style.css).
      const icon = this.L.divIcon({ html: t.box, className: "map-subject-icon", iconSize: null });
      t.marker = this.L.marker(last, { icon }).addTo(this.map);
      t.marker.bindPopup(() => subjectPopup(t.subject));
      t.marker.on("click", () => this.subjects.select(t.job, t.key));
      const sel = this.subjects.selected();
      this.markOne(t, !!sel && sel.job === t.job && sel.key === t.key);
    } else {
      t.line.setLatLngs(t.points);
      t.marker.setLatLng(last);
    }
    this.drawFace(t);
    this.centre(t.points[0]);
  }

  // What the marker says: its heading, its label, whether it is dim.
  drawFace(t) {
    const s = t.subject;
    const name = s.row.subject.heading;
    const heading = name ? s.state[name] : null;
    const known = Number.isFinite(heading);
    t.arrow.className = known ? "arrow" : "arrow dot";
    t.arrow.style.transform = known ? `rotate(${heading}deg)` : "";
    const alt = s.position ? s.position.alt_m : null;
    t.label.textContent =
      alt === null ? subjectLabel(s) : `${subjectLabel(s)} ${metricText(alt, "m")}`;
    t.box.classList.toggle("dim", s.dim);
  }

  markOne(t, on) {
    t.box.classList.toggle("selected", on);
    t.marker.setZIndexOffset(on ? SELECTED_Z : 0);
  }

  // The tracks of subjects (a job's own entry has no `key`).
  subjectTracks() {
    return [...this.tracks.values()].filter((t) => t.key !== undefined);
  }

  markDim() {
    for (const t of this.subjectTracks()) t.box?.classList.toggle("dim", t.subject.dim);
  }

  markSelected(sel) {
    for (const t of this.subjectTracks()) {
      const on = !!sel && sel.job === t.job && sel.key === t.key;
      if (t.marker) this.markOne(t, on);
      if (!on || !this.map || this.el.hidden) continue;
      const at = t.points[t.points.length - 1];
      if (!this.map.getBounds().contains(at)) this.map.panTo(at);
    }
  }

  dropSubject(jobId, key) {
    const id = `${jobId}\u0000${key}`;
    const t = this.tracks.get(id);
    if (!t) return;
    t.marker?.remove();
    t.line?.remove();
    this.tracks.delete(id);
  }

  draw(id) {
    const t = this.tracks.get(id);
    if (!this.map || !t) return;
    if (t.key !== undefined) {
      this.drawSubject(t);
      return;
    }
    const last = t.points[t.points.length - 1];
    if (!t.marker) {
      t.line = this.L.polyline(t.points, { className: "map-track", interactive: false }).addTo(
        this.map,
      );
      t.marker = this.L.circleMarker(last, { radius: 6, className: "map-pos" }).addTo(this.map);
      t.marker.bindPopup(() => {
        const box = document.createElement("div");
        const summary = document.createElement("div");
        summary.textContent = t.summary || "(no summary)";
        const age = document.createElement("div");
        age.className = "age";
        age.textContent = ageText(Date.now() - t.at);
        box.append(summary, age);
        return box;
      });
    } else {
      t.line.setLatLngs(t.points);
      t.marker.setLatLng(last);
    }
    this.centre(t.points[0]);
  }

  sync() {
    const running = new Set(
      (this.jobs() || [])
        .filter((j) => j.state && j.state.status === "running")
        .map((j) => j.id),
    );
    for (const [id, t] of [...this.tracks]) {
      if (running.has(t.job ?? id)) continue;
      t.marker?.remove();
      t.line?.remove();
      this.tracks.delete(id);
    }
  }

  rowText(id) {
    const home = homeOf(this.view);
    if (!home) return null;
    let t = this.tracks.get(id);
    if (!t) {
      const mine = this.subjectTracks().filter((s) => s.job === id);
      if (mine.length !== 1) return null;
      t = mine[0];
    }
    const [lat, lon] = t.points[t.points.length - 1];
    const { km, deg: b } = distanceBearing(home, { lat, lon });
    return `${km.toFixed(1)} km, ${Math.round(b) % 360}°`;
  }

  destroy() {
    this.destroyed = true;
    this.ro.disconnect();
    this.map?.remove();
    this.map = null;
    this.tracks.clear();
    this.el.hidden = true;
  }
}
