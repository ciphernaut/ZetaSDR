//! [`PfbChannelBank`]: the polyphase filter bank behind core's
//! [`ChannelBank`], one per wide source, shared by every channel on it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use zeta_core::bank::{BankError, BankFactory, ChannelBank, PFB_MIN_RATE_HZ};
use zeta_core::port::{IqBlock, IqReceiver, Tuning};
use zeta_core::types::{Hz, Timestamp};

use crate::pfb::{PfbBank, bins_for};

/// Blocks buffered per bin subscriber before it lags.
pub const BIN_CAPACITY: usize = 64;
/// Input blocks queued for the bank's thread before [`ChannelBank::push`]
/// drops them.
pub const INPUT_CAPACITY: usize = 64;

/// What the bank and its thread share: geometry and the bins' senders.
struct Shared {
    band: Tuning,
    bins: usize,
    senders: Mutex<Vec<Option<broadcast::Sender<IqBlock>>>>,
}

impl Shared {
    fn rate(&self) -> u64 {
        self.band.bandwidth_hz.0
    }

    fn bin_hz(&self) -> u64 {
        self.rate() / self.bins as u64
    }

    fn bin_rate_hz(&self) -> u64 {
        2 * self.bin_hz()
    }

    /// Absolute centre of bin `k` (natural order), clamped at 0 Hz.
    fn bin_center_hz(&self, k: usize) -> u64 {
        let rel = k as i128 - (self.bins / 2) as i128;
        let off = rel * self.rate() as i128 / self.bins as i128;
        (self.band.center_hz.0 as i128 + off).max(0) as u64
    }
}

/// The bank thread's filter bank and where it stands in the input.
struct Running {
    pfb: PfbBank,
    /// Inputs pushed since `pfb` was made.
    inputs: u64,
    /// Hops (outputs per bin) produced since `pfb` was made.
    hops: u64,
    /// False while no bin had a subscriber (or after a reset): the next
    /// active block starts a fresh `pfb`, since skipped input left its
    /// history stale.
    active: bool,
    /// The reset epoch the state belongs to.
    epoch: u64,
}

/// A [`PfbBank`] over one capture band, run on its own thread. Each bin's
/// stream is a broadcast channel made on its first
/// [`ChannelBank::subscribe`]. [`ChannelBank::push`] only queues the block
/// (at most [`INPUT_CAPACITY`]; a full queue drops the block and counts it in
/// [`ChannelBank::dropped`]); the thread runs the bank and sends to bins with
/// subscribers only, and skips the filtering altogether while nobody
/// subscribes. A drop also resets the filter (the input has a gap), and the
/// bank's first drop is logged. The thread ends when the bank is dropped.
///
/// A bin block's `t0` is the input time of its first sample's content: the
/// input block's `t0`, plus the time from that block's start to the newest
/// input of the first hop, minus the prototype's group delay
/// (`(taps - 1) / 2` input samples, `taps` = [`PfbBank::taps_len`]; just
/// under 8 samples at the bin rate).
/// Its `center_hz` is the bin centre and its rate the bin rate.
pub struct PfbChannelBank {
    shared: Arc<Shared>,
    tx: SyncSender<(u64, IqBlock)>,
    /// Bumped by [`ChannelBank::reset`]; blocks carry the epoch they were
    /// queued in, and the thread starts afresh when it changes.
    epoch: AtomicU64,
    dropped: AtomicU64,
}

