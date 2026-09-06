//! General chain filtering with truncation.
//!
//! A [`ChainFilter`] hides history nodes that match a predicate while preserving
//! each chain's endpoints (the oldest root and newest leaf stay visible). When
//! [`ChainFilter::splice`] is set, hidden *intermediate* nodes are removed and
//! their causal edges are reconnected so each kept child points at its nearest
//! kept ancestor — producing a truncated view rather than disconnected stubs.
//!
//! This subsumes hiding timestamp-less records (`last-prompt`, `custom-title`,
//! etc.) that import with `Clock::UnixMs(0)` via [`ChainFilter::hide_undated`],
//! and hiding semantic trace rows (duplicate/echo/transport envelopes) via
//! [`ChainFilter::hide_trace`].

use std::collections::{HashMap, HashSet};

use editchain_core::{Op, OpId};

use crate::taxonomy::Visibility;
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
/// - [`Self::include_kind_pattern`] is an INCLUSIVE constraint: when non-empty,
///   only nodes whose kind tag matches are kept — except structural relationship
///   anchors/targets required to preserve branch and reconnect geometry. This
///   lets "Show messages only" stay server-side without severing the execution
///   topology.
/// - [`Self::hide_trace`] hides semantic trace rows (classified
///   [`crate::taxonomy::Visibility::Trace`]) unconditionally, like
///   `hide_undated`.
///
/// Chain endpoints (nodes with no parent or no child in the full graph) are
/// always preserved regardless of *hide* predicate matches. `hide_undated`,
/// `hide_trace`, and `include_kind_pattern` are not ordinarily endpoint-aware;
/// structural relationship anchors/targets are the topology-preserving
/// exception.
#[derive(Debug)]
pub struct ChainFilter {
    /// Regex/literal pattern matched against each node's display summary.
    pub summary_pattern: String,
    /// Regex/literal pattern matched against each node's kind tag.
    pub kind_pattern: String,
    /// Inclusive kind constraint: when non-empty, only nodes whose kind tag
    /// matches are kept. Empty means no inclusion constraint.
    pub include_kind_pattern: String,
    /// Hide nodes with no real timestamp (`timestamp_ms() == 0`).
    pub hide_undated: bool,
    /// Hide semantic trace rows (duplicate/echo/transport envelopes classified
    /// as [`crate::taxonomy::Visibility::Trace`]) unconditionally, splicing
    /// causal edges across them. Structural relationship anchors/targets are
    /// always preserved, matching `hide_undated` semantics.
    pub hide_trace: bool,
    /// Reconnect causal edges across hidden intermediate nodes so chains stay
    /// continuous instead of leaving disconnected stubs.
    pub splice: bool,
    summary_matcher: Matcher,
    kind_matcher: Matcher,
    include_kind_matcher: Matcher,
}

impl Default for ChainFilter {
    fn default() -> Self {
        Self::new(
            String::new(),
            String::new(),
            String::new(),
            true,
            true,
            false,
        )
    }
}

impl ChainFilter {
    /// Build a filter from raw patterns and flags.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        clippy::fn_params_excessive_bools,
        reason = "the filter flags are independent, named constructor parameters used across the workspace"
    )]
    pub fn new(
        summary_pattern: String,
        kind_pattern: String,
        include_kind_pattern: String,
        hide_undated: bool,
        splice: bool,
        hide_trace: bool,
    ) -> Self {
        Self {
            summary_matcher: Matcher::new(&summary_pattern),
            kind_matcher: Matcher::new(&kind_pattern),
            include_kind_matcher: Matcher::new(&include_kind_pattern),
            summary_pattern,
            kind_pattern,
            include_kind_pattern,
            hide_undated,
            hide_trace,
            splice,
        }
    }

    /// Whether this filter would hide nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.hide_undated
            && !self.hide_trace
            && self.summary_pattern.is_empty()
            && self.kind_pattern.is_empty()
            && self.include_kind_pattern.is_empty()
    }

    /// Whether a single node matches any active pattern-based HIDE predicate.
    #[must_use]
    fn matches_hide_pattern(&self, node: &HistoryNode) -> bool {
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
            include_kind_pattern: self.include_kind_pattern.clone(),
            hide_undated: self.hide_undated,
            hide_trace: self.hide_trace,
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
    /// Inclusive kind pattern string (empty = no inclusion constraint).
    pub include_kind_pattern: String,
    /// Hide undated flag.
    pub hide_undated: bool,
    /// Hide trace flag.
    pub hide_trace: bool,
    /// Splice flag.
    pub splice: bool,
}

/// Apply a [`ChainFilter`] to a canonical newest-first node list.
///
/// Returns cloned [`HistoryNode`]s with their causal parents rewritten so that
/// every kept child points at its nearest kept ancestor through any run of
/// hidden intermediate nodes.
///
/// `hide_undated` hides every ordinary undated node (including leaves).
/// `hide_trace` hides every trace-classified node (including leaves).
/// `include_kind_pattern` keeps only ordinary matching kinds (including
/// leaves). Pattern-based hide truncation preserves endpoints (no parent / no
/// child in the full graph) so a filtered chain keeps its anchors. Structural
/// relationship anchor and target rows (the rows that carry or point at a
/// `ForkOf` / `SubagentOf` / `ReconnectsTo` note) are preserved from every
/// hide predicate so branch/reconnect geometry stays visible even when their
/// kind (e.g. a tool-kind spawn marker) would be excluded by "messages only".
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "The relationship-note map keeps the default RandomState hasher, consistent with the projection's field; not worth generalizing for a read-only traversal."
)]
pub fn apply(
    nodes: &[HistoryNode],
    links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
    note_map: &HashMap<OpId, Vec<Op>>,
    representative: &HashMap<OpId, OpId>,
    filter: &ChainFilter,
) -> Vec<HistoryNode> {
    apply_owned(nodes.to_vec(), links, note_map, representative, filter)
}

