use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, EntityId, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::repository::Repository;

/// One `Cargo.toml` deserialized just enough to build Phase 14's
/// repository-level entities. Fields Codeate doesn't model yet (workspace
/// dependency inheritance, target-specific deps, `[patch]`, ...) are
/// deliberately left out rather than partially, incorrectly modeled.
#[derive(Debug, Default, Deserialize)]
struct CargoManifest {
    package: Option<PackageTable>,
    dependencies: Option<HashMap<String, toml::Value>>,
    #[serde(rename = "dev-dependencies")]
    dev_dependencies: Option<HashMap<String, toml::Value>>,
    #[serde(rename = "build-dependencies")]
    build_dependencies: Option<HashMap<String, toml::Value>>,
    lib: Option<TargetTable>,
    #[serde(default)]
    bin: Vec<TargetTable>,
    #[serde(default)]
    test: Vec<TargetTable>,
    #[serde(default)]
    bench: Vec<TargetTable>,
    #[serde(default)]
    example: Vec<TargetTable>,
    profile: Option<HashMap<String, toml::Value>>,
}

#[derive(Debug, Deserialize)]
struct PackageTable {
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct TargetTable {
    name: Option<String>,
    path: Option<String>,
}

/// A `BuildTarget` this run discovered, kept around (rather than only its
/// entity) so a later stage (Phase 14 task 4's entry-point detection) can
/// find "the file this target builds from" without re-parsing the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildTargetInfo {
    pub id: EntityId,
    pub kind: BuildTargetKind,
    /// Path (relative to the repository root) of the target's root source
    /// file, e.g. `src/main.rs` or `src/lib.rs`.
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildTargetKind {
    Lib,
    Bin,
    Test,
    Bench,
    Example,
}

impl BuildTargetKind {
    fn as_str(&self) -> &'static str {
        match self {
            BuildTargetKind::Lib => "lib",
            BuildTargetKind::Bin => "bin",
            BuildTargetKind::Test => "test",
            BuildTargetKind::Bench => "bench",
            BuildTargetKind::Example => "example",
        }
    }
}

/// Entities/edges Phase 14's manifest parsing contributes to the graph,
/// meant to be merged into a `pipeline::AnalysisResult` alongside the
/// Phase 4-8 output.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ManifestResult {
    pub entities: Vec<SyntaxEntity>,
    pub symbols: Vec<SymbolEntity>,
    pub edges: Vec<Edge>,
    pub owner_file: HashMap<EntityId, String>,
    pub build_targets: Vec<BuildTargetInfo>,
}

/// Parse every `Cargo.toml` ingested into `repository` (a manifest with no
/// `[package]` table -- a pure virtual workspace root -- is skipped: there
/// is no package to attach dependencies/targets/profiles to) into `Package`,
/// `Dependency`, `BuildTarget`, and `Configuration` entities.
pub fn parse_manifests(repository: &Repository) -> Result<ManifestResult> {
    let mut result = ManifestResult::default();

    for file in &repository.files {
        if !is_cargo_manifest(&file.path) {
            continue;
        }

        let full_path = repository.root.join(&file.path);
        let text = std::fs::read_to_string(&full_path)?;
        let manifest: CargoManifest = toml::from_str(&text)
            .map_err(|err| Error::Message(format!("invalid Cargo.toml at {}: {err}", file.path)))?;
        let Some(package) = &manifest.package else {
            continue;
        };

        let location = file.path.as_str();
        let package_entity = SyntaxEntity::new(SyntaxKind::Package, location, &package.name);
        let package_symbol = SymbolEntity::new(SymbolKind::Namespace, location, &package.name);
        result
            .owner_file
            .insert(package_entity.id.clone(), file.path.clone());
        result
            .owner_file
            .insert(package_symbol.id.clone(), file.path.clone());

        add_dependencies(
            &mut result,
            &manifest,
            &package_entity,
            &package_symbol,
            location,
            &file.path,
        );
        add_build_targets(
            &mut result,
            &manifest,
            package,
            &package_entity,
            location,
            &file.path,
            repository,
        );
        add_configurations(
            &mut result,
            &manifest,
            &package_entity,
            location,
            &file.path,
        );

        result.entities.push(package_entity);
        result.symbols.push(package_symbol);
    }

    Ok(result)
}

