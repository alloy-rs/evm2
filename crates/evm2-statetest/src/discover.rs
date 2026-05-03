use crate::error::{TestError, TestErrorKind};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Finds all JSON state test files under `paths`.
pub(crate) fn find_json_tests(paths: &[PathBuf]) -> Result<Vec<PathBuf>, TestError> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            if is_json_test(path) {
                files.push(path.clone());
            }
            continue;
        }
        if !path.exists() {
            return Err(TestError::path(path, TestErrorKind::InvalidPath));
        }
        for entry in WalkDir::new(path).follow_links(true) {
            let entry = entry.map_err(|err| TestError::path(path, err.into()))?;
            if entry.file_type().is_file() && is_json_test(entry.path()) {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files.sort_unstable();
    if files.is_empty() {
        return Err(TestError::path(PathBuf::new(), TestErrorKind::NoJsonFiles));
    }
    Ok(files)
}

fn is_json_test(path: &Path) -> bool {
    path.file_name().is_none_or(|name| name != "index.json")
        && path.extension().is_some_and(|ext| ext == "json")
}
