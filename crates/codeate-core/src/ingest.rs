use std::path::Path;

use ignore::WalkBuilder;

use crate::error::{Error, Result};
use crate::hash::hash_bytes;
use crate::repository::{File, Repository};

/// Walk `root`, respecting `.gitignore` and `.codeateignore`, and return a
/// `Repository` describing every non-ignored file: its path (relative to
/// `root`, normalized), content hash, and size.
///
/// File order is deterministic (sorted by path) regardless of the host OS's
/// directory iteration order.
pub fn ingest(root: impl AsRef<Path>) -> Result<Repository> {
    let root = root.as_ref();
    if !root.is_dir() {
        return Err(Error::Message(format!(
            "ingestion root is not a directory: {}",
            root.display()
        )));
    }

    let mut files = Vec::new();

    let walker = WalkBuilder::new(root)
        .add_custom_ignore_filename(".codeateignore")
        .build();

    for entry in walker {
        let entry = entry.map_err(|err| Error::Message(err.to_string()))?;

        let is_file = entry.file_type().map(|ft| ft.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }

        let absolute_path = entry.path();
        let relative_path = absolute_path
            .strip_prefix(root)
            .unwrap_or(absolute_path)
            .to_string_lossy()
            .into_owned();

        let bytes = std::fs::read(absolute_path)?;
        let content_hash = hash_bytes(&bytes);
        let size_bytes = bytes.len() as u64;

        files.push(File::new(relative_path, content_hash, size_bytes));
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Repository::new(root, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingesting_a_missing_directory_returns_an_error() {
        let result = ingest("this/path/does/not/exist");
        assert!(result.is_err());
    }
}
