import type * as vscode from 'vscode';

export type InputChange = Pick<vscode.TextDocumentContentChangeEvent, 'rangeOffset' | 'rangeLength' | 'text'>;
type Span = [number, number];

/** Only removal of text inserted in this human episode can be a correction. */
export function retractsInserted(spans: Span[], changes: readonly InputChange[]): boolean {
  return changes.length > 0 && changes.every(change => change.text === '' && change.rangeLength > 0
    && spans.some(([start, end]) => start <= change.rangeOffset && end >= change.rangeOffset + change.rangeLength));
}

/** Keep UTF-16 ownership ranges through confirmed input, including replacements. */
export function insertedRanges(spans: Span[], changes: readonly InputChange[]): Span[] {
  for (const change of changes) {
    const start = change.rangeOffset, end = start + change.rangeLength;
    const delta = change.text.length - change.rangeLength;
    const next: Span[] = [];
    for (const [left, right] of spans) {
      if (right <= start) next.push([left, right]);
      else if (left >= end) next.push([left + delta, right + delta]);
      else {
        if (left < start) next.push([left, start]);
        if (right > end) next.push([start + change.text.length, right + delta]);
      }
    }
    if (change.text.length) next.push([start, start + change.text.length]);
    spans = [];
    for (const [left, right] of next.sort((a, b) => a[0] - b[0])) {
      const previous = spans.at(-1);
      if (previous && left <= previous[1]) previous[1] = Math.max(previous[1], right);
      else spans.push([left, right]);
    }
  }
  return spans;
}
