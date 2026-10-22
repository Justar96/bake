// Write the Pi golden session files and what Pi reads from each.
//
//   cd <pi checkout at v1.1.0, abe508e1b89912adde45528136c3221eb69acdd7>
//   PI_DIR=$PWD node --import "file://$PWD/packages/coding-agent/src/experimental/source-resolver.ts" \
//     <this dir>/generate.mjs
//
// Outputs, beside this script:
//   pi-v3.jsonl            a session Pi's SessionManager wrote, with every entry kind,
//                          then one line of an entry type v1.1.0 does not know
//   pi-v2.jsonl, pi-v1.jsonl, pi-torn.jsonl
//                          hand-written inputs: Pi's older versions and a torn tail
//   <name>.pi.json         Pi's reading of <name>.jsonl (see summary.mjs)
//   <name>.pi-after-open.jsonl
//                          the file's bytes after Pi opened it: migrated or repaired
//
// Pi writes random ids and the current time, so each run produces new bytes;
// the committed files are one run's output.

import { appendFileSync, copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { loadSessionManager, summarize } from "./summary.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const CWD = "/golden/project";
const SessionManager = await loadSessionManager();
const work = mkdtempSync(join(tmpdir(), "pi-golden-"));

const usage = {
	input: 1200,
	output: 345,
	cacheRead: 100000,
	cacheWrite: 0,
	totalTokens: 101545,
	cost: { input: 0.0000015, output: 1e-7, cacheRead: 0.30000000000000004, cacheWrite: 0, total: 1.5e-300 },
};

function writePiSession() {
	const manager = SessionManager.create(CWD, work);
	manager.appendModelChange("anthropic", "claude-test");
	manager.appendThinkingLevelChange("high");
	manager.appendMessage({
		role: "system",
		content: "You are a coding agent.",
		sections: { rules: "Be brief.", 2: "indexed section" },
		toolsAdded: [
			{
				name: "read",
				description: "Read a file",
				parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
				futureToolField: { kept: true },
			},
		],
		timestamp: 1767225600000,
	});
	const user1 = manager.appendMessage({
		role: "user",
		content: [
			{ type: "text", text: "Read \"a.ts\" — then explain\nwith émoji 😀 and \u0001 control" },
			{ type: "image", data: "iVBORw0KGgo=", mimeType: "image/png" },
		],
		timestamp: 1767225601000,
	});
	const assistant1 = manager.appendMessage({
		role: "assistant",
		content: [
			{ type: "thinking", thinking: "Need the file.", thinkingSignature: "sig==" },
			{ type: "text", text: "Reading it." },
			{ type: "toolCall", id: "call_1", name: "read", arguments: { path: "a.ts", "0": "first" } },
		],
		api: "anthropic-messages",
		provider: "anthropic",
		model: "claude-test",
		responseId: "msg_01",
		usage,
		stopReason: "toolUse",
		timestamp: 1767225602000,
		futureMessageField: { nested: [1, 2.5, null, "x"] },
	});
	const toolResult = manager.appendMessage({
		role: "toolResult",
		toolCallId: "call_1",
		toolName: "read",
		content: [{ type: "text", text: "export const a = 1;\n" }],
		details: { path: "a.ts", lines: 1 },
		isError: false,
		timestamp: 1767225603000,
		futureResultField: "kept",
	});
	manager.appendCustomEntry("ext-state", { mode: "plan", counts: [0, -1, 1e21] });
	const customMessage = manager.appendCustomMessageEntry("note", "A custom note for the model.", true, { source: "ext" });
	manager.appendMessage({
		role: "bashExecution",
		command: "ls",
		output: "a.ts\n",
		exitCode: 0,
		cancelled: false,
		truncated: false,
		timestamp: 1767225604000,
	});
	manager.appendMessage({
		role: "custom",
		customType: "inline",
		content: [{ type: "text", text: "inline custom" }],
		display: false,
		timestamp: 1767225605000,
	});
	manager.appendMessage({ role: "futureRole", payload: { a: 1 }, timestamp: 1767225606000 });
	manager.appendLabelChange(user1, "checkpoint");
	manager.appendSessionInfo("Golden\r\nsession ");
	const usageEntry = manager.appendUsage("cache_warm", "anthropic", "claude-test", usage, "warm").id;
	manager.appendMessage({ role: "user", content: "go on", timestamp: 1767225607000 });
	manager.appendMessage({
		role: "assistant",
		content: [{ type: "text", text: "abandoned answer" }],
		api: "anthropic-messages",
		provider: "anthropic",
		model: "claude-test",
		usage,
		stopReason: "stop",
		timestamp: 1767225608000,
	});
	manager.branchWithSummary(usageEntry, "Tried another approach.", { files: ["a.ts"] }, false, usage);
	manager.appendMessage({ role: "user", content: "new direction", timestamp: 1767225609000 });
	manager.appendContextEdit(user1, { content: "edited first request" });
	manager.appendContextEdit(toolResult, { content: "edited tool output" });
	manager.appendCompaction("Compacted the start.", customMessage, 4321, { kind: "structured" }, false, usage);
	manager.appendMessage({ role: "user", content: "after compaction", timestamp: 1767225610000 });
	manager.appendMessage({
		role: "assistant",
		content: [{ type: "text", text: "done" }],
		api: "openai-responses",
		provider: "openai",
		model: "gpt-test",
		usage,
		stopReason: "stop",
		timestamp: 1767225611000,
	});
	manager.appendThinkingLevelChange("low");
	manager.appendLabelChange(assistant1, "answer");
	manager.appendLabelChange(assistant1, undefined);
	const file = manager.getSessionFile();
	// A line a later Pi version might write: an entry kind v1.1.0 does not know.
	appendFileSync(
		file,
		`${JSON.stringify({
			type: "future_entry",
			id: "f0000001",
			parentId: manager.getLeafId(),
			timestamp: new Date().toISOString(),
			payload: { x: 1 },
		})}\n`,
	);
	copyFileSync(file, join(here, "pi-v3.jsonl"));
}

const header = (fields) => JSON.stringify({ type: "session", ...fields });
const lines = (...values) => `${values.map((value) => (typeof value === "string" ? value : JSON.stringify(value))).join("\n")}\n`;

function writeLegacyInputs() {
	writeFileSync(
		join(here, "pi-v1.jsonl"),
		lines(
			header({ id: "legacy-v1", timestamp: "2025-12-09T00:53:29.825Z", cwd: CWD, provider: "anthropic", modelId: "claude-opus-4-5", thinkingLevel: "off" }),
			{ type: "message", timestamp: "2025-12-09T00:53:30.000Z", message: { role: "user", content: [{ type: "text", text: "old question" }], timestamp: 1765241610000 } },
			{ type: "message", timestamp: "2025-12-09T00:53:31.000Z", message: { role: "assistant", content: [{ type: "text", text: "old answer" }], api: "anthropic-messages", provider: "anthropic", model: "claude-opus-4-5", usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: "stop", timestamp: 1765241611000 } },
			{ type: "thinking_level_change", timestamp: "2025-12-09T00:53:32.000Z", thinkingLevel: "medium" },
			{ type: "message", timestamp: "2025-12-09T00:53:33.000Z", message: { role: "user", content: "kept question", timestamp: 1765241613000 } },
			{ type: "compaction", timestamp: "2025-12-09T00:53:34.000Z", summary: "The old exchange.", firstKeptEntryIndex: 4, tokensBefore: 50000 },
			{ type: "message", timestamp: "2025-12-09T00:53:35.000Z", message: { role: "hookMessage", customType: "hook", content: "from a hook", display: true, timestamp: 1765241615000 } },
			"not json at all",
			{ type: "message", timestamp: "2025-12-09T00:53:36.000Z", message: { role: "user", content: "latest", timestamp: 1765241616000 } },
		),
	);
	writeFileSync(
		join(here, "pi-v2.jsonl"),
		lines(
			header({ version: 2, id: "legacy-v2", timestamp: "2026-01-01T00:00:00.000Z", cwd: CWD }),
			{ type: "message", id: "a1b2c3d4", parentId: null, timestamp: "2026-01-01T00:00:01.000Z", message: { role: "user", content: "hello", timestamp: 1767225601000 } },
			{ type: "message", id: "b2c3d4e5", parentId: "a1b2c3d4", timestamp: "2026-01-01T00:00:02.000Z", message: { role: "hookMessage", customType: "x", content: [{ type: "text", text: "hook text" }], display: false, details: { n: 1 }, timestamp: 1767225602000 } },
			{ type: "label", id: "c3d4e5f6", parentId: "b2c3d4e5", timestamp: "2026-01-01T00:00:03.000Z", targetId: "a1b2c3d4", label: "start" },
		),
	);
	// A writer killed mid-line: the last line is a torn fragment, unterminated.
	writeFileSync(
		join(here, "pi-torn.jsonl"),
		`${lines(
			header({ version: 3, id: "torn", timestamp: "2026-01-01T00:00:00.000Z", cwd: CWD }),
			{ type: "message", id: "d4e5f6a7", parentId: null, timestamp: "2026-01-01T00:00:01.000Z", message: { role: "user", content: "survives", timestamp: 1767225601000 } },
		)}{"type":"message","id":"e5f6a7b8","parentId":"d4e5f6a7","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assis`,
	);
}

async function recordPiReading(name) {
	const dir = mkdtempSync(join(work, `${name}-`));
	const copy = join(dir, `${name}.jsonl`);
	copyFileSync(join(here, `${name}.jsonl`), copy);
	const manager = SessionManager.open(copy, dir);
	writeFileSync(join(here, `${name}.pi.json`), `${JSON.stringify(await summarize(SessionManager, manager, CWD))}\n`);
	writeFileSync(join(here, `${name}.pi-after-open.jsonl`), readFileSync(copy));
}

try {
	writePiSession();
	writeLegacyInputs();
	for (const name of ["pi-v3", "pi-v2", "pi-v1", "pi-torn"]) await recordPiReading(name);
} finally {
	rmSync(work, { recursive: true, force: true });
}
