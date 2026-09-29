use std::path::PathBuf;
use std::process::Command;

fn codeate() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codeate"))
}

/// A throwaway directory under the OS temp dir, populated with the given
/// (relative path, content) files, removed on drop.
struct TempRepo {
    root: PathBuf,
}

impl TempRepo {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codeate-cli-test-{}-{}",
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

#[test]
fn version_flag_exits_zero_and_prints_the_binary_name_and_version() {
    let output = codeate()
        .arg("--version")
        .output()
        .expect("run codeate --version");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("codeate"));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag_exits_zero_and_prints_usage() {
    let output = codeate()
        .arg("--help")
        .output()
        .expect("run codeate --help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.to_lowercase().contains("usage"));
}

#[test]
fn no_args_is_a_usage_error_since_a_subcommand_is_required() {
    let output = codeate().output().expect("run codeate with no args");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.to_lowercase().contains("usage"));
}

/// Analyze `repo` and return the path `codeate analyze` wrote the graph to.
/// Panics (via assert) on failure -- shared setup for query-command tests,
/// not itself a test.
fn analyze(repo: &TempRepo) -> PathBuf {
    let output = codeate()
        .arg("analyze")
        .arg(&repo.root)
        .output()
        .expect("run codeate analyze");
    assert!(
        output.status.success(),
        "analyze failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    repo.root.join(".codeate").join("graph.json")
}

#[test]
fn analyze_persists_a_graph_and_prints_a_summary() {
    let repo = TempRepo::new(&[(
        "src/lib.rs",
        "pub struct Point { pub x: i32 }\npub fn run() {}",
    )]);

    let output = codeate()
        .arg("analyze")
        .arg(&repo.root)
        .output()
        .expect("run codeate analyze");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("files"));
    assert!(stdout.contains("symbols"));
    assert!(stdout.contains("edges"));

    let store_path = repo.root.join(".codeate").join("graph.json");
    assert!(store_path.exists(), "expected {} to exist", store_path.display());

    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).expect("read store"))
            .expect("store is valid json");
    assert!(stored["symbols"]
        .as_array()
        .expect("symbols array")
        .iter()
        .any(|s| s["name"] == "Point"));
}

#[test]
fn query_reports_a_clear_error_when_the_store_is_missing() {
    let output = codeate()
        .args(["query", "entry-points", "--store"])
        .arg("/definitely/not/a/real/store/codeate-test.json")
        .output()
        .expect("run codeate query");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("codeate-test.json"));
    assert!(stderr.to_lowercase().contains("analyze"));
}

