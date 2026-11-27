//! What shaders touch lives on the card's side: the default on NVIDIA cards
//! (`VICTORY_CARD_SIDE=0`: the placement of before), and `VICTORY_CARD_SIDE=1`
//! elsewhere, where it is not measured yet.
//!
//! A host copy in the PC's memory is read and written by shaders across the
//! bus. In a timing pair on an RTX 5090 (runs integ-t3-base and
//! integ-t3-allcard, the same scene) the card's work went from 53.8 ms a
//! frame to 26.6 with every title buffer's copy and the windows of title
//! memory on the card (`VICTORY_CARD_BUFFERS_MIN_KB=64
//! VICTORY_WINDOWS_BAR_MB=2048`); dispatches from 29.9 to 7.5 ms. The two
//! shadow-mask programs went from 5.6 and 2.1 ms to 0.2 and 0.3: their loads
//! reach 4 MiB windows. The processor's side did not grow much: 191 MB a
//! frame up in 2,077 ranges, against 185 MB in 1,488.
//!
//! With the switch, the sizes are chosen by the card's budget (the system's,
//! `video_memory`) where no switch of their own says otherwise
//! ([`set_for`]):
//! - every title buffer's copy on the card (a copy is 64 KiB at least, so a
//!   least of 64 KiB takes them all), up to a total by budget;
//! - windows of title memory outside buffers on the card, up to a total by
//!   budget: written by the CPU through the bus into card memory it maps
//!   (`VICTORY_WINDOWS_BAR_MB`'s way) where the card and the system allow it
//!   (`D3D12_FEATURE_DATA_D3D12_OPTIONS16::GPUUploadHeapSupported`, which
//!   needs resizable BAR), else with two sides (`VICTORY_CARD_WINDOWS_MB`'s
//!   way). `VICTORY_WINDOWS_BAR_MB=0` asks for two sides: see "Measured"
//!   below;
//! - the texture queue keeps the room of both totals, less what they hold
//!   already (`card_room_kept`), so that the copies made after the textures
//!   came still find it.
//!
//! The two ways for windows. Through the bus the CPU writes the window once,
//! where the card reads it, and nothing more (no CPU side, no list of what
//! changed, no copy before each submission, no hash). Two sides cost a second
//! copy of every byte the title changes (into the submission's own memory)
//! and a copy command on the card, and give in return what the bus does not:
//! a submission in the air keeps the bytes it was prepared with, and a shader
//! write to a window is compared and brought back on the card.
//!
//! Measured (RTX 5090, the same scene, the process at high priority; the
//! median frame of each run's last minute, and what the thread that prepares
//! submissions worked a frame). The placement of before: 85.5 ms (79.8). The
//! windows through the bus, three runs: 62.1 (56.3), 71.8 (67.8), 74.0 (68.8).
//! Two-sided windows, four runs: 66.7 (65.3), 69.6 (63.0), 83.7 (84.7), 74.5
//! (73.7). The later runs of both had other programs on the card and the
//! processor. Run for run the bus is the faster or the equal one, it spares
//! that thread about 7 ms, and it keeps no second copy of the windows in the
//! PC's memory (up to the windows' total again), so it is the placement's
//! choice where it exists.
//!
//! (0.13.0 chose two sides, from figures that did not hold: the runs' means,
//! which a slow first minute weighed on unequally, and one line of the card's
//! clock each.)
//!
//! Every switch of its own still wins: `VICTORY_CARD_BUFFERS_MIN_KB`,
//! `_MIN_MB`, `VICTORY_CARD_BUFFERS_MB`, `VICTORY_WINDOWS_BAR_MB`,
//! `VICTORY_CARD_WINDOWS_MB`.

use std::sync::OnceLock;

/// Whether the placement is by [`set_for`]: `VICTORY_CARD_SIDE=1` or `=0`
/// says so; unsaid, it is on NVIDIA cards, where it is measured (asked once
/// the host is there: [`set`]).
pub(crate) fn on() -> bool {
    const NVIDIA: u32 = 0x10DE;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| match std::env::var("VICTORY_CARD_SIDE").ok().as_deref().map(str::trim) {
        Some("1") => true,
        Some("0") => false,
        _ => crate::host::host().is_some_and(|h| h.adapter_vendor_id() == Some(NVIDIA)),
    })
}

