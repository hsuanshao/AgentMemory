import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { buildApp, coreFetch, jsonResponse } from "./helpers.js";

const noCore = () => coreFetch(() => jsonResponse({}));

describe("system routes", () => {
  it("GET /health", async () => {
    const res = await buildApp(noCore()).request("/health");
    expect(await res.json()).toEqual({ status: "ok" });
  });

  it("GET /api/config exposes no secrets", async () => {
    const res = await buildApp(noCore()).request("/api/config");
    const body = await res.json();
    expect(body).toEqual({ department_id: "dept-1", core_url_display: "http://core.test:8420" });
  });

  it("unknown /api paths return a JSON 404", async () => {
    const res = await buildApp(noCore()).request("/api/nope");
    expect(res.status).toBe(404);
    expect(await res.json()).toMatchObject({ code: 404 });
  });
});

describe("static UI", () => {
  let dir: string;

  beforeAll(async () => {
    dir = await mkdtemp(join(tmpdir(), "panel-ui-"));
    await mkdir(join(dir, "assets"));
    await writeFile(join(dir, "index.html"), "<!doctype html><title>panel</title>");
    await writeFile(join(dir, "assets", "app.js"), "console.log(1)");
  });

  afterAll(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  it("serves assets with a long cache", async () => {
    const res = await buildApp(noCore(), { uiDistDir: dir }).request("/assets/app.js");
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toContain("text/javascript");
    expect(res.headers.get("cache-control")).toContain("immutable");
  });

  it("falls back to index.html for client routes", async () => {
    const res = await buildApp(noCore(), { uiDistDir: dir }).request("/memory/l1");
    expect(res.status).toBe(200);
    expect(await res.text()).toContain("<title>panel</title>");
  });

  it("does not escape the dist directory", async () => {
    const res = await buildApp(noCore(), { uiDistDir: dir }).request("/..%2f..%2fetc%2fpasswd");
    expect(await res.text()).toContain("<title>panel</title>");
  });
});
