//! No name that must stay out of the tree enters it (the asset policy):
//! the title's, its makers', its world's. The names are not written here:
//! [`FORBIDDEN`] holds the BLAKE3 hashes of them, and each text file's words,
//! and each run of two and of three words, are hashed and looked up. A hit
//! names the file and the line, never the word.
//!
//! How a word or phrase is made into what is hashed (`cargo xtask
//! names-hash <text>` prints the hash of any text made so):
//! 1. the text, read as UTF-8 (invalid bytes as U+FFFD), is lower-cased
//!    character by character (Unicode `to_lowercase`);
//! 2. its words are the longest runs of alphanumeric characters (letters and
//!    digits of any script, `char::is_alphanumeric`); anything else parts
//!    them: spaces, punctuation, `_`, `-`, line ends (so a phrase may run
//!    over the end of a line);
//! 3. a phrase is two or three words in a row, joined by one space (U+0020);
//! 4. the hash is BLAKE3 of the word's or the phrase's UTF-8 bytes, written
//!    as 64 lower-case hex digits.
//!
//! `Foo-Bar baz` gives the words `foo`, `bar`, `baz` and the phrases
//! `foo bar`, `bar baz` and `foo bar baz`.

use std::collections::HashSet;
use std::sync::OnceLock;

/// The hashes of the words and phrases that must not be in the tree, as
/// 64 lower-case hex digits each.
pub const FORBIDDEN: &[&str] = &[];

/// The words of `text`, each with the line (from 1) it starts on.
pub fn words(text: &str) -> Vec<(String, usize)> {
    let mut words = Vec::new();
    let (mut line, mut word, mut word_line) = (1, String::new(), 1);
    for c in text.chars() {
        if c.is_alphanumeric() {
            if word.is_empty() {
                word_line = line;
            }
            word.extend(c.to_lowercase());
            continue;
        }
        if !word.is_empty() {
            words.push((std::mem::take(&mut word), word_line));
        }
        if c == '\n' {
            line += 1;
        }
    }
    if !word.is_empty() {
        words.push((word, word_line));
    }
    words
}

/// The hash of a word or phrase as [`FORBIDDEN`] holds it.
pub fn hash_of(text: &str) -> String {
    let joined: Vec<String> = words(text).into_iter().map(|(w, _)| w).collect();
    blake3::hash(joined.join(" ").as_bytes()).to_hex().to_string()
}

/// [`FORBIDDEN`] as digests.
fn forbidden() -> &'static HashSet<[u8; 32]> {
    static SET: OnceLock<HashSet<[u8; 32]>> = OnceLock::new();
    SET.get_or_init(|| parse(FORBIDDEN))
}

/// Hex hashes as digests (a malformed one is left out; a test checks there
/// is none).
fn parse(hashes: &[&str]) -> HashSet<[u8; 32]> {
    hashes.iter().filter_map(|h| blake3::Hash::from_hex(h.trim()).ok()).map(|h| *h.as_bytes()).collect()
}

/// The lines of `text` where a word or phrase of `forbidden` starts, each
/// once, in order.
pub fn hits_in(text: &str, forbidden: &HashSet<[u8; 32]>) -> Vec<usize> {
    if forbidden.is_empty() {
        return Vec::new();
    }
    let words = words(text);
    let mut lines = Vec::new();
    let mut phrase = String::new();
    for i in 0..words.len() {
        phrase.clear();
        for (n, (word, _)) in words[i..].iter().take(3).enumerate() {
            if n > 0 {
                phrase.push(' ');
            }
            phrase.push_str(word);
            if forbidden.contains(blake3::hash(phrase.as_bytes()).as_bytes()) {
                lines.push(words[i].1);
            }
        }
    }
    lines.dedup();
    lines
}

/// The lines of `content` where a forbidden word or phrase starts.
pub fn hits(content: &[u8]) -> Vec<usize> {
    hits_in(&String::from_utf8_lossy(content), forbidden())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_well_formed() {
        for h in FORBIDDEN {
            assert_eq!(h.len(), 64, "{h}");
            assert!(h.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)), "{h}");
        }
        assert_eq!(parse(FORBIDDEN).len(), FORBIDDEN.len(), "a hash is there twice");
    }

    #[test]
    fn words_are_split_and_lower_cased() {
        let got: Vec<(String, usize)> = words("Foo-Bar baz_Qux\n  ÉTÉ 2x\n");
        let want = [("foo", 1), ("bar", 1), ("baz", 1), ("qux", 1), ("été", 2), ("2x", 2)];
        assert_eq!(got, want.map(|(w, l)| (w.to_owned(), l)));
        assert_eq!(hash_of("Foo-Bar  BAZ"), blake3::hash(b"foo bar baz").to_hex().to_string());
    }

    #[test]
    fn a_made_up_name_is_found_alone_and_in_phrases() {
        // Made-up names, not any real one.
        let forbidden = parse(&[&hash_of("zorblax"), &hash_of("Quent Varno"), &hash_of("ul mara tesh")]);
        let text = "fn ok() {}\n// the Zorblax's code\nquent\nvarno: x\nUl-Mara_Tesh!\nzorblaxian, quentvarno, mara tesh\n";
        assert_eq!(hits_in(text, &forbidden), [2, 3, 5]);
        assert!(hits_in("nothing here", &forbidden).is_empty());
        assert!(hits_in("zorblax", &HashSet::new()).is_empty());
    }
}
