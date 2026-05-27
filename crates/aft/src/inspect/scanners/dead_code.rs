use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use rayon::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::cache_freshness::{self, FileFreshness};
use crate::inspect::{
    CallgraphOutboundCall, CallgraphSnapshot, FileContribution, InspectCategory, InspectJob,
    InspectResult, InspectScanSuccess,
};

const MAX_DRILL_DOWN_ITEMS: usize = 100;

pub fn run_dead_code_scan(job: &InspectJob) -> InspectResult {
    let started = Instant::now();

    let Some(snapshot) = job.callgraph_snapshot.as_deref() else {
        let success = InspectScanSuccess {
            scanned_files: job.scope_files.clone(),
            contributions: Vec::new(),
            aggregate: json!({
                "count": 0,
                "items": [],
                "drill_down_capped": false,
                "callgraph_available": false,
                "scanned_files": job.scope_files.len(),
                "notes": ["callgraph_unavailable"],
            }),
        };
        return InspectResult::success(job, success, started.elapsed());
    };

    let exported_symbols = snapshot
        .exported_symbols
        .iter()
        .map(|export| export.symbol.as_str())
        .collect::<HashSet<_>>();
    let project_files = snapshot
        .files
        .iter()
        .map(|file| relative_path(&job.project_root, file))
        .collect::<BTreeSet<_>>();
    let exported_file_symbols = snapshot
        .exported_symbols
        .iter()
        .map(|export| {
            (
                relative_path(&job.project_root, &export.file),
                export.symbol.clone(),
            )
        })
        .collect::<BTreeSet<_>>();

    let contributions = job
        .scope_files
        .par_iter()
        .map(|file| {
            gather_file_contribution(
                job,
                snapshot,
                file,
                &exported_symbols,
                &project_files,
                &exported_file_symbols,
            )
        })
        .collect::<Vec<_>>();

    let public_api_files = collect_public_api_files(&job.project_root);
    let aggregate = roll_up_dead_code(job, snapshot, &contributions, &public_api_files);
    let success = InspectScanSuccess {
        scanned_files: job.scope_files.clone(),
        contributions,
        aggregate,
    };

    InspectResult::success(job, success, started.elapsed())
}

fn gather_file_contribution(
    job: &InspectJob,
    snapshot: &CallgraphSnapshot,
    file: &Path,
    exported_symbols: &HashSet<&str>,
    project_files: &BTreeSet<String>,
    exported_file_symbols: &BTreeSet<(String, String)>,
) -> FileContribution {
    let file_name = relative_path(&job.project_root, file);
    let exports = snapshot
        .exported_symbols
        .iter()
        .filter(|export| same_file(&job.project_root, &export.file, file))
        .map(|export| {
            json!({
                "symbol": export.symbol,
                "kind": export.kind,
                "line": export.line,
            })
        })
        .collect::<Vec<_>>();

    let mut internal_calls = snapshot
        .outbound_calls
        .iter()
        .filter(|call| same_file(&job.project_root, &call.caller_file, file))
        .filter_map(|call| {
            project_internal_call(
                &job.project_root,
                call,
                exported_symbols,
                project_files,
                exported_file_symbols,
            )
        })
        .collect::<Vec<_>>();
    internal_calls.sort_by(|left, right| {
        left.line
            .cmp(&right.line)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });

    FileContribution::new(
        InspectCategory::DeadCode,
        file.to_path_buf(),
        collect_freshness(file),
        json!({
            "file": file_name,
            "exports": exports,
            "internal_calls": internal_calls
                .into_iter()
                .map(|call| json!({ "symbol": call.symbol, "line": call.line }))
                .collect::<Vec<_>>(),
        }),
    )
}

fn roll_up_dead_code(
    job: &InspectJob,
    snapshot: &CallgraphSnapshot,
    contributions: &[FileContribution],
    public_api_files: &BTreeSet<String>,
) -> serde_json::Value {
    let parsed = contributions
        .iter()
        .filter_map(|contribution| {
            serde_json::from_value::<DeadCodeContribution>(contribution.contribution.clone()).ok()
        })
        .collect::<Vec<_>>();

    let mut exports_by_symbol: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for contribution in &parsed {
        for export in &contribution.exports {
            exports_by_symbol
                .entry(export.symbol.clone())
                .or_default()
                .push(contribution.file.clone());
        }
    }

    let mut callers_by_export: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for contribution in &parsed {
        for call in &contribution.internal_calls {
            if let Some(files) = exports_by_symbol.get(&call.symbol) {
                for file in files {
                    callers_by_export
                        .entry((file.clone(), call.symbol.clone()))
                        .or_default()
                        .insert(contribution.file.clone());
                }
            }
        }
    }

    let entry_points = snapshot
        .entry_points
        .iter()
        .map(|file| relative_path(&job.project_root, file))
        .collect::<BTreeSet<_>>();

    let mut dead_items = Vec::new();
    for contribution in &parsed {
        let is_entry_point_file = entry_points.contains(&contribution.file);
        let is_public_api_file = public_api_files.contains(&contribution.file);
        for export in &contribution.exports {
            if callers_by_export.contains_key(&(contribution.file.clone(), export.symbol.clone())) {
                continue;
            }
            if is_entry_point_file || is_public_api_file {
                continue;
            }
            dead_items.push(json!({
                "file": contribution.file,
                "symbol": export.symbol,
                "kind": export.kind,
                "line": export.line,
            }));
        }
    }

    let count = dead_items.len();
    let drill_down_capped = count > MAX_DRILL_DOWN_ITEMS;
    dead_items.truncate(MAX_DRILL_DOWN_ITEMS);

    json!({
        "count": count,
        "items": dead_items,
        "drill_down_capped": drill_down_capped,
        "callgraph_available": true,
        "scanned_files": contributions.len(),
    })
}

