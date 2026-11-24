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

/// A named group of a plan's channels (`lower40_voice`): the channels of `ranges`, each
/// `(first, last)` inclusive, less those in `except`. `plan_start` and `jobs_stop` take
/// its `id` in place of a channel list.
pub struct Group {
    pub id: &'static str,
    pub name: &'static str,
    pub ranges: &'static [(u32, u32)],
    pub except: &'static [u32],
}

impl Group {
    /// The group's channel numbers, sorted, each once.
    pub fn channels(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .ranges
            .iter()
            .flat_map(|&(first, last)| first..=last)
            .filter(|n| !self.except.contains(n))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
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
    /// The FM peak deviation the plan's channels use, Hz; `plan_start` gives an
    /// `fm_demod` it when the caller's `deviation_hz` is `null` (`None`: the kind's
    /// default).
    pub deviation_hz: Option<u64>,
    /// Named groups of the plan's channels, checked by [`check_groups`].
    pub groups: &'static [Group],
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
    // 5, 35, 11, 22, 23 are ACMA designations and 61-63 are reserved by the class
    // licence; 40 "road" is the highway convention. The repeaters' outputs are 1-8 and
    // 41-48 (their inputs 31-38 and 71-78).
    notes: &[
        (5, "emergency"),
        (11, "calling"),
        (22, "data, no voice"),
        (23, "data, no voice"),
        (35, "emergency"),
        (40, "road (convention)"),
        (61, "reserved"),
        (62, "reserved"),
        (63, "reserved"),
    ],
    no_voice: &[22, 23],
    deviation_hz: Some(2_500),
    groups: &[
        Group {
            id: "lower40_voice",
            name: "Lower 40, voice",
            ranges: &[(1, 40)],
            except: &[22, 23],
        },
        Group {
            id: "upper40_voice",
            name: "Upper 40, voice",
            ranges: &[(41, 80)],
            except: &[61, 62, 63],
        },
        Group {
            id: "voice",
            name: "Voice",
            ranges: &[(1, 80)],
            except: &[22, 23, 61, 62, 63],
        },
        Group {
            id: "repeater_outputs",
            name: "Repeater outputs",
            ranges: &[(1, 8), (41, 48)],
            except: &[],
        },
        Group {
            id: "data",
            name: "Data",
            ranges: &[(22, 23)],
            except: &[],
        },
    ],
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
    deviation_hz: None,
    groups: &[],
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
    deviation_hz: None,
    groups: &[],
};

/// Every plan the node knows; a frequency on two plans takes the first. The page's
/// `auto` plan policy names the first plan for its `Start plan…` entry.
pub const PLANS: &[ChannelPlan] = &[UHF_CB_AU, ACARS_VHF, AIS_VHF];

/// The plan in [`PLANS`] with this `id`.
pub fn plan(id: &str) -> Option<&'static ChannelPlan> {
    PLANS.iter().find(|p| p.id == id)
}

/// The group of `plan` with this `id`.
pub fn group<'p>(plan: &'p ChannelPlan, id: &str) -> Option<&'p Group> {
    plan.groups.iter().find(|g| g.id == id)
}

