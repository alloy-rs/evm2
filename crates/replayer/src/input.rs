use crate::{
    corpus,
    error::{Error, Result},
};
use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

pub(crate) struct Plan {
    pub(crate) label: PathBuf,
    pub(crate) files: Vec<PathBuf>,
}

/// Builds a replay plan from the input path supplied on the CLI.
///
/// Supported inputs are:
/// - a generated corpus directory containing `manifest.json`; block files are loaded in manifest
///   order from the manifest's `file_name` entries,
/// - a single replay `.bin` file,
/// - a directory containing replay `.bin` files directly,
/// - a directory containing a `blocks/` subdirectory with replay `.bin` files.
///
/// Shortcomings:
/// - directory discovery is intentionally shallow and does not recurse below the selected
///   directory,
/// - ad-hoc directory input falls back to lexicographic filename order, so it relies on stable
///   sortable corpus names,
/// - single-file input checks only the `.bin` extension; decoding still validates the file content,
/// - manifest input does not check that listed files exist until the replay reads them.
pub(crate) fn plan_from_path(path: PathBuf) -> Result<Plan> {
    if path.join("manifest.json").is_file() {
        let manifest_path = path.join("manifest.json");
        let manifest = corpus::read_manifest(&manifest_path)?;
        corpus::validate_manifest(&manifest)?;
        let files = manifest
            .blocks
            .iter()
            .map(|artifact| path.join(&artifact.file_name))
            .collect::<Vec<_>>();
        return Ok(Plan { label: path, files });
    }

    if path.is_file() {
        if !has_block_extension(&path) {
            return Err(Error::UnsupportedInputFile { path });
        }
        return Ok(Plan { label: path.clone(), files: vec![path] });
    }

    let blocks_dir = if path.join("blocks").is_dir() { path.join("blocks") } else { path.clone() };
    let files = collect_block_files(blocks_dir)?;
    if files.is_empty() {
        return Err(Error::MissingInput { path });
    }

    Ok(Plan { label: path, files })
}

fn collect_block_files(root: PathBuf) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(&root)
        .map_err(|source| Error::ListBlockFiles { path: root.clone(), source })?
    {
        let path = entry
            .map_err(|source| Error::ReadBlockFileEntry { path: root.clone(), source })?
            .path();
        if has_block_extension(&path) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn has_block_extension(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "bin")
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plan")
            .field("label", &self.label)
            .field("files", &self.files.len())
            .finish()
    }
}
