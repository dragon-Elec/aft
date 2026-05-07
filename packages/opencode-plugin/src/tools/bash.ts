import type { BridgeRequestOptions } from "@cortexkit/aft-bridge";
import type { ToolContext, ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import { trackBgTask } from "../bg-notifications.js";
import { storeToolMetadata } from "../metadata-store.js";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";
import { runAsk } from "./permissions.js";

const z = tool.schema;
const METADATA_PREVIEW_LIMIT = 30 * 1024;
const DEFAULT_BASH_TIMEOUT_MS = 30_000;
const BASH_TRANSPORT_TIMEOUT_OVERHEAD_MS = 5_000;
const FOREGROUND_POLL_INTERVAL_MS = 100;

const BASH_DESCRIPTION = `Hoisted bash tool with output compression, command rewriting to AFT tools, and optional background execution. By default, output is compressed; pass compressed: false for raw output. Pass background: true to spawn in the background and get a task_id for bash_status/bash_kill.`;

interface PermissionAsk {
  kind: "external_directory" | "bash";
  patterns: string[];
  always: string[];
}

type BridgeCaller = typeof callBridge;

async function withPermissionLoop(
  ctx: PluginContext,
  runtime: ToolContext,
  params: Record<string, unknown>,
  bridgeCall: BridgeCaller,
  options?: BridgeRequestOptions,
): ReturnType<BridgeCaller> {
  const first = await bridgeCall(ctx, runtime, "bash", params, options);
  if (first.success !== false || first.code !== "permission_required") return first;

  const asks = Array.isArray(first.asks) ? (first.asks as PermissionAsk[]) : [];
  const permissionsGranted: string[] = [];
  for (const ask of asks) {
    const permission = ask.kind === "external_directory" ? "external_directory" : "bash";
    await runAsk(
      runtime.ask({
        permission,
        patterns: ask.patterns,
        always: ask.always,
        metadata: {},
      }),
    );
    permissionsGranted.push(...(ask.always.length > 0 ? ask.always : ask.patterns));
  }

  const second = await bridgeCall(
    ctx,
    runtime,
    "bash",
    { ...params, permissions_granted: permissionsGranted },
    options,
  );
  if (second.success === false && second.code === "permission_required") {
    throw new Error("bash permission retry failed");
  }
  return second;
}

export function createBashTool(ctx: PluginContext): ToolDefinition {
  return {
    description: BASH_DESCRIPTION,
    args: {
      command: z
        .string()
        .describe(
          "Shell command to execute through AFT's unified bash schema. Supports normal shell syntax, pipes, redirection, and command rewriting to dedicated AFT tools when available.",
        ),
      timeout: z
        .number()
        .optional()
        .describe(
          "Maximum execution time in milliseconds for foreground commands. Defaults to 30000 (30 seconds) when omitted. For commands expected to run longer than 30s (builds, installs, full test suites), use background: true instead.",
        ),
      workdir: z
        .string()
        .optional()
        .describe(
          "Working directory for command execution. Relative paths resolve through the bridge; defaults to the current tool context/project root when omitted.",
        ),
      description: z
        .string()
        .optional()
        .describe(
          "Short 5-10 word human-readable summary shown in OpenCode UI metadata instead of raw shell syntax.",
        ),
      background: z
        .boolean()
        .optional()
        .describe(
          "When true, spawn the command in the background and return a task_id for bash_status/bash_kill instead of waiting for completion. Defaults to false.",
        ),
      compressed: z
        .boolean()
        .optional()
        .describe(
          "When true or omitted, return compressed output with noisy terminal control sequences reduced. Set to false for raw output.",
        ),
    },
    execute: async (args, context) => {
      let accumulatedOutput = "";
      const description = args.description as string | undefined;
      const metadata = (context as { metadata?: (data: Record<string, unknown>) => void }).metadata;
      const command = args.command as string;
      const cwd = (args.workdir as string | undefined) ?? context.directory;
      const shellEnv = await ctx.plugin?.trigger?.(
        "shell.env",
        { cwd, sessionID: context.sessionID, callID: getCallID(context) },
        { env: {} },
      );

      const data = await withPermissionLoop(
        ctx,
        context,
        {
          command,
          timeout: args.timeout,
          workdir: args.workdir,
          env: shellEnv?.env ?? {},
          description,
          background: args.background,
          notify_on_completion: args.background === true,
          compressed: args.compressed,
          permissions_requested: true,
        },
        callBridge,
        {
          transportTimeoutMs: bashTransportTimeoutMs(args.timeout as number | undefined),
          // Rust bash has its own watchdog that kills the child shell on the
          // bash-level timeout (`args.timeout`) and returns a normal timed_out
          // response well before our transport timeout fires. If we hit the
          // transport deadline anyway it means the response is just late —
          // don't sacrifice the bridge (and all its warm state) for that.
          keepBridgeOnTimeout: true,
          onProgress: ({ text }) => {
            accumulatedOutput = preview(accumulatedOutput + text);
            metadata?.({ output: accumulatedOutput, description });
          },
        },
      );

      if (data.success === false) {
        throw new Error((data.message as string) || "bash failed");
      }

      if (data.status === "running" && typeof data.task_id === "string") {
        const callID = getCallID(context);
        const taskId = data.task_id;
        if (args.background === true) {
          trackBgTask(context.sessionID, taskId);
          const startedLine = formatBackgroundLaunch(taskId);
          const metadataPayload = { description, output: startedLine, status: "running", taskId };
          metadata?.(metadataPayload);
          if (callID) {
            storeToolMetadata(context.sessionID, callID, {
              title: description ?? shortenCommand(command),
              metadata: metadataPayload,
            });
          }
          return startedLine;
        }

        const waitTimeoutMs = (args.timeout as number | undefined) ?? DEFAULT_BASH_TIMEOUT_MS;
        const startedAt = Date.now();
        while (true) {
          const status = await callBridge(ctx, context, "bash_status", { task_id: taskId });
          if (status.success === false) {
            throw new Error((status.message as string | undefined) ?? "bash_status failed");
          }
          if (isTerminalStatus(status.status)) {
            const rendered = formatForegroundResult(status);
            const metadataPayload = foregroundMetadata(description, status, rendered);
            metadata?.(metadataPayload);
            if (callID) {
              storeToolMetadata(context.sessionID, callID, {
                title: description ?? shortenCommand(command),
                metadata: metadataPayload,
              });
            }
            return rendered;
          }
          if (Date.now() - startedAt >= waitTimeoutMs) {
            const promoted = await callBridge(ctx, context, "bash_promote", { task_id: taskId });
            if (promoted.success === false) {
              throw new Error((promoted.message as string | undefined) ?? "bash_promote failed");
            }
            trackBgTask(context.sessionID, taskId);
            const message = formatPromotionMessage(
              taskId,
              command,
              args.timeout as number | undefined,
            );
            const metadataPayload = { description, output: message, status: "running", taskId };
            metadata?.(metadataPayload);
            if (callID) {
              storeToolMetadata(context.sessionID, callID, {
                title: description ?? shortenCommand(command),
                metadata: metadataPayload,
              });
            }
            return message;
          }
          await sleep(FOREGROUND_POLL_INTERVAL_MS);
        }
      }

      const output = (data.output as string | undefined) ?? "";
      const metadataOutput = preview(output);
      const exit = data.exit_code as number | undefined;
      const truncated = data.truncated as boolean | undefined;
      const outputPath = data.output_path as string | undefined;
      const timedOut = data.timed_out === true;
      const callID = getCallID(context);
      const metadataPayload = {
        description,
        output: metadataOutput,
        exit,
        truncated,
        ...(outputPath ? { outputPath } : {}),
      };

      metadata?.(metadataPayload);
      if (callID) {
        storeToolMetadata(context.sessionID, callID, {
          title: description ?? shortenCommand(command),
          metadata: metadataPayload,
        });
      }

      // Agent-visible output is the raw bash output (matches OpenCode's native
      // bash contract). Exit code, truncation, output path are UI metadata —
      // they go through metadata?.() above. We surface the bare minimum the
      // agent NEEDS to know directly in the text:
      //   - non-zero exit code (agent must be able to detect command failure)
      //   - timeout marker (separate signal beyond exit 124)
      //   - truncation pointer (so agent knows full output exists on disk)
      let rendered = output;
      if (truncated && outputPath) {
        rendered += `\n[output truncated; full output at ${outputPath}]`;
      }
      if (timedOut) {
        rendered += `\n[command timed out]`;
      }
      if (typeof exit === "number" && exit !== 0) {
        rendered += `\n[exit code: ${exit}]`;
      }
      return rendered;
    },
  };
}

export function createBashStatusTool(ctx: PluginContext): ToolDefinition {
  return {
    description:
      "Check the status and captured output of a background bash task spawned with bash({ background: true }). Returns status (running | completed | failed | killed | timed_out), exit code, duration, and a preview of captured output.",
    args: {
      taskId: z
        .string()
        .describe("Background task ID returned by bash({ background: true }), e.g. bgb-6b454047."),
    },
    execute: async (args, context) => {
      const data = await callBridge(ctx, context, "bash_status", {
        task_id: args.taskId as string,
      });
      if (data.success === false) {
        throw new Error((data.message as string | undefined) ?? "bash_status failed");
      }
      const status = data.status as string;
      const exit = typeof data.exit_code === "number" ? ` (exit ${data.exit_code})` : "";
      const dur =
        typeof data.duration_ms === "number" ? ` ${Math.round(data.duration_ms / 1000)}s` : "";
      let text = `Task ${args.taskId}: ${status}${exit}${dur}`;
      const preview = data.output_preview as string | undefined;
      if (preview && status !== "running") {
        text += `\n${preview.slice(0, 2000)}`;
      }
      // For still-running tasks, append the same anti-polling reminder we add
      // to the initial spawn line. Agents that ignore the spawn-line guidance
      // and call bash_status anyway get the reminder again here so they don't
      // call back-to-back. Terminal statuses (completed/failed/killed/timed_out)
      // get NO such suffix — we want clean output once the task is actually
      // done and the agent is ready to consume the result.
      if (status === "running") {
        text += `\nA completion reminder will be delivered automatically; don't poll.`;
      }
      return text;
    },
  };
}

export function createBashKillTool(ctx: PluginContext): ToolDefinition {
  return {
    description:
      "Terminate a running background bash task spawned with bash({ background: true }). Returns confirmation of kill or an error if the task already finished.",
    args: {
      taskId: z
        .string()
        .describe("Background task ID returned by bash({ background: true }), e.g. bgb-6b454047."),
    },
    execute: async (args, context) => {
      const data = await callBridge(ctx, context, "bash_kill", {
        task_id: args.taskId as string,
      });
      if (data.success === false) {
        throw new Error((data.message as string | undefined) ?? "bash_kill failed");
      }
      return `Task ${args.taskId}: ${String(data.status ?? "killed")}`;
    },
  };
}

function bashTransportTimeoutMs(timeout: number | undefined): number {
  const bashTimeout = timeout ?? DEFAULT_BASH_TIMEOUT_MS;
  return Math.max(30_000, bashTimeout + BASH_TRANSPORT_TIMEOUT_OVERHEAD_MS);
}

function preview(output: string): string {
  return output.length <= METADATA_PREVIEW_LIMIT ? output : output.slice(-METADATA_PREVIEW_LIMIT);
}

function isTerminalStatus(status: unknown): boolean {
  return (
    status === "completed" || status === "failed" || status === "killed" || status === "timed_out"
  );
}

function formatBackgroundLaunch(taskId: string): string {
  return `Background task started: ${taskId}. A completion reminder will be delivered automatically; don't poll bash_status.`;
}

function formatPromotionMessage(
  taskId: string,
  command: string,
  timeout: number | undefined,
): string {
  const waited = timeout ?? DEFAULT_BASH_TIMEOUT_MS;
  return `Foreground bash exceeded ${waited}ms and was promoted to background: ${taskId}. A completion reminder will be delivered automatically; use bash_status({ taskId: "${taskId}" }) to inspect output or bash_kill({ taskId: "${taskId}" }) to terminate. Command: ${shortenCommand(command)}`;
}

function formatForegroundResult(data: Record<string, unknown>): string {
  const output = (data.output_preview as string | undefined) ?? "";
  const outputPath = data.output_path as string | undefined;
  const truncated = data.output_truncated === true;
  const status = data.status as string | undefined;
  const exit = data.exit_code as number | undefined;
  let rendered = output;
  if (truncated && outputPath) {
    rendered += `\n[output truncated; full output at ${outputPath}]`;
  }
  if (status === "timed_out") {
    rendered += `\n[command timed out]`;
  }
  if (typeof exit === "number" && exit !== 0) {
    rendered += `\n[exit code: ${exit}]`;
  }
  return rendered;
}

function foregroundMetadata(
  description: string | undefined,
  data: Record<string, unknown>,
  rendered: string,
): Record<string, unknown> {
  const outputPath = data.output_path as string | undefined;
  return {
    description,
    output: preview(rendered),
    exit: data.exit_code as number | undefined,
    truncated: data.output_truncated as boolean | undefined,
    ...(outputPath ? { outputPath } : {}),
  };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function getCallID(ctx: unknown): string | undefined {
  const c = ctx as { callID?: string; callId?: string; call_id?: string };
  return c.callID ?? c.callId ?? c.call_id;
}

function shortenCommand(command: string): string {
  const collapsed = command.replace(/\s+/g, " ").trim();
  return collapsed.length <= 80 ? collapsed : `${collapsed.slice(0, 77)}...`;
}
