//! A shared-memory ring of IQ slots between the node and a worker child.
//!
//! The stdout pipe cannot carry tens of megasamples a second, so blocks travel
//! through a memfd instead: the node creates the ring, the child inherits its
//! descriptor, the child copies each block into a fixed slot and tells the node
//! which one over the pipe, and the node copies the slot out and releases it.
//!
//! The mapping starts with a 64-byte header: the magic `b"ZRNG"`, the version
//! (`u32`), the samples per slot (`u32`) and the slot count (`u32`), then
//! `head` and `tail` as `AtomicU64` at offsets 16 and 24; the rest is zero.
//! Slot `i` starts at byte `64 + i * SLOT_SAMPLES * 8`. Block `seq` lives in
//! slot `seq % slots`; the producer stores `head = seq + 1` once it is
//! written, and the consumer stores `tail = seq + 1` once it has copied it
//! out, which frees the slot. The producer refuses a block while `slots`
//! blocks are unreleased.
//!
//! Header fields and samples are native endian (samples as `f32` pairs): both
//! ends are the same executable on the same machine, so there is nothing to
//! convert.

use std::fmt;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use num_complex::Complex;

/// Samples per slot: 2 MiB of `cf32`, about 70 ms at 30 Msps.
pub const SLOT_SAMPLES: usize = 262_144;

/// The ring size the node asks for when nothing else is said: 64 MiB, which
/// is 31 slots.
pub const DEFAULT_RING_BYTES: usize = 64 << 20;

/// The fewest slots a ring may have, so the producer can keep writing while
/// the consumer is still copying.
pub const MIN_SLOTS: usize = 4;

const MAGIC: [u8; 4] = *b"ZRNG";
const VERSION: u32 = 1;
const HEADER: usize = 64;
const SLOT_BYTES: usize = SLOT_SAMPLES * std::mem::size_of::<Complex<f32>>();
const OFF_VERSION: usize = 4;
const OFF_SLOT_SAMPLES: usize = 8;
const OFF_SLOTS: usize = 12;
const OFF_HEAD: usize = 16;
const OFF_TAIL: usize = 24;

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
// header is written once before the ring is shared and only read after; `head`
// and `tail` are atomics. The only unsynchronised bytes are slot contents,
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
        // A fresh memfd is zero, so `head`, `tail` and the padding start at 0.
        unsafe {
            base.cast::<[u8; 4]>().write(MAGIC);
            base.add(OFF_VERSION).cast::<u32>().write(VERSION);
            base.add(OFF_SLOT_SAMPLES)
                .cast::<u32>()
                .write(SLOT_SAMPLES as u32);
            base.add(OFF_SLOTS).cast::<u32>().write(slots_u32);
        }
        Ok(ring)
    }

    /// Maps a ring another process created, checking its header and that the
    /// file is exactly the size the header says.
    pub fn open(fd: OwnedFd) -> io::Result<Ring> {
        let bad = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
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
        let (magic, version, slot_samples, slots) = unsafe {
            (
                base.cast::<[u8; 4]>().read(),
                base.add(OFF_VERSION).cast::<u32>().read(),
                base.add(OFF_SLOT_SAMPLES).cast::<u32>().read(),
                base.add(OFF_SLOTS).cast::<u32>().read(),
            )
        };
        if magic != MAGIC {
            return Err(bad(format!("ring magic {magic:?}, not {MAGIC:?}")));
        }
        if version != VERSION {
            return Err(bad(format!("ring version {version}, not {VERSION}")));
        }
        if slot_samples as usize != SLOT_SAMPLES {
            return Err(bad(format!(
                "ring slots of {slot_samples} samples, not {SLOT_SAMPLES}"
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

    /// Producer: copies block `seq` into slot `seq % slots` and publishes it,
    /// returning the slot index, or refuses while `slots` blocks are
    /// unreleased.
    ///
    /// # Panics
    ///
    /// When `samples` is longer than [`SLOT_SAMPLES`].
    pub fn try_write(&self, seq: u64, samples: &[Complex<f32>]) -> Result<u32, RingFull> {
        assert!(
            samples.len() <= SLOT_SAMPLES,
            "a block of {} samples does not fit a slot of {SLOT_SAMPLES}",
            samples.len()
        );
        // Acquire pairs with the consumer's release, so its copy of the slot
        // is finished before the slot is overwritten.
        let tail = self.tail().load(Ordering::Acquire);
        if seq.wrapping_sub(tail) >= self.slots as u64 {
            return Err(RingFull);
        }
        let slot = (seq % self.slots as u64) as usize;
        // SAFETY: the slot lies inside the mapping, holds `SLOT_SAMPLES >=
        // samples.len()` samples, and is not being read: the consumer has
        // released every block that used it before (`seq - tail < slots`).
        // `Complex<f32>` is `repr(C)`, two `f32`s, so its bytes are the wire.
        unsafe {
            std::ptr::copy_nonoverlapping(samples.as_ptr(), self.slot_ptr(slot), samples.len());
        }
        self.head().store(seq + 1, Ordering::Release);
        Ok(slot as u32)
    }

    /// Consumer: copies out the `n` samples of block `seq` in `slot`, after
    /// checking the descriptor against the ring. The slot stays held until
    /// [`Ring::release`].
    pub fn read(&self, seq: u64, slot: u32, n: u32) -> Result<Arc<[Complex<f32>]>, RingError> {
        let bad = |why: String| Err(RingError::BadDescriptor(why));
        if slot as usize >= self.slots {
            return bad(format!("slot {slot} of a ring of {}", self.slots));
        }
        if n as usize > SLOT_SAMPLES {
            return bad(format!("{n} samples in a slot of {SLOT_SAMPLES}"));
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
        let mut out = Arc::<[Complex<f32>]>::new_uninit_slice(n as usize);
        let dst = Arc::get_mut(&mut out).expect("a fresh Arc is unique");
        // SAFETY: the slot lies inside the mapping and holds at least `n`
        // samples, which the producer finished writing before `head` passed
        // `seq`; it will not overwrite them until this block is released.
        // `dst` is `n` fresh, distinct elements.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.slot_ptr(slot as usize).cast_const(),
                dst.as_mut_ptr().cast::<Complex<f32>>(),
                n as usize,
            );
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

/// Whether this kernel gives out memfds: a 4 KiB one is created and closed.
pub fn supported() -> bool {
    memfd(c"zeta-ring-probe", 4096).is_ok()
}

/// A close-on-exec memfd of `len` bytes, so it reaches a child only where
/// the spawner hands it over on purpose.
fn memfd(name: &std::ffi::CStr, len: usize) -> io::Result<OwnedFd> {
    // SAFETY: `name` is a valid C string; the call takes no other pointers.
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
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
        Ring::create(64 + MIN_SLOTS * SLOT_SAMPLES * 8).expect("create a 4-slot ring")
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
    fn a_ring_too_small_for_min_slots_is_refused() {
        assert!(Ring::create(1 << 20).is_err());
    }
}