#[test]
fn query_reports_a_clear_error_when_the_store_is_not_valid_json() {
    let repo = TempRepo::new(&[("graph.json", "not json at all")]);
    let store_path = repo.root.join("graph.json");

    let output = codeate()
        .args(["query", "entry-points", "--store"])
        .arg(&store_path)
        .output()
        .expect("run codeate query");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn analyze_rejects_a_nonexistent_path() {
    let output = codeate()
        .arg("analyze")
        .arg("/definitely/not/a/real/path/codeate-test")
        .output()
        .expect("run codeate analyze");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn query_symbol_finds_a_known_symbol_by_name_in_both_output_modes() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub struct Point { pub x: i32 }")]);
    let store = analyze(&repo);

    let human = codeate()
        .args(["query", "symbol", "Point", "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query symbol");
    assert!(human.status.success());
    let stdout = String::from_utf8(human.stdout).expect("utf8 stdout");
    assert!(stdout.contains("Point"));
    assert!(stdout.contains("Type"));

    let json_output = codeate()
        .args(["query", "symbol", "Point", "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query symbol --json");
    assert!(json_output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&json_output.stdout).expect("valid json");
    let matches = value.as_array().expect("json array");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["name"], "Point");
    assert_eq!(matches[0]["kind"], "Type");
    assert!(matches[0]["id"].as_str().is_some());
}

#[test]
fn query_symbol_with_no_matches_exits_zero_with_an_empty_result() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub struct Point { pub x: i32 }")]);
    let store = analyze(&repo);

    let output = codeate()
        .args(["query", "symbol", "Nonexistent", "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query symbol");

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    assert_eq!(value.as_array().expect("json array").len(), 0);
}

fn symbol_id(store: &std::path::Path, name: &str) -> String {
    let output = codeate()
        .args(["query", "symbol", name, "--store"])
        .arg(store)
        .arg("--json")
        .output()
        .expect("run codeate query symbol");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    value.as_array().expect("json array")[0]["id"]
        .as_str()
        .expect("id string")
        .to_string()
}

#[test]
fn query_definition_follows_the_symbol_to_its_declaring_entity() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub struct Point { pub x: i32 }")]);
    let store = analyze(&repo);
    let id = symbol_id(&store, "Point");

    let output = codeate()
        .args(["query", "definition", "--id", &id, "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query definition");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("src/lib.rs"));
}

#[test]
fn query_callers_and_callees_reflect_a_call_edge() {
    let repo = TempRepo::new(&[(
        "src/lib.rs",
        "pub fn helper() {}\npub fn run() { helper(); }",
    )]);
    let store = analyze(&repo);
    let helper_id = symbol_id(&store, "helper");
    let run_id = symbol_id(&store, "run");

    let callers = codeate()
        .args(["query", "callers", "--id", &helper_id, "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query callers");
    assert!(callers.status.success());
    let value: serde_json::Value = serde_json::from_slice(&callers.stdout).expect("valid json");
    assert_eq!(value[0]["name"], "run");

    let callees = codeate()
        .args(["query", "callees", "--id", &run_id, "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query callees");
    assert!(callees.status.success());
    let value: serde_json::Value = serde_json::from_slice(&callees.stdout).expect("valid json");
    assert_eq!(value[0]["name"], "helper");
}

#[test]
fn query_callers_rejects_an_unknown_id() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub fn helper() {}")]);
    let store = analyze(&repo);

    let output = codeate()
        .args(["query", "callers", "--id", "not-a-real-id", "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query callers");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn query_dependencies_and_dependents_follow_a_cross_file_import() {
    let repo = TempRepo::new(&[
        ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
        (
            "src/circle.rs",
            "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
        ),
    ]);
    let store = analyze(&repo);
    let circle_module_id = symbol_id(&store, "src/circle.rs");
    let shape_id = symbol_id(&store, "Shape");

    let deps = codeate()
        .args(["query", "dependencies", "--id", &circle_module_id, "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query dependencies");
    assert!(deps.status.success());
    let value: serde_json::Value = serde_json::from_slice(&deps.stdout).expect("valid json");
    assert!(value
        .as_array()
        .expect("json array")
        .iter()
        .any(|v| v["name"] == "Shape"));

    let dependents = codeate()
        .args(["query", "dependents", "--id", &shape_id, "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query dependents");
    assert!(dependents.status.success());
    let value: serde_json::Value = serde_json::from_slice(&dependents.stdout).expect("valid json");
    assert!(value
        .as_array()
        .expect("json array")
        .iter()
        .any(|v| v["name"] == "src/circle.rs"));
}

#[test]
fn query_implementers_finds_a_trait_implementer() {
    let repo = TempRepo::new(&[(
        "src/lib.rs",
        "pub trait Shape { fn area(&self) -> f64; }\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
    )]);
    let store = analyze(&repo);
    let shape_id = symbol_id(&store, "Shape");

    let output = codeate()
        .args(["query", "implementers", "--id", &shape_id, "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query implementers");

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    assert!(value
        .as_array()
        .expect("json array")
        .iter()
        .any(|v| v["name"] == "Circle"));
}

#[test]
fn query_references_finds_a_referencing_function() {
    let repo = TempRepo::new(&[(
        "src/lib.rs",
        "pub fn helper() {}\npub fn run() { helper(); }",
    )]);
    let store = analyze(&repo);
    let helper_id = symbol_id(&store, "helper");

    let output = codeate()
        .args(["query", "references", "--id", &helper_id, "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query references");

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    assert!(value
        .as_array()
        .expect("json array")
        .iter()
        .any(|v| v["name"] == "run"));
}

#[test]
fn query_impact_reports_every_transitive_caller() {
    let repo = TempRepo::new(&[(
        "src/lib.rs",
        "pub fn c() {}\npub fn b() { c(); }\npub fn a() { b(); }",
    )]);
    let store = analyze(&repo);
    let c_id = symbol_id(&store, "c");

    let output = codeate()
        .args(["query", "impact", "--id", &c_id, "--store"])
        .arg(&store)
        .arg("--json")
        .output()
        .expect("run codeate query impact");

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let names: Vec<&str> = value
        .as_array()
        .expect("json array")
        .iter()
        .map(|v| v["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"b"));
    assert!(names.contains(&"a"));
}

#[test]
fn query_impact_rejects_an_unknown_id() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub fn helper() {}")]);
    let store = analyze(&repo);

    let output = codeate()
        .args(["query", "impact", "--id", "not-a-real-id", "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query impact");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn query_references_rejects_an_unknown_id() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub fn helper() {}")]);
    let store = analyze(&repo);

    let output = codeate()
        .args(["query", "references", "--id", "not-a-real-id", "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query references");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn query_implementers_rejects_an_unknown_id() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub fn helper() {}")]);
    let store = analyze(&repo);

    let output = codeate()
        .args(["query", "implementers", "--id", "not-a-real-id", "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query implementers");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn query_dependencies_rejects_an_unknown_id() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub fn helper() {}")]);
    let store = analyze(&repo);

    let output = codeate()
        .args(["query", "dependencies", "--id", "not-a-real-id", "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query dependencies");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn query_definition_rejects_an_unknown_id() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub struct Point { pub x: i32 }")]);
    let store = analyze(&repo);

    let output = codeate()
        .args(["query", "definition", "--id", "not-a-real-id", "--store"])
        .arg(&store)
        .output()
        .expect("run codeate query definition");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
}
