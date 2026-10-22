// Record Pi v1.1.0 (abe508e1b89912adde45528136c3221eb69acdd7) as the oracle
// for `tests/bash.rs`. Run it from the coding-agent package of that checkout
// with its dependencies installed, so tsx resolves Pi's sources:
//
//   cd <pi>/packages/coding-agent
//   npx tsx <path to this file> > <path to golden.json>
//
// It records the bash tool's definition and prompt text, then runs Pi's
// `createBashTool` over scripted `BashOperations` (no process starts) and
// over Pi's local operations only where they fail before spawning. Spill
// paths are replaced by `<FULL_OUTPUT_PATH>`, the wall time is dropped, and a
// string over 4096 code points is kept as its UTF-8 length and its first and
// last 256 code points; everything else is what Pi returned, threw, or sent
// as an update.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.cwd();
const revision = "abe508e1b89912adde45528136c3221eb69acdd7";
if (execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim() !== revision) {
	throw new Error("Run from packages/coding-agent of Pi v1.1.0 at the recorded revision");
}
const load = (path: string) => import(pathToFileURL(join(root, "src", path)).href);
const { bashToolSystemPromptContribution, createBashTool, createBashToolDefinition } = await load(
	"core/tools/bash.ts",
);
const { getShellConfig } = await load("utils/shell.ts");

type Chunk =
	| { text: string }
	| { bytes: number[] }
	| { lines: { from: number; to: number; prefix: string; pad: number } }
	| { repeat: { text: string; count: number } };

/** Expands a chunk spec into the chunks the scripted operation emits; `tests/bash.rs` mirrors it. */
function expand(spec: Chunk): Buffer[] {
	if ("text" in spec) return [Buffer.from(spec.text, "utf-8")];
	if ("bytes" in spec) return [Buffer.from(spec.bytes)];
	if ("repeat" in spec) return [Buffer.from(spec.repeat.text.repeat(spec.repeat.count), "utf-8")];
	const out: Buffer[] = [];
	for (let i = spec.lines.from; i <= spec.lines.to; i++) {
		out.push(Buffer.from(`${spec.lines.prefix}${String(i).padStart(spec.lines.pad, "0")}\n`, "utf-8"));
	}
	return out;
}

interface Scenario {
	name: string;
	chunks: Chunk[];
	/** `exitCode` returns it (null included); `throw` throws an Error with that message. */
	outcome: { exitCode: number | null } | { throw: string };
}

