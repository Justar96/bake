// Record Pi v1.1.0's file tools: their definitions, prompt contributions, and
// the results of running them on real files, plus the `rg --json` and `fd`
// output its grep and find tools parsed.
//
// Run from a Pi checkout at the recorded revision, with `rg` and `fd` on PATH:
//   cd <Pi checkout> && npx tsx <this file> <Pi checkout> > <this dir>/golden.json
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { crc32, deflateSync } from "node:zlib";

const root = resolve(process.argv[2] ?? ".");
const revision = "abe508e1b89912adde45528136c3221eb69acdd7";
if (execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim() !== revision) {
	throw new Error("Use Pi v1.1.0 at the recorded revision");
}
const src = `${root}/packages/coding-agent/src`;
const load = (path: string) => import(pathToFileURL(`${src}/${path}`).href);
const tools = await load("core/tools/index.ts");
const prompt = await load("core/system-prompt.ts");
const settings = await load("core/settings-manager.ts");
const read = await load("core/tools/read.ts");
const bash = await load("core/tools/bash.ts");
const edit = await load("core/tools/edit.ts");
const write = await load("core/tools/write.ts");
const grep = await load("core/tools/grep.ts");
const find = await load("core/tools/find.ts");
const ls = await load("core/tools/ls.ts");
const toolResultImages = await load("utils/tool-result-images.ts");
const mcp = await load("core/mcp-servers.ts");

const DIR = "<DIR>";

// ---------------------------------------------------------------- definitions

const definitionsByName = tools.createAllToolDefinitions("/workspace");
const definitions: Record<string, unknown> = {};
for (const name of ["read", "write", "edit", "ls", "find", "grep", "bash"]) {
	const d = definitionsByName[name];
	definitions[name] = {
		name: d.name,
		label: d.label,
		description: d.description,
		parameters: JSON.parse(JSON.stringify(d.parameters)),
		outputSchema: d.outputSchema === undefined ? null : JSON.parse(JSON.stringify(d.outputSchema)),
		constrainedSampling: d.constrainedSampling ?? null,
		promptSnippet: d.promptSnippet ?? null,
		promptGuidelines: d.promptGuidelines ?? null,
		hasPrepareArguments: typeof d.prepareArguments === "function",
		executionMode: d.executionMode ?? null,
	};
}
const contributions = {
	read: read.readToolSystemPromptContribution,
	bash: bash.bashToolSystemPromptContribution,
	edit: edit.editToolSystemPromptContribution,
	write: write.writeToolSystemPromptContribution,
	grep: grep.grepToolSystemPromptContribution,
	find: find.findToolSystemPromptContribution,
	ls: ls.lsToolSystemPromptContribution,
};

// The `tools` and `rules` sections Pi's session builds for a tool selection.
function promptSections(selected: string[]) {
	const toolSnippets: Record<string, string> = {};
	const toolGuidelines: Record<string, string[]> = {};
	for (const [name, d] of Object.entries<any>(definitionsByName)) {
		if (d.promptSnippet) toolSnippets[name] = d.promptSnippet;
		if (d.promptGuidelines?.length) toolGuidelines[name] = [...d.promptGuidelines];
	}
	const sections = prompt.buildSystemPromptSections({ cwd: "/work", selectedTools: selected, toolSnippets, toolGuidelines });
	return { selected, tools: sections.tools, rules: sections.rules };
}
const prompts = [
	["read", "bash", "edit", "write"],
	["read", "edit", "write"],
	["read", "grep", "find", "ls"],
	["read", "bash", "edit", "write", "grep", "find", "ls"],
	["bash"],
	[],
].map(promptSections);

const prepareArguments = [
	{ path: "file.txt", oldText: "before", newText: "after" },
	{ path: "file.txt", edits: [{ oldText: "a", newText: "b" }], oldText: "c", newText: "d" },
	{ path: "file.txt", edits: [{ oldText: "a", newText: "b" }] },
	{ path: "file.txt", edits: JSON.stringify([{ oldText: "a", newText: "b" }]) },
	{ path: "file.txt", edits: JSON.stringify({ oldText: "a", newText: "b" }) },
	{ path: "file.txt", edits: "not json" },
	{ path: "file.txt", edits: { oldText: "a", newText: "b" } },
	{ path: "file.txt", oldText: "x" },
	{ path: "file.txt", edits: 5, oldText: "c", newText: "d" },
	null,
	"garbage",
	[1, 2],
].map((input) => ({ input, output: definitionsByName.edit.prepareArguments(structuredClone(input)) ?? null }));

