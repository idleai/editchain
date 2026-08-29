<!-- BEGIN MAIN-AGENT ORCHESTRATION -->
## Main-agent orchestration policy

When multi-agent tools are available, the top-level agent should operate primarily as the
coordinator, integrator, and final reviewer. Delegate every substantive exploration,
implementation, debugging, testing, review, or documentation work item to a subagent by default.
The main agent should normally perform only control-plane work:

- understand the request well enough to decompose it;
- define task boundaries, dependencies, constraints, and acceptance criteria;
- assign one clear owner to each task and avoid concurrent edits to overlapping files;
- monitor progress, answer subagent questions, and coordinate handoffs;
- inspect and integrate completed work, resolve conflicts, and run final repository-level checks;
- communicate status, risks, decisions, and the final result to the user.

Use the project-configured default subagent profile. Omit explicit model or profile overrides unless
the user requests a different route or the task has a concrete model-specific requirement.
Do not request `max` reasoning effort for routine delegation. Omit the reasoning override so the
project profile supplies its `high` default. Use a different effort only when the user requests it
or the task has a specific, stated reason for matching another effort level.

### Give subagents room to work

- Give each subagent enough context to act autonomously: the objective, exact scope, relevant paths,
  constraints, expected deliverables, and required verification.
- Prefer end-to-end task ownership over fragmented command-by-command delegation. A subagent that
  owns an implementation should normally inspect, edit, test, and report on that implementation.
- Do not duplicate a subagent's assigned investigation or implementation while it is running.
- Parallelize independent tasks, but serialize tasks that touch the same files or depend on the same
  unresolved design decision.
- Treat slow progress as normal. Use long, patient waits and status checks instead of interrupting
  an agent merely because it has not responded quickly.
- Never send "hurry up," "stop exploring," "return now," or equivalent instructions merely because
  a few minutes have elapsed or one or more wait calls timed out. A wait timeout is not evidence that
  the subagent is stuck.
- Before nudging a running subagent, require objective evidence of a problem: an explicit error, a
  request for help, repeated identical failed actions without new evidence, or a user-imposed
  deadline. If the agent is still making progress, leave it alone.

### Recovery and intervention

When there is objective evidence that a subagent is struggling, preserve its ownership when
practical and recover in this order. Do not enter this recovery sequence based on elapsed time alone:

1. Send a focused follow-up with the missing evidence, corrected constraint, or narrower objective.
2. Ask the same subagent to stop broad exploration and complete the smallest useful result.
3. Assign a replacement or specialist subagent when the original agent is genuinely stuck or the
   task needs an independent approach.
4. Have the main agent take over implementation only when repeated recovery attempts fail, when an
   integration issue spans multiple delegated tasks, or when immediate intervention is needed for
   safety or correctness.

The main agent may intervene sooner for destructive or externally visible actions, permission or
credential boundaries, ambiguous user intent, shared-workspace conflicts, and final integration.
It remains responsible for reviewing the actual diff and evidence; a subagent's success report is
not by itself proof that the overall request is complete.

### Child-agent behavior

A spawned subagent should execute its assigned task directly and own it through verification. It
should not delegate again by default, because recursive delegation obscures ownership and can create
unbounded agent trees. A child may delegate only when its assignment explicitly calls for parallel
work or when it first tells the parent why another agent is necessary. Stay within the assigned
scope, preserve unrelated workspace changes, and return concrete findings, changed paths, tests, and
remaining risks.

<!-- END MAIN-AGENT ORCHESTRATION -->

<!-- BEGIN QUALITY POLICY -->
## Lint policy

Before declaring a code task complete, run `./scripts/lint.sh` and report
its exact result.

Unless a task explicitly authorizes policy work, the agent must not:

- weaken, remove, or reclassify a lint;
- change complexity, coverage, duplication, or mutation thresholds;
- rewrite a quality baseline;
- skip or conditionally bypass a CI quality job;
- add a coverage, complexity, mutation, or dependency exclusion;
- add `#[allow(...)]` or crate-wide `#![allow(...)]`;
- mark a failing test ignored;
- delete a test solely to pass a gate.

Necessary suppressions must use narrowly scoped `#[expect(..., reason = "...")]`
and must be called out in the final report.

New code must meet absolute limits, and modified code may not worsen tracked metrics.

<!-- END QUALITY POLICY -->

<!-- BEGIN DEBUGMCP INTERACTIVE DEBUGGING -->
## Interactive debugging with DebugMCP

This repository uses Microsoft DebugMCP to share the visible VS Code debug
session between the developer and the coding agent.

### Required launch behavior

- Use the existing VS Code launch configuration named:
  `Rust: vGDB (interactive) — editchain-node/editchain`
- Always pass the exact `configurationName` to `start_debugging`.
- Pass an absolute Rust source path as `fileFullPath`.
- Pass the repository root as `workingDirectory`.
- Do not rely on DebugMCP's automatically generated Rust configuration; this
  project intentionally uses its named GDB-backed DAP configuration.

### Shared-session contract

- Treat an active VS Code debug session as developer-owned shared state.
- Do not start a second session when the intended Rust session is already active.
- Do not stop, restart, continue, or step the session without stating the next
  observation goal. When the developer is actively driving the session, wait for
  explicit authorization before changing execution state.
- The developer may manually step, pause, continue, select another thread, or
  select another stack frame in VS Code.
- Manual developer actions are not assumed to stream automatically to the agent.
  When the developer says "inspect now," re-read the active variables/frame.
- When execution is running and no breakpoint is imminent, ask the developer to
  use VS Code's Pause control; do not invent a DebugMCP pause capability.
- Do not restart or stop a session just to recover context. First inspect the
  current active stopped frame.

### Breakpoint and inspection discipline

- Prefer one or two hypothesis-driven breakpoints.
- For `add_breakpoint`, use exact, nonblank `lineContent` that is unique in the current file; the tool adds a breakpoint to every matching line.
- Confirm the breakpoint is listed and verified before continuing.
- At a stop, first inspect locals, then evaluate only narrowly scoped expressions.
- Do not evaluate functions or expressions with possible side effects unless the
  developer explicitly approves it.
- Never probe credentials, tokens, private keys, process environment secrets, or
  unrelated sensitive memory.
- Limit unreviewed stepping to ten source-level operations per hypothesis.
- Report the current file, line, frame/function, relevant local values, and the
  observation that supports or rejects the hypothesis.

### Suggested tool sequence

1. `add_breakpoint`
2. `list_breakpoints`
3. `start_debugging` with the exact named configuration, but only when no intended
   session is already active
4. `get_variables_values` with `scope: "local"`
5. zero or more bounded `step_over`, `step_into`, or `step_out` operations
6. `get_variables_values` again after each meaningful stop
7. `continue_execution` only toward a known breakpoint/observation
8. `stop_debugging` only when the developer authorizes ending the session

### Tool call shape

Use values equivalent to:

```json
{
  "fileFullPath": "/mnt/hot/ambientlight/repos/editchain/crates/editchain-node/src/bin/editchain.rs",
  "workingDirectory": "/mnt/hot/ambientlight/repos/editchain",
  "configurationName": "Rust: vGDB (interactive) — editchain-node/editchain"
}
```

For a breakpoint:

```json
{
  "fileFullPath": "/mnt/hot/ambientlight/repos/editchain/crates/editchain-node/src/bin/editchain.rs",
  "lineContent": "let cli = Cli::parse();"
}
```
<!-- END DEBUGMCP INTERACTIVE DEBUGGING -->
