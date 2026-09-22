# Using EditChain multiplayer

Multiplayer shares recorded edit history between workspaces. Each participant
keeps their own working files and can open the other person's recorded diffs.

## Before you start

- Install the same multiplayer build on both devices. From this checkout, run
  `./reinstall-vscode.sh` in the target VS Code window's integrated terminal,
  then reload that window. This builds and bundles the history service and
  multiplayer peer worker together.
- To use the prebuilt test package on another device, choose **Extensions:
  Install from VSIX…** and select `outputs/editchain-history-multiplayer.vsix`
  (currently Linux x64).
- Open and trust a separate workspace or repository checkout on each device.
- Both participants sign in to their own GitHub account when prompted. Joining
  uses the host's invitation; the joining account supplies the name for local work.
- For two copies on one machine, launch VS Code with separate `--user-data-dir`
  directories so each copy has its own device identity.

Run the commands below from the Command Palette (`Ctrl+Shift+P` or `Cmd+Shift+P`).

## Connect two people

1. **Joining person:** run **EditChain: Create Multiplayer Join Request**.
   Send the copied request directly to the host.
2. **Host:** run **EditChain: Host Shared History / Invite Device** and paste
   that request. Choose **Share records added from now on**, or **Include
   existing history** to share past activity too. Approve the device identity
   shown in the prompt and sign in to GitHub.
3. **Host:** send the copied private invitation to the joining person. Use it
   within its expiry window (at most one hour); keep it out of public logs and
   issue trackers.
4. **Joining person:** run **EditChain: Join Shared History**, paste the
   invitation, choose which of your history to share, approve the host, and sign
   in to your own GitHub account.
5. **Both:** wait for **Sharing · 1/1 connected** in the status bar, then run
   **EditChain: Open History Explorer**. A **syncing** suffix means the connection
   is working and shared history is still being checked or transferred. Received
   rows appear as they are saved; you can keep editing during this check.

## Try it

Edit and save a text file on one device. On the other, find the new History row
and click its file change to open the recorded before/after diff. The receiving
device's working file stays as it was. Repeat in the opposite direction.
Keep History in live mode (**EditChain: Resume Live History** if it was paused).
Saved multiplayer receipts wake the open live view; a slow outgoing reply does
not delay that notification. Recorded diffs become available as their content
arrives, including when the view was opened before its content store existed.

For a Codex conversation, keep its source device's History view open in live
mode so the local session importer captures new activity. First confirm that a
new message or tool result appears locally, then find the same content on the
other device. **Sent (confirmed saved by peer)** and the other device's
**Received here** counts show durable delivery; opening the received row checks
the complete capture → transfer → display path. A completed percentage alone
does not prove that a particular conversation was captured or displayed.

An ongoing Codex session can be shared from a cutoff in its middle. The live
view shows the latest fully validated item revisions it has received and applies
received removals, even when earlier parts of the session remain private.
Temporary **Import** rows become normal conversation rows as their supporting
records arrive; that transition should not make the received session disappear.
Older session names and ancestry may be unavailable if their records precede
the cutoff. Updating this build and reopening History repairs previously hidden
received items from the local cache; enabling full-history backfill is unnecessary.

### Human-session names

New human activity uses session headers such as **ambientlight**,
using the GitHub account name available when the activity was recorded. The
recorded name travels with shared history, so received work keeps its original
name. Session IDs and device identities stay unchanged.

Reloading VS Code keeps the same human session and continues its graph lane
when the extension storage, workspace URI, and chain path stay the same. Existing
live caches automatically repair recorder-restart lane changes on first open.

Activity without a recorded name uses **VS Code**.
Names are display metadata, not verified GitHub authorship. Already received
history is not retroactively attributed to the current account. After updating
an existing sharing session, use **EditChain: Resume / Reconnect Shared History**
on both devices and complete sign-in if prompted; subsequent activity uses the
available account name.

## Reconnect or stop

- Connections retry automatically. Reopening an enabled workspace resumes
  sharing; a hard process exit can take about 90 seconds to recover.
- Updating the native worker paths restarts sharing with the saved session.
  Adding another workspace folder leaves the current session running. Changing
  the shared chain or removing its folder stops that session.
- Run **EditChain: Show Multiplayer Status** to inspect the connection, or
  **EditChain: Resume / Reconnect Shared History** to retry. An expired grant
  needs a fresh invitation from the host.
- Run **EditChain: Stop Sharing History** to disconnect, delete your hosted
  tunnel and disable automatic resume. Received history stays on each device.
  Stop is final: it also takes effect if a start, join or cleanup is still in
  progress. Use Host or Join to enable a new sharing session after Stop.
- Use **EditChain: Remove Shared Device** to revoke a device's access to your
  replica. Previously received copies remain with that participant.
- If cleanup failed, stop sharing first, then run
  **EditChain: Clean Up Multiplayer Tunnels** in the same workspace/account.
- If VS Code forgets the workspace settings, Host can recover the space from
  the chain on disk. If the saved invitation or device identity is also gone,
  exchange a fresh join request and invitation.

## Change which history you send

Run **EditChain: Change Shared History Scope** on the device whose outgoing
history you want to change:

- **Share records added from now on** establishes a **new cutoff**, including
  when you previously selected **Include existing history**. Records already
  present on this device stop being offered to peers; newly appended records
  remain eligible. Choosing it again moves the cutoff forward again.
- **Include existing history** removes the cutoff and makes the retained
  history eligible for backfill.