// Pi's tool selection: `getToolListError`, `applyToolModifiers`, the `defaultTools`
// resolution, and `createToolNameMatcher`.
const toolSelection = {
	listErrors: [["read", "grep"], ["+grep", "-write"], ["read", "+grep"], ["-gr*"], ["mcp__*"], []].map((entries) => ({
		entries,
		error: settings.getToolListError(entries) ?? null,
	})),
	modifiers: [
		[["read", "bash"], ["+grep", "-bash", "+grep", "-missing", "+"]],
		[[], ["-read", "+read"]],
	].map(([base, entries]) => ({ base, entries, result: settings.applyToolModifiers(base, entries) })),
	defaults: [[], ["grep", "find"], ["+grep", "-write"], ["ls", "+grep", "-ls"], ["-read"]].map((entries) => ({
		entries,
		result: settings.SettingsManager.inMemory({ defaultTools: entries }).getDefaultTools() ?? null,
	})),
	matchers: [
		[["read", "gr*"], ["read", "grep", "green", "bash", "gr"]],
		[["*"], ["read", "x"]],
		[["a.b*", "(x)"], ["a.bc", "axbc", "(x)"]],
	].map(([entries, names]) => {
		const matcher = mcp.createToolNameMatcher(entries);
		return { entries, names, matches: names.map((name: string) => matcher(name)) };
	}),
};

// ---------------------------------------------------------------- execution

const base = realpathSync(mkdtempSync(join(tmpdir(), "pi-tools-golden-")));
let counter = 0;

// A file's text is a string or a list of parts: strings, ["repeat", text, count], and
// ["lines", count, parts, separator?], whose parts may contain "{i}" (1-based) or
// ["pad", width] (the line number padded with zeros). `series` makes `count` empty
// files named by `name(i)` for i from 0.
type Part = string | ["repeat", string, number] | ["lines", number, Part[], string?] | ["pad", number];
type FileSpec = {
	path: string;
	text?: string | Part[];
	base64?: string;
	mode?: number;
	dir?: boolean;
	symlink?: string;
	series?: { count: number; pad: number; suffix: Part[] };
};

function materialize(parts: string | Part[], i = 0): string {
	if (typeof parts === "string") return parts.split("{i}").join(String(i));
	return parts
		.map((part) => {
			if (typeof part === "string") return part.split("{i}").join(String(i));
			if (part[0] === "repeat") return part[1].repeat(part[2]);
			if (part[0] === "pad") return String(i).padStart(part[1], "0");
			const [, count, inner, separator] = part;
			return Array.from({ length: count }, (_, k) => materialize(inner, k + 1)).join(separator ?? "\n");
		})
		.join("");
}

function setup(files: FileSpec[]): string {
	const dir = join(base, `case-${counter++}`);
	mkdirSync(dir, { recursive: true });
	for (const file of files) {
		const path = join(dir, file.path);
		if (file.dir) {
			mkdirSync(path, { recursive: true });
			continue;
		}
		mkdirSync(dirname(path), { recursive: true });
		if (file.series) {
			for (let i = 0; i < file.series.count; i++) {
				writeFileSync(join(path, `${String(i).padStart(file.series.pad, "0")}${materialize(file.series.suffix)}`), "");
			}
			continue;
		}
		if (file.symlink !== undefined) {
			symlinkSync(file.symlink, path);
			continue;
		}
		writeFileSync(path, file.base64 !== undefined ? Buffer.from(file.base64, "base64") : materialize(file.text ?? ""));
		if (file.mode !== undefined) chmodSync(path, file.mode);
	}
	return dir;
}

// FNV-1a over UTF-8 bytes, 64-bit.
function fnv1a64(text: string): string {
	let hash = 0xcbf29ce484222325n;
	for (const byte of Buffer.from(text, "utf8")) {
		hash ^= BigInt(byte);
		hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
	}
	return hash.toString(16).padStart(16, "0");
}

// Strings longer than 4 KiB are recorded as their hash, UTF-8 length, and ends.
function compact(value: unknown): unknown {
	if (typeof value === "string" && value.length > 4096) {
		return { $hash: fnv1a64(value), bytes: Buffer.byteLength(value, "utf8"), head: value.slice(0, 120), tail: value.slice(-120) };
	}
	if (Array.isArray(value)) return value.map(compact);
	if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, compact(v)]));
	return value;
}

function scrub(value: unknown, dir: string): unknown {
	return compact(JSON.parse(JSON.stringify(value ?? null).split(dir).join(DIR)));
}

function substitute(value: unknown, dir: string): any {
	return JSON.parse(JSON.stringify(value ?? null).split(DIR).join(dir));
}

// Files a write or edit leaves behind, as base64.
function snapshot(dir: string, tool: string, args: any) {
	if (tool !== "write" && tool !== "edit") return undefined;
	const path = substitute(args.path, dir);
	try {
		return compact(readFileSync(path.startsWith("/") ? path : join(dir, path)).toString("base64"));
	} catch {
		return null;
	}
}

async function run(tool: string, name: string, files: FileSpec[], args: unknown, extra: Record<string, unknown> = {}) {
	const dir = setup(files);
	const definition = tools.createAllToolDefinitions(dir)[tool];
	const ctx = extra.model ? { cwd: dir, model: extra.model } : undefined;
	const record: Record<string, unknown> = { tool, name, files, args, ...extra };
	try {
		const result = await definition.execute(`call-${name}`, substitute(args, dir), undefined, undefined, ctx);
		record.result = scrub(
			{ content: result.content, details: result.details ?? null, structuredContent: result.structuredContent ?? null },
			dir,
		);
	} catch (error: any) {
		record.error = scrub(error?.message ?? String(error), dir);
	}
	record.after = snapshot(dir, tool, args) ?? null;
	return record;
}

