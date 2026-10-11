// Record Pi v1.1.0 (abe508e1b89912adde45528136c3221eb69acdd7) compaction as the oracle:
// the summarization prompts and request options, the summaries `compact()` and
// `generateBranchSummary()` build, the serialized conversation, token estimates,
// cut points, `prepareCompaction()`, and the summary messages `convertToLlm()` sends.
//
//   cd <pi checkout at the revision above>
//   node --import "file://$PWD/packages/coding-agent/src/experimental/source-resolver.ts" \
//     --import "file://<this dir>/catalog-stub.mjs" <this dir>/generate.mjs "$PWD"
//
// The stub stands in for the provider catalogs a Pi build generates; these
// functions never read them. Output: golden.json beside this script.
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(process.argv[2]);
const revision = "abe508e1b89912adde45528136c3221eb69acdd7";
if (execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim() !== revision) {
	throw new Error("Use Pi v1.1.0 at the recorded revision");
}
const here = dirname(fileURLToPath(import.meta.url));
const load = (path) => import(pathToFileURL(`${root}/packages/coding-agent/src/${path}`));
const compaction = await load("core/compaction/index.ts");
const { convertToLlm } = await load("core/messages.ts");
const { buildSessionProjection } = await load("core/session-manager.ts");
const { createAssistantMessageEventStream, getSystemMessageText, contentText } = await import("@earendil-works/pi-ai");

const usage = (input, output) => ({
	input,
	output,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: input + output,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
});
const assistant = (content, extra = {}) => ({
	role: "assistant",
	content,
	api: "faux",
	provider: "faux",
	model: "faux-1",
	usage: usage(0, 0),
	stopReason: "stop",
	timestamp: 10,
	...extra,
});

// Every message kind, with JavaScript-specific shapes: array-index argument keys,
// UTF-16 lengths, images, and tool output over the 2000-character cap.
const messages = [
	{
		role: "system",
		content: "",
		sections: { preamble: "You are pi.", cwd: "Current working directory: /p" },
		toolsAdded: [{ name: "read", description: "Read a file", parameters: { type: "object" } }],
		timestamp: 1,
	},
	{ role: "user", content: "Fix the bug in src/app.ts 🍞", timestamp: 2 },
	{
		role: "user",
		content: [
			{ type: "text", text: "look" },
			{ type: "image", data: "AAAA", mimeType: "image/png" },
			{ type: "text", text: "here" },
		],
		timestamp: 3,
	},
	assistant(
		[
			{ type: "thinking", thinking: "I should read the file." },
			{ type: "thinking", thinking: "And then edit it." },
			{ type: "text", text: "Reading." },
			{ type: "toolCall", id: "c1", name: "read", arguments: { path: "src/app.ts", 1: "x", limit: 10, nested: { b: 1, a: [1.5, "é", null] } } },
			{ type: "toolCall", id: "c2", name: "edit", arguments: { path: "src/app.ts", edits: [] } },
			{ type: "toolCall", id: "c3", name: "write", arguments: { path: "new.ts", content: "x" } },
			{ type: "toolCall", id: "c4", name: "read", arguments: { path: "z/ü.md" } },
			{ type: "toolCall", id: "c5", name: "bash", arguments: { command: "ls" } },
		],
		{ stopReason: "toolUse", usage: usage(1200, 80) },
	),
	{
		role: "toolResult",
		toolCallId: "c1",
		toolName: "read",
		content: [{ type: "text", text: `🍞${"line of code\n".repeat(200)}` }],
		isError: false,
		timestamp: 11,
	},
	{
		role: "toolResult",
		toolCallId: "c2",
		toolName: "edit",
		content: [
			{ type: "text", text: "ok" },
			{ type: "image", data: "BBBB", mimeType: "image/png" },
		],
		isError: false,
		timestamp: 12,
	},
	{ role: "toolResult", toolCallId: "c3", toolName: "write", content: [], isError: true, timestamp: 13 },
	assistant([
		{ type: "text", text: "First part." },
		{ type: "text", text: "Second part." },
	]),
	assistant([{ type: "toolCall", id: "c6", name: "codemode", arguments: {} }], { stopReason: "toolUse" }),
	{
		role: "toolResult",
		toolCallId: "c6",
		toolName: "codemode",
		content: [{ type: "text", text: "done" }],
		isError: false,
		timestamp: 14,
		nestedCalls: {
			calls: [
				{ id: "c6/1", name: "read", arguments: { path: "nested-read.ts" }, status: "ok" },
				{ id: "c6/2", name: "edit", arguments: { path: "nested-edit.ts", edits: [] }, status: "ok" },
				{ id: "c6/3", name: "write", argumentsBytes: 40000, status: "ok" },
			],
			complete: false,
		},
	},
	{ role: "custom", customType: "note", content: "remember the deadline", display: true, timestamp: 15 },
	{ role: "custom", customType: "note", content: [{ type: "text", text: "block note" }], display: false, timestamp: 16 },
	{ role: "bashExecution", command: "ls -la", output: "a\nb", exitCode: 1, cancelled: false, truncated: false, timestamp: 17 },
	{ role: "bashExecution", command: "secret", output: "x", cancelled: false, truncated: false, timestamp: 18, excludeFromContext: true },
	{ role: "branchSummary", summary: "explored another fix", fromId: "b1", timestamp: 19 },
	{ role: "compactionSummary", summary: "earlier work", tokensBefore: 5000, timestamp: 20 },
	assistant([{ type: "text", text: "" }], { stopReason: "error", errorMessage: "overloaded" }),
	{ role: "user", content: [], timestamp: 21 },
];

