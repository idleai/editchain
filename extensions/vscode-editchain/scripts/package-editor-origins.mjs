import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

// Keep the ordinary package on stable APIs. This local VSIX opts into the
// proposal only in its staged manifest; VS Code still requires runtime opt-in.
const extension = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const manifest = JSON.parse(fs.readFileSync(path.join(extension, 'package.json'), 'utf8'));
const destination = path.resolve(process.argv[2] || path.join(extension, '../../outputs', `editchain-history-${manifest.version}-editor-origins.vsix`));
const stage = fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-editor-origins-'));
try {
  for (const name of ['out', 'media', 'README.md', 'LICENSE.md', '.vscodeignore', 'package-lock.json']) {
    fs.cpSync(path.join(extension, name), path.join(stage, name), { recursive: true });
  }
  if (fs.existsSync(path.join(extension, 'bin'))) fs.cpSync(path.join(extension, 'bin'), path.join(stage, 'bin'), { recursive: true });
  // Stage the production dependency tree exactly as vsce selects it (npm list --production),
  // so runtime dependencies such as @microsoft/dev-tunnels-* ship like the regular package.
  const npmList = execFileSync(
    'npm', ['list', '--production', '--parseable', '--depth=99999', '--loglevel=error'],
    { cwd: extension, encoding: 'utf8' });
  for (const dir of npmList.split(/[\r\n]+/)) {
    if (!dir || !path.isAbsolute(dir)) continue;
    const relative = path.relative(extension, dir);
    if (!relative || relative.startsWith('..') || path.isAbsolute(relative)) continue;
    if (relative !== 'node_modules' && !relative.startsWith(`node_modules${path.sep}`)) continue;
    fs.cpSync(dir, path.join(stage, relative), { recursive: true });
  }
  manifest.enabledApiProposals = ['textDocumentChangeReason'];
  fs.writeFileSync(path.join(stage, 'package.json'), JSON.stringify(manifest, null, 2));
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  execFileSync(path.join(extension, 'node_modules/.bin/vsce'), ['package', '--out', destination],
    { cwd: stage, stdio: 'inherit' });
} finally {
  fs.rmSync(stage, { recursive: true, force: true });
}
