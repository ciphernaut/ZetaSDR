// voice.js: the microphone button. Records in the browser and sends the recording to the
// node's local speech daemon (POST /stt); no browser speech API is used.
//
// Exports:
//   STT_RATE_HZ = 16000, MAX_RECORD_S = 30
//   downsample(samples, fromRate, toRate = STT_RATE_HZ) -> Float32Array
//       Linear interpolation; floor(n * toRate / fromRate) samples. `samples` unchanged when
//       the rates are equal.
//   encodeWav(samples, rate) -> ArrayBuffer
//       Mono PCM s16le WAV with the canonical 44-byte header; samples clamped to [-1, 1].
//   mountMic(button, api, onText, {status = #mic-status} = {}) -> {destroy}
//       Enables `button`; no request is made until it is used. First click: getUserMedia
//       {audio: true} into an AudioWorkletNode (inline module from a Blob URL; a
//       ScriptProcessorNode when `audioWorklet` is unavailable) that accumulates the channels'
//       mean as Float32 at the context rate; the button has class `recording` (a red dot)
//       meanwhile. Second click, or MAX_RECORD_S later: stops the tracks and the context,
//       downsamples to 16 kHz, encodes the WAV and calls api.stt. A transcript calls
//       onText(text) (a reply without a string `text` shows "speech daemon: bad reply"); a
//       503 shows "no speech daemon on this node" in `status`; other failures show their
//       message there. The button is disabled only while the upload is in flight.
//       destroy() abandons any recording (nothing is sent) and disables the button.

export const STT_RATE_HZ = 16000;
export const MAX_RECORD_S = 30;
export const NO_DAEMON = "no speech daemon on this node";

const WAV_HEADER_BYTES = 44;
const TAP_NAME = "zeta-mic-tap";
const SCRIPT_BUFFER = 4096;

// Posts each render quantum's channel mean (mono) to the main thread.
const TAP_MODULE = `
class Tap extends AudioWorkletProcessor {
  process(inputs) {
    const chans = inputs[0];
    if (chans && chans.length) {
      const n = chans[0].length;
      const out = new Float32Array(n);
      for (const ch of chans) for (let i = 0; i < n; i++) out[i] += ch[i] / chans.length;
      this.port.postMessage(out, [out.buffer]);
    }
    return true;
  }
}
registerProcessor(${JSON.stringify(TAP_NAME)}, Tap);
`;

export function downsample(samples, fromRate, toRate = STT_RATE_HZ) {
  if (fromRate === toRate) return samples;
  const n = samples.length;
  const out = new Float32Array(Math.floor((n * toRate) / fromRate));
  const step = fromRate / toRate;
  for (let i = 0; i < out.length; i++) {
    const pos = i * step;
    const i0 = Math.floor(pos);
    const f = pos - i0;
    const a = samples[i0];
    const b = samples[Math.min(i0 + 1, n - 1)];
    out[i] = a + (b - a) * f;
  }
  return out;
}

export function encodeWav(samples, rate) {
  const dataLen = samples.length * 2;
  const buf = new ArrayBuffer(WAV_HEADER_BYTES + dataLen);
  const v = new DataView(buf);
  const tag = (at, s) => {
    for (let i = 0; i < 4; i++) v.setUint8(at + i, s.charCodeAt(i));
  };
  tag(0, "RIFF");
  v.setUint32(4, 36 + dataLen, true);
  tag(8, "WAVE");
  tag(12, "fmt ");
  v.setUint32(16, 16, true); // fmt chunk size
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true); // byte rate
  v.setUint16(32, 2, true); // block align
  v.setUint16(34, 16, true); // bits per sample
  tag(36, "data");
  v.setUint32(40, dataLen, true);
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    v.setInt16(WAV_HEADER_BYTES + 2 * i, s < 0 ? s * 0x8000 : s * 0x7fff, true);
  }
  return buf;
}

