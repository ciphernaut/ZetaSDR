//! A shared-memory ring of IQ slots between the node and a worker child.
//!
//! The stdout pipe cannot carry tens of megasamples a second, so blocks travel
//! through a memfd instead: the node creates the ring, the child inherits its
//! descriptor, the child copies each block into a fixed slot and tells the node
//! which one over the pipe, and the node copies the slot out and releases it.
//!
//! The mapping starts with a 64-byte header: the magic `b"ZRNG"`, the version
//! (`u32`, [`VERSION`] 2), the bytes per slot (`u32`, [`SLOT_BYTES`]) and the
//! slot count (`u32`), then `head` and `tail` as `AtomicU64` at offsets 16 and
//! 24, and the sample format (`AtomicU32` at offset 32: 0 not set yet, 1
//! `cf32`, 2 `sc16`); the rest is zero. Slot `i` starts at byte `64 + i *
//! SLOT_BYTES`. Block `seq` lives in slot `seq % slots`; the producer stores
//! `head = seq + 1` once it is written, and the consumer stores `tail = seq +
//! 1` once it has copied it out, which frees the slot. The producer refuses a
//! block while `slots` blocks are unreleased.
//!
//! The node creates the ring before the child has opened its device, so the
//! child chooses the format: once the device is open, and before its first
//! slot, it sets it ([`Ring::set_format`]) and names it in its hello. A slot
//! is [`SLOT_BYTES`] whatever the format, so an `sc16` slot holds twice the
//! samples of a `cf32` one ([`SlotFormat::samples_per_slot`]) and the ring
//! twice the time; [`Ring::read`] converts an `sc16` slot by
//! [`zeta_core::port::sc16_to_cf32`]'s per-sample conversion
//! ([`sc16_sample_to_cf32`]), the one a source in the node uses, so the node
//! reads the same bits either way. Version 1 had no format (always `cf32`)
//! and counted its slots in samples; a child that finds another version than
//! its own serves on the pipe.
//!
//! Header fields and samples are native endian (samples as `f32` or `i16`
//! pairs): both ends are the same executable on the same machine, so there is
//! nothing to convert but the format.

use std::fmt;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use num_complex::Complex;
use zeta_core::port::sc16_sample_to_cf32;

/// The bytes of one slot, whatever its format: 2 MiB.
pub const SLOT_BYTES: usize = 2 << 20;

/// Samples per `cf32` slot: 2 MiB of `cf32`, about 70 ms at 30 Msps. An `sc16`
/// slot holds twice as many ([`SlotFormat::samples_per_slot`]).
pub const SLOT_SAMPLES: usize = SLOT_BYTES / std::mem::size_of::<Complex<f32>>();

/// The ring layout's version, checked by [`Ring::open`].
pub const VERSION: u32 = 2;

/// The ring size the node asks for when nothing else is said: 64 MiB, which
/// is 31 slots.
pub const DEFAULT_RING_BYTES: usize = 64 << 20;

/// The fewest slots a ring may have, so the producer can keep writing while
/// the consumer is still copying.
pub const MIN_SLOTS: usize = 4;

const MAGIC: [u8; 4] = *b"ZRNG";
const HEADER: usize = 64;
const OFF_VERSION: usize = 4;
const OFF_SLOT_BYTES: usize = 8;
const OFF_SLOTS: usize = 12;
const OFF_HEAD: usize = 16;
const OFF_TAIL: usize = 24;
const OFF_FORMAT: usize = 32;

/// The sample format of a ring's slots, chosen by the producer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotFormat {
    /// `Complex<f32>`, 8 bytes a sample.
    Cf32,
    /// `Complex<i16>` as a 16-bit device gives them, 4 bytes a sample.
    Sc16,
}

