// Record Pi v1.1.0 (abe508e1b89912adde45528136c3221eb69acdd7) as the oracle.
// Run: node <this file> <Pi checkout>
// Only a surrogate split by truncateLine is normalized, to Rust's U+FFFD.
import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const root = resolve(process.argv[2]);
const revision = "abe508e1b89912adde45528136c3221eb69acdd7";
if (execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim() !== revision) {
    throw new Error("Use Pi v1.1.0 at the recorded revision");
}
const load = (path) => import(pathToFileURL(`${root}/packages/coding-agent/src/${path}`));
const { truncateHead, truncateTail, truncateMiddle, truncateLine, formatSize } = await load("core/tools/truncate.ts");
const { OutputAccumulator } = await load("core/tools/output-accumulator.ts");
let seed = 0x5134ba;
const random = (max) => {
    seed ^= seed << 13;
    seed ^= seed >>> 17;
    seed ^= seed << 5;
    return (seed >>> 0) % max;
};

const truncations = [];
const texts = ["", "\n", "\r\n", "one\ntwo\n", "a\n\n", "€🍞é中\r\nlast", "x".repeat(65)];
for (const content of texts) {
    for (const maxLines of [0, 1, 2, 2000]) {
        for (const maxBytes of [0, 1, 4, 16, 51200]) {
            const options = { maxLines, maxBytes };
            truncations.push({ content, options, head: truncateHead(content, options), tail: truncateTail(content, options) });
        }
    }
}
const alphabet = ["a", "\n", "\r", "中", "é", "🍞", "\0", "\uFEFF"];
for (let i = 0; i < 80; i++) {
    const content = Array.from({ length: random(100) }, () => alphabet[random(alphabet.length)]).join("");
    const options = { maxLines: random(12), maxBytes: random(128) };
    truncations.push({ content, options, head: truncateHead(content, options), tail: truncateTail(content, options) });
}
const middle = texts.flatMap(content => [0, 1, 3, 7, 20, 51200].map(maxBytes => ({ content, maxBytes, result: truncateMiddle(content, maxBytes) })));
const line = texts.flatMap(content => [0, 1, 2, 3, 7, 500].map(maxChars => {
    const result = truncateLine(content, maxChars);
    result.text = result.text.toWellFormed();
    return { content, maxChars, result };
}));

const inputs = [
    { name: "empty", chunks: [], options: {} },
    { name: "line-only spill", chunks: [Buffer.from("1\n2\n"), Buffer.from("3\n4\n")], options: { maxLines: 2, maxBytes: 100 } },
    { name: "rolling complete lines", chunks: Array.from({ length: 30 }, (_, i) => Buffer.from(`line-${i}\n`)), options: { maxLines: 2, maxBytes: 12 } },
    { name: "rolling partial line", chunks: [Buffer.from("x".repeat(200)), Buffer.from("end\nlast\n")], options: { maxLines: 4, maxBytes: 8 } },
    { name: "finish triggers spill", chunks: [Buffer.from([0xe2])], options: { maxBytes: 2 } },
    { name: "raw BOM triggers spill", chunks: [Buffer.from("\uFEFF")], options: { maxBytes: 1 } },
    { name: "zero bytes", chunks: [Buffer.from("🍞tail")], options: { maxBytes: 0 } },
    { name: "zero lines", chunks: [Buffer.from("abc\n")], options: { maxLines: 0 } },
];
const utf8 = Buffer.from("\uFEFF€\n🍞é\r\nlast\uFEFF");
for (let split = 0; split <= utf8.length; split++) {
    inputs.push({ name: `utf8 split ${split}`, chunks: [utf8.subarray(0, split), utf8.subarray(split)], options: { maxBytes: 8 } });
}
for (let i = 0; i < 64; i++) {
    const bytes = Buffer.from(Array.from({ length: random(100) }, () => random(256)));
    const chunks = [];
    for (let offset = 0; offset < bytes.length;) {
        const end = Math.min(bytes.length, offset + random(12) + 1);
        chunks.push(bytes.subarray(offset, end));
        offset = end;
    }
    inputs.push({ name: `random bytes ${i}`, chunks, options: { maxLines: random(8), maxBytes: random(60) } });
}
const streams = [];
for (const { name, chunks, options } of inputs) {
    const accumulator = new OutputAccumulator(options);
    const snapshots = [];
    let file;
    const snapshot = () => {
        const { content, truncation, fullOutputPath } = accumulator.snapshot();
        file = fullOutputPath;
        return { content, truncation, spilled: !!file, lastLineBytes: accumulator.getLastLineBytes() };
    };
    try {
        for (const chunk of chunks) {
            accumulator.append(chunk);
            snapshots.push(snapshot());
        }
        accumulator.finish();
        snapshots.push(snapshot());
        await accumulator.closeTempFile();
        const reads = [];
        for (const maxBytes of [0, 1, 3, 8, 51200]) {
            reads.push({ maxBytes, result: await accumulator.readFullOutput(maxBytes) });
        }
        streams.push({ name, chunks: chunks.map(chunk => [...chunk]), options, snapshots, reads, raw: file ? [...readFileSync(file)] : null });
    } finally {
        await accumulator.closeTempFile();
        if (file) rmSync(file);
    }
}
const sizes = [0, 1, 1023, 1024, 1280, 1048575, 1048576, 1310720].map(bytes => ({ bytes, result: formatSize(bytes) }));
const fixture = { revision, truncations, middle, line, sizes, streams };
const fields = Object.entries(fixture).map(([key, value]) => {
    const body = Array.isArray(value) ? `[\n${value.map(item => `    ${JSON.stringify(item)}`).join(",\n")}\n  ]` : JSON.stringify(value);
    return `  ${JSON.stringify(key)}: ${body}`;
});
writeFileSync(new URL("golden.json", import.meta.url), `{\n${fields.join(",\n")}\n}\n`);
