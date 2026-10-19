//! [`PfbChannelBank`]: the polyphase filter bank behind core's
//! [`ChannelBank`], one per wide source, shared by every channel on it.

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use zeta_core::bank::{BankError, BankFactory, ChannelBank, PFB_MIN_RATE_HZ};
use zeta_core::port::{IqBlock, IqReceiver, Tuning};
use zeta_core::types::{Hz, Timestamp};

use crate::pfb::{PfbBank, bins_for};

/// Blocks buffered per bin subscriber before it lags.
pub const BIN_CAPACITY: usize = 64;

/// The running filter bank and where it stands in the input.
struct Running {
    pfb: PfbBank,
    /// Inputs pushed since `pfb` was made.
    inputs: u64,
    /// Hops (outputs per bin) produced since `pfb` was made.
    hops: u64,
    /// False while no bin had a subscriber: the next active push starts a
    /// fresh `pfb`, since the skipped input left its history stale.
    active: bool,
}

/// A [`PfbBank`] over one capture band. Each bin's stream is a broadcast
/// channel made on its first [`ChannelBank::subscribe`]; [`ChannelBank::push`]
/// runs the bank and sends to bins with subscribers only, and skips the
/// filtering altogether while nobody subscribes.
///
/// A bin block's `t0` is the input time of its first sample's content: the
/// input block's `t0`, plus the time from that block's start to the newest
/// input of the first hop, minus the prototype's group delay
/// (`(taps - 1) / 2` input samples, `taps` = [`PfbBank::taps_len`]; just
/// under 8 samples at the bin rate).
/// Its `center_hz` is the bin centre and its rate the bin rate.
pub struct PfbChannelBank {
    band: Tuning,
    bins: usize,
    running: Mutex<Running>,
    senders: Mutex<Vec<Option<broadcast::Sender<IqBlock>>>>,
}

impl PfbChannelBank {
    /// A bank of `bins` bins over the capture `rate_hz` around `center_hz`.
    /// Panics unless `bins` is a power of two of at least 2.
    pub fn new(rate_hz: u64, center_hz: u64, bins: usize) -> Self {
        Self {
            band: Tuning {
                center_hz: Hz(center_hz),
                bandwidth_hz: Hz(rate_hz),
            },
            bins,
            running: Mutex::new(Running {
                pfb: PfbBank::new(rate_hz, bins),
                inputs: 0,
                hops: 0,
                active: false,
            }),
            senders: Mutex::new(vec![None; bins]),
        }
    }

    fn rate(&self) -> u64 {
        self.band.bandwidth_hz.0
    }

    /// Sample rate of every bin: `2 x rate / bins`.
    pub fn bin_rate_hz(&self) -> u64 {
        2 * self.rate() / self.bins as u64
    }

    /// Absolute centre of bin `k` (natural order), clamped at 0 Hz.
    fn bin_center_hz(&self, k: usize) -> u64 {
        let rel = k as i128 - (self.bins / 2) as i128;
        let off = rel * self.rate() as i128 / self.bins as i128;
        (self.band.center_hz.0 as i128 + off).max(0) as u64
    }

    /// Nearest bin to `center_hz`, after checking the channel fits the band.
    fn bin_for(&self, center_hz: u64, bandwidth_hz: u64) -> Result<usize, BankError> {
        let bin_hz = self.bin_hz();
        if bandwidth_hz > bin_hz {
            return Err(BankError::TooWide {
                bandwidth_hz,
                bin_hz,
            });
        }
        let (r, bc, c, bw) = (
            self.rate() as i128,
            self.band.center_hz.0 as i128,
            center_hz as i128,
            bandwidth_hz as i128,
        );
        if 2 * c - bw < 2 * bc - r || 2 * c + bw > 2 * bc + r {
            return Err(BankError::OutOfBand {
                center_hz,
                min_hz: self.band.center_hz.0.saturating_sub(self.rate() / 2),
                max_hz: self.band.center_hz.0.saturating_add(self.rate() / 2),
            });
        }
        // Round half up to the nearest bin index.
        let b = bin_hz as i128;
        let k = (2 * (c - bc) + b).div_euclid(2 * b) + (self.bins / 2) as i128;
        Ok(k.clamp(0, self.bins as i128 - 1) as usize)
    }
}

impl ChannelBank for PfbChannelBank {
    fn band(&self) -> Tuning {
        self.band
    }

    fn bin_hz(&self) -> u64 {
        self.rate() / self.bins as u64
    }

    fn subscribe(
        &self,
        center_hz: u64,
        bandwidth_hz: u64,
    ) -> Result<(IqReceiver, Hz, Hz), BankError> {
        let k = self.bin_for(center_hz, bandwidth_hz)?;
        let mut senders = self.senders.lock().unwrap();
        let tx = senders[k].get_or_insert_with(|| broadcast::channel(BIN_CAPACITY).0);
        Ok((
            tx.subscribe(),
            Hz(self.bin_rate_hz()),
            Hz(self.bin_center_hz(k)),
        ))
    }

    /// Subscribers summed over every bin.
    fn consumers(&self) -> usize {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .map(|tx| tx.receiver_count())
            .sum()
    }

    fn push(&self, block: &IqBlock) {
        // A block from another band (the port retuned before the node
        // replaced this bank) is not this bank's input.
        if block.sample_rate_hz != self.band.bandwidth_hz || block.center_hz != self.band.center_hz
        {
            return;
        }
        let active: Vec<(usize, broadcast::Sender<IqBlock>)> = self
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
        let mut run = self.running.lock().unwrap();
        if active.is_empty() {
            run.active = false;
            return;
        }
        if !run.active {
            *run = Running {
                pfb: PfbBank::new(self.rate(), self.bins),
                inputs: 0,
                hops: 0,
                active: true,
            };
        }
        let (start, first_hop) = (run.inputs, run.hops);
        let mut outs = run.pfb.push(&block.samples);
        let n = outs[0].len();
        run.inputs += block.samples.len() as u64;
        run.hops += n as u64;
        if n == 0 {
            return;
        }
        let hop = (self.bins / 2) as u64;
        let rate = self.rate() as u128;
        // Newest input of the first hop, counted from this block's start;
        // that hop completes within this block, so it is not before it.
        let newest = (first_hop * hop + hop - 1 - start) as u128;
        let delay_ns = ((run.pfb.taps_len() - 1) as u128 * 1_000_000_000 / (2 * rate)) as u64;
        drop(run);
        let t0 = (block.t0.0 + (newest * 1_000_000_000 / rate) as u64).saturating_sub(delay_ns);
        for (k, tx) in active {
            let _ = tx.send(IqBlock {
                t0: Timestamp(t0),
                sample_rate_hz: Hz(self.bin_rate_hz()),
                center_hz: Hz(self.bin_center_hz(k)),
                samples: std::mem::take(&mut outs[k]).into(),
            });
        }
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