impl SlotFormat {
    /// Its name in a hello's `transport.format`.
    pub fn name(self) -> &'static str {
        match self {
            SlotFormat::Cf32 => "cf32",
            SlotFormat::Sc16 => "sc16",
        }
    }

    /// The format a hello names `name`, if this build knows it.
    pub fn from_name(name: &str) -> Option<SlotFormat> {
        [SlotFormat::Cf32, SlotFormat::Sc16]
            .into_iter()
            .find(|f| f.name() == name)
    }

    /// The most samples a slot of this format holds.
    pub fn samples_per_slot(self) -> usize {
        SLOT_BYTES / self.sample_bytes()
    }

    fn sample_bytes(self) -> usize {
        match self {
            SlotFormat::Cf32 => std::mem::size_of::<Complex<f32>>(),
            SlotFormat::Sc16 => std::mem::size_of::<Complex<i16>>(),
        }
    }

    fn code(self) -> u32 {
        match self {
            SlotFormat::Cf32 => 1,
            SlotFormat::Sc16 => 2,
        }
    }

    fn from_code(code: u32) -> Option<SlotFormat> {
        [SlotFormat::Cf32, SlotFormat::Sc16]
            .into_iter()
            .find(|f| f.code() == code)
    }
}

impl fmt::Display for SlotFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The producer's refusal: every slot holds a block the consumer has not
/// released yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingFull;

impl fmt::Display for RingFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the ring is full: no slot has been released")
    }
}

impl std::error::Error for RingFull {}

/// The consumer's refusal of a block descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RingError {
    /// The descriptor names a slot, length or sequence the ring cannot hold.
    BadDescriptor(String),
}

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RingError::BadDescriptor(why) => write!(f, "bad ring descriptor: {why}"),
        }
    }
}

impl std::error::Error for RingError {}

/// A mapped memfd ring. It owns the descriptor and the mapping, and unmaps on
/// drop.
pub struct Ring {
    fd: OwnedFd,
    map: NonNull<u8>,
    len: usize,
    slots: usize,
}

// SAFETY: the mapping is shared memory that lives as long as the `Ring`. The
// header is written once before the ring is shared and only read after; `head`,
// `tail` and the format are atomics. The only unsynchronised bytes are slot contents,
// which the protocol hands from producer to consumer through the descriptor
// and `head`, and back through `tail`, so no slot is written while it is read.
unsafe impl Send for Ring {}
// SAFETY: as for `Send`; every `&self` method touches the header only through
// atomics or reads of fields fixed at creation.
unsafe impl Sync for Ring {}

