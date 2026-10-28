// map.js: the map pane (plan 17 spec 7.2): where the decoded events that carry a `position`
// ({lat, lon, alt_m}, from a mode row's `position` map) were heard from.
//
// Exports:
//   TRACK_POINTS   fixes kept per job track (the oldest dropped first)
//   OSM_TILES, OSM_ATTRIBUTION   the only tile layer: OpenStreetMap's, with its attribution;
//                  a tile that fails (offline, or the e2e suite aborting the host) leaves its
//                  square blank
//   distanceBearing(home, pos) -> {km, deg}
//       Great-circle distance (haversine, mean Earth radius) and initial bearing (degrees
//       clockwise from true north, 0 <= deg < 360) from `home` to `pos`, both {lat, lon} in
//       decimal degrees.
//   positionOf(payload) -> {lat, lon, alt_m} | null   an event's `position`, when it has finite
//       lat and lon
//   mountMap(el, api, view, {jobs}) -> MapPane
//       `el` (#map) is shown while view.get("map") is "on" and hidden otherwise (view.onChange).
//       Leaflet (vendor/leaflet, served by the node) is loaded the first time the map shows,
//       so a page that never shows it never loads it. `jobs()` returns the last `job_list`
//       result. `api` is the node's client (unused so far: every fix arrives by event).
//   MapPane:
//     map.event(job, payload)   one event of a watched job (overlays.js onEvent): one with a
//                               `position` is that job's newest fix. Fixes are kept while the
//                               map is hidden, so the track is whole when it shows.
//     map.sync()                after each jobs poll: the tracks of jobs no longer running
//                               (stopped, failed, forgotten) are dropped with their marker
//     map.rowText(jobId) -> "<km, 1 dp> km, <deg rounded>°" | null
//                               the job's last fix from view `home` (null without a home or
//                               a fix), for its jobs pane row (agent.js)
//     map.destroy()
//   Drawn, once Leaflet is in: one L.circleMarker (class `map-pos`) at each job's newest fix,
//   whose popup is the last event's `summary` and its age (`12 s ago`, from when this page
//   received it), and one non-interactive L.polyline (class `map-track`) through its fixes;
//   a non-interactive circle marker (class `map-home`) at view `home`. The view is centred
//   once, on the first fix or on home (whichever comes first), at zoom FIRST_ZOOM, and never
//   moved again: after that the user pans.

import { homePoint } from "./view.js";

export const TRACK_POINTS = 3600;
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

export function mountMap(el, api, view, opts = {}) {
  return new MapPane(el, api, view, opts);
}

class MapPane {
  constructor(el, api, view, { jobs = () => [] } = {}) {
    this.el = el;
    this.api = api;
    this.view = view;
    this.jobs = jobs;
    this.tracks = new Map(); // job id -> {points: [[lat, lon]], summary, at, marker, line}
    this.map = null; // the Leaflet map, once loaded and shown
    this.homeMarker = null;
    this.centred = false;
    this.loading = false;
    this.destroyed = false;
    this.ro = new ResizeObserver(() => this.map?.invalidateSize());
    this.ro.observe(el);
    view.onChange((key) => {
      if (key === "map") this.show();
      else if (key === "home") this.drawHome();
    });
    this.show();
  }

  show() {
    if (this.destroyed) return;
    const on = this.view.get("map") === "on";
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
    const home = homePoint(this.view.get("home"));
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
    const home = homePoint(this.view.get("home"));
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

  draw(id) {
    const t = this.tracks.get(id);
    if (!this.map || !t) return;
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
      if (running.has(id)) continue;
      t.marker?.remove();
      t.line?.remove();
      this.tracks.delete(id);
    }
  }

  rowText(id) {
    const home = homePoint(this.view.get("home"));
    const t = this.tracks.get(id);
    if (!home || !t) return null;
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
