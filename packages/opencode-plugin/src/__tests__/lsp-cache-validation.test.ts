import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readdirSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runAutoInstall } from "../lsp-auto-install.js";
import { runGithubAutoInstall } from "../lsp-github-install.js";

const roots = new Set<string>();

function tempCache(): string {
  const root = mkdtempSync(join(tmpdir(), "aft-lsp-cache-validation-"));
  roots.add(root);
  process.env.AFT_CACHE_DIR = root;
  return root;
}

afterEach(() => {
  delete process.env.AFT_CACHE_DIR;
  for (const root of roots) rmSync(root, { recursive: true, force: true });
  roots.clear();
});

describe("cached LSP validation before lsp_paths_extra", () => {
  test("npm cached binary with mismatched sha is excluded and quarantined", () => {
    const root = tempCache();
    const pkgDir = join(root, "lsp-packages", "pyright");
    const binDir = join(pkgDir, "node_modules", ".bin");
    mkdirSync(binDir, { recursive: true });
    writeFileSync(join(binDir, "pyright"), "tampered");
    writeFileSync(
      join(pkgDir, ".aft-installed"),
      JSON.stringify({ version: "1.1.300", installedAt: "now", sha256: "0".repeat(64) }),
    );

    const result = runAutoInstall(root, {
      autoInstall: false,
      graceDays: 7,
      versions: {},
      disabled: new Set(),
    });

    expect(result.cachedBinDirs).not.toContain(binDir);
    const quarantine = join(root, "lsp-packages", ".quarantine", "pyright");
    expect(existsSync(quarantine)).toBe(true);
    expect(readdirSync(quarantine).length).toBeGreaterThan(0);
  });

  test("GitHub cached binary with mismatched sha is excluded and quarantined", () => {
    const root = tempCache();
    const pkgDir = join(root, "lsp-binaries", "clangd");
    const binDir = join(pkgDir, "bin");
    mkdirSync(binDir, { recursive: true });
    writeFileSync(join(binDir, "clangd"), "tampered");
    writeFileSync(
      join(pkgDir, ".aft-installed"),
      JSON.stringify({ version: "21.1.0", installedAt: "now", sha256: "0".repeat(64) }),
    );

    const result = runGithubAutoInstall(new Set(), {
      autoInstall: false,
      graceDays: 7,
      versions: {},
      disabled: new Set(),
    });

    expect(result.cachedBinDirs).not.toContain(binDir);
    const quarantine = join(root, "lsp-binaries", ".quarantine", "clangd");
    expect(existsSync(quarantine)).toBe(true);
    expect(readdirSync(quarantine).length).toBeGreaterThan(0);
  });
});
