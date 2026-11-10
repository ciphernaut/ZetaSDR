//! `zeta soapy-worker --id ID --args ARGS [--rate-cap-hz N] [--depth-cap BITS:HZ]...
//! [--ring-fd N]`: one SoapySDR device served over stdin and stdout to the node that
//! spawned it.
//!
//! Once the device opens, the worker sets its rate cap (`max_bandwidth_hz`) before
//! serving: `max_rate_hz=N` in the args asserts it; `--rate-cap-hz N` (a cap an earlier
//! open measured, passed back by the node) reports it as measured; otherwise the device's
//! rates are probed from the top ([`zeta_source_soapy::probe`]) and the first that
//! streams cleanly is the measured cap. A probe that finds no rate clean measures
//! nothing: the device's own asserted cap stays, and the respawn probes again.
//!
//! A device whose link carries more than one sample depth (a B210's `sc16`, `sc12` and
//! `sc8`) has a cap per depth instead: `max_rate_hz=N` asserts every depth at `N`; each
//! `--depth-cap BITS:HZ` reports that depth's as measured; every other depth is probed,
//! lossy ones included. The source then runs `bits=N`'s depth, else the fastest lossless
//! one, and the record's cap is that depth's (`--rate-cap-hz` is not read).
//!
//! A node attaches every `--source-soapy` device through this child (see
//! `node_setup::open_soapy`), so a driver that crashes, hangs or leaks takes down a
//! process the node can respawn instead of the node itself. The child speaks the
//! protocol of `zeta_source_worker::serve`: its exit code tells the node what happened
//! ([`EXIT_NOT_FOUND`] when no device matches, [`EXIT_OPEN_FAILED`] when the device
//! would not open, the last stderr line saying why).
//!
//! [`EXIT_NOT_FOUND`]: zeta_source_worker::serve::EXIT_NOT_FOUND
//! [`EXIT_OPEN_FAILED`]: zeta_source_worker::serve::EXIT_OPEN_FAILED

use std::process::ExitCode;

use crate::cli::SoapyWorkerArgs;

/// Run the worker and end the process with its exit code; never returns.
///
/// The process is ended with [`std::process::exit`] rather than by returning to
/// `main`: tokio's stdin read blocks a pool thread until a line arrives, and after a
/// `shutdown` there may be none, so dropping the runtime would wait on it forever.
/// `serve` has already flushed its frames (or given up on a parent that stopped
/// reading) by the time it returns.
pub async fn run(args: SoapyWorkerArgs) -> ExitCode {
    let code = imp::run(args).await;
    std::process::exit(exit_value(code));
}

/// The number behind `code`: `ExitCode` has no getter, but it compares, and every code
/// the worker gives is a `u8`.
fn exit_value(code: ExitCode) -> i32 {
    (0..=u8::MAX)
        .find(|n| ExitCode::from(*n) == code)
        .map_or(1, i32::from)
}

#[cfg(not(soapy))]
mod imp {
    use std::process::ExitCode;

    use crate::cli::SoapyWorkerArgs;
    use crate::node_setup::NO_SOAPY;

    /// A build without the driver has nothing to serve. A node of such a build never
    /// spawns the worker (it refuses `--source-soapy` first); this answers someone who
    /// runs it by hand.
    pub async fn run(_args: SoapyWorkerArgs) -> ExitCode {
        eprintln!("zeta: soapy-worker: {NO_SOAPY}");
        ExitCode::FAILURE
    }
}

#[cfg(soapy)]
mod imp {
    use std::fs::File;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::ExitCode;
    use std::sync::Arc;

    use zeta_core::port::Port;
    use zeta_core::types::{Claim, Hz, SourceId};
    use zeta_source_soapy::SoapySource;
    use zeta_source_soapy::probe::{asserted_depths, own_depth, probe_depths, probe_rate};
    use zeta_source_worker::port::parse_depth_cap;
    use zeta_source_worker::ring::Ring;
    use zeta_source_worker::serve::{
        EXIT_NOT_FOUND, EXIT_OPEN_FAILED, redirect_stdout_to_stderr, requests_until_signal, serve,
    };

    use crate::cli::SoapyWorkerArgs;

