// audio.js: plays a job's audio stream through WebAudio.
//
// Exports:
//   parseEnvelope(buf) -> {header, samples}
//       One binary stream message (ArrayBuffer): `u32LE header_len | FrameSummary JSON |
//       u32LE body_len | body`, the body f32 little-endian PCM for audio. Throws on a
//       truncated envelope.
//   parseEnvelopeRaw(buf) -> {header, body: Uint8Array}
//       The same envelope with the body as bytes (a video frame's PNG).
//   splitChannels(samples, channels) -> Float32Array[]
//       De-interleaves audio into at most two channels: mono stays one, stereo is two, and
//       anything above 2 keeps only the first two (a downmix by dropping the rest). A trailing
//       partial frame is ignored.
//   audioStreamId(api, jobId, role = null) -> Promise<id | null>
//       The GET /streams entry with provenance.job == jobId, kind "audio" and `role` equal to
//       `role` (null: the job's main stream, the one with no role; a voice's role, such as
//       `slot1`, its own stream), or null when there is none.
//   playJob(api, job, {role, onError, onEnd, ctx}) -> {stop}
//       Creates an AudioContext at once (inside the click that asked for it; with `ctx` given
//       it plays on that one instead, which a click made before awaiting the op that starts
//       the job, as browsers tie autoplay to the gesture), finds the job's
//       audio stream (audioStreamId with `role`, null by default: the main stream,
//       looked up STREAM_LOOKUPS times STREAM_LOOKUP_MS apart while the job starts), opens it
//       with api.streamBinary (credits 256, top-up 128, as zeta-client) and plays every frame
//       as one AudioBufferSourceNode at the header's payload.sample_rate_hz (one buffer
//       channel per audio channel, up to 2; a missing payload.channels is 1), queued back to
//       back: `nextStart` advances by each frame's duration; when the queue has run dry
//       (nextStart before ctx.currentTime) it is re-anchored at ctx.currentTime plus the
//       lead (below). Past MAX_AHEAD_S of queue (the stream outran real time, or the context
//       stood still) what is queued is cut and it is re-anchored the same way, at each frame
//       and when the context runs again. A context found suspended on `visibilitychange` or `statechange`
//       (a phone's lock screen) is resumed. The stream is re-resolved by provenance after a drop
//       (graph_put restarts the job with new stream ids). onError(message) reports a failure
//       (no audio stream, a bad envelope, an unplayable rate), after which playback stops and
//       onEnd() is called; stop() closes the socket and the context (and calls onEnd).
//   newState() -> state, planFrame(state, {now, startNs, endNs, duration}) -> {start, state, dry}
//       The pure scheduling arithmetic of playJob (testable without audio). `state` is
//       {nextStart, lead, lastEndNs, calmSince, gaps, gapS}; it is not changed, the new one
//       is returned. `now` is the context clock (s), `startNs` and `endNs` the frame's span
//       from its header, `duration` its length (s). `start` is when to start the frame and
//       `dry` the gap (s) a dry re-anchor found, else 0.
//       The lead starts at LEAD_S. A frame that finds the queue dry (nextStart behind the
//       clock) is a starved page when its span continues the previous frame's (startNs
//       within CONTIGUOUS_NS of the last endNs: the node produced it without a pause, so
//       it was late): that counts one gap, adds the time the queue stood empty to gapS and
//       grows the lead by it, to MAX_LEAD_S. A frame whose span jumps after the last (the
//       mix sends nothing in silence, and a demodulator nothing while its squelch is shut)
//       or the first frame after a start is not a gap: it only re-anchors at now + lead. A header
//       without `start_ns` / `end_ns` (a non-number compares as a jump) reads as silence and
//       is never counted.
//       A lead over LEAD_S falls back by half its excess after each LEAD_QUIET_S with no
//       dry re-anchor (to LEAD_S once the excess is under a millisecond).
//   audioNote(a = window.zetaAudio) -> string
//       The page's own starvation count as a row's text: `page gaps 3` when gaps is not
//       zero, `lead 350 ms` when the lead is over LEAD_S, joined by a comma; "" otherwise.
//       app.js puts it in the playing mix's row (it is the page's count, not the node's
//       `underruns`).
//   window.zetaAudio = {framesPlayed, framesDropped, playing, lastRate, lastChannels,
//       gaps, gapMs, leadMs}: frames scheduled and frames cut (beyond MAX_AHEAD_S) since the
//       last playJob, the job id being played (null when stopped), the sample_rate_hz of the
//       last frame's header (null before one), the channels of the last frame's header (null
//       before one), the dry re-anchors of this page (planFrame), the time they stood silent
//       (ms) and the current lead (ms); read by the e2e scenarios. Every player that uses
//       playJob shares this path: a job row's play button (a demodulator, a voice's stream or
//       the mix); `onGap()` is called at each dry re-anchor.

