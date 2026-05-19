import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { findBinary } from "./resolver.js";

export type MigrationHarness = "opencode" | "pi";

export interface MigrationOptions {
  harness: MigrationHarness;
  binaryPath?: string;
  logger?: {
    warn?: (msg: string) => void;
    info?: (msg: string) => void;
    log?: (msg: string) => void;
  };
  timeoutMs?: number;
}

const SOURCE_MARKER = ".migrated_to_cortexkit";
const TARGET_MARKER = ".migrated_from_legacy";
const DEFAULT_TIMEOUT_MS = 120_000;

function dataHome(): string {
  if (process.env.XDG_DATA_HOME) return process.env.XDG_DATA_HOME;
  if (process.platform === "darwin") return join(homeDir(), "Library", "Application Support");
  if (process.platform === "win32") {
    return process.env.LOCALAPPDATA || process.env.APPDATA || join(homeDir(), "AppData", "Local");
  }
  return join(homeDir(), ".local", "share");
}

function homeDir(): string {
  if (process.platform === "win32") return process.env.USERPROFILE || process.env.HOME || homedir();
  return process.env.HOME || homedir();
}

export function resolveLegacyStorageRoot(harness: MigrationHarness): string {
  if (harness === "pi") return join(homeDir(), ".pi", "agent", "aft");
  return join(dataHome(), "opencode", "storage", "plugin", "aft");
}

export function resolveCortexKitStorageRoot(): string {
  return join(dataHome(), "cortexkit", "aft");
}

function tail(value: string | undefined): string {
  if (!value) return "";
  return value.split("\n").slice(-20).join("\n").trim();
}

function spawnErrorLabel(error: Error): string {
  const code = "code" in error ? String((error as Error & { code?: unknown }).code ?? "") : "";
  return [code, error.message].filter(Boolean).join(": ");
}

function migrationLogPath(
  newRoot: string,
  harness: MigrationHarness,
  logger?: MigrationOptions["logger"],
): string {
  const desired = join(newRoot, "logs", "migration", `${harness}-${Date.now()}.jsonl`);
  try {
    mkdirSync(dirname(desired), { recursive: true });
    return desired;
  } catch (err) {
    const fallback = join(tmpdir(), `aft-migration-${harness}-${Date.now()}.jsonl`);
    logger?.warn?.(
      `Failed to create AFT migration log directory ${dirname(desired)}: ${err instanceof Error ? err.message : String(err)}. ` +
        `Using fallback log path ${fallback}.`,
    );
    return fallback;
  }
}

export async function ensureStorageMigrated(opts: MigrationOptions): Promise<void> {
  const legacyRoot = resolveLegacyStorageRoot(opts.harness);
  const newRoot = resolveCortexKitStorageRoot();
  const sourceMarker = join(legacyRoot, SOURCE_MARKER);
  const targetMarker = join(newRoot, opts.harness, TARGET_MARKER);

  if (existsSync(sourceMarker) || existsSync(targetMarker)) return;

  // Commit 4's migrate-storage treats a missing source as a no-op, but plugin
  // bootstrap should not require a binary just to discover a fresh install.
  if (!existsSync(legacyRoot)) return;

  const logPath = migrationLogPath(newRoot, opts.harness, opts.logger);
  const binaryPath = opts.binaryPath ?? (await findBinary());
  const info = opts.logger?.info ?? opts.logger?.log;
  info?.(`Running AFT storage migration for ${opts.harness}: ${legacyRoot} -> ${newRoot}`);

  const result = spawnSync(
    binaryPath,
    [
      "migrate-storage",
      "--from",
      legacyRoot,
      "--to",
      newRoot,
      "--harness",
      opts.harness,
      "--log",
      logPath,
    ],
    {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      timeout: opts.timeoutMs ?? DEFAULT_TIMEOUT_MS,
    },
  );

  if (!result.error && result.status === 0) return;

  const detail = result.error
    ? `spawn error ${spawnErrorLabel(result.error)}`
    : result.status === null
      ? `terminated by signal ${result.signal ?? "unknown"}`
      : `exit ${result.status}`;
  const stderrTail = tail(result.stderr);
  const stdoutTail = tail(result.stdout);

  throw new Error(
    `AFT storage migration failed (${detail}). ` +
      `Harness: ${opts.harness}. Legacy: ${legacyRoot}. Target: ${newRoot}. ` +
      `See log: ${logPath}. ` +
      `Plugin load aborted to prevent legacy/new state divergence.` +
      (stderrTail ? ` Stderr tail: ${stderrTail}` : "") +
      (stdoutTail ? ` Stdout tail: ${stdoutTail}` : ""),
  );
}
