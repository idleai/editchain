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

Three follow-up corrections landed after checkpoint 5 and are recorded in the
validation log below: `3b84515` preserves durable local provenance for received
baselines, `8c6dfdf` makes Stop final across pending sharing operations, and
`0e0f1d6` makes interrupted relay cleanup retryable.

The final different-account/different-network observation requires a second
authorized account and machine and is deferred at the user's request. The
follow-up fixes were verified locally; the validation record identifies
earlier live relay runs separately from offline regression checks.

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
- Initial release-binary relay run: three replicas passed large-content transfer,
  exact historical diffs, restart catch-up, explicit reconnect, forwarding while
  the source was offline, and revocation. Setup was 1,806 ms; total 14,574 ms;
  19 content blobs were retained and both temporary tunnels were deleted.
- Initial packaged UI run: both VS Code cases passed in about 105 seconds of
  scenario execution. Actual device approval, typing, received History rows,
  exact native diffs, unchanged receiving files and recovery after a full host
  restart all passed. Tunnel cleanup completed; the log audit found no account
  token, connect grant or private invitation material. The VSIX contains both
  executable Linux x64 binaries and the runtime it was built with, and excludes
  the test provider and automation scripts. Local evidence:
  [run result](../extensions/vscode-editchain/trace/multiplayer/run.json),
  [host observations](../extensions/vscode-editchain/trace/multiplayer/host/observations.json),
  [diff after restart](../extensions/vscode-editchain/trace/multiplayer/host/received-after-reload-diff.png).
  That UI run used VSIX SHA-256
  `d57eaf860a37821c9927df8db85b849bf39eed8c85e5d43badc2cbd69fc32546`;
  a later rebuilt recovery package is verified in the recovery checkpoints
  below. The latest package has its own final verification record.
- Baseline provenance (`3b84515`): the replication ledger now records durable
  local provenance separately from received-byte receipts. An exact copy of a
  withheld local baseline that a peer supplies makes the record shareable but
  no longer makes the editor treat it as peer-authored, so a rebuilt capture
  cache keeps deriving from it instead of quarantining correct rows. Three new
  ledger tests in `crates/editchain-sync` cover mixed local and foreign provenance,
  blob access, retransmission and version-1 compatibility. A fourth test drives
  the real public recorder and `Replica`
  ingestion through a cold rebuild. `cargo clippy` on the affected crates and
  `cargo test --workspace --all-features` passed locally; the canonical root
  lint later passed independently.
- Stop finality (`8c6dfdf`): a Stop request issued while a start, host, join or
  cleanup operation is pending is no longer undone when that operation
  completes. The session stays disabled until the user hosts or joins again. Eleven
  lifecycle tests cover the pending-operation cases.
- Independent verification: the canonical `./scripts/lint.sh` passed all six
  gates in an isolated checkout, and 12 new tests passed under a zero-outbound
  guard: one real native-TLS provenance test and 11 lifecycle tests.

## Recovery review checkpoints

### Local capture

- A conflicting received variant no longer permanently blocks the editor
  checkpoint. The recorder rebuilds its derived state from canonical sources,
  retains conflicting derivations as evidence, and settles replay before
  admitting the next observation. Quarantined snapshots cannot supply future
  revisions.
- Exact local retries and sequence checks consult a unique retained local
  variant using received-byte receipts together with durable local provenance.
  A retry can also match complete retained source
  bytes when all variants have received receipts. Sequence checks require an
  undisputed actor/stream when a unique local admission is unavailable. Neither
  operation restores a variant to canonical history or acknowledges new
  conflicting retry content.
- Regression coverage includes a conflict at the recorder frontier, loss of a
  previously used snapshot, exact retries, and rebuilding the derived cache.
  `./scripts/lint.sh`: exit 0, `RESULT: PASS`.

### Received-baseline provenance

- The replication ledger records two independent facts per exact record key:
  whether a peer supplied it (`received`, with `received_blobs` for its content)
  and whether this device retained it locally before any peer supplied it
  (`local`). Record export still follows `excluded`; blob access still follows
  `received` and `received_blobs`. Capture also consults local provenance.
- When a peer independently supplies the exact bytes of a previously withheld
  local baseline, the record leaves the withheld set and becomes shareable, its
  preexisting blobs stay unexportable until their content is supplied through
  the space, and its local provenance is preserved. A rebuilt capture cache
  therefore still derives from it instead of quarantining rows that legitimately
  used it. Genuinely peer-authored sources with no local bytes remain skipped.
- Regression coverage: the ledger schema, mixed local and foreign provenance,
  retransmission, version-1 compatibility, and an end-to-end cold rebuild that
  drives the real public recorder and `Replica` ingestion. A real native-TLS
  provenance harness test and 11 lifecycle tests passed under a zero-outbound
  guard. `./scripts/lint.sh`: exit 0, `RESULT: PASS` (all six gates).

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
- Stop is final (`8c6dfdf`): a stop request issued while a start, host, join or
  cleanup operation is pending is no longer undone when that operation
  completes, and the session stays disabled until the user hosts or joins
  again. Eleven lifecycle tests cover the pending-operation cases and passed
  under the zero-outbound guard.

### Relay cleanup after interrupted startup

- Fixed in `0e0f1d6`. A disconnect callback may suspend the relay host before its
  initial connect rejects. Suspension and resource removal now have separate
  completion states, so the failed startup can still delete its tunnel.
- Stop owns and awaits the starting host through cleanup retries. Failed
  removals remain reachable for retry; successful resource deletion is not
  repeated when only management-client disposal needs retrying. A production
  management-client factory permits cleanup after suspension released the
  original client.
