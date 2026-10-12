// Record Pi's execCommand against the native child fixture. Run with Node
// from rust/ so rustc uses the pinned toolchain: node <this file> <Pi checkout>.
import { execFileSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const revision = 'abe508e1b89912adde45528136c3221eb69acdd7';
const pi = resolve(process.argv[2]);
if (execFileSync('git', ['-C', pi, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim() !== revision) {
  throw new Error('Use Pi v1.1.0 at the recorded revision');
}
const { execCommand } = await import(pathToFileURL(join(pi, 'packages/coding-agent/src/core/exec.ts')));
const work = await mkdtemp(join(tmpdir(), 'pi-exec-oracle-'));
try {
  const executable = join(work, process.platform === 'win32' ? 'exec-child.exe' : 'exec-child');
  execFileSync('rustc', ['--edition=2024', '--crate-name=exec_child', fileURLToPath(new URL('child.rs', import.meta.url)), '-o', executable]);
  const cases = [];
  const texts = ['', 'hello\n', 'stderr\r\n', 'literal $HOME; `echo no` & "quotes"', 'λ ไทย 日本語 😀\n', '\u001b[31mred\u001b[0m\n'];
  for (let index = 0; index < 24; index++) {
    const args = ['capture', texts[index % texts.length], texts[(index * 5 + 1) % texts.length], String([0, 1, 7, 255][index % 4])];
    cases.push({ args, result: await execCommand(executable, args, work) });
  }
  cases.push({ args: ['stdin'], result: await execCommand(executable, ['stdin'], work) });
  await writeFile(new URL('golden.json', import.meta.url), `${JSON.stringify({ revision, cases }, null, 2)}\n`);
} finally {
  await rm(work, { recursive: true, force: true });
}