/// Apply a [`ChainFilter`] while consuming an already-owned node list.
///
/// Projection ordering naturally produces an owned vector. Consuming it here
/// avoids cloning every operation payload a second time merely to rewrite the
/// parent set of retained rows.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "The relationship-note map keeps the default RandomState hasher, consistent with the projection's field; not worth generalizing for a read-only traversal."
)]
pub fn apply_owned(
    nodes: Vec<HistoryNode>,
    links: &std::collections::BTreeMap<OpId, Vec<editchain_core::GitLink>>,
    note_map: &HashMap<OpId, Vec<Op>>,
    representative: &HashMap<OpId, OpId>,
    filter: &ChainFilter,
) -> Vec<HistoryNode> {
    if filter.is_empty() || nodes.is_empty() {
        return nodes;
    }
    // The visible rows of THIS filtered list: a spliced/kept parent is only kept
    // when it resolves to one of them (directly or via a folded representative).
    let present = crate::row_node_keys(&nodes);

    // Structural relationship anchor/target rows are graph-topology-critical:
    // hiding them (e.g. "messages only" excluding a tool-kind spawn marker, or
    // a pattern that matches a branch row) would sever the virtual
    // fork/subagent/reconnect edges. Like chain endpoints, they are preserved
    // from every hide predicate below — a filtered view keeps branch/reconnect
    // geometry visible. Anchors are the canonical (visible) note-map keys;
    // targets are each note's raw ids lifted to their visible rows.
    let mut structural_keys = HashSet::with_capacity(note_map.len());
    for (anchor, notes) in note_map {
        for note in notes {
            if let editchain_core::OpKind::Note(n) = &note.kind {
                if !crate::is_protected_structural_relationship(n.relationship) {
                    continue;
                }
                if let Some(key) =
                    crate::canonical_parent_key(&anchor.to_string(), representative, &present)
                {
                    let _: bool = structural_keys.insert(key);
                }
                for target in &n.target_ids {
                    if let Some(key) =
                        crate::canonical_parent_key(&target.to_string(), representative, &present)
                    {
                        let _: bool = structural_keys.insert(key);
                    }
                }
            }
        }
    }

    // Original parent keys per key (op ids + git oid hex).
    let mut parents_of_key = HashMap::with_capacity(nodes.len());
    // Reverse adjacency for endpoint detection.
    let mut children_of_key = HashMap::with_capacity(nodes.len());
    for n in &nodes {
        let key = n.node_key();
        let ps = crate::canonicalize_parents(
            n.parent_keys(links, note_map),
            representative,
            &present,
            &key,
        );
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
    // not survive just because it happens to be an endpoint. `hide_trace`
    // behaves the same way for trace-classified rows: duplicate/echo/transport
    // envelopes are noise even as endpoints.
    //
    // Pattern-based hide truncation (summary/kind) instead preserves endpoints
    // (nodes with no parent or no child in the full graph) so a filtered chain
    // keeps its anchors — the oldest root and newest leaf stay visible even
    // when they match. The inclusive-kind constraint is NOT ordinarily
    // endpoint-aware: "messages only" excludes non-message kinds, including
    // lone leaves that would otherwise survive as anchors. Structural relation
    // anchors/targets are the explicit exception handled below.
    let mut hidden = HashSet::with_capacity(nodes.len());
    for n in &nodes {
        let key = n.node_key();
        // Structural relation anchors/targets are never hidden: their rows
        // carry the virtual edges that keep branch/reconnect geometry visible.
        if structural_keys.contains(&key) {
            continue;
        }
        if filter.hide_undated && n.timestamp_ms() == 0 {
            let _: bool = hidden.insert(key);
            continue;
        }
        if filter.hide_trace && n.visibility() == Visibility::Trace {
            let _: bool = hidden.insert(key);
            continue;
        }
        if !filter.include_kind_pattern.is_empty()
            && !filter.include_kind_matcher.matches(&n.kind())
        {
            let _: bool = hidden.insert(key);
            continue;
        }
        if !filter.summary_pattern.is_empty() || !filter.kind_pattern.is_empty() {
            let has_parent = !parents_of_key.get(&key).is_none_or(Vec::is_empty);
            let has_child = !children_of_key.get(&key).is_none_or(Vec::is_empty);
            let is_endpoint = !has_parent || !has_child;
            if !is_endpoint && filter.matches_hide_pattern(n) {
                let _: bool = hidden.insert(key);
            }
        }
    }

    // Rewrite each kept node's parents to its nearest kept ancestors.
    let mut result = Vec::with_capacity(nodes.len());
    for mut n in nodes {
        let key = n.node_key();
        if hidden.contains(&key) {
            continue;
        }
        let spliced = if filter.splice && !hidden.is_empty() {
            nearest_kept_ancestors(&key, &parents_of_key, &hidden)
        } else {
            n.parent_keys(links, note_map)
        };
        n.override_parent_keys(&crate::canonicalize_parents(
            spliced,
            representative,
            &present,
            &key,
        ));
        result.push(n);
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
