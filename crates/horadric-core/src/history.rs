//! The browser pane's history: every page the user went to, how often and
//! when last, and the zoom each site was given. The address field suggests
//! from it as it is typed, and finishes a site's name inline, as a browser
//! does. WebView2 keeps a history of its own, but has no way to read it.
//!
//! No I/O here: the UI reads and writes the file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The file it is kept in, beside the state.
pub const FILE: &str = "browser-history.json";

/// The most pages kept. Past it the one longest unvisited goes.
pub const KEEP: usize = 5000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub pages: Vec<Page>,
    /// The zoom factor each site was last given by hand, by host. A site
    /// at 100% has none.
    #[serde(default)]
    pub zooms: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub visits: u32,
    /// When it was last gone to, in Unix seconds.
    #[serde(default)]
    pub last: u64,
}

impl History {
    /// What the file holds, or nothing when it does not read: losing the
    /// history is better than not starting the browser.
    pub fn read(bytes: &[u8]) -> History {
        serde_json::from_slice(bytes).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The page at `url` was gone to at `now`. False when it is not kept,
    /// as a blank page or a search engine's internals are not.
    pub fn visit(&mut self, url: &str, title: &str, now: u64) -> bool {
        if !kept(url) {
            return false;
        }
        let url = without_fragment(url);
        match self.pages.iter_mut().find(|p| p.url == url) {
            Some(p) => {
                p.visits = p.visits.saturating_add(1);
                p.last = now;
                if !title.trim().is_empty() {
                    p.title = title.trim().to_string();
                }
            }
            None => {
                self.pages.push(Page {
                    url: url.to_string(),
                    title: title.trim().to_string(),
                    visits: 1,
                    last: now,
                });
                if self.pages.len() > KEEP {
                    if let Some(oldest) = (0..self.pages.len()).min_by_key(|&i| self.pages[i].last)
                    {
                        self.pages.swap_remove(oldest);
                    }
                }
            }
        }
        true
    }

    /// The page at `url` has a title now, which most pages only have after
    /// they loaded. False when nothing changed.
    pub fn retitle(&mut self, url: &str, title: &str) -> bool {
        let (url, title) = (without_fragment(url), title.trim());
        match self.pages.iter_mut().find(|p| p.url == url) {
            Some(p) if !title.is_empty() && p.title != title => {
                p.title = title.to_string();
                true
            }
            _ => false,
        }
    }

    /// Takes a page out of the history.
    pub fn forget(&mut self, url: &str) -> bool {
        let before = self.pages.len();
        self.pages.retain(|p| p.url != url);
        self.pages.len() != before
    }

    /// The pages that fit what was typed, best first, at most `max`. Every
    /// word typed must be in the address or the title. With nothing typed,
    /// the latest sites, one page each: the last one gone to there.
    pub fn suggest(&self, typed: &str, now: u64, max: usize) -> Vec<&Page> {
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            let mut latest: Vec<&Page> = self.pages.iter().collect();
            latest.sort_by_key(|p| std::cmp::Reverse(p.last));
            let mut hosts = Vec::new();
            latest.retain(|p| {
                let h = host(&p.url).to_string();
                let new = !hosts.contains(&h);
                hosts.push(h);
                new
            });
            latest.truncate(max);
            return latest;
        }
        let words: Vec<&str> = typed.split_whitespace().collect();
        let mut found: Vec<(f64, &Page)> = self
            .pages
            .iter()
            .filter_map(|p| {
                let address = bare(&p.url).to_lowercase();
                let title = p.title.to_lowercase();
                if !words
                    .iter()
                    .all(|w| address.contains(w) || title.contains(w))
                {
                    return None;
                }
                let lead = if address.starts_with(&typed) {
                    4.0
                } else if words
                    .iter()
                    .any(|w| host(&address).split('.').any(|part| part.starts_with(w)))
                {
                    2.0
                } else if words.iter().any(|w| starts_a_word(&title, w)) {
                    1.5
                } else {
                    1.0
                };
                Some((lead * score(p, now), p))
            })
            .collect();
        found.sort_by(|a, b| b.0.total_cmp(&a.0));
        found.into_iter().take(max).map(|(_, p)| p).collect()
    }

