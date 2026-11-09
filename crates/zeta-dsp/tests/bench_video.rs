//! The video chain's CPU cost at 30.72 Msps, bounded generously: for manual runs only
//! (`cargo test --release -p zeta-dsp --test bench_video -- --ignored --nocapture`), since a
//! CPU bound on a shared 2-CPU runner would flake. `cargo run --release -p zeta-dsp
//! --example bench_video` prints the same table.

#[path = "../examples/bench_video/bench.rs"]
mod bench;

#[test]
#[ignore = "a CPU benchmark: run by hand in release"]
fn every_video_stage_runs_well_under_one_core_at_30_72_msps() {
    for case in bench::cases() {
        let c = bench::run(case, 1.0);
        eprintln!("{case:?}: {c:?}");
        for (stage, cost) in [
            ("channel", c.channel),
            ("video_fm_demod", c.demod),
            ("composite_decode", c.composite),
        ] {
            assert!(cost < 0.9, "{case:?}: {stage} {cost:.3} CPU-s per signal-s");
        }
        assert!(c.fields >= 45, "{case:?}: {} fields of 50", c.fields);
    }
}
