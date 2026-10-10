// Open a session file with Pi's SessionManager and print what it reads.
//
//   cd <pi checkout at v1.1.0>
//   PI_DIR=$PWD node --import "file://$PWD/packages/coding-agent/src/experimental/source-resolver.ts" \
//     <this dir>/read-with-pi.mjs <session.jsonl> <list cwd>
//
// The file is copied to a temporary directory first, since opening may
// migrate or repair it.

import { copyFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { loadSessionManager, summarize } from "./summary.mjs";

const [file, listCwd] = process.argv.slice(2);
if (!file || !listCwd) throw new Error("usage: read-with-pi.mjs <session.jsonl> <list cwd>");
const SessionManager = await loadSessionManager();
const dir = mkdtempSync(join(tmpdir(), "pi-read-"));
try {
	const copy = join(dir, basename(file));
	copyFileSync(file, copy);
	const manager = SessionManager.open(copy, dir);
	process.stdout.write(`${JSON.stringify(await summarize(SessionManager, manager, listCwd))}\n`);
} finally {
	rmSync(dir, { recursive: true, force: true });
}
