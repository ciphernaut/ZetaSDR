// mix.js: who is talking on the audio mix, from one event socket.
//
// Exports:
//   pickMix(jobs, playingId) -> job | null
//       Among the running jobs of kind `mix`, the one with id `playingId`, else the newest
//       (`created_ns`). Null when none runs.
//   class MixWatch(api, {onChange})
//       .sync(jobs, playingId)   keeps exactly one `api.streamText` subscription, on the event
//                                stream of pickMix's mix (found in `api.streams()` by
//                                `provenance.job` and `kind === "event"`, and again through
//                                `onReconnect` after the mix was restarted); a pick that
//                                changes or goes closes it. When the socket has opened, first
//                                and after a reconnect, `active` and `last` are seeded from the
//                                mix's `job_context.mix`, unless an event came first (a mix says
//                                nothing until its set changes). Each `protocol: "mix"` event
//                                replaces `active`; a job that left it becomes `last`, stamped
//                                with the page's own time. `last` survives a restart of the
//                                same mix; a socket closing clears `active` only; another mix
//                                clears both.
//       .activeIds() -> string[] the job ids speaking now (overlays.js setSpeaking)
//       .rowText(mixId, labelOf, nowMs) -> string
//                                `ch9 now`, `ch9, ch22 now`, `last ch9 12 s ago`, or "" before
//                                any (and for a mix other than the watched one). `labelOf(hz)`
//                                names a centre: the plan channel, else `476.625 MHz`.
//       .close()
//   DEMOD_KINDS                the job kinds a mix can mute (the four demodulators)
//   mixCovering(job, jobs, playingId) -> mix | null
//       The running mix that would hear `job`: null for a mix, an attached job or a kind
//       outside DEMOD_KINDS; else pickMix among the running mixes whose `mix` node's
//       `sources` is null or holds `job.source`. A mute goes to that mix only: when several
//       cover a source, the others go on hearing the channel.
//   isMuted(job, mix) -> boolean
//       Whether the mix's graph mutes the job's channel: an entry of `muted` on the job's
//       source within CHANNEL_MATCH_HZ of its centre (a mute is a channel, not a job, so it
//       outlives the job and a restart at the same band is still muted).
//   muteOp(job, mixId, graph, on) -> op
//       The `graph_put` of `graph`, the mix's own as a `graph_get` just read it, with
//       `{source, center_hz}` added to `muted` (`on`), or every entry on that source within
//       CHANNEL_MATCH_HZ removed. The node keeps the mix's id and relaunches it.
//   window.zetaMix = {job, active, last}: the watched mix id and what the page believes; read
//       by the e2e scenarios.
//
// The mix has no band, so the page lights the demodulators it hears (the job ids in `active`),
// not an outline of its own, and its row carries the text.

export function pickMix(jobs, playingId) {
  const mixes = (jobs || []).filter(
    (j) => j.kind === "mix" && j.state && j.state.status === "running",
  );
  if (mixes.length === 0) return null;
  const playing = mixes.find((j) => j.id === playingId);
  if (playing) return playing;
  return mixes.reduce((a, b) => ((b.created_ns ?? 0) > (a.created_ns ?? 0) ? b : a));
}

export const DEMOD_KINDS = ["fm_demod", "wfm_demod", "am_demod", "ssb_demod"];
const CHANNEL_MATCH_HZ = 1_000; // zeta_core::plans::CHANNEL_MATCH_HZ

const mixNode = (mix) => ((mix.graph && mix.graph.nodes) || []).find((n) => n.kind === "mix");

export function mixCovering(job, jobs, playingId) {
  if (!job || job.input || !DEMOD_KINDS.includes(job.kind)) return null;
  const covering = (jobs || []).filter((j) => {
    const node = j.kind === "mix" ? mixNode(j) : null;
    const sources = node && node.params ? node.params.sources : undefined;
    return sources === null || (Array.isArray(sources) && sources.includes(job.source));
  });
  return pickMix(covering, playingId);
}

const sameChannel = (m, job) =>
  m.source === job.source && Math.abs(m.center_hz - job.tuning.center_hz) <= CHANNEL_MATCH_HZ;

export function isMuted(job, mix) {
  const node = mixNode(mix);
  return !!node && ((node.params && node.params.muted) || []).some((m) => sameChannel(m, job));
}

export function muteOp(job, mixId, read, on) {
  const graph = JSON.parse(JSON.stringify(read));
  const node = mixNode({ graph });
  const kept = ((node.params && node.params.muted) || []).filter((m) => !sameChannel(m, job));
  node.params = {
    ...(node.params || {}),
    muted: on ? [...kept, { source: job.source, center_hz: job.tuning.center_hz }] : kept,
  };
  return { op: "graph_put", job: mixId, graph };
}

