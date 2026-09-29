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
