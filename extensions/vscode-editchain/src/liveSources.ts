import { execFile } from 'child_process';
import { promises as fs } from 'fs';
import * as path from 'path';
import { LiveCapture } from './liveSync';

export interface LivePaths {
  workspace: string;
  chain: string;
  sessions: string;
  cli: string;
  helper: string;
}

function missing(error: unknown): boolean {
  return (error as NodeJS.ErrnoException).code === 'ENOENT';
}

async function sourceStamp(file: string): Promise<{ key: string; modified: bigint }> {
  try {
    const stat = await fs.stat(file, { bigint: true });
    return { key: `${stat.ino}:${stat.size}:${stat.mtimeNs}:${stat.ctimeNs}`, modified: stat.mtimeNs };
  } catch (error) {
    if (missing(error)) return { key: '', modified: 0n };
    throw error;
  }
}

async function stamp(file: string): Promise<string> {
  return (await sourceStamp(file)).key;
}

/** Polling also catches atomic replacement and creation of a previously absent
 * sessions tree; it does not depend on editor watcher exclusions or OS events. */
async function tree(root: string, accepts: (name: string) => boolean, recursive = true, newestFirst = false): Promise<Map<string, string>> {
  const found: { file: string; key: string; modified: bigint }[] = [];
  const pending = [root];
  while (pending.length) {
    const directory = pending.pop()!;
    let entries;
    try { entries = await fs.readdir(directory, { withFileTypes: true }); }
    catch (error) { if (missing(error)) continue; throw error; }
    for (const entry of entries) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory() && recursive) pending.push(file);
      else if (entry.isFile() && accepts(entry.name)) {
        const version = await sourceStamp(file);
        if (version.key) found.push({ file, ...version });
      }
    }
  }
  if (newestFirst) found.sort((a, b) => a.modified === b.modified
    ? a.file.localeCompare(b.file) : a.modified > b.modified ? -1 : 1);
  return new Map(found.map(({ file, key }) => [file, key]));
}

async function textFile(file: string): Promise<string> {
  try { return await fs.readFile(file, 'utf8'); }
  catch (error) { if (missing(error)) return ''; throw error; }
}

async function gitStamp(workspace: string): Promise<string> {
  let git = path.join(workspace, '.git');
  const kind = await fs.stat(git).catch(error => { if (missing(error)) return undefined; throw error; });
  if (!kind) return '';
  if (kind.isFile()) {
    const pointer = (await textFile(git)).trim();
    if (!pointer.startsWith('gitdir: ')) return '';
    git = path.resolve(workspace, pointer.slice(8));
  }
  const commonFile = (await textFile(path.join(git, 'commondir'))).trim();
  const common = commonFile ? path.resolve(git, commonFile) : git;
  const refs = await tree(path.join(common, 'refs'), name => !name.endsWith('.lock'));
  const fixed = await Promise.all([
    path.join(git, 'HEAD'), path.join(common, 'packed-refs'), path.join(common, 'shallow'),
  ].map(async file => [file, await stamp(file)]));
  return JSON.stringify([...refs, ...fixed].sort());
}

async function titleStamp(sessions: string): Promise<string> {
  const direct = await stamp(path.join(sessions, 'session_index.jsonl'));
  return direct || (path.basename(sessions) === 'sessions'
    ? await stamp(path.join(sessions, '..', 'session_index.jsonl')) : '');
}

export async function captureSources(paths: LivePaths, nativeFrontier = false): Promise<LiveCapture> {
  const [sessions, chain, git, titles] = await Promise.all([
    tree(paths.sessions, name => name.startsWith('rollout-') && name.endsWith('.jsonl'), true, true),
    nativeFrontier ? Promise.resolve(new Map<string, string>()) : tree(paths.chain, name => name.endsWith('.eclog'), false),
    nativeFrontier ? Promise.resolve('') : gitStamp(paths.workspace),
    titleStamp(paths.sessions),
  ]);
  return { sessions, titles, history: JSON.stringify([...chain].sort()) + git };
}

/** The header is only a discovery optimization. Unknown headers still reach
 * the native importer, which authoritatively checks projected session cwd. */
export async function belongsToWorkspace(file: string, workspace: string): Promise<boolean> {
  const handle = await fs.open(file, 'r');
  try {
    const { buffer, bytesRead } = await handle.read(Buffer.alloc(65536), 0, 65536, 0);
    const line = buffer.subarray(0, bytesRead).toString('utf8').split('\n', 1)[0];
    let value;
    try { value = JSON.parse(line); } catch { return true; }
    if (value.type !== 'session_meta' || typeof value.payload?.cwd !== 'string') return true;
    if (!path.isAbsolute(value.payload.cwd)) return true;
    const cwd = await fs.realpath(value.payload.cwd).catch(() => path.resolve(value.payload.cwd));
    const root = await fs.realpath(workspace);
    const relative = path.relative(root, cwd);
    return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
  } finally { await handle.close(); }
}

export async function importSources(paths: LivePaths, files: string[], signal: AbortSignal, log: (text: string) => void = () => {}): Promise<void> {
  const selected = [];
  for (const file of files) {
    signal.throwIfAborted();
    if (!await belongsToWorkspace(file, paths.workspace)) continue;
    selected.push(file);
  }
  log(`Matched ${selected.length} of ${files.length} changed rollouts to this workspace.`);
  // Bound argv size during initial catch-up; an ordinary live pass has one or
  // a few files and produces one durable batch and one prepared render snapshot.
  for (let offset = 0; offset < selected.length; offset += 32) {
    signal.throwIfAborted();
    const args = ['import', '--provider', 'codex', '--workspace', paths.workspace,
      '--chain', paths.chain, '--sessions-dir', paths.sessions, '--codex-helper', paths.helper,
      ...selected.slice(offset, offset + 32).flatMap(file => ['--codex-rollout', file])];
    log(`Importing ${Math.min(32, selected.length - offset)} rollouts; most recently modified: ${path.basename(selected[offset])}`);
    await new Promise<void>((resolve, reject) => {
      let killTimer: ReturnType<typeof setTimeout> | undefined;
      const started = Date.now();
      let progress: ReturnType<typeof setInterval> | undefined;
      const child = execFile(paths.cli, args, { cwd: paths.workspace, killSignal: 'SIGINT', maxBuffer: 1024 * 1024 }, (error, _stdout, stderr) => {
        if (progress) clearInterval(progress);
        signal.removeEventListener('abort', cancel);
        if (killTimer) clearTimeout(killTimer);
        if (error) reject(new Error(`Codex import failed: ${stderr.trim() || error.message}`));
        else resolve();
      });
      progress = setInterval(() => log(`Importer still running (${Math.round((Date.now() - started) / 1000)}s); waiting for durable import and snapshot preparation.`), 10_000);
      // Stream the native CLI's reports instead of hiding all output until exit.
      child.stdout?.on('data', chunk => log(chunk.toString().trimEnd()));
      child.stderr?.on('data', chunk => log(chunk.toString().trimEnd()));
      // The CLI's SIGINT handler cancels its exporter process tree and leaves
      // checkpoints replayable. Await exit before another loop may own import.
      const cancel = () => {
        child.kill('SIGINT');
        killTimer = setTimeout(() => child.kill('SIGKILL'), 5000);
      };
      signal.addEventListener('abort', cancel, { once: true });
      if (signal.aborted) cancel();
    });
  }
}