function pngChunk(type: string, body: Buffer): Buffer {
	const header = Buffer.alloc(8);
	header.writeUInt32BE(body.length, 0);
	header.write(type, 4, "ascii");
	const checksum = Buffer.alloc(4);
	checksum.writeUInt32BE(crc32(Buffer.concat([header.subarray(4), body])), 0);
	return Buffer.concat([header, body, checksum]);
}

function createPng(width: number, height: number): Buffer {
	const ihdr = Buffer.alloc(13);
	ihdr.writeUInt32BE(width, 0);
	ihdr.writeUInt32BE(height, 4);
	ihdr[8] = 8;
	ihdr[9] = 0;
	const raw = Buffer.alloc((width + 1) * height);
	for (let row = 0; row < height; row++) raw.fill(row % 256, row * (width + 1) + 1, (row + 1) * (width + 1));
	return Buffer.concat([
		Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
		pngChunk("IHDR", ihdr),
		pngChunk("IDAT", deflateSync(raw)),
		pngChunk("IEND", Buffer.alloc(0)),
	]);
}

function createTinyBmp(): Buffer {
	const buffer = Buffer.alloc(58);
	buffer.write("BM", 0, "ascii");
	buffer.writeUInt32LE(buffer.length, 2);
	buffer.writeUInt32LE(54, 10);
	buffer.writeUInt32LE(40, 14);
	buffer.writeInt32LE(1, 18);
	buffer.writeInt32LE(1, 22);
	buffer.writeUInt16LE(1, 26);
	buffer.writeUInt16LE(24, 28);
	buffer.writeUInt32LE(0, 30);
	buffer.writeUInt32LE(4, 34);
	buffer[56] = 0xff;
	return buffer;
}

const PNG_1X1 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==";
const GIF_1X1 = "R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==";
const WEBP_1X1 = "UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA";
const JPEG_2X1 =
	"/9j/4AAQSkZJRgABAgAAAQABAAD/wAARCAABAAIDAREAAhEBAxEB/9sAQwADAgIDAgIDAwMDBAMDBAUIBQUEBAUKBwcGCAwKDAwLCgsLDQ4SEA0OEQ4LCxAWEBETFBUVFQwPFxgWFBgSFBUU/9sAQwEDBAQFBAUJBQUJFA0LDRQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD4H8Q/8h/Uv+vmX/0M1/o1wJ/ySWU/9g1D/wBNRMOM/wDkp8z/AOv9b/05I//Z";
function app1(payload: Buffer): Buffer {
	const segment = Buffer.alloc(payload.length + 4);
	segment[0] = 0xff;
	segment[1] = 0xe1;
	segment.writeUInt16BE(payload.length + 2, 2);
	payload.copy(segment, 4);
	return segment;
}
const jpeg = Buffer.from(JPEG_2X1, "base64");
const JPEG_ROTATED = Buffer.concat([
	jpeg.subarray(0, 2),
	app1(Buffer.from('http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x="adobe:ns:meta/"/>')),
	app1(Buffer.concat([Buffer.from("Exif\0\0"), Buffer.from("49492a0008000000010012010300010000000600000000000000", "hex")])),
	jpeg.subarray(2),
]).toString("base64");

const cases: unknown[] = [];
const add = async (...args: Parameters<typeof run>) => cases.push(await run(...args));

// read
const small = "Hello, world!\nLine 2\nLine 3";

