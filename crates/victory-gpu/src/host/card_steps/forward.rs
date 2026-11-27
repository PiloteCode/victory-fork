//! A fill of upload memory that reads only what a copy at its place in a list
//! wrote just before it reads the copy's source instead
//! (`VICTORY_FILL_FORWARD`, on; `=0`: it waits for the flights, as before).
//!
//! The one settle left in a steady frame (38 ms a frame of the preparing
//! thread, RTX 5090, runs integ-q2-base and integ-q2-both) was a constant
//! buffer of the title's: a list copies 240 bytes out of the title's heap
//! into its ring of constants (a copy at its place, `place.rs`), and a draw
//! of the same list reads them as constants (a fill, done when the list is
//! submitted). The flights in the air may write the heap (shaders write
//! through a view of all of it), so title memory does not take the copy at
//! submission, and the fill waited for every flight to land. Done on the
//! card in front of the lists it would read the ring before the copy has
//! written it, and the copy and the fill are in the same list, so no cut
//! between lists parts them (`VICTORY_PLACED_ORDER`).
//!
//! What the fill reads is what the copy wrote, which is what the copy read:
//! the source's bytes as they are when the copy runs. Those are the bytes in
//! front of the submission's lists, where the steps done on the card read
//! them, if nothing of the submission writes them ahead of the copy: no step
//! (a copy, another copy at its place) and no shader (a declared write).
//! And the fill reads what the copy left only if nothing writes those bytes
//! between the copy and the fill. Where both hold the fill becomes a fill of
//! the source's bytes, done on the card with the other steps; where they do
//! not it is left as it was. Writes nobody declares (views picked at a
//! computed index, `crate::picked`) could break the first: with those
//! looked for, nothing is forwarded.

use super::*;

/// Whether such fills read the copy's source (`VICTORY_FILL_FORWARD`).
pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FILL_FORWARD").is_none_or(|v| v != "0"))
}

/// Since the last frame line: fills given the bytes of a copy's source.
pub(super) static FORWARDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What `d` writes as a copy (empty for anything else).
fn writes(d: &Does) -> Option<(usize, usize)> {
    match *d {
        Does::Copy { dst, len, .. } | Does::Placed { dst, len, .. } => Some((dst, dst + len)),
        _ => None,
    }
}

/// The fills of `does` (step `i` is `steps[i]`: its list by address, its place
/// in it; `runs` in the submission's order) that read only bytes a copy at its
/// place ahead of them wrote, where that is the source's (see the module):
/// each made a fill of the source's bytes, to be done on the card. How many.
/// `shader_writes`: the title memory a shader declares it writes, at a place
/// (run, step) of the submission.
pub(super) fn forward(does: &mut [Does], places: &[(usize, usize)], shader_writes: &[((usize, usize), (usize, usize))]) -> usize {
    if !on() || crate::picked::on() || does.iter().any(|d| matches!(d, Does::Run)) {
        return 0;
    }
    let mut n = 0;
    for i in 0..does.len() {
        let Does::Fill { src: at, len, to: true, .. } = does[i] else {
            continue;
        };
        if len == 0 {
            continue;
        }
        let read = (at, at + len);
        // The last step ahead of it that writes what it reads.
        let Some(p) = (0..i).rev().find(|&j| writes(&does[j]).is_some_and(|w| meet(w, read))) else {
            continue;
        };
        let Does::Placed { dst, src, len: copied } = does[p] else {
            continue;
        };
        if at < dst || at + len > dst + copied {
            continue;
        }
        let from = (src + (at - dst), src + (at - dst) + len);
        // Nothing of the submission writes the source's bytes ahead of the
        // copy; no shader writes what the fill reads between the two.
        let ahead = (0..p).any(|j| writes(&does[j]).is_some_and(|w| meet(w, from)));
        let (copy, fill) = (places[p], places[i]);
        let shaders = shader_writes
            .iter()
            .any(|&(place, w)| (place < copy && meet(w, from)) || (copy < place && place < fill && meet(w, read)));
        if ahead || shaders {
            continue;
        }
        does[i] = Does::Fill {
            src: from.0,
            len,
            asked: true,
            to: true,
        };
        n += 1;
    }
    n
}

