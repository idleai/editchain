import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const extension = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repository = path.resolve(extension, '../..');
const destination = path.join(extension, 'bin', `${process.platform}-${process.arch}`);
for (const [name, crate] of [['editchain-vscode-service', 'editchain-node'], ['editchain-peer', 'editchain-sync']]) {
  execFileSync('cargo', ['build', '--release', '-p', crate, '--bin', name, '--locked'], { cwd: repository, stdio: 'inherit' });
  const binary = name + (process.platform === 'win32' ? '.exe' : '');
  fs.mkdirSync(destination, { recursive: true });
  fs.copyFileSync(path.join(repository, 'target/release', binary), path.join(destination, binary));
}
