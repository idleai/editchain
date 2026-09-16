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
| 4. Reconnect, discovery and three peers | Offline catch-up, revocation, stale discovery, third-party forwarding; root lint | Complete |
| 5. Full extension E2E | Actual VS Code history visibility, packaged build, final regression checks | Complete on one machine |

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
- Checkpoint 4a: automatic peer/host reconnect with bounded backoff, deterministic
  opposite-connection resolution, and workspace restart recovery are implemented.
  Approved grants are stored in VS Code SecretStorage; restoring a saved session
  rechecks current local membership and never reenrolls a removed device. Host
  resources survive reload with fresh endpoints; explicit Stop deletes them and
  disables resume. Capture, live imports and sync now share the same bounded
  writer-acquisition helper after the full extension suite exposed a capture-side
  contention race. All 166 extension tests passed. `./scripts/lint.sh` exited 0:
  `RESULT: PASS`. The real relay test passed with three independent stores,
  host/guest restart, offline catch-up, exact historical diffs forwarded through
  a second replica while the source was offline, and live revocation. Setup was
  3,563 ms; total 27,448 ms; both temporary tunnels were deleted. Optional directory
  discovery and the two-window VS Code UI run remain for checkpoints 4b and 5.
- Checkpoint 4b: optional repository-variable discovery is implemented with
  explicit repository/scope consent, public-only advertisements, serialized
  polling, cancellation and withdrawal. Tests verify pagination, stale/wrong-space
  entries, altered certificates, unknown devices, oversized responses, safe
  errors and Stop during authentication/publication. A grant cannot move to
  another tunnel. All 172 extension tests passed. One earlier full run hit the
  existing renderer harness's Chrome `Promise was collected` error; the complete
  rerun passed without exclusions or changes to that test. `./scripts/lint.sh`
  exited 0: `RESULT: PASS`. Directory HTTP behavior is tested through controlled
  API responses; no real repository variable has been written during this work.
- Checkpoint 5a: the VSIX passed the two-window UI E2E in VS Code 1.132.0.
  Both instances loaded the package from separate profile installations, used
  the real approval dialogs and captured actual typing. Both remote History rows
  opened exact native before/after diffs without changing receiving working
  files. The host process restarted with its profile and chain; another guest
  edit arrived without a new invitation. The run took 111 seconds, including
  recovery after the application restart. Both test cases passed,
  tunnel cleanup completed, and the artifact audit found no credentials in logs.
  The test-only GitHub provider used the CLI account; it is not shipped.
  `./scripts/lint.sh` exited 0: `RESULT: PASS`. Later timing separated the
  application restart from the longer peer reconnect, as recorded below.
- Checkpoint 5b: cleanup now claims saved-resource ownership before contacting
  the service, checks actual service labels, preserves another live process's
  lease, and refuses cleanup while this window is enabled but reconnecting.
  A missing extension-host process permits immediate lease takeover. The native
  watchdog also covers opening handshakes and pending transport writes; the
  blocked-handshake regression failed before the fix. Both blocked handshakes
  and blocked writes between authenticated native peers now time out and release
  their processes/streams. All 177 extension tests passed, with no failures or
  skips; TypeScript compilation and the UI harness type/syntax checks passed.
  `./scripts/lint.sh` exited 0: `RESULT: PASS` (fmt, check, clippy, tests, doc tests,
  deny). No quality policies or suppressions changed.
- Final release-binary relay run: three replicas passed large-content transfer,
  exact historical diffs, restart catch-up, explicit reconnect, forwarding while
  the source was offline, and revocation. Setup was 1,806 ms; total 14,574 ms;
  19 content blobs were retained and both temporary tunnels were deleted.
- Final packaged UI run: both VS Code cases passed in about 105 seconds of
  scenario execution. Actual device approval, typing, received History rows,
  exact native diffs, unchanged receiving files and recovery after a full host
  restart all passed. Tunnel cleanup completed; the log audit found no account
  token, connect grant or private invitation material. The VSIX contains both
  executable Linux x64 binaries and the current runtime, and excludes the test
  provider and automation scripts. Local artifacts:
  [VSIX](../outputs/editchain-history-multiplayer.vsix),
  [run result](../extensions/vscode-editchain/trace/multiplayer/run.json),
  [host observations](../extensions/vscode-editchain/trace/multiplayer/host/observations.json),
  [diff after restart](../extensions/vscode-editchain/trace/multiplayer/host/received-after-reload-diff.png).
  VSIX SHA-256: `d57eaf860a37821c9927df8db85b849bf39eed8c85e5d43badc2cbd69fc32546`.

## Recovery review checkpoints

### Local capture

