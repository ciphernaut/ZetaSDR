// audio.js: plays a job's audio stream through WebAudio.
//
// Exports:
//   parseEnvelope(buf) -> {header, samples}
//       One binary stream message (ArrayBuffer): `u32LE header_len | FrameSummary JSON |
//       u32LE body_len | body`, the body f32 little-endian PCM for audio. Throws on a
//       truncated envelope.
//   playJob(api, job, {onError, onEnd}) -> {stop}
//       Creates an AudioContext at once (inside the click that asked for it), finds the job's
//       audio stream (the GET /streams entry with provenance.job == job.id and kind "audio",
//       looked up STREAM_LOOKUPS times STREAM_LOOKUP_MS apart while the job starts), opens it
//       with api.streamBinary (credits 256, top-up 128, as zeta-client) and plays every frame
//       as one AudioBufferSourceNode at the header's payload.sample_rate_hz, queued back to
//       back: `nextStart` advances by each frame's duration; when the queue has run dry
//       (nextStart before ctx.currentTime) it is re-anchored at ctx.currentTime + LEAD_S. A
//       frame that would start more than MAX_AHEAD_S ahead is dropped (bounds the latency if
//       the stream outruns real time). The stream is re-resolved by provenance after a drop
//       (graph_put restarts the job with new stream ids). onError(message) reports a failure
//       (no audio stream, a bad envelope, an unplayable rate), after which playback stops and
//       onEnd() is called; stop() closes the socket and the context (and calls onEnd).
//   window.zetaAudio = {framesPlayed, framesDropped, playing}: frames scheduled and frames
//       dropped (beyond MAX_AHEAD_S) since the last playJob, and the job id being played (null
//       when stopped); read by the e2e scenarios.

export const LEAD_S = 0.1;
export const MAX_AHEAD_S = 2.0;
const STREAM_LOOKUPS = 20;
const STREAM_LOOKUP_MS = 250;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

window.zetaAudio = { framesPlayed: 0, framesDropped: 0, playing: null };

export function parseEnvelope(buf) {
  const view = new DataView(buf);
  if (buf.byteLength < 4) throw new Error("envelope ended inside the header length");
  const hlen = view.getUint32(0, true);
  if (buf.byteLength < 8 + hlen) throw new Error("envelope ended inside the header");
  const header = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 4, hlen)));
  const blen = view.getUint32(4 + hlen, true);
  const at = 8 + hlen;
  if (buf.byteLength < at + blen) throw new Error("envelope ended inside the body");
  const n = Math.floor(blen / 4);
  const samples = new Float32Array(n);
  // The body need not be 4-byte aligned in the message, so read it through the DataView.
  for (let i = 0; i < n; i++) samples[i] = view.getFloat32(at + 4 * i, true);
  return { header, samples };
}

async function audioStreamId(api, jobId) {
  const list = await api.streams();
  const s = list.find((i) => i.kind === "audio" && i.provenance && i.provenance.job === jobId);
  return s ? s.id : null;
}

export function playJob(api, job, { onError = () => {}, onEnd = () => {} } = {}) {
  const ctx = new AudioContext();
  ctx.resume().catch(() => {});
  let sub = null;
  let stopped = false;
  let nextStart = 0;
  window.zetaAudio = { framesPlayed: 0, framesDropped: 0, playing: job.id };

  const stop = () => {
    if (stopped) return;
    stopped = true;
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
    const n = frame.samples.length;
    if (n === 0) return;
    let buffer;
    try {
      buffer = ctx.createBuffer(1, n, rate);
    } catch (e) {
      fail(`audio: cannot play ${n} samples at ${rate} Hz: ${e.message}`);
      return;
    }
    buffer.copyToChannel(frame.samples, 0);
    const now = ctx.currentTime;
    if (nextStart < now) nextStart = now + LEAD_S;
    if (nextStart - now > MAX_AHEAD_S) {
      window.zetaAudio.framesDropped += 1;
      return;
    }
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(ctx.destination);
    src.start(nextStart);
    nextStart += n / rate;
    window.zetaAudio.framesPlayed += 1;
  };

  (async () => {
    let id = null;
    for (let i = 0; i < STREAM_LOOKUPS && !stopped && !id; i++) {
      if (i > 0) await sleep(STREAM_LOOKUP_MS);
      try {
        id = await audioStreamId(api, job.id);
      } catch {
        id = null;
      }
    }
    if (stopped) return;
    if (!id) {
      fail(`audio: job ${job.id} has no audio stream`);
      return;
    }
    sub = api.streamBinary(id, play, {
      credits: 256,
      topup: 128,
      onReconnect: () => audioStreamId(api, job.id),
    });
  })();

  return { stop };
}
