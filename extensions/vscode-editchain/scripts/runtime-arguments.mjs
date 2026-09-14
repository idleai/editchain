import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { parse, modify, applyEdits } from 'jsonc-parser';

export function withEditorOrigins(text, extension) {
  const errors = [];
  const current = parse(text, errors, { allowTrailingComma: true });
  if (errors.length || !current || typeof current !== 'object' || Array.isArray(current)) {
    throw new Error('Runtime arguments must be a valid JSONC object; no settings were changed.');
  }
  const enabled = current['enable-proposed-api'];
  if (enabled !== undefined && (!Array.isArray(enabled) || enabled.some(value => typeof value !== 'string'))) {
    throw new Error('Invalid enable-proposed-api list; no settings were changed.');
  }
  // An existing empty array already enables all proposals; preserve its scope.
  if (enabled && (!enabled.length || enabled.some(value => value.toLowerCase() === extension.toLowerCase()))) return text;
  return applyEdits(text, modify(text, ['enable-proposed-api'], [...(enabled ?? []), extension], {
    formattingOptions: { insertSpaces: true, tabSize: 2, eol: text.includes('\r\n') ? '\r\n' : '\n' },
  }));
}

export function enableEditorOrigins(location, extension) {
  const exists = fs.existsSync(location);
  const text = exists ? fs.readFileSync(location, 'utf8') : '{}\n';
  const next = withEditorOrigins(text, extension);
  if (text === next) return;
  fs.mkdirSync(path.dirname(location), { recursive: true });
  if (exists) fs.copyFileSync(location, `${location}.editchain-${randomUUID()}.bak`, fs.constants.COPYFILE_EXCL);
  const temporary = `${location}.${randomUUID()}.tmp`;
  try {
    fs.writeFileSync(temporary, next, { mode: exists ? fs.statSync(location).mode : 0o600, flag: 'wx' });
    fs.renameSync(temporary, location);
  } finally { fs.rmSync(temporary, { force: true }); }
}