const scenarios: Scenario[] = [
	{ name: "simple", chunks: [{ text: "test output\n" }], outcome: { exitCode: 0 } },
	{ name: "exit-3", chunks: [{ text: "out\n" }], outcome: { exitCode: 3 } },
	{ name: "empty", chunks: [], outcome: { exitCode: 0 } },
	{ name: "empty-exit-1", chunks: [], outcome: { exitCode: 1 } },
	{ name: "null-exit", chunks: [{ text: "partial\n" }], outcome: { exitCode: null } },
	{ name: "null-exit-empty", chunks: [], outcome: { exitCode: null } },
	{ name: "signal-exit-137", chunks: [{ text: "before-kill\n" }], outcome: { exitCode: 137 } },
	{
		name: "lines-4000",
		chunks: [{ lines: { from: 1, to: 4000, prefix: "line-", pad: 4 } }],
		outcome: { exitCode: 0 },
	},
	{
		name: "lines-3000-exit-2",
		chunks: [{ lines: { from: 1, to: 3000, prefix: "", pad: 0 } }],
		outcome: { exitCode: 2 },
	},
	{
		name: "bytes-100-lines",
		chunks: [{ lines: { from: 1, to: 100, prefix: "x".repeat(1000), pad: 3 } }],
		outcome: { exitCode: 0 },
	},
	{ name: "last-line-partial", chunks: [{ repeat: { text: "y", count: 60000 } }], outcome: { exitCode: 0 } },
	{
		name: "last-line-partial-multibyte",
		chunks: [{ text: "head\n" }, { repeat: { text: "€", count: 30000 } }],
		outcome: { exitCode: 0 },
	},
	{
		name: "timeout-5-truncated",
		chunks: [{ lines: { from: 1, to: 3000, prefix: "", pad: 0 } }],
		outcome: { throw: "timeout:5" },
	},
	{
		name: "aborted-truncated",
		chunks: [{ lines: { from: 1, to: 3000, prefix: "", pad: 0 } }],
		outcome: { throw: "aborted" },
	},
	{ name: "timeout-0.05-empty", chunks: [], outcome: { throw: "timeout:0.05" } },
	{ name: "timeout-with-output", chunks: [{ text: "tick\n" }], outcome: { throw: "timeout:2" } },
	{ name: "aborted-empty", chunks: [], outcome: { throw: "aborted" } },
	{ name: "aborted-with-output", chunks: [{ text: "partial" }], outcome: { throw: "aborted" } },
	{ name: "other-error", chunks: [{ text: "ignored\n" }], outcome: { throw: "boom" } },
	{ name: "non-utf8", chunks: [{ bytes: [0xff, 0xfe, 0x20, 0x61, 0x62, 0x63, 0x0a] }], outcome: { exitCode: 0 } },
	{ name: "split-utf8", chunks: [{ bytes: [0xe2] }, { bytes: [0x82, 0xac, 0x0a] }], outcome: { exitCode: 0 } },
	{ name: "unfinished-utf8", chunks: [{ text: "a" }, { bytes: [0xe2, 0x82] }], outcome: { exitCode: 0 } },
	{ name: "bom", chunks: [{ bytes: [0xef, 0xbb, 0xbf, 0x68, 0x69, 0x0a] }], outcome: { exitCode: 0 } },
	{ name: "ansi-and-crlf", chunks: [{ text: "\u001b[31mred\u001b[0m\r\n" }], outcome: { exitCode: 0 } },
	{ name: "no-trailing-newline", chunks: [{ text: "a\nb" }], outcome: { exitCode: 0 } },
	{
		name: "structured-over-1mib",
		chunks: [{ lines: { from: 1, to: 120, prefix: "z".repeat(10000), pad: 3 } }],
		outcome: { exitCode: 0 },
	},
];

const PATH_TOKEN = "<FULL_OUTPUT_PATH>";
/** Strings longer than this many code points are kept as their UTF-8 length, head, and tail. */
const LONG = 4096;
const EDGE = 256;

function abbreviate(text: string): unknown {
	const points = [...text];
	if (points.length <= LONG) return text;
	return {
		$long: {
			bytes: Buffer.byteLength(text, "utf-8"),
			head: points.slice(0, EDGE).join(""),
			tail: points.slice(-EDGE).join(""),
		},
	};
}

function normalize(value: unknown, path: string | undefined): unknown {
	if (typeof value === "string") return abbreviate(path ? value.split(path).join(PATH_TOKEN) : value);
	if (Array.isArray(value)) return value.map((item) => normalize(item, path));
	if (value && typeof value === "object") {
		const out: Record<string, unknown> = {};
		for (const [key, item] of Object.entries(value)) {
			if (key === "wall_time_seconds") continue;
			if (item !== undefined) out[key] = normalize(item, path);
		}
		return out;
	}
	return value;
}

const pathIn = (text: string) => text.match(/Full output: ([^\]\n]+)/)?.[1];

