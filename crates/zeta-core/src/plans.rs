//! Channel plans: tables of numbered channels on a frequency raster, served by
//! the `channel_plans` op so the page, the CLI and the agent share one set of
//! numbers. A plan is one [`ChannelPlan`] constant and one entry in [`PLANS`].

use crate::port::Tuning;
use crate::types::Hz;

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
    /// Channels that carry data, never voice: `plan_start` attaches no `transcribe`
    /// to them and a mix does not read a job centred on one.
    pub no_voice: &'static [u32],
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
    no_voice: &[22, 23],
};

/// VHF ACARS: the seven channels aircraft use around 131 MHz, numbered by
/// frequency. They sit on no raster, so each is a segment of one.
pub const ACARS_VHF: ChannelPlan = ChannelPlan {
    id: "acars_vhf",
    name: "VHF ACARS",
    region: "world",
    bandwidth_hz: 10_000,
    segments: &[
        Segment {
            first: 1,
            count: 1,
            first_hz: 130_025_000,
            step_hz: 0,
        },
        Segment {
            first: 2,
            count: 1,
            first_hz: 130_450_000,
            step_hz: 0,
        },
        Segment {
            first: 3,
            count: 1,
            first_hz: 131_125_000,
            step_hz: 0,
        },
        Segment {
            first: 4,
            count: 1,
            first_hz: 131_450_000,
            step_hz: 0,
        },
        Segment {
            first: 5,
            count: 1,
            first_hz: 131_550_000,
            step_hz: 0,
        },
        Segment {
            first: 6,
            count: 1,
            first_hz: 131_725_000,
            step_hz: 0,
        },
        Segment {
            first: 7,
            count: 1,
            first_hz: 131_825_000,
            step_hz: 0,
        },
    ],
    notes: &[
        (1, "North America"),
        (2, "North America"),
        (3, "North America"),
        (4, "Asia"),
        (5, "worldwide primary"),
        (6, "Europe"),
        (7, "Europe"),
    ],
    no_voice: &[1, 2, 3, 4, 5, 6, 7],
};

/// AIS: the two maritime VHF channels, 87B and 88B, in one capture. Data only.
pub const AIS_VHF: ChannelPlan = ChannelPlan {
    id: "ais_vhf",
    name: "AIS VHF",
    region: "world",
    bandwidth_hz: 25_000,
    segments: &[
        Segment {
            first: 1,
            count: 1,
            first_hz: 161_975_000,
            step_hz: 0,
        },
        Segment {
            first: 2,
            count: 1,
            first_hz: 162_025_000,
            step_hz: 0,
        },
    ],
    notes: &[(1, "channel A, 87B"), (2, "channel B, 88B")],
    no_voice: &[1, 2],
};

/// Every plan the node knows; a frequency on two plans takes the first. The page's
/// `auto` plan policy names the first plan for its `Start plan…` entry.
pub const PLANS: &[ChannelPlan] = &[UHF_CB_AU, ACARS_VHF, AIS_VHF];

/// The plan in [`PLANS`] with this `id`.
pub fn plan(id: &str) -> Option<&'static ChannelPlan> {
    PLANS.iter().find(|p| p.id == id)
}

/// The band bounding the channels `numbers` of `plan`: from the lowest channel's
/// lower edge to the highest's upper edge, each channel being its centre ±
/// `bandwidth_hz / 2`. Numbers the plan lacks are ignored; none at all is a
/// zero-width band at 0 Hz.
pub fn bounding(plan: &ChannelPlan, numbers: &[u32]) -> Tuning {
    let half = plan.bandwidth_hz / 2;
    let centres: Vec<u64> = channels(plan)
        .into_iter()
        .filter(|(n, _)| numbers.contains(n))
        .map(|(_, hz)| hz)
        .collect();
    let (Some(lo), Some(hi)) = (centres.iter().min(), centres.iter().max()) else {
        return Tuning {
            center_hz: Hz(0),
            bandwidth_hz: Hz(0),
        };
    };
    let (lo, hi) = (lo - half, hi + half);
    Tuning {
        center_hz: Hz(lo + (hi - lo) / 2),
        bandwidth_hz: Hz(hi - lo),
    }
}

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

/// The plan channel at `hz` (as [`channel_at`]) when that channel carries no voice.
pub fn no_voice_at(hz: u64) -> Option<(&'static ChannelPlan, u32)> {
    channel_at(hz).filter(|(p, n)| p.no_voice.contains(n))
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
    fn plan_finds_by_id_and_bounding_spans_the_channel_edges() {
        assert_eq!(plan("uhf_cb_au").map(|p| p.id), Some("uhf_cb_au"));
        assert!(plan("nope").is_none());
        let all: Vec<u32> = (1..=80).collect();
        let t = bounding(&UHF_CB_AU, &all);
        assert_eq!(t.center_hz, Hz(476_918_750));
        assert_eq!(t.bandwidth_hz, Hz(1_000_000));
        let one = bounding(&UHF_CB_AU, &[20]);
        assert_eq!(one.center_hz, Hz(476_900_000));
        assert_eq!(one.bandwidth_hz, Hz(12_500));
    }

    #[test]
    fn acars_vhf_holds_the_seven_channels_in_one_capture() {
        let c = channels(&ACARS_VHF);
        let hz: Vec<u64> = c.iter().map(|x| x.1).collect();
        assert_eq!(
            hz,
            [
                130_025_000,
                130_450_000,
                131_125_000,
                131_450_000,
                131_550_000,
                131_725_000,
                131_825_000
            ]
        );
        let all: Vec<u32> = (1..=7).collect();
        assert_eq!(c.iter().map(|x| x.0).collect::<Vec<_>>(), all);
        assert_eq!(adjacent_hz(&ACARS_VHF), 100_000);
        // The band the field-craft recipe tunes for ACARS: 130.925 MHz, under 2 MHz.
        let t = bounding(&ACARS_VHF, &all);
        assert_eq!(t.center_hz, Hz(130_925_000));
        assert_eq!(t.bandwidth_hz, Hz(1_810_000));
        let n = channel_at(131_550_000).map(|(p, n)| (p.id, n));
        assert_eq!(n, Some(("acars_vhf", 5)));
    }

    #[test]
    fn ais_vhf_holds_both_channels_in_one_capture() {
        let c = channels(&AIS_VHF);
        assert_eq!(c, [(1, 161_975_000), (2, 162_025_000)]);
        let t = bounding(&AIS_VHF, &[1, 2]);
        assert_eq!(t.center_hz, Hz(162_000_000));
        assert_eq!(t.bandwidth_hz, Hz(75_000));
        assert_eq!(
            no_voice_at(162_025_000).map(|(p, n)| (p.id, n)),
            Some(("ais_vhf", 2))
        );
    }

    #[test]
    fn no_voice_names_the_cb_data_channels_and_every_acars_channel() {
        let n = |hz| no_voice_at(hz).map(|(p, n)| (p.id, n));
        assert_eq!(n(476_950_000), Some(("uhf_cb_au", 22)));
        assert_eq!(n(476_975_000), Some(("uhf_cb_au", 23)));
        assert_eq!(n(476_900_000), None);
        assert_eq!(n(131_550_000), Some(("acars_vhf", 5)));
        assert_eq!(n(147_050_000), None);
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
