import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { networkInterfaces, tmpdir } from "node:os";
import { join } from "node:path";
import { rejectReason } from "./guard";

const PORT = 1776;
const req = (method: string, headers: Record<string, string>) =>
  new Request(`http://localhost:${PORT}/api/issues`, { method, headers });

describe("rejectReason (AMT-34)", () => {
  test("the board's own page may read and write", () => {
    for (const host of [`localhost:${PORT}`, `127.0.0.1:${PORT}`, `[::1]:${PORT}`]) {
      expect(rejectReason(req("GET", { host }), PORT)).toBeNull();
      const own = { host, origin: `http://${host}`, "sec-fetch-site": "same-origin" };
      expect(rejectReason(req("POST", own), PORT)).toBeNull();
      expect(rejectReason(req("PATCH", own), PORT)).toBeNull();
    }
  });

  test("local non-browser clients (no Origin, no Sec-Fetch-Site) may write", () => {
    expect(rejectReason(req("POST", { host: `localhost:${PORT}` }), PORT)).toBeNull();
  });

  test("a foreign Host is refused for reads too (DNS rebinding)", () => {
    expect(rejectReason(req("GET", { host: `attacker.example:${PORT}` }), PORT)).toContain("host");
    // Right name, wrong port: some other local service's page, not the board.
    expect(rejectReason(req("GET", { host: "localhost:3000" }), PORT)).toContain("host");
    expect(rejectReason(req("GET", {}), PORT)).toContain("host");
  });

  test("a mutation from another origin is refused", () => {
    const host = `localhost:${PORT}`;
    for (const origin of ["https://evil.example", "null", "http://localhost:3000"]) {
      expect(rejectReason(req("POST", { host, origin }), PORT)).toContain("cross-origin");
    }
    // Origin stripped but the browser still says cross-site.
    expect(rejectReason(req("PATCH", { host, "sec-fetch-site": "cross-site" }), PORT)).toContain("cross-site");
    expect(rejectReason(req("POST", { host, "sec-fetch-site": "same-site" }), PORT)).toContain("cross-site");
  });

  test("Host and Origin compare case-insensitively", () => {
    const host = `LOCALHOST:${PORT}`;
    expect(rejectReason(req("POST", { host, origin: `HTTP://LocalHost:${PORT}` }), PORT)).toBeNull();
  });
});

// End to end against the real server: the attack the review reproduced must
// now get a 403, and the board must not be reachable off-host.
describe("server.ts (AMT-34)", () => {
  const port = 20000 + Math.floor(Math.random() * 40000);
  const base = `http://127.0.0.1:${port}`;
  let dir = "";
  let proc: ReturnType<typeof Bun.spawn> | null = null;

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), "amt-guard-"));
    // The server only registers a workspace whose database exists. Give it the
    // two tables /api/workspaces reads; writes would shell out to AMT_BIN.
    mkdirSync(join(dir, ".ametrite"));
    const db = new Database(join(dir, ".ametrite", "ametrite.db"));
    db.run("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)");
    db.run("INSERT INTO meta VALUES ('workspace_name', 'Guard'), ('id_prefix', 'GD')");
    db.run("CREATE TABLE issues (id TEXT, status TEXT)");
    db.close();
    writeFileSync(join(dir, "registry.json"), JSON.stringify({ workspaces: { guard: dir } }));
    proc = Bun.spawn(["bun", "run", join(import.meta.dir, "server.ts")], {
      cwd: dir,
      env: {
        ...process.env,
        AMT_REGISTRY: join(dir, "registry.json"),
        AMT_PORT: String(port),
        AMT_WORKSPACE: "",
        AMT_BIN: join(dir, "no-such-amt"), // a write that got through would fail loudly, not touch a real board
      },
      stdout: "ignore",
      stderr: "pipe",
    });
    for (let i = 0; i < 80; i++) {
      if (proc.exitCode !== null) {
        throw new Error(`board exited ${proc.exitCode}: ${await new Response(proc.stderr as ReadableStream).text()}`);
      }
      try {
        await fetch(`${base}/api/workspaces`);
        return;
      } catch {
        await Bun.sleep(50);
      }
    }
    throw new Error("board did not start");
  });

  afterAll(() => {
    proc?.kill(); // this exact child only
    if (dir) rmSync(dir, { recursive: true, force: true });
  });

  test("same-origin reads work", async () => {
    const res = await fetch(`${base}/api/workspaces`);
    expect(res.status).toBe(200);
    expect((await res.json()).map((w: any) => w.prefix)).toContain("GD");
  });

  test("a cross-origin text/plain POST is refused before it reaches amt", async () => {
    const res = await fetch(`${base}/api/issues?ws=guard`, {
      method: "POST",
      headers: { origin: "https://evil.example", "content-type": "text/plain" },
      body: JSON.stringify({ title: "cross-origin write" }),
    });
    expect(res.status).toBe(403);
  });

  test("a rebinding Host is refused", async () => {
    const res = await fetch(`${base}/api/workspaces`, { headers: { host: `attacker.example:${port}` } });
    expect(res.status).toBe(403);
  });

  test("the board is not reachable on a non-loopback address", async () => {
    const lan = Object.values(networkInterfaces())
      .flat()
      .find((a) => a && a.family === "IPv4" && !a.internal)?.address;
    if (!lan) return; // no external interface on this machine: nothing to probe
    const reached = await fetch(`http://${lan}:${port}/api/workspaces`, { signal: AbortSignal.timeout(2000) })
      .then(() => true)
      .catch(() => false);
    expect(reached).toBe(false);
  });
});
