import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

// Build and install the same artifact. Never depend on a remembered VSIX filename.
const extension = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repository = path.resolve(extension, '../..');
const manifest = JSON.parse(fs.readFileSync(path.join(extension, 'package.json'), 'utf8'));
const destination = path.join(repository, 'outputs', `${manifest.name}-${manifest.version}.vsix`);
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const desktop = ['/snap/code/current/usr/share/code/bin/code', '/usr/share/code/bin/code'].find(location => fs.existsSync(location));
const [code = desktop || 'code', ...options] = process.argv.slice(2);
const env = { ...process.env };
delete env.VSCODE_IPC_HOOK_CLI;
const run = (command, args, cwd = extension) => execFileSync(command, args, { cwd, env, stdio: 'inherit' });

run('cargo', ['build', '--release', '-p', 'editchain-node', '--bin', 'editchain-vscode-service', '--locked'], repository);
run(npm, ['run', 'build:renderer']);
run(npm, ['run', 'compile']);
fs.mkdirSync(path.dirname(destination), { recursive: true });
run(npm, ['run', 'package', '--', '--out', destination]);
run(code, [...options, '--install-extension', destination, '--force']);
const installed = execFileSync(code, [...options, '--list-extensions', '--show-versions'], { env, encoding: 'utf8' });
const expected = `${manifest.publisher}.${manifest.name}@${manifest.version}`;
if (!installed.split(/\r?\n/).some(line => line.trim().toLowerCase() === expected.toLowerCase())) {
  throw new Error(`VS Code did not select ${expected}; inspect its extension profile.`);
}
console.log(`Verified ${expected}. Reload the VS Code window to activate it.`);
console.log(`Native service: ${path.join(repository, 'target/release/editchain-vscode-service')}`);