/// The places (run, step) of `steps` (list by address, step) in `runs`, and
/// the shader writes the runs declare, at theirs: [`forward`]'s.
pub(super) fn places_of(runs: &[&HostList], steps: &[(usize, usize)]) -> (Vec<(usize, usize)>, Vec<((usize, usize), (usize, usize))>) {
    let run = |list: usize| runs.iter().position(|l| std::ptr::from_ref::<HostList>(l) as usize == list).unwrap_or(usize::MAX);
    let places = steps.iter().map(|&(list, step)| (run(list), step)).collect();
    let mut writes = Vec::new();
    for (r, list) in runs.iter().enumerate() {
        for (k, step) in list.deferred().copies.iter().enumerate() {
            if let Step::GpuWrite(_, at, len) = step {
                writes.push(((r, k), (*at, *at + *len)));
            }
        }
    }
    (places, writes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(src: usize, len: usize) -> Does {
        Does::Fill { src, len, asked: true, to: true }
    }

    /// A list copies 240 bytes out of the heap into the ring and reads them
    /// as constants: the fill reads the heap's bytes, and the copy at its
    /// place is no longer in the way of the steps done on the card.
    #[test]
    fn a_fill_of_what_a_copy_just_wrote_reads_the_copys_source() {
        let placed = Does::Placed { dst: 0x9000, src: 0x1000, len: 0xf0 };
        let mut does = [placed, fill(0x9000, 0xf0), fill(0x9010, 0x20), fill(0x9100, 0x10)];
        let places: Vec<(usize, usize)> = (0..4).map(|k| (0, k)).collect();
        assert_eq!(forward(&mut does, &places, &[]), 2);
        assert_eq!(does[1], fill(0x1000, 0xf0));
        assert_eq!(does[2], fill(0x1010, 0x20));
        // Past the copy's bytes: left.
        assert_eq!(does[3], fill(0x9100, 0x10));
        assert!(plan(&does, 2, true, &mut None).is_ok());
    }

    #[test]
    fn nothing_is_forwarded_where_the_bytes_change_in_between() {
        let placed = Does::Placed { dst: 0x9000, src: 0x1000, len: 0xf0 };
        let places: Vec<(usize, usize)> = (0..3).map(|k| (0, k)).collect();
        // A copy writes the source ahead of the placed one.
        let mut does = [Does::Copy { dst: 0x1000, src: 0x5000, len: 8, asked: false }, placed, fill(0x9000, 0xf0)];
        assert_eq!(forward(&mut does, &places, &[]), 0);
        // A shader writes the source ahead of the copy, or the ring between it and the fill.
        let mut does = [fill(0x7000, 4), placed, fill(0x9000, 0xf0)];
        assert_eq!(forward(&mut does, &places, &[((0, 0), (0x1080, 0x1090))]), 0);
        let mut does = [placed, fill(0x7000, 4), fill(0x9000, 0xf0)];
        assert_eq!(forward(&mut does, &places, &[((0, 1), (0x9000, 0x9004))]), 0);
        // A shader writing the source after the copy changes nothing of it.
        assert_eq!(forward(&mut does, &places, &[((0, 1), (0x1000, 0x1004))]), 1);
        // Another copy writes the fill's bytes after the placed one: that one is the last.
        let mut does = [placed, Does::Copy { dst: 0x9000, src: 0x5000, len: 4, asked: false }, fill(0x9000, 0xf0)];
        assert_eq!(forward(&mut does, &places, &[]), 0);
        // A step of ours runs: nothing.
        let mut does = [placed, Does::Run, fill(0x9000, 0xf0)];
        assert_eq!(forward(&mut does, &places, &[]), 0);
    }
}