/// Check `plan`'s groups: each id listed once, every channel a group's ranges or
/// exceptions name on the plan, and no group without channels. The error names the
/// group by index: `groups[<i>]: ...`. The built-in plans are checked by a test.
pub fn check_groups(plan: &ChannelPlan) -> Result<(), String> {
    let on_plan: Vec<u32> = channels(plan).into_iter().map(|(n, _)| n).collect();
    for (i, g) in plan.groups.iter().enumerate() {
        if plan.groups[..i].iter().any(|o| o.id == g.id) {
            return Err(format!("groups[{i}].id: {} is listed twice", g.id));
        }
        let named = g
            .ranges
            .iter()
            .flat_map(|&(first, last)| first..=last)
            .chain(g.except.iter().copied());
        for n in named {
            if !on_plan.contains(&n) {
                return Err(format!("groups[{i}]: channel {n} is not on the plan"));
            }
        }
        if g.channels().is_empty() {
            return Err(format!("groups[{i}]: {} has no channels", g.id));
        }
    }
    Ok(())
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

/// [`channel_at`] among the plans of `region` (compared case-insensitively) and the
/// `world` plans: a plan of another region says nothing about a signal here.
pub fn plan_channel_at(center_hz: u64, region: &str) -> Option<(&'static ChannelPlan, u32)> {
    PLANS
        .iter()
        .filter(|p| p.region.eq_ignore_ascii_case(region) || p.region == "world")
        .find_map(|plan| {
            channels(plan)
                .into_iter()
                .map(|(n, c)| (c.abs_diff(center_hz), n))
                .filter(|&(d, _)| d <= CHANNEL_MATCH_HZ)
                .min()
                .map(|(_, n)| (plan, n))
        })
}

/// The FM deviation of the plan channel at `center_hz` in `region`, when it has one.
pub fn plan_deviation_at(center_hz: u64, region: &str) -> Option<u64> {
    plan_channel_at(center_hz, region).and_then(|(p, _)| p.deviation_hz)
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

    /// The group `id` of the UHF CB plan, expanded.
    fn cb_group(id: &str) -> Vec<u32> {
        group(&UHF_CB_AU, id).map(Group::channels).unwrap()
    }

    #[test]
    fn uhf_cb_au_names_its_voice_repeater_and_data_groups() {
        let ids: Vec<&str> = UHF_CB_AU.groups.iter().map(|g| g.id).collect();
        assert_eq!(
            ids,
            [
                "lower40_voice",
                "upper40_voice",
                "voice",
                "repeater_outputs",
                "data"
            ]
        );
        let lower = cb_group("lower40_voice");
        assert_eq!(lower.len(), 38);
        assert_eq!(
            lower,
            (1..=40)
                .filter(|n| ![22, 23].contains(n))
                .collect::<Vec<u32>>()
        );
        let upper = cb_group("upper40_voice");
        assert_eq!(
            upper,
            (41..=80)
                .filter(|n| !(61..=63).contains(n))
                .collect::<Vec<u32>>()
        );
        let both: Vec<u32> = lower.iter().chain(&upper).copied().collect();
        assert_eq!(cb_group("voice"), both);
        let repeaters: Vec<u32> = (1..=8).chain(41..=48).collect();
        assert_eq!(cb_group("repeater_outputs"), repeaters);
        // The data group is the plan's own no-voice channels.
        assert_eq!(cb_group("data"), UHF_CB_AU.no_voice);
        // The channels a voice group leaves out are the ones the plan's notes reserve.
        let note = |n: u32| {
            UHF_CB_AU
                .notes
                .iter()
                .find(|(c, _)| *c == n)
                .map(|(_, t)| *t)
        };
        for n in (1..=80).filter(|n| !both.contains(n)) {
            assert!(
                matches!(note(n), Some("data, no voice" | "reserved")),
                "ch{n}: {:?}",
                note(n)
            );
        }
        assert!(group(&UHF_CB_AU, "nope").is_none());
    }

    #[test]
    fn every_builtin_plans_groups_check() {
        for p in PLANS {
            assert_eq!(check_groups(p), Ok(()), "{}", p.id);
        }
    }

    /// A one-segment plan of channels 1-10 with `groups`.
    const fn ten(groups: &'static [Group]) -> ChannelPlan {
        ChannelPlan {
            id: "ten",
            name: "ten",
            region: "test",
            bandwidth_hz: 10_000,
            segments: &[Segment {
                first: 1,
                count: 10,
                first_hz: 100_000_000,
                step_hz: 10_000,
            }],
            notes: &[],
            no_voice: &[],
            deviation_hz: None,
            groups,
        }
    }

    #[test]
    fn a_group_listed_twice_on_a_missing_channel_or_empty_is_refused() {
        const fn g(
            id: &'static str,
            ranges: &'static [(u32, u32)],
            except: &'static [u32],
        ) -> Group {
            Group {
                id,
                name: id,
                ranges,
                except,
            }
        }
        const TWICE: ChannelPlan = ten(&[g("a", &[(1, 2)], &[]), g("a", &[(3, 4)], &[])]);
        const MISSING: ChannelPlan = ten(&[g("a", &[(9, 11)], &[])]);
        const MISSING_EXCEPT: ChannelPlan = ten(&[g("a", &[(1, 2)], &[12])]);
        const EMPTY: ChannelPlan = ten(&[g("a", &[(1, 2)], &[1, 2])]);
        const NONE: ChannelPlan = ten(&[g("a", &[], &[])]);
        const GOOD: ChannelPlan = ten(&[g("a", &[(1, 3), (2, 5)], &[4])]);
        let err = |p: &ChannelPlan| check_groups(p).unwrap_err();
        assert_eq!(err(&TWICE), "groups[1].id: a is listed twice");
        assert_eq!(err(&MISSING), "groups[0]: channel 11 is not on the plan");
        assert_eq!(
            err(&MISSING_EXCEPT),
            "groups[0]: channel 12 is not on the plan"
        );
        assert_eq!(err(&EMPTY), "groups[0]: a has no channels");
        assert_eq!(err(&NONE), "groups[0]: a has no channels");
        assert_eq!(check_groups(&GOOD), Ok(()));
        let a = group(&GOOD, "a").map(Group::channels);
        assert_eq!(a, Some(vec![1, 2, 3, 5]), "overlapping ranges count once");
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