fn is_cargo_manifest(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("Cargo.toml")
}

fn manifest_dir(manifest_path: &str) -> String {
    match std::path::Path::new(manifest_path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.to_string_lossy().replace('\\', "/")
        }
        _ => String::new(),
    }
}

fn join_relative(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

fn add_dependencies(
    result: &mut ManifestResult,
    manifest: &CargoManifest,
    package_entity: &SyntaxEntity,
    package_symbol: &SymbolEntity,
    location: &str,
    owner_path: &str,
) {
    let sections: [&Option<HashMap<String, toml::Value>>; 3] = [
        &manifest.dependencies,
        &manifest.dev_dependencies,
        &manifest.build_dependencies,
    ];

    let mut names: Vec<&str> = Vec::new();
    for deps in sections.into_iter().flatten() {
        names.extend(deps.keys().map(String::as_str));
    }
    names.sort_unstable();
    names.dedup();

    for name in names {
        let dep_entity = SyntaxEntity::new(SyntaxKind::Dependency, location, name);
        let dep_symbol = SymbolEntity::new(SymbolKind::Constant, location, name);
        result
            .owner_file
            .insert(dep_entity.id.clone(), owner_path.to_string());
        result
            .owner_file
            .insert(dep_symbol.id.clone(), owner_path.to_string());
        result.edges.push(Edge::new(
            package_entity.id.clone(),
            dep_entity.id.clone(),
            EdgeKind::Contains,
        ));
        result.edges.push(Edge::new(
            package_symbol.id.clone(),
            dep_symbol.id.clone(),
            EdgeKind::DependsOn,
        ));
        result.entities.push(dep_entity);
        result.symbols.push(dep_symbol);
    }
}

#[allow(clippy::too_many_arguments)]
fn add_build_targets(
    result: &mut ManifestResult,
    manifest: &CargoManifest,
    package: &PackageTable,
    package_entity: &SyntaxEntity,
    location: &str,
    owner_path: &str,
    repository: &Repository,
) {
    let dir = manifest_dir(owner_path);
    let has_file = |rel: &str| {
        let candidate = join_relative(&dir, rel);
        repository.files.iter().any(|f| f.path == candidate)
    };

    let mut targets: Vec<(BuildTargetKind, String, String)> = Vec::new();

    if let Some(lib) = &manifest.lib {
        let path = lib.path.clone().unwrap_or_else(|| "src/lib.rs".to_string());
        let name = lib.name.clone().unwrap_or_else(|| package.name.clone());
        targets.push((BuildTargetKind::Lib, name, join_relative(&dir, &path)));
    } else if has_file("src/lib.rs") {
        targets.push((
            BuildTargetKind::Lib,
            package.name.clone(),
            join_relative(&dir, "src/lib.rs"),
        ));
    }

    if !manifest.bin.is_empty() {
        for bin in &manifest.bin {
            let path = bin
                .path
                .clone()
                .unwrap_or_else(|| "src/main.rs".to_string());
            let name = bin.name.clone().unwrap_or_else(|| package.name.clone());
            targets.push((BuildTargetKind::Bin, name, join_relative(&dir, &path)));
        }
    } else if has_file("src/main.rs") {
        targets.push((
            BuildTargetKind::Bin,
            package.name.clone(),
            join_relative(&dir, "src/main.rs"),
        ));
    }

    for (kind, entries) in [
        (BuildTargetKind::Test, &manifest.test),
        (BuildTargetKind::Bench, &manifest.bench),
        (BuildTargetKind::Example, &manifest.example),
    ] {
        for entry in entries {
            let (Some(name), Some(path)) = (&entry.name, &entry.path) else {
                continue;
            };
            targets.push((kind, name.clone(), join_relative(&dir, path)));
        }
    }

    for (kind, name, path) in targets {
        let entity = SyntaxEntity::new(
            SyntaxKind::BuildTarget,
            location,
            format!("{}:{name}", kind.as_str()),
        );
        result
            .owner_file
            .insert(entity.id.clone(), owner_path.to_string());
        result.edges.push(Edge::new(
            package_entity.id.clone(),
            entity.id.clone(),
            EdgeKind::Contains,
        ));
        result.build_targets.push(BuildTargetInfo {
            id: entity.id.clone(),
            kind,
            path,
        });
        result.entities.push(entity);
    }
}

fn add_configurations(
    result: &mut ManifestResult,
    manifest: &CargoManifest,
    package_entity: &SyntaxEntity,
    location: &str,
    owner_path: &str,
) {
    let Some(profiles) = &manifest.profile else {
        return;
    };
    let mut names: Vec<&str> = profiles.keys().map(String::as_str).collect();
    names.sort_unstable();

    for name in names {
        let entity = SyntaxEntity::new(SyntaxKind::Configuration, location, name);
        result
            .owner_file
            .insert(entity.id.clone(), owner_path.to_string());
        result.edges.push(Edge::new(
            package_entity.id.clone(),
            entity.id.clone(),
            EdgeKind::Contains,
        ));
        result.entities.push(entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::LanguageId;
    use crate::repository::File;

    struct TempRepo {
        root: std::path::PathBuf,
    }

    impl TempRepo {
        fn new(files: &[(&str, &str)]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "codeate-manifest-test-{}-{}",
                std::process::id(),
                unique_suffix()
            ));
            std::fs::create_dir_all(&root).expect("create temp repo root");
            for (path, content) in files {
                let full = root.join(path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).expect("create temp repo subdir");
                }
                std::fs::write(&full, content).expect("write temp repo file");
            }
            Self { root }
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn repo_with_paths(root: &std::path::Path, paths: &[&str]) -> Repository {
        let files = paths
            .iter()
            .map(|path| {
                let full = root.join(path);
                let size = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
                File::new(*path, "irrelevant-hash", size).with_language(LanguageId::Unknown)
            })
            .collect();
        Repository::new(root, files)
    }

    #[test]
    fn a_manifest_with_no_package_table_is_skipped() {
        let repo = TempRepo::new(&[("Cargo.toml", "[workspace]\nmembers = []\n")]);
        let repository = repo_with_paths(&repo.root, &["Cargo.toml"]);

        let result = parse_manifests(&repository).expect("parse succeeds");

        assert!(result.entities.is_empty());
    }

    #[test]
    fn a_package_table_produces_a_package_entity_and_symbol() {
        let repo = TempRepo::new(&[("Cargo.toml", "[package]\nname = \"widgets\"\n")]);
        let repository = repo_with_paths(&repo.root, &["Cargo.toml"]);

        let result = parse_manifests(&repository).expect("parse succeeds");

        let package = result
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Package && e.name == "widgets")
            .expect("package entity");
        assert!(result
            .symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Namespace && s.name == "widgets"));
        assert_eq!(
            result.owner_file.get(&package.id).map(String::as_str),
            Some("Cargo.toml")
        );
    }

    #[test]
    fn dependencies_across_all_three_sections_become_dependency_entities() {
        let repo = TempRepo::new(&[(
            "Cargo.toml",
            "[package]\nname = \"widgets\"\n\n[dependencies]\nserde = \"1\"\n\n[dev-dependencies]\nmockall = \"0.11\"\n\n[build-dependencies]\ncc = \"1\"\n",
        )]);
        let repository = repo_with_paths(&repo.root, &["Cargo.toml"]);

        let result = parse_manifests(&repository).expect("parse succeeds");

        for name in ["serde", "mockall", "cc"] {
            assert!(
                result
                    .entities
                    .iter()
                    .any(|e| e.kind == SyntaxKind::Dependency && e.name == name),
                "expected a Dependency entity named {name}"
            );
        }

        let package_id = result
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Package)
            .unwrap()
            .id
            .clone();
        let serde_id = result
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Dependency && e.name == "serde")
            .unwrap()
            .id
            .clone();
        assert!(result
            .edges
            .contains(&Edge::new(package_id, serde_id, EdgeKind::Contains)));
    }

    #[test]
    fn an_explicit_lib_and_bin_target_become_build_target_entities() {
        let repo = TempRepo::new(&[(
            "Cargo.toml",
            "[package]\nname = \"widgets\"\n\n[lib]\nname = \"widgets_core\"\n\n[[bin]]\nname = \"widgets-cli\"\npath = \"src/cli.rs\"\n",
        )]);
        let repository = repo_with_paths(&repo.root, &["Cargo.toml"]);

        let result = parse_manifests(&repository).expect("parse succeeds");

        assert!(result
            .entities
            .iter()
            .any(|e| e.kind == SyntaxKind::BuildTarget && e.name == "lib:widgets_core"));
        assert!(result
            .entities
            .iter()
            .any(|e| e.kind == SyntaxKind::BuildTarget && e.name == "bin:widgets-cli"));

        let bin_target = result
            .build_targets
            .iter()
            .find(|t| t.kind == BuildTargetKind::Bin)
            .expect("bin build target info");
        assert_eq!(bin_target.path, "src/cli.rs");
    }

    #[test]
    fn an_implicit_lib_and_bin_are_detected_from_conventional_paths() {
        let repo = TempRepo::new(&[
            ("Cargo.toml", "[package]\nname = \"widgets\"\n"),
            ("src/lib.rs", ""),
            ("src/main.rs", ""),
        ]);
        let repository = repo_with_paths(&repo.root, &["Cargo.toml", "src/lib.rs", "src/main.rs"]);

        let result = parse_manifests(&repository).expect("parse succeeds");

        assert!(result
            .build_targets
            .iter()
            .any(|t| t.kind == BuildTargetKind::Lib && t.path == "src/lib.rs"));
        assert!(result
            .build_targets
            .iter()
            .any(|t| t.kind == BuildTargetKind::Bin && t.path == "src/main.rs"));
    }

    #[test]
    fn a_nested_crate_manifest_resolves_targets_relative_to_its_own_directory() {
        let repo = TempRepo::new(&[
            (
                "crates/widgets/Cargo.toml",
                "[package]\nname = \"widgets\"\n",
            ),
            ("crates/widgets/src/lib.rs", ""),
        ]);
        let repository = repo_with_paths(
            &repo.root,
            &["crates/widgets/Cargo.toml", "crates/widgets/src/lib.rs"],
        );

        let result = parse_manifests(&repository).expect("parse succeeds");

        let lib_target = result
            .build_targets
            .iter()
            .find(|t| t.kind == BuildTargetKind::Lib)
            .expect("lib build target info");
        assert_eq!(lib_target.path, "crates/widgets/src/lib.rs");
    }

    #[test]
    fn profile_sections_become_configuration_entities() {
        let repo = TempRepo::new(&[(
            "Cargo.toml",
            "[package]\nname = \"widgets\"\n\n[profile.release]\nopt-level = 3\n\n[profile.dev]\nopt-level = 0\n",
        )]);
        let repository = repo_with_paths(&repo.root, &["Cargo.toml"]);

        let result = parse_manifests(&repository).expect("parse succeeds");

        for name in ["release", "dev"] {
            assert!(result
                .entities
                .iter()
                .any(|e| e.kind == SyntaxKind::Configuration && e.name == name));
        }
    }

    #[test]
    fn a_malformed_manifest_is_a_clear_error() {
        let repo = TempRepo::new(&[("Cargo.toml", "not valid toml [[[")]);
        let repository = repo_with_paths(&repo.root, &["Cargo.toml"]);

        let err = parse_manifests(&repository).expect_err("malformed manifest should error");

        assert!(matches!(err, Error::Message(_)));
        assert!(err.to_string().contains("Cargo.toml"));
    }

    #[test]
    fn a_repository_with_no_manifest_produces_nothing() {
        let repo = TempRepo::new(&[("src/lib.rs", "")]);
        let repository = repo_with_paths(&repo.root, &["src/lib.rs"]);

        let result = parse_manifests(&repository).expect("parse succeeds");

        assert!(result.entities.is_empty());
        assert!(result.symbols.is_empty());
        assert!(result.edges.is_empty());
    }
}
