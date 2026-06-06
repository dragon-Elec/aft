use aft::callgraph_store::CallGraphStore;
use aft::commands::callgraph_store_adapter;
use rusqlite::params;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

#[test]
fn rust_method_dispatch_name_match_surfaces_approximate_edges_for_store_ops() {
    let dir = tempdir().unwrap();
    let root = canonical_root(dir.path());
    write_file(
        &root.join("src/context.rs"),
        r#"pub struct AppContext;

impl AppContext {
    pub fn callgraph_store_for_ops(&self) -> usize {
        1
    }
}
"#,
    );
    for name in [
        "callers",
        "call_tree",
        "impact",
        "trace_to",
        "trace_to_symbol",
    ] {
        write_file(
            &root.join(format!("src/commands/{name}.rs")),
            &format!(
                r#"use crate::context::AppContext;

pub fn handle_{name}(ctx: &AppContext) -> usize {{
    ctx.callgraph_store_for_ops()
}}
"#
            ),
        );
    }

    let store = build_store(&root, "rust-name-match", &project_files(&root));
    let callers = json(callgraph_store_adapter::callers_result(
        &store,
        &root.join("src/context.rs"),
        "AppContext::callgraph_store_for_ops",
        1,
    ));
    let entries = flattened_callers(&callers);
    assert_eq!(entries.len(), 5, "callers output: {callers:#}");
    assert!(
        entries
            .iter()
            .all(|entry| entry["approximate"] == true && entry["resolved_by"] == "name_match"),
        "all callers should be marked as name_match: {callers:#}"
    );

    let impact = json(callgraph_store_adapter::impact_result(
        &store,
        &root.join("src/context.rs"),
        "AppContext::callgraph_store_for_ops",
        1,
    ));
    let impact_callers = impact["callers"].as_array().unwrap();
    assert_eq!(impact_callers.len(), 5, "impact output: {impact:#}");
    assert!(impact_callers
        .iter()
        .all(|caller| { caller["approximate"] == true && caller["resolved_by"] == "name_match" }));

    let tree = json(callgraph_store_adapter::call_tree_result(
        &store,
        &root.join("src/commands/callers.rs"),
        "handle_callers",
        1,
    ));
    let child = tree["children"].as_array().unwrap().first().unwrap();
    assert_eq!(child["name"], "AppContext::callgraph_store_for_ops");
    assert_eq!(child["approximate"], true, "call_tree output: {tree:#}");
    assert_eq!(child["resolved_by"], "name_match");

    let trace = json(callgraph_store_adapter::trace_to_result(
        &store,
        &root.join("src/context.rs"),
        "AppContext::callgraph_store_for_ops",
        2,
    ));
    let target_hop = trace["paths"][0]["hops"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(
        target_hop["approximate"], true,
        "trace_to output: {trace:#}"
    );
    assert_eq!(target_hop["resolved_by"], "name_match");

    let path = json(callgraph_store_adapter::trace_to_symbol_result(
        &store,
        &root.join("src/commands/callers.rs"),
        "handle_callers",
        "callgraph_store_for_ops",
        Some(&root.join("src/context.rs")),
        2,
    ));
    let target_hop = path["path"]
        .as_array()
        .unwrap_or_else(|| panic!("trace_to_symbol output: {path:#}"))
        .last()
        .unwrap();
    assert_eq!(
        target_hop["approximate"], true,
        "trace_to_symbol output: {path:#}"
    );
    assert_eq!(target_hop["resolved_by"], "name_match");

    let conn = rusqlite::Connection::open(store.sqlite_path()).unwrap();
    let persisted: i64 = conn
        .query_row(
            "SELECT COUNT(*)
             FROM edges e JOIN refs r ON r.ref_id = e.ref_id
             WHERE e.provenance = 'name_match'
               AND r.status = 'unresolved'
               AND e.target_symbol = 'AppContext::callgraph_store_for_ops'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, 5, "name_match edges must leave refs unresolved");
}

#[test]
fn typescript_class_method_name_match_is_language_agnostic() {
    let dir = tempdir().unwrap();
    let root = canonical_root(dir.path());
    write_file(
        &root.join("worker.ts"),
        r#"export class Worker {
  run() {
    return 1;
  }
}
"#,
    );
    write_file(
        &root.join("entry.ts"),
        r#"import { Worker } from './worker';

export function entry(worker: Worker) {
  return worker.run();
}
"#,
    );

    let store = build_store(&root, "ts-name-match", &project_files(&root));
    let callers = json(callgraph_store_adapter::callers_result(
        &store,
        &root.join("worker.ts"),
        "Worker::run",
        1,
    ));
    let entries = flattened_callers(&callers);
    assert_eq!(entries.len(), 1, "TS callers output: {callers:#}");
    assert_eq!(entries[0]["symbol"], "entry");
    assert_eq!(entries[0]["approximate"], true);
    assert_eq!(entries[0]["resolved_by"], "name_match");
}

