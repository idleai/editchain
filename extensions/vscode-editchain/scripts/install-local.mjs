import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { enableEditorOrigins } from './runtime-arguments.mjs';

// Build and install the same artifact. Never depend on a remembered VSIX filename.
const extension = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repository = path.resolve(extension, '../..');
const manifest = JSON.parse(fs.readFileSync(path.join(extension, 'package.json'), 'utf8'));
const args = process.argv.slice(2);
const origins = args[0] === '--editor-origins';
if (origins) args.shift();
let runtimeArguments;
if (args[0] === '--runtime-args') { args.shift(); runtimeArguments = args.shift(); }
if (origins && !runtimeArguments) throw new Error('Direct editor reasons require --runtime-args PATH to the file opened by Preferences: Configure Runtime Arguments.');
const destination = path.join(repository, 'outputs', `${manifest.name}-${manifest.version}${origins ? '-editor-origins' : ''}.vsix`);
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const desktop = ['/snap/code/current/usr/share/code/bin/code', '/usr/share/code/bin/code'].find(location => fs.existsSync(location));
const [code = desktop || 'code', ...options] = args;
const env = { ...process.env };
delete env.VSCODE_IPC_HOOK_CLI;
const run = (command, args, cwd = extension) => execFileSync(command, args, { cwd, env, stdio: 'inherit' });

run('cargo', ['build', '--release', '-p', 'editchain-node', '--bin', 'editchain-vscode-service', '--locked'], repository);
run(npm, ['run', 'build:renderer']);
run(npm, ['run', 'compile']);
fs.mkdirSync(path.dirname(destination), { recursive: true });
run(npm, origins ? ['run', 'package:editor-origins', '--', destination] : ['run', 'package', '--', '--out', destination]);
run(code, [...options, '--install-extension', destination, '--force']);
const installed = execFileSync(code, [...options, '--list-extensions', '--show-versions'], { env, encoding: 'utf8' });
const expected = `${manifest.publisher}.${manifest.name}@${manifest.version}`;
if (!installed.split(/\r?\n/).some(line => line.trim().toLowerCase() === expected.toLowerCase())) {
  throw new Error(`VS Code did not select ${expected}; inspect its extension profile.`);
}
if (origins) {
  enableEditorOrigins(path.resolve(runtimeArguments), `${manifest.publisher}.${manifest.name}`);
  console.log(`Verified ${expected} with direct editor reasons. Quit and reopen VS Code to apply its runtime arguments; window reload alone is insufficient.`);
} else console.log(`Verified ${expected} with limited stable attribution. Reload the VS Code window to activate it.`);
console.log(`Native service: ${path.join(repository, 'target/release/editchain-vscode-service')}`);
