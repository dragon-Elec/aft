import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callBridge, optionalInt } from "./_shared.js";

const z = tool.schema;
/**
 * Tool definitions for LSP commands: diagnostics.
 */
export function lspTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const diagnosticsTool: ToolDefinition = {
    description:
      "On-demand LSP file/scope check. NOT a project-wide type checker — use `tsc --noEmit`, `cargo check`, `pyright` etc. for full coverage.\n" +
      "\n" +
      "Honesty: `total: 0` is only clean when `complete: true` AND `lsp_servers_used[].status` includes `pull_ok`. Empty `lsp_servers_used`, or any `binary_not_installed`/`spawn_failed`/`no_root_marker`/`push_only` without diagnostics means the file wasn't actually checked — say so, don't report 'clean'. For per-server breakdown run `npx @cortexkit/aft doctor lsp <filePath>`.",
    args: {
      filePath: z
        .string()
        .optional()
        .describe(
          "Path to a file to check. Mutually exclusive with 'directory'. Omit both to dump all cached diagnostics.",
        ),
      directory: z
        .string()
        .optional()
        .describe(
          "Path to a directory. Returns cached diagnostics + workspace pull; capped at 200 walked files.",
        ),
      severity: z
        .enum(["error", "warning", "information", "hint", "all"])
        .optional()
        .describe("Filter by severity (default: 'all')."),
      waitMs: optionalInt(1, 10_000).describe(
        "Wait up to N ms (max 10000) for push diagnostics. Push-only servers like bash-language-server and yaml-language-server — use after an edit.",
      ),
    },
    execute: async (args, context): Promise<string> => {
      const filePath = args.filePath || undefined; // treat empty string as absent
      const directory = args.directory || undefined;
      if (filePath !== undefined && directory !== undefined) {
        throw new Error(
          "'filePath' and 'directory' are mutually exclusive — provide one or neither",
        );
      }
      const params: Record<string, unknown> = {};
      if (filePath !== undefined) params.file = filePath;
      if (directory !== undefined) params.directory = directory;
      if (args.severity !== undefined) params.severity = args.severity;
      if (args.waitMs !== undefined) params.wait_ms = args.waitMs;
      const result = await callBridge(ctx, context, "lsp_diagnostics", params);
      if (result.success === false) {
        throw new Error((result.message as string) || "lsp_diagnostics failed");
      }
      return JSON.stringify(result);
    },
  };

  // When hoisting: register as lsp_diagnostics (override oh-my-opencode's)
  // When not hoisting: register as aft_lsp_diagnostics
  const hoisting = ctx.config.hoist_builtin_tools !== false;
  return {
    [hoisting ? "lsp_diagnostics" : "aft_lsp_diagnostics"]: diagnosticsTool,
  };
}