#[test]
fn name_match_keeps_unknown_external_methods_as_noise() {
    let dir = tempdir().unwrap();
    let root = canonical_root(dir.path());
    write_file(
        &root.join("src/lib.rs"),
        r#"pub fn noisy(input: Option<String>) -> String {
    let cloned = input.clone();
    cloned.unwrap()
}
"#,
    );

    let store = build_store(&root, "noise", &project_files(&root));
    assert_eq!(count_name_match_edges(&store), 0);
}

#[test]
fn ambiguous_methods_below_score_threshold_do_not_create_spurious_edges() {
    let dir = tempdir().unwrap();
    let root = canonical_root(dir.path());
    write_file(
        &root.join("ambiguous.ts"),
        r#"class Alpha {
  handle() {}
}

class Beta {
  handle() {}
}

export function entry(service: { handle(): void }) {
  service.handle();
}
"#,
    );

    let store = build_store(&root, "ambiguous", &project_files(&root));
    let conn = rusqlite::Connection::open(store.sqlite_path()).unwrap();
    let handle_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE provenance = 'name_match' AND target_symbol LIKE '%::handle'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        handle_edges, 0,
        "ambiguous receiver should not pick an arbitrary handle"
    );

    let tree = json(callgraph_store_adapter::call_tree_result(
        &store,
        &root.join("ambiguous.ts"),
        "entry",
        1,
    ));
    let child = tree["children"].as_array().unwrap().first().unwrap();
    assert_eq!(child["name"], "handle", "call_tree output: {tree:#}");
    assert_eq!(child["resolved"], false);
    assert!(child.get("approximate").is_none());
}

#[test]
fn scored_ambiguous_methods_pick_receiver_matching_candidate() {
    let dir = tempdir().unwrap();
    let root = canonical_root(dir.path());
    write_file(
        &root.join("engines.ts"),
        r#"class PermissionRuleEngine {
  evaluate() { return true; }
}

class BillingRuleEngine {
  evaluate() { return false; }
}

export function entry(permissionRuleEngine: PermissionRuleEngine) {
  return permissionRuleEngine.evaluate();
}
"#,
    );

    let store = build_store(&root, "scored", &project_files(&root));
    let callers = json(callgraph_store_adapter::callers_result(
        &store,
        &root.join("engines.ts"),
        "PermissionRuleEngine::evaluate",
        1,
    ));
    let entries = flattened_callers(&callers);
    assert_eq!(entries.len(), 1, "scored callers output: {callers:#}");
    assert_eq!(entries[0]["symbol"], "entry");
    assert_eq!(entries[0]["approximate"], true);

    let conn = rusqlite::Connection::open(store.sqlite_path()).unwrap();
    let billing_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE provenance = 'name_match' AND target_symbol = ?1",
            params!["BillingRuleEngine::evaluate"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        billing_edges, 0,
        "receiver scoring should not cross-edge to BillingRuleEngine"
    );
}

fn canonical_root(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn build_store(root: &Path, name: &str, files: &[PathBuf]) -> CallGraphStore {
    let store =
        CallGraphStore::open(root.join(format!(".{name}-store")), root.to_path_buf()).unwrap();
    store.cold_build(files).unwrap();
    store
}

fn json<T: serde::Serialize>(value: Result<T, aft::callgraph_store::CallGraphStoreError>) -> Value {
    serde_json::to_value(value.unwrap()).unwrap()
}

fn flattened_callers(result: &Value) -> Vec<&Value> {
    result["callers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["callers"].as_array().unwrap().iter())
        .collect()
}

fn count_name_match_edges(store: &CallGraphStore) -> i64 {
    let conn = rusqlite::Connection::open(store.sqlite_path()).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM edges WHERE provenance = 'name_match'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn project_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_project_files(root, &mut files);
    files.sort();
    files
}

fn collect_project_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }
            collect_project_files(&path, files);
            continue;
        }
        if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "ts")
        ) {
            files.push(path);
        }
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}
