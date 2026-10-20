import { describe, expect, it } from "vitest";
import { ConfigError, displayCoreUrl, loadConfig } from "../src/config/config.js";

describe("loadConfig", () => {
  it("applies defaults", () => {
    expect(loadConfig({})).toEqual({
      port: 8125,
      host: "0.0.0.0",
      coreUrl: "http://127.0.0.1:8420",
      coreApiKey: undefined,
      coreDepartmentId: "default",
      coreTimeoutMs: 15_000,
      uiDistDir: undefined,
      requireUserKey: true,
    });
  });

  it("reads overrides", () => {
    const cfg = loadConfig({
      PORT: "9000",
      CORE_URL: "http://core:8420/",
      CORE_API_KEY: "k",
      CORE_DEPARTMENT_ID: "dept",
      CORE_TIMEOUT_MS: "500",
      UI_DIST_DIR: "/srv/ui",
      PANEL_REQUIRE_USER_KEY: "false",
    });
    expect(cfg).toMatchObject({ port: 9000, coreUrl: "http://core:8420", coreApiKey: "k", coreTimeoutMs: 500, requireUserKey: false });
  });

  it("rejects invalid numbers and URLs", () => {
    expect(() => loadConfig({ PORT: "abc" })).toThrow(ConfigError);
    expect(() => loadConfig({ CORE_TIMEOUT_MS: "-1" })).toThrow(ConfigError);
    expect(() => loadConfig({ CORE_URL: "not a url" })).toThrow(ConfigError);
  });

  it("strips credentials from the display URL", () => {
    expect(displayCoreUrl("http://user:pw@core:8420")).toBe("http://core:8420");
  });
});