const model = {
	id: "faux-1",
	name: "Faux",
	api: "faux",
	provider: "faux",
	baseUrl: "http://localhost",
	reasoning: true,
	input: ["text", "image"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 50000,
	maxTokens: 3000,
};

// Records each summarization request and answers with the next text.
const requests = [];
const answers = [];
const streamFn = (requestModel, context, options) => {
	const transcript = context.messages;
	requests.push({
		model: requestModel.id,
		systemPrompt: getSystemMessageText(transcript[0]),
		roles: transcript.map((message) => message.role),
		prompt: contentText(transcript[1].content),
		maxTokens: options.maxTokens,
		reasoning: options.reasoning ?? null,
		cacheRetention: options.cacheRetention,
		sessionIdSupplied: options.sessionId === "routing",
	});
	const stream = createAssistantMessageEventStream();
	const text = answers.shift() ?? "## Goal\nsummary";
	queueMicrotask(() => {
		const message = assistant([{ type: "text", text }], { usage: usage(30, 7) });
		stream.push({ type: "done", reason: "stop", message });
	});
	return stream;
};
const take = () => requests.splice(0);

// Session entries: a previous compaction, then a long split turn.
let counter = 0;
let parent = null;
const entry = (fields) => {
	const id = `e${counter++}`;
	const made = { id, parentId: parent, timestamp: `2025-01-01T00:00:${String(counter).padStart(2, "0")}.000Z`, ...fields };
	parent = id;
	return made;
};
const message = (msg) => entry({ type: "message", message: msg });
const entries = [];
entries.push(message({ role: "user", content: "old request ".repeat(30), timestamp: 100 }));
entries.push(message(assistant([{ type: "text", text: "old answer ".repeat(30) }])));
const keptFrom = message({ role: "user", content: "kept request ".repeat(40), timestamp: 101 });
entries.push(keptFrom);
entries.push(message(assistant([{ type: "toolCall", id: "k1", name: "read", arguments: { path: "kept.ts" } }], { stopReason: "toolUse" })));
entries.push(message({ role: "toolResult", toolCallId: "k1", toolName: "read", content: [{ type: "text", text: "kept file ".repeat(50) }], isError: false, timestamp: 102 }));
entries.push(entry({ type: "compaction", summary: "First summary", firstKeptEntryId: keptFrom.id, tokensBefore: 9000, details: { readFiles: ["prev-read.ts", "kept.ts"], modifiedFiles: ["prev-mod.ts"] } }));
entries.push(entry({ type: "model_change", provider: "faux", modelId: "faux-1" }));
entries.push(message({ role: "user", content: "new request ".repeat(40), timestamp: 103 }));
entries.push(message(assistant([{ type: "text", text: "partial ".repeat(40) }, { type: "toolCall", id: "n1", name: "edit", arguments: { path: "edited.ts", edits: [] } }], { stopReason: "toolUse", usage: usage(4000, 100) })));
entries.push(message({ role: "toolResult", toolCallId: "n1", toolName: "edit", content: [{ type: "text", text: "edited ".repeat(80) }], isError: false, timestamp: 104 }));
entries.push(entry({ type: "custom_message", customType: "note", content: "custom context ".repeat(20), display: true }));
entries.push(message(assistant([{ type: "toolCall", id: "n2", name: "write", arguments: { path: "written.ts", content: "y" } }], { stopReason: "toolUse" })));
entries.push(message({ role: "toolResult", toolCallId: "n2", toolName: "write", content: [{ type: "text", text: "written ".repeat(80) }], isError: false, timestamp: 105 }));
entries.push(message(assistant([{ type: "text", text: "final answer ".repeat(30) }], { usage: usage(6000, 200) })));

const cutPoints = [0, 1, 50, 200, 400, 1000, 100000].map((keep) => ({
	keep,
	result: compaction.findCutPoint(entries, 0, entries.length, keep),
}));
const preparations = [];
for (const keep of [1, 150, 300, 600, 100000]) {
	const prep = compaction.prepareCompaction(entries, { enabled: true, reserveTokens: 2000, keepRecentTokens: keep });
	preparations.push({
		keep,
		preparation: prep && {
			firstKeptEntryId: prep.firstKeptEntryId,
			messagesToSummarize: prep.messagesToSummarize,
			turnPrefixMessages: prep.turnPrefixMessages,
			isSplitTurn: prep.isSplitTurn,
			tokensBefore: prep.tokensBefore,
			previousSummary: prep.previousSummary ?? null,
			fileOps: Object.fromEntries(Object.entries(prep.fileOps).map(([key, set]) => [key, [...set]])),
		},
	});
}

// Summaries: plain, update with focus, and split-turn compaction.
const summaries = [];
for (const [previousSummary, customInstructions, thinkingLevel] of [
	[undefined, undefined, "high"],
	["Earlier summary", "the parser", "off"],
]) {
	const result = await compaction.generateSummaryWithUsage(
		messages, model, 20000, undefined, undefined, undefined, customInstructions, previousSummary, thinkingLevel, streamFn,
	);
	summaries.push({ previousSummary: previousSummary ?? null, customInstructions: customInstructions ?? null, thinkingLevel, result, requests: take() });
}
const compactions = [];
for (const keep of [300, 150]) {
	const prep = compaction.prepareCompaction(entries, { enabled: true, reserveTokens: 2000, keepRecentTokens: keep });
	answers.length = 0;
	answers.push("## Goal\nhistory summary", "## Original Request\nturn prefix");
	const result = await compaction.compact(prep, model, undefined, undefined, undefined, undefined, "medium", streamFn, undefined, undefined, undefined, "routing");
	compactions.push({ keep, result, requests: take() });
}

// Branch summaries over the same entries: default, appended focus, replaced prompt, and a budget.
const branch = [];
for (const options of [
	{},
	{ customInstructions: "the edits" },
	{ customInstructions: "Only list files.", replaceInstructions: true },
	{ reserveTokens: 49500 },
]) {
	answers.length = 0;
	answers.push("## Goal\nbranch");
	const result = await compaction.generateBranchSummary(entries, { model, signal: new AbortController().signal, streamFn, ...options });
	branch.push({ options, result, requests: take() });
}
const branchPreparations = [0, 300, 1000].map((budget) => {
	const prep = compaction.prepareBranchEntries(entries, budget);
	return { budget, messages: prep.messages, totalTokens: prep.totalTokens, fileOps: Object.fromEntries(Object.entries(prep.fileOps).map(([key, set]) => [key, [...set]])) };
});

// A context edit after the last usage: the estimate falls back to characters,
// counting the replayed system message once.
const edited = [];
parent = null;
const editedEntry = (fields) => {
	const made = entry(fields);
	edited.push(made);
	return made;
};
editedEntry({ type: "message", message: messages[0] });
editedEntry({ type: "message", message: { role: "system", content: "", sections: { cwd: "Current working directory: /q" }, timestamp: 2 } });
editedEntry({ type: "message", message: { role: "user", content: "question ".repeat(20), timestamp: 3 } });
editedEntry({ type: "message", message: assistant([{ type: "text", text: "answer" }], { usage: usage(900, 100) }) });
const target = editedEntry({ type: "message", message: { role: "user", content: "follow-up ".repeat(20), timestamp: 4 } });
editedEntry({ type: "context_edit", targetId: target.id, replacement: { content: "short" } });
const projectedEstimate = compaction.estimateProjectedContextTokens(buildSessionProjection(edited), edited);

const llm = convertToLlm(messages);
const golden = {
	model,
	messages,
	estimates: messages.map((msg) => compaction.estimateTokens(msg)),
	contextEstimate: compaction.estimateContextTokens(messages),
	serialized: compaction.serializeConversation(llm),
	converted: llm.map((msg) => ({ role: msg.role, text: msg.role === "user" ? contentText(msg.content, "\u0000") : null })),
	entries,
	cutPoints,
	preparations,
	summaries,
	compactions,
	branch,
	branchPreparations,
	edited,
	projectedEstimate,
};
writeFileSync(join(here, "golden.json"), `${JSON.stringify(golden, null, "\t")}\n`);
