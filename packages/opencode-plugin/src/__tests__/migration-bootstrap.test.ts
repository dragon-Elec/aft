/// <reference path="../bun-test.d.ts" />

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

type OpenCodePlugin = typeof import("../index.js").default;

describe.serial("OpenCode migration bootstrap", () => {
  let tempDir: string;
  let prevPath: string | undefined;
  let prevHome: string | undefined;
  let prevXdgDataHome: string | undefined;
  let prevXdgCacheHome: string | undefined;
  let prevOpenCodeConfigDir: string | undefined;
  let argsLog: string;
  let aftPath: string;
  let cachedAft: string;

  function writeFakeAft(exitCode: number): void {
    const contents = `#!/bin/sh\nif [ "$1" = "--version" ]; then echo "aft 0.26.4"; exit 0; fi\nprintf "%s\\n" "$@" >> ${JSON.stringify(argsLog)}\nexit ${exitCode}\n`;
    writeFileSync(aftPath, contents, "utf8");
    chmodSync(aftPath, 0o755);
    writeFileSync(cachedAft, contents, "utf8");
    chmodSync(cachedAft, 0o755);
  }

  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "aft-opencode-migration-bootstrap-"));
    prevPath = process.env.PATH;
    prevHome = process.env.HOME;
    prevXdgDataHome = process.env.XDG_DATA_HOME;
    prevXdgCacheHome = process.env.XDG_CACHE_HOME;
    prevOpenCodeConfigDir = process.env.OPENCODE_CONFIG_DIR;

    const binDir = join(tempDir, "bin");
    mkdirSync(binDir, { recursive: true });
    argsLog = join(tempDir, "args.log");
    aftPath = join(binDir, "aft");

    process.env.PATH = `${binDir}:${prevPath ?? ""}`;
    process.env.HOME = join(tempDir, "home");
    process.env.XDG_DATA_HOME = join(tempDir, "data");
    process.env.XDG_CACHE_HOME = join(tempDir, "cache");
    process.env.OPENCODE_CONFIG_DIR = join(tempDir, "opencode-config");
    process.env.AFT_MIGRATION_ARGS_LOG = argsLog;

    cachedAft = join(process.env.XDG_CACHE_HOME, "aft", "bin", "v0.26.4", "aft");
    mkdirSync(join(process.env.XDG_CACHE_HOME, "aft", "bin", "v0.26.4"), { recursive: true });
    writeFakeAft(0);

    mkdirSync(process.env.OPENCODE_CONFIG_DIR, { recursive: true });
    writeFileSync(
      join(process.env.OPENCODE_CONFIG_DIR, "aft.json"),
      JSON.stringify({ lsp: { auto_install: false }, semantic_search: false }),
      "utf8",
    );
  });

  afterEach(() => {
    if (prevPath === undefined) delete process.env.PATH;
    else process.env.PATH = prevPath;
    if (prevHome === undefined) delete process.env.HOME;
    else process.env.HOME = prevHome;
    if (prevXdgDataHome === undefined) delete process.env.XDG_DATA_HOME;
    else process.env.XDG_DATA_HOME = prevXdgDataHome;
    if (prevXdgCacheHome === undefined) delete process.env.XDG_CACHE_HOME;
    else process.env.XDG_CACHE_HOME = prevXdgCacheHome;
    if (prevOpenCodeConfigDir === undefined) delete process.env.OPENCODE_CONFIG_DIR;
    else process.env.OPENCODE_CONFIG_DIR = prevOpenCodeConfigDir;
    delete process.env.AFT_MIGRATION_ARGS_LOG;
    rmSync(tempDir, { recursive: true, force: true });
  });

  function createLegacyRoot(): string {
    const legacyRoot = join(
      process.env.XDG_DATA_HOME as string,
      "opencode",
      "storage",
      "plugin",
      "aft",
    );
    mkdirSync(legacyRoot, { recursive: true });
    writeFileSync(join(legacyRoot, "warned_tools.json"), "{}", "utf8");
    return legacyRoot;
  }

  async function loadPlugin(): Promise<OpenCodePlugin> {
    const mod = await import(`../index.js?migration-bootstrap-${Date.now()}-${Math.random()}`);
    return mod.default;
  }

  test("opencode_plugin_calls_ensureStorageMigrated_with_opencode_harness", async () => {
    const legacyRoot = createLegacyRoot();
    const plugin = await loadPlugin();
    const hooks = (await plugin({
      directory: tempDir,
      client: {},
    } as Parameters<OpenCodePlugin>[0])) as {
      dispose?: () => Promise<void>;
    };

    const argv = readFileSync(argsLog, "utf8").trim().split("\n");
    expect(argv).toContain("migrate-storage");
    expect(argv).toContain("--harness");
    expect(argv).toContain("opencode");
    expect(argv).toContain("--from");
    expect(argv).toContain(legacyRoot);
    expect(argv).toContain("--to");
    expect(argv).toContain(join(process.env.XDG_DATA_HOME as string, "cortexkit", "aft"));

    await hooks.dispose?.();
  });

  test("opencode_plugin_aborts_on_migration_error", async () => {
    createLegacyRoot();
    writeFakeAft(5);
    const plugin = await loadPlugin();

    await expect(
      plugin({ directory: tempDir, client: {} } as Parameters<OpenCodePlugin>[0]),
    ).rejects.toThrow(/AFT storage migration failed.*exit 5/);
  });
});
