# EditChain multiplayer: GitHub discovery and VS Code peer streaming

**Research and architecture notes — September 10, 2026**  
**Status:** Proposed architecture; not an end-to-end tested implementation.  
**Scope:** Consolidation of the research and design decisions from this conversation.

> **Recommended starting point:** Reuse VS Code's authorized GitHub sign-in for a small repository-backed discovery directory. Bundle Microsoft's Dev Tunnels SDK in the extension to establish an EditChain-only duplex connection. Authenticate peers separately, then replicate immutable EditChain operations and referenced blobs through the Rust synchronization engine.
>
> This gives independent, local-first replicas and no EditChain backend to operate. The initial networking adapter is Microsoft-relayed, not direct-only networking.

## Contents

1. [Goals and terminology](#1-goals-and-terminology)
2. [Recommended architecture](#2-recommended-architecture)
3. [What VS Code supplies—and what the extension supplies](#3-what-vs-code-suppliesand-what-the-extension-supplies)
4. [Authentication and authorization](#4-authentication-and-authorization)
5. [Lightweight workspace advertisement](#5-lightweight-workspace-advertisement)
6. [Dev Tunnels streaming integration](#6-dev-tunnels-streaming-integration)
7. [Peer trust and confidentiality](#7-peer-trust-and-confidentiality)
8. [EditChain replication semantics](#8-editchain-replication-semantics)
9. [Presence, topology, and recovery](#9-presence-topology-and-recovery)
10. [Alternatives and limits](#10-alternatives-and-limits)
11. [Implementation sequence and acceptance tests](#11-implementation-sequence-and-acceptance-tests)
12. [Open questions before shipping](#12-open-questions-before-shipping)
13. [Final design decisions](#13-final-design-decisions)
14. [Primary references](#14-primary-references)

---

## 1. Goals and terminology

### Product requirements

The intended multiplayer experience is:

- Different users run their own EditChain installations and maintain independent working trees.
- Users do **not** need shell access, Remote SSH access, terminal sharing, or access to someone else's VS Code session.
- Only the explicitly shared workspace's EditChain history is replicated: operations and permitted referenced content.
- In-progress sessions can stream before they finish. Offline work and incomplete replicas are normal.
- GitHub or Microsoft account sign-in should reuse platform capabilities rather than require an EditChain authentication server.
- Discovery should be lightweight and replaceable. A waiting room or presence update should not create a Git commit.
- No custom cloud coordinator, history database, relay deployment, or continuously running Actions job is required for the proposed first version.

### Three distinctions that matter

| Term | Meaning in this design |
|---|---|
| **Peer-oriented replication** | Every participant owns a local replica. No participant is the authoritative history server. Any authorized replica can supply history it holds. |
| **Direct peer networking** | Bytes travel directly between participant machines rather than through a relay. The proposed Dev Tunnels adapter does not provide this property. |
| **No backend to operate** | EditChain does not deploy its own cloud service. GitHub and Microsoft still operate services on which discovery and connectivity depend. |

“Stateless” should mean **reconstructible connection/session state**, not the absence of all stored state. History and device keys persist locally; advertisements persist in GitHub until removed; tunnel resources and active relay connections exist in Microsoft's service. Dev Tunnels is documented as a relay-backed connectivity service.[^tunnels-overview]

### Evidence boundary

The conversation reviewed public documentation, SDK source, and selected EditChain files. It did **not** execute the complete VS Code-token → tunnel creation → different-account connection → replicated-history workflow.

This document packages those findings; it is not a fresh source audit. SDK links often point to moving branches, and no dependency versions or source revisions are pinned here. Recheck APIs, quotas, policies, and service support before implementation or release. Proposed defaults and protocol additions below are design recommendations, not existing platform features.

## 2. Recommended architecture

```text
                         GitHub repository variables
                     Endpoint advertisements / enrollment
                               ^             ^
                               |             |
                     Platform API calls with consent
                               |             |
  Alice's machine              |             |              Bob's machine
  +----------------------------+--+       +--+----------------------------+
  | VS Code extension host        |       | VS Code extension host        |
  | - GitHub authentication       |       | - GitHub authentication       |
  | - Directory refresh           |       | - Directory refresh           |
  | - Bundled Dev Tunnels SDK     |       | - Bundled Dev Tunnels SDK     |
  | - Bounded stream adapter      |       | - Bounded stream adapter      |
  +---------------+---------------+       +---------------+---------------+
                  |                                       |
                  +---------- Microsoft relay ------------+
                  |         transport connection          |
  +---------------+---------------+       +---------------+---------------+
  | Rust EditChain service        |       | Rust EditChain service        |
  | - Peer authentication         |       | - Peer authentication         |
  | - Workspace authorization     |       | - Workspace authorization     |
  | - Inventory / reconciliation  |       | - Inventory / reconciliation  |
  | - Durable ops and blobs       |       | - Durable ops and blobs       |
  | - Local projections           |       | - Local projections           |
  +-------------------------------+       +-------------------------------+
```

The diagram separates responsibilities, not literal socket placement. The extension's SDK adapter bridges the network stream into a **dedicated replication interface** in Rust.

### Responsibility split

| Component | Owns |
|---|---|
| VS Code authentication API | Authorized account sessions, consent, and session lifecycle. |
| GitHub discovery adapter | Advertisement CRUD, caching, expiry interpretation, repository selection, and retry/backoff. |
| Dev Tunnels adapter | Tunnel management, permitted host/client connections, and SDK stream lifecycle. |
| Rust peer layer | Cryptographic peer authentication, workspace policy, framing, limits, and protocol negotiation. |
| Rust synchronization engine | Missing-operation detection, blob transfer, durable acknowledgments, deduplication, and reconciliation. |
| History UI | Local projections, connection status, and explicit incomplete-history indicators. |

**Never attach a remote stream directly to the existing general-purpose service RPC endpoint.** A remote peer should be able to request authorized history—not invoke arbitrary service commands.

## 3. What VS Code supplies—and what the extension supplies

### The SDK is not a special extension permission

VS Code uses Dev Tunnels for its own port-forwarding features. That does not imply a stable public `vscode.devTunnels.createPeerSession()` API. The proposed extension uses Microsoft's public SDK directly instead of depending on VS Code's internal implementation.[^vscode-ports]

The TypeScript packages identified in Microsoft's sample are:[^sdk-host-package]

```bash
npm install \
  @microsoft/dev-tunnels-management \
  @microsoft/dev-tunnels-connections \
  @microsoft/dev-tunnels-contracts
```

Pin mutually compatible versions during implementation. These commands identify packages; they are not a tested lockfile.

| Package | Intended role |
|---|---|
| `@microsoft/dev-tunnels-management` | Create, retrieve, configure, and delete tunnel resources. |
| `@microsoft/dev-tunnels-connections` | Host/client connectivity and stream handling. |
| `@microsoft/dev-tunnels-contracts` | Shared service contracts and types. |

The SDK sample is a standalone Node.js application. With this approach, users should need **the EditChain extension**, not separate installations of the Dev Tunnels CLI, Live Share, or Remote – Tunnels. The extension must package its runtime dependencies correctly.[^sdk-host][^extension-bundling]

### Will VS Code block SDK calls?

Normally, **no**. Desktop Node.js extensions are not subject to a special Dev Tunnels capability gate. VS Code documents that extensions can access the network, files, and processes with the permissions of the VS Code process.[^extension-security]

The integration belongs in the **Node.js extension host**, not the history webview. A browser-only extension host is a different runtime; this Node SDK and native Rust design must not be assumed to work unchanged in `vscode.dev`.[^extension-host]

For SSH, WSL, containers, and Codespaces, choose the extension placement deliberately: networking occurs from the extension host running the adapter. For the first implementation, colocating the adapter with the Rust workspace service avoids an additional cross-host bridge.

### Actual blockers to account for

| Blocker | Effect |
|---|---|
| User declines authentication consent | No authorized session for the extension. |
| Insufficient repository permissions or OAuth scope | GitHub rejects directory operations. |
| Organization OAuth/SSO restrictions | Account or repository access may require organization approval. |
| Tunnel service authorization or policy | Creation, hosting, or connection fails. |
| Corporate proxy/firewall restrictions | Service endpoints or long-lived connections may be unreachable. |
| Enterprise extension allowlist | EditChain may be blocked or disabled entirely. |
| Workspace trust / unsupported runtime | The extension should not activate sensitive functionality in an inappropriate environment. |

These are runtime, service, network, and organizational controls—not a blanket prohibition on using the SDK. Respect access restrictions; do not treat anonymous tunnel access as a workaround for an administrator's prohibition.[^vscode-api][^oauth-restrictions][^tunnels-security][^enterprise-extensions]

### APIs and features that are not substitutes

- **`vscode.env.asExternalUri()`** is not a universal peer-publishing API. Its behavior depends on local versus remote extension execution.[^vscode-api]
- **The Ports UI** exposes a user-facing port-forwarding workflow, not automatic EditChain membership or trust.[^vscode-ports]
- **Remote – Tunnels** provides remote development access. That is a separate product experience and broader than the required history channel.[^remote-tunnels]

## 4. Authentication and authorization

### Reuse sign-in with consent

VS Code exposes built-in `github` and `microsoft` authentication providers. Being signed in does not automatically authorize EditChain to access an account. The extension requests a session and the necessary scopes; VS Code can prompt for consent or additional authorization.[^vscode-api]

The GitHub sign-in portion can start from this illustrative command handler:

```ts
import * as vscode from "vscode";

export async function authorizeGitHubDiscovery(): Promise<
  vscode.AuthenticationSession | undefined
> {
  try {
    return await vscode.authentication.getSession(
      "github",
      ["repo"],
      { createIfNone: true }
    );
  } catch {
    // No automatic fallback to a less secure discovery mode.
    await vscode.window.showWarningMessage(
      "GitHub authorization was not completed. Discovery remains disabled."
    );
    return undefined;
  }
}
```

Later automatic reconnects should use a silent session lookup and pause discovery if authorization is unavailable, rather than repeatedly prompting on startup. Treat session changes and revocation as part of the connection lifecycle.[^vscode-api]

### Repository variables require real repository authorization

GitHub's variable endpoints document `repo` scope for OAuth/classic token access, and separate `Variables` permissions for supported fine-grained credentials. The actor also needs the appropriate repository access.[^github-variables-api]

**`repo` is broad.** It is not an EditChain-only or variable-prefix-only permission. There is no documented `variables:write` OAuth scope that can simply be passed to VS Code's GitHub provider. Fine-grained tokens and GitHub Apps provide different permission models, but require a different authorization design.[^oauth-scopes][^github-variables-api]

Therefore:

- Enable sharing explicitly per workspace/repository.
- Explain the broad permission request and what metadata EditChain writes.
- Keep user tokens in the trusted extension-host authentication path, never the webview, advertisements, peer messages, or logs.
- Do not assume a read-only contributor to a public repository can advertise through repository variables.

### Tunnel-owner identity is separate from peer authorization

The SDK management interface accepts a user-token callback and documents GitHub and Microsoft/AAD authentication support. The source reviewed in the conversation includes a GitHub authorization scheme with the shape:[^sdk-management]

```ts
// Conceptual callback result for the trusted tunnel management API.
// Obtain the authorized session through VS Code and handle refresh/revocation.
async () => `github ${githubSession.accessToken}`
```

**Unverified integration point:** A current VS Code-provided token must actually be accepted for the required management operations in the intended account/organization environment. The presence of this SDK hook does not establish that the complete flow has been tested.

Microsoft sign-in can be relevant to tunnel management, but it does **not** grant GitHub repository-variable access. The current discovery design still needs GitHub authorization. A Microsoft-only user experience would need a different directory or invitation-based joining.

### Three identities to keep separate

```text
Account identity       GitHub / Microsoft account used with platform APIs
Device identity        EditChain installation's cryptographic key
Recorded actor         Original human, agent, or source represented in history
```

The peer transmitting an operation is not necessarily the operation's author. Transport authentication alone does not cryptographically prove the authorship of imported historical events.

## 5. Lightweight workspace advertisement

### Use mutable metadata, not Git commits

The initial commit-backed directory proposal was superseded. GitHub Actions repository variables expose ordinary REST CRUD endpoints; calling them does not require a workflow, runner, branch, or commit.[^github-variables-api]

```http
POST   /repos/{owner}/{repo}/actions/variables
GET    /repos/{owner}/{repo}/actions/variables
PATCH  /repos/{owner}/{repo}/actions/variables/{name}
DELETE /repos/{owner}/{repo}/actions/variables/{name}
```

This is a **proposed repurposing of configuration storage**, not a native GitHub presence feature or an endorsement of using it as a high-frequency message bus.

### One advertisement per active instance

Proposed variable name:

```text
EDITCHAIN_AD_<WORKSPACE_HASH>_<INSTANCE_ID>
```

Proposed value, serialized as a JSON string:

```json
{
  "v": 1,
  "workspaceId": "github:<repository-id>:<collaboration-space>",
  "peerId": "<device-public-key-fingerprint>",
  "instanceId": "<random-process-instance-id>",
  "publicKey": "<public-device-key>",
  "transport": "dev-tunnel",
  "endpoint": {
    "tunnelId": "<opaque-tunnel-id>",
    "clusterId": "<opaque-cluster-id>",
    "port": 4711
  },
  "protocolVersions": [1],
  "expiresAt": "<RFC-3339-UTC-expiration>"
}
```

Placeholders make this a schema example, not a usable credential or live endpoint. `4711` is an arbitrary logical service-port example, not a reserved EditChain port or required listening socket.

Use a stable collaboration-space identity. Do not infer identity from the local folder path, branch name, or the name `origin`. Forks and multiple histories in one repository should join a collaboration space explicitly. Existing local chain IDs may need a mapping to that space; do not rewrite original operation identities to create the mapping.

Separate records avoid a shared roster's lost-update problem. They are **not** separate permission domains: a variable-name prefix does not protect a device's record from another authorized directory writer.

### Advertisement lifecycle

```text
Enable sharing
  -> Start endpoint
  -> Publish instance advertisement

Discover
  -> List variables, follow pagination, filter the namespace
  -> Validate schema, protocol, workspace, and expiry
  -> Attempt authenticated connections

Stay active
  -> Renew advertisement occasionally
  -> Send real heartbeats over peer connections

Close normally
  -> Delete own advertisement and clean up tunnel resources

Crash
  -> Readers ignore expired advertisements
  -> Later cleanup removes abandoned records
```

**Suggested prototype defaults:** a 30-minute advertisement lease renewed every 10 minutes. These are intentionally coarse starting values, not a platform requirement or a liveness guarantee. Earlier brainstorming considered shorter leases; the final recommendation is to keep presence traffic off GitHub.

Expiry is application-enforced. Repository variables do not expose the proposed automatic TTL behavior. A stale record can remain stored even after EditChain stops using it. Repository-variable quotas and existing configuration also limit how many instances can be advertised; the prior discussion cited a 500-variable repository limit, which should be rechecked before release.[^github-variables-api][^variables-limits]

Use conservative polling, manual refresh, caching/conditional requests where supported, and rate-limit backoff. Do not assume conditional requests make all traffic free or remove secondary limits. GitHub documents efficiency and throttling guidance.[^github-api-practices]

### What an advertisement means

> “Here is a candidate endpoint to contact.”

It does **not** mean:

- the peer is online;
- the endpoint is trusted merely because it parsed;
- the listed public key belongs to the claimed GitHub user;
- all history behind the endpoint is authorized for this connection.

Validate peer identity and workspace policy after connection and before exposing an inventory.

### A shared locator is not a shared credential

Never put these in a non-secret variable:

```text
GitHub access token
Microsoft access token
Tunnel management or host token
Unencrypted connect-only bearer token
Device private key
Shared history-encryption key
```

A session ID, tunnel locator, or public key can be advertisement metadata. A bearer credential is not merely a “join key.” GitHub describes variables as non-sensitive configuration, with values unmasked if written into workflow output.[^variables-security]

## 6. Dev Tunnels streaming integration

### A stream, not a shell

The SDK exposes a client method `connectToForwardedPort()` returning a Node.js `Duplex` stream. The host source exposes `forwardConnectionsToLocalPorts` and a `forwardedPortConnecting` event for handling forwarded connections.[^sdk-client][^sdk-relay-host]

Relevant controls reviewed in the conversation:

```ts
// Host: do not forward incoming connections into arbitrary local TCP services.
host.forwardConnectionsToLocalPorts = false;

// Client: do not create local listeners for forwarded remote ports.
client.acceptLocalConnectionsForForwardedPorts = false;
```

Illustrative client connection core:

```ts
// Connection sketch, not a complete runnable tunnel setup.
// remoteTunnel must include the service-required connection information.
const client = new TunnelRelayTunnelClient();
client.acceptLocalConnectionsForForwardedPorts = false;

await client.connect(remoteTunnel);
await client.waitForForwardedPort(EDITCHAIN_PORT);
const stream = await client.connectToForwardedPort(EDITCHAIN_PORT);

// Bridge only to the dedicated peer-authentication / replication endpoint.
// Add cancellation, backpressure, deadlines, error handling, and disposal.
```

Verify constructor arguments and event/stream handling against pinned package versions. In particular, turning off local forwarding is only a configuration step: the host must correctly consume the forwarded connection through the supported event semantics.

No shell protocol is attached. No arbitrary filesystem API is attached. A remote blob request names an authorized content hash, not an arbitrary host path.

### Different user accounts: two access-control layers

By default, tunnel access is restricted. Dev Tunnels documents anonymous connection access and tunnel-scoped access tokens; its host sample demonstrates an anonymous access-control entry restricted to `Connect`.[^tunnels-security][^sdk-host]

Two proposed integration modes:

| Mode | Transport admission | EditChain admission |
|---|---|---|
| **Small trusted-team prototype** | Permit anonymous **connect only** to the endpoint. No anonymous hosting or management. | Require device authentication and workspace authorization before sending any history or inventory. |
| **Stricter transport admission** | Issue a connect-only token and deliver it encrypted to the approved device. | Still require device authentication and workspace authorization. |

Anonymous transport removes the need for collaborators to share a tunnel-owner account. It also means anyone who learns the endpoint can attempt a connection. Apply connection caps, short handshake deadlines, bounded unauthenticated reads, and abuse-resistant failure behavior. Validate anonymous-connect availability and organizational policy instead of assuming it is universally enabled.

A token-protected transport needs a secure grant-exchange mechanism. That is additional complexity, not something that appears automatically from a GitHub login. Never make public/non-secret advertisements carry plaintext bearer grants.

### One tunnel per active peer instance

The relay-host implementation reviewed in the discussion supports a single active host connection for a tunnel; another host can displace it. Avoid sharing host credentials or treating one tunnel as a platform-managed multi-host room.[^sdk-relay-host]

For the initial design:

```text
Each active instance owns its endpoint.
Each instance can initiate connections to other instances.
One established duplex connection exchanges data in both directions.
```

A transport “host” is a connection role, not the owner of the canonical history.

### Resource lifecycle

Explicitly handle tunnel creation, required port/service configuration, host startup, connection-token refresh where applicable, disconnect, deletion, and abandoned-resource cleanup. Do not assume an SDK-created resource automatically inherits the CLI's temporary-tunnel behavior. The CLI documents temporary tunnels; the exact SDK lifecycle must be implemented and verified.[^tunnels-cli]

## 7. Peer trust and confidentiality

### Minimal trusted-team policy

The smallest proposed trust policy is:

> Device keys enrolled through the repository's EditChain directory are permitted to participate in that collaboration space.

This deliberately trusts everyone who can modify that directory to enroll or replace keys. It does **not** provide protected per-user namespaces or verified GitHub-user-to-device attribution. The repository-variable permission model is repository-level.[^github-variables-api]

A stricter product can add separately approved device enrollment, signed membership records, or administrator-controlled membership. That need not change the underlying replication protocol, but it is more than endpoint advertising.

### Authenticated connection sequence

```text
Open transport
  -> Run an established authenticated encryption handshake
  -> Verify remote key against the selected trust policy
  -> Bind workspace, protocol version, and roles to the session
  -> Negotiate sharing scope and resource limits
  -> Exchange inventories
  -> Replicate permitted history
```

Use a reviewed protocol/library rather than inventing a signature exchange or encryption framing. Noise was identified as one possible protocol family; selecting a pattern, key lifecycle, and implementation requires explicit design and testing.[^noise]

### Encryption caveat

Dev Tunnels' web-forwarding documentation describes TLS termination at service ingress. That statement concerns the documented web-forwarding path; it should **not** be generalized into an unverified claim about every raw SDK stream's encryption properties.[^tunnels-security]

For a transport-independent confidentiality guarantee, use a reviewed application-layer authenticated encrypted channel, or verify and document equivalent end-to-end guarantees of the selected SDK mode. Do not claim “`wss://` means the relay cannot read it.”

Application-layer encryption also does not hide all metadata, such as connection timing or endpoint/account relationships.

### Data and execution boundaries

The dedicated replication endpoint should accept only explicitly supported synchronization operations. Reject unknown methods, malformed frames, unexpected protocol versions, unauthorized workspaces, oversized batches, excessive dependency requests, and content-hash mismatches.

Treat remote history as untrusted data. In particular:

- Never execute received historical commands or tool calls.
- Never interpret imported transcript text as instructions to the host or agent.
- Never expose arbitrary file reads, directory traversal, terminals, or command execution through blob retrieval.
- Verify that requested blobs belong to the authorized sharing scope; knowing a hash is not authorization.
- Avoid logging tokens, raw history, or sensitive request payloads.

Authorization freshness and revocation need explicit policy. An expired endpoint advertisement is not necessarily a revoked device; likewise, a stale cached directory must not silently authorize a new unknown device. Define how long existing authenticated sessions may continue if GitHub becomes unavailable.

## 8. EditChain replication semantics

### Replicate logical deltas, not directory patches

The intended delta is:

> Complete immutable operations the receiver does not possess, plus permitted missing content-addressed blobs referenced by those operations.

Do **not** mirror the physical `.editchain` directory. Import cursors, locks, local indexes, physical segment offsets, and transient files belong to local storage management.

The EditChain README examined during the conversation describes persisted content-addressed blobs, deterministic replay, duplicate canonicalization through `OpSet`, and quarantine of same-ID conflicts. It also explicitly distinguishes at-least-once physical append behavior from atomic log/cursor transactions.[^editchain-readme]

These are useful foundations, not proof that multiplayer is already implemented or fully correct.

### Existing protocol skeleton

The reviewed `editchain-sync/src/msg.rs` defines:[^editchain-sync]

```text
Hello { node, protocol, frontier }
Have  { frontier }
Need  { ranges }
Ops   { ops }
Ack   { frontier }
Error { code }
```

Transport is explicitly out of scope in that file. `Need` and `Ops` use generic payloads; their exact wire formats, validation rules, batching, and recovery contracts still need implementation decisions.

Proposed additions include workspace/scope negotiation, gap-aware inventories, blob transfer, protocol compatibility, bounded requests, and distinct operation-versus-content completeness reporting. These are proposed protocol capabilities, not verified current APIs.

### “Partial history is okay” needs a contract

| Situation | Required behavior |
|---|---|
| Agent session is still active | Replicate complete persisted operations immediately. |
| Independent sessions have not arrived | Show available history and reconcile later. |
| An operation's causal parent is missing | Track the dependency and defer dependent materialization where required. |
| A referenced blob is missing | Track hydration separately; fetch only when authorized. |
| Only half a network frame has arrived | Buffer; do not ingest a partial encoded operation. |
| The same operation arrives again | Deduplicate without changing its original identity. |
| The same ID arrives with different content | Quarantine or reject according to deterministic integrity rules. |

The goal is convergence for the same permitted operation set under compatible reducer semantics. A CRDT does not eliminate missing-message detection, dependency handling, schema compatibility, or authorization.

### Frontier warning: maximum sequence is not complete possession

The earlier code discussion reported a frontier containing `(node, boot, max_seq)`, with `max_seq` described as the maximum sequence seen. Reconfirm that representation in the current checkout; this note does not include a pinned source revision for it.

The correctness issue is independent of the implementation:

```text
Alice has: 1, 2, 3, 4, 5
Bob has:   1, 2,       5

Both have seen a maximum of 5.
Bob is not fully synchronized.
```

Use received ranges, a contiguous prefix plus additional ranges, or another inventory that can express gaps. For example:

```text
contiguousThrough = 2
additionalReceived = [5]
```

Selective sharing makes this more important: intentionally excluded operations must not be confused with accidentally missing operations. Define inventories over the agreed sharing scope and handle dependencies without leaking excluded content.

### Durable synchronization lifecycle

```text
Authenticate and authorize
  -> Negotiate protocol, scope, and resource limits
  -> Compare gap-aware inventories
  -> Request missing operations
  -> Validate and durably persist batches
  -> Fetch authorized missing blobs
  -> Acknowledge durable progress
  -> Stream newly persisted operations
  -> Reconcile on reconnect and when needed
```

A live stream without reconciliation can lose data during disconnects. Acknowledgments should reflect durable state, not merely an in-memory receive buffer. Distinguish:

```text
Operations durably received
Required blobs available
Projection materialized
Scope fully reconciled with this peer
```

These are not automatically the same milestone.

### Preserve provenance and avoid replication loops

Receiving Bob's copy of Alice's operation must preserve the original operation ID and recorded actor. Do not re-import it as a fresh Bob-authored event.

Track canonical ingestion so forwarding does not cause unbounded echo or repeated physical append. A peer should be able to forward previously received, authorized operations and blobs—not only events it originally produced.

Cryptographic transport identity authenticates the current sender, not the original historical author. Strong signed provenance would require additional event-level evidence and key binding.

### Sharing history is not editing another working tree

A received file operation records work that happened. It must not automatically apply a patch, change a checkout, run a tool, or execute a command. Applying historical changes locally should remain a distinct, explicit feature.

### Privacy scope

“Only `.editchain`” does not mean “only harmless metadata.” The conversation identified prompts, messages, tool arguments/results, command output, source patches, raw imported records, and content references as potential history content. Confirm the exact schema and exporters in the current checkout before choosing a sharing policy.

The opt-in should state whether it shares existing history, future history, or both. It should apply only to the selected collaboration space, not every locally imported session. Turning sharing off stops future access; it cannot retract copies already received by other peers.

## 9. Presence, topology, and recovery

### Keep presence in memory

GitHub advertises candidate endpoints. Authenticated peer connections supply the live roster and liveness signals.

```text
Directory record: “try this endpoint”
Handshake success: “this is an authorized peer”
Heartbeat:         “this connection is currently responsive”
Reconciliation:    “this replica has these operations and blobs”
```

Do not store ordinary heartbeats, cursor/viewing state, or transient status changes in canonical history unless the product explicitly needs them as retained events.

### Small-team topology

Start with one bidirectional connection per pair of active instances. A deterministic ordering of instance IDs can select the initiator and avoid duplicate connections. Validate and resolve duplicate simultaneous connections anyway.

A full mesh is simple for a small group but has quadratic connection growth. Treat it as an MVP topology, not the final answer for large teams. The protocol should allow later sparse connectivity and forwarding without changing history semantics.

### No permanent session owner

```text
Alice synchronizes with Bob.
Alice disconnects.
Carol connects to Bob.
Bob supplies Alice's already-received, authorized history.
```

If history exists only on Alice's offline device, it remains unavailable until a holder returns. No transport or CRDT can retrieve bytes from an unreachable device without an available copy elsewhere.

### Recovery requirements

- Restarting a peer can publish a new endpoint without changing the collaboration space.
- Reconnecting peers reconstruct progress from durable local history, not only a remembered stream offset.
- Expired advertisements are ignored and later pruned.
- Authentication cancellation or revoked access leaves local history usable.
- Bounded queues prevent slow peers from exhausting memory or blocking local ingestion indefinitely.
- Loss of GitHub discovery need not automatically stop already-authenticated connections; behavior depends on the explicit authorization-freshness policy.
- Tunnel/network failures should report useful, non-sensitive reasons and retry with backoff.

## 10. Alternatives and limits

| Approach | Good fit | Limitation |
|---|---|---|
| **Repository variables + Dev Tunnels** | Small-team automatic discovery without custom backend deployment. | Broad GitHub permissions, configuration-store quotas, relayed transport, platform support questions. |
| **Invitation + Dev Tunnels** | Minimal explicit joining flow. | New participants need a reachable peer's invitation; no automatic repository-wide discovery. |
| **Dev Tunnels labels** | Discovering tunnels belonging to the same account. | SDK listing is documented in terms of tunnels owned by the caller; do not assume cross-account repository discovery. |
| **Live Share extension API** | Custom messages during an explicit host/guest collaboration session. | Does not itself provide independent persistent EditChain peer discovery. |
| **GitHub-only history batches** | Asynchronous store-and-forward when peers are never online together. | GitHub becomes a durable history store, not merely discovery; not direct networking. |
| **WebRTC DataChannels** | Direct transport when network conditions allow it. | Requires signaling and connectivity infrastructure; reliable arbitrary-network operation may need relaying. |

The tunnel directory ownership contract, Live Share custom service API, and WebRTC connectivity model were reviewed in the cited primary sources.[^sdk-management][^liveshare-counter][^webrtc]

### Why not GitHub Events for presence?

GitHub documents the Events API as unsuitable for real-time use, with possible delays of 30 seconds to six hours. Do not use it as a live workspace-presence stream.[^github-events]

### Literal direct P2P

WebRTC DataChannels can carry arbitrary application data. Signaling is separate; ICE uses connectivity candidates and commonly relies on STUN/TURN infrastructure. Sign-in is not a NAT-traversal solution.[^webrtc]

GitHub variables could carry low-volume signaling as an experiment, but they do not supply STUN/TURN. A Dev Tunnels endpoint is not a TURN server; it would be a separate fallback transport for the same EditChain protocol.

The research did not establish a documented public GitHub/VS Code API that combines direct P2P, general STUN/TURN access, and repository-member authentication for arbitrary extensions. Do not infer access to internal Live Share or VS Code infrastructure from their first-party capabilities.

### Platform support caveat

At the time discussed, Microsoft's public Dev Tunnels documentation described the service as **public preview**, with **no SLA**, and **not recommended for production workloads**. Its use inside VS Code does not automatically establish production support for a third-party extension's usage.[^tunnels-overview]

Keep the transport replaceable and validate intended usage, lifecycle, limits, availability, and enterprise compatibility before shipping. Review current SDK licenses and package notices separately; service access and library licensing are different questions.

## 11. Implementation sequence and acceptance tests

### Phase 1 — Prove the cross-account stream before adding history

Build a minimal extension experiment that:

1. Obtains an authorized GitHub session through VS Code.
2. Creates and hosts its own tunnel through the SDK.
3. Advertises a locator and device key, initially through an explicit invitation or a repository variable.
4. Connects from a different user's extension on a different network.
5. Authenticates approved device keys and exchanges bounded dummy messages.
6. Cleans up connections/resources on normal shutdown and handles restart.

**Pass criteria:** Different accounts exchange only the intended application bytes. Unknown devices receive no history inventory. Neither machine needs shell access to the other. No arbitrary local TCP service or general service RPC is exposed.

This validates the highest-risk platform assumption: the actual VS Code account-session → SDK management → cross-account connection path.

### Phase 2 — Two local replicas and durable reconciliation

Connect two Rust replicas through a simple test transport first. Define and test operation framing, blob transfer, gap-aware inventories, durability, duplicate handling, and resource bounds independently of Microsoft's SDK.

**Pass criteria:** The same permitted operation set and blobs converge after duplicate, delayed, reordered, and interrupted delivery. Existing original operation identities and actors are preserved.

### Phase 3 — Attach real streaming

Bridge the SDK stream into the dedicated Rust replication interface. Add encrypted peer authentication, scope negotiation, token/session lifecycle handling, and useful connection-state reporting.

**Pass criteria:** New local operations appear remotely without waiting for session completion; disconnect/reconnect repairs missed data without requiring a full reimport.

### Phase 4 — Add automatic discovery and three-peer behavior

Implement per-instance advertisements, leases, cleanup, deterministic dialing, live roster exchange, and authorized forwarding.

**Pass criteria:** When Alice disconnects, Bob and Carol continue. Carol can retrieve Alice's history already held by Bob. Expired endpoint records do not appear as online peers.

### Test matrix

| Area | Required test |
|---|---|
| Sign-in | Existing session, first consent, denial, revoked session, account switch. |
| GitHub access | Personal repository, organization repository, insufficient access, applicable OAuth/SSO policy. |
| Cross-account transport | Two different users, then users in different organizations; no shared host credentials. |
| Network | Different NATs, proxy/firewall failure, interrupted connection, laptop sleep/wake. |
| Extension environment | Desktop first; then explicitly supported WSL/SSH/container/Codespaces placements. |
| Peer authentication | Approved key, unknown key, replaced key, wrong workspace, replayed/expired advertisement. |
| Operation integrity | Duplicate delivery, same-ID conflict, wrong hash, missing parent, out-of-order arrival. |
| Inventory | `1,2,5` versus `1,2,3,4,5`; excluded scope versus missing content. |
| Durability | Crash before acknowledgment, crash after persistence, restart during blob transfer. |
| Security boundary | Unknown RPC, arbitrary path request, oversized frame, excessive requests, malicious transcript text. |
| Three-peer availability | Original author offline, relay of previously received history, duplicate connection resolution. |
| Cleanup | Normal exit, crash, stale variables, abandoned tunnels, authentication withdrawal. |
| Privacy | Existing-history opt-in, permitted blob closure, no credential/content leakage in logs. |

## 12. Open questions before shipping

| Question | Why it matters |
|---|---|
| Does the current VS Code GitHub session work with every required SDK management operation? | Source-level authentication support is not a completed integration test. |
| What is the exact supported Microsoft-account token/resource flow? | Microsoft sign-in is not interchangeable with a GitHub token or repository authorization. |
| Is the proposed third-party product usage supported by the tunnel service? | Public-preview documentation and first-party usage do not settle production suitability. |
| Which tunnel policies work for individual users and target organizations? | Anonymous-connect availability, approval requirements, and connect-token delivery affect onboarding. |
| What is the exact raw SDK stream confidentiality model? | Avoid equating web-forwarding TLS behavior with all SDK transport modes. |
| Who is allowed to enroll/revoke device keys? | Repository-variable writers are a deliberately broad trust authority. |
| How is a device key bound to an actual human account when attribution matters? | A signed handshake proves possession, not the truth of an arbitrary account label. |
| How do existing chain IDs map into a shared workspace? | Replication must preserve identity while supporting independent local stores. |
| Does the current frontier represent gaps and partial scope correctly? | Maximum-sequence equality alone is insufficient for reconciliation. |
| How are missing parents, intentionally omitted content, and blob permissions represented? | Partial synchronization must not deadlock or leak excluded history. |
| What happens to existing sessions when membership changes or the directory is unavailable? | Revocation and availability need an explicit freshness policy. |
| What finite tunnel, connection, API, and variable quotas apply? | The initial design is small-team oriented; limits must be checked and measured. |

## 13. Final design decisions

### Adopt for the prototype

- **GitHub variables are a replaceable low-frequency discovery adapter**, not a message bus or canonical membership history.
- **The extension bundles the public SDK** and runs it in the Node.js extension host.
- **Different user accounts are expected.** Platform connection admission and EditChain authorization are separate layers.
- **The channel exposes only replication.** No shell, filesystem browsing, arbitrary service RPC, or working-tree mutation is provided to peers.
- **Rust owns correctness.** Authenticate, validate, persist, reconcile, deduplicate, and authorize data there.
- **Share logical operations and blobs**, not physical directory patches or import cursors.
- **Preserve local-first independence.** Connections are disposable; no session creator owns the canonical history.
- **Keep the transport replaceable.** The initial implementation is relay-backed even though the application is peer-oriented.

### Do not assume

- Signed in means the extension is authorized.
- A session locator is a safe place to put a shared secret.
- A public key inside an advertisement proves a GitHub user's identity.
- A CRDT removes the need for missing-data reconciliation.
- A maximum sequence number proves complete possession.
- “Only EditChain” means the contents are non-sensitive.
- An offline device's unique history is available without another reachable copy.
- VS Code's use of Dev Tunnels grants a third-party extension a production service guarantee.

**Implementation priority:** First prove an authenticated, restricted duplex stream between two different accounts on different networks. Then connect the existing Rust protocol and prove reconnect-safe convergence. Automatic workspace discovery is a small adapter around that core—not the foundation of its correctness.

## 14. Primary references

The references below were collected during the conversation. They are links for verification and implementation, not a claim that every page was re-fetched while packaging this note. Moving source branches should be pinned during implementation.

[^vscode-api]: Visual Studio Code, **API reference**—authentication sessions, scopes, lifecycle, and `env.asExternalUri()`. https://code.visualstudio.com/api/references/vscode-api

[^extension-security]: Visual Studio Code, **Extension runtime security**. https://code.visualstudio.com/docs/configure/extensions/extension-runtime-security

[^extension-host]: Visual Studio Code, **Extension Host**—desktop, remote, and web runtimes. https://code.visualstudio.com/api/advanced-topics/extension-host

[^vscode-ports]: Visual Studio Code, **Port Forwarding**. https://code.visualstudio.com/docs/debugtest/port-forwarding

[^remote-tunnels]: Visual Studio Code, **Remote Development using Tunnels**. https://code.visualstudio.com/docs/remote/tunnels

[^extension-bundling]: Visual Studio Code, **Bundling Extensions**. https://code.visualstudio.com/api/working-with-extensions/bundling-extension

[^enterprise-extensions]: Visual Studio Code, **Enterprise extension management**. https://code.visualstudio.com/docs/enterprise/extensions

[^github-variables-api]: GitHub, **REST API endpoints for GitHub Actions variables**. https://docs.github.com/en/rest/actions/variables

[^variables-security]: GitHub, **Variables**—non-sensitive configuration and visibility. https://docs.github.com/en/actions/concepts/workflows-and-actions/variables

[^variables-limits]: GitHub, **Variables reference**—naming, precedence, and limits. https://docs.github.com/en/actions/reference/variables-reference

[^oauth-scopes]: GitHub, **Scopes for OAuth apps**. https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/scopes-for-oauth-apps

[^oauth-restrictions]: GitHub, **About OAuth app access restrictions**. https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions

[^github-api-practices]: GitHub, **Best practices for using the REST API**. https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api

[^github-events]: GitHub, **REST API endpoints for events**—real-time limitations. https://docs.github.com/en/rest/activity/events

[^tunnels-overview]: Microsoft Learn, **Dev tunnels overview**—service model and preview caveats. https://learn.microsoft.com/en-us/azure/developer/dev-tunnels/overview

[^tunnels-security]: Microsoft Learn, **Dev tunnels security**—authentication, access controls, and forwarding security. https://learn.microsoft.com/en-us/azure/developer/dev-tunnels/security

[^tunnels-cli]: Microsoft Learn, **Dev tunnels CLI commands**—temporary tunnels, access controls, and connection tokens. https://learn.microsoft.com/en-us/azure/developer/dev-tunnels/cli-commands

[^sdk-host-package]: Microsoft Dev Tunnels SDK, **TypeScript host sample package.json**. https://github.com/microsoft/dev-tunnels/blob/main/samples/ts/host/package.json

[^sdk-host]: Microsoft Dev Tunnels SDK, **TypeScript host sample**. https://github.com/microsoft/dev-tunnels/blob/main/samples/ts/host/index.ts

[^sdk-management]: Microsoft Dev Tunnels SDK, **TunnelManagementClient**—authentication callback and owned-tunnel listing contract. https://github.com/microsoft/dev-tunnels/blob/main/ts/src/management/tunnelManagementClient.ts

[^sdk-client]: Microsoft Dev Tunnels SDK, **TunnelClient**—forwarded-port connections and `Duplex` stream API. https://github.com/microsoft/dev-tunnels/blob/main/ts/src/connections/tunnelClient.ts

[^sdk-relay-host]: Microsoft Dev Tunnels SDK, **TunnelRelayTunnelHost**—forwarding controls and host connection behavior. https://github.com/microsoft/dev-tunnels/blob/main/ts/src/connections/tunnelRelayTunnelHost.ts

[^liveshare-counter]: Microsoft-linked Live Share extension sample, **Counter**—custom shared service requests and notifications. https://github.com/vsls-contrib/counter

[^noise]: **The Noise Protocol Framework**. https://noiseprotocol.org/noise.html

[^webrtc]: WebRTC, **Peer connections**—signaling and ICE/STUN/TURN concepts. https://webrtc.org/getting-started/peer-connections

[^editchain-readme]: EditChain, **README** referenced in the code review. https://github.com/idleai/editchain/blob/main/README.md

[^editchain-sync]: EditChain, **Synchronization message definitions** referenced in the code review. https://github.com/idleai/editchain/blob/main/crates/editchain-sync/src/msg.rs