export const LEAD_S = 0.1; // the lead a player starts with and falls back to
export const MAX_LEAD_S = 0.5; // the most a starved page grows its lead to
export const LEAD_QUIET_S = 60; // a lead over LEAD_S falls back one step after this long with no gap
export const CONTIGUOUS_NS = 5_000_000; // a frame starting this near the last one's end continues it
export const MAX_AHEAD_S = 1.0;
const STREAM_LOOKUPS = 20;
const STREAM_LOOKUP_MS = 250;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

window.zetaAudio = {
  framesPlayed: 0,
  framesDropped: 0,
  playing: null,
  lastRate: null,
  lastChannels: null,
  gaps: 0,
  gapMs: 0,
  leadMs: LEAD_S * 1000,
};

export function newState() {
  return { nextStart: 0, lead: LEAD_S, lastEndNs: null, calmSince: 0, gaps: 0, gapS: 0 };
}

export function planFrame(state, { now, startNs, endNs, duration }) {
  let { lead, calmSince, gaps, gapS } = state;
  if (lead > LEAD_S && now - calmSince >= LEAD_QUIET_S) {
    lead = lead - LEAD_S < 0.001 ? LEAD_S : LEAD_S + (lead - LEAD_S) / 2;
    calmSince = now;
  }
  let start = state.nextStart;
  let dry = 0;
  if (start < now) {
    const continues =
      state.lastEndNs !== null && Math.abs(startNs - state.lastEndNs) <= CONTIGUOUS_NS;
    if (state.nextStart > 0 && continues) {
      dry = now - state.nextStart;
      gaps += 1;
      gapS += dry;
      lead = Math.min(MAX_LEAD_S, lead + dry);
      calmSince = now;
    }
    start = now + lead;
  }
  return {
    start,
    dry,
    state: { nextStart: start + duration, lead, lastEndNs: endNs, calmSince, gaps, gapS },
  };
}

export function audioNote(a = window.zetaAudio) {
  const parts = [];
  if (a.gaps > 0) parts.push(`page gaps ${a.gaps}`);
  if (a.leadMs > LEAD_S * 1000 + 0.5) parts.push(`lead ${Math.round(a.leadMs)} ms`);
  return parts.join(", ");
}

// The header and the body's offset and length of an envelope.
function envelope(buf) {
  const view = new DataView(buf);
  if (buf.byteLength < 4) throw new Error("envelope ended inside the header length");
  const hlen = view.getUint32(0, true);
  if (buf.byteLength < 8 + hlen) throw new Error("envelope ended inside the header");
  const header = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 4, hlen)));
  const blen = view.getUint32(4 + hlen, true);
  const at = 8 + hlen;
  if (buf.byteLength < at + blen) throw new Error("envelope ended inside the body");
  return { view, header, at, blen };
}

export function parseEnvelopeRaw(buf) {
  const { header, at, blen } = envelope(buf);
  return { header, body: new Uint8Array(buf, at, blen) };
}

export function parseEnvelope(buf) {
  const { view, header, at, blen } = envelope(buf);
  const n = Math.floor(blen / 4);
  const samples = new Float32Array(n);
  // The body need not be 4-byte aligned in the message, so read it through the DataView.
  for (let i = 0; i < n; i++) samples[i] = view.getFloat32(at + 4 * i, true);
  return { header, samples };
}

export function splitChannels(samples, channels) {
  const frames = Math.floor(samples.length / channels);
  const out = Math.min(channels, 2);
  if (channels === 1) return [samples.subarray(0, frames)];
  const split = [];
  for (let c = 0; c < out; c++) {
    const data = new Float32Array(frames);
    for (let i = 0; i < frames; i++) data[i] = samples[i * channels + c];
    split.push(data);
  }
  return split;
}

