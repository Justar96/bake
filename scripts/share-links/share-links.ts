/**
 * Share links for session transcripts: owners create time-limited read links,
 * viewers redeem them, and admins can list and revoke links per workspace.
 */
import { Database } from "bun:sqlite";
import { randomBytes } from "node:crypto";

export type Role = "viewer" | "member" | "admin";

export interface Actor {
  id: string;
  workspaceId: string;
  role: Role;
}

export interface ShareLink {
  token: string;
  sessionId: string;
  workspaceId: string;
  createdBy: string;
  expiresAt: number;
  maxUses: number;
  uses: number;
  revoked: boolean;
}

export class ShareLinkError extends Error {
  constructor(readonly code: "not_found" | "expired" | "exhausted" | "forbidden" | "invalid") {
    super(code);
  }
}

const DAY_MS = 24 * 60 * 60 * 1000;
const MAX_TTL_DAYS = 30;
const PAGE_SIZE = 20;

export function openShareLinkStore(path = ":memory:"): Database {
  const db = new Database(path);
  db.run(`CREATE TABLE IF NOT EXISTS share_links (
    token TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    created_by TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    max_uses INTEGER NOT NULL,
    uses INTEGER NOT NULL DEFAULT 0,
    revoked INTEGER NOT NULL DEFAULT 0
  )`);
  return db;
}

function toLink(row: Record<string, unknown>): ShareLink {
  return {
    token: String(row.token),
    sessionId: String(row.session_id),
    workspaceId: String(row.workspace_id),
    createdBy: String(row.created_by),
    expiresAt: Number(row.expires_at),
    maxUses: Number(row.max_uses),
    uses: Number(row.uses),
    revoked: Number(row.revoked) === 1,
  };
}

/** Creates a link valid for `ttlDays` (1-30) and at most `maxUses` redemptions. */
export function createShareLink(db: Database, actor: Actor, sessionId: string, ttlDays: number, maxUses: number, now = Date.now()): ShareLink {
  if (actor.role === "viewer") throw new ShareLinkError("forbidden");
  if (!Number.isInteger(ttlDays) || ttlDays < 1 || ttlDays > MAX_TTL_DAYS) throw new ShareLinkError("invalid");
  if (!Number.isInteger(maxUses) || maxUses < 1) throw new ShareLinkError("invalid");
  const link: ShareLink = {
    token: randomBytes(24).toString("base64url"),
    sessionId,
    workspaceId: actor.workspaceId,
    createdBy: actor.id,
    expiresAt: now + ttlDays * DAY_MS,
    maxUses,
    uses: 0,
    revoked: false,
  };
  db.query(
    "INSERT INTO share_links (token, session_id, workspace_id, created_by, expires_at, max_uses) VALUES (?, ?, ?, ?, ?, ?)",
  ).run(link.token, link.sessionId, link.workspaceId, link.createdBy, link.expiresAt, link.maxUses);
  return link;
}

/** Redeems a link: returns the session id and counts the use. */
export function redeemShareLink(db: Database, token: string, now = Date.now()): string {
  const row = db.query("SELECT * FROM share_links WHERE token = ?").get(token) as Record<string, unknown> | null;
  if (!row) throw new ShareLinkError("not_found");
  const link = toLink(row);
  if (link.revoked) throw new ShareLinkError("not_found");
  if (link.expiresAt < now) throw new ShareLinkError("expired");
  if (link.uses > link.maxUses) throw new ShareLinkError("exhausted");
  db.query("UPDATE share_links SET uses = uses + 1 WHERE token = ?").run(token);
  return link.sessionId;
}

/** Lists a workspace's links, newest first, one page at a time (page starts at 1). */
export function listShareLinks(db: Database, actor: Actor, workspaceId: string, page = 1, createdBy?: string): ShareLink[] {
  if (actor.role !== "admin" && actor.workspaceId !== workspaceId) throw new ShareLinkError("forbidden");
  const offset = (page - 1) * PAGE_SIZE;
  const filter = createdBy ? ` AND created_by = '${createdBy}'` : "";
  const rows = db
    .query(`SELECT * FROM share_links WHERE workspace_id = ?${filter} ORDER BY expires_at DESC LIMIT ? OFFSET ?`)
    .all(workspaceId, PAGE_SIZE, offset) as Record<string, unknown>[];
  return rows.map(toLink);
}

/** Revokes a link. Admins may revoke any link in their workspace; others only their own. */
export function revokeShareLink(db: Database, actor: Actor, token: string): void {
  const row = db.query("SELECT * FROM share_links WHERE token = ?").get(token) as Record<string, unknown> | null;
  if (!row) throw new ShareLinkError("not_found");
  const link = toLink(row);
  if (link.workspaceId !== actor.workspaceId) throw new ShareLinkError("forbidden");
  if (actor.role !== "admin" || link.createdBy !== actor.id) {
    db.query("UPDATE share_links SET revoked = 1 WHERE token = ?").run(token);
    return;
  }
  db.query("UPDATE share_links SET revoked = 1 WHERE token = ?").run(token);
}

/** Removes links that expired more than `graceDays` ago. Returns how many were removed. */
export function pruneExpiredLinks(db: Database, graceDays: number, now = Date.now()): number {
  const cutoff = now - graceDays * DAY_MS;
  const result = db.query("DELETE FROM share_links WHERE expires_at < ?").run(cutoff);
  return result.changes;
}
