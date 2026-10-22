// What a reader of a session sees, as one JSON value, for comparing Pi's
// SessionManager with Bake's port. `tests/session/golden.rs` builds the same
// value in Rust; the two must serialize to identical strings.

export async function summarize(SessionManager, manager, listCwd) {
	const roots = manager.getTree();
	const nodes = new Map();
	const stack = [...roots];
	while (stack.length > 0) {
		const node = stack.pop();
		nodes.set(node.entry.id, node);
		stack.push(...node.children);
	}
	const sessions = await SessionManager.list(listCwd, manager.getSessionDir());
	const info = sessions.find((session) => session.id === manager.getSessionId());
	return {
		header: manager.getHeader(),
		sessionId: manager.getSessionId(),
		leafId: manager.getLeafId(),
		sessionName: manager.getSessionName() ?? null,
		context: manager.buildSessionContext(),
		contextEntryIds: manager.buildContextEntries().map((entry) => entry.id),
		branchIds: manager.getBranch().map((entry) => entry.id),
		roots: roots.map((node) => node.entry.id),
		tree: manager.getEntries().map((entry) => {
			const node = nodes.get(entry.id);
			return [
				entry.id,
				node ? node.children.map((child) => child.entry.id) : null,
				manager.getLabel(entry.id) ?? null,
				node?.labelTimestamp ?? null,
			];
		}),
		info: info
			? {
					id: info.id,
					cwd: info.cwd,
					name: info.name,
					parentSessionPath: info.parentSessionPath,
					created: info.created,
					modified: info.modified,
					messageCount: info.messageCount,
					firstMessage: info.firstMessage,
					allMessagesText: info.allMessagesText,
				}
			: null,
	};
}

export async function loadSessionManager() {
	const piDir = process.env.PI_DIR;
	if (!piDir) throw new Error("Set PI_DIR to a Pi v1.1.0 checkout");
	return (await import(`${piDir}/packages/coding-agent/src/core/session-manager.ts`)).SessionManager;
}