    /// What finishes `typed` as an address gone to before: a site's name
    /// while no path is typed, else a whole address. Only the part after
    /// what was typed, so the field keeps the typing as it was. None when
    /// nothing begins that way, or `typed` reads as a search.
    pub fn complete(&self, typed: &str, now: u64) -> Option<String> {
        if typed.is_empty() || typed.contains(char::is_whitespace) || typed.contains("://") {
            return None;
        }
        let whole = typed.contains('/');
        let www = typed.to_ascii_lowercase().starts_with("www.");
        let mut best: BTreeMap<String, f64> = BTreeMap::new();
        for p in &self.pages {
            let address = if www {
                after_scheme(&p.url)
            } else {
                bare(&p.url)
            };
            let address = address.trim_end_matches('/');
            let candidate = if whole {
                address
            } else {
                address.split('/').next().unwrap_or(address)
            };
            let begins = candidate
                .get(..typed.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(typed));
            if begins && candidate.len() > typed.len() {
                *best.entry(candidate.to_string()).or_default() += score(p, now);
            }
        }
        let (candidate, _) = best.into_iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
        Some(candidate[typed.len()..].to_string())
    }

    /// The zoom the site at `url` was given, where it was given one.
    pub fn zoom(&self, url: &str) -> Option<f64> {
        self.zooms.get(host(after_scheme(url))).copied()
    }

    /// The site at `url` was zoomed to `zoom` by hand. False when nothing
    /// changed or it has no host.
    pub fn set_zoom(&mut self, url: &str, zoom: f64) -> bool {
        let h = host(after_scheme(url));
        if h.is_empty() || !kept(url) {
            return false;
        }
        if (zoom - 1.0).abs() < 0.01 {
            return self.zooms.remove(h).is_some();
        }
        let old = self.zooms.insert(h.to_string(), zoom);
        old.is_none_or(|o| (o - zoom).abs() > 1e-6)
    }
}

/// Whether a page belongs in the history: one on the web or on disk, not a
/// blank page, an error page or the browser's own.
pub fn kept(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file:"))
        && !after_scheme(url).is_empty()
}

/// How often and how lately a page was gone to: visits count for less
/// each, and for less the longer ago the last was.
fn score(p: &Page, now: u64) -> f64 {
    let days = now.saturating_sub(p.last) as f64 / 86_400.0;
    (1.0 + (p.visits as f64).ln_1p()) / (1.0 + days / 7.0)
}

/// An address without its scheme.
fn after_scheme(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest)
}

/// An address as it is typed: no scheme, no `www.`.
pub fn bare(url: &str) -> &str {
    let rest = after_scheme(url);
    match rest.get(..4) {
        Some(w) if w.eq_ignore_ascii_case("www.") => &rest[4..],
        _ => rest,
    }
}

