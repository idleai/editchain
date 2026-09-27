# Engine queries

`editchain_engine::queries::ChainQueries` reads recorded facts from one chain.
Workspace taxonomy, UI state, and controller interpretation belong to consumers.

Open a handle with `engine.queries()`, `ChainQueries::open(path)`, or
`ChainQueries::from_index(index)`. Reuse it: the handle owns the index checkpoint lock.

```rust
use editchain_engine::{OpId, queries::{ChainQueries, ContentField, ContentQuery, PageRequest}};

fn inspect(chain: &std::path::Path, revision: OpId) -> std::io::Result<()> {
    let mut queries = ChainQueries::open(chain)?;
    let _changes = queries.refresh()?;
    let _history = queries.history(None, PageRequest::default())?;
    let _matches = queries.search("error", None, PageRequest::default())?;
    let _content = queries.content(ContentQuery {
        operation: revision,
        field: ContentField::FileAfter,
    })?;
    let _diff = queries.diff(revision)?;
    let _context = queries.operation_context(revision)?;
    let _ancestors = queries.ancestors(revision, 100)?;
    Ok(())
}
```

## Queries

| Method | Returns |
| --- | --- |
| `history(key, page)` | Accepted operations, record references, and content availability. |
| `operation(id)` | One operation, or an explicit missing/conflicted result. |
| `record_variants(id)` | All original encoded variants, including conflicts. |
| `search(text, key, page)` | Case-sensitive literal matches with byte ranges and content gaps. |
| `content(ContentQuery)` | Exact bytes of a selected record field. |
| `diff(revision)` | A file revision's before/after content, recorded edit, and byte comparison. |
| `compare(before, after)` | A byte comparison between two selected fields. |
| `operation_context(id)` | The operation, actor/session records, direct parents, and relationships. |
| `ancestors(id, limit)` | A bounded parent walk, including missing/conflicted records and an unvisited frontier. |
| `relationships(entity, page)` | Recorded causal, annotation, session, and Git relationships. |
| `git(GitQuery, page)` | Commit observations and links for a repository and optional full OID. |

The optional `IndexKey` filters history and search by recorded actor, scope, path,
parent, or content address. Full types and limits are in the
[Rust API source](../crates/editchain-engine/src/queries/mod.rs).

## Record references and missing content

Recorded facts carry a `record_ref: RecordRef { operation, record_hash }`. The hash
is BLAKE3 of the original encoded record. Match it against the `reference` of an
`EncodedRecord` from `record_variants(operation)` to retrieve the bytes. References
survive rebuilds and replay into another directory.

Lookups return `Found`, `Missing`, or `Conflicted`. Conflicted identities stay out
of accepted history; their original encoded variants remain available.
Parent and ancestor entries use `OperationLookup` to keep the requested ID alongside
its lookup result.

Content returns `Available(bytes)`, `NotRecorded` (no field value), `Missing`
(blob absent), `Corrupt`, or `Unresolvable`. Empty content is valid. Search reports
gaps for unavailable fields; an empty hit list does not cover those fields.

Diffs compare complete byte sequences, including binary files. A change is one
replacement range in each side. If either side is unavailable, the comparison is
`Unavailable`; any recorded patch remains accessible separately.

Causal walks follow only envelope parents. Git queries preserve recorded ref
snapshots and require a repository identity; they do not read current branches
or the working tree. Task status and authorship scores are consumer interpretations
of these recorded facts.

## Paging and refresh

`PageRequest` uses an exclusive `after: OpId` and scans 1–1000 candidate operations
(default 100) in operation-ID order. Follow `next_after` until it is `None`, even
when a filtered page is empty. This bounds operations, not content size.
`operation_context` may scan the whole chain; use paged relationships for bounded reads.

Call `refresh()` to observe new records, conflicts, and late blobs. Use its change
IDs to update results: new imports can have IDs older than your page cursor.
After reopening or rebuilding, query again; refresh changes are not a durable
subscription.

Use `index().stats()` for conflict, undecodable-record, and incomplete-tail counts,
and `index().verify_integrity()` to audit storage. For unchanged records and content
availability, reopening, rebuilding, and replay preserve query results.
