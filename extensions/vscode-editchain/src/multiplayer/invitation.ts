import { X509Certificate } from 'node:crypto';
import { Tunnel, TunnelAccessScopes, TunnelConnectionMode, TunnelProtocol, TunnelRelayTunnelEndpoint } from '@microsoft/dev-tunnels-contracts';
import { TunnelAccessTokenProperties } from '@microsoft/dev-tunnels-management';
import type { PublicDevice } from './native';
import { ProbeError } from '../devTunnels/probe';

export const MULTIPLAYER_PORT = 43188;
export const INVITE_LIMIT = 32 * 1024;
export type JoinRequest = { version: 1; kind: 'request'; device: PublicDevice };
export type RelayEndpoint = { tunnelId: string; clusterId: string; hostId: string; clientRelayUri: string; hostPublicKeys: string[] };
export type Invitation = { version: 1; kind: 'invite'; space: string; host: PublicDevice; guest: string;
  endpoint: RelayEndpoint; connectToken: string; expiresAt: number };

export function encodeInvitation(value: JoinRequest | Invitation): string {
  return `editchain:${Buffer.from(JSON.stringify(value)).toString('base64url')}`;
}

function decode(text: string): any {
  const source = text.trim();
  if (source.length > INVITE_LIMIT || !/^editchain:[A-Za-z0-9_-]+$/.test(source)) throw new ProbeError('Invalid EditChain invitation.');
  try { return JSON.parse(Buffer.from(source.slice('editchain:'.length), 'base64url').toString('utf8')); }
  catch { throw new ProbeError('Invalid EditChain invitation encoding.'); }
}

export function publicDevice(value: unknown): PublicDevice {
  const device = value as Partial<PublicDevice> | null;
  if (!device || typeof device.certificate !== 'string' || device.certificate.length > 8192 ||
    typeof device.fingerprint !== 'string' || !/^[a-f0-9]{64}$/.test(device.fingerprint)) throw new ProbeError('Invalid device identity.');
  try { new X509Certificate(Buffer.from(device.certificate, 'base64')); }
  catch { throw new ProbeError('Invalid device certificate.'); }
  // Rust recomputes the BLAKE3 fingerprint and verifies the certificate. The
  // manager compares that result before any UI approval or connection attempt.
  return { certificate: device.certificate, fingerprint: device.fingerprint };
}

export function parseRequest(text: string): JoinRequest {
  const value = decode(text);
  if (value?.version !== 1 || value.kind !== 'request') throw new ProbeError('Expected an EditChain join request.');
  return { version: 1, kind: 'request', device: publicDevice(value.device) };
}

export function validateEndpoint(value: unknown): RelayEndpoint {
  const endpoint = value as Partial<RelayEndpoint> | null;
  if (!endpoint || ![endpoint.tunnelId, endpoint.clusterId, endpoint.hostId].every(value => typeof value === 'string' && /^[a-zA-Z0-9-]{1,128}$/.test(value)) ||
    typeof endpoint.clientRelayUri !== 'string' || endpoint.clientRelayUri.length > 4096 ||
    !Array.isArray(endpoint.hostPublicKeys) || !endpoint.hostPublicKeys.length || endpoint.hostPublicKeys.length > 4 ||
    !endpoint.hostPublicKeys.every(key => typeof key === 'string' && /^[a-zA-Z0-9+/=]{1,4096}$/.test(key))) throw new ProbeError('Invalid relay endpoint.');
  let url: URL;
  try { url = new URL(endpoint.clientRelayUri); } catch { throw new ProbeError('Invalid relay address.'); }
  if (url.protocol !== 'wss:' || url.username || url.password || url.hash || (url.port && url.port !== '443') ||
    !/^[a-z0-9-]+\.rel\.tunnels\.api\.visualstudio\.com$/.test(url.hostname)) throw new ProbeError('Invitation must use the Microsoft Dev Tunnels relay.');
  return { tunnelId: endpoint.tunnelId!, clusterId: endpoint.clusterId!, hostId: endpoint.hostId!,
    clientRelayUri: url.toString(), hostPublicKeys: [...endpoint.hostPublicKeys] };
}

export function parseInvitation(text: string, now = Date.now()): Invitation {
  const value = decode(text);
  if (value?.version !== 1 || value.kind !== 'invite' || typeof value.space !== 'string' || !/^[a-zA-Z0-9-]{1,128}$/.test(value.space) ||
    typeof value.guest !== 'string' || !/^[a-f0-9]{64}$/.test(value.guest) || typeof value.connectToken !== 'string' || value.connectToken.length > 8192 ||
    !Number.isSafeInteger(value.expiresAt) || value.expiresAt <= now || value.expiresAt > now + 24 * 60 * 60 * 1000) {
    throw new ProbeError('Invalid or expired EditChain invitation. Ask the host for a new invitation.');
  }
  const expiration = TunnelAccessTokenProperties.tryParse(value.connectToken)?.expiration?.getTime();
  if (!expiration || expiration <= now || expiration < value.expiresAt) throw new ProbeError('The invitation connect grant is expired or invalid.');
  return { version: 1, kind: 'invite', space: value.space, host: publicDevice(value.host), guest: value.guest,
    endpoint: validateEndpoint(value.endpoint), connectToken: value.connectToken, expiresAt: value.expiresAt };
}

export function invitationTunnel(invitation: Invitation): Tunnel {
  const { endpoint } = invitation;
  const relay: TunnelRelayTunnelEndpoint = { connectionMode: TunnelConnectionMode.TunnelRelay,
    hostId: endpoint.hostId, hostPublicKeys: endpoint.hostPublicKeys, clientRelayUri: endpoint.clientRelayUri };
  return { tunnelId: endpoint.tunnelId, clusterId: endpoint.clusterId,
    endpoints: [relay], ports: [{ portNumber: MULTIPLAYER_PORT, protocol: TunnelProtocol.Auto }],
    accessTokens: { [TunnelAccessScopes.Connect]: invitation.connectToken } };
}
