use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use aft::config::Config;
use aft::inspect::{InspectCategory, InspectManager, InspectScanSuccess, InspectSnapshot};
use aft::parser::SymbolCache;
use serde_json::Value;

fn write_file(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture file has parent")).expect("create parent");
    fs::write(&path, contents).expect("write fixture");
    path
}

fn snapshot(project_root: &Path, inspect_dir: &Path) -> InspectSnapshot {
    let config = Config {
        project_root: Some(project_root.to_path_buf()),
        ..Config::default()
    };
    InspectSnapshot::new(
        project_root.to_path_buf(),
        inspect_dir.to_path_buf(),
        Arc::new(config),
        Arc::new(RwLock::new(SymbolCache::new())),
    )
}

fn duplicate_source() -> String {
    r#"
export function calculate(input: number) {
  const first = input + 1;
  const second = first + 2;
  const third = second + first;
  const fourth = third + 3;
  const fifth = fourth + third;
  const sixth = fifth + second;
  return sixth + fourth;
}
"#
    .to_string()
}

fn changed_source() -> String {
    r#"
export function calculate(input: number) {
  const first = input + 10;
  const second = first + 20;
  const third = second + first;
  const fourth = third + 30;
  const fifth = fourth + third;
  const sixth = fifth + second;
  const seventh = sixth + fifth;
  return seventh + fourth;
}
"#
    .to_string()
}

fn build_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join("project");
    fs::create_dir_all(&root).expect("create project");
    let source = duplicate_source();
    let mut mutated_file = PathBuf::new();
    for index in 0..32 {
        let relative = format!("src/file_{index:02}.ts");
        let path = write_file(&root, &relative, &source);
        if index == 7 {
            mutated_file = path;
        }
    }
    (temp_dir, root, mutated_file)
}

fn run_reuse(
    manager: &InspectManager,
    snapshot: InspectSnapshot,
) -> (InspectScanSuccess, Duration) {
    run_reuse_category(manager, snapshot, InspectCategory::Duplicates)
}

fn run_reuse_category(
    manager: &InspectManager,
    snapshot: InspectSnapshot,
    category: InspectCategory,
) -> (InspectScanSuccess, Duration) {
    let started = Instant::now();
    let result = manager.tier2_run_with_reuse_result(snapshot, category, None);
    let elapsed = started.elapsed();
    (result.outcome.expect("tier2 reuse run succeeds"), elapsed)
}

