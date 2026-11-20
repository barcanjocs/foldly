//! Fuzzy filtering, fzf-style, using nucleo.
//!
//! Each space-separated word must match on its own, in any order, and fuzzily: `rhpar`
//! finds "Radiohead — Paranoid Android". fzf's operators work too: `'word` exact,
//! `^start`, `end$`, `!exclude`.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32String};

pub struct Filter {
    matcher: Matcher,
}

impl Filter {
    pub fn new() -> Self {
        Self { matcher: Matcher::new(Config::DEFAULT) }
    }

    /// Indices of the haystacks matching `query`. With `rank`, best matches come first
    /// (ties keep their original order); otherwise the original order is kept.
    pub fn matches(&mut self, query: &str, haystacks: &[Utf32String], rank: bool) -> Vec<usize> {
        // Smart normalization lets "beyonce" match "Beyoncé".
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(usize, u32)> = haystacks
            .iter()
            .enumerate()
            .filter_map(|(i, h)| pattern.score(h.slice(..), &mut self.matcher).map(|s| (i, s)))
            .collect();
        if rank {
            scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        }
        scored.into_iter().map(|(i, _)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(query: &str, items: &[&str], rank: bool) -> Vec<&'static str> {
        let haystacks: Vec<Utf32String> = items.iter().map(|s| Utf32String::from(*s)).collect();
        let found = Filter::new().matches(query, &haystacks, rank);
        // Leak is fine in tests; keeps the assertion readable.
        found.into_iter().map(|i| &*Box::leak(items[i].to_string().into_boxed_str())).collect()
    }

    const LIBRARY: &[&str] = &[
        "Radiohead — Airbag OK Computer",
        "Radiohead — Paranoid Android OK Computer",
        "Portishead — Roads Dummy",
        "Beyoncé — Halo I Am... Sasha Fierce",
    ];

    #[test]
    fn words_match_independently_in_any_order() {
        assert_eq!(run("android radio", LIBRARY, true), ["Radiohead — Paranoid Android OK Computer"]);
        assert_eq!(run("computer ok", LIBRARY, false).len(), 2);
    }

    #[test]
    fn fuzzy_abbreviations_and_accents() {
        assert_eq!(run("rhpar", LIBRARY, true)[0], "Radiohead — Paranoid Android OK Computer");
        assert_eq!(run("beyonce", LIBRARY, true), ["Beyoncé — Halo I Am... Sasha Fierce"]);
    }

    #[test]
    fn fzf_operators() {
        assert_eq!(run("head !radio", LIBRARY, true), ["Portishead — Roads Dummy"]);
        assert_eq!(run("'roads", LIBRARY, true), ["Portishead — Roads Dummy"]);
    }

    #[test]
    fn unranked_keeps_original_order() {
        assert_eq!(
            run("radiohead", LIBRARY, false),
            ["Radiohead — Airbag OK Computer", "Radiohead — Paranoid Android OK Computer"]
        );
    }
}
