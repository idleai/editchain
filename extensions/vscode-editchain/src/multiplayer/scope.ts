/** Local outgoing consent. It does not limit what another device shares with us. */
export type SharingScope = { space: string; mode: 'all' | 'from_now' | 'legacy_from_now'; cutoff_ms: number | null;
  revision: number; active: boolean; legacy_excluded_records: number };
export type ScopeChoice = boolean | 'keep';

export function validScope(value: SharingScope): boolean {
  return !!value && typeof value.space === 'string' && typeof value.active === 'boolean'
    && ['all', 'from_now', 'legacy_from_now'].includes(value.mode)
    && Number.isSafeInteger(value.revision) && value.revision >= 0
    && Number.isSafeInteger(value.legacy_excluded_records) && value.legacy_excluded_records >= 0
    && (value.mode === 'from_now' ? Number.isSafeInteger(value.cutoff_ms) && value.cutoff_ms! >= 0 && value.cutoff_ms! <= 8.64e15 : value.cutoff_ms === null);
}

export function describeScope(value?: SharingScope): string {
  if (!value) return 'Outgoing history scope: not configured.';
  if (!value.active) return 'Outgoing history scope: change interrupted; choose the scope again to resume.';
  if (value.mode === 'all') return 'Outgoing history scope: all retained history.';
  if (value.mode === 'legacy_from_now') return `Outgoing history scope: saved earlier boundary (${value.legacy_excluded_records.toLocaleString('en-US')} excluded records; selection time unavailable).`;
  return `Outgoing history scope: records added after ${new Date(value.cutoff_ms!).toISOString()}.`;
}
