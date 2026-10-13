// Records `skills.json`: Pi's `buildSystemPrompt` with skills, at Pi v1.2.0
// (06a466935536f8c9bcacb2d5efc0fc021be1bdff). Copy this file into Pi's
// `packages/coding-agent/test/` as a `.test.ts` file, run it there with
// `PI_PACKAGE_DIR=/opt/pi npx vitest run <file>`, and copy
// `/tmp/skills-prompt.json` here with a final newline.
import { writeFileSync } from "node:fs";
import { test } from "vitest";
import { buildSystemPrompt } from "../src/core/system-prompt.ts";

const skill = (name: string, description: string, dir: string, disable = false) => ({
	name,
	description,
	filePath: `${dir}/SKILL.md`,
	baseDir: dir,
	sourceInfo: { path: `${dir}/SKILL.md`, source: "test" } as any,
	disableModelInvocation: disable,
});
const skills = [
	skill("alpha", "Uses <tags> & \"quotes\" and 'apostrophes'.", "/home/u/.agents/skills/alpha"),
	skill("hidden", "Hidden.", "/home/u/.agents/skills/hidden", true),
	skill("beta", "Line one.\nLine two.", "/work/project/.agents/skills/beta"),
];

test("record", () => {
	const base = {
		cwd: "/work/project",
		contextFiles: [{ path: "/work/project/AGENTS.md", content: "Rules." }],
		skills,
		appendSystemPrompt: "Appended.",
	};
	const out = {
		read: buildSystemPrompt({ ...base, selectedTools: ["read", "bash"], toolSnippets: { read: "Read", bash: "Run" } }),
		bash: buildSystemPrompt({ ...base, selectedTools: ["bash"], toolSnippets: { bash: "Run" } }),
		indirect: buildSystemPrompt({
			...base,
			selectedTools: ["read", "bash", "run"],
			hiddenTools: ["read", "bash"],
			toolSnippets: { run: "Run a task" },
		}),
		none: buildSystemPrompt({ ...base, selectedTools: ["write"] }),
		custom: buildSystemPrompt({ ...base, customPrompt: "Custom.", selectedTools: ["bash"] }),
		allHidden: buildSystemPrompt({ ...base, skills: [skills[1]], selectedTools: ["read"] }),
	};
	writeFileSync("/tmp/skills-prompt.json", JSON.stringify(out, null, 2));
});
