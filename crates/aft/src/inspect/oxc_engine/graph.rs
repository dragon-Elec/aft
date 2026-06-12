use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::resolver::ResolvedModule;
use super::types::{
    ExportFact, ExportName, FileId, ImportKind, LivenessVerdict, OxcExportVerdict, OxcFileVerdicts,
    ReExportKind, OXC_PROVENANCE,
};

#[derive(Debug, Clone)]
struct ExportState {
    fact: ExportFact,
    status: ExportStatus,
}

#[derive(Debug, Clone)]
enum ExportStatus {
    Used(String),
    Uncertain(String),
    Unused,
}

impl ExportStatus {
    fn verdict(&self) -> (LivenessVerdict, String) {
        match self {
            Self::Used(reason) => (LivenessVerdict::Used, reason.clone()),
            Self::Uncertain(reason) => (LivenessVerdict::Uncertain, reason.clone()),
            Self::Unused => (LivenessVerdict::Unused, "no_references".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
struct ModuleState {
    path: PathBuf,
    exports: Vec<ExportState>,
}

pub fn compute_verdicts(
    project_root: &Path,
    modules: &[ResolvedModule],
    entry_points: &BTreeSet<PathBuf>,
    public_api_files: &BTreeSet<PathBuf>,
) -> Vec<OxcFileVerdicts> {
    let mut graph = GraphBuilder::new(modules, entry_points, public_api_files);
    graph.apply_same_file_references();
    graph.apply_imports();
    graph.apply_re_exports();
    graph.apply_dynamic_imports();
    graph.into_verdicts(project_root)
}

struct GraphBuilder<'a> {
    modules: &'a [ResolvedModule],
    states: Vec<ModuleState>,
}

impl<'a> GraphBuilder<'a> {
    fn new(
        modules: &'a [ResolvedModule],
        entry_points: &BTreeSet<PathBuf>,
        public_api_files: &BTreeSet<PathBuf>,
    ) -> Self {
        let states = modules
            .iter()
            .map(|module| {
                let mut exports = module
                    .facts
                    .exports
                    .iter()
                    .cloned()
                    .map(|fact| ExportState {
                        fact,
                        status: ExportStatus::Unused,
                    })
                    .collect::<Vec<_>>();

                for re_export in &module.re_exports {
                    match re_export.fact.kind {
                        ReExportKind::Named => {
                            if let Some(exported_name) = &re_export.fact.exported_name {
                                push_synthetic_export(
                                    &mut exports,
                                    ExportName::Named(exported_name.clone()),
                                    "re_export",
                                    re_export.fact.is_type_only,
                                    re_export.fact.line,
                                );
                            }
                        }
                        ReExportKind::Namespace => {
                            if let Some(exported_name) = &re_export.fact.exported_name {
                                push_synthetic_export(
                                    &mut exports,
                                    ExportName::Named(exported_name.clone()),
                                    "namespace",
                                    re_export.fact.is_type_only,
                                    re_export.fact.line,
                                );
                            }
                        }
                        ReExportKind::Star => {}
                    }
                }

                let mut state = ModuleState {
                    path: module.facts.path.clone(),
                    exports,
                };
                if entry_points.contains(&module.facts.path)
                    || public_api_files.contains(&module.facts.path)
                {
                    for export in &mut state.exports {
                        mark_used(export, "entry_point");
                    }
                }
                state
            })
            .collect::<Vec<_>>();
        Self { modules, states }
    }

    fn apply_same_file_references(&mut self) {
        for (idx, module) in self.modules.iter().enumerate() {
            if module.facts.same_file_value_references.is_empty() {
                continue;
            }
            for export in &mut self.states[idx].exports {
                let Some(local_name) = export.fact.local_name.as_deref() else {
                    continue;
                };
                if module.facts.same_file_value_references.contains(local_name) {
                    mark_used(export, "same_file_value_reference");
                }
            }
        }
    }

    fn apply_imports(&mut self) {
        for module in self.modules {
            for import in &module.imports {
                let Some(target) = import.target else {
                    continue;
                };
                if !import_binding_is_used(module, import) {
                    continue;
                }
                match import.fact.kind {
                    ImportKind::Named => {
                        if let Some(name) = import.fact.imported_name.as_deref() {
                            let mut visited = BTreeSet::new();
                            self.mark_imported_name(target, name, "import", &mut visited);
                        }
                    }
                    ImportKind::Default => {
                        let mut visited = BTreeSet::new();
                        self.mark_imported_name(target, "default", "import", &mut visited);
                    }
                    ImportKind::Namespace => {
                        let mut visited = BTreeSet::new();
                        self.mark_all_uncertain(target, "namespace_import", &mut visited);
                    }
                    ImportKind::SideEffect => {}
                }
            }
        }
    }

    fn apply_re_exports(&mut self) {
        for module in self.modules {
            for re_export in &module.re_exports {
                let Some(target) = re_export.target else {
                    continue;
                };
                match re_export.fact.kind {
                    ReExportKind::Named => {
                        if let Some(imported_name) = re_export.fact.imported_name.as_deref() {
                            let mut visited = BTreeSet::new();
                            self.mark_imported_name(
                                target,
                                imported_name,
                                "re_export",
                                &mut visited,
                            );
                        }
                    }
                    ReExportKind::Star => {
                        let mut visited = BTreeSet::new();
                        self.mark_all_uncertain(target, "wildcard_import", &mut visited);
                    }
                    ReExportKind::Namespace => {
                        let exported_name = re_export.fact.exported_name.as_deref();
                        let namespace_is_used = exported_name.is_some_and(|name| {
                            self.export_status(module.facts.file_id, name)
                                .is_some_and(|status| matches!(status, ExportStatus::Used(_)))
                        });
                        if namespace_is_used {
                            let mut visited = BTreeSet::new();
                            self.mark_all_uncertain(target, "namespace_import", &mut visited);
                        }
                    }
                }
            }
        }
    }

    fn apply_dynamic_imports(&mut self) {
        let mut has_non_literal = false;
        let mut literal_targets = Vec::new();
        for module in self.modules {
            for dynamic in &module.dynamic_imports {
                if dynamic.fact.is_literal {
                    if let Some(target) = dynamic.target {
                        literal_targets.push(target);
                    }
                } else {
                    has_non_literal = true;
                }
            }
        }
        for target in literal_targets {
            let mut visited = BTreeSet::new();
            self.mark_all_uncertain(target, "dynamic_import", &mut visited);
        }
        if has_non_literal {
            for state in &mut self.states {
                for export in &mut state.exports {
                    mark_uncertain(export, "dynamic_import_nonliteral");
                }
            }
        }
    }

    fn mark_imported_name(
        &mut self,
        target: FileId,
        name: &str,
        reason: &str,
        visited: &mut BTreeSet<(usize, String)>,
    ) {
        if !visited.insert((target.0, name.to_string())) {
            return;
        }
        if let Some(state) = self.states.get_mut(target.0) {
            for export in state
                .exports
                .iter_mut()
                .filter(|export| export.fact.name.matches_str(name))
            {
                mark_used(export, reason);
            }
        }

        let Some(module) = self.modules.get(target.0) else {
            return;
        };
        let re_exports = module.re_exports.clone();
        for re_export in re_exports {
            let Some(source) = re_export.target else {
                continue;
            };
            match re_export.fact.kind {
                ReExportKind::Named => {
                    if re_export.fact.exported_name.as_deref() == Some(name) {
                        if let Some(imported_name) = re_export.fact.imported_name.as_deref() {
                            self.mark_imported_name(source, imported_name, "re_export", visited);
                        }
                    }
                }
                ReExportKind::Star => {
                    self.mark_imported_name(source, name, "re_export", visited);
                }
                ReExportKind::Namespace => {
                    if re_export.fact.exported_name.as_deref() == Some(name) {
                        let mut uncertain_visited = BTreeSet::new();
                        self.mark_all_uncertain(source, "namespace_import", &mut uncertain_visited);
                    }
                }
            }
        }
    }

    fn mark_imported_name_uncertain(
        &mut self,
        target: FileId,
        name: &str,
        reason: &str,
        visited: &mut BTreeSet<(usize, String)>,
    ) {
        if !visited.insert((target.0, name.to_string())) {
            return;
        }
        if let Some(state) = self.states.get_mut(target.0) {
            for export in state
                .exports
                .iter_mut()
                .filter(|export| export.fact.name.matches_str(name))
            {
                mark_uncertain(export, reason);
            }
        }
        let Some(module) = self.modules.get(target.0) else {
            return;
        };
        let re_exports = module.re_exports.clone();
        for re_export in re_exports {
            let Some(source) = re_export.target else {
                continue;
            };
            match re_export.fact.kind {
                ReExportKind::Named => {
                    if re_export.fact.exported_name.as_deref() == Some(name) {
                        if let Some(imported_name) = re_export.fact.imported_name.as_deref() {
                            self.mark_imported_name_uncertain(
                                source,
                                imported_name,
                                reason,
                                visited,
                            );
                        }
                    }
                }
                ReExportKind::Star => {
                    self.mark_imported_name_uncertain(source, name, reason, visited);
                }
                ReExportKind::Namespace => {}
            }
        }
    }

    fn mark_all_uncertain(&mut self, target: FileId, reason: &str, visited: &mut BTreeSet<usize>) {
        if !visited.insert(target.0) {
            return;
        }
        if let Some(state) = self.states.get_mut(target.0) {
            for export in &mut state.exports {
                mark_uncertain(export, reason);
            }
        }
        let Some(module) = self.modules.get(target.0) else {
            return;
        };
        let re_exports = module.re_exports.clone();
        for re_export in re_exports {
            let Some(source) = re_export.target else {
                continue;
            };
            match re_export.fact.kind {
                ReExportKind::Star => self.mark_all_uncertain(source, reason, visited),
                ReExportKind::Named => {
                    if let Some(imported_name) = re_export.fact.imported_name.as_deref() {
                        let mut name_visited = BTreeSet::new();
                        self.mark_imported_name_uncertain(
                            source,
                            imported_name,
                            reason,
                            &mut name_visited,
                        );
                    }
                }
                ReExportKind::Namespace => {}
            }
        }
    }

    fn export_status(&self, file_id: FileId, name: &str) -> Option<&ExportStatus> {
        self.states.get(file_id.0).and_then(|state| {
            state
                .exports
                .iter()
                .find(|export| export.fact.name.matches_str(name))
                .map(|export| &export.status)
        })
    }

    fn into_verdicts(self, project_root: &Path) -> Vec<OxcFileVerdicts> {
        self.states
            .into_iter()
            .map(|state| {
                let relative_file = relative_string(project_root, &state.path);
                let mut seen = BTreeSet::new();
                let exports = state
                    .exports
                    .into_iter()
                    .filter(|export| seen.insert(export.fact.name.as_symbol()))
                    .map(|export| {
                        let (verdict, reason) = export.status.verdict();
                        OxcExportVerdict {
                            symbol: export.fact.name.as_symbol(),
                            kind: export.fact.kind,
                            line: export.fact.line,
                            verdict,
                            reason,
                            provenance: OXC_PROVENANCE.to_string(),
                        }
                    })
                    .collect::<Vec<_>>();
                OxcFileVerdicts {
                    file: state.path,
                    relative_file,
                    exports,
                }
            })
            .collect()
    }
}

fn import_binding_is_used(
    module: &ResolvedModule,
    import: &super::resolver::ResolvedImport,
) -> bool {
    match import.fact.kind {
        ImportKind::SideEffect => false,
        ImportKind::Named | ImportKind::Default | ImportKind::Namespace => import
            .fact
            .local_name
            .as_ref()
            .is_some_and(|local| module.facts.used_import_bindings.contains(local)),
    }
}

fn push_synthetic_export(
    exports: &mut Vec<ExportState>,
    name: ExportName,
    kind: &str,
    is_type_only: bool,
    line: u32,
) {
    if exports.iter().any(|export| export.fact.name == name) {
        return;
    }
    exports.push(ExportState {
        fact: ExportFact {
            name,
            local_name: None,
            kind: kind.to_string(),
            is_type_only,
            line,
            declared: true,
        },
        status: ExportStatus::Unused,
    });
}

fn mark_used(export: &mut ExportState, reason: &str) {
    if !matches!(export.status, ExportStatus::Used(_)) {
        export.status = ExportStatus::Used(reason.to_string());
    }
}

fn mark_uncertain(export: &mut ExportState, reason: &str) {
    if matches!(export.status, ExportStatus::Unused) {
        export.status = ExportStatus::Uncertain(reason.to_string());
    }
}

fn relative_string(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