await add("read", "small", [{ path: "test.txt", text: small }], { path: `${DIR}/test.txt` });
await add("read", "relative", [{ path: "sub/test.txt", text: small }], { path: "sub/test.txt" });
await add("read", "at-prefix", [{ path: "test.txt", text: small }], { path: "@test.txt" });
await add("read", "missing", [], { path: `${DIR}/nonexistent.txt` });
await add("read", "directory", [{ path: "sub", dir: true }], { path: "sub" });
await add("read", "line-limit", [{ path: "large.txt", text: [["lines", 2500, ["Line {i}"]]] }], { path: "large.txt" });
await add("read", "byte-limit", [{ path: "b.txt", text: [["lines", 500, ["Line {i}: ", ["repeat", "x", 200]]]] }], { path: "b.txt" });
await add("read", "offset", [{ path: "o.txt", text: [["lines", 100, ["Line {i}"]]] }], { path: "o.txt", offset: 51 });
await add("read", "limit", [{ path: "o.txt", text: [["lines", 100, ["Line {i}"]]] }], { path: "o.txt", limit: 10 });
await add("read", "offset-limit", [{ path: "o.txt", text: [["lines", 100, ["Line {i}"]]] }], { path: "o.txt", offset: 41, limit: 20 });
await add("read", "offset-beyond", [{ path: "s.txt", text: "Line 1\nLine 2\nLine 3" }], { path: "s.txt", offset: 100 });
await add("read", "offset-last-empty", [{ path: "s.txt", text: "a\nb\n" }], { path: "s.txt", offset: 3 });
await add("read", "offset-zero", [{ path: "s.txt", text: "a\nb" }], { path: "s.txt", offset: 0 });
await add("read", "offset-negative", [{ path: "s.txt", text: "a\nb" }], { path: "s.txt", offset: -4 });
await add("read", "limit-zero", [{ path: "s.txt", text: "a\nb\nc" }], { path: "s.txt", limit: 0 });
await add("read", "limit-negative", [{ path: "s.txt", text: [["lines", 10, ["L{i}"]]] }], { path: "s.txt", offset: 2, limit: -3 });
await add("read", "limit-negative-past-start", [{ path: "s.txt", text: [["lines", 10, ["L{i}"]]] }], { path: "s.txt", offset: 3, limit: -9 });
await add("read", "fractional", [{ path: "s.txt", text: [["lines", 10, ["L{i}"]]] }], { path: "s.txt", offset: 2.5, limit: 2.5 });
await add("read", "limit-past-end", [{ path: "s.txt", text: "a\nb" }], { path: "s.txt", limit: 1e300 });
await add("read", "trailing-newline", [{ path: "s.txt", text: "a\nb\n" }], { path: "s.txt" });
await add("read", "empty", [{ path: "e.txt", text: "" }], { path: "e.txt" });
await add("read", "crlf", [{ path: "c.txt", text: "a\r\nb\r\n" }], { path: "c.txt" });
await add("read", "invalid-utf8", [{ path: "u.txt", base64: Buffer.from([0x61, 0xff, 0x0a, 0xe2, 0x82, 0x0a, 0xf0, 0x9f, 0x98, 0x80, 0xc3]).toString("base64") }], { path: "u.txt" });
await add("read", "first-line-too-long", [{ path: "w.txt", text: [["repeat", "x", 61440], "\nshort"] }], { path: "w.txt" });
await add("read", "later-line-too-long", [{ path: "w.txt", text: ["short\n", ["repeat", "é", 40960], "\nend"] }], { path: "w.txt", offset: 2 });
await add("read", "long-line-after-short", [{ path: "w.txt", text: ["short\n", ["repeat", "y", 61440], "\nend"] }], { path: "w.txt" });
await add("read", "exact-lines", [{ path: "x.txt", text: [["lines", 2000, ["{i}"]], "\n"] }], { path: "x.txt" });
await add("read", "lines-and-trailing", [{ path: "x.txt", text: [["lines", 2001, ["{i}"]], "\n"] }], { path: "x.txt" });
await add("read", "limit-under-truncation", [{ path: "x.txt", text: [["lines", 3000, ["{i}"]]] }], { path: "x.txt", offset: 10, limit: 2500 });
await add("read", "bytes-with-limit", [{ path: "b.txt", text: [["lines", 500, ["Line {i}: ", ["repeat", "z", 300]]]] }], { path: "b.txt", offset: 7, limit: 400 });
await add("read", "png-magic", [{ path: "image.txt", base64: PNG_1X1 }], { path: "image.txt" });
await add("read", "png-non-vision", [{ path: "a.png", base64: PNG_1X1 }], { path: "a.png" }, { model: { input: ["text"] } });
await add("read", "gif", [{ path: "a.gif", base64: GIF_1X1 }], { path: "a.gif" });
await add("read", "webp", [{ path: "a.webp", base64: WEBP_1X1 }], { path: "a.webp" });
await add("read", "jpeg-oriented", [{ path: "a.jpg", base64: JPEG_ROTATED }], { path: "a.jpg" });
await add("read", "bmp", [{ path: "image.bmp", base64: createTinyBmp().toString("base64") }], { path: "image.bmp" }, { imageDataExact: false });
await add("read", "bmp-non-vision", [{ path: "image.bmp", base64: createTinyBmp().toString("base64") }], { path: "image.bmp" }, { imageDataExact: false, model: { input: ["text"] } });
await add("read", "png-resized", [{ path: "wide.png", base64: createPng(2400, 100).toString("base64") }], { path: "wide.png" }, { imageDataExact: false });
await add("read", "png-broken", [{ path: "broken.png", base64: Buffer.concat([createPng(4, 4).subarray(0, 33), Buffer.from("garbage")]).toString("base64") }], { path: "broken.png" });
await add("read", "not-an-image", [{ path: "not-an-image.png", text: "definitely not a png" }], { path: "not-an-image.png" });
await add("read", "curly-quote-variant", [{ path: "Capture d\u2019cran.txt", text: "curly" }], { path: "Capture d'cran.txt" });
await add("read", "am-pm-variant", [{ path: "Shot 10.00.00\u202Fam.png", text: "ampm" }], { path: "Shot 10.00.00 am.png" });
await add("read", "nfd-variant", [{ path: "cafe\u0301.txt", text: "nfd" }], { path: "caf\u00e9.txt" });
await add("read", "unicode-space", [{ path: "a b.txt", text: "space" }], { path: "a\u00A0b.txt" });

