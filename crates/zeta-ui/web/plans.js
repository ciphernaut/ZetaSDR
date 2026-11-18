// plans.js: the node's channel plans (spec 4), loaded once, and the channel names they give.
//
// Exports:
//   loadPlans(api) -> Promise<void>   `channel_plans` once; an error leaves the list empty
//   planIds() -> string[]             the loaded plan ids
//   resolvePlan(policy) -> string | null   the id of the first plan `policy` selects
//   planGroups(id) -> [{id, name, channels}]   the plan's named groups (none for an older
//       node's plans, which carry no `groups`, or an unknown plan)
//   channelLabel(hz, policy) -> {text: "ch20", title: "uhf_cb_au ch20 · <note>", plan: "uhf_cb_au"} | null
//       The channel centred within CHANNEL_MATCH_HZ of `hz`. `policy` is the view's `plan`:
//       `auto` any plan, a plan id that plan only, `none` null.
//   channelHolding(hz) -> {plan, number, bandwidthHz} | null
//       The channel (of any loaded plan) whose band `centre ± plan.bandwidth_hz / 2` holds `hz`;
//       the nearest centre when several do. The fit's width rule reads it: a plan covers a spot
//       only where its channels are, so no plan is chosen by view policy.
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

export function planGroups(id) {
  const p = plans.find((x) => x.id === id);
  return p && Array.isArray(p.groups) ? p.groups : [];
}

// The numbers of plan `id`'s data channels (`no_voice`; none from an older node).
export function planNoVoice(id) {
  const p = plans.find((x) => x.id === id);
  return p && Array.isArray(p.no_voice) ? p.no_voice : [];
}

// The id of the first plan `policy` selects (null for `none` or when no plan is loaded).
export function resolvePlan(policy) {
  const p = selected(policy)[0];
  return p ? p.id : null;
}

export function channelLabel(hz, policy) {
  if (typeof hz !== "number") return null;
  for (const p of selected(policy)) {
    for (const c of p.channels) {
      if (Math.abs(c.center_hz - hz) <= CHANNEL_MATCH_HZ) {
        const note = c.note ? ` · ${c.note}` : "";
        return { text: `ch${c.number}`, title: `${p.id} ch${c.number}${note}`, plan: p.id };
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

export function channelHolding(hz) {
  if (typeof hz !== "number") return null;
  let best = null;
  for (const p of plans) {
    const half = typeof p.bandwidth_hz === "number" ? p.bandwidth_hz / 2 : 0;
    for (const c of p.channels) {
      const off = Math.abs(c.center_hz - hz);
      if (off <= half && (best === null || off < best.off)) {
        best = { off, plan: p.id, number: c.number, bandwidthHz: p.bandwidth_hz };
      }
    }
  }
  return best && { plan: best.plan, number: best.number, bandwidthHz: best.bandwidthHz };
}
