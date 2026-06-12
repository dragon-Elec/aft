//! Per-diagnostic classification for environment/setup failures vs real code issues.
//!
//! Environmental diagnostics (missing TypeScript install, JSON schema fetch
//! failures for editor tooling, etc.) may appear in the warm LSP set but must not
//! inflate the agent status bar `E`/`W` counts or `aft_inspect` diagnostics
//! summary totals — only per-diagnostic, not per-file.

use super::diagnostics::StoredDiagnostic;

/// True when this diagnostic reflects the tooling/environment, not project source.
pub fn is_environmental_diagnostic(diagnostic: &StoredDiagnostic) -> bool {
    let message = diagnostic.message.as_str();
    let code = diagnostic.code.as_deref().unwrap_or("");
    is_environmental_message(message, code)
}

fn is_environmental_message(message: &str, code: &str) -> bool {
    let lower = message.to_ascii_lowercase();

    // TypeScript language service / tsserver environment failures.
    if lower.contains("could not find a valid typescript installation") {
        return true;
    }

    // JSON / $schema fetch failures (vscode-json-languageservice and peers).
    if message_contains_schema_fetch_failure(&lower) {
        return true;
    }

    // Known TS codes for config / project setup (not per-line source defects).
    matches!(code, "18003" | "TS18003" | "6053" | "TS6053")
        || lower.contains("no inputs were found in config file")
}

fn message_contains_schema_fetch_failure(lower: &str) -> bool {
    const FETCH_VERBS: &[&str] = &[
        "failed to fetch",
        "failed to load",
        "failed to download",
        "failed to resolve",
        "failed to read",
        "unable to load",
        "unable to fetch",
        "unable to resolve",
        "could not load",
        "could not fetch",
        "could not resolve",
        "could not download",
        "error loading",
        "error fetching",
        "error resolving",
    ];
    FETCH_VERBS
        .iter()
        .any(|verb| lower.contains(verb) && lower.contains("schema"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::is_environmental_diagnostic;
    use crate::lsp::diagnostics::{DiagnosticSeverity, StoredDiagnostic};

    fn stored(message: &str, code: Option<&str>, source: Option<&str>) -> StoredDiagnostic {
        StoredDiagnostic {
            file: PathBuf::from("/repo/src/app.ts"),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 2,
            severity: DiagnosticSeverity::Error,
            message: message.into(),
            code: code.map(str::to_string),
            source: source.map(str::to_string),
        }
    }

    #[test]
    fn typescript_installation_is_environmental() {
        assert!(is_environmental_diagnostic(&stored(
            "Could not find a valid TypeScript installation. Try `npm i typescript`.",
            None,
            Some("typescript"),
        )));
    }

    #[test]
    fn schema_fetch_failure_is_environmental() {
        assert!(is_environmental_diagnostic(&stored(
            "Failed to fetch schema from https://example.com/schema.json: getaddrinfo ENOTFOUND",
            None,
            Some("json"),
        )));
    }

    #[test]
    fn real_syntax_error_is_not_environmental() {
        assert!(!is_environmental_diagnostic(&stored(
            "Type 'string' is not assignable to type 'number'.",
            Some("TS2322"),
            Some("typescript"),
        )));
    }

    #[test]
    fn global_types_typo_is_not_environmental() {
        assert!(!is_environmental_diagnostic(&stored(
            "Cannot find global type 'NotARealPackage'.",
            Some("TS2688"),
            Some("typescript"),
        )));
    }

    #[test]
    fn mixed_file_classifier_is_per_diagnostic() {
        let syntax = stored(
            "Cannot find name 'foo'.",
            Some("TS2304"),
            Some("typescript"),
        );
        let schema = stored(
            "Failed to load schema from https://cdn.example/pkg/schema.json",
            None,
            Some("json"),
        );
        assert!(!is_environmental_diagnostic(&syntax));
        assert!(is_environmental_diagnostic(&schema));
    }
}
