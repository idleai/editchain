#![doc = "Document identity, bounded continuation, and BM25 query contracts."]

use std::collections::HashSet;

use editchain_core::{GitCommitKey, GitOid, NodeId, OpId, RepositoryId};
use editchain_index::{
    ChunkOptions, DocumentId, LexicalIndex, LexicalIndexBuilder, SearchDocument, MAX_CANDIDATES,
};
use tantivy as _;

fn index(documents: &[SearchDocument<'_>]) -> Result<LexicalIndex, Box<dyn std::error::Error>> {
    let mut builder = LexicalIndexBuilder::new(ChunkOptions::default())?;
    for document in documents {
        builder.add_document(document)?;
    }
    builder.publish()
}

const fn operation(seq: u64) -> DocumentId {
    DocumentId::Operation(OpId::new(NodeId(1), 0, seq))
}

#[test]
fn index_and_search_message() {
    let index = index(&[SearchDocument {
        id: operation(1),
        text: "hello world test query",
        exact_terms: &[],
    }])
    .unwrap();
    let results = index
        .candidates("hello", 10)
        .unwrap()
        .next_page(10)
        .unwrap();
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits.first().unwrap().document, operation(1));
    assert!(!results.more);
}

#[test]
fn source_domains_and_repository_qualified_ids_remain_exact() {
    let sha1 = GitOid::from_sha1([0xab; 20]);
    let identities = [
        DocumentId::Operation(OpId::new(NodeId(u64::MAX), u32::MAX, u64::MAX)),
        DocumentId::GitCommit(GitCommitKey::new(RepositoryId(u64::MAX), sha1)),
        DocumentId::GitCommit(GitCommitKey::new(RepositoryId(2), sha1)),
        DocumentId::GitCommit(GitCommitKey::new(
            RepositoryId(2),
            GitOid::from_sha256([0xab; 32]),
        )),
    ];
    let documents: Vec<_> = identities
        .iter()
        .map(|id| SearchDocument {
            id: *id,
            text: "shared needle",
            exact_terms: &[],
        })
        .collect();
    let index = index(&documents).unwrap();
    let page = index
        .candidates("needle", 10)
        .unwrap()
        .next_page(10)
        .unwrap();
    let actual: HashSet<_> = page.hits.iter().map(|hit| hit.document).collect();
    assert_eq!(actual, HashSet::from(identities));
    assert!(!page.more);
}

#[test]
fn continuation_preserves_ranking_without_losing_or_repeating_chunks() {
    let long = "needle ".repeat(2000);
    let index = index(&[
        SearchDocument {
            id: operation(1),
            text: &long,
            exact_terms: &[],
        },
        SearchDocument {
            id: operation(2),
            text: "needle two",
            exact_terms: &[],
        },
        SearchDocument {
            id: operation(3),
            text: "needle three",
            exact_terms: &[],
        },
    ])
    .unwrap();
    let budget = index.num_docs().saturating_add(1);
    let all = index
        .candidates("needle", budget)
        .unwrap()
        .next_page(budget)
        .unwrap();
    assert!(all.hits.len() > 3);
    assert!(!all.more);
    let mut query = index.candidates("needle", budget).unwrap();
    let mut paged = Vec::new();
    loop {
        let page = query.next_page(2).unwrap();
        paged.extend(page.hits);
        if !page.more {
            break;
        }
    }
    assert_eq!(paged, all.hits);
    let mut bounded = index.candidates("needle", 3).unwrap();
    assert!(bounded.next_page(2).unwrap().more);
    let last = bounded.next_page(2).unwrap();
    assert_eq!(last.hits.len(), 1);
    assert!(last.more);
    assert_eq!(bounded.remaining_budget(), 0);
    assert!(bounded.next_page(1).is_err());
}

#[test]
fn invalid_limits_and_syntax_return_errors() {
    let index = index(&[]).unwrap();
    assert!(index.candidates("x", 0).is_err());
    assert!(index
        .candidates("x", MAX_CANDIDATES.saturating_add(1))
        .is_err());
    assert!(index.candidates("\"", 1).is_err());
    assert!(index.candidates("x", 1).unwrap().next_page(0).is_err());
    let empty = index.candidates("x", 1).unwrap().next_page(1).unwrap();
    assert!(empty.hits.is_empty());
    assert!(!empty.more);
}

#[test]
fn prose_phrases_paths_identifiers_and_exact_git_oids() {
    let sha1 = "abcdef0123456789abcdef0123456789abcdef0123";
    let sha256 = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
    let path = "src/search_engine.rs";
    let index = index(&[SearchDocument {
        id: operation(1),
        text: "hello world src/search_engine.rs parse_history API::find exact-thing",
        exact_terms: &[sha1, sha256, path],
    }])
    .unwrap();
    for query in [
        "HELLO",
        "\"hello world\"",
        "src/search_engine.rs",
        "\"src/search_engine.rs\"",
        "parse_history",
        "\"API::find\"",
        "exact-thing",
        sha1,
        sha256,
    ] {
        let page = index.candidates(query, 10).unwrap().next_page(10).unwrap();
        assert_eq!(page.hits.len(), 1, "query: {query}");
    }
    assert!(
        index.candidates("API::find", 10).is_err(),
        "Tantivy field punctuation still requires quoting"
    );
    assert!(index
        .candidates("\"world hello\"", 10)
        .unwrap()
        .next_page(10)
        .unwrap()
        .hits
        .is_empty());
}
