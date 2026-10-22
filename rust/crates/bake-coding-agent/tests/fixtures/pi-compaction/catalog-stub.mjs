import { registerHooks } from "node:module";
registerHooks({
	resolve(specifier, context, nextResolve) {
		if (/\/providers\/data\/[^/]+\.json$/.test(specifier) || /^\.\/data\/[^/]+\.json$/.test(specifier) && context.parentURL?.includes("/packages/ai/src/providers/")) {
			return { url: "data:application/json,{}", shortCircuit: true, importAttributes: context.importAttributes };
		}
		return nextResolve(specifier, context);
	},
});
