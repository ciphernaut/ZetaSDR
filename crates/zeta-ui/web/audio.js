// audio.js: plays a job's audio stream through WebAudio.
//
// Exports:
//   parseEnvelope(buf) -> {header, samples}
//       One binary stream message (ArrayBuffer): `u32LE header_len | FrameSummary JSON |
//       u32LE body_len | body`, the body f32 little-endian PCM for audio. Throws on a
//       truncated envelope.
//   splitChannels(samples, channels) -> Float32Array[]
//       De-interleaves audio into at most two channels: mono stays one, stereo is two, and
//       anything above 2 keeps only the first two (a downmix by dropping the rest). A trailing
//       partial frame is ignored.
//   playJob(api, job, {onError, onEnd, ctx}) -> {stop}
//       Creates an AudioContext at once (inside the click that asked for it; with `ctx` given
//       it plays on that one instead, which a click made before awaiting the op that starts
//       the job, as browsers tie autoplay to the gesture), finds the job's
//       audio stream (the GET /streams entry with provenance.job == job.id and kind "audio",
//       looked up STREAM_LOOKUPS times STREAM_LOOKUP_MS apart while the job starts), opens it
//       with api.streamBinary (credits 256, top-up 128, as zeta-client) and plays every frame
//       as one AudioBufferSourceNode at the header's payload.sample_rate_hz (one buffer
//       channel per audio channel, up to 2; a missing payload.channels is 1), queued back to
//       back: `nextStart` advances by each frame's duration; when the queue has run dry
//       (nextStart before ctx.currentTime) it is re-anchored at ctx.currentTime + LEAD_S. A
//       frame that would start more than MAX_AHEAD_S ahead is dropped (bounds the latency if
//       the stream outruns real time). The stream is re-resolved by provenance after a drop
//       (graph_put restarts the job with new stream ids). onError(message) reports a failure
//       (no audio stream, a bad envelope, an unplayable rate), after which playback stops and
//       onEnd() is called; stop() closes the socket and the context (and calls onEnd).
//   window.zetaAudio = {framesPlayed, framesDropped, playing, lastRate}: frames scheduled and
//       frames dropped (beyond MAX_AHEAD_S) since the last playJob, the job id being played
//       (null when stopped), the sample_rate_hz of the last frame's header (null before
//       one) and the channels of the last frame's header (null before one); read by the e2e
//       scenarios.

export const LEAD_S = 0.1;
export const MAX_AHEAD_S = 2.0;
const STREAM_LOOKUPS = 20;
const STREAM_LOOKUP_MS = 250;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

window.zetaAudio = {
  framesPlayed: 0,
  framesDropped: 0,
  playing: null,
  lastRate: null,
  lastChannels: null,
};

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

async function audioStreamId(api, jobId) {
  const list = await api.streams();
  const s = list.find((i) => i.kind === "audio" && i.provenance && i.provenance.job === jobId);
  return s ? s.id : null;
}

export function playJob(api, job, { onError = () => {}, onEnd = () => {}, ctx = null } = {}) {
  ctx = ctx || new AudioContext();
  ctx.resume().catch(() => {});
  let sub = null;
  let stopped = false;
  let nextStart = 0;
  window.zetaAudio = {
    framesPlayed: 0,
    framesDropped: 0,
    playing: job.id,
    lastRate: null,
    lastChannels: null,
  };

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
