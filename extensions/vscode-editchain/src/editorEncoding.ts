import type { EditorEvent } from './editorOutbox';

type Snapshot = { text: string; json: Buffer };

/** Reuse unchanged JSON text while still emitting independent full snapshots. */
export class EditorEncoding {
  private previous: Snapshot | undefined;

  encode(value: EditorEvent): Buffer {
    const event = value.event;
    if ('toJSON' in value) return encode(value);
    if (event.type === 'tracking_stopped' || event.type === 'tracking_gap') this.previous = undefined;
    if (event.type === 'document_snapshot' && typeof event.text === 'string') {
      this.previous = { text: event.text, json: encode(event.text) };
      return this.withText(value, { ...event, text: undefined }, [['text', this.previous.json]]);
    }
    if (event.type !== 'document_changed' || typeof event.before !== 'string' || typeof event.after !== 'string') return encode(value);
    const before = this.previous?.text === event.before ? this.previous : { text: event.before, json: encode(event.before) };
    const after = replacement(before, event.after, event.changes) ?? encode(event.after);
    this.previous = { text: event.after, json: after };
    return this.withText(value, { ...event, before: undefined, after: undefined }, [['before', before.json], ['after', after]]);
  }

  private withText(value: EditorEvent, metadata: EditorEvent['event'], texts: [string, Buffer][]): Buffer {
    // Metadata is small; JSON string tokens for snapshots are already encoded.
    const { event: _event, ...envelope } = value;
    const prefix = JSON.stringify({ ...envelope, event: metadata }).slice(0, -2);
    return Buffer.concat([Buffer.from(prefix), ...texts.flatMap(([key, json]) => [Buffer.from(`,"${key}":`), json]), Buffer.from('}}')]);
  }
}

function encode(value: unknown): Buffer { return Buffer.from(JSON.stringify(value)); }

function replacement(before: Snapshot, after: string, changes: unknown): Buffer | undefined {
  if (!Array.isArray(changes) || changes.length !== 1) return undefined;
  if (!changes[0] || typeof changes[0] !== 'object') return undefined;
  const { offset, length, text } = changes[0];
  const end = offset + length;
  if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(length) || offset < 0 || length < 0
    || end > before.text.length || typeof text !== 'string' || splitsPair(before.text, offset) || splitsPair(before.text, end)) return undefined;
  const prefix = before.text.slice(0, offset), suffix = before.text.slice(end);
  // Only reuse an encoding if the complete recorded replacement is exact.
  if (prefix + text + suffix !== after) return undefined;
  const start = offset <= before.text.length / 2 ? 1 + encode(prefix).length - 2
    : before.json.length - 1 - (encode(before.text.slice(offset)).length - 2);
  const removed = encode(before.text.slice(offset, end)).length - 2;
  const inserted = encode(text);
  return Buffer.concat([before.json.subarray(0, start), inserted.subarray(1, -1), before.json.subarray(start + removed)]);
}

function splitsPair(text: string, offset: number): boolean {
  const left = text.charCodeAt(offset - 1), right = text.charCodeAt(offset);
  return left >= 0xd800 && left <= 0xdbff && right >= 0xdc00 && right <= 0xdfff;
}