- A conflicting received variant no longer permanently blocks the editor
  checkpoint. The recorder rebuilds its derived state from canonical sources,
  retains conflicting derivations as evidence, and settles replay before
  admitting the next observation. Quarantined snapshots cannot supply future
  revisions.
- Exact local retries and sequence checks can consult a unique retained local
  variant, distinguished by the received-byte receipts. This never restores
  that variant to canonical history or acknowledges changed retry content.
- Regression coverage includes a conflict at the recorder frontier, loss of a
  previously used snapshot, exact retries, and rebuilding the derived cache.
  `./scripts/lint.sh`: exit 0, `RESULT: PASS`.

### Durable space recovery

- The read-only native `scope` command returns the existing on-disk binding.
  Host and Resume recover it when VS Code workspace metadata is missing,
  including after moving a chain. Inspection cannot create storage or widen
  the history baseline. Corrupt or unsupported metadata fails explicitly.
- A conflicting cached space still requires a separate replica; recovery
  never silently rebinds history or changes device approvals.
- Native scope validation and 11 extension/native recovery tests passed,
  including baseline preservation, moved storage and stale metadata.
  `./scripts/lint.sh`: exit 0, `RESULT: PASS`.

### Session lifecycle and discovery

- Changes to worker paths suspend and resume the existing session. Unrelated
  folders/settings leave it running; changing the actual chain or removing its
  folder closes that session. Serialized restarts retain Stop cancellation and
  ignore status callbacks from replaced managers.
- Resume starts saved discovery without a redundant host connection attempt.
  Hosting failures retry independently of outbound connections. Cancelling or
  failing discovery input/authentication keeps the previous configuration.
  Stop during sign-in, resume or restart cannot restart sharing or display a
  stale command error.
- Extension compilation, all 189 harness tests and multiplayer type checks
  passed. The targeted command/native recovery run also passed with an offline
  guard recording zero socket, fetch or GitHub CLI attempts.
  `./scripts/lint.sh`: exit 0, `RESULT: PASS`.

### Permission diagnostics

- Native peer authentication failures now carry a distinct internal error type.
  A local filesystem permission failure returns `storage_permission_denied`,
  while an unapproved/revoked device or TLS failure retains
  `authentication_failed`. Neither response exposes paths or credentials.
- Real worker probes with unreadable membership metadata and an unwritable
  policy directory returned the storage code. The production extension decoder
  preserved it. Existing TLS/pin/revocation tests, all 189 extension tests,
  multiplayer type checks and `./scripts/lint.sh` passed (exit 0, `RESULT: PASS`).

## Remaining validation and known limits

- Different-account and different-network joins need another authorized
  environment. Use the [Host/Join steps](../extensions/vscode-editchain/README.md#multiplayer-history),
  verify both fingerprints, type on both devices, inspect the received diffs,
  interrupt one connection, and confirm catch-up and Stop cleanup.
- Repository discovery has contract tests against controlled HTTP responses.
  A live check should publish and withdraw advertisements in an authorized test
  repository and verify that stale or unknown devices gain no access. No live
  repository variable was created in this implementation session.
- The packaged UI run uses Linux x64, VS Code 1.132.0, one machine and one CLI
  account through a test-only provider. Other platform packages and the built-in
  sign-in flow across different accounts still need their own observations.
- A hard process exit can take about 90 seconds to recover through the current
  peer liveness deadline. The final run measured 1,075 ms for WebDriver
  restart, 3,505 ms for extension readiness and 89,502 ms for peer reconnect.
  The replacement process reclaimed the tunnel lease immediately; the long wait
  was in peer recovery. Normal closed-socket recovery is exercised separately.
- Exact inventories and scoped metadata scan retained history. Very large
  history performance remains unmeasured; no latency or large-team scalability
  target is claimed. Each connection bounds frames and transfers, and each
  workspace limits active streams to eight and approved devices to 32.

## Native peer interface

`editchain-peer` reads local JSON frames with a four-byte little-endian length
bounded to 512 KiB. Commands are `identity`, `verify`, `scope`, `configure`, `approve`, `revoke`,
`devices`, `open`, `turn`, and `close`. One worker owns one peer connection.
Network input is base64 inside `turn`, decoded with a 64 KiB bound, and passed
only to TLS. A turn returns up to 256 KiB of opaque output plus public identity
and durable progress. Errors expose fixed codes, not payloads or credentials.
Use a new connection after a failed turn; durable inventories repair replay.

The implementation uses the ordinary certificate verifiers and byte-stream
APIs in [Rustls](https://docs.rs/rustls/latest/rustls/client/struct.ClientConnection.html),
including its [mandatory client certificate verifier](https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html).
It does not implement a custom signature or certificate-verification algorithm.