- Cleanup failures retain the ownership marker for **EditChain: Clean Up
  Multiplayer Tunnels**. Errors from the SDK remain sanitized. Successful
  suspension still preserves the tunnel for workspace reload.
- Sixteen regressions cover callback ordering, Stop during cleanup and deletion
  retry, failed disposal, retained ownership, suspension/reload, sanitized
  errors, and stopped status after a resumed startup fails. The management API
  and SDK host are controlled stubs; the manager, relay lifecycle, journal
  handling and native storage are real.

### Permission diagnostics

- Native peer authentication failures now carry a distinct internal error type.
  A local filesystem permission failure returns `storage_permission_denied`,
  while an unapproved/revoked device or TLS failure retains
  `authentication_failed`. Neither response exposes paths or credentials.
- Real worker probes with unreadable membership metadata and an unwritable
  policy directory returned the storage code. The production extension decoder
  preserved it. Existing TLS/pin/revocation tests, all 189 extension tests,
  multiplayer type checks and `./scripts/lint.sh` passed (exit 0, `RESULT: PASS`).

### Recovery validation before the provenance fix

- An additional regression covered a local baseline that a peer independently
  supplied before conflicting with it. Every new quarantine now invalidates
  cached recorder state, regardless of received receipts. Both receipt cases
  cover continued recording, exact retries, cold recovery and rejection of
  changed retry content or recorder identity.
- That checkpoint's `./scripts/lint.sh`: exit 0, `RESULT: PASS`; no quality
  policy, thresholds, exclusions or suppressions changed. The extension build,
  all 189 harness tests and multiplayer type checks passed.
- Extracted the rebuilt VSIX and ran 58 multiplayer/Dev Tunnels tests against
  its actual runtime and native binaries. All passed, with zero skips and zero
  socket/fetch/GitHub CLI attempts under the offline guard. The packaged service
  also accepted new capture and exact retries after a conflicting record was
  injected through the public replication API, including after service restart.
- Verified all 29 packaged runtime files and both executable binaries against
  the fresh build; test automation remains excluded. That checkpoint's
  [VSIX](../outputs/editchain-history-multiplayer-before-20260919.vsix) SHA-256:
  `26b761be08d0a3c565c3d56ca549532ef53d6a412495574076ea01882ac132ee`.
  [Package verification](../outputs/multiplayer-recovery-verification.json).
- These recovery checks used disposable local fixtures and controlled byte
  transports. They did not rerun the live relay or VS Code UI scenarios, create
  cloud tunnels, or access an account token. Different-account/network testing
  remains outstanding.

### Final verification — September 19, 2026

- All three fixes are included in the current package. Canonical
  `./scripts/lint.sh`: exit 0, `RESULT: PASS` across all six gates in an isolated
  checkout. No quality policies, thresholds, exclusions or suppressions changed;
  unrelated edits in the developer's checkout were preserved.
- TypeScript compilation, multiplayer type/syntax checks and all **217**
  extension harness tests passed, with zero failures or skips. Both release
  binaries were rebuilt.
- Extracted the new VSIX and ran **86** multiplayer/Dev Tunnels tests against
  its actual runtime and bundled native binaries. All passed, with zero skips
  and zero socket/fetch/GitHub CLI attempts under the offline guard. This
  includes native mutual-TLS history replication, the cold-cache provenance
  regression, command/manager lifecycle tests and the controlled SDK cleanup
  failures. The packaged service also passed capture, exact retries and restart
  after a conflict injected through the public replication API.
- All **29** packaged runtime files and both executable binaries match the
  fresh build; test automation is excluded. Current
  [VSIX](../outputs/editchain-history-multiplayer.vsix) SHA-256:
  `24155bca29486114f5463f718ca694abc12c2e09925e7abc14fd041873acd7d8`.
  [Verification report](../outputs/multiplayer-fixes-20260919-verification.json).
- These checks use disposable local fixtures. The earlier live relay and VS
  Code multiplayer UI runs were not repeated for this package. Two-account,
  different-network and live repository-discovery checks remain deferred.

## Migration and compatibility

- `multiplayer/scope.json` stays at version 1 and gains one optional array,
  `local`, holding the exact record keys this device retained before a peer
  independently supplied them. It is independent of `received` and
  `received_blobs`: those still mean "a peer supplied this" and still gate
  export of preexisting blob content, unchanged. Capture uses `local` when
  selecting sources for replay and exact retries.
- A version-1 ledger written before the field existed still loads; `local`
  defaults to empty. Keys the earlier code already moved to `received` are
  indistinguishable from genuinely peer-authored records, so their lost local
  provenance is **not** reconstructed and no authorship is inferred for them.
- There is no automatic unquarantine or repair of damage the earlier behavior
  already caused. Rows quarantined before the fix stay quarantined; re-deriving
  an already retained variant does not remove its conflicting evidence.
- Mixed builds are unsupported. An older replication worker rejects the added
  field and fails closed (`invalid scope metadata`) rather than continuing.
  Older capture code ignores the unknown field, so it does not fail — it keeps
  the previous behavior. Use the service and peer binaries from the same
  updated build on every device.

## Remaining validation and known limits

- Different-account and different-network joins need another authorized
  environment and are explicitly deferred at the user's request. When they are
  run, use the [Host/Join steps](../extensions/vscode-editchain/README.md#multiplayer-history),
  verify both fingerprints, type on both devices, inspect the received diffs,
  interrupt one connection, and confirm catch-up and Stop cleanup.
- Repository discovery has contract tests against controlled HTTP responses.
  A live check remains pending: publish and withdraw advertisements in an
  authorized test repository and verify that stale or unknown devices gain no
  access. No live repository variable was created in this implementation
  session.
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
