//! The switches the title is played with, as `victory run`'s defaults: the
//! set `scripts\play.ps1` gave (measured on an RX 7900 XTX, README.md), so
//! that a plain `victory run` behaves as the script did. A switch the
//! environment sets keeps its value; `VICTORY_PLAY_DEFAULTS=0` leaves every
//! one at the layer's own default, `=safe` takes the script's `-Safe` set.
//!
//! Not among them: `VICTORY_AMBIENT` (the stand-in for the traced indirect
//! light, off unless set: `VICTORY_AMBIENT=0.8` as the script had it) and
//! `VICTORY_BVH_BUILDER` (the ray-tracing structure builder, off unless set
//! to 1). Copies of textures shaders write back into title memory are on by
//! the layer's own default.

/// Name and value of each switch, as the script set them.
const PLAY: &[(&str, &str)] = &[
    ("VICTORY_TEXTURE_NO_WAIT", "1"),
    ("VICTORY_RELAY", "2"),
    ("VICTORY_WIDE_BACK", "1"),
    ("VICTORY_CARD_REHASH_RUNS", "1"),
    ("VICTORY_JOIN_COPIES", "1"),
    ("VICTORY_INDIRECT_WINDOW", "1"),
    ("VICTORY_RELAY_COPY", "1"),
    ("VICTORY_CARD_STEPS", "2"),
    ("VICTORY_INDIRECT_BATCH", "2"),
    ("VICTORY_MAP_THREADS", "1"),
    ("VICTORY_TITLE_PRIORITIES", "0"),
    ("VICTORY_CARD_AWAKE", "1"),
    ("VICTORY_SHOW_AFTER", "1"),
    // (Crash tracing that can stay on; diagnostics builds only.)
    ("VICTORY_D3D12_DRED", "light"),
];

/// The script's newest ways, left out by its `-Safe`.
const FAST: &[(&str, &str)] = &[("VICTORY_THIN", "1"), ("VICTORY_JOIN_PARTS", "8"), ("VICTORY_UPLOAD_AHEAD", "1")];

/// What `-Safe` set in their place.
const SAFE: &[(&str, &str)] = &[("VICTORY_UNDER_WIDE", "0"), ("VICTORY_LAND_ECHO", "0")];

/// What our wave 12 did unasked, measured on an RTX 5090 (0 tables refused,
/// the picture right), and the RX 7900 XTX fork made switches of, off there:
/// codec-d/cool_fox's memory budgets, heaps of one slot where a list sets
/// none, the heap buffer pass per heap, XMA voices refilled after their end,
/// a DirectStorage staging floor; and textures at their full size.
const ARCHIVE: &[(&str, &str)] = &[
    ("VICTORY_RESIDENCY", "1"),
    ("VICTORY_FALLBACK_HEAPS", "1"),
    ("VICTORY_SWEEP_PER_HEAP", "1"),
    ("VICTORY_XMA_REFILL", "1"),
    ("VICTORY_DSTORAGE_STAGING_MB", "256"),
    // The RX 9070 XT work (beloooo, e9428969) skips the largest levels of
    // streamed textures past 1024 texels a side unless told (0: none), for
    // cards of 16 GB; with 32 the title's own sizes, as in wave 12.
    ("VICTORY_TEXTURE_MAX", "0"),
];

/// The defaults for the value of `VICTORY_PLAY_DEFAULTS` (unset: the play
/// set; `0`: none; `safe`: the play set without the newest ways).
fn wanted(choice: Option<&str>) -> Vec<(&'static str, &'static str)> {
    match choice.map(str::trim) {
        Some("0") => Vec::new(),
        Some("safe") => PLAY.iter().chain(ARCHIVE).chain(SAFE).copied().collect(),
        _ => PLAY.iter().chain(ARCHIVE).chain(FAST).copied().collect(),
    }
}

/// Sets each default the environment does not set, and says which. Called
/// first thing in `main` for `victory run`, before any thread reads the
/// environment.
pub(crate) fn apply() {
    let set = set();
    if !set.is_empty() {
        println!("defaults       {} (VICTORY_PLAY_DEFAULTS=0: none)", set.join(" "));
    }
}

/// Sets each default the environment does not set, and returns them as
/// `NAME=value` (`pilotebox play` says it shorter). Before any thread
/// reads the environment.
pub(crate) fn set() -> Vec<String> {
    let choice = std::env::var("VICTORY_PLAY_DEFAULTS").ok();
    let mut set = Vec::new();
    for (name, value) in wanted(choice.as_deref()) {
        if std::env::var_os(name).is_none() {
            // SAFETY: the process has one thread yet (`main` calls this
            // before it starts any, and before the title's).
            unsafe { std::env::set_var(name, value) };
            set.push(format!("{name}={value}"));
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The play set by default; the script's `-Safe` without its newest
    /// ways; nothing for `0`; the stand-in light and the structure builder
    /// never.
    #[test]
    fn the_play_set_is_the_default() {
        let names = |choice| wanted(choice).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        let all = names(None);
        assert!(all.contains(&"VICTORY_THIN") && all.contains(&"VICTORY_RELAY_COPY"));
        assert_eq!(all.len(), PLAY.len() + ARCHIVE.len() + FAST.len());
        let safe = names(Some("safe"));
        assert!(!safe.contains(&"VICTORY_THIN") && safe.contains(&"VICTORY_UNDER_WIDE"));
        assert!(names(Some("0")).is_empty());
        for choice in [None, Some("safe")] {
            assert!(!names(choice).iter().any(|n| *n == "VICTORY_AMBIENT" || *n == "VICTORY_BVH_BUILDER"));
        }
    }
}
