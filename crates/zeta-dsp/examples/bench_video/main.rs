//! `cargo run --release -p zeta-dsp --example bench_video [seconds]`: prints each video
//! stage's CPU-seconds per signal-second at 30.72 Msps (one thread, 64 k blocks) and the
//! fields decoded, for 8 and 1.5 MHz deviation with the carrier on the capture's centre
//! and 1 MHz off it.

mod bench;

fn main() {
    let seconds: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2.0);
    println!(
        "{:>8} {:>9} {:>8} {:>8} {:>10} {:>7}",
        "dev_MHz", "offset", "channel", "demod", "composite", "fields"
    );
    for case in bench::cases() {
        let c = bench::run(case, seconds);
        println!(
            "{:>8.1} {:>9} {:>8.3} {:>8.3} {:>10.3} {:>7}",
            case.deviation_hz / 1e6,
            case.offset_hz,
            c.channel,
            c.demod,
            c.composite,
            c.fields
        );
    }
}
