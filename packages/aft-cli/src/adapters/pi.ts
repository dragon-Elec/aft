import { execSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { dirSize } from "../lib/fs-util.js";
import { detectJsoncFile } from "../lib/jsonc.js";
import { getTmpLogPath } from "../lib/paths.js";
import type {
  HarnessAdapter,
  HarnessConfigPaths,
  PluginCacheInfo,
  PluginEntryResult,
} from "./types.js";

const PLUGIN_NAME = "@cortexkit/aft-pi";
const PLUGIN_ENTRY = `npm:${PLUGIN_NAME}`;

function getPiAgentDir(): string {
  return join(homedir(), ".pi", "agent");
}

/**
 * Pi extensions are installed via `pi install npm:<package>` and managed by
 * Pi itself — there's no user-editable registration file equivalent to
 * OpenCode's `plugin` array. We detect installation by looking at Pi's
 * extension index (if it exists) and fall back to probing the host.
 */
function readPiExtensionIndex(): { installed: string[]; path: string | null } {
  const candidates = [
    join(getPiAgentDir(), "extensions.json"),
    join(getPiAgentDir(), "extensions.jsonc"),
    join(getPiAgentDir(), "config.json"),
    join(getPiAgentDir(), "config.jsonc"),
  ];
  for (const path of candidates) {
    if (!existsSync(path)) continue;
    try {
      const raw = readFileSync(path, "utf-8");
      const trimmed = raw.replace(/^\uFEFF/, "");
      const value = JSON.parse(trimmed) as Record<string, unknown>;
      const extensions = (value.extensions ?? value.plugins ?? []) as unknown;
      if (Array.isArray(extensions)) {
        const installed = extensions
          .map((e) =>
            typeof e === "string"
              ? e
              : typeof (e as { name?: string })?.name === "string"
                ? (e as { name: string }).name
                : "",
          )
          .filter((name): name is string => name.length > 0);
        return { installed, path };
      }
    } catch {
      // try next
    }
  }
  return { installed: [], path: null };
}

function piHasOurExtension(): boolean {
  const { installed } = readPiExtensionIndex();
  return installed.some(
    (entry) => entry === PLUGIN_NAME || entry === PLUGIN_ENTRY || entry.includes("aft-pi"),
  );
}

export class PiAdapter implements HarnessAdapter {
  readonly kind = "pi" as const;
  readonly displayName = "Pi";
  readonly pluginPackageName = PLUGIN_NAME;
  readonly pluginEntryWithVersion = PLUGIN_ENTRY;

  isInstalled(): boolean {
    try {
      execSync("pi --version", { stdio: "ignore" });
      return true;
    } catch {
      return false;
    }
  }

  getHostVersion(): string | null {
    try {
      return execSync("pi --version", { encoding: "utf-8", stdio: "pipe" }).trim();
    } catch {
      return null;
    }
  }

  detectConfigPaths(): HarnessConfigPaths {
    const configDir = getPiAgentDir();
    // Pi doesn't have a user-editable "harness config" analogous to opencode.jsonc;
    // point at the likely extensions index for diagnostic purposes only.
    const index = readPiExtensionIndex();
    const aft = detectJsoncFile(configDir, "aft");
    return {
      configDir,
      harnessConfig: index.path ?? join(configDir, "extensions.json"),
      harnessConfigFormat: index.path ? "json" : "none",
      aftConfig: aft.path,
      aftConfigFormat: aft.format,
    };
  }

  hasPluginEntry(): boolean {
    return piHasOurExtension();
  }

  async ensurePluginEntry(): Promise<PluginEntryResult> {
    if (this.hasPluginEntry()) {
      return {
        ok: true,
        action: "already_present",
        message: `${PLUGIN_NAME} is already installed`,
        configPath: this.detectConfigPaths().harnessConfig,
      };
    }
    if (!this.isInstalled()) {
      return {
        ok: false,
        action: "error",
        message: "pi CLI not found on PATH. Install Pi first: https://github.com/badlogic/pi-mono",
        configPath: this.detectConfigPaths().harnessConfig,
      };
    }
    try {
      execSync(`pi install ${PLUGIN_ENTRY}`, { stdio: "inherit" });
      return {
        ok: true,
        action: "added",
        message: `Installed ${PLUGIN_ENTRY} via \`pi install\``,
        configPath: this.detectConfigPaths().harnessConfig,
      };
    } catch (error) {
      return {
        ok: false,
        action: "error",
        message: `Failed to run \`pi install ${PLUGIN_ENTRY}\`: ${error instanceof Error ? error.message : String(error)}`,
        configPath: this.detectConfigPaths().harnessConfig,
      };
    }
  }

  getPluginCacheInfo(): PluginCacheInfo {
    // Pi manages its own extension cache location; doctor reports whether the
    // extension is registered, not an on-disk cache path. Best-effort: look
    // for a node_modules install under Pi's agent dir.
    const candidates = [
      join(getPiAgentDir(), "node_modules", "@cortexkit", "aft-pi", "package.json"),
      join(getPiAgentDir(), "extensions", "node_modules", "@cortexkit", "aft-pi", "package.json"),
    ];
    for (const candidate of candidates) {
      if (!existsSync(candidate)) continue;
      try {
        const pkg = JSON.parse(readFileSync(candidate, "utf-8")) as { version?: unknown };
        const cached = typeof pkg.version === "string" ? pkg.version : undefined;
        return {
          path: candidate,
          cached,
          latest: undefined,
          exists: true,
        };
      } catch {
        // next
      }
    }
    return {
      path: join(getPiAgentDir(), "extensions"),
      exists: false,
    };
  }

  getStorageDir(): string {
    // Pi's storage dir convention from packages/pi-plugin/src/index.ts.
    return join(getPiAgentDir(), "aft");
  }

  getLogFile(): string {
    return getTmpLogPath("aft-pi.log");
  }

  getInstallHint(): string {
    return "Install Pi: https://github.com/badlogic/pi-mono";
  }

  async clearPluginCache(_force: boolean): Promise<{
    action: "cleared" | "up_to_date" | "not_found" | "not_applicable" | "error";
    path: string;
    cached?: string;
    latest?: string;
    error?: string;
  }> {
    // Pi owns its extension cache — we don't touch it from here. `doctor --force`
    // is an OpenCode-specific remedy for the npm/npx package cache that
    // OpenCode populates under `~/.cache/opencode/packages/`.
    return {
      action: "not_applicable",
      path: this.getPluginCacheInfo().path,
    };
  }

  describeStorageSubtrees(): Record<string, number> {
    const storage = this.getStorageDir();
    return {
      index: dirSize(join(storage, "index")),
      semantic: dirSize(join(storage, "semantic")),
      backups: dirSize(join(storage, "backups")),
      url_cache: dirSize(join(storage, "url_cache")),
      onnxruntime: dirSize(join(storage, "onnxruntime")),
    };
  }
}
