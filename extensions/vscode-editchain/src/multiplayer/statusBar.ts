import type { SharingStatus } from './manager';

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
  const phase = connected.some(peer => peer.progress?.synchronizing) ? ' · syncing'
    : connected.some(peer => peer.progress?.unavailable) ? ' · waiting for content' : '';
  return `Sharing · ${connected.length}/${value.peers.length} connected${phase}`;
}

export function sharingDetails(value: SharingStatus): string {
  const peers = value.peers.map(peer => {
    const progress = peer.progress;
    const phase = !progress?.accepted ? peer.state : progress.synchronizing
      ? `Connected; syncing shared history${progress.rounds ? '' : ' (first pass)'}`
      : progress.unavailable ? 'Connected; waiting for content' : 'Connected; caught up at last check';
    return `${peer.fingerprint?.slice(0, 12) || 'Device'}: ${phase}`;
  });
  return [value.message, ...peers, 'Show Multiplayer Status for automatic transfer updates.'].filter(Boolean).join('\n');
}
