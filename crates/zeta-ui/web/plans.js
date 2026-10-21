// plans.js: the node's channel plans (spec 4), loaded once, and the channel names they give.
//
// Exports:
//   loadPlans(api) -> Promise<void>   `channel_plans` once; an error leaves the list empty
//   planIds() -> string[]             the loaded plan ids
//   channelLabel(hz, policy) -> {text: "ch20", title: "uhf_cb_au ch20 · <note>"} | null
//       The channel centred within CHANNEL_MATCH_HZ of `hz`. `policy` is the view's `plan`:
//       `auto` any plan, a plan id that plan only, `none` null.
//   channelsIn(lo, hi, policy) -> [{number, center_hz}]   the channels centred in lo..hi

const CHANNEL_MATCH_HZ = 1_000;

let plans = [];
let loading = null;

export function loadPlans(api) {
  if (!loading) {
    loading = api
      .op({ op: "channel_plans" })
      .then((r) => {
        const list = r && r.value && r.value.plans;
        plans = Array.isArray(list) ? list : [];
      })
      .catch(() => {
        plans = [];
        loading = null; // a failed call is asked again on the next loadPlans
      });
  }
  return loading;
}

export function planIds() {
  return plans.map((p) => p.id);
}

function selected(policy) {
  if (policy === "none") return [];
  return policy === "auto" || policy === undefined ? plans : plans.filter((p) => p.id === policy);
}

export function channelLabel(hz, policy) {
  if (typeof hz !== "number") return null;
  for (const p of selected(policy)) {
    for (const c of p.channels) {
      if (Math.abs(c.center_hz - hz) <= CHANNEL_MATCH_HZ) {
        const note = c.note ? ` · ${c.note}` : "";
        return { text: `ch${c.number}`, title: `${p.id} ch${c.number}${note}` };
      }
    }
  }
  return null;
}

export function channelsIn(lo, hi, policy) {
  const out = [];
  const seen = new Set();
  for (const p of selected(policy)) {
    for (const c of p.channels) {
      if (c.center_hz >= lo && c.center_hz <= hi && !seen.has(c.center_hz)) {
        seen.add(c.center_hz);
        out.push({ number: c.number, center_hz: c.center_hz });
      }
    }
  }
  return out;
}
