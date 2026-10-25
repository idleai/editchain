# CI and release checks

Version commits are staged on `ci/release/<source>` before release verification.
The publish job records the successful `CI required` check on that exact commit
and advances `main` with a normal fast-forward push. Failed validation or a newer
main commit prevents promotion. The merge rules therefore need no bot bypass.

The repository's `CI required` ruleset accepts this check only from GitHub
Actions, without requiring a fresh base-branch rebuild for every merge. Activate
it after the release promotion workflow above lands on the default branch.
