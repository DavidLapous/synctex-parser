use crate as synctex;
use std::{
    fs, io,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

pub const MAX_WARNING_BYTES: usize = 4096;

pub(crate) fn is_compiler_sidecar_input(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext,
                "aux"
                    | "nav"
                    | "out"
                    | "toc"
                    | "w18"
                    | "vrb"
                    | "snm"
                    | "bbl"
                    | "blg"
                    | "log"
                    | "fls"
                    | "fdb_latexmk"
            )
        })
}

pub(crate) fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute() && path.to_str().is_some() && !path.as_os_str().as_bytes().contains(&0)
}

pub(crate) fn valid_warning(warning: &str) -> bool {
    !warning.is_empty()
        && warning.len() <= MAX_WARNING_BYTES
        && !warning.chars().any(char::is_control)
}

pub(crate) fn normalize_absolute(path: &Path, cwd: &Path) -> io::Result<PathBuf> {
    let absolute: PathBuf = if path.is_absolute() {
        path.components().collect()
    } else {
        cwd.components().chain(path.components()).collect()
    };
    if !valid_absolute_path(&absolute) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source path must be a valid absolute UTF-8 path",
        ));
    }
    Ok(absolute)
}

pub(crate) fn canonical_sidecar_dir(sidecar: &Path) -> Option<PathBuf> {
    sidecar
        .parent()
        .and_then(|dir| fs::canonicalize(dir).ok())
        .or_else(|| sidecar.parent().map(Path::to_owned))
}

pub(crate) fn sidecar_dir_for(pdf: &Path, build_directory: Option<&Path>) -> Option<PathBuf> {
    synctex::locate_synctex_resolved(pdf, build_directory)
        .and_then(|sidecar| canonical_sidecar_dir(&sidecar))
}

pub(crate) fn resolve_relative_input(
    raw_path: &Path,
    cwd: Option<&Path>,
    pdf_dir: Option<&Path>,
    sidecar_dir: Option<&Path>,
) -> Option<PathBuf> {
    if raw_path.is_absolute() {
        return None;
    }
    for base in [cwd, pdf_dir, sidecar_dir].into_iter().flatten() {
        if let Ok(cand) = normalize_absolute(raw_path, base)
            && let Ok(resolved) = fs::canonicalize(cand)
        {
            return Some(resolved);
        }
    }
    None
}

pub(crate) fn open_regular(path: &Path) -> io::Result<fs::File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source metadata requires a regular file",
        ));
    }
    Ok(file)
}
