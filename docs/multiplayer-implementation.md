# Multiplayer implementation checkpoints

Started September 15, 2026 from `92be7c9`. Implementation follows
[the architecture](multiplayer-architecture.md). Every checkpoint is tested and
committed locally using `astramax(f1/multiplayer):`.

## Decisions for the first implementation

- Share one explicitly selected workspace per space. Default to new history;
  backfill is an explicit choice. Retain original encoded evidence and actors.
- Use persistent device certificates and mutual TLS in Rust over the tunnel
  stream. Pair approved device identities through an invitation exchange.
- Keep network data on a dedicated native peer interface. Reuse short durable
  store transactions and the existing writer lock, allowing local capture to run.
- Each replica owner controls its device allowlist. Discovery cannot enroll a
  device. Removing a device closes local sessions; it does not recall copies
  another participant already received.
- Implement exact paginated inventories first, with separate durable record and
  blob acknowledgments. Measure before adding inventory compression.
- Keep GitHub discovery optional and independent of live synchronization.

## Checkpoints

| Checkpoint | Required evidence | Status |
| --- | --- | --- |
| 0. Architecture and execution contract | Source links and repository state checked | Complete |
| 1. Durable replication kernel | Gap/conflict convergence, blob validation, scope and replay/restart tests; root lint | In progress |
| 2. Authenticated native peers | Separate processes, pinned certificates, rejection before inventory, bounded IPC; root lint | Pending |
| 3. Extension Host/Join and live relay | Real relay between separate native replicas, lifecycle and cleanup, UI commands and packaging tests; root lint | Pending |
| 4. Reconnect, discovery and three peers | Offline catch-up, revocation, stale discovery, third-party forwarding; root lint | Pending |
| 5. Full extension E2E | Actual VS Code history visibility, packaged build, final regression checks | Pending |

The final different-account/different-network observation requires a second
authorized account and machine. That input has been requested while local and
relay automation proceed. Automated same-machine tests will be identified as
such; they are not evidence of a second network.

## Validation record

- Baseline: the developer's VS Code spike passed using `read:user, read:org`,
  encrypted V1 transport, 20 round trips, and successful tunnel deletion.
- Checkpoint 0: existing research files preserved; architecture references and
  checkpoint scope reviewed against the current tree. No runtime change.
- Checkpoint 1a: added `editchain-sync` with exact conflict-aware inventories,
  durable scoped record/blob ingestion, persistent new-history exclusions, and
  bounded incremental framing. Seven real-store/framing tests passed, including
  writer contention, restart/replay, pagination, and private-blob access through
  forged remote references. `cargo clippy -p editchain-sync --all-targets --locked
  -- -D warnings` exited 0. `./scripts/lint.sh` exited 0: `RESULT: PASS`.
  Peer scheduling and nested structured-content hydration follow in 1b.
