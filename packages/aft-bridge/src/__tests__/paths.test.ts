/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repairRootScopedStorageFile, resolveHarnessStoragePath } from "../paths.js";

const tempRoots = new Set<string>();

function createStorageRoot(): string {
  const root = mkdtempSync(join(tmpdir(), "aft-bridge-paths-"));
  tempRoots.add(root);
  return root;
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("harness storage paths", () => {
  test("resolveHarnessStoragePath scopes paths by harness", () => {
    const root = createStorageRoot();

    expect(resolveHarnessStoragePath(root, "opencode", "last_announced_version")).toBe(
      join(root, "opencode", "last_announced_version"),
    );
  });

  test("repairRootScopedStorageFile moves root copy when harness copy is absent", () => {
    const root = createStorageRoot();
    writeFileSync(join(root, "last-update-check.json"), "{}", "utf8");

    const path = repairRootScopedStorageFile(root, "opencode", "last-update-check.json");

    expect(path).toBe(join(root, "opencode", "last-update-check.json"));
    expect(existsSync(join(root, "last-update-check.json"))).toBe(false);
    expect(readFileSync(path, "utf8")).toBe("{}");
  });

  test("repairRootScopedStorageFile does not overwrite existing harness copy", () => {
    const root = createStorageRoot();
    writeFileSync(join(root, "last_announced_version"), "root", "utf8");
    const harnessPath = resolveHarnessStoragePath(root, "pi", "last_announced_version");
    mkdirSync(join(root, "pi"), { recursive: true });
    writeFileSync(harnessPath, "harness", "utf8");

    const path = repairRootScopedStorageFile(root, "pi", "last_announced_version");

    expect(path).toBe(harnessPath);
    expect(readFileSync(path, "utf8")).toBe("harness");
    expect(readFileSync(join(root, "last_announced_version"), "utf8")).toBe("root");
  });
});