fn relative_paths(project_root: &Path, files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|file| {
            file.strip_prefix(project_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn inspect_tier2_reuse_skips_fresh_files_and_rescans_stale_file() {
    let (_temp_dir, root, mutated_file) = build_fixture();
    let inspect_dir = root.join(".aft-cache").join("inspect");

    let first_manager = InspectManager::new();
    let (first, _t1) = run_reuse(&first_manager, snapshot(&root, &inspect_dir));
    assert_eq!(first.scanned_files.len(), 32);
    assert!(first.aggregate["groups_count"].as_u64().unwrap_or(0) > 0);

    let second_manager = InspectManager::new();
    let (second, _t2) = run_reuse(&second_manager, snapshot(&root, &inspect_dir));
    // Cache reuse is proven behaviorally: a fully-fresh second run rescans
    // zero files and returns the identical aggregate. (A wall-clock "faster"
    // assertion was removed — it flaked under parallel test load while adding
    // no signal beyond the scanned_files/aggregate checks below.)
    assert!(second.scanned_files.is_empty());
    assert_eq!(second.aggregate, first.aggregate);

    fs::write(&mutated_file, changed_source()).expect("mutate one fixture file");

    let third_manager = InspectManager::new();
    let (third, _t3) = run_reuse(&third_manager, snapshot(&root, &inspect_dir));
    assert_eq!(
        relative_paths(&root, &third.scanned_files),
        vec!["src/file_07.ts"]
    );
    assert_ne!(third.aggregate, first.aggregate);
}

#[test]
fn inspect_tier2_reuse_rescans_same_size_content_change_with_restored_mtime() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join("project");
    fs::create_dir_all(&root).expect("create project");
    let source = write_file(&root, "src/export.ts", "export function one() {}\n");
    let fixed_mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(&source, fixed_mtime).expect("set fixed mtime");
    let inspect_dir = root.join(".aft-cache").join("inspect");

    let first_manager = InspectManager::new();
    let (first, _t1) = run_reuse_category(
        &first_manager,
        snapshot(&root, &inspect_dir),
        InspectCategory::UnusedExports,
    );
    assert_eq!(first.scanned_files.len(), 1);
    assert_eq!(first.aggregate["items"][0]["symbol"], "one");

    fs::write(&source, "export function two() {}\n").expect("same-size mutate");
    filetime::set_file_mtime(&source, fixed_mtime).expect("restore mtime");

    let second_manager = InspectManager::new();
    let (second, _t2) = run_reuse_category(
        &second_manager,
        snapshot(&root, &inspect_dir),
        InspectCategory::UnusedExports,
    );

    assert_eq!(
        relative_paths(&root, &second.scanned_files),
        vec!["src/export.ts"]
    );
    assert_eq!(second.aggregate["items"][0]["symbol"], "two");
    assert_ne!(second.aggregate, first.aggregate);
}

fn unused_contribution_payloads(
    project_root: &Path,
    success: &InspectScanSuccess,
) -> Vec<(String, Value)> {
    let mut payloads = success
        .contributions
        .iter()
        .map(|contribution| {
            let relative = contribution
                .file_path
                .strip_prefix(project_root)
                .unwrap_or(&contribution.file_path)
                .to_string_lossy()
                .replace('\\', "/");
            (relative, contribution.contribution.clone())
        })
        .collect::<Vec<_>>();
    payloads.sort_by(|left, right| left.0.cmp(&right.0));
    payloads
}

fn assert_unused_exports_incremental_matches_cold<S, E>(name: &str, setup: S, edit: E)
where
    S: FnOnce(&Path),
    E: FnOnce(&Path),
{
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join(format!("project-{name}"));
    fs::create_dir_all(&root).expect("create project");
    setup(&root);

    let warm_inspect_dir = temp_dir.path().join(format!("inspect-warm-{name}"));
    let warm_manager = InspectManager::new();
    let (first, _first_elapsed) = run_reuse_category(
        &warm_manager,
        snapshot(&root, &warm_inspect_dir),
        InspectCategory::UnusedExports,
    );
    assert!(
        !first.contributions.is_empty(),
        "{name}: initial cold scan should populate contributions"
    );

    edit(&root);

    let (warm, _warm_elapsed) = run_reuse_category(
        &warm_manager,
        snapshot(&root, &warm_inspect_dir),
        InspectCategory::UnusedExports,
    );
    let cold_inspect_dir = temp_dir.path().join(format!("inspect-cold-{name}"));
    let cold_manager = InspectManager::new();
    let (cold, _cold_elapsed) = run_reuse_category(
        &cold_manager,
        snapshot(&root, &cold_inspect_dir),
        InspectCategory::UnusedExports,
    );

    assert_eq!(warm.aggregate, cold.aggregate, "{name}: aggregate mismatch");
    assert_eq!(
        unused_contribution_payloads(&root, &warm),
        unused_contribution_payloads(&root, &cold),
        "{name}: per-file contribution payload mismatch"
    );
}

#[test]
fn inspect_unused_exports_incremental_oxc_invariants_match_cold() {
    assert_unused_exports_incremental_matches_cold(
        "last_importer_removed",
        |root| {
            write_file(
                root,
                "src/exported.ts",
                "export const x = 1;
export const y = 2;
",
            );
            write_file(
                root,
                "src/use.ts",
                "import { x } from './exported';
console.log(x);
",
            );
        },
        |root| {
            write_file(
                root,
                "src/use.ts",
                "console.log('import removed');
",
            );
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "importer_deleted",
        |root| {
            write_file(
                root,
                "src/exported.ts",
                "export const x = 1;
",
            );
            write_file(
                root,
                "src/use.ts",
                "import { x } from './exported';
console.log(x);
",
            );
        },
        |root| {
            fs::remove_file(root.join("src/use.ts")).expect("delete importer");
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "file_renamed",
        |root| {
            write_file(
                root,
                "src/exported.ts",
                "export const x = 1;
",
            );
            write_file(
                root,
                "src/use.ts",
                "import { x } from './exported';
console.log(x);
",
            );
        },
        |root| {
            fs::create_dir_all(root.join("src/moved")).expect("create moved dir");
            fs::rename(
                root.join("src/exported.ts"),
                root.join("src/moved/exported.ts"),
            )
            .expect("rename exported file");
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "tsconfig_alias_change",
        |root| {
            write_file(
                root,
                "tsconfig.json",
                r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib":["src/a.ts"]}}}"#,
            );
            write_file(
                root,
                "src/a.ts",
                "export const x = 'a';
",
            );
            write_file(
                root,
                "src/b.ts",
                "export const x = 'b';
",
            );
            write_file(
                root,
                "src/use.ts",
                "import { x } from '@lib';
console.log(x);
",
            );
        },
        |root| {
            write_file(
                root,
                "tsconfig.json",
                r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib":["src/b.ts"]}}}"#,
            );
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "barrel_target_changed",
        |root| {
            write_file(
                root,
                "src/target.ts",
                "export const named = 1;
export default function def() { return named; }
",
            );
            write_file(
                root,
                "src/extra.ts",
                "export const star = 1;
",
            );
            write_file(
                root,
                "src/barrel.ts",
                "export { named } from './target';
export { default } from './target';
export * from './extra';
export * as ns from './target';
",
            );
            write_file(
                root,
                "src/use.ts",
                "import { named, default as def, star, ns } from './barrel';
console.log(named, def, star, ns);
",
            );
        },
        |root| {
            write_file(
                root,
                "src/target.ts",
                "export const named = 1;
export const added = 2;
export default function def() { return named + added; }
",
            );
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "namespace_import_uncertain",
        |root| {
            write_file(
                root,
                "src/target.ts",
                "export const a = 1;
export const b = 2;
",
            );
            write_file(
                root,
                "src/use.ts",
                "import { a } from './target';
console.log(a);
",
            );
        },
        |root| {
            write_file(
                root,
                "src/use.ts",
                "import * as target from './target';
console.log(target);
",
            );
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "dynamic_import_added",
        |root| {
            write_file(
                root,
                "src/lazy.ts",
                "export const lazy = 1;
",
            );
            write_file(
                root,
                "src/main.ts",
                "console.log('main');
",
            );
        },
        |root| {
            write_file(
                root,
                "src/main.ts",
                "import('./lazy').then((module) => console.log(module));
",
            );
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "dynamic_import_removed",
        |root| {
            write_file(
                root,
                "src/lazy.ts",
                "export const lazy = 1;
",
            );
            write_file(
                root,
                "src/main.ts",
                "import('./lazy').then((module) => console.log(module));
",
            );
        },
        |root| {
            write_file(
                root,
                "src/main.ts",
                "console.log('main');
",
            );
        },
    );

    assert_unused_exports_incremental_matches_cold(
        "new_sibling_resolution_candidate",
        |root| {
            write_file(
                root,
                "src/foo/index.ts",
                "export const x = 1;
export const oldOnly = 2;
",
            );
            write_file(
                root,
                "src/use.ts",
                "import { x } from './foo';
console.log(x);
",
            );
        },
        |root| {
            write_file(
                root,
                "src/foo.ts",
                "export const x = 1;
export const newOnly = 3;
",
            );
        },
    );
}

#[test]
fn inspect_unused_exports_twice_cold_is_deterministic() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join("project-twice-cold");
    fs::create_dir_all(&root).expect("create project");
    write_file(
        root.as_path(),
        "src/a.ts",
        "export const a = 1;
export const unused = 2;
",
    );
    write_file(
        root.as_path(),
        "src/b.ts",
        "import { a } from './a';
console.log(a);
",
    );

    let manager_a = InspectManager::new();
    let (cold_a, _elapsed_a) = run_reuse_category(
        &manager_a,
        snapshot(&root, &temp_dir.path().join("inspect-cold-a")),
        InspectCategory::UnusedExports,
    );
    let manager_b = InspectManager::new();
    let (cold_b, _elapsed_b) = run_reuse_category(
        &manager_b,
        snapshot(&root, &temp_dir.path().join("inspect-cold-b")),
        InspectCategory::UnusedExports,
    );

    assert_eq!(cold_a.aggregate, cold_b.aggregate);
    assert_eq!(
        unused_contribution_payloads(&root, &cold_a),
        unused_contribution_payloads(&root, &cold_b)
    );
}
