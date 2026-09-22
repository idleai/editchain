/** Totals belong to a frozen inventory check, including already present records. */
export type CheckProgress = { pass: number; total_records: number | null; checked_records: number; complete: boolean; unavailable: number };
export type DownloadProgress = { content: boolean; received_bytes: number; total_bytes: number | null };
export type WorkProgress = { incoming?: CheckProgress; outgoing?: CheckProgress; pending_records?: number; pending_blobs?: number; download?: DownloadProgress | null };

const count = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) >= 0;
function validCheck(value: CheckProgress | undefined): boolean {
  return !!value && count(value.pass) && count(value.checked_records) && count(value.unavailable) && typeof value.complete === 'boolean'
    && (value.total_records === null ? !value.complete && value.checked_records === 0
      : count(value.total_records) && value.checked_records <= value.total_records && (!value.complete || value.checked_records === value.total_records));
}

export function validWorkProgress(value: WorkProgress): boolean {
  const download = value.download;
  return validCheck(value.incoming) && validCheck(value.outgoing) && count(value.pending_records) && count(value.pending_blobs)
    && (download === null || !!download && typeof download.content === 'boolean' && count(download.received_bytes)
      && (download.total_bytes === null ? download.received_bytes === 0 : count(download.total_bytes) && download.received_bytes <= download.total_bytes));
}

export function checking(value: WorkProgress & { synchronizing: boolean }): boolean {
  return value.synchronizing || !!value.outgoing?.pass && !value.outgoing.complete;
}

export function missingContent(value: WorkProgress & { unavailable: number }): boolean {
  return !!value.unavailable || !!value.outgoing?.unavailable;
}

/** Truncate so an unfinished check never rounds up to 100%. */
export function checkPercent(value?: CheckProgress): string | undefined {
  if (!value || value.total_records === null) return undefined;
  const percent = value.total_records === 0 ? (value.complete ? 100 : 0)
    : Math.floor(10000 * value.checked_records / value.total_records) / 100;
  return `${value.complete || value.total_records === 0 ? percent : Math.min(99.99, percent)}%`;
}

const number = (value: number) => value.toLocaleString('en-US');

export function describeCheck(label: string, value?: CheckProgress): string {
  if (!value || value.total_records === null) return `${label}: waiting for the shared-history total.`;
  const remaining = value.total_records - value.checked_records;
  const missing = value.unavailable ? ` ${number(value.unavailable)} content request(s) still unavailable.` : '';
  return `${label} check #${value.pass}: ${checkPercent(value)} — ${number(value.checked_records)}/${number(value.total_records)} records checked; ${number(remaining)} remaining to check.${missing}`;
}

export function describeDownload(value: WorkProgress): string {
  const queue = `Current receive batch: ${number(value.pending_records ?? 0)} records to save; ${number(value.pending_blobs ?? 0)} known content downloads left (includes the active one).`;
  const download = value.download;
  if (!download) return queue;
  const kind = download.content ? 'content' : 'record';
  if (download.total_bytes === null) return `${queue}\n  Waiting for ${kind} data; size not yet known.`;
  const percent = download.total_bytes === 0 ? 100 : Math.floor(1000 * download.received_bytes / download.total_bytes) / 10;
  return `${queue}\n  Downloading ${kind}: ${number(download.received_bytes)}/${number(download.total_bytes)} bytes (${percent}%); not yet saved.`;
}

/** Suppress routine completed pass numbers while still printing actual work. */
export function workSignature(value?: WorkProgress): unknown[] {
  const signature = (check?: CheckProgress) => check && [check.total_records, check.checked_records, check.complete, check.unavailable];
  return [signature(value?.incoming), signature(value?.outgoing), value?.pending_records, value?.pending_blobs, value?.download];
}
