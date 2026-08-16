//! General chain filtering with truncation.
//!
//! A [`ChainFilter`] hides history nodes that match a predicate while preserving
//! each chain's endpoints (the oldest root and newest leaf stay visible). When
//! [`ChainFilter::splice`] is set, hidden *intermediate* nodes are removed and
//! their causal edges are reconnected so each kept child points at its nearest
//! kept ancestor — producing a truncated view rather than disconnected stubs.
//!
//! This subsumes hiding timestamp-less records (`last-prompt`, `custom-title`,
//! etc.) that import with `Clock::UnixMs(0)` via [`ChainFilter::hide_undated`].

use std::collections::{HashMap, HashSet};

use editchain_core::OpId;

use crate::HistoryNode;

/// A compiled matcher over display text.
///
/// Treats the pattern as a regular expression when it compiles; otherwise falls
/// back to literal substring matching so plain keywords still work even when
/// they aren't valid regex.
#[derive(Debug)]
enum Matcher {
    /// No pattern — never matches.
    None,
    /// Literal substring match (fallback when the pattern isn't valid regex).
    Literal(String),
    /// Compiled regular expression.
    Regex(regex::Regex),
}

impl Matcher {
    /// Build a matcher from a raw pattern string.
    #[must_use]
    fn new(pattern: &str) -> Self {
        if pattern.is_empty() {
            return Self::None;
        }
        match regex::Regex::new(pattern) {
            Ok(re) => Self::Regex(re),
            Err(_) => Self::Literal(pattern.to_string()),
        }
    }

    /// Whether this matcher matches `text`.
    #[must_use]
    fn matches(&self, text: &str) -> bool {
        match self {
            Self::None => false,
            Self::Literal(s) => text.contains(s.as_str()),
            Self::Regex(re) => re.is_match(text),
        }
    }
}
/// A chain filter over history nodes.
///
/// A node is hidden when any active predicate matches it:
/// - [`Self::hide_undated`] hides nodes whose clock is unknown (`timestamp_ms() == 0`);
/// - [`Self::summary_pattern`] hides nodes whose display summary matches;
/// - [`Self::kind_pattern`] hides nodes whose kind tag matches.
///
/// Chain endpoints (nodes with no parent or no child in the full graph) are
/// always preserved regardless of predicate matches.
#[derive(Debug)]
pub struct ChainFilter {
    /// Regex/literal pattern matched against each node's display summary.
    pub summary_pattern: String,
    /// Regex/literal pattern matched against each node's kind tag.
    pub kind_pattern: String,
    /// Hide nodes with no real timestamp (`timestamp_ms() == 0`).
    pub hide_undated: bool,
    /// Reconnect causal edges across hidden intermediate nodes so chains stay
    /// continuous instead of leaving disconnected stubs.
    pub splice: bool,
    summary_matcher: Matcher,
    kind_matcher: Matcher,
}

impl Default for ChainFilter {
    fn default() -> Self {
        Self::new(String::new(), String::new(), true, true)
    }
}

impl ChainFilter {
    /// Build a filter from raw patterns and flags.
    #[must_use]
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "hide_undated and splice are independent, mutually-exclusive filter flags"
    )]
    pub fn new(
        summary_pattern: String,
        kind_pattern: String,
        hide_undated: bool,
        splice: bool,
    ) -> Self {
        Self {
            summary_matcher: Matcher::new(&summary_pattern),
            kind_matcher: Matcher::new(&kind_pattern),
            summary_pattern,
            kind_pattern,
            hide_undated,
            splice,
        }
    }

    /// Whether this filter would hide nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.hide_undated && self.summary_pattern.is_empty() && self.kind_pattern.is_empty()
    }

    /// Whether a single node matches any active predicate.
    #[must_use]
    fn matches(&self, node: &HistoryNode) -> bool {
        if self.hide_undated && node.timestamp_ms() == 0 {
            return true;
        }
        if self.summary_matcher.matches(&node.summary()) {
            return true;
        }
        self.kind_matcher.matches(&node.kind())
    }

    /// A stable identity for cache keying across requests.
    ///
    /// Uses only the raw patterns and flags (not compiled regexes), so two
    /// filters that behave identically share one cache entry.
    #[must_use]
    pub fn key(&self) -> ChainFilterKey {
        ChainFilterKey {
            summary_pattern: self.summary_pattern.clone(),
            kind_pattern: self.kind_pattern.clone(),
            hide_undated: self.hide_undated,
            splice: self.splice,
        }
    }
}

/// A hashable identity for [`ChainFilter`], used as a cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChainFilterKey {
    /// Summary pattern string.
    pub summary_pattern: String,
    /// Kind pattern string.
    pub kind_pattern: String,
    /// Hide undated flag.
    pub hide_undated: bool,
    /// Splice flag.
    pub splice: bool,
}

