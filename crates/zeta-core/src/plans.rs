//! Channel plans: tables of numbered channels on a frequency raster, served by
//! the `channel_plans` op so the page, the CLI and the agent share one set of
//! numbers. A plan is one [`ChannelPlan`] constant and one entry in [`PLANS`].

/// A run of `count` channels numbered from `first`, `step_hz` apart from `first_hz`.
pub struct Segment {
    pub first: u32,
    pub count: u32,
    pub first_hz: u64,
    pub step_hz: u64,
}

/// A channel plan: its channels are the union of its segments.
pub struct ChannelPlan {
    pub id: &'static str,
    pub name: &'static str,
    pub region: &'static str,
    /// The width of one channel, Hz.
    pub bandwidth_hz: u64,
    pub segments: &'static [Segment],
    /// Channel number and what it is reserved for.
    pub notes: &'static [(u32, &'static str)],
}

/// A frequency within this many Hz of a channel centre is on the channel.
pub const CHANNEL_MATCH_HZ: u64 = 1_000;

/// How much stronger (dB) a neighbouring channel must be for the adjacent-channel
/// guard of an `nbfm_demod` on a plan channel to hold its squelch closed.
pub const GUARD_DB_DEFAULT: i64 = 12;

/// Australian UHF CB (ACMA class licence): 80 channels on two 25 kHz rasters
/// 12.5 kHz apart, 1-40 and 41-80.
pub const UHF_CB_AU: ChannelPlan = ChannelPlan {
    id: "uhf_cb_au",
    name: "Australian UHF CB",
    region: "au",
    bandwidth_hz: 12_500,
    segments: &[
        Segment {
            first: 1,
            count: 40,
            first_hz: 476_425_000,
            step_hz: 25_000,
        },
        Segment {
            first: 41,
            count: 40,
            first_hz: 476_437_500,
            step_hz: 25_000,
        },
    ],
    // 5, 35, 11, 22, 23 are ACMA designations; 40 "road" is the highway convention.
    notes: &[
        (5, "emergency"),
        (11, "calling"),
        (22, "data, no voice"),
        (23, "data, no voice"),
        (35, "emergency"),
        (40, "road (convention)"),
    ],
};

/// Every plan the node knows; a frequency on two plans takes the first.
pub const PLANS: &[ChannelPlan] = &[UHF_CB_AU];

/// The plan's `(number, centre Hz)` pairs, sorted by frequency.
pub fn channels(plan: &ChannelPlan) -> Vec<(u32, u64)> {
    let mut v: Vec<(u32, u64)> = plan
        .segments
        .iter()
        .flat_map(|s| {
            (0..s.count).map(move |i| (s.first + i, s.first_hz + u64::from(i) * s.step_hz))
        })
        .collect();
    v.sort_by_key(|&(n, hz)| (hz, n));
    v
}

/// The channel whose centre is nearest `hz`, within [`CHANNEL_MATCH_HZ`]; the
/// first plan in [`PLANS`] wins.
pub fn channel_at(hz: u64) -> Option<(&'static ChannelPlan, u32)> {
    PLANS.iter().find_map(|plan| {
        channels(plan)
            .into_iter()
            .map(|(n, c)| (c.abs_diff(hz), n))
            .filter(|&(d, _)| d <= CHANNEL_MATCH_HZ)
            .min()
            .map(|(_, n)| (plan, n))
    })
}

/// The smallest gap between two sorted channel centres of the plan, Hz.
pub fn adjacent_hz(plan: &ChannelPlan) -> u64 {
    let c = channels(plan);
    c.windows(2).map(|w| w[1].1 - w[0].1).min().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uhf_cb_au_has_80_channels_on_two_interleaved_rasters() {
        let c = channels(&UHF_CB_AU);
        assert_eq!(c.len(), 80);
        let hz = |n: u32| c.iter().find(|x| x.0 == n).unwrap().1;
        assert_eq!(hz(1), 476_425_000);
        assert_eq!(hz(40), 477_400_000);
        assert_eq!(hz(41), 476_437_500);
        assert_eq!(hz(80), 477_412_500);
        assert_eq!(adjacent_hz(&UHF_CB_AU), 12_500);
        assert!(c.windows(2).all(|w| w[0].1 < w[1].1), "sorted by frequency");
    }

    #[test]
    fn channel_at_matches_within_a_kilohertz() {
        let n = |hz| channel_at(hz).map(|(p, n)| (p.id, n));
        assert_eq!(n(476_900_000), Some(("uhf_cb_au", 20)));
        assert_eq!(n(476_887_500), Some(("uhf_cb_au", 59)));
        assert_eq!(n(476_912_500), Some(("uhf_cb_au", 60)));
        assert_eq!(n(476_900_900), Some(("uhf_cb_au", 20)));
        assert_eq!(n(476_901_500), None);
        assert_eq!(n(147_050_000), None);
    }
}
