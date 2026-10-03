# Operation schema 3

EC03 wire version 2 now carries operation schema 3. The binary framing and the
32 MiB segment / 16 MiB inline defaults stay unchanged.

| Type | Meaning |
| --- | --- |
| Session | Session settings, participants and recorded lifecycle changes |
| Turn | An agent execution, its attempt, triggers and outcome |
| Message | Text, reasoning, plans or summaries, in ordered content blocks |
| Tool | A call and its updates; terminal commands include command, cwd and exit code |
| File | Open, close, view, read, create, change, save, rename, delete or snapshot |
| Commit | A repository-scoped Git commit observation |
| Note | Comment, label, correction, standalone error or recording gap |
| Author | Recorded identity metadata and role |
| Link | A connection recorded separately from its endpoints |
| Original | Exact input bytes, source identities and optional recorded location |

The common envelope has `id`, `item`, `author`, `recorder`, `session`, `turn`,
`time_ms`, `sequence`, `parents`, `causes`, `original`, and `legacy`. Missing
information is explicit. Human edits need no turn. A buffer observation alone
cannot establish who made the edit: later input receipts become targeted notes.
File edits retain byte or UTF-16 coordinates explicitly; a reported applied change
can have unavailable patch bytes. Unknown completion status stays `Unknown`.

`id` identifies one immutable observation. `item` identifies the message, call,
session, turn or other logical object across observations. Both use full 256-bit
addresses. CLI output can abbreviate operation addresses; indexed item filters
also accept unique prefixes. Save full IDs, since later imports can make a short
prefix ambiguous.

## Streaming

Producers construct `activity::Operation`, validate it with `into_op()`, and append
through the existing durable writer or CLI JSON input. `append`, `follow`, export,
restore and replication accept schema-three records. Older operation schemas remain readable through `Operation::view`. Producers
supply the meaning and source identity of their records; the engine validates
and stores their envelopes without parsing application payloads.

Each event is immutable. Content updates specify `Append` or `Replace`, a block ID,
optional position and explicit predecessor. Tool output also belongs to an attempt
and channel. `StreamState` accepts reordered and repeated delivery, reports missing
prefixes as partial, and rejects conflicting bytes, cross-item predecessors and
unjoined branches. Whole snapshots can use a recorded sequence from one recorder;
timestamps and hash order do not decide which snapshot wins. Final archive records
do not manufacture starts, intermediate chunks or successful outcomes.

A conflicting ID permanently quarantines all its versions in replay. Reads of
affected items or tool attempts fail explicitly. Replacements still validate
known predecessors, including their item, block, attempt and cycle structure;
rebuilding a snapshot does not require superseded payload bytes. Consumers use
`Op::parent_ids()` for the complete causal list, including third and later parents.

## Queries

History queries can filter by kind, item, session and turn. `Operation::view`
provides the current vocabulary for older records. `display_op` is a lossy read
adapter and must not be persisted. Physical storage migration preserves records;
application schema conversion belongs to the producer.
