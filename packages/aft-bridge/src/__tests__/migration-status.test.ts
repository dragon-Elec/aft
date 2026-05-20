/// <reference path="../bun-test.d.ts" />

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

describe("storage migration status", () => {
  let tempDir: string;
  let prevXdgDataHome: string | undefined;
  let prevHome: string | undefined;

  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "aft-migration-status-test-"));
    prevXdgDataHome = process.env.XDG_DATA_HOME;
    prevHome = process.env.HOME;
    process.env.XDG_DATA_HOME = tempDir;
    process.env.HOME = tempDir;
  });

  afterEach(() => {
    if (prevXdgDataHome === undefined) delete process.env.XDG_DATA_HOME;
    else process.env.XDG_DATA_HOME = prevXdgDataHome;
    if (prevHome === undefined) delete process.env.HOME;
    else process.env.HOME = prevHome;
    rmSync(tempDir, { recursive: true, force: true });
    mock.restore();
  });

  async function loadWithSpawn(result: unknown) {
    const spawnSync = mock(() => result);
    mock.module("node:child_process", () => ({ spawnSync }));
    const migration = await import(`../migration.js?status-${Date.now()}-${Math.random()}`);
    return { ...migration, spawnSync };
  }

  test("getMigrationStatus_returns_migrated_true_when_marker_exists", async () => {
    const payload = {
      harness: "opencode",
      target_root: join(tempDir, "cortexkit", "aft"),
      migrated: true,
      marker_path: join(tempDir, "cortexkit", "aft", "opencode", ".migrated_from_legacy"),
      migrated_at: "2026-05-19T15:00:00.123Z",
      source_path: "/legacy/aft",
      aft_version: "0.27.0",
    };
    const { getMigrationStatus, spawnSync } = await loadWithSpawn({
      status: 0,
      signal: null,
      error: undefined,
      stdout: `${JSON.stringify(payload)}\n`,
      stderr: "",
    });

    await expect(
      getMigrationStatus({ harness: "opencode", binaryPath: "/bin/aft" }),
    ).resolves.toEqual(payload);
    expect(spawnSync).toHaveBeenCalledWith(
      "/bin/aft",
      [
        "migrate-storage",
        "--status",
        "--to",
        join(tempDir, "cortexkit", "aft"),
        "--harness",
        "opencode",
      ],
      expect.any(Object),
    );
  });

  test("getMigrationStatus_returns_migrated_false_when_marker_absent", async () => {
    const payload = {
      harness: "opencode",
      target_root: join(tempDir, "cortexkit", "aft"),
      migrated: false,
    };
    const { getMigrationStatus } = await loadWithSpawn({
      status: 0,
      signal: null,
      error: undefined,
      stdout: `${JSON.stringify(payload)}\n`,
      stderr: "",
    });

    await expect(
      getMigrationStatus({ harness: "opencode", binaryPath: "/bin/aft" }),
    ).resolves.toEqual(payload);
  });

  test("getMigrationStatus_throws_on_invalid_json", async () => {
    const { getMigrationStatus } = await loadWithSpawn({
      status: 0,
      signal: null,
      error: undefined,
      stdout: "not-json\n",
      stderr: "",
    });

    await expect(
      getMigrationStatus({ harness: "opencode", binaryPath: "/bin/aft" }),
    ).rejects.toThrow(/invalid JSON/);
  });

  test("getMigrationStatus_handles_nonzero_exit", async () => {
    const { getMigrationStatus } = await loadWithSpawn({
      status: 1,
      signal: null,
      error: undefined,
      stdout: "",
      stderr: "failed",
    });

    await expect(
      getMigrationStatus({ harness: "opencode", binaryPath: "/bin/aft" }),
    ).rejects.toThrow(/exit 1.*failed/);
  });
});
