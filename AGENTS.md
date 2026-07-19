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