use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn ingest_excludes_gitignored_and_codeateignored_files() {
    let repo = codeate_core::ingest(fixture("basic_repo")).expect("ingest basic_repo");

    let paths: Vec<&str> = repo.files.iter().map(|f| f.path.as_str()).collect();

    assert!(paths.contains(&"README.md"));
    assert!(paths.contains(&"src/main.rs"));
    assert!(paths.contains(&"src/lib.rs"));

    assert!(
        !paths.iter().any(|p| p.contains("target/")),
        "gitignored target/ leaked into ingested files: {paths:?}"
    );
    assert!(
        !paths.contains(&"secrets.local"),
        "codeateignored file leaked into ingested files: {paths:?}"
    );
}

#[test]
fn ingest_returns_files_sorted_by_path() {
    let repo = codeate_core::ingest(fixture("basic_repo")).expect("ingest basic_repo");

    let paths: Vec<&str> = repo.files.iter().map(|f| f.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort();

    assert_eq!(paths, sorted);
}

#[test]
fn ingest_computes_size_and_hash_for_each_file() {
    let repo = codeate_core::ingest(fixture("basic_repo")).expect("ingest basic_repo");

    let lib_rs = repo
        .files
        .iter()
        .find(|f| f.path == "src/lib.rs")
        .expect("src/lib.rs present");

    let expected_bytes = std::fs::read(fixture("basic_repo").join("src/lib.rs")).unwrap();
    assert_eq!(lib_rs.size_bytes, expected_bytes.len() as u64);
    assert_eq!(lib_rs.content_hash, codeate_core::hash_bytes(&expected_bytes));
}

#[test]
fn ingest_of_a_missing_root_is_an_error() {
    let result = codeate_core::ingest(fixture("does_not_exist"));
    assert!(result.is_err());
}