// write
await add("write", "write", [], { path: `${DIR}/write-test.txt`, content: "Test content" });
await add("write", "nested", [], { path: "nested/dir/test.txt", content: "Nested content" });
await add("write", "overwrite", [{ path: "f.txt", text: "old" }], { path: "f.txt", content: "new\r\n\u00e9" });
await add("write", "directory", [{ path: "sub", dir: true }], { path: "sub", content: "x" });

// edit
const e = (oldText: string, newText: string) => ({ oldText, newText });
await add("edit", "replace", [{ path: "edit-test.txt", text: "Hello, world!" }], { path: `${DIR}/edit-test.txt`, edits: [e("world", "testing")] });
await add("edit", "not-found", [{ path: "f.txt", text: "Hello, world!" }], { path: "f.txt", edits: [e("nonexistent", "testing")] });
await add("edit", "missing", [], { path: `${DIR}/missing.txt`, edits: [e("hello", "world")] });
await add("edit", "duplicates", [{ path: "f.txt", text: "foo foo foo" }], { path: "f.txt", edits: [e("foo", "bar")] });
await add("edit", "multi", [{ path: "f.txt", text: "alpha\nbeta\ngamma\ndelta\n" }], { path: "f.txt", edits: [e("alpha\n", "ALPHA\n"), e("gamma\n", "GAMMA\n")] });
await add("edit", "large-gap", [{ path: "f.txt", text: [["lines", 600, ["line ", ["pad", 3]]], "\n"] }], { path: "f.txt", edits: [e("line 100\n", "LINE 100\n"), e("line 300\n", "LINE 300\n"), e("line 500\n", "LINE 500\n")] });
await add("edit", "original-not-incremental", [{ path: "f.txt", text: "foo\nbar\nbaz\n" }], { path: "f.txt", edits: [e("foo\n", "foo bar\n"), e("bar\n", "BAR\n")] });
await add("edit", "empty-edits", [{ path: "f.txt", text: "hello\nworld\n" }], { path: "f.txt", edits: [] });
await add("edit", "overlap", [{ path: "f.txt", text: "one\ntwo\nthree\n" }], { path: "f.txt", edits: [e("one\ntwo\n", "ONE\nTWO\n"), e("two\nthree\n", "TWO\nTHREE\n")] });
await add("edit", "partial", [{ path: "f.txt", text: "alpha\nbeta\ngamma\n" }], { path: "f.txt", edits: [e("alpha\n", "ALPHA\n"), e("missing\n", "MISSING\n")] });
await add("edit", "readonly", [{ path: "f.txt", text: "hello\n", mode: 0o444 }], { path: "f.txt", edits: [e("hello", "world")] });
await add("edit", "empty-old-text", [{ path: "f.txt", text: "hello\n" }], { path: "f.txt", edits: [e("", "world")] });
await add("edit", "empty-old-text-multi", [{ path: "f.txt", text: "hello\n" }], { path: "f.txt", edits: [e("hello", "x"), e("", "world")] });
await add("edit", "no-change", [{ path: "f.txt", text: "hello\n" }], { path: "f.txt", edits: [e("hello", "hello")] });
await add("edit", "no-change-multi", [{ path: "f.txt", text: "a\nb\n" }], { path: "f.txt", edits: [e("a", "a"), e("b", "b")] });
await add("edit", "duplicates-multi", [{ path: "f.txt", text: "a a b" }], { path: "f.txt", edits: [e("b", "c"), e("a", "x")] });
await add("edit", "not-found-multi", [{ path: "f.txt", text: "a b" }], { path: "f.txt", edits: [e("a", "c"), e("q", "x")] });
await add("edit", "trailing-ws", [{ path: "f.txt", text: "line one   \nline two  \nline three\n" }], { path: "f.txt", edits: [e("line one\nline two\n", "replaced\n")] });
await add("edit", "fullwidth", [{ path: "f.txt", text: "你好，世界\n你好（世界）\n" }], { path: "f.txt", edits: [e("你好,世界\n你好(世界)\n", "你好，pi\n你好(pi)\n")] });
await add("edit", "compatibility", [{ path: "f.txt", text: "ＡＢＣ１２３\ncafe\u0301\n" }], { path: "f.txt", edits: [e("ABC123\ncafé\n", "XYZ789\ncoffee\n")] });
await add("edit", "smart-single", [{ path: "f.txt", text: "console.log(\u2018hello\u2019);\n" }], { path: "f.txt", edits: [e("console.log('hello');", "console.log('world');")] });
await add("edit", "smart-double", [{ path: "f.txt", text: "const msg = \u201CHello World\u201D;\n" }], { path: "f.txt", edits: [e('const msg = "Hello World";', 'const msg = "Goodbye";')] });
await add("edit", "dashes", [{ path: "f.txt", text: "range: 1\u20135\nbreak\u2014here\n" }], { path: "f.txt", edits: [e("range: 1-5\nbreak-here", "range: 10-50\nbreak--here")] });
await add("edit", "nbsp", [{ path: "f.txt", text: "hello\u00A0world\n" }], { path: "f.txt", edits: [e("hello world", "hello universe")] });
await add("edit", "exact-preferred", [{ path: "f.txt", text: "const x = 'exact';\nconst y = 'other';\n" }], { path: "f.txt", edits: [e("const x = 'exact';", "const x = 'changed';")] });
await add("edit", "fuzzy-not-found", [{ path: "f.txt", text: "completely different content\n" }], { path: "f.txt", edits: [e("this does not exist", "replacement")] });
await add("edit", "fuzzy-duplicates", [{ path: "f.txt", text: "hello world   \nhello world\n" }], { path: "f.txt", edits: [e("hello world", "replaced")] });
await add("edit", "fuzzy-multi", [{ path: "f.txt", text: "console.log(\u2018hello\u2019);\nhello\u00A0world\n" }], { path: "f.txt", edits: [e("console.log('hello');\n", "console.log('world');\n"), e("hello world\n", "hello universe\n")] });
await add("edit", "fuzzy-duplicate-line", [{ path: "f.txt", text: "replace me   \nafter   \n" }], { path: "f.txt", edits: [e("replace me\n", "after\n")] });
await add("edit", "fuzzy-preserve-multi", [{ path: "f.txt", text: "keep before  \nfirst target  \nfirst after\nkeep middle   \nsecond target  \nsecond after\nkeep after  \n" }], { path: "f.txt", edits: [e("first target\nfirst after", "FIRST\nFIRST2"), e("second target\nsecond after", "SECOND\nSECOND2")] });
await add("edit", "fuzzy-trailing-feff", [{ path: "f.txt", text: "a\uFEFF\nb\n" }], { path: "f.txt", edits: [e("a\nb", "c")] });
await add("edit", "crlf-match", [{ path: "f.txt", text: "line one\r\nline two\r\nline three\r\n" }], { path: "f.txt", edits: [e("line two\n", "replaced line\n")] });
await add("edit", "crlf-preserve", [{ path: "f.txt", text: "first\r\nsecond\r\nthird\r\n" }], { path: "f.txt", edits: [e("second\n", "REPLACED\n")] });
await add("edit", "mixed-endings", [{ path: "f.txt", text: "hello\r\nworld\r\n---\r\nhello\nworld\n" }], { path: "f.txt", edits: [e("hello\nworld\n", "replaced\n")] });
await add("edit", "lone-cr", [{ path: "f.txt", text: "a\rb\nc\n" }], { path: "f.txt", edits: [e("b\nc", "B\r\nC")] });
await add("edit", "bom", [{ path: "f.txt", text: "\uFEFFfirst\r\nsecond\r\nthird\r\n" }], { path: "f.txt", edits: [e("second\n", "REPLACED\n")] });
await add("edit", "bom-crlf-multi", [{ path: "f.txt", text: "\uFEFFfirst\r\nsecond\r\nthird\r\nfourth\r\n" }], { path: "f.txt", edits: [e("second\n", "SECOND\n"), e("fourth\n", "FOURTH\n")] });
await add("edit", "no-trailing-newline", [{ path: "f.txt", text: "a\nb\nc" }], { path: "f.txt", edits: [e("c", "C\nD")] });
await add("edit", "remove-all", [{ path: "f.txt", text: "only\n" }], { path: "f.txt", edits: [e("only\n", "")] });
await add("edit", "add-to-empty-line", [{ path: "f.txt", text: "\n" }], { path: "f.txt", edits: [e("\n", "x\n")] });
await add("edit", "diff-context", [{ path: "f.txt", text: [["lines", 30, ["row {i}"]], "\n"] }], { path: "f.txt", edits: [e("row 3\n", "ROW 3\n"), e("row 12\n", "ROW 12\nnew\n"), e("row 15\n", ""), e("row 28\n", "ROW 28\n")] });
await add("edit", "invalid-utf8", [{ path: "f.txt", base64: Buffer.from([0x61, 0xff, 0x0a, 0x62]).toString("base64") }], { path: "f.txt", edits: [e("b", "c")] });
await add("edit", "directory", [{ path: "sub", dir: true }], { path: "sub", edits: [e("a", "b")] });