fn project_internal_call(
    project_root: &Path,
    call: &CallgraphOutboundCall,
    exported_symbols: &HashSet<&str>,
    project_files: &BTreeSet<String>,
    exported_file_symbols: &BTreeSet<(String, String)>,
) -> Option<InternalCall> {
    let target = parse_target(project_root, &call.target);
    let symbol = target.symbol?;

    let internal = target.file.as_ref().is_some_and(|file| {
        project_files.contains(file)
            || exported_file_symbols.contains(&(file.clone(), symbol.clone()))
    }) || exported_symbols.contains(symbol.as_str());

    if internal {
        Some(InternalCall {
            symbol,
            line: call.line,
        })
    } else {
        None
    }
}

fn parse_target(project_root: &Path, target: &str) -> ParsedTarget {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return ParsedTarget {
            file: None,
            symbol: None,
        };
    }

    if let Some((file, symbol)) = trimmed.rsplit_once("::") {
        return ParsedTarget {
            file: Some(relative_path(project_root, Path::new(file))),
            symbol: clean_symbol(symbol),
        };
    }

    if let Some((file, symbol)) = trimmed.rsplit_once('#') {
        return ParsedTarget {
            file: Some(relative_path(project_root, Path::new(file))),
            symbol: clean_symbol(symbol),
        };
    }

    ParsedTarget {
        file: None,
        symbol: clean_symbol(trimmed),
    }
}

fn clean_symbol(symbol: &str) -> Option<String> {
    let trimmed = symbol.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn collect_public_api_files(project_root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    collect_package_public_api(project_root, project_root, &mut files);

    let package_json = project_root.join("package.json");
    let Ok(bytes) = std::fs::read(&package_json) else {
        return files;
    };
    let Ok(package) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return files;
    };

    for workspace in workspace_dirs(project_root, &package) {
        collect_package_public_api(project_root, &workspace, &mut files);
    }

    files
}

fn collect_package_public_api(
    project_root: &Path,
    package_dir: &Path,
    files: &mut BTreeSet<String>,
) {
    let package_json = package_dir.join("package.json");
    let Ok(bytes) = std::fs::read(package_json) else {
        return;
    };
    let Ok(package) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };

    if let Some(main) = package.get("main").and_then(|value| value.as_str()) {
        insert_public_api_path(project_root, package_dir, main, files);
    }
    if let Some(exports) = package.get("exports") {
        collect_export_values(project_root, package_dir, exports, files);
    }
}

fn collect_export_values(
    project_root: &Path,
    package_dir: &Path,
    value: &serde_json::Value,
    files: &mut BTreeSet<String>,
) {
    match value {
        serde_json::Value::String(path) => {
            insert_public_api_path(project_root, package_dir, path, files)
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_export_values(project_root, package_dir, value, files);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_export_values(project_root, package_dir, value, files);
            }
        }
        _ => {}
    }
}

fn insert_public_api_path(
    project_root: &Path,
    package_dir: &Path,
    value: &str,
    files: &mut BTreeSet<String>,
) {
    if value.starts_with('#') || value.contains('*') {
        return;
    }

    let trimmed = value.trim_start_matches("./");
    if trimmed.is_empty() {
        return;
    }

    let path = package_dir.join(trimmed);
    files.insert(relative_path(project_root, &path));
}

fn workspace_dirs(project_root: &Path, package: &serde_json::Value) -> Vec<PathBuf> {
    let Some(workspaces) = package.get("workspaces") else {
        return Vec::new();
    };

    let patterns = match workspaces {
        serde_json::Value::Array(values) => {
            values.iter().filter_map(|value| value.as_str()).collect()
        }
        serde_json::Value::Object(map) => map
            .get("packages")
            .and_then(|value| value.as_array())
            .map(|values| values.iter().filter_map(|value| value.as_str()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let mut dirs = Vec::new();
    for pattern in patterns {
        let pattern = pattern.trim_end_matches('/');
        if let Some(prefix) = pattern.strip_suffix("/*") {
            let parent = project_root.join(prefix);
            let Ok(entries) = std::fs::read_dir(parent) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.join("package.json").is_file() {
                    dirs.push(path);
                }
            }
        } else {
            let path = project_root.join(pattern);
            if path.join("package.json").is_file() {
                dirs.push(path);
            }
        }
    }
    dirs
}

fn collect_freshness(file: &Path) -> FileFreshness {
    cache_freshness::collect(file).unwrap_or_else(|_| FileFreshness {
        mtime: UNIX_EPOCH,
        size: 0,
        content_hash: cache_freshness::zero_hash(),
    })
}

fn same_file(project_root: &Path, left: &Path, right: &Path) -> bool {
    normalize_absolute(project_root, left) == normalize_absolute(project_root, right)
}

fn relative_path(project_root: &Path, path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let normalized = normalize_path(&absolute);
    normalized
        .strip_prefix(&normalize_path(project_root))
        .unwrap_or(normalized.as_path())
        .to_string_lossy()
        .replace('\\', "/")
}

fn normalize_absolute(project_root: &Path, path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    normalize_path(&absolute)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[derive(Debug, Clone, Deserialize)]
struct DeadCodeContribution {
    file: String,
    exports: Vec<ExportContribution>,
    internal_calls: Vec<InternalCallContribution>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExportContribution {
    symbol: String,
    kind: String,
    line: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct InternalCallContribution {
    symbol: String,
}

#[derive(Debug, Clone)]
struct InternalCall {
    symbol: String,
    line: u32,
}

#[derive(Debug, Clone)]
struct ParsedTarget {
    file: Option<String>,
    symbol: Option<String>,
}