/// Apply a [`ChainFilter`] to a canonical newest-first node list.
///
/// Returns cloned [`HistoryNode`]s with their causal parents rewritten so that
/// every kept child points at its nearest kept ancestor through any run of
/// hidden intermediate nodes.
///
/// `hide_undated` hides every undated node unconditionally (including leaves).
/// Pattern-based truncation preserves endpoints (no parent / no child in the
/// full graph) so a filtered chain keeps its anchors.
#[must_use]
pub fn apply(
    nodes: &[HistoryNode],
    links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
    filter: &ChainFilter,
) -> Vec<HistoryNode> {
    if filter.is_empty() || nodes.is_empty() {
        return nodes.to_vec();
    }

    // Original parent keys per key (op ids + git oid hex).
    let mut parents_of_key = HashMap::with_capacity(nodes.len());
    // Reverse adjacency for endpoint detection.
    let mut children_of_key = HashMap::with_capacity(nodes.len());
    for n in nodes {
        let key = n.node_key();
        let ps = n.parent_keys(links);
        drop(parents_of_key.insert(key.clone(), ps.clone()));
        for p in ps {
            children_of_key
                .entry(p)
                .or_insert_with(Vec::new)
                .push(key.clone());
        }
        // Ensure every key has an entry even with no children.
        let _: &mut Vec<String> = children_of_key.entry(key.clone()).or_default();
        let _: &mut Vec<String> = parents_of_key.entry(key).or_default();
    }

    // Decide which nodes to hide.
    //
    // `hide_undated` hides EVERY undated node unconditionally — including leaf
    // nodes. Undated records (e.g. `last-prompt`, `custom-title`) are metadata
    // with no meaningful chain position, so a lone undated leaf is junk and must
    // not survive just because it happens to be an endpoint.
    //
    // Pattern-based truncation (summary/kind) instead preserves endpoints (nodes
    // with no parent or no child in the full graph) so a filtered chain keeps its
    // anchors — the oldest root and newest leaf stay visible even when they match.
    let mut hidden = HashSet::with_capacity(nodes.len());
    for n in nodes {
        let key = n.node_key();
        if filter.hide_undated && n.timestamp_ms() == 0 {
            let _: bool = hidden.insert(key);
            continue;
        }
        if !filter.summary_pattern.is_empty() || !filter.kind_pattern.is_empty() {
            let has_parent = !parents_of_key.get(&key).is_none_or(Vec::is_empty);
            let has_child = !children_of_key.get(&key).is_none_or(Vec::is_empty);
            let is_endpoint = !has_parent || !has_child;
            if !is_endpoint && filter.matches(n) {
                let _: bool = hidden.insert(key);
            }
        }
    }

    // Rewrite each kept node's parents to its nearest kept ancestors.
    let mut result = Vec::with_capacity(nodes.len());
    for n in nodes {
        let key = n.node_key();
        if hidden.contains(&key) {
            continue;
        }
        let mut out = n.clone();
        let spliced = if filter.splice && !hidden.is_empty() {
            nearest_kept_ancestors(&key, &parents_of_key, &hidden)
        } else {
            n.parent_keys(links)
        };
        out.set_parent_keys(&spliced);
        result.push(out);
    }
    result
}

/// Compute the nearest kept ancestors of `key`, walking up through runs of
/// hidden intermediate nodes.
///
/// Returns every reachable kept ancestor; when all ancestors up to a root are
/// hidden (or a cycle is encountered), those paths contribute nothing so the
/// node becomes effectively rootless along them.
#[must_use]
pub(crate) fn nearest_kept_ancestors(
    key: &str,
    parents_of_key: &HashMap<String, Vec<String>>,
    hidden: &HashSet<String>,
) -> Vec<String> {
    #[expect(
        clippy::too_many_arguments,
        reason = "walk threads the shared traversal state through recursive calls"
    )]
    fn walk(
        cur_key: &str,
        parents_of_key: &HashMap<String, Vec<String>>,
        hidden: &HashSet<String>,
        visited: &mut HashSet<String>,
        out: &mut Vec<String>,
        seen_out_keys: &mut HashSet<String>,
    ) {
        if !visited.insert(cur_key.to_string()) {
            return; // cycle guard
        }
        let parents = parents_of_key.get(cur_key).map_or(&[][..], Vec::as_slice);
        if parents.is_empty() {
            return; // root reached
        }
        for parent in parents {
            if hidden.contains(parent) {
                walk(parent, parents_of_key, hidden, visited, out, seen_out_keys);
            } else if seen_out_keys.insert(parent.clone()) {
                out.push(parent.clone());
            }
        }
    }

    let mut out = Vec::new();
    let mut seen_out_keys = HashSet::new();
    let mut visited = HashSet::new();
    walk(
        key,
        parents_of_key,
        hidden,
        &mut visited,
        &mut out,
        &mut seen_out_keys,
    );
    out
}
