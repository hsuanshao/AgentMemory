import { describe, expect, it } from "vitest";
import { ADMIN_ONLY_V3_PATHS, CORE_V3_ALLOWLIST, resolveV3Path } from "../src/http/allowlist.js";

describe("resolveV3Path", () => {
  it("accepts allowlisted paths", () => {
    expect(resolveV3Path("atomic/query")).toBe("atomic/query");
    expect(resolveV3Path("meta/auth/verify")).toBe("meta/auth/verify");
    expect(resolveV3Path("memory-generation-log/list")).toBe("memory-generation-log/list");
    expect(resolveV3Path("memory-generation-log/summary")).toBe("memory-generation-log/summary");
    expect(resolveV3Path("pipeline/dead-letter/list")).toBe("pipeline/dead-letter/list");
  });

  it("normalises redundant slashes", () => {
    expect(resolveV3Path("atomic//query/")).toBe("atomic/query");
  });

  it.each([
    "instance/destroy",
    "internal/meta/user/init-admin",
    "meta/acl/grant",
    "atomic/../instance/destroy",
    "atomic/query%2f..",
    "ATOMIC/QUERY",
    "",
    "/",
  ])("rejects %j", (tail) => {
    expect(resolveV3Path(tail)).toBeUndefined();
  });

  it("only guards paths that are allowlisted", () => {
    for (const path of ADMIN_ONLY_V3_PATHS) expect(CORE_V3_ALLOWLIST.has(path), path).toBe(true);
  });

  it("never allowlists destructive admin routes", () => {
    for (const path of CORE_V3_ALLOWLIST) {
      expect(path.startsWith("internal/")).toBe(false);
      expect(path.startsWith("instance/")).toBe(false);
    }
  });
});