/// The sizes of the placement, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Set {
    /// The least size of a title buffer whose copy goes to the card.
    pub(crate) buffers_least: u64,
    /// The most bytes of such copies.
    pub(crate) buffers_most: u64,
    /// The most bytes of windows on the card.
    pub(crate) windows_most: u64,
}

/// The set for a card whose budget is `budget` bytes (`None`: not known, as
/// on the software adapter: nothing is changed). By the size of the card it
/// is on (the budget is about nine tenths of it):
///
/// | card | budget | buffers | windows |
/// |---|---|---|---|
/// | 8 GB | up to 9 GiB | 768 MB | 512 MB |
/// | 12 GB | up to 13 GiB | 1 GB | 1 GB |
/// | 16 GB | up to 20 GiB | 1.5 GB | 1.5 GB |
/// | 24 GB | up to 26 GiB | 2 GB | 2 GB |
/// | 32 GB and more | more | 3 GB | 3 GB |
///
/// What was seen: title buffers of 64 KiB and more held 1.3 GB in 3,714
/// copies, the windows 2 GB of a 2 GB limit and 3.3 GB in a captured frame;
/// the textures picked by index took the rest of a 32 GB card. On the
/// smaller cards the copies made first take the room and the later ones
/// stay in the PC's memory, as before. Only the 32 GB row is measured; the
/// 8 GB and 12 GB rows are guesses still to be measured.
pub(crate) fn set_for(budget: Option<u64>) -> Option<Set> {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    let budget = budget?;
    let (buffers, windows) = match budget {
        b if b <= 9 * GIB => (768, 512),
        b if b <= 13 * GIB => (1024, 1024),
        b if b <= 20 * GIB => (1536, 1536),
        b if b <= 26 * GIB => (2048, 2048),
        _ => (3072, 3072),
    };
    Some(Set {
        buffers_least: 64 << 10,
        buffers_most: buffers * MIB,
        windows_most: windows * MIB,
    })
}

/// The set in force: [`set_for`] the host's budget with the switch on,
/// read once (and said once).
pub(crate) fn set() -> Option<Set> {
    static SET: OnceLock<Option<Set>> = OnceLock::new();
    *SET.get_or_init(|| {
        if !on() {
            return None;
        }
        let host = crate::host::host()?;
        let budget = host.video_memory().map(|[budget, ..]| budget);
        let set = set_for(budget);
        match set {
            Some(set) => tracing::info!(
                budget_mb = budget.map(|b| b >> 20),
                buffers_least_kb = set.buffers_least >> 10,
                buffers_most_mb = set.buffers_most >> 20,
                windows_most_mb = set.windows_most >> 20,
                windows_through_the_bus = host.mapped_card_memory(),
                "what shaders touch lives on the card's side (VICTORY_CARD_SIDE=0: the placement of before); a switch of its own overrides each size"
            ),
            None => tracing::info!("the card's budget is not known: the placement stays as it was (VICTORY_CARD_SIDE)"),
        }
        set
    })
}

/// The size a switch of its own (`name`, in MiB) or the set gives.
pub(crate) fn mb_or(name: &str, from_set: impl FnOnce(Set) -> u64) -> Option<u64> {
    match std::env::var(name).ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(mb) => Some(mb << 20),
        None => set().map(from_set),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_set_grows_with_the_card() {
        const GIB: u64 = 1 << 30;
        const MIB: u64 = 1 << 20;
        assert_eq!(set_for(None), None);
        let sizes = |gib: u64| {
            let set = set_for(Some(gib * GIB)).unwrap();
            assert_eq!(set.buffers_least, 64 << 10, "every title buffer");
            (set.buffers_most / MIB, set.windows_most / MIB)
        };
        // Budgets as the system gives them for cards of 8, 12, 16, 24 and 32 GB.
        assert_eq!(sizes(7), (768, 512));
        assert_eq!(sizes(11), (1024, 1024));
        assert_eq!(sizes(15), (1536, 1536));
        assert_eq!(sizes(22), (2048, 2048));
        assert_eq!(sizes(30), (3072, 3072));
    }
}
