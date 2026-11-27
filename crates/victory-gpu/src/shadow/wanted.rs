//! Copies in the card's memory for what shaders do with a buffer, not for its
//! size alone.
//!
//! A title buffer's copy goes to the card's memory if it is big
//! (`card_limits`: 1 MiB on a card of 32 GB); the small ones stay in the PC's
//! memory, and the card reaches them over the bus. Of 57.5 ms of the card's
//! work a frame (RTX 5090), about 11 ms went to nine small programs whose
//! atomics and stores land on small title buffers there: one atomic a wave on
//! a counter of 88 bytes took 4 ms, and atomics on one address across the bus
//! wait for each other. Putting every buffer of 64 KiB or more on the card
//! (`VICTORY_CARD_BUFFERS_MIN_KB=64`, 3,714 copies) sent 36,000 to 49,000
//! ranges a frame up to them and did not make the frame shorter.
//!
//! So a copy is made on the card, whatever its size, when the bind that asks
//! for it knows that a shader does atomics on it (`VICTORY_CARD_ATOMIC=1`), or
//! stores to it and it is under `VICTORY_CARD_WRITTEN_KB` (0: none). The bind
//! says so while it asks ([`asking`]); a copy made earlier for another
//! use stays where it is, and is counted.

use super::*;
use std::cell::Cell;

/// What a shader does with the buffer a bind asks the copy of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Use {
    /// Reads it, or nothing known.
    Read,
    /// May store to it (an unordered access view).
    Written,
    /// Does atomics on it.
    Atomic,
}

thread_local! {
    static USE: Cell<Use> = const { Cell::new(Use::Read) };
}

/// Whether a buffer shaders do atomics on goes to the card at any size
/// (`VICTORY_CARD_ATOMIC=1`; off by default).
fn atomic_on_card() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CARD_ATOMIC").is_some_and(|v| v == "1"))
}

/// The bytes under which a buffer shaders store to goes to the card
/// (`VICTORY_CARD_WRITTEN_KB`, in KiB; 0, the default: none).
fn written_most() -> u64 {
    static MOST: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MOST.get_or_init(|| {
        std::env::var("VICTORY_CARD_WRITTEN_KB").ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0) << 10
    })
}

/// Whether anything is asked of this module.
fn on() -> bool {
    cards() && (atomic_on_card() || written_most() > 0)
}

/// While it lives, the copies this thread asks for are asked for a shader
/// that does what [`asking`] was told with them.
pub(crate) struct Asking(Option<Use>);

impl Drop for Asking {
    fn drop(&mut self) {
        if let Some(was) = self.0 {
            USE.set(was);
        }
    }
}

/// The copies asked for until the result is dropped are for `wanted` (see
/// the module).
pub(crate) fn asking(wanted: Use) -> Asking {
    if wanted == Use::Read || !on() {
        return Asking(None);
    }
    set(wanted)
}

/// [`asking`], whatever the switches say.
fn set(wanted: Use) -> Asking {
    Asking(Some(USE.replace(wanted)))
}

/// What a shader does with the buffer of a view it uses as `kind`, as far
/// as that tells: a view of unordered access may be stored through.
pub(crate) fn by_kind(kind: Option<victory_shader::native::ResourceType>) -> Use {
    if kind == Some(victory_shader::native::ResourceType::Uav) { Use::Written } else { Use::Read }
}

/// Whether a copy of a buffer of `len` bytes made now goes to the card's
/// memory for what the asking bind does with it.
pub(super) fn made_on_card(len: u64) -> bool {
    wants(USE.get(), len, atomic_on_card(), written_most())
}

/// Whether a copy of `len` bytes that a shader uses as `used` goes to the
/// card: `atomic`, at any size for atomics; under `most` bytes for stores.
fn wants(used: Use, len: u64, atomic: bool, most: u64) -> bool {
    match used {
        Use::Read => false,
        Use::Written => len < most,
        Use::Atomic => atomic || len < most,
    }
}

/// Since the start: copies made on the card for a shader's stores, for its
/// atomics, and their bytes; copies such a bind found in the PC's memory,
/// made earlier for another use (each once), and their bytes.
static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// A copy of `len` bytes was made on the card because [`made_on_card`] said so.
pub(super) fn made(len: u64) {
    COUNTS[if USE.get() == Use::Atomic { 1 } else { 0 }].fetch_add(1, Ordering::Relaxed);
    COUNTS[2].fetch_add(len, Ordering::Relaxed);
}

/// A bind that would have had `shadow` on the card found it in the PC's
/// memory.
pub(super) fn found(shadow: &Shadow) {
    if shadow.card.is_some()
        || shadow.window
        || !made_on_card(shadow.len)
        || shadow.wanted_noted.swap(true, Ordering::Relaxed)
    {
        return;
    }
    COUNTS[3].fetch_add(1, Ordering::Relaxed);
    COUNTS[4].fetch_add(shadow.len, Ordering::Relaxed);
}

/// The frame line's (since the start).
pub(crate) fn log() {
    if !on() {
        return;
    }
    let c = COUNTS.each_ref().map(|a| a.load(Ordering::Relaxed));
    tracing::info!(
        "copies in the card's memory for what shaders do (VICTORY_CARD_ATOMIC={}, VICTORY_CARD_WRITTEN_KB={}), since the start: \
         {} made there for stores and {} for atomics ({} KB); {} that such a bind found made in the PC's memory already ({} KB), \
         left there",
        u8::from(atomic_on_card()),
        written_most() >> 10,
        c[0],
        c[1],
        c[2] >> 10,
        c[3],
        c[4] >> 10,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomics_go_to_the_card_at_any_size_stores_under_the_threshold() {
        const KB: u64 = 1 << 10;
        // Reads never.
        assert!(!wants(Use::Read, 88, true, 512 * KB));
        // Stores under the threshold, not at it, and not with none.
        assert!(wants(Use::Written, 32 * KB, false, 512 * KB));
        assert!(!wants(Use::Written, 512 * KB, false, 512 * KB));
        assert!(!wants(Use::Written, 4, false, 0));
        // Atomics at any size with the switch, else as stores.
        assert!(wants(Use::Atomic, 64 << 20, true, 0));
        assert!(wants(Use::Atomic, 88, false, 512 * KB));
        assert!(!wants(Use::Atomic, 88, false, 0));
    }

    #[test]
    fn what_a_bind_asks_for_ends_with_it() {
        assert_eq!(USE.get(), Use::Read);
        {
            let _written = set(Use::Written);
            assert_eq!(USE.get(), Use::Written);
            {
                let _atomic = set(Use::Atomic);
                assert_eq!(USE.get(), Use::Atomic);
            }
            assert_eq!(USE.get(), Use::Written);
            // Without the switches nothing is asked.
            let _read = asking(Use::Atomic);
            assert_eq!(USE.get(), Use::Written);
        }
        assert_eq!(USE.get(), Use::Read);
        assert_eq!(by_kind(Some(victory_shader::native::ResourceType::Uav)), Use::Written);
        assert_eq!(by_kind(Some(victory_shader::native::ResourceType::Srv)), Use::Read);
        assert_eq!(by_kind(None), Use::Read);
    }
}
