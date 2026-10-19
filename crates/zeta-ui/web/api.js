// api.js: the node's HTTP and websocket API for the web app.
//
// Exports:
//   connect(secret, base = location.origin) -> Promise<Api>
//       GET /health with `Authorization: Bearer <secret>`; throws Unauthorized on 401,
//       Error on any other failure. Opens no websocket.
//   b64url(str) -> unpadded base64url of the UTF-8 bytes of `str`.
//   class Unauthorized extends Error
//   class OpError extends Error { json, status }: an error reply ({"error": name, ...}).
//
//   Api:
//     api.onUnauthorized = fn   called (once per 401) before a request throws Unauthorized,
//         and when a websocket that never opened is followed by a 401 from GET /health
//         (browsers hide the refused upgrade's status, so /health is probed)
//     api.op(obj) -> Promise<result JSON>              POST /op; throws OpError (with .json)
//     api.ask(text, maxTurns = 8) -> Promise<Transcript>  POST /ask {request, max_turns};
//         throws OpError (503 with {"error":"other","message":...} when serve has no LLM)
//     api.stt(wav) -> Promise<{text}>   POST /stt with the WAV bytes (ArrayBuffer) as
//         `Content-Type: audio/wav`; throws OpError (503 when serve has no speech daemon)
//     api.health() -> Promise<{status, version, uptime_s, ws_connections}>
//     api.streams() -> Promise<[{id, kind, provenance}]>   GET /streams (not an op)
//     api.streamText(id, onMessage, onClose, {reconnect = true, onReconnect}) -> {close}
//         Text websocket /stream/{id}; onMessage(msg) per parsed message
//         ({"type":"frame",...}, {"type":"lagged","missed":n}, {"type":"closed","reason":s});
//         onClose({code, reason, reconnecting}) after every socket close. While `reconnect`
//         is true it reopens with backoff 0.5 s doubling to 8 s (reset on open); when
//         `onReconnect(oldId) -> Promise<id|null>` is given the id is re-resolved first
//         (null: try again after the next backoff; nothing more happens if the subscription
//         was closed meanwhile). Stream ids change when a job restarts under `graph_put`, so
//         callers re-resolve by provenance with `streams()`.
//     api.streamBinary(id, onFrame, {credits = 256, topup = 128, onText, onClose,
//                      reconnect = true, onReconnect}) -> {close}
//         Binary websocket /stream/{id}?format=binary. Grants `credits` on open with
//         {"type":"credit","packets":N} and `topup` more each time `topup` binary messages
//         have been consumed, as zeta-client does. onFrame(ArrayBuffer) per binary message;
//         text messages (lagged, closed) go to onText(msg).
//
// Websockets authenticate with the subprotocols ["zeta", "zeta.bearer." + b64url(secret)].

export const BACKOFF_MIN_MS = 500;
export const BACKOFF_MAX_MS = 8000;

export class Unauthorized extends Error {
  constructor() {
    super("secret rejected");
    this.name = "Unauthorized";
  }
}

export class OpError extends Error {
  constructor(json, status) {
    super(json && json.message ? json.message : (json && json.error) || `HTTP ${status}`);
    this.name = "OpError";
    this.json = json;
    this.status = status;
  }
}

