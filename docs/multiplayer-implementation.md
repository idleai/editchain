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
| 1. Durable replication kernel | Gap/conflict convergence, blob validation, scope and replay/restart tests; root lint | Complete |
| 2. Authenticated native peers | Separate processes, pinned certificates, rejection before inventory, bounded IPC; root lint | Complete |
| 3. Extension Host/Join and live relay | Real relay between separate native replicas, lifecycle and cleanup, UI commands and packaging tests; root lint | Complete |
| 4. Reconnect, discovery and three peers | Offline catch-up, revocation, stale discovery, third-party forwarding; root lint | In progress |
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
- Checkpoint 1b: pull-based peer sessions negotiate the space and both encoding
  versions, batch durable records, bound each transfer, and hydrate known
  `vscode.work` revision references. Records and blobs have separate durable
  acknowledgments. All 15 tests passed: fragmented frames, paginated exact
  convergence, conflict quarantine, three replicas, mid-blob disconnect, lost
  acknowledgment, late content, private-content boundaries and writer contention.
  `./scripts/lint.sh` exited 0: `RESULT: PASS`.
- Identity audit: editor operation IDs derive from the recorder's full session
  identity and sequence; normalized records use separate deterministic lanes.
  Human attribution is explicitly unsigned and retained unchanged. Provider
  import streams use provider-owned source keys for portable identity. Peer
  authentication will identify a supplying device, not assert record authorship.
- Checkpoint 2: added the dedicated `editchain-peer` executable. Device keys stay
  in application-private local storage, outside replicated chains; Unix files are
  mode 0600 in a mode 0700 directory. Rustls 0.23.45 and rcgen 0.14.10 implement
  TLS 1.3 with mutual certificate validation, byte-exact pins and required ALPN.
  Resumption and early data are disabled. Each local turn rechecks approval.
  Twenty unit tests plus one real separate-process E2E test passed. Wrong client,
  wrong server, wrong space, ciphertext tampering, raw history-RPC injection,
  restart and live revocation were exercised. `./scripts/lint.sh` exited 0:
  `RESULT: PASS`. The extension packaging checkpoint will ship both binaries.
- Checkpoint 3: production Host/Join commands, private connect-only invitations,
  endpoint validation, device approval/removal, bounded native stream bridge,
  workspace/account lifecycle and leased cleanup journals are wired. Received
  editor source evidence is excluded from local normalization; foreign conflicts
  still quarantine without blocking independent local capture. Native peers wait
  up to two seconds for a short local writer transaction before reconnect is
  required, after the concurrent-capture test exposed premature disconnection.
  All 161 extension tests passed (no skips), including simultaneous large
  transfers, exact historical diffs, credential-safe errors and Stop during login.
  `./scripts/lint.sh` exited 0: `RESULT: PASS`.
- Checkpoint 3 package: staged both release binaries and production dependencies
  in `outputs/editchain-history-multiplayer-checkpoint-3.vsix`. Verified executable
  permissions after extracting the VSIX. The extracted package completed the
  real relay E2E with two native processes: setup 2,411 ms, total 5,482 ms, 14
  content blobs, received History rows and exact before/after diff verified,
  working tree unchanged, tunnel deleted. This used one machine and one host
  GitHub account; no different-network claim is made.

## Native peer interface

`editchain-peer` reads local JSON frames with a four-byte little-endian length
bounded to 512 KiB. Commands are `identity`, `verify`, `configure`, `approve`, `revoke`,
`devices`, `open`, `turn`, and `close`. One worker owns one peer connection.
Network input is base64 inside `turn`, decoded with a 64 KiB bound, and passed
only to TLS. A turn returns up to 256 KiB of opaque output plus public identity
and durable progress. Errors expose fixed codes, not payloads or credentials.
Use a new connection after a failed turn; durable inventories repair replay.

The implementation uses the ordinary certificate verifiers and byte-stream
APIs in [Rustls](https://docs.rs/rustls/latest/rustls/client/struct.ClientConnection.html),
including its [mandatory client certificate verifier](https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html).
It does not implement a custom signature or certificate-verification algorithm.
