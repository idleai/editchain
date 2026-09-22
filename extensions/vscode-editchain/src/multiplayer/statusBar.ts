import type { SharingStatus } from './manager';
import { checking, checkPercent, describeCheck, describeDownload, missingContent } from './progress';

/** Connection health stays visible while an authenticated peer checks history. */
export function sharingLabel(value: SharingStatus): string {
  if (!value.enabled) return 'Sharing stopped';
  if (!value.peers.length) return value.hosting ? 'Sharing · waiting for peers' : 'Sharing · reconnecting';
  const connected = value.peers.filter(peer => peer.progress?.accepted);
  if (!connected.length) {
    if (value.peers.some(peer => peer.state === 'Authenticating')) return 'Sharing · authenticating';
    if (value.peers.some(peer => peer.state === 'Connecting')) return 'Sharing · connecting';
    return 'Sharing · reconnecting';
  }
  const working = connected.some(peer => peer.progress && checking(peer.progress));
  const progress = connected.length === 1 ? connected[0].progress : undefined;
  const percentages = progress && (checkPercent(progress.incoming) !== undefined || checkPercent(progress.outgoing) !== undefined)
    ? ` · ↓${checkPercent(progress.incoming) ?? '…'} ↑${checkPercent(progress.outgoing) ?? '…'}` : ' · syncing';
  const phase = working ? percentages
    : connected.some(peer => peer.progress && missingContent(peer.progress)) ? ' · waiting for content' : '';
  return `Sharing · ${connected.length}/${value.peers.length} connected${phase}`;
}

export function sharingDetails(value: SharingStatus): string {
  const peers = value.peers.map(peer => {
    const progress = peer.progress;
    const phase = !progress?.accepted ? peer.state : checking(progress)
      ? `Connected; syncing shared history${progress.rounds ? '' : ' (first pass)'}`
      : missingContent(progress) ? 'Connected; waiting for content' : 'Connected; caught up at last check';
    const details = progress?.accepted && progress.incoming ? `\n${describeCheck('Receiving ↓', progress.incoming)}\n${describeCheck('Sending ↑ (peer confirmed)', progress.outgoing)}\n${describeDownload(progress)}` : '';
    return `${peer.fingerprint?.slice(0, 12) || 'Device'}: ${phase}${details}`;
  });
  return [value.message, ...peers, 'Percentages measure history checks, including already present records. New edits enter the next pass.',
    'Show Multiplayer Status for automatic transfer updates.'].filter(Boolean).join('\n');
}