/// The host of an address without its scheme, its port kept.
fn host(rest: &str) -> &str {
    let rest = after_scheme(rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

fn without_fragment(url: &str) -> &str {
    url.split_once('#').map_or(url, |(u, _)| u)
}

/// Whether `word` starts a word of `text`.
fn starts_a_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn history(pages: &[(&str, &str, u32, u64)]) -> History {
        History {
            pages: pages
                .iter()
                .map(|&(url, title, visits, last)| Page {
                    url: url.into(),
                    title: title.into(),
                    visits,
                    last,
                })
                .collect(),
            zooms: BTreeMap::new(),
        }
    }

    #[test]
    fn a_visit_counts_once_per_page() {
        let mut h = History::default();
        assert!(h.visit("https://example.com/a#top", "A", 10));
        assert!(h.visit("https://example.com/a#end", "", 20));
        assert_eq!(h.pages.len(), 1);
        assert_eq!(h.pages[0].url, "https://example.com/a");
        assert_eq!(h.pages[0].visits, 2);
        assert_eq!(h.pages[0].last, 20);
        // A visit with no title yet keeps the one it had.
        assert_eq!(h.pages[0].title, "A");
        assert!(h.retitle("https://example.com/a", "Page A"));
        assert!(!h.retitle("https://example.com/a", "Page A"));
        assert_eq!(h.pages[0].title, "Page A");
    }

    #[test]
    fn blank_and_internal_pages_are_not_kept() {
        let mut h = History::default();
        assert!(!h.visit("about:blank", "", 1));
        assert!(!h.visit("edge://settings", "", 1));
        assert!(!h.visit("data:text/html,hi", "", 1));
        assert!(h.visit("http://localhost:3000/", "", 1));
        assert!(h.visit("file:///C:/a.html", "", 1));
        assert_eq!(h.pages.len(), 2);
    }

    #[test]
    fn the_longest_unvisited_page_goes_when_full() {
        let mut h = History::default();
        for i in 0..KEEP as u64 {
            h.visit(&format!("https://a.com/{i}"), "", 100 + i);
        }
        h.visit("https://a.com/0", "", 1_000_000);
        h.visit("https://b.com/", "", 1_000_001);
        assert_eq!(h.pages.len(), KEEP);
        assert!(h.pages.iter().any(|p| p.url == "https://a.com/0"));
        assert!(!h.pages.iter().any(|p| p.url == "https://a.com/1"));
    }

    #[test]
    fn every_word_typed_must_be_in_the_address_or_title() {
        let h = history(&[
            ("https://github.com/rust-lang/rust", "Rust", 3, 10 * DAY),
            ("https://docs.rs/serde", "serde docs", 1, 10 * DAY),
        ]);
        let urls = |typed| {
            h.suggest(typed, 10 * DAY, 8)
                .iter()
                .map(|p| p.url.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(urls("serde"), ["https://docs.rs/serde"]);
        assert_eq!(urls("RUST github"), ["https://github.com/rust-lang/rust"]);
        assert!(urls("rust serde").is_empty());
    }

    #[test]
    fn an_address_begun_comes_before_a_word_found_inside() {
        let h = history(&[
            ("https://example.com/github", "A link to it", 50, 10 * DAY),
            ("https://github.com/", "GitHub", 1, 9 * DAY),
        ]);
        let first = h.suggest("git", 10 * DAY, 8)[0].url.clone();
        assert_eq!(first, "https://github.com/");
    }

    #[test]
    fn a_page_gone_to_often_and_lately_comes_first() {
        let h = history(&[
            ("https://a.com/old", "Notes", 20, 0),
            ("https://b.com/new", "Notes", 2, 100 * DAY),
            ("https://c.com/once", "Notes", 1, 100 * DAY),
        ]);
        let order: Vec<&str> = h
            .suggest("notes", 100 * DAY, 8)
            .iter()
            .map(|p| p.url.as_str())
            .collect();
        assert_eq!(
            order,
            [
                "https://b.com/new",
                "https://c.com/once",
                "https://a.com/old"
            ]
        );
    }

    #[test]
    fn nothing_typed_lists_the_latest_sites_once_each() {
        let h = history(&[
            ("https://a.com/1", "", 1, 1),
            ("https://b.com/", "", 1, 2),
            ("https://a.com/2", "", 1, 3),
        ]);
        let order: Vec<&str> = h.suggest("", 3, 8).iter().map(|p| p.url.as_str()).collect();
        assert_eq!(order, ["https://a.com/2", "https://b.com/"]);
        assert_eq!(h.suggest(" ", 3, 1).len(), 1);
    }

    #[test]
    fn a_site_name_is_finished_inline() {
        let h = history(&[
            ("https://www.github.com/rust-lang/rust", "", 5, DAY),
            ("https://github.com/", "", 1, DAY),
            ("https://gitlab.com/", "", 1, DAY),
            ("http://localhost:3000/login", "", 1, DAY),
        ]);
        assert_eq!(h.complete("git", DAY).as_deref(), Some("hub.com"));
        assert_eq!(h.complete("GitL", DAY).as_deref(), Some("ab.com"));
        assert_eq!(h.complete("loc", DAY).as_deref(), Some("alhost:3000"));
        // With a path typed, the whole address.
        assert_eq!(
            h.complete("github.com/r", DAY).as_deref(),
            Some("ust-lang/rust")
        );
        assert_eq!(h.complete("www.git", DAY).as_deref(), Some("hub.com"));
        // Nothing to add, a search, or nothing like it.
        assert_eq!(h.complete("github.com", DAY), None);
        assert_eq!(h.complete("git hub", DAY), None);
        assert_eq!(h.complete("zzz", DAY), None);
        assert_eq!(h.complete("", DAY), None);
    }

    #[test]
    fn a_zoom_is_kept_per_site() {
        let mut h = History::default();
        assert!(h.set_zoom("https://example.com/a", 1.25));
        assert!(!h.set_zoom("https://example.com/b", 1.25));
        assert_eq!(h.zoom("https://example.com/c?x"), Some(1.25));
        assert_eq!(h.zoom("http://localhost:3000/"), None);
        // Back to 100% is no zoom of its own.
        assert!(h.set_zoom("https://example.com/", 1.0));
        assert_eq!(h.zoom("https://example.com/"), None);
        assert!(!h.set_zoom("about:blank", 1.5));
    }

    #[test]
    fn a_damaged_file_reads_as_none() {
        assert_eq!(History::read(b"{not json"), History::default());
        let mut h = History::default();
        h.visit("https://example.com/", "Example", 5);
        h.set_zoom("https://example.com/", 0.9);
        assert_eq!(History::read(h.to_json().as_bytes()), h);
    }

    #[test]
    fn a_word_is_found_at_its_start() {
        assert!(starts_a_word("the rust book", "rust"));
        assert!(starts_a_word("rust", "ru"));
        assert!(!starts_a_word("trust", "rust"));
    }
}