impl PfbChannelBank {
    /// A bank of `bins` bins over the capture `rate_hz` around `center_hz`,
    /// with its thread started. Panics unless `bins` is a power of two of at
    /// least 2, or if the thread cannot be spawned.
    pub fn new(rate_hz: u64, center_hz: u64, bins: usize) -> Self {
        let shared = Arc::new(Shared {
            band: Tuning {
                center_hz: Hz(center_hz),
                bandwidth_hz: Hz(rate_hz),
            },
            bins,
            senders: Mutex::new(vec![None; bins]),
        });
        let running = Running {
            pfb: PfbBank::new(rate_hz, bins),
            inputs: 0,
            hops: 0,
            active: false,
            epoch: 0,
        };
        let (tx, rx) = sync_channel(INPUT_CAPACITY);
        let s = shared.clone();
        std::thread::Builder::new()
            .name(format!("pfb-{center_hz}"))
            .spawn(move || run(s, running, rx))
            .expect("spawn the pfb bank thread");
        Self {
            shared,
            tx,
            epoch: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Count a dropped input block; the bank's first drop is logged.
    fn count_drop(&self, why: &str) {
        if self.dropped.fetch_add(1, Ordering::Relaxed) == 0 {
            let band = self.shared.band;
            eprintln!(
                "zeta: channel bank at {} Hz ({} Hz wide) {why}; see dropped()",
                band.center_hz.0, band.bandwidth_hz.0
            );
        }
    }

    /// Sample rate of every bin: `2 x rate / bins`.
    pub fn bin_rate_hz(&self) -> u64 {
        self.shared.bin_rate_hz()
    }

    /// Nearest bin to `center_hz`, after checking the channel fits the band.
    fn bin_for(&self, center_hz: u64, bandwidth_hz: u64) -> Result<usize, BankError> {
        let sh = &self.shared;
        let bin_hz = sh.bin_hz();
        if bandwidth_hz > bin_hz {
            return Err(BankError::TooWide {
                bandwidth_hz,
                bin_hz,
            });
        }
        let (r, bc, c, bw) = (
            sh.rate() as i128,
            sh.band.center_hz.0 as i128,
            center_hz as i128,
            bandwidth_hz as i128,
        );
        if 2 * c - bw < 2 * bc - r || 2 * c + bw > 2 * bc + r {
            return Err(BankError::OutOfBand {
                center_hz,
                min_hz: sh.band.center_hz.0.saturating_sub(sh.rate() / 2),
                max_hz: sh.band.center_hz.0.saturating_add(sh.rate() / 2),
            });
        }
        // Round half up to the nearest bin index.
        let b = bin_hz as i128;
        let k = (2 * (c - bc) + b).div_euclid(2 * b) + (sh.bins / 2) as i128;
        Ok(k.clamp(0, sh.bins as i128 - 1) as usize)
    }
}

/// The bank thread: filter each queued block until the bank is dropped.
fn run(sh: Arc<Shared>, mut run: Running, rx: Receiver<(u64, IqBlock)>) {
    while let Ok((epoch, block)) = rx.recv() {
        if epoch != run.epoch {
            run.epoch = epoch;
            run.active = false;
        }
        process(&sh, &mut run, &block);
    }
}

fn process(sh: &Shared, run: &mut Running, block: &IqBlock) {
    let active: Vec<(usize, broadcast::Sender<IqBlock>)> = sh
        .senders
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .filter_map(|(k, tx)| {
            tx.clone()
                .filter(|t| t.receiver_count() > 0)
                .map(|t| (k, t))
        })
        .collect();
    if active.is_empty() {
        run.active = false;
        return;
    }
    if !run.active {
        run.pfb = PfbBank::new(sh.rate(), sh.bins);
        run.inputs = 0;
        run.hops = 0;
        run.active = true;
    }
    let (start, first_hop) = (run.inputs, run.hops);
    let ks: Vec<usize> = active.iter().map(|(k, _)| *k).collect();
    let mut outs = run.pfb.push_bins(&block.samples, &ks);
    let n = outs[0].len();
    run.inputs += block.samples.len() as u64;
    run.hops += n as u64;
    if n == 0 {
        return;
    }
    let hop = (sh.bins / 2) as u64;
    let rate = sh.rate() as u128;
    // Newest input of the first hop, counted from this block's start; that
    // hop completes within this block, so it is not before it.
    let newest = (first_hop * hop + hop - 1 - start) as u128;
    let delay_ns = ((run.pfb.taps_len() - 1) as u128 * 1_000_000_000 / (2 * rate)) as u64;
    let t0 = (block.t0.0 + (newest * 1_000_000_000 / rate) as u64).saturating_sub(delay_ns);
    for ((k, tx), out) in active.into_iter().zip(outs.iter_mut()) {
        let _ = tx.send(IqBlock {
            t0: Timestamp(t0),
            sample_rate_hz: Hz(sh.bin_rate_hz()),
            center_hz: Hz(sh.bin_center_hz(k)),
            samples: std::mem::take(out).into(),
        });
    }
}

impl ChannelBank for PfbChannelBank {
    fn band(&self) -> Tuning {
        self.shared.band
    }

    fn bin_hz(&self) -> u64 {
        self.shared.bin_hz()
    }

    fn subscribe(
        &self,
        center_hz: u64,
        bandwidth_hz: u64,
    ) -> Result<(IqReceiver, Hz, Hz), BankError> {
        let k = self.bin_for(center_hz, bandwidth_hz)?;
        let mut senders = self.shared.senders.lock().unwrap();
        let tx = senders[k].get_or_insert_with(|| broadcast::channel(BIN_CAPACITY).0);
        Ok((
            tx.subscribe(),
            Hz(self.shared.bin_rate_hz()),
            Hz(self.shared.bin_center_hz(k)),
        ))
    }

    /// Subscribers summed over every bin.
    fn consumers(&self) -> usize {
        self.shared
            .senders
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .map(|tx| tx.receiver_count())
            .sum()
    }

    /// Queue `block` for the bank thread; never blocks. A block from another
    /// band (the port retuned before the node replaced this bank) is not this
    /// bank's input and is ignored.
    fn push(&self, block: &IqBlock) {
        let band = self.shared.band;
        if block.sample_rate_hz != band.bandwidth_hz || block.center_hz != band.center_hz {
            return;
        }
        let epoch = self.epoch.load(Ordering::Relaxed);
        match self.tx.try_send((epoch, block.clone())) {
            Ok(()) => {}
            // The input now has a gap: the next accepted block starts the
            // filter afresh rather than splicing across it.
            Err(TrySendError::Full(_)) => {
                self.count_drop("cannot keep up; dropping input blocks");
                self.reset();
            }
            // The thread is gone (it panicked); count rather than lose silently.
            Err(TrySendError::Disconnected(_)) => {
                self.count_drop("thread has ended; dropping input blocks");
            }
        }
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Start the filter afresh from the next queued block (its history and
    /// input/hop counters), as after a gap in the input.
    fn reset(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }
}

/// Makes a [`PfbChannelBank`] of [`bins_for`] bins for each capture at or
/// above [`PFB_MIN_RATE_HZ`]; declines narrower captures and rates the bin
/// count does not divide (bin centres and rate would not be whole hertz).
pub struct PfbBankFactory;

impl BankFactory for PfbBankFactory {
    fn make(&self, rate_hz: Hz, center_hz: Hz) -> Option<Arc<dyn ChannelBank>> {
        let bins = bins_for(rate_hz.0);
        if rate_hz.0 < PFB_MIN_RATE_HZ || !rate_hz.0.is_multiple_of(bins as u64) {
            return None;
        }
        Some(Arc::new(PfbChannelBank::new(rate_hz.0, center_hz.0, bins)))
    }
}