window.zetaMix = { job: null, active: [], last: null };

export class MixWatch {
  constructor(api, { onChange = () => {} } = {}) {
    this.api = api;
    this.onChange = onChange;
    this.jobId = null;
    this.active = [];
    this.last = null; // {job, source, center_hz, atMs}
    this.sub = null;
    this.resolving = false;
    this.sawEvent = false;
    this.closed = false;
  }

  sync(jobs, playingId) {
    if (this.closed) return;
    const pick = pickMix(jobs, playingId);
    const id = pick ? pick.id : null;
    if (id !== this.jobId) {
      this.#drop();
      this.jobId = id;
      this.active = [];
      this.last = null;
      this.sawEvent = false;
      this.#publish();
      this.onChange();
    }
    if (id && !this.sub && !this.resolving) this.#open(id);
  }

  activeIds() {
    return this.active.map((a) => a.job);
  }

  rowText(mixId, labelOf, nowMs) {
    if (mixId !== this.jobId) return "";
    if (this.active.length > 0) {
      const centres = [...this.active].sort((a, b) => a.center_hz - b.center_hz);
      return `${centres.map((a) => labelOf(a.center_hz)).join(", ")} now`;
    }
    if (this.last) {
      const s = Math.max(0, Math.round((nowMs - this.last.atMs) / 1000));
      return `last ${labelOf(this.last.center_hz)} ${s} s ago`;
    }
    return "";
  }

  close() {
    this.closed = true;
    this.#drop();
    this.jobId = null;
    this.active = [];
    this.last = null;
    this.#publish();
  }

  #drop() {
    if (this.sub) this.sub.close();
    this.sub = null;
  }

  #publish() {
    window.zetaMix = { job: this.jobId, active: this.active, last: this.last };
  }

  async #streamId(id) {
    const list = await this.api.streams();
    const s = list.find(
      (i) => i.kind === "event" && i.provenance && i.provenance.job === id,
    );
    return s ? s.id : null;
  }

  async #open(id) {
    this.resolving = true;
    let stream = null;
    try {
      stream = await this.#streamId(id);
    } catch {
      stream = null;
    }
    this.resolving = false;
    // Gone or changed while looking: the next sync tries again (a mix still starting has no
    // stream yet).
    if (this.closed || this.jobId !== id || this.sub || !stream) return;
    this.sub = this.api.streamText(
      stream,
      (msg) => {
        if (msg.type === "frame" && msg.payload && msg.payload.protocol === "mix") {
          this.#event(id, msg.payload);
        }
      },
      () => {
        if (this.jobId !== id || this.active.length === 0) return;
        this.active = [];
        this.#publish();
        this.onChange();
      },
      {
        // A restarted mix (a mute's graph_put) publishes again only when its active set
        // changes, so what it holds now is read again, as when the socket first opened.
        onReconnect: async () => {
          const next = await this.#streamId(id);
          if (next && this.jobId === id) this.sawEvent = false;
          return next;
        },
        // Only now: the node subscribes a socket before it answers the upgrade, so a change
        // after this read reaches the socket. Read sooner, a change between the read and the
        // subscription reaches neither, and the page stays dark until the set next changes.
        onOpen: () => this.#seed(id),
      },
    );
  }

  #event(id, payload) {
    if (this.jobId !== id) return;
    this.sawEvent = true;
    const next = Array.isArray(payload.active) ? payload.active : [];
    const ids = new Set(next.map((a) => a.job));
    const left = this.active.filter((a) => !ids.has(a.job));
    if (left.length > 0) {
      const a = left[left.length - 1];
      this.last = { job: a.job, source: a.source, center_hz: a.center_hz, atMs: Date.now() };
    }
    this.active = next;
    this.#publish();
    this.onChange();
  }

  // What the mix's context latched, asked once the socket is open: an event that arrived first
  // is newer and wins, as the overlays' seedSquelch.
  async #seed(id) {
    let r = null;
    try {
      r = await this.api.op({ op: "job_context", job: id });
    } catch {
      return;
    }
    const m = r && r.value && r.value.mix;
    if (!m || this.closed || this.jobId !== id || this.sawEvent) return;
    this.active = Array.isArray(m.active) ? m.active : [];
    if (!this.last && m.last && typeof m.last.center_hz === "number") {
      this.last = {
        job: m.last.job,
        source: m.last.source,
        center_hz: m.last.center_hz,
        atMs: Math.floor((m.last.t_ns ?? 0) / 1e6),
      };
    }
    this.#publish();
    this.onChange();
  }
}
