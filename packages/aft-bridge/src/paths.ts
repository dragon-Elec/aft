import { existsSync, mkdirSync, renameSync } from "node:fs";
import { dirname, join } from "node:path";
import type { MigrationHarness } from "./migration.js";

export function resolveHarnessStoragePath(
  storageRoot: string,
  harness: MigrationHarness,
  ...segments: string[]
): string {
  return join(storageRoot, harness, ...segments);
}

export function repairRootScopedStorageFile(
  storageRoot: string,
  harness: MigrationHarness,
  fileName: string,
): string {
  const harnessPath = resolveHarnessStoragePath(storageRoot, harness, fileName);
  const rootPath = join(storageRoot, fileName);

  if (existsSync(harnessPath) || !existsSync(rootPath)) return harnessPath;

  try {
    mkdirSync(dirname(harnessPath), { recursive: true });
    renameSync(rootPath, harnessPath);
  } catch {
    // Best-effort compatibility repair. Callers still use the harness path so
    // new writes stop extending the root-scoped layout.
  }

  return harnessPath;
}