impl Ring {
    /// Creates a ring of `bytes` (the header and as many whole slots as fit),
    /// refusing one with fewer than [`MIN_SLOTS`] slots.
    pub fn create(bytes: usize) -> io::Result<Ring> {
        let slots = bytes.saturating_sub(HEADER) / SLOT_BYTES;
        if slots < MIN_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a ring of {bytes} bytes holds {slots} slots of {SLOT_BYTES} bytes; at least {MIN_SLOTS} are needed"
                ),
            ));
        }
        let slots_u32 = u32::try_from(slots)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many ring slots"))?;
        let len = HEADER + slots * SLOT_BYTES;
        let fd = memfd(c"zeta-ring", len)?;
        let ring = Ring::map(fd, len, slots)?;
        let base = ring.map.as_ptr();
        // SAFETY: the mapping is `len >= HEADER` bytes, writable, page aligned
        // (so the `u32` writes are aligned), and not yet shared with anyone.
        // A fresh memfd is zero, so `head`, `tail`, the format (not set) and
        // the padding start at 0.
        unsafe {
            base.cast::<[u8; 4]>().write(MAGIC);
            base.add(OFF_VERSION).cast::<u32>().write(VERSION);
            base.add(OFF_SLOT_BYTES)
                .cast::<u32>()
                .write(SLOT_BYTES as u32);
            base.add(OFF_SLOTS).cast::<u32>().write(slots_u32);
        }
        Ok(ring)
    }

    /// Maps a ring another process created, checking its header and that the
    /// file is exactly the size the header says. The descriptor is made
    /// close-on-exec first: it was inherited on purpose, and must not leak on
    /// into anything this process runs in turn (a driver's helper).
    pub fn open(fd: OwnedFd) -> io::Result<Ring> {
        let bad = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
        // SAFETY: plain `fcntl` calls on a descriptor this process owns; they
        // take no pointers.
        unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let mut st = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fstat` writes a `stat` into the buffer it is given and
        // nothing else; the buffer is read only when it succeeds.
        let st = unsafe {
            if libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) < 0 {
                return Err(io::Error::last_os_error());
            }
            st.assume_init()
        };
        let len = usize::try_from(st.st_size)
            .map_err(|_| bad(format!("a ring file of {} bytes", st.st_size)))?;
        if len < HEADER {
            return Err(bad(format!(
                "a ring file of {len} bytes is smaller than its header"
            )));
        }
        // The slot count is checked below, before anything uses it.
        let mut ring = Ring::map(fd, len, 0)?;
        let base = ring.map.as_ptr();
        // SAFETY: the mapping is at least `HEADER` bytes and page aligned; the
        // creator wrote these fields before handing the descriptor over.
        let (magic, version, slot_bytes, slots) = unsafe {
            (
                base.cast::<[u8; 4]>().read(),
                base.add(OFF_VERSION).cast::<u32>().read(),
                base.add(OFF_SLOT_BYTES).cast::<u32>().read(),
                base.add(OFF_SLOTS).cast::<u32>().read(),
            )
        };
        if magic != MAGIC {
            return Err(bad(format!("ring magic {magic:?}, not {MAGIC:?}")));
        }
        // Before the layout fields: another version may lay them out otherwise.
        if version != VERSION {
            return Err(bad(format!("ring version {version}, not {VERSION}")));
        }
        if slot_bytes as usize != SLOT_BYTES {
            return Err(bad(format!(
                "ring slots of {slot_bytes} bytes, not {SLOT_BYTES}"
            )));
        }
        let slots = slots as usize;
        if slots < MIN_SLOTS {
            return Err(bad(format!(
                "a ring of {slots} slots; at least {MIN_SLOTS} are needed"
            )));
        }
        let want = slots
            .checked_mul(SLOT_BYTES)
            .and_then(|b| b.checked_add(HEADER));
        if want != Some(len) {
            return Err(bad(format!(
                "a ring file of {len} bytes for {slots} slots of {SLOT_BYTES} bytes"
            )));
        }
        ring.slots = slots;
        Ok(ring)
    }

    /// The descriptor, for the child to inherit.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// How many slots the ring has.
    pub fn slots(&self) -> usize {
        self.slots
    }

    /// The size of the mapping, header included.
    pub fn bytes(&self) -> usize {
        self.len
    }

    /// The producer, once and before its first write: say what format the
    /// slots hold. Setting the format it already has is no change; another is
    /// an error naming both.
    pub fn set_format(&self, f: SlotFormat) -> io::Result<()> {
        // Release pairs with the consumer's acquire in `format`, though the
        // hello that names the format, sent after this, orders it anyway.
        match self
            .format_word()
            .compare_exchange(0, f.code(), Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(()),
            Err(was) if was == f.code() => Ok(()),
            Err(was) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the ring's format is {} already, not {f}",
                    SlotFormat::from_code(was)
                        .map_or_else(|| format!("code {was}"), |w| w.to_string())
                ),
            )),
        }
    }

    /// The slots' format as the producer set it; `None` before it did (or for
    /// a code this build does not know).
    pub fn format(&self) -> Option<SlotFormat> {
        SlotFormat::from_code(self.format_word().load(Ordering::Acquire))
    }

    /// Producer: copies block `seq` into slot `seq % slots` and publishes it,
    /// returning the slot index, or refuses while `slots` blocks are
    /// unreleased.
    ///
    /// # Panics
    ///
    /// When the ring's format is not `cf32` (not set yet, or `sc16`), or
    /// `samples` is longer than [`SLOT_SAMPLES`].
    pub fn try_write(&self, seq: u64, samples: &[Complex<f32>]) -> Result<u32, RingFull> {
        self.write_slot(SlotFormat::Cf32, seq, samples)
    }

    /// Producer: as [`Ring::try_write`], for a ring of `sc16` slots: up to
    /// `SLOT_BYTES / 4` raw samples, written as they are.
    ///
    /// # Panics
    ///
    /// When the ring's format is not `sc16`, or `samples` is longer than
    /// [`SlotFormat::samples_per_slot`].
    pub fn try_write_sc16(&self, seq: u64, samples: &[Complex<i16>]) -> Result<u32, RingFull> {
        self.write_slot(SlotFormat::Sc16, seq, samples)
    }

    /// The write of `samples` in `format`, which must be the ring's; `T` is
    /// that format's sample.
    fn write_slot<T: Copy>(
        &self,
        format: SlotFormat,
        seq: u64,
        samples: &[T],
    ) -> Result<u32, RingFull> {
        debug_assert_eq!(std::mem::size_of::<T>(), format.sample_bytes());
        match self.format() {
            Some(f) if f == format => {}
            Some(f) => panic!("{} write to a {f} ring", article(format)),
            None => panic!(
                "{} write to a ring whose format is not set",
                article(format)
            ),
        }
        let per_slot = format.samples_per_slot();
        assert!(
            samples.len() <= per_slot,
            "a block of {} samples does not fit a slot of {per_slot}",
            samples.len()
        );
        // Acquire pairs with the consumer's release, so its copy of the slot
        // is finished before the slot is overwritten.
        let tail = self.tail().load(Ordering::Acquire);
        if seq.wrapping_sub(tail) >= self.slots as u64 {
            return Err(RingFull);
        }
        let slot = (seq % self.slots as u64) as usize;
        // SAFETY: the slot lies inside the mapping, holds `SLOT_BYTES >=
        // samples.len() * size_of::<T>()` bytes (checked above), is aligned for
        // `T` (a slot starts 8-byte aligned), and is not being read: the
        // consumer has released every block that used it before (`seq - tail <
        // slots`). `Complex<f32>` and `Complex<i16>` are `repr(C)` pairs, so
        // their bytes are the wire.
        unsafe {
            std::ptr::copy_nonoverlapping(
                samples.as_ptr(),
                self.slot_ptr(slot).cast::<T>(),
                samples.len(),
            );
        }
        self.head().store(seq + 1, Ordering::Release);
        Ok(slot as u32)
    }

    /// Consumer: copies out the `n` samples of block `seq` in `slot`, after
    /// checking the descriptor against the ring, converted to `cf32` by the
    /// ring's format (an `sc16` slot through [`sc16_sample_to_cf32`], the
    /// conversion of [`zeta_core::port::sc16_to_cf32`]); `n` is bounded
    /// by what a slot of that format holds. The slot stays held until
    /// [`Ring::release`].
    ///
    /// `seq` must be the block after the last one released (the node's reader
    /// refuses any other descriptor before it gets here): only then is the
    /// slot one the producer has finished and will not touch until it is
    /// released. These checks alone cannot tell a stale `seq` from the right
    /// one.
    pub fn read(&self, seq: u64, slot: u32, n: u32) -> Result<Arc<[Complex<f32>]>, RingError> {
        let bad = |why: String| Err(RingError::BadDescriptor(why));
        let Some(format) = self.format() else {
            return bad("the ring's format is not set".into());
        };
        if slot as usize >= self.slots {
            return bad(format!("slot {slot} of a ring of {}", self.slots));
        }
        let per_slot = format.samples_per_slot();
        if n as usize > per_slot {
            return bad(format!("{n} samples in a {format} slot of {per_slot}"));
        }
        let want = (seq % self.slots as u64) as u32;
        if slot != want {
            return bad(format!("block {seq} in slot {slot}, not {want}"));
        }
        // Acquire pairs with the producer's store, so the slot's contents are
        // visible here.
        let head = self.head().load(Ordering::Acquire);
        if seq >= head {
            return bad(format!("block {seq} not written yet (head {head})"));
        }
        let n = n as usize;
        let src = self.slot_ptr(slot as usize).cast_const();
        if format == SlotFormat::Sc16 {
            // SAFETY: as below, for `n` `Complex<i16>`s (a slot holds
            // `per_slot >= n` of them, aligned): the producer finished them
            // before `head` passed `seq` and leaves them until the release.
            let raw = unsafe { std::slice::from_raw_parts(src.cast::<Complex<i16>>(), n) };
            // Converted straight into the block's `Arc`: an exact-size iterator
            // is collected in one allocation, with no buffer filled first.
            return Ok(raw.iter().map(|&s| sc16_sample_to_cf32(s)).collect());
        }
        let mut out = Arc::<[Complex<f32>]>::new_uninit_slice(n);
        let dst = Arc::get_mut(&mut out).expect("a fresh Arc is unique");
        // SAFETY: the slot lies inside the mapping and holds at least `n`
        // samples, and `dst` is `n` fresh, distinct elements. The producer
        // finished writing them before `head` passed `seq`, and will not
        // overwrite them until this block is released, because `seq` is the
        // next unreleased block: the caller holds descriptors to the order
        // 0, 1, 2, ... (see above). A misbehaving child writing the slot anyway
        // races only on plain `f32`s (or `i16`s) in shared memory, for which
        // any bit pattern is valid, so the worst it does is garble its own
        // samples.
        unsafe {
            std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr().cast::<Complex<f32>>(), n);
        }
        // SAFETY: all `n` elements were written above.
        Ok(unsafe { out.assume_init() })
    }

    /// Consumer: frees every slot up to and including block `seq`'s.
    pub fn release(&self, seq: u64) {
        self.tail().store(seq + 1, Ordering::Release);
    }

    /// Maps `len` bytes of `fd`, shared and writable.
    fn map(fd: OwnedFd, len: usize, slots: usize) -> io::Result<Ring> {
        // SAFETY: a fresh shared mapping of a descriptor this process owns; the
        // kernel picks the address, so nothing existing is replaced.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let map = NonNull::new(p.cast::<u8>()).ok_or_else(|| io::Error::other("mmap gave null"))?;
        Ok(Ring {
            fd,
            map,
            len,
            slots,
        })
    }

    fn head(&self) -> &AtomicU64 {
        // SAFETY: offset 16 is inside the header, 8-byte aligned (the mapping
        // is page aligned), lives as long as `self`, and is only ever accessed
        // atomically.
        unsafe { AtomicU64::from_ptr(self.map.as_ptr().add(OFF_HEAD).cast()) }
    }

    fn tail(&self) -> &AtomicU64 {
        // SAFETY: as for `head`, at offset 24.
        unsafe { AtomicU64::from_ptr(self.map.as_ptr().add(OFF_TAIL).cast()) }
    }

    fn format_word(&self) -> &AtomicU32 {
        // SAFETY: as for `head`, at offset 32 (4-byte aligned).
        unsafe { AtomicU32::from_ptr(self.map.as_ptr().add(OFF_FORMAT).cast()) }
    }

    /// The first sample of `slot`; the caller keeps `slot < self.slots`.
    fn slot_ptr(&self, slot: usize) -> *mut Complex<f32> {
        debug_assert!(slot < self.slots);
        // SAFETY: `HEADER + slot * SLOT_BYTES` is inside the mapping for any
        // `slot < slots`, and 8-byte aligned.
        unsafe {
            self.map
                .as_ptr()
                .add(HEADER + slot * SLOT_BYTES)
                .cast::<Complex<f32>>()
        }
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: `map` and `len` are the mapping made in `Ring::map`, and no
        // reference into it outlives `self`.
        unsafe { libc::munmap(self.map.as_ptr().cast(), self.len) };
    }
}

