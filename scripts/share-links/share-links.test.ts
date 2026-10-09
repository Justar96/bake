import { describe, expect, test } from "bun:test";
import { createShareLink, listShareLinks, openShareLinkStore, redeemShareLink, ShareLinkError } from "./share-links";

const owner = { id: "u1", workspaceId: "w1", role: "member" as const };

describe("share links", () => {
  test("a created link can be redeemed", () => {
    const db = openShareLinkStore();
    const link = createShareLink(db, owner, "s1", 7, 3, 1_000);
    expect(redeemShareLink(db, link.token, 2_000)).toBe("s1");
  });

  test("viewers cannot create links", () => {
    const db = openShareLinkStore();
    expect(() => createShareLink(db, { ...owner, role: "viewer" }, "s1", 7, 1)).toThrow(ShareLinkError);
  });

  test("expired links are rejected", () => {
    const db = openShareLinkStore();
    const link = createShareLink(db, owner, "s1", 1, 3, 0);
    expect(() => redeemShareLink(db, link.token, 2 * 24 * 60 * 60 * 1000)).toThrow("expired");
  });

  test("members list their workspace links", () => {
    const db = openShareLinkStore();
    createShareLink(db, owner, "s1", 7, 3);
    expect(listShareLinks(db, owner, "w1")).toHaveLength(1);
  });
});