    pub async fn run(args: SoapyWorkerArgs) -> ExitCode {
        die_with_parent();
        // Before anything that can write to stdout: the driver logs there.
        let frames = match redirect_stdout_to_stderr() {
            // SAFETY: the descriptor was just duplicated for us and nothing else owns it.
            Ok(fd) => unsafe { File::from_raw_fd(fd) },
            Err(e) => {
                eprintln!("zeta: soapy-worker: redirect stdout: {e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        // Before the device opens: a SIGTERM (the node's, or the parent-death signal)
        // or a terminal's SIGINT during a hung open must still end the worker, which
        // the watchdog this arms on the first signal does.
        let requests = match requests_until_signal() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("zeta: soapy-worker: signal handlers: {e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        // Enumerate first, on the driver's keys alone: an absent device is exit 3, which
        // the node polls on, not an open failure.
        let (driver_args, own) = match zeta_source_soapy::split_zeta_args(&args.args) {
            Ok(split) => split,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        let found = match zeta_source_soapy_driver::find(&driver_args) {
            Ok(found) => found,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        let Some(first) = found.first() else {
            eprintln!("device not found: {}", args.args);
            return ExitCode::from(EXIT_NOT_FOUND);
        };
        let driver = first
            .get("driver")
            .cloned()
            .unwrap_or_else(|| "soapy".into());
        let mut src = match zeta_source_soapy_driver::open(SourceId(args.id.clone()), &args.args) {
            Ok(src) => src,
            Err(e) => {
                // The last stderr line is the reason the node reports.
                eprintln!("{e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        let known: Result<Vec<(u8, u64)>, String> =
            args.depth_cap.iter().map(|c| parse_depth_cap(c)).collect();
        let set = match known {
            Ok(_) if src.link_depths().is_empty() => match own.bits {
                // The link has one depth: nothing for `bits=` to choose.
                Some(bits) => own_depth(&[], Some(bits)).map(|_| ()),
                None => {
                    set_rate_cap(&mut src, &args.id, own.max_rate_hz, args.rate_cap_hz).await;
                    Ok(())
                }
            },
            Ok(known) => set_depths(&mut src, &args.id, own.max_rate_hz, own.bits, &known).await,
            Err(e) => Err(format!("--depth-cap {e}")),
        };
        if let Err(e) = set {
            eprintln!("{e}");
            return ExitCode::from(EXIT_OPEN_FAILED);
        }
        let port = Arc::new(src) as Arc<dyn Port>;
        // After the device: a ring that will not open costs the speed, not the device.
        let ring = args.ring_fd.and_then(open_ring);
        serve(port, &driver, requests, frames, ring).await
    }

    /// Set the source's rate cap before it is served: `asserted` (`max_rate_hz=`) as
    /// asserted; else `cached` (`--rate-cap-hz`) as measured; else the cap the rate
    /// probe measures at the centre of the tune range, logged in one stderr line. Only
    /// a clean rate is a measurement: a probe that found none leaves the device's own
    /// asserted cap, and its line names the slowest rate that streamed as the
    /// `max_rate_hz=` to pin (a measured cap would be cached by the node and replayed
    /// on every start, so a bad probe must not become one). A probe that cannot tune
    /// any rate, or that lost the device, leaves the asserted cap too, with a stderr
    /// line saying why: `serve` then reports a lost device's fault and exits, and the
    /// respawn probes again.
    async fn set_rate_cap(
        src: &mut SoapySource,
        id: &str,
        asserted: Option<u64>,
        cached: Option<u64>,
    ) {
        let claim = match (asserted, cached) {
            (Some(hz), _) => Claim::Asserted(Hz(hz)),
            (None, Some(hz)) => Claim::Measured(Hz(hz)),
            (None, None) => {
                let caps = src.capabilities();
                let center = (caps.tune_min_hz.value().0 + caps.tune_max_hz.value().0) / 2;
                match probe_rate(src, center).await {
                    Ok(p) if p.clean => {
                        let (cap, tried) = (p.cap_hz, p.tried);
                        eprintln!(
                            "zeta-soapy-worker[{id}]: rate cap {cap} Hz measured ({tried} rungs tried)"
                        );
                        Claim::Measured(Hz(cap))
                    }
                    Ok(p) => {
                        // `cap_hz` is the slowest rate that streamed when none was
                        // clean.
                        let (lowest, tried) = (p.cap_hz, p.tried);
                        eprintln!(
                            "zeta-soapy-worker[{id}]: rate cap not measured: no rung clean ({tried} rungs tried, lowest {lowest} Hz); set max_rate_hz={lowest} to pin one"
                        );
                        return;
                    }
                    Err(e) => {
                        eprintln!("zeta-soapy-worker[{id}]: {e}; keeping the asserted cap");
                        return;
                    }
                }
            }
        };
        src.set_max_bandwidth(claim);
    }

    /// Fill the source's sample depths before it is served: every depth asserted at
    /// `asserted` (`max_rate_hz=`) when given; else each depth in `known` (`--depth-cap`)
    /// measured at its cap and every other probed at the centre of the tune range
    /// ([`probe_depths`]), one stderr line per depth. The source then runs `asked`'s depth
    /// (`bits=`), else the fastest lossless one ([`own_depth`]). A `bits=` the link does
    /// not carry is the error, found before anything is probed. A probe that cannot tune
    /// any rate, or that lost the device, leaves the asserted cap and no depths, with a
    /// stderr line, as [`set_rate_cap`] does.
    async fn set_depths(
        src: &mut SoapySource,
        id: &str,
        asserted: Option<u64>,
        asked: Option<u8>,
        known: &[(u8, u64)],
    ) -> Result<(), String> {
        // The depths' bits alone decide whether `asked` is one of them.
        own_depth(&asserted_depths(src, 0), asked)?;
        let depths = match asserted {
            Some(hz) => asserted_depths(src, hz),
            None => {
                let caps = src.capabilities();
                let center = (caps.tune_min_hz.value().0 + caps.tune_max_hz.value().0) / 2;
                match probe_depths(src, center, known).await {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("zeta-soapy-worker[{id}]: {e}; keeping the asserted cap");
                        return Ok(());
                    }
                }
            }
        };
        for d in &depths {
            let bits = d.bits;
            let lossy = if d.lossless { "" } else { " (lossy)" };
            let cap = d.max_bandwidth_hz.value().0;
            let how = match (&d.max_bandwidth_hz, asserted) {
                (Claim::Asserted(_), Some(_)) => "asserted (max_rate_hz=)".to_string(),
                (Claim::Asserted(_), None) => {
                    "not measured: no rung clean, the device's asserted cap kept".to_string()
                }
                (Claim::Measured(_), _) if known.iter().any(|(b, _)| *b == bits) => {
                    "measured (passed back)".to_string()
                }
                (Claim::Measured(_), _) => "measured".to_string(),
            };
            eprintln!("zeta-soapy-worker[{id}]: {bits}-bit depth{lossy}: rate cap {cap} Hz {how}");
        }
        let own = own_depth(&depths, asked)?;
        eprintln!("zeta-soapy-worker[{id}]: runs at {own} bits");
        src.set_depths(depths, own);
        Ok(())
    }

    /// The node's ring, inherited on `fd`; a ring that will not open is one stderr line,
    /// and the worker serves on the pipe.
    fn open_ring(fd: i32) -> Option<Ring> {
        // SAFETY: the node put the ring on `fd` for this process (`--ring-fd`), and
        // nothing else here owns that descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        match Ring::open(owned) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("zeta-soapy-worker: ring fd {fd}: {e}; serving on the pipe");
                None
            }
        }
    }

    /// Ask the kernel to send SIGTERM when the parent goes, so a node killed with
    /// SIGKILL does not leave the device held by an orphan. The signal fires when the
    /// thread that spawned the worker exits; the node spawns it from a runtime worker
    /// thread, which lives as long as the node. A parent that died before the call
    /// left us reparented already, so that is checked after it.
    #[cfg(target_os = "linux")]
    fn die_with_parent() {
        // SAFETY: getppid and prctl take no pointers; PR_SET_PDEATHSIG takes a signal
        // number.
        unsafe {
            let parent = libc::getppid();
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            if libc::getppid() != parent {
                std::process::exit(0);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn die_with_parent() {}
}
