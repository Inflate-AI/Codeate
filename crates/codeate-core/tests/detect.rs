use std::path::PathBuf;

use codeate_core::LanguageId;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn detect_languages_annotates_every_ingested_file() {
    let repo = codeate_core::ingest(fixture("basic_repo")).expect("ingest basic_repo");
    let registry = codeate_core::default_registry();
    let annotated = codeate_core::detect_languages(&repo, &registry).expect("detect languages");

    let language_of = |path: &str| {
        annotated
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} present in annotated repository"))
            .language
            .clone()
    };

    assert_eq!(language_of("src/main.rs"), LanguageId::known("rust"));
    assert_eq!(language_of("src/lib.rs"), LanguageId::known("rust"));
    assert_eq!(language_of("README.md"), LanguageId::known("markdown"));
    assert_eq!(language_of("scripts/build"), LanguageId::known("python"));
    assert_eq!(language_of("notes.xyz"), LanguageId::Unknown);

    // Ingestion's own guarantees (file set, ordering) are untouched.
    assert_eq!(annotated.files.len(), repo.files.len());
}
