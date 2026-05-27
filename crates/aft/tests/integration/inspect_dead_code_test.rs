use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aft::config::Config;
use aft::inspect::scanners::dead_code::run_dead_code_scan;
use aft::inspect::{
    CallgraphExport, CallgraphOutboundCall, CallgraphSnapshot, InspectCategory, InspectJob,
    InspectScanSuccess, JobKey,
};
use aft::parser::SymbolCache;
use serde_json::json;

fn fixture_project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join("project");
    fs::create_dir_all(&root).expect("create project root");

    let paths = files
        .iter()
        .map(|(relative, contents)| {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&path, contents).expect("write fixture file");
            path
        })
        .collect::<Vec<_>>();

    (temp_dir, root, paths)
}

fn job(
    root: &Path,
    scope_files: Vec<PathBuf>,
    callgraph_snapshot: Option<CallgraphSnapshot>,
) -> InspectJob {
    InspectJob {
        job_id: 1,
        key: JobKey::for_project_category(InspectCategory::DeadCode),
        category: InspectCategory::DeadCode,
        scope_files,
        project_root: root.to_path_buf(),
        inspect_dir: root.join(".aft-cache").join("inspect"),
        config: Arc::new(Config {
            project_root: Some(root.to_path_buf()),
            ..Config::default()
        }),
        symbol_cache: Arc::new(RwLock::new(SymbolCache::new())),
        callgraph_snapshot: callgraph_snapshot.map(Arc::new),
    }
}

fn snapshot(
    files: Vec<PathBuf>,
    exported_symbols: Vec<CallgraphExport>,
    outbound_calls: Vec<CallgraphOutboundCall>,
    entry_points: Vec<PathBuf>,
) -> CallgraphSnapshot {
    CallgraphSnapshot {
        generated_at: None,
        files,
        exported_symbols,
        outbound_calls,
        entry_points: entry_points.into_iter().collect::<BTreeSet<_>>(),
    }
}

fn export(root: &Path, file: &str, symbol: &str, kind: &str, line: u32) -> CallgraphExport {
    CallgraphExport {
        file: root.join(file),
        symbol: symbol.to_string(),
        kind: kind.to_string(),
        line,
    }
}

fn outbound(root: &Path, caller_file: &str, target: &str, line: u32) -> CallgraphOutboundCall {
    CallgraphOutboundCall {
        caller_file: root.join(caller_file),
        target: target.to_string(),
        line,
    }
}

fn scan(job: InspectJob) -> InspectScanSuccess {
    run_dead_code_scan(&job).outcome.expect("scan succeeds")
}

#[test]
fn inspect_dead_code_unavailable_callgraph_returns_empty_result() {
    let (_temp_dir, root, paths) = fixture_project(&[("src/foo.ts", "export function foo() {}\n")]);

    let success = scan(job(&root, paths, None));

    assert!(success.contributions.is_empty());
    assert_eq!(success.aggregate["count"], 0);
    assert_eq!(success.aggregate["callgraph_available"], false);
    assert_eq!(success.aggregate["drill_down_capped"], false);
}

#[test]
fn inspect_dead_code_reports_exported_uncalled_function() {
    let (_temp_dir, root, paths) =
        fixture_project(&[("src/foo.ts", "export function unused() {}\n")]);
    let graph = snapshot(
        paths.clone(),
        vec![export(&root, "src/foo.ts", "unused", "function", 1)],
        Vec::new(),
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 1);
    assert_eq!(
        success.aggregate["items"].as_array().expect("items").len(),
        1
    );
    assert_eq!(
        success.aggregate["items"][0],
        json!({"file": "src/foo.ts", "symbol": "unused", "kind": "function", "line": 1})
    );
}

#[test]
fn inspect_dead_code_does_not_report_export_called_from_another_file() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("src/foo.ts", "export function used() {}\n"),
        ("src/bar.ts", "import { used } from './foo';\nused();\n"),
    ]);
    let graph = snapshot(
        paths.clone(),
        vec![export(&root, "src/foo.ts", "used", "function", 1)],
        vec![outbound(&root, "src/bar.ts", "used", 2)],
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
    assert!(success.aggregate["items"]
        .as_array()
        .expect("items")
        .is_empty());
}

#[test]
fn inspect_dead_code_does_not_report_entry_point_exports() {
    let (_temp_dir, root, paths) =
        fixture_project(&[("src/main.ts", "export function main() {}\n")]);
    let graph = snapshot(
        paths.clone(),
        vec![export(&root, "src/main.ts", "main", "function", 1)],
        Vec::new(),
        vec![root.join("src/main.ts")],
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
}

#[test]
fn inspect_dead_code_does_not_report_package_json_main_export() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("package.json", "{\"main\":\"src/public.ts\"}\n"),
        ("src/public.ts", "export function publicApi() {}\n"),
    ]);
    let source_files = vec![root.join("src/public.ts")];
    let graph = snapshot(
        source_files.clone(),
        vec![export(&root, "src/public.ts", "publicApi", "function", 1)],
        Vec::new(),
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
}

#[test]
fn inspect_dead_code_caps_drill_down_after_one_hundred_items() {
    let source = (0..101)
        .map(|index| format!("export function unused_{index}() {{}}\n"))
        .collect::<String>();
    let (_temp_dir, root, paths) = fixture_project(&[("src/many.ts", &source)]);
    let exports = (0..101)
        .map(|index| {
            export(
                &root,
                "src/many.ts",
                &format!("unused_{index}"),
                "function",
                index + 1,
            )
        })
        .collect::<Vec<_>>();
    let graph = snapshot(paths.clone(), exports, Vec::new(), Vec::new());

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 101);
    assert_eq!(
        success.aggregate["items"].as_array().expect("items").len(),
        100
    );
    assert_eq!(success.aggregate["drill_down_capped"], true);
}

#[test]
fn inspect_dead_code_contribution_shape_matches_contract() {
    let (_temp_dir, root, paths) = fixture_project(&[
        (
            "src/foo.ts",
            "export class Foo {}\nexport function helper() { return Bar(); }\n",
        ),
        ("src/bar.ts", "export function Bar() {}\n"),
    ]);
    let graph = snapshot(
        paths.clone(),
        vec![
            export(&root, "src/foo.ts", "Foo", "class", 1),
            export(&root, "src/foo.ts", "helper", "function", 2),
            export(&root, "src/bar.ts", "Bar", "function", 1),
        ],
        vec![
            outbound(&root, "src/foo.ts", "Bar", 2),
            outbound(&root, "src/foo.ts", "external_dependency", 3),
        ],
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));
    let contribution = success
        .contributions
        .iter()
        .find(|contribution| contribution.file_path == root.join("src/foo.ts"))
        .expect("foo contribution");

    assert_eq!(
        contribution.contribution,
        json!({
            "file": "src/foo.ts",
            "exports": [
                {"symbol": "Foo", "kind": "class", "line": 1},
                {"symbol": "helper", "kind": "function", "line": 2}
            ],
            "internal_calls": [
                {"symbol": "Bar", "line": 2}
            ]
        })
    );
}
