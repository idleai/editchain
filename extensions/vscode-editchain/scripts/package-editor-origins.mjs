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
  for (const name of ['out', 'media', 'README.md', 'LICENSE.md', '.vscodeignore']) {
    fs.cpSync(path.join(extension, name), path.join(stage, name), { recursive: true });
  }
  manifest.enabledApiProposals = ['textDocumentChangeReason'];
  fs.writeFileSync(path.join(stage, 'package.json'), JSON.stringify(manifest, null, 2));
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  execFileSync(path.join(extension, 'node_modules/.bin/vsce'), ['package', '--no-dependencies', '--out', destination],
    { cwd: stage, stdio: 'inherit' });
} finally {
  fs.rmSync(stage, { recursive: true, force: true });
}
