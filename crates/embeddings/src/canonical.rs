//! Canonicalisation of text for cache-keying.
//!
//! Per `docs/concepts/embedding-strategy.md` §3, the canonicalisation is
//! part of the cache contract — changing it requires bumping a cache
//! version. The current canonicalisation: trim + collapse internal
//! whitespace runs. Unicode normalisation and case-folding can be added
//! when a real provider needs them; for now we keep it minimal so the
//! contract is small.

/// Canonicalise text for cache keying. Idempotent: `canonicalise(canonicalise(x)) == canonicalise(x)`.
pub fn canonicalise(text: &str) -> String {
    let trimmed = text.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalise_collapses_internal_whitespace() {
        assert_eq!(canonicalise("hello   world"), "hello world");
    }

    #[test]
    fn canonicalise_trims_edges() {
        assert_eq!(canonicalise("  hello  "), "hello");
    }

    #[test]
    fn canonicalise_handles_tabs_and_newlines() {
        assert_eq!(canonicalise("a\tb\nc  d"), "a b c d");
    }

    #[test]
    fn canonicalise_is_idempotent() {
        let inputs = ["", "  ", "hello", "  hello  world  ", "a\tb\n c"];
        for input in inputs {
            let once = canonicalise(input);
            let twice = canonicalise(&once);
            assert_eq!(once, twice, "canonicalise must be idempotent on {input:?}");
        }
    }
}
