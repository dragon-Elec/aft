//! Pure oxc-backed TS/JS module graph facts and export-liveness verdicts.
//!
//! H1-1 intentionally stops at an engine/API boundary: scanners wire this into
//! inspect contributions in H1-2. The engine is pure (files in, verdicts out)
//! and keeps only an in-memory facts cache keyed by file content hash + parser
//! facts format. That is enough for the required warm-cache perf gate while
//! avoiding premature persistence coupling to InspectCache/AppContext.

mod facts;
mod graph;
mod resolver;
pub mod types;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use facts::parse_file_facts;
use graph::compute_verdicts;
use resolver::{normalize_path, ModuleResolver};
pub use types::{
    DynamicImportFact, ExportFact, ExportName, FileFacts, FileId, ImportFact, ImportKind,
    LivenessVerdict, OxcEngineError, OxcEngineResult, OxcEngineStats, OxcExportVerdict,
    OxcFileVerdicts, OxcResolvedEdge, ReExportFact, ReExportKind, ResolverConfigInput,
    OXC_PROVENANCE,
};

const FACTS_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Default)]
pub struct AnalyzeOptions {
    pub entry_points: Vec<PathBuf>,
    pub public_api_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct OxcFactsCache {
    entries_by_hash: BTreeMap<String, FileFacts>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OxcFactsCacheStats {
    pub hits: usize,
    pub misses: usize,
}

impl OxcFactsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries_by_hash.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries_by_hash.is_empty()
    }

    fn facts_for_source(
        &mut self,
        file_id: FileId,
        path: &Path,
        source: &str,
        stats: &mut OxcFactsCacheStats,
    ) -> FileFacts {
        let content_hash = crate::cache_freshness::hash_bytes(source.as_bytes())
            .to_hex()
            .to_string();
        let cache_key = format!("v{FACTS_FORMAT_VERSION}:{content_hash}");
        if let Some(cached) = self.entries_by_hash.get(&cache_key) {
            stats.hits += 1;
            let mut facts = cached.clone();
            facts.file_id = file_id;
            facts.path = path.to_path_buf();
            facts.content_hash = content_hash;
            return facts;
        }

        stats.misses += 1;
        let facts = parse_file_facts(file_id, path, source, content_hash);
        self.entries_by_hash.insert(cache_key, facts.clone());
        facts
    }
}

pub fn analyze_files(
    project_root: &Path,
    files: &[PathBuf],
    options: AnalyzeOptions,
) -> Result<OxcEngineResult, String> {
    let mut cache = OxcFactsCache::new();
    analyze_files_with_cache(project_root, files, options, &mut cache)
}

pub fn analyze_files_with_cache(
    project_root: &Path,
    files: &[PathBuf],
    options: AnalyzeOptions,
    cache: &mut OxcFactsCache,
) -> Result<OxcEngineResult, String> {
    let project_root =
        fs::canonicalize(project_root).unwrap_or_else(|_| normalize_path(project_root));
    let files = normalize_file_set(files);
    let mut cache_stats = OxcFactsCacheStats::default();
    let mut errors = Vec::new();
    let mut facts = Vec::with_capacity(files.len());

    for (idx, path) in files.iter().enumerate() {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                errors.push(OxcEngineError {
                    file: path.clone(),
                    message: format!("read: {error}"),
                });
                continue;
            }
        };
        let file_facts = cache.facts_for_source(FileId(idx), path, &source, &mut cache_stats);
        if let Some(parse_error) = &file_facts.parse_error {
            errors.push(OxcEngineError {
                file: path.clone(),
                message: format!("parse: {parse_error}"),
            });
        }
        facts.push(file_facts);
    }

    // Preserve dense FileId indexing when unreadable files were skipped.
    for (idx, fact) in facts.iter_mut().enumerate() {
        fact.file_id = FileId(idx);
    }
    let resolved_files = facts
        .iter()
        .map(|fact| fact.path.clone())
        .collect::<Vec<_>>();
    let resolver = ModuleResolver::new(&project_root, &resolved_files);
    let (resolved_modules, tracker, edges) = resolver.resolve_modules(&facts);
    let entry_points = normalize_option_paths(&options.entry_points);
    let public_api_files = normalize_option_paths(&options.public_api_files);
    let file_verdicts = compute_verdicts(
        &project_root,
        &resolved_modules,
        &entry_points,
        &public_api_files,
    );
    let resolved_edges = edges
        .iter()
        .filter(|edge| edge.resolved_file.is_some())
        .count();
    let unresolved_edges = edges.len().saturating_sub(resolved_edges);
    let resolver_config_inputs = tracker.inputs();
    let resolver_config_fingerprint = tracker.fingerprint();

    Ok(OxcEngineResult {
        files: file_verdicts,
        resolver_config_inputs,
        resolver_config_fingerprint,
        edges,
        stats: OxcEngineStats {
            files: facts.len(),
            cache_hits: cache_stats.hits,
            cache_misses: cache_stats.misses,
            resolved_edges,
            unresolved_edges,
        },
        errors,
    })
}

fn normalize_file_set(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = files
        .iter()
        .filter(|path| is_ts_js_file(path))
        .map(|path| fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path)))
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

fn normalize_option_paths(paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    paths
        .iter()
        .map(|path| fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path)))
        .collect()
}

fn is_ts_js_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs"
            )
        })
}