export async function audioStreamId(api, jobId, role = null) {
  const list = await api.streams();
  const s = list.find(
    (i) => i.kind === "audio" && (i.role ?? null) === role && i.provenance && i.provenance.job === jobId,
  );
  return s ? s.id : null;
}

export function playJob(
  api,
  job,
  { role = null, onError = () => {}, onEnd = () => {}, onGap = () => {}, ctx = null } = {},
) {
  ctx = ctx || new AudioContext();
  ctx.resume().catch(() => {});
  let sub = null;
  let stopped = false;
  let st = newState();
  const queued = new Set();
  window.zetaAudio = {
    framesPlayed: 0,
    framesDropped: 0,
    playing: job.id,
    lastRate: null,
    lastChannels: null,
    gaps: 0,
    gapMs: 0,
    leadMs: LEAD_S * 1000,
  };

  // Cut a queue grown past MAX_AHEAD_S: played late it would be delay for good.
  const reanchor = () => {
    const now = ctx.currentTime;
    if (st.nextStart - now <= MAX_AHEAD_S) return;
    for (const src of queued) src.stop();
    window.zetaAudio.framesDropped += queued.size;
    queued.clear();
    st = { ...st, nextStart: now + st.lead };
  };
  const wake = () => {
    if (ctx.state === "running") reanchor();
    else if (ctx.state !== "closed") ctx.resume().catch(() => {});
  };
  ctx.addEventListener("statechange", wake);
  document.addEventListener("visibilitychange", wake);

  const stop = () => {
    if (stopped) return;
    stopped = true;
    ctx.removeEventListener("statechange", wake);
    document.removeEventListener("visibilitychange", wake);
    if (sub) sub.close();
    ctx.close().catch(() => {});
    if (window.zetaAudio.playing === job.id) window.zetaAudio.playing = null;
    onEnd();
  };
  const fail = (message) => {
    if (stopped) return;
    onError(message);
    stop();
  };

  const play = (buf) => {
    if (stopped) return;
    let frame;
    try {
      frame = parseEnvelope(buf);
    } catch (e) {
      fail(`audio: bad envelope: ${e.message}`);
      return;
    }
    const p = frame.header.payload || {};
    const rate = p.sample_rate_hz;
    window.zetaAudio.lastRate = rate ?? null;
    const channels = p.channels ?? 1;
    if (!Number.isInteger(channels) || channels < 1) {
      fail(`audio: bad channel count ${channels}`);
      return;
    }
    window.zetaAudio.lastChannels = channels;
    const split = splitChannels(frame.samples, channels);
    const n = split[0].length;
    if (n === 0) return;
    let buffer;
    try {
      buffer = ctx.createBuffer(split.length, n, rate);
    } catch (e) {
      fail(`audio: cannot play ${n} samples at ${rate} Hz: ${e.message}`);
      return;
    }
    split.forEach((data, c) => buffer.copyToChannel(data, c));
    const plan = planFrame(st, {
      now: ctx.currentTime,
      startNs: frame.header.start_ns,
      endNs: frame.header.end_ns,
      duration: n / rate,
    });
    // The cut of a runaway queue comes before the frame is placed, as it always did.
    st = { ...plan.state, nextStart: plan.start };
    reanchor();
    const start = st.nextStart;
    st = { ...st, nextStart: start + n / rate };
    const za = window.zetaAudio;
    za.leadMs = st.lead * 1000;
    if (plan.dry > 0) {
      za.gaps = st.gaps;
      za.gapMs = st.gapS * 1000;
      onGap();
    }
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(ctx.destination);
    src.onended = () => queued.delete(src);
    queued.add(src);
    src.start(start);
    window.zetaAudio.framesPlayed += 1;
  };

  (async () => {
    let id = null;
    for (let i = 0; i < STREAM_LOOKUPS && !stopped && !id; i++) {
      if (i > 0) await sleep(STREAM_LOOKUP_MS);
      try {
        id = await audioStreamId(api, job.id, role);
      } catch {
        id = null;
      }
    }
    if (stopped) return;
    if (!id) {
      fail(`audio: job ${job.id} has no ${role ? `${role} ` : ""}audio stream`);
      return;
    }
    sub = api.streamBinary(id, play, {
      credits: 256,
      topup: 128,
      onReconnect: () => audioStreamId(api, job.id, role),
    });
  })();

  return { stop };
}