async function record(scenario: Scenario) {
	const operations = {
		exec: async (_command: string, _cwd: string, { onData }: { onData: (data: Buffer) => void }) => {
			for (const spec of scenario.chunks) for (const chunk of expand(spec)) onData(chunk);
			if ("throw" in scenario.outcome) throw new Error(scenario.outcome.throw);
			return { exitCode: scenario.outcome.exitCode };
		},
	};
	const tool = createBashTool(root, { operations });
	const updates: unknown[] = [];
	const raw: unknown[] = [];
	let result: any;
	let error: string | undefined;
	try {
		result = await tool.execute("call", { command: "scripted" }, undefined, (update: unknown) => {
			raw.push(update);
		});
	} catch (err) {
		error = (err as Error).message;
	}
	const path: string | undefined = result?.details?.fullOutputPath ?? (error ? pathIn(error) : undefined);
	for (const update of raw) updates.push(normalize(JSON.parse(JSON.stringify(update)), path));
	const spilled = path ? readFileSync(path) : undefined;
	return {
		...scenario,
		...(error === undefined
			? { result: normalize(JSON.parse(JSON.stringify(result)), path) }
			: { error: normalize(error, path) }),
		updates,
		spillBytes: spilled ? spilled.length : null,
		spillExists: path ? existsSync(path) : null,
	};
}

async function localFailure(name: string, cwd: string, params: Record<string, unknown>, options = {}) {
	const tool = createBashTool(cwd, options);
	try {
		await tool.execute("call", params);
		throw new Error(`${name}: expected a failure`);
	} catch (err) {
		// The package directory stands in for any existing directory.
		return { name, cwd: cwd === root ? null : cwd, params, options, error: (err as Error).message };
	}
}

function noBashOnWindows() {
	const platform = Object.getOwnPropertyDescriptor(process, "platform")!;
	const saved = { pf: process.env.ProgramFiles, pf86: process.env["ProgramFiles(x86)"], path: process.env.PATH };
	try {
		Object.defineProperty(process, "platform", { configurable: true, value: "win32" });
		process.env.ProgramFiles = "C:\\Bake Test Missing\\Program Files";
		process.env["ProgramFiles(x86)"] = "C:\\Bake Test Missing\\Program Files (x86)";
		process.env.PATH = "";
		getShellConfig();
		throw new Error("expected no bash");
	} catch (err) {
		return {
			programFiles: "C:\\Bake Test Missing\\Program Files",
			programFilesX86: "C:\\Bake Test Missing\\Program Files (x86)",
			error: (err as Error).message,
		};
	} finally {
		Object.defineProperty(process, "platform", platform);
		process.env.ProgramFiles = saved.pf;
		process.env["ProgramFiles(x86)"] = saved.pf86;
		process.env.PATH = saved.path;
	}
}

const definition = createBashToolDefinition("/workspace");
const tool = createBashTool("/workspace");
const hidden = createBashToolDefinition("/workspace", { exposeSessionEnvironment: false });
const missing = "/this/directory/definitely/does/not/exist/12345";
const golden = {
	pi: { version: "1.1.0", revision },
	definition: {
		name: tool.name,
		label: tool.label,
		description: tool.description,
		parameters: tool.parameters,
		outputSchema: tool.outputSchema,
		constrainedSampling: tool.constrainedSampling,
		promptSnippet: tool.promptSnippet,
		promptGuidelines: tool.promptGuidelines,
		promptGuidelinesWithoutSessionEnvironment: hidden.promptGuidelines ?? null,
		contribution: bashToolSystemPromptContribution,
		parametersJson: JSON.stringify(definition.parameters),
		outputSchemaJson: JSON.stringify(definition.outputSchema),
	},
	scenarios: [] as unknown[],
	local: [
		await localFailure("timeout-zero", root, { command: "echo never", timeout: 0 }),
		await localFailure("timeout-negative", root, { command: "echo never", timeout: -1 }),
		await localFailure("timeout-over-max", root, { command: "echo never", timeout: 2147484 }),
		await localFailure("missing-cwd", missing, { command: "echo test" }),
		await localFailure("missing-custom-shell", root, { command: "echo test" }, { shellPath: "/custom/bash" }),
	],
	windowsNoBash: noBashOnWindows(),
};
for (const scenario of scenarios) golden.scenarios.push(await record(scenario));
process.stdout.write(`${JSON.stringify(golden, null, "\t")}\n`);