// ls
const lsNames = ["_a", "a1", "a_b", "B", "a-b", "ab", ".x", "Z", "10", "9", "e", "\u00e9", "a.b", "a b", "~x", "A", "a"];
await add("ls", "sorted", [...lsNames.filter((n) => n !== "A").map((path) => ({ path, text: "" })), { path: "dir", dir: true }, { path: ".hidden-dir", dir: true }], {});
await add("ls", "limit", [{ path: "a", text: "" }, { path: "b", text: "" }, { path: "c", dir: true }, { path: "d", text: "" }], { path: ".", limit: 2 });
await add("ls", "limit-zero", [{ path: "a", text: "" }], { limit: 0 });
await add("ls", "empty", [{ path: "empty", dir: true }], { path: "empty" });
await add("ls", "not-a-directory", [{ path: "f.txt", text: "" }], { path: "f.txt" });
await add("ls", "missing", [], { path: "nope" });
await add("ls", "broken-symlink", [{ path: "real", text: "" }, { path: "dangling", symlink: "nowhere" }, { path: "linkdir", symlink: "sub" }, { path: "sub", dir: true }], {});
await add("ls", "byte-limit", [{ path: ".", series: { count: 300, pad: 3, suffix: ["-", ["repeat", "n", 200]] } }], { limit: 1000 });

