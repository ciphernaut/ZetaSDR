#!/usr/bin/env python3
"""Run demodulators only on the channels of a plan that are in use.

Watches a source's capture with one elastic `spectrum` job. A plan channel whose peak rises
`--open-db` dB over the row's noise floor gets an `fm_demod` plus an attached `transcribe`;
a channel idle for `--idle-s` seconds loses both. Standard library only; talks to a node's
`POST /op` with the shared secret read from the environment variable named by `--secret-env`.

    export ZETA_SECRET=...
    python3 -I cb_watch.py --node http://127.0.0.1:8080 --secret-env ZETA_SECRET --source b210

`--dry-run` prints the plan channels inside the source's capture and starts nothing. The jobs
this script starts are ordinary persisted jobs: it stops them on SIGINT/SIGTERM, and a restart
adopts running `fm_demod` jobs already on plan channels (it times them out, it does not stop
them on exit). A demodulator started mid-transmission seeds its automatic squelch floor on the
signal; it settles when the channel next goes quiet.
"""

import argparse
import json
import signal
import sys
import time
import urllib.error
import urllib.request

CHANNEL_MATCH_HZ = 1_000


class OpError(Exception):
    pass


class Node:
    def __init__(self, url, secret):
        self.url = url.rstrip("/") + "/op"
        self.secret = secret

    def op(self, body, timeout=15):
        req = urllib.request.Request(
            self.url,
            data=json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + self.secret, "Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                out = json.load(resp)
        except urllib.error.HTTPError as e:
            raise OpError("%s: HTTP %d %s" % (body.get("op"), e.code, e.read().decode(errors="replace")[:300]))
        except (urllib.error.URLError, OSError) as e:
            raise OpError("%s: %s" % (body.get("op"), e))
        if "error" in out:
            raise OpError("%s: %s" % (body.get("op"), json.dumps(out)[:300]))
        return out.get("value")


def percentile(values, pct):
    s = sorted(values)
    return s[min(len(s) - 1, int(len(s) * pct / 100.0))]


def plan_channels(node, plan_id):
    for p in node.op({"op": "channel_plans"})["plans"]:
        if p["id"] == plan_id:
            return p
    raise OpError("no channel plan %r on the node" % plan_id)


def in_capture(plan, current):
    """The plan's channels whose whole band lies inside the capture `current`."""
    if not current:
        return []
    half = current["bandwidth_hz"] / 2.0
    lo, hi = current["center_hz"] - half, current["center_hz"] + half
    bw = plan["bandwidth_hz"] / 2.0
    return [c for c in plan["channels"] if lo <= c["center_hz"] - bw and c["center_hz"] + bw <= hi]


def channel_loads(row, channels, bandwidth_hz):
    """Per channel number, dB the peak of its band stands over the row's 15th-percentile floor."""
    q, size, bin_hz, centre = row["q"], row["size"], row["bin_hz"], row["center_hz"]
    floor_q = percentile(q, 15)
    out = {}
    for c in channels:
        lo = int((c["center_hz"] - bandwidth_hz / 2.0 - centre) / bin_hz + size / 2.0)
        hi = int((c["center_hz"] + bandwidth_hz / 2.0 - centre) / bin_hz + size / 2.0)
        band = q[max(lo, 0) : max(min(hi, size - 1), 0) + 1]
        if band:
            out[c["number"]] = (max(band) - floor_q) * row["step_db"]
    return out


class Watcher:
    def __init__(self, node, args, plan, channels, current):
        self.node, self.args, self.plan, self.channels = node, args, plan, channels
        self.current = current
        self.by_number = {c["number"]: c for c in channels}
        self.active = {}  # channel number -> {"demod", "stt", "last", "mine"}
        self.spectrum = None
        self.stop = False

    def log(self, msg):
        print(time.strftime("%H:%M:%S"), msg, flush=True)

    def adopt(self):
        for j in self.node.op({"op": "job_list"}):
            if j["kind"] != "fm_demod" or j["source"] != self.args.source or j["state"]["status"] != "running":
                continue
            for c in self.channels:
                if abs(j["tuning"]["center_hz"] - c["center_hz"]) <= CHANNEL_MATCH_HZ:
                    stt = [t["id"] for t in self.node.op({"op": "job_list"}) if t["kind"] == "transcribe"
                           and t.get("input") == j["id"] and t["state"]["status"] == "running"]
                    self.active[c["number"]] = {"demod": j["id"], "stt": stt[0] if stt else None,
                                                "last": time.time(), "mine": False}
                    self.log("adopted ch%d (%s)" % (c["number"], j["id"]))

    def start_spectrum(self):
        self.spectrum = self.node.op({
            "op": "job_start", "kind": "spectrum", "source": self.args.source,
            "center_hz": self.current["center_hz"], "bandwidth_hz": self.current["bandwidth_hz"],
            "deviation_hz": None, "sideband": None, "elastic": True, "replace": [],
        })
        self.spectrum_stream = None

    def event_stream(self):
        if self.spectrum_stream is None:
            for s in self.node.op({"op": "stream_list"}):
                if s["kind"] == "event" and s["provenance"].get("job") == self.spectrum:
                    self.spectrum_stream = s["id"]
        return self.spectrum_stream

    def latest_row(self):
        sid = self.event_stream()
        if sid is None:
            return None
        try:
            frames = self.node.op({"op": "stream_tail", "stream": sid, "max_frames": 4, "timeout_ms": 1500},
                                  timeout=15)["frames"]
        except OpError:
            self.spectrum_stream = None
            raise
        rows = [f["payload"] for f in frames if "q" in f.get("payload", {})]
        return rows[-1] if rows else None

    def open_channel(self, c):
        a = self.args
        demod = self.node.op({
            "op": "job_start", "kind": "fm_demod", "source": a.source, "center_hz": c["center_hz"],
            "bandwidth_hz": self.plan["bandwidth_hz"], "deviation_hz": None, "sideband": None,
            "elastic": False, "replace": [],
        })
        stt = None
        try:
            stt = self.node.op({"op": "job_attach", "kind": "transcribe", "job": demod, "language": None})
        except OpError as e:
            self.log("ch%d: transcribe not attached: %s" % (c["number"], e))
        self.active[c["number"]] = {"demod": demod, "stt": stt, "last": time.time(), "mine": True}
        self.log("ch%d active: fm_demod %s, transcribe %s" % (c["number"], demod, stt))

    def close_channel(self, number, why):
        st = self.active.pop(number)
        for jid in (st["stt"], st["demod"]):
            if jid:
                try:
                    self.node.op({"op": "job_stop", "job": jid})
                except OpError as e:
                    self.log("ch%d: stop %s: %s" % (number, jid, e))
        self.log("ch%d %s: jobs stopped" % (number, why))

    def tick(self):
        row = self.latest_row()
        if row is None:
            return
        now = time.time()
        loads = channel_loads(row, self.channels, self.plan["bandwidth_hz"])
        for number, over in loads.items():
            if over >= self.args.open_db:
                if number in self.active:
                    self.active[number]["last"] = now
                elif len([s for s in self.active.values() if s["mine"]]) < self.args.max_tuners:
                    try:
                        self.open_channel(self.by_number[number])
                    except OpError as e:
                        self.log("ch%d: could not start: %s" % (number, e))
        for number in list(self.active):
            if now - self.active[number]["last"] >= self.args.idle_s:
                self.close_channel(number, "idle %d s" % self.args.idle_s)

    def run(self):
        self.adopt()
        self.start_spectrum()
        self.log("watching %d channels of %s on %s (spectrum job %s)"
                 % (len(self.channels), self.plan["id"], self.args.source, self.spectrum))
        try:
            while not self.stop:
                t0 = time.time()
                try:
                    self.tick()
                except OpError as e:
                    self.log("tick: %s" % e)
                time.sleep(max(0.0, 1.0 - (time.time() - t0)))
        finally:
            for number in [n for n, s in self.active.items() if s["mine"]]:
                self.close_channel(number, "exit")
            try:
                self.node.op({"op": "job_stop", "job": self.spectrum})
            except OpError:
                pass


def main():
    ap = argparse.ArgumentParser(description="Run demodulators only on the active channels of a plan.")
    ap.add_argument("--node", required=True, help="node URL, e.g. http://127.0.0.1:8080")
    ap.add_argument("--secret-env", required=True, help="environment variable holding the shared secret")
    ap.add_argument("--source", required=True, help="source id")
    ap.add_argument("--plan", default="uhf_cb_au")
    ap.add_argument("--open-db", type=float, default=10.0, help="dB over the floor that opens a channel")
    ap.add_argument("--idle-s", type=float, default=60.0, help="seconds idle before a channel's jobs stop")
    ap.add_argument("--max-tuners", type=int, default=8)
    ap.add_argument("--dry-run", action="store_true", help="list the channels in the capture, start nothing")
    args = ap.parse_args()

    import os
    secret = os.environ.get(args.secret_env)
    if not secret:
        sys.exit("cb_watch: environment variable %s is not set" % args.secret_env)
    node = Node(args.node, secret)
    try:
        plan = plan_channels(node, args.plan)
        src = next((s for s in node.op({"op": "source_list"}) if s["id"] == args.source), None)
        if src is None:
            sys.exit("cb_watch: no source %r" % args.source)
        current = src.get("current")
        channels = in_capture(plan, current)
        if args.dry_run:
            print("plan %s, source %s, capture %s" % (plan["id"], args.source, json.dumps(current)))
            for c in channels:
                print("ch%d %d Hz" % (c["number"], c["center_hz"]))
            if not channels:
                print("no channel of the plan lies inside the capture")
            return
        if not channels:
            sys.exit("cb_watch: no channel of %s lies inside the source's capture" % plan["id"])
        w = Watcher(node, args, plan, channels, current)
        signal.signal(signal.SIGINT, lambda *_: setattr(w, "stop", True))
        signal.signal(signal.SIGTERM, lambda *_: setattr(w, "stop", True))
        w.run()
    except OpError as e:
        sys.exit("cb_watch: %s" % e)


if __name__ == "__main__":
    main()