/// `a cf32` or `an sc16`, for the panics of a write in the wrong format.
fn article(f: SlotFormat) -> &'static str {
    match f {
        SlotFormat::Cf32 => "a cf32",
        SlotFormat::Sc16 => "an sc16",
    }
}

/// Whether this kernel gives out memfds: a 4 KiB one is created and closed.
pub fn supported() -> bool {
    memfd(c"zeta-ring-probe", 4096).is_ok()
}

/// A close-on-exec memfd of `len` bytes, so it reaches a child only where
/// the spawner hands it over on purpose. Its size is sealed: a child that
/// shrank the file would turn the node's next read of the mapping into a
/// SIGBUS.
fn memfd(name: &std::ffi::CStr, len: usize) -> io::Result<OwnedFd> {
    // SAFETY: `name` is a valid C string; the call takes no other pointers.
    let raw =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a descriptor just opened and owned by nobody else.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let size = libc::off_t::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "ring too large"))?;
    // SAFETY: a plain syscall on a descriptor this process owns.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(n: usize, base: f32) -> Vec<Complex<f32>> {
        (0..n)
            .map(|i| Complex::new(base + i as f32, -(i as f32)))
            .collect()
    }

    fn ring4() -> Ring {
        let ring = Ring::create(64 + MIN_SLOTS * SLOT_BYTES).expect("create a 4-slot ring");
        ring.set_format(SlotFormat::Cf32)
            .expect("a fresh ring takes a format");
        ring
    }

    fn sc16_ring4() -> Ring {
        let ring = Ring::create(64 + MIN_SLOTS * SLOT_BYTES).expect("create a 4-slot ring");
        ring.set_format(SlotFormat::Sc16)
            .expect("a fresh ring takes a format");
        ring
    }

    /// `n` raw samples spanning the whole `i16` range, both ends included.
    fn raw(n: usize) -> Vec<Complex<i16>> {
        (0..n - 1)
            .map(|i| {
                let v = (i as i64 * 7919 % 65_536 - 32_768) as i16;
                Complex::new(v, v.wrapping_neg().wrapping_sub(1))
            })
            .chain([Complex::new(i16::MIN, i16::MAX)])
            .collect()
    }

    fn bits(x: &[Complex<f32>]) -> Vec<(u32, u32)> {
        x.iter().map(|s| (s.re.to_bits(), s.im.to_bits())).collect()
    }

    #[test]
    fn an_sc16_slot_holds_twice_the_samples() {
        let ring = sc16_ring4();
        assert_eq!(SlotFormat::Sc16.samples_per_slot(), 2 * SLOT_SAMPLES);
        assert_eq!(SlotFormat::Cf32.samples_per_slot(), SLOT_SAMPLES);
        let samples = raw(2 * SLOT_SAMPLES);
        assert_eq!(ring.try_write_sc16(0, &samples), Ok(0));
        let got = ring
            .read(0, 0, (2 * SLOT_SAMPLES) as u32)
            .expect("a full sc16 slot reads back");
        assert_eq!(got.len(), 2 * SLOT_SAMPLES);
        // One sample more than an sc16 slot holds is refused, as one more
        // than a cf32 slot holds is on a cf32 ring.
        assert!(matches!(
            ring.read(0, 0, (2 * SLOT_SAMPLES) as u32 + 1),
            Err(RingError::BadDescriptor(_))
        ));
        assert!(matches!(
            ring4().read(0, 0, SLOT_SAMPLES as u32 + 1),
            Err(RingError::BadDescriptor(_))
        ));
    }

    #[test]
    #[should_panic(expected = "does not fit a slot")]
    fn an_sc16_block_longer_than_a_slot_panics() {
        let _ = sc16_ring4().try_write_sc16(0, &raw(2 * SLOT_SAMPLES + 1));
    }

    #[test]
    fn sc16_and_cf32_slots_read_bit_identical_blocks() {
        // The same samples, written raw to an sc16 ring and converted by the
        // one shared function to a cf32 one: the node reads the same bits.
        let samples = raw(100_000);
        let mut converted = vec![Complex::new(0.0f32, 0.0); samples.len()];
        zeta_core::port::sc16_to_cf32(&samples, &mut converted);
        let (sc16, cf32) = (sc16_ring4(), ring4());
        assert_eq!(sc16.try_write_sc16(0, &samples), Ok(0));
        assert_eq!(cf32.try_write(0, &converted), Ok(0));
        let n = samples.len() as u32;
        let a = sc16.read(0, 0, n).expect("the sc16 slot");
        let b = cf32.read(0, 0, n).expect("the cf32 slot");
        assert_eq!(bits(&a), bits(&b));
        assert_eq!(
            a[samples.len() - 1],
            Complex::new(-1.0, 32_767.0 / 32_768.0)
        );
    }

    #[test]
    fn the_format_is_set_once() {
        let ring = Ring::create(64 + MIN_SLOTS * SLOT_BYTES).unwrap();
        assert_eq!(ring.format(), None);
        ring.set_format(SlotFormat::Sc16).unwrap();
        // The same format again is no change; another is refused, naming both.
        ring.set_format(SlotFormat::Sc16).unwrap();
        let e = ring.set_format(SlotFormat::Cf32).unwrap_err();
        assert_eq!(e.to_string(), "the ring's format is sc16 already, not cf32");
        assert_eq!(ring.format(), Some(SlotFormat::Sc16));
        // The consumer sees the producer's format.
        let consumer = Ring::open(ring.fd().try_clone_to_owned().unwrap()).unwrap();
        assert_eq!(consumer.format(), Some(SlotFormat::Sc16));
    }

    #[test]
    fn a_ring_of_version_1_is_refused_naming_both_versions() {
        let ring = ring4();
        // SAFETY: the version field lies inside the header, and nothing else
        // touches this test's ring.
        unsafe { ring.map.as_ptr().add(OFF_VERSION).cast::<u32>().write(1) };
        let e = Ring::open(ring.fd().try_clone_to_owned().unwrap())
            .err()
            .expect("refused");
        assert_eq!(e.to_string(), "ring version 1, not 2");
    }

    #[test]
    #[should_panic(expected = "a cf32 write to a ring whose format is not set")]
    fn a_write_before_the_format_is_refused() {
        let ring = Ring::create(64 + MIN_SLOTS * SLOT_BYTES).unwrap();
        let _ = ring.try_write(0, &block(10, 0.0));
    }

    #[test]
    #[should_panic(expected = "an sc16 write to a cf32 ring")]
    fn a_write_in_the_other_format_is_refused() {
        let _ = ring4().try_write_sc16(0, &raw(10));
    }

    #[test]
    fn a_read_before_the_format_is_set_is_refused() {
        let fresh = Ring::create(64 + MIN_SLOTS * SLOT_BYTES).unwrap();
        assert!(matches!(
            fresh.read(0, 0, 0),
            Err(RingError::BadDescriptor(why)) if why.contains("format")
        ));
    }

    #[test]
    fn a_block_round_trips_through_a_slot() {
        let ring = ring4();
        assert_eq!(ring.slots(), 4);
        let samples = block(1000, 0.5);
        assert_eq!(ring.try_write(0, &samples), Ok(0));
        let got = ring.read(0, 0, 1000).expect("read slot 0");
        assert_eq!(&got[..], &samples[..]);
        ring.release(0);
    }

    #[test]
    fn a_full_ring_refuses_until_released() {
        let ring = ring4();
        for seq in 0..4u64 {
            assert_eq!(ring.try_write(seq, &block(10, seq as f32)), Ok(seq as u32));
        }
        assert_eq!(ring.try_write(4, &block(10, 4.0)), Err(RingFull));
        ring.release(0);
        assert_eq!(ring.try_write(4, &block(10, 4.0)), Ok(0));
    }

    #[test]
    fn a_descriptor_that_does_not_match_the_sequence_is_refused() {
        let ring = ring4();
        assert!(matches!(
            ring.read(5, 0, 10),
            Err(RingError::BadDescriptor(_))
        ));
        assert!(matches!(
            ring.read(0, 9, 10),
            Err(RingError::BadDescriptor(_))
        ));
        assert!(matches!(
            ring.read(0, 0, SLOT_SAMPLES as u32 + 1),
            Err(RingError::BadDescriptor(_))
        ));
    }

    #[test]
    fn open_on_the_duplicated_fd_sees_the_producers_writes() {
        let producer = ring4();
        let consumer = Ring::open(producer.fd().try_clone_to_owned().expect("dup the fd"))
            .expect("open the ring");
        assert_eq!(consumer.slots(), producer.slots());
        assert_eq!(consumer.bytes(), producer.bytes());
        let samples = block(SLOT_SAMPLES, 3.0);
        assert_eq!(producer.try_write(0, &samples), Ok(0));
        let got = consumer.read(0, 0, SLOT_SAMPLES as u32).expect("read");
        assert_eq!(&got[..], &samples[..]);
        // The consumer's release frees the producer's slot.
        for seq in 1..4u64 {
            assert_eq!(producer.try_write(seq, &block(1, 0.0)), Ok(seq as u32));
        }
        assert_eq!(producer.try_write(4, &block(1, 0.0)), Err(RingFull));
        consumer.release(0);
        assert_eq!(producer.try_write(4, &block(1, 0.0)), Ok(0));
    }

    #[test]
    fn open_makes_an_inherited_fd_close_on_exec() {
        let producer = ring4();
        // A plain `dup` is not close-on-exec, like a descriptor a child inherits.
        // SAFETY: `dup` of a live descriptor; the copy is owned by nobody else.
        let fd = unsafe { OwnedFd::from_raw_fd(libc::dup(producer.fd().as_raw_fd())) };
        // SAFETY: `fcntl(F_GETFD)` on a live descriptor.
        let before = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert_eq!(before & libc::FD_CLOEXEC, 0, "flags {before:#x}");
        let consumer = Ring::open(fd).expect("open the ring");
        // SAFETY: as above, on the ring's own descriptor.
        let flags = unsafe { libc::fcntl(consumer.fd().as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "flags {flags:#x}");
    }

    #[test]
    fn a_ring_too_small_for_min_slots_is_refused() {
        assert!(Ring::create(1 << 20).is_err());
    }
}