// ---------------------------------------------------------------- rg and fd

// Run the binary as Pi does and keep what it printed, then Pi's tool on the same
// files. rg and fd interleave files nondeterministically, so retry until Pi's
// result matches the recorded order.
async function external(kind: "grep" | "find", name: string, files: FileSpec[], args: any, buildArgs: (dir: string) => string[]) {
	for (let attempt = 0; attempt < 20; attempt++) {
		const dir = setup(files);
		const argv = buildArgs(dir);
		const binary = kind === "grep" ? "rg" : "fd";
		const out = spawnSync(binary, argv, { encoding: "buffer" });
		const record: any = { tool: kind, name, files, args };
		const definition = tools.createAllToolDefinitions(dir)[kind];
		try {
			const result = await definition.execute(`call-${name}`, substitute(args, dir), undefined, undefined, undefined);
			record.result = scrub({ content: result.content, details: result.details ?? null }, dir);
		} catch (error: any) {
			record.error = scrub(error?.message ?? String(error), dir);
		}
		record.argv = scrub(argv, dir);
		record.stdout = out.stdout.toString("utf8").split(dir).join(DIR);
		record.stderr = out.stderr.toString("utf8").split(dir).join(DIR);
		record.code = out.status;
		const text = record.result?.content?.[0]?.text ?? "";
		const order = (record.stdout as string)
			.split("\n")
			.filter(Boolean)
			.map((line) => (kind === "grep" ? (JSON.parse(line).data?.path?.text ?? "") : line))
			.filter(Boolean)
			.map((path) => path.slice(path.lastIndexOf("/") + 1));
		const seen = order.filter((p, i) => order.indexOf(p) === i);
		const positions = seen.map((p) => text.indexOf(p));
		if (positions.every((p, i) => i === 0 || positions[i - 1] <= p)) return record;
	}
	throw new Error(`could not record a stable ${kind} order for ${name}`);
}

const rgArgs = (args: any) => (dir: string) => {
	const a = ["--json", "--line-number", "--color=never", "--hidden"];
	if (args.ignoreCase) a.push("--ignore-case");
	if (args.literal) a.push("--fixed-strings");
	if (args.glob) a.push("--glob", args.glob);
	a.push("--", args.pattern, substitute(args.path ? join(dir, args.path) : dir, dir));
	return a;
};
const grepCases: unknown[] = [];
const g = async (name: string, files: FileSpec[], args: any) => grepCases.push(await external("grep", name, files, args, rgArgs(args)));
await g("single-file", [{ path: "example.txt", text: "first line\nmatch line\nlast line" }], { pattern: "match", path: "example.txt" });
await g("limit-context", [{ path: "context.txt", text: "before\nmatch one\nafter\nmiddle\nmatch two\nafter two" }], { pattern: "match", path: "context.txt", limit: 1, context: 1 });
await g("directory", [{ path: "src/a.ts", text: "const x = 1;\n// TODO one\n" }, { path: "src/b.md", text: "no\n" }], { pattern: "TODO" });
await g("two-files", [{ path: "a.txt", text: "hit\n" }, { path: "z/b.txt", text: "x\nhit\nhit\n" }], { pattern: "hit" });
await g("ignore-case", [{ path: "f.txt", text: "Alpha\nalpha\nALPHA\n" }], { pattern: "alpha", ignoreCase: true });
await g("literal", [{ path: "f.txt", text: "a.b\naxb\n" }], { pattern: "a.b", literal: true });
await g("glob", [{ path: "a.ts", text: "hit\n" }, { path: "b.js", text: "hit\n" }], { pattern: "hit", glob: "*.ts" });
await g("no-match", [{ path: "f.txt", text: "nothing\n" }], { pattern: "absent" });
await g("long-line", [{ path: "f.txt", text: [["repeat", "y", 700], "hit\nshort hit\n"] }], { pattern: "hit" });
await g("context-crlf", [{ path: "f.txt", text: "one\r\ntwo hit\r\nthree\r\n" }], { pattern: "hit", context: 2 });
await g("context-long", [{ path: "f.txt", text: [["repeat", "p", 600], "\nhit\n"] }], { pattern: "hit", context: 1 });
await g("non-utf8-line", [{ path: "f.bin.txt", base64: Buffer.from([0x68, 0x69, 0x74, 0xff, 0x0a, 0x6f, 0x6b, 0x0a]).toString("base64") }], { pattern: "hit" });
await g("hidden", [{ path: ".env", text: "SECRET=hit\n" }], { pattern: "hit" });
await g("gitignored", [{ path: ".gitignore", text: "ignored.txt\n" }, { path: ".git/HEAD", text: "ref: refs/heads/main\n" }, { path: "ignored.txt", text: "hit\n" }, { path: "kept.txt", text: "hit\n" }], { pattern: "hit" });
await g("flag-like", [{ path: "target.txt", text: "target\n" }], { pattern: "--pre=/bin/false" });
await g("bad-regex", [{ path: "f.txt", text: "x\n" }], { pattern: "(" });
await g("limit-many", [{ path: "f.txt", text: [["lines", 10, ["hit {i}"]]] }], { pattern: "hit", limit: 3 });
await g("missing-path", [], { pattern: "x", path: "nope" });

