// Write `pi-held.jsonl` and record what Pi reads from it.
//
//   cd <pi checkout at v1.1.0, abe508e1b89912adde45528136c3221eb69acdd7>
//   PI_DIR=$PWD node --import "file://$PWD/packages/coding-agent/src/experimental/source-resolver.ts" \
//     <this dir>/generate-held.mjs
//
// `pi-held.jsonl` is a hand-written version 2 file of lines `JSON.parse`
// reads and `serde_json` does not: a custom entry nested 300 levels deep,
// lone UTF-16 surrogate escapes, and a number beyond the double range. Pi's
// reading goes to `pi-held.pi.json`, and the bytes Pi leaves after opening
// (the version 3 migration) to `pi-held.pi-after-open.jsonl`. The input is
// fixed, so each run writes the same input; the listing's times come from
// the entries, not the clock.

import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { loadSessionManager, summarize } from "./summary.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const CWD = "/golden/project";
const SessionManager = await loadSessionManager();

const deep = `${'{"a":'.repeat(300)}"\\ud800"${"}".repeat(300)}`;
const input = [
	JSON.stringify({ type: "session", version: 2, id: "held", timestamp: "2026-01-01T00:00:00.000Z", cwd: CWD }),
	JSON.stringify({ type: "message", id: "a1b2c3d4", parentId: null, timestamp: "2026-01-01T00:00:01.000Z", message: { role: "user", content: "first", timestamp: 1767225601000 } }),
	`{"type":"custom","customType":"ext","data":${deep},"id":"b2c3d4e5","parentId":"a1b2c3d4","timestamp":"2026-01-01T00:00:02.000Z"}`,
	`{"type":"custom","customType":"ext","data":{"n":1e400,"s":"x\\udc00"},"id":"c3d4e5f6","parentId":"b2c3d4e5","timestamp":"2026-01-01T00:00:03.000Z"}`,
	`{"type":"message","id":"d4e5f6a7","parentId":"c3d4e5f6","timestamp":"2026-01-01T00:00:04.000Z","message":{"role":"user","content":"x\\uD800y","timestamp":1767225604000}}`,
	`{"type":"message","id":"e5f6a7b8","parentId":"d4e5f6a7","timestamp":"2026-01-01T00:00:05.000Z","message":{"role":"hookMessage","customType":"x","content":"hook \\udc00","display":true,"timestamp":1767225605000}}`,
];
writeFileSync(join(here, "pi-held.jsonl"), `${input.join("\n")}\n`);

const work = mkdtempSync(join(tmpdir(), "pi-held-"));
try {
	const copy = join(work, "pi-held.jsonl");
	copyFileSync(join(here, "pi-held.jsonl"), copy);
	const manager = SessionManager.open(copy, work);
	writeFileSync(join(here, "pi-held.pi.json"), `${JSON.stringify(await summarize(SessionManager, manager, CWD))}\n`);
	writeFileSync(join(here, "pi-held.pi-after-open.jsonl"), readFileSync(copy));
} finally {
	rmSync(work, { recursive: true, force: true });
}
