// Load pi-provider.json through Pi's own models.json reader and model
// runtime, and print what Pi makes of it. Run it from the coding-agent
// package of a Pi v1.1.0 checkout (`abe508e1`) with its dependencies
// installed, so tsx resolves Pi's workspace packages to their sources:
//
//   cd <pi>/packages/coding-agent
//   CLIPROXYAPI_API_KEY=fake npx tsx <path to this file>
//
// A source checkout lacks the generated provider catalogs
// (`packages/ai/src/providers/data/*.json`); the hook below serves an empty
// catalog (a temporary `{}` file) for a missing one, which leaves only custom
// providers. Nothing is written to the checkout. Pi must report no models.json error, list the
// three models with their protocols, endpoints, limits, thinking levels, and
// compat, resolve the `$CLIPROXYAPI_API_KEY` reference, and resolve each
// escaped header to the literal value the 0.3 route held, running nothing.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { register } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scratch = mkdtempSync(join(tmpdir(), "bake-pi-check-"));
process.on("exit", () => rmSync(scratch, { recursive: true, force: true }));
const emptyCatalog = join(scratch, "empty.json");
writeFileSync(emptyCatalog, "{}");

const hook = `
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
export async function resolve(specifier, context, next) {
	if (specifier.endsWith(".json") && specifier.includes("/data/")) {
		const url = new URL(specifier, context.parentURL);
		if (url.protocol === "file:" && !existsSync(fileURLToPath(url))) {
			return { url: ${JSON.stringify(pathToFileURL(emptyCatalog).href)}, shortCircuit: true };
		}
	}
	return next(specifier, context);
}`;
register(`data:text/javascript,${encodeURIComponent(hook)}`);

const core = (name: string) => pathToFileURL(join(process.cwd(), "src/core", name)).href;
const { getSupportedThinkingLevels, InMemoryModelsStore } = await import(
	pathToFileURL(join(process.cwd(), "../ai/src/index.ts")).href
);
const { AuthStorage } = await import(core("auth-storage.ts"));
const { ModelRuntime } = await import(core("model-runtime.ts"));

const modelsPath = join(dirname(fileURLToPath(import.meta.url)), "pi-provider.json");
const runtime = await ModelRuntime.create({
	credentials: AuthStorage.inMemory(),
	modelsPath,
	modelsStore: new InMemoryModelsStore(),
	allowModelNetwork: false,
	refreshOnCreate: true,
});
console.log("error:", runtime.getError() ?? "none");
for (const model of runtime.getModels().filter((model: { provider: string }) => model.provider === "cliproxyapi")) {
	console.log(
		JSON.stringify({
			id: model.id,
			api: model.api,
			baseUrl: model.baseUrl,
			input: model.input,
			contextWindow: model.contextWindow,
			maxTokens: model.maxTokens,
			levels: getSupportedThinkingLevels(model),
			compat: model.compat,
		}),
	);
}
const auth = await runtime.getAuth("cliproxyapi");
console.log("key resolved:", auth?.auth.apiKey === process.env.CLIPROXYAPI_API_KEY);
// The 0.3 route's header values, which Pi must send unchanged.
const literalHeaders = {
	"x-team": "core",
	"x-cmd": "!echo pwned",
	"x-env": "a $HOME ${HOME} $$ $! b!",
	"x-edge": "${ $1 ${1x} !! $",
};
console.log("headers:", JSON.stringify(auth?.auth.headers));
console.log("headers literal:", JSON.stringify(auth?.auth.headers) === JSON.stringify(literalHeaders));