const fdArgs = (args: any) => (dir: string) => {
	const searchPath = args.path ? join(dir, args.path) : dir;
	const a = ["--glob", "--color=never", "--hidden"];
	let inside = false;
	for (let current = searchPath; ; ) {
		try {
			readFileSync(join(current, ".git", "HEAD"));
			inside = true;
			break;
		} catch {}
		const parent = dirname(current);
		if (parent === current) break;
		current = parent;
	}
	if (!inside) a.push("--no-require-git");
	const limit = args.limit ?? 1000;
	a.push("--max-results", String(limit));
	let pattern = args.pattern;
	if (pattern.includes("/")) {
		a.push("--full-path");
		if (!pattern.startsWith("/") && !pattern.startsWith("**/") && pattern !== "**") pattern = `**/${pattern}`;
	}
	a.push("--", pattern, searchPath);
	return a;
};
const findCases: unknown[] = [];
const f = async (name: string, files: FileSpec[], args: any) => findCases.push(await external("find", name, files, args, fdArgs(args)));
await f("hidden", [{ path: ".secret/hidden.txt", text: "" }, { path: "visible.txt", text: "" }], { pattern: "**/*.txt" });
await f("gitignore", [{ path: ".gitignore", text: "ignored.txt\n" }, { path: "ignored.txt", text: "" }, { path: "kept.txt", text: "" }], { pattern: "**/*.txt" });
await f("basename", [{ path: "a/b/c.spec.ts", text: "" }, { path: "a/d.ts", text: "" }], { pattern: "*.spec.ts" });
await f("full-path", [{ path: "src/x/y.spec.ts", text: "" }, { path: "lib/y.spec.ts", text: "" }], { pattern: "src/**/*.spec.ts" });
await f("directories", [{ path: "pkg/inner", dir: true }, { path: "pkg/file", text: "" }], { pattern: "*" });
await f("limit", [{ path: "a.md", text: "" }, { path: "b.md", text: "" }, { path: "c.md", text: "" }], { pattern: "*.md", limit: 2 });
await f("none", [{ path: "a.md", text: "" }], { pattern: "*.rs" });
await f("bad-glob", [{ path: "a.md", text: "" }], { pattern: "[" });
await f("flag-like", [{ path: "a.md", text: "" }], { pattern: "--help" });
await f("subdir", [{ path: "sub/a.md", text: "" }, { path: "b.md", text: "" }], { pattern: "*.md", path: "sub" });

// ---------------------------------------------------------------- images in tool results

const imageCases = [];
const pngSizes = new Map<string, [number, number]>();
const png = (w: number, h: number) => {
	const data = createPng(w, h).toString("base64");
	pngSizes.set(data, [w, h]);
	return data;
};
for (const [name, content, options] of [
	["text-only", [{ type: "text", text: "no images here" }], undefined],
	["within-limits", [{ type: "text", text: "s" }, { type: "image", data: PNG_1X1, mimeType: "image/png" }], undefined],
	["oversized", [{ type: "image", data: png(2400, 4800), mimeType: "image/png" }], undefined],
	["oversized-no-resize", [{ type: "image", data: png(2400, 4800), mimeType: "image/png" }], { autoResizeImages: false }],
	["bmp-no-resize", [{ type: "image", data: createTinyBmp().toString("base64"), mimeType: "image/bmp" }], { autoResizeImages: false }],
	["undecodable", [{ type: "image", data: "bm90LWFuLWltYWdl", mimeType: "image/png" }], undefined],
	["order", [{ type: "text", text: "before" }, { type: "image", data: png(2400, 100), mimeType: "image/png" }, { type: "text", text: "after" }], undefined],
] as const) {
	const normalized = await toolResultImages.normalizeToolResultImages(content, options);
	imageCases.push({
		name,
		// Generated PNGs are recorded as their dimensions; the test builds the same pixels.
		content: content.map((block: any) => {
			const size = pngSizes.get(block.data);
			return size ? { ...block, data: { $png: size } } : block;
		}),
		options: options ?? null,
		unchanged: normalized === content,
		output: normalized.map((block: any) => (block.type === "image" ? { type: "image", mimeType: block.mimeType } : block)),
	});
}

rmSync(base, { recursive: true, force: true });
process.stdout.write(
	`${JSON.stringify(
		{ revision, definitions, contributions, prompts, prepareArguments, toolSelection, cases, grep: grepCases, find: findCases, toolResultImages: imageCases },
		null,
		1,
	)}\n`,
);
