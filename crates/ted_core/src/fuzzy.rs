//! Helm-style fuzzy matching: space-separated tokens must all match the title or subtitle.

/// Lowercased match targets for one item, computed once per picker: `title subtitle` in
/// one string.
pub struct MatchKeys {
    full: String,
    title_len: usize,
}

impl MatchKeys {
    pub fn new(title: &str, subtitle: &str) -> Self {
        let mut full = String::with_capacity(title.len() + 1 + subtitle.len());
        full.push_str(title);
        if !subtitle.is_empty() {
            full.push(' ');
            full.push_str(subtitle);
        }
        full.make_ascii_lowercase();
        Self { full, title_len: title.len() }
    }

    fn title(&self) -> &str {
        &self.full[..self.title_len]
    }

    fn subtitle(&self) -> &str {
        self.full.get(self.title_len + 1..).unwrap_or("")
    }
}

struct Token {
    chars: Vec<char>,
    lower: String,
}

fn tokenize(query: &str) -> Vec<Token> {
    query
        .split_whitespace()
        .map(|t| {
            let lower = t.to_ascii_lowercase();
            Token { chars: lower.chars().collect(), lower }
        })
        .collect()
}

/// Scores an item against all tokens; `None` unless every token matches.
fn score_keys(tokens: &[Token], keys: &MatchKeys) -> Option<i32> {
    let mut total = 0;
    for tok in tokens {
        let title = fuzzy_score_with_pattern(&tok.chars, &tok.lower, keys.title()).map(|s| s + 500);
        let sub = fuzzy_score_with_pattern(&tok.chars, &tok.lower, keys.subtitle());
        total += match (title, sub) {
            (Some(t), Some(s)) => t.max(s),
            (Some(t), None) => t,
            (None, Some(s)) => s,
            (None, None) => fuzzy_score_with_pattern(&tok.chars, &tok.lower, &keys.full)?,
        };
    }
    if tokens.first().is_some_and(|t| keys.title().starts_with(&t.lower)) {
        total += 200;
    }
    Some(total)
}

/// Items narrowed down by a query as it is typed: the indices of those matching, best
/// first.
#[derive(Default)]
pub struct Narrowing {
    keys: Vec<MatchKeys>,
    /// The query `matches` holds the matches of.
    query: String,
    matches: Vec<usize>,
}

impl Narrowing {
    /// All of the items, before any query.
    pub fn new(keys: Vec<MatchKeys>) -> Self {
        let matches = (0..keys.len()).collect();
        Self { keys, query: String::new(), matches }
    }

    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// Adds an item; `rescan` or `show_all` then places it.
    pub fn push(&mut self, keys: MatchKeys) {
        self.keys.push(keys);
    }

    pub fn clear(&mut self) {
        self.keys.clear();
        self.matches.clear();
    }

    /// Filters for `query`, rescanning only the previous matches when the query just grew.
    pub fn narrow(&mut self, query: &str) {
        if !query.starts_with(&self.query) {
            self.rescan(query);
            return;
        }
        self.matches = filter_within(query, &self.keys, self.matches.iter().copied());
        self.query = query.to_string();
    }

    /// Filters every item for `query`.
    pub fn rescan(&mut self, query: &str) {
        self.matches = filter(query, &self.keys);
        self.query = query.to_string();
    }

    /// Every item, in order, whatever the query.
    pub fn show_all(&mut self, query: &str) {
        self.matches = (0..self.keys.len()).collect();
        self.query = query.to_string();
    }
}

/// Indices of the items matching `query`, best first. Ties keep the original order.
pub fn filter(query: &str, keys: &[MatchKeys]) -> Vec<usize> {
    filter_within(query, keys, 0..keys.len())
}

/// Like `filter`, scoring only the items at `candidates`. Every match of a query is also a
/// match of its prefixes, so narrowing a query only needs to rescan the previous matches.
pub fn filter_within(query: &str, keys: &[MatchKeys], candidates: impl Iterator<Item = usize>) -> Vec<usize> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        let mut all: Vec<usize> = candidates.collect();
        all.sort_unstable();
        return all;
    }
    let mut scored: Vec<(usize, i32)> =
        candidates.filter_map(|i| score_keys(&tokens, &keys[i]).map(|s| (i, s))).collect();
    scored.sort_unstable_by_key(|&(i, score)| (std::cmp::Reverse(score), i));
    scored.into_iter().map(|(i, _)| i).collect()
}

pub fn helm_score(query: &str, title: &str, subtitle: &str) -> Option<i32> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return Some(0);
    }
    score_keys(&tokens, &MatchKeys::new(title, subtitle))
}

pub fn fuzzy_score(pattern: &str, candidate: &str) -> Option<i32> {
    let lower = pattern.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    fuzzy_score_with_pattern(&chars, &lower, &candidate.to_ascii_lowercase())
}

fn is_separator(b: u8) -> bool {
    matches!(b, b'/' | b'.' | b'_' | b'-')
}

pub fn fuzzy_score_with_pattern(pat_chars: &[char], pat_lower: &str, candidate: &str) -> Option<i32> {
    if pat_chars.is_empty() {
        return Some(0);
    }
    let bytes = candidate.as_bytes();
    let starts_segment = |i: usize| i == 0 || bytes.get(i - 1).copied().is_some_and(is_separator);

    if let Some(idx) = candidate.find(pat_lower) {
        let mut score = 1000 + pat_chars.len() as i32 * 25;
        if starts_segment(idx) {
            score += 400;
        }
        return Some(score - candidate.len() as i32 / 2);
    }

    let (mut p, mut score, mut consecutive, mut prev) = (0, 0, 0, -2i32);
    for (i, c) in candidate.chars().enumerate() {
        if p < pat_chars.len() && c == pat_chars[p] {
            let mut s = 10;
            if starts_segment(i) {
                s += 40;
            }
            if i as i32 == prev + 1 {
                consecutive += 1;
                s += consecutive * 20;
            } else {
                consecutive = 0;
            }
            score += s;
            prev = i as i32;
            p += 1;
        }
    }
    (p == pat_chars.len()).then(|| score - candidate.len() as i32 / 4)
}
