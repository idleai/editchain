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
- The host signs in to GitHub when prompted. Joining uses the host's invitation.
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
   invitation, choose which of your history to share, and approve the host.
5. **Both:** wait for **Sharing · 1/1 live** in the status bar, then run
   **EditChain: Open History Explorer**.

## Try it

Edit and save a text file on one device. On the other, find the new History row
and click its file change to open the recorded before/after diff. The receiving
device's working file stays as it was. Repeat in the opposite direction.
Keep History in live mode (**EditChain: Resume Live History** if it was paused).
Saved multiplayer receipts wake the open live view; a slow outgoing reply does
not delay that notification. Recorded diffs become available as their content
arrives, including when the view was opened before its content store existed.

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

## Watch synchronization progress

Run **EditChain: Show Multiplayer Status** once. The **EditChain Multiplayer**
Output channel prints a JSON snapshot, then follows progress automatically.
Updates are timestamped and grouped to at most once per second, with a status
line every 15 seconds while a peer is catching up or waiting. You do not need to
run the command again; leave Output's automatic scrolling enabled to follow it.

Each peer line reports records and content objects **received and saved on this
device**, records and content **sent and confirmed saved by the peer**, completed
synchronization passes, missing-content responses, and how
long since a saved-data update was observed. Counts apply to the current
connection and reset on reconnect. A's counters describe B-to-A transfers; B's
describe A-to-B transfers. A content object stores recorded revision data.

`Checking shared history (first pass)` means the first inventory check has not
finished. This also happens with new-history-only sharing; it does not indicate
which history was approved for sharing. Total remaining work, percentage, scans, and partial
downloads are not available yet. A waiting update confirms that status reporting
is running; unchanged saved counts alone cannot distinguish scanning, downloading,
or a stalled transfer. Quiet, caught-up peers do not produce repeated lines.

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

- A workspace whose sharing ledger was written by the updated build is rejected
  by an older peer worker, which fails closed instead of continuing.
- Older capture code ignores the added ledger field, so mixing versions can
  keep the earlier behavior that treated a peer's copy of your own history as
  peer-authored.
- Updating prevents new problems of that kind. It does not repair or restore
  history that was already quarantined by the earlier behavior.
