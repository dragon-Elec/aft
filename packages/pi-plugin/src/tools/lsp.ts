/**
 * lsp_diagnostics — on-demand LSP diagnostics.
 * Edit/write flows already inject diagnostics inline; this tool is for
 * explicit checks on a file or directory.
 */

import { StringEnum } from "@earendil-works/pi-ai";
import type { AgentToolResult, ExtensionAPI, Theme } from "@earendil-works/pi-coding-agent";
import { type Static, Type } from "typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";
import {
  accentPath,
  asNumber,
  asRecord,
  asRecords,
  asString,
  distinctCount,
  extractStructuredPayload,
  groupByFile,
  type RenderContextLike,
  renderErrorResult,
  renderSections,
  renderToolCall,
  severityBadge,
  shortenPath,
} from "./render-helpers.js";

const LspDiagnosticsParams = Type.Object({
  filePath: Type.Optional(
    Type.String({ description: "File to get diagnostics for (mutually exclusive with directory)" }),
  ),
  directory: Type.Optional(
    Type.String({
      description: "Directory to get diagnostics for (mutually exclusive with filePath)",
    }),
  ),
  severity: Type.Optional(
    StringEnum(["error", "warning", "information", "hint", "all"] as const, {
      description: "Filter by severity (default: all)",
    }),
  ),
  waitMs: Type.Optional(
    Type.Number({
      description: "Wait N ms for fresh diagnostics (max 10000, default: 0)",
    }),
  ),
});

/** Exported for renderer unit tests. */
export function buildDiagnosticsSections(payload: unknown, theme: Theme): string[] {
  const response = asRecord(payload);
  if (!response) return [theme.fg("muted", "No diagnostics available.")];

  const diagnostics = asRecords(response.diagnostics);
  const total = asNumber(response.total) ?? diagnostics.length;
  const filesWithErrors =
    asNumber(response.files_with_errors) ??
    distinctCount(
      diagnostics
        .filter((diag) => asString(diag.severity) === "error")
        .map((diag) => asString(diag.file)),
    );
  const filesCount = distinctCount(diagnostics.map((diag) => asString(diag.file)));
  const sections = [
    `${theme.fg(total > 0 ? "warning" : "success", `${total} diagnostic${total === 1 ? "" : "s"}`)} ${theme.fg("muted", `across ${filesCount} file${filesCount === 1 ? "" : "s"}, ${filesWithErrors} error file${filesWithErrors === 1 ? "" : "s"}`)}`,
  ];

  if (diagnostics.length === 0) {
    sections.push(theme.fg("muted", "No diagnostics found."));
    return sections;
  }

  const grouped = groupByFile(diagnostics, (diag) => asString(diag.file));
  for (const [file, fileDiagnostics] of grouped.entries()) {
    const lines = [theme.fg("accent", shortenPath(file))];
    fileDiagnostics.forEach((diagnostic) => {
      const severity = asString(diagnostic.severity) ?? "information";
      const line = asNumber(diagnostic.line) ?? 0;
      const column = asNumber(diagnostic.column) ?? 0;
      const code = asString(diagnostic.code);
      const message = asString(diagnostic.message) ?? "(no message)";
      const location = `${line}:${column}`;
      lines.push(
        `  ${severityBadge(theme, severity)} ${location}${code ? ` ${theme.fg("muted", code)}` : ""} ${message}`,
      );
    });
    sections.push(lines.join("\n"));
  }

  return sections;
}

/** Exported for renderer unit tests. */
export function renderDiagnosticsCall(
  args: Static<typeof LspDiagnosticsParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  const target = args.filePath ?? args.directory;
  const summary = [
    target ? accentPath(theme, target) : undefined,
    args.severity ? theme.fg("toolOutput", args.severity) : undefined,
  ]
    .filter(Boolean)
    .join(" ");
  return renderToolCall("lsp diagnostics", summary, theme, context);
}

/** Exported for renderer unit tests. */
export function renderDiagnosticsResult(
  result: AgentToolResult<unknown>,
  theme: Theme,
  context: RenderContextLike,
) {
  if (context.isError) return renderErrorResult(result, "lsp diagnostics failed", theme, context);
  return renderSections(buildDiagnosticsSections(extractStructuredPayload(result), theme), context);
}

export function registerLspTools(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "lsp_diagnostics",
    label: "lsp diagnostics",
    description:
      "On-demand LSP file/scope check. NOT a project-wide type checker — use `tsc --noEmit`, `cargo check`, `pyright` etc. for full coverage.\n\nHonesty: `total: 0` is only clean when `complete: true` AND `lsp_servers_used[].status` includes `pull_ok`. Empty `lsp_servers_used`, or any `binary_not_installed`/`spawn_failed`/`no_root_marker`/`push_only` without diagnostics means the file wasn't actually checked — say so, don't report 'clean'. For per-server breakdown run `npx @cortexkit/aft doctor lsp <filePath>`.",
    parameters: LspDiagnosticsParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof LspDiagnosticsParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const hasFile = typeof params.filePath === "string" && params.filePath.length > 0;
      const hasDir = typeof params.directory === "string" && params.directory.length > 0;
      if (hasFile && hasDir) {
        throw new Error(
          "'filePath' and 'directory' are mutually exclusive — provide one or neither",
        );
      }
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {};
      if (hasFile) req.file = params.filePath;
      if (hasDir) req.directory = params.directory;
      if (params.severity !== undefined) req.severity = params.severity;
      if (params.waitMs !== undefined) req.wait_ms = params.waitMs;
      const response = await callBridge(bridge, "lsp_diagnostics", req, extCtx);
      return textResult(JSON.stringify(response, null, 2));
    },
    renderCall(args, theme, context) {
      return renderDiagnosticsCall(args, theme, context);
    },
    renderResult(result, _options, theme, context) {
      return renderDiagnosticsResult(result, theme, context);
    },
  });
}