export function b64url(str) {
  const bytes = new TextEncoder().encode(str);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function replyJson(resp) {
  const text = await resp.text();
  try {
    return JSON.parse(text);
  } catch {
    return { error: "other", message: text || `HTTP ${resp.status}` };
  }
}

export async function connect(secret, base = location.origin) {
  const resp = await fetch(base + "/health", {
    headers: { Authorization: "Bearer " + secret },
  });
  if (resp.status === 401) throw new Unauthorized();
  if (!resp.ok) throw new Error(`health: HTTP ${resp.status}`);
  return new Api(base, secret);
}

export class Api {
  constructor(base, secret) {
    this.base = base;
    this.secret = secret;
    this.onUnauthorized = null;
  }

  // `body` is sent as JSON, or as is when `type` names its Content-Type.
  async #request(method, path, body, type = null) {
    const headers = { Authorization: "Bearer " + this.secret };
    if (body !== undefined) headers["Content-Type"] = type || "application/json";
    const resp = await fetch(this.base + path, {
      method,
      headers,
      body: body === undefined || type ? body : JSON.stringify(body),
    });
    const json = await replyJson(resp);
    if (resp.status === 401) {
      if (this.onUnauthorized) this.onUnauthorized();
      throw new Unauthorized();
    }
    if (!resp.ok) throw new OpError(json, resp.status);
    return json;
  }

  op(obj) {
    return this.#request("POST", "/op", obj);
  }

  ask(text, maxTurns = 8) {
    return this.#request("POST", "/ask", { request: text, max_turns: maxTurns });
  }

  stt(wav) {
    return this.#request("POST", "/stt", wav, "audio/wav");
  }

  health() {
    return this.#request("GET", "/health");
  }

  streams() {
    return this.#request("GET", "/streams");
  }

  #wsUrl(id, query) {
    const u = new URL(this.base);
    u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
    u.pathname = "/stream/" + encodeURIComponent(id);
    u.search = query || "";
    return u.toString();
  }

  // One logical subscription that survives socket closes. `handlers.open(ws)` and
  // `handlers.message(ev, ws)` see each socket.
  #resilient(id, query, binary, handlers, { onClose, reconnect = true, onReconnect } = {}) {
    let ws = null;
    let closed = false;
    let delay = BACKOFF_MIN_MS;
    let timer = null;
    const protocols = ["zeta", "zeta.bearer." + b64url(this.secret)];
    const open = () => {
      if (closed) return;
      let opened = false;
      ws = new WebSocket(this.#wsUrl(id, query), protocols);
      if (binary) ws.binaryType = "arraybuffer";
      ws.onopen = () => {
        opened = true;
        delay = BACKOFF_MIN_MS;
        handlers.open(ws);
      };
      ws.onmessage = (ev) => handlers.message(ev, ws);
      ws.onclose = (ev) => {
        // A refused upgrade: find out whether the secret is the reason (fires onUnauthorized).
        if (!opened && !closed) this.health().catch(() => {});
        const again = reconnect && !closed;
        if (onClose) onClose({ code: ev.code, reason: ev.reason, reconnecting: again });
        if (again) schedule();
      };
    };
    const schedule = () => {
      const wait = delay;
      delay = Math.min(delay * 2, BACKOFF_MAX_MS);
      timer = setTimeout(async () => {
        if (closed) return;
        if (onReconnect) {
          let next = null;
          try {
            next = await onReconnect(id);
          } catch {
            next = null;
          }
          if (closed) return; // onReconnect may close this subscription (e.g. to resubscribe)
          if (!next) return schedule();
          id = next;
        }
        open();
      }, wait);
    };
    open();
    return {
      close() {
        closed = true;
        clearTimeout(timer);
        if (ws) ws.close();
      },
    };
  }

  streamText(id, onMessage, onClose, opts = {}) {
    return this.#resilient(
      id,
      "",
      false,
      {
        open() {},
        message(ev) {
          let msg;
          try {
            msg = JSON.parse(ev.data);
          } catch {
            return;
          }
          onMessage(msg);
        },
      },
      { ...opts, onClose },
    );
  }

  streamBinary(id, onFrame, opts = {}) {
    const { credits = 256, topup = 128, onText, ...rest } = opts;
    let sinceGrant = 0;
    const grant = (ws, n) => ws.send(JSON.stringify({ type: "credit", packets: n }));
    return this.#resilient(
      id,
      "?format=binary",
      true,
      {
        open(ws) {
          sinceGrant = 0;
          grant(ws, credits);
        },
        message(ev, ws) {
          if (typeof ev.data === "string") {
            if (onText) {
              try {
                onText(JSON.parse(ev.data));
              } catch {
                /* not JSON: ignored */
              }
            }
            return;
          }
          onFrame(ev.data);
          sinceGrant += 1;
          if (sinceGrant >= topup) {
            sinceGrant = 0;
            grant(ws, topup);
          }
        },
      },
      rest,
    );
  }
}