The cutoff applies to all approved peers of this workspace. Connections restart
to discard inventories prepared under the old scope. Approvals, invitations,
local history and copies already received by other devices remain intact.
Reconnecting or reloading preserves the cutoff, including work recorded offline
after it. New records use their local append order, not their recorded timestamp.
Stopping backfill mid-transfer can leave earlier received rows missing content;
selecting **Include existing history** later lets that content finish transferring.

When inviting or joining again, **Keep current sharing scope** preserves the
saved boundary. Selecting **Share records added from now on** explicitly creates
a new one. **Show Multiplayer Status** and the status-bar tooltip display the
effective outgoing scope and cutoff time. Older saved exclusion baselines are
identified as such until you choose a new scope.

Each device controls its own outgoing scope. To stop historical backfill in both
directions, select **Share records added from now on** on both devices. A new
record can still reference content needed to show its recorded before/after diff.
If a scope change was interrupted, repeat this command to complete it; the old
inventory is blocked until the policy is consistent again.

## Watch synchronization progress

The status bar counts authenticated **connections**, including peers that are
syncing. **Reconnecting** means the connection was lost; **waiting for content**
means some referenced revision data was unavailable at the last check. A connected
peer without either suffix has no active check or reported missing content.
With one connected peer, **↓25% ↑50%** shows receiving and sending checks.
Hover over the status bar to see totals and remaining checks in both directions.

Run **EditChain: Show Multiplayer Status** once. The **EditChain Multiplayer**
Output channel prints a JSON snapshot, then follows progress automatically.
Updates are timestamped and grouped to at most once per second, with a status
line every 15 seconds while a peer is catching up or waiting. You do not need to
run the command again; leave Output's automatic scrolling enabled to follow it.

Each update shows a separate check in each direction, for example:

```text
Receiving check #1: 25% — 250/1,000 records checked; 750 remaining to check.
Sending (peer confirmed) check #1: 50% — 500/1,000 records checked; 500 remaining to check.
Current receive batch: 0 records to save; 5 known content downloads left (includes the active one).
Downloading content: 65,536/190,000 bytes (34.4%); not yet saved.
```

**What the percentage means:** each pass fixes its total to all records in the
peer's approved sharing scope at the start of that pass. Already-present shared
records count toward the check without being downloaded again. New edits enter the next
pass, which has a new number and total. Records are storage entries, not History
rows; one edit can produce several records.

Checked counts advance after each batch (at most 128 records), including its
content responses. Sending progress waits for the peer's confirmation. **100%
means that check finished.** Any unavailable content remains explicitly listed,
and the status stays **waiting for content** until a later pass repairs it.

Remaining checks are not an estimate of bytes or time left. Content sizes and
nested references are discovered during transfer, so the queue shows currently
known downloads, not a fixed total of every content object. Partial byte progress
applies only to the current object and does not count as saved content.

The saved counters below the check progress are cumulative for this connection:
records and content **received and saved here**, and **sent and confirmed saved
by the peer**. They reset on reconnect; saved history persists. A's receiving
direction is B-to-A; A's sending direction is A-to-B.

During catch-up, older builds could display detached **EditChain ops** command
results before their session records arrived. They reconnect automatically when
the missing records arrive. Current builds send shared parents before children
to avoid this transfer-order gap. Parents outside the approved sharing scope are
not fetched; new-history-only sharing can therefore start at a real history boundary.

`Checking shared history (first pass)` means the first inventory check has not
finished. This also happens with new-history-only sharing; it does not indicate
which history was approved for sharing. Before the first inventory page arrives,
the total is shown as unknown. Initial local scans have no percentage yet. A
waiting update confirms that status reporting is running; quiet, caught-up peers
do not produce repeated lines.

## History says Codex retry

**Live · Codex retry** means local Codex-session import failed while the History
view can still update from saved records. Hover over the status or open the
**EditChain History** Output channel for the specific error. Received multiplayer
history does not require a local Codex exporter.

If the error says `codex helper ... could not be spawned`, build the separate
exporter from the EditChain checkout:

```sh
cargo build --release --locked --manifest-path tools/codex-session-exporter/Cargo.toml
```

This requires the sibling Codex checkout described in the
[exporter build instructions](../tools/codex-session-exporter/README.md#build-and-run-contract-local-only).
Reload the VS Code window after building, or set
`editchain-history.live.codexHelperPath` to an existing exporter executable's
absolute path. `./reinstall-vscode.sh` does not build this optional helper.
Failed imports stay queued and retry; their source checkpoints do not advance.

## Optional repository discovery

After connecting, **EditChain: Configure Multiplayer Repository Discovery** can
publish public connection metadata in an `owner/repository` you can collaborate
on. It requests GitHub `repo` access. Each new pair still needs an invitation;
use the same command to disable discovery.
Cancelling a configuration change keeps the previous discovery setup.

## Compatibility

Install the same EditChain build on every device: the extension, the Rust
service and the native peer worker ship together and are not meant to be mixed.
Update all devices together.

Progress totals and confirmations use peer protocol 3. Update and reload both devices, then
run **EditChain: Resume / Reconnect Shared History** on both. Existing device
approvals, received records, and sharing choices persist. Protocol-1 and protocol-2
peers cannot synchronize with a protocol-3 peer.

- A workspace whose sharing ledger was written by the updated build is rejected
  by an older peer worker, which fails closed instead of continuing.
- Older capture code ignores the added ledger field, so mixing versions can
  keep the earlier behavior that treated a peer's copy of your own history as
  peer-authored.
- Updating prevents new problems of that kind. It does not repair or restore
  history that was already quarantined by the earlier behavior.
