# Using EditChain multiplayer

Multiplayer shares recorded edit history between workspaces. Each participant
keeps their own working files and can open the other person's recorded diffs.

## Before you start

- Install the multiplayer EditChain extension on both devices. For the local
  build, use **Extensions: Install from VSIX…** and select
  `outputs/editchain-history-multiplayer.vsix` (currently Linux x64).
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

## Reconnect or stop

- Connections retry automatically. Reopening an enabled workspace resumes
  sharing; a hard process exit can take about 90 seconds to recover.
- Run **EditChain: Show Multiplayer Status** to inspect the connection, or
  **EditChain: Resume / Reconnect Shared History** to retry. An expired grant
  needs a fresh invitation from the host.
- Run **EditChain: Stop Sharing History** to disconnect, delete your hosted
  tunnel and disable automatic resume. Received history stays on each device.
- Use **EditChain: Remove Shared Device** to revoke a device's access to your
  replica. Previously received copies remain with that participant.
- If cleanup failed, stop sharing first, then run
  **EditChain: Clean Up Multiplayer Tunnels** in the same workspace/account.

## Optional repository discovery

After connecting, **EditChain: Configure Multiplayer Repository Discovery** can
publish public connection metadata in an `owner/repository` you can collaborate
on. It requests GitHub `repo` access. Each new pair still needs an invitation;
use the same command to disable discovery.
