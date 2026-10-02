# Extension assembly handoff (f43)

The new Idle package in `idleai/vscode-extension` mounts the shared web-ui
workspace/history/session components over app-core. Local history discovery,
queries, exact record inspection and native file/diff actions use explicit
folder bindings and the packaged engine service. Capture belongs to activation,
including when views are closed. Its VSIX contains all native/WASM assets for
the build host's platform and requires no source checkout at runtime.

The temporary peer state binding moved from
`crates/editchain-history-renderer/src/client_state.rs` to
`crates/editchain-client-state/src/lib.rs`. The renderer no longer exports
`SharedJoin` or `SharedConnection`. The existing Node import path and class APIs
are unchanged; they load a separate small module backed by app-core's
`idle-history`. `npm run build:client-state` can build that module without the
renderer. `npm run build:renderer` builds both committed asset trees, and CI
continues checking both for reproducibility and WASM lint errors.

The remaining legacy extension is still an active consumer. Keep its existing
commands, capture recovery and live-history behavior runnable while the following
callers switch. Installing the new VSIX alone does not migrate legacy settings or
pending capture outboxes. Use one capture owner for a workspace during migration;
drain the old capture outbox before disabling its tracking.

| Remaining source | Removal condition and owner |
| --- | --- |
| `tools/codex-session-exporter`, extension `liveHost.ts` / `liveSources.ts` / `liveSync.ts` and native live import calls | f10 switches the exporter and its consumers to Evo while retaining raw records, retry identities and ongoing capture. |
| `src/multiplayer/`, `src/devTunnels/`, `editchain-client-state`, `media/client-state/pkg/` | f18 switches standalone coordination/discovery consumers. Their destination must preserve sharing approval, reconnect state and cleanup behavior. |
| `src/humanWork.ts`, editor capture adapters and native editor compatibility entrypoints | f43 completes the installed-host/settings/outbox handoff to the f39 owner. The old extension still uses these while its users migrate. |
| Legacy `extension.ts` document providers and history command wiring | f43 switches the remaining legacy commands to f40's bound native actions. The new Idle view already uses f40 directly. |
| `editchain-history-renderer::app` / `shell`, `media/main.css`, `media/rust-history/`, coordinate-based service endpoints and geometry/protocol re-exports | f43 retires the old extension mount after those live consumers switch; f60 owns any remaining browser host caller. Replacement browser/native interaction tests must continue covering the retired behavior. |

The engine schema, store, indexes, queries, replication and CLI remain in EditChain.
Shared state and rendering remain in app-core/web-ui. This handoff does not mark
production runtime connections or complete legacy source retirement as finished.
