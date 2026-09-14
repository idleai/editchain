import { promises as fs } from 'node:fs';
import * as path from 'node:path';
import { randomUUID, createHash } from 'node:crypto';

export type HumanIdentity = { kind: 'unsigned'; guid: string; stream: string };

/** Publish once, even when two extension hosts first activate together. */
export async function unsignedIdentity(directory: string): Promise<string> {
  await fs.mkdir(directory, { recursive: true });
  const destination = path.join(directory, 'unsigned-human-identity.json');
  try { return await readIdentity(destination); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
  const temporary = `${destination}.${randomUUID()}.tmp`;
  const file = await fs.open(temporary, 'wx', 0o600);
  try {
    try { await file.writeFile(JSON.stringify({ schema: 1, kind: 'unsigned', guid: randomUUID() })); await file.sync(); }
    finally { await file.close(); }
    try { await fs.link(temporary, destination); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error; }
    if (process.platform !== 'win32') {
      const parent = await fs.open(directory, 'r');
      try { await parent.sync(); } finally { await parent.close(); }
    }
  } finally { await fs.rm(temporary, { force: true }); }
  return await readIdentity(destination);
}

async function readIdentity(location: string): Promise<string> {
  const value = JSON.parse(await fs.readFile(location, 'utf8'));
  if (value.schema !== 1 || value.kind !== 'unsigned' || typeof value.guid !== 'string'
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value.guid)) {
    throw new Error('Stored unsigned human identity is invalid. Restore its identity file before resuming capture.');
  }
  return value.guid;
}

export function workspaceIdentity(guid: string, uri: string, root: string, chain: string): HumanIdentity {
  const stream = createHash('sha256').update(uri + '\0' + path.resolve(root, chain)).digest('hex').slice(0, 24);
  return { kind: 'unsigned', guid, stream };
}