function concat(chunks, total) {
  const out = new Float32Array(total);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

function errorText(e) {
  if (e && e.status === 503) return NO_DAEMON;
  if (e && e.name === "NotAllowedError") return "microphone permission denied";
  if (e && e.name === "NotFoundError") return "no microphone";
  return (e && e.message) || String(e);
}

// Starts capturing; resolves to {rate, stop() -> Float32Array} once audio flows. The context
// is created first, synchronously inside the click, so browsers let it start.
async function capture(onChunkLimit) {
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
    // Browsers expose the microphone only in a secure context (https or localhost).
    throw new Error("microphone needs a secure context: open the ui over https or localhost");
  }
  const ctx = new AudioContext();
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch (e) {
    ctx.close().catch(() => {});
    throw e;
  }
  const rate = ctx.sampleRate;
  const limit = Math.ceil(rate * MAX_RECORD_S);
  const chunks = [];
  let total = 0;
  const keep = (chunk) => {
    if (total >= limit) return;
    const c = chunk.length > limit - total ? chunk.subarray(0, limit - total) : chunk;
    chunks.push(c);
    total += c.length;
    if (total >= limit) onChunkLimit();
  };
  const nodes = [];
  try {
    await ctx.resume();
    const src = ctx.createMediaStreamSource(stream);
    // A muted path to the destination keeps the graph pulled in every browser.
    const mute = ctx.createGain();
    mute.gain.value = 0;
    mute.connect(ctx.destination);
    nodes.push(src, mute);
    if (ctx.audioWorklet && typeof AudioWorkletNode === "function") {
      const url = URL.createObjectURL(new Blob([TAP_MODULE], { type: "text/javascript" }));
      try {
        await ctx.audioWorklet.addModule(url);
      } finally {
        URL.revokeObjectURL(url);
      }
      const tap = new AudioWorkletNode(ctx, TAP_NAME, { numberOfOutputs: 1 });
      tap.port.onmessage = (ev) => keep(ev.data);
      src.connect(tap).connect(mute);
      nodes.push(tap);
    } else {
      const tap = ctx.createScriptProcessor(SCRIPT_BUFFER, 1, 1);
      tap.onaudioprocess = (ev) => {
        const ib = ev.inputBuffer;
        const out = new Float32Array(ib.length);
        for (let c = 0; c < ib.numberOfChannels; c++) {
          const ch = ib.getChannelData(c);
          for (let i = 0; i < out.length; i++) out[i] += ch[i] / ib.numberOfChannels;
        }
        keep(out);
      };
      src.connect(tap).connect(mute);
      nodes.push(tap);
    }
  } catch (e) {
    for (const t of stream.getTracks()) t.stop();
    ctx.close().catch(() => {});
    throw e;
  }
  return {
    rate,
    stop() {
      for (const t of stream.getTracks()) t.stop();
      for (const n of nodes) n.disconnect();
      ctx.close().catch(() => {});
      return concat(chunks, total);
    },
  };
}

export function mountMic(button, api, onText, { status = document.getElementById("mic-status") } = {}) {
  // idle | starting | recording | sending
  let phase = "idle";
  let rec = null;
  let timer = null;
  let stopWanted = false;
  let destroyed = false;

  const show = (text) => {
    if (status) status.textContent = text;
  };
  const setRecording = (on) => {
    button.classList.toggle("recording", on);
    button.setAttribute("aria-pressed", String(on));
    button.title = on ? "stop and transcribe" : "voice input (local speech daemon)";
  };

  const finish = async () => {
    if (phase !== "recording") return;
    clearTimeout(timer);
    timer = null;
    const r = rec;
    rec = null;
    const raw = r.stop();
    setRecording(false);
    if (destroyed) {
      phase = "idle";
      return;
    }
    if (raw.length === 0) {
      phase = "idle";
      show("nothing recorded");
      return;
    }
    phase = "sending";
    button.disabled = true;
    show("transcribing…");
    let text = null;
    try {
      const wav = encodeWav(downsample(raw, r.rate), STT_RATE_HZ);
      const reply = await api.stt(wav);
      if (reply && typeof reply.text === "string") {
        text = reply.text;
        show(text.trim() ? "" : "nothing heard");
      } else {
        show("speech daemon: bad reply");
      }
    } catch (e) {
      show(errorText(e));
    } finally {
      phase = "idle";
      if (!destroyed) button.disabled = false;
    }
    if (!destroyed && text && text.trim()) onText(text.trim());
  };

  const begin = async () => {
    phase = "starting";
    stopWanted = false;
    show("");
    setRecording(true);
    try {
      rec = await capture(() => finish());
    } catch (e) {
      phase = "idle";
      setRecording(false);
      show(`microphone: ${errorText(e)}`);
      return;
    }
    phase = "recording";
    if (destroyed) {
      rec.stop();
      rec = null;
      phase = "idle";
      setRecording(false);
      return;
    }
    timer = setTimeout(() => finish(), MAX_RECORD_S * 1000);
    if (stopWanted) finish();
  };

  const onClick = () => {
    if (phase === "idle") begin();
    else if (phase === "starting") stopWanted = true;
    else if (phase === "recording") finish();
  };

  button.addEventListener("click", onClick);
  button.disabled = false;
  setRecording(false);

  return {
    destroy() {
      destroyed = true;
      button.removeEventListener("click", onClick);
      clearTimeout(timer);
      if (rec) {
        rec.stop();
        rec = null;
      }
      phase = "idle";
      setRecording(false);
      button.disabled = true;
      show("");
    },
  };
}
