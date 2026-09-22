/** Full snapshots remain available for every historical editor diff. */
export const MAX_EDITOR_BUFFER_BYTES = 8 * 1024 * 1024;
// Before, after, and a full replacement can each expand sixfold in JSON.
// Keep a metadata allowance below the native 160 MiB request-frame bound.
export const MAX_EDITOR_EVENT_BYTES = 18 * MAX_EDITOR_BUFFER_BYTES + 1024 * 1024;
export const EDITOR_BATCH_BYTES = 4 * 1024 * 1024;
export const EDITOR_QUEUE_BYTES = 256 * 1024 * 1024;
export const EDITOR_DISK_BYTES = 512 * 1024 * 1024;
/** Pending JSONL bytes while the archive write is behind; never blocks capture. */
export const HISTORY_ARCHIVE_QUEUE_BYTES = 64 * 1024 * 1024;
// One archive line is a full event plus the archive envelope.
export const HISTORY_ARCHIVE_EVENT_BYTES = MAX_EDITOR_EVENT_BYTES + 4096;
