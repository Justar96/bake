// Execute Pi v1.1.0's queue over real read/modify/write operations.
// Run: node <this file> <Pi checkout>
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const root = resolve(process.argv[2]);
const revision = "abe508e1b89912adde45528136c3221eb69acdd7";
if (execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim() !== revision) {
    throw new Error("Use Pi v1.1.0 at the recorded revision");
}
const { withFileMutationQueue } = await import(pathToFileURL(`${root}/packages/coding-agent/src/core/tools/file-mutation-queue.ts`));
let seed = 0xface;
const random = max => {
    seed ^= seed << 13;
    seed ^= seed >>> 17;
    seed ^= seed << 5;
    return (seed >>> 0) % max;
};
const cases = [];
for (let index = 0; index < 16; index++) {
    const work = await mkdtemp(join(tmpdir(), "pi-mutation-oracle-"));
    const files = ["a", "b", "c"];
    const operations = Array.from({ length: 24 }, (_, operation) => {
        const file = files[random(files.length)];
        return { path: random(2) ? file : `folder/../${file}`, text: `${operation},` };
    });
    try {
        await mkdir(join(work, "folder"));
        await Promise.all(files.map(file => writeFile(join(work, file), "")));
        await Promise.all(operations.map(({ path, text }) => {
            const file = join(work, path);
            return withFileMutationQueue(file, async () => {
                const before = await readFile(file, "utf8");
                await Promise.resolve();
                await writeFile(file, before + text);
            });
        }));
        const expected = Object.fromEntries(await Promise.all(files.map(async file => [file, await readFile(join(work, file), "utf8")])));
        cases.push({ operations, expected });
    } finally {
        await rm(work, { recursive: true, force: true });
    }
}
await writeFile(new URL("golden.json", import.meta.url), `${JSON.stringify({ revision, cases }, null, 2)}\n`);
