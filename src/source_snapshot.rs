use crate::{self as synctex, DocumentRevision, Operation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, Read},
    os::unix::{ffi::OsStrExt, fs::MetadataExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_SOURCE_MAP_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_WARNING_BYTES: usize = 4096;

/// Compiler input identities, read without invoking the SyncTeX executable or
/// interpreting geometry. Relative inputs belong to the actual build cwd.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInfo {
    pub pdf: PathBuf,
    pub revision: DocumentRevision,
    pub cwd: PathBuf,
    pub main: Option<PathBuf>,
    pub inputs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<PathBuf, PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl SourceInfo {
    pub fn read(pdf: &Path) -> io::Result<Self> {
        Self::read_with_operation(pdf, &Operation::default())
    }

    pub fn read_in(pdf: &Path, build_directory: Option<&Path>) -> io::Result<Self> {
        Self::read_with_operation_in(pdf, &Operation::default(), build_directory)
    }

    pub fn read_with_operation(pdf: &Path, operation: &Operation) -> io::Result<Self> {
        Self::read_with_operation_in(pdf, operation, None)
    }

    pub fn read_with_operation_in(
        pdf: &Path,
        operation: &Operation,
        build_directory: Option<&Path>,
    ) -> io::Result<Self> {
        operation.check()?;
        let pdf = fs::canonicalize(pdf)?;
        let file = open_regular(&pdf, false)?;
        drop(file);
        let revision = DocumentRevision::read_in(&pdf, build_directory)?;
        let cwd = fs::canonicalize(std::env::current_dir()?)?;
        let mut opened = None;
        for (candidate, _) in synctex::synctex_candidates(&pdf, build_directory) {
            match open_regular(&candidate, false) {
                Ok(file) => {
                    opened = Some((file, candidate));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let Some((file, sidecar_path)) = opened else {
            revision.check_in(&pdf, build_directory)?;
            operation.check()?;
            return Ok(Self {
                pdf,
                revision,
                cwd,
                main: None,
                inputs: Vec::new(),
                aliases: BTreeMap::new(),
                warning: Some("compiled source list unavailable: no SyncTeX sidecar".into()),
            });
        };
        let pdf_dir = pdf
            .parent()
            .and_then(|dir| fs::canonicalize(dir).ok())
            .or_else(|| pdf.parent().map(Path::to_owned));
        let sidecar_dir = sidecar_path
            .parent()
            .and_then(|dir| fs::canonicalize(dir).ok())
            .or_else(|| sidecar_path.parent().map(Path::to_owned));
        let mut fallback_dirs = Vec::new();
        if let Some(ref pdir) = pdf_dir
            && pdir.as_path() != cwd.as_path()
        {
            fallback_dirs.push(pdir.as_path());
        }
        if let Some(ref sdir) = sidecar_dir
            && sdir.as_path() != cwd.as_path()
            && pdf_dir.as_deref() != Some(sdir.as_path())
        {
            fallback_dirs.push(sdir.as_path());
        }
        let (main, inputs, aliases, unavailable) = read_inputs_with_fallbacks(
            synctex::decode_synctex_file(file, MAX_SOURCE_MAP_BYTES + 1)?,
            &cwd,
            &fallback_dirs,
            operation,
        )?;
        operation.check()?;
        revision.check_in(&pdf, build_directory)?;
        let warning = if inputs.is_empty() {
            Some("compiled source list unavailable: SyncTeX has no inputs".into())
        } else if unavailable > 0 {
            Some(format!(
                "compiled source list contains {unavailable} unavailable input paths; absolute compiler identities retained"
            ))
        } else {
            None
        };
        Ok(Self {
            pdf,
            revision,
            cwd,
            main,
            inputs,
            aliases,
            warning,
        })
    }
}

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
                    | "synctex"
            )
        })
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".synctex.gz"))
}

#[cfg(test)]
fn read_inputs(
    reader: impl BufRead,
    cwd: &Path,
    operation: &Operation,
) -> io::Result<(Option<PathBuf>, Vec<PathBuf>, usize)> {
    let (main, paths, _, unavailable) = read_inputs_with_fallbacks(reader, cwd, &[], operation)?;
    Ok((main, paths, unavailable))
}

#[allow(clippy::type_complexity)]
fn read_inputs_with_fallbacks(
    mut reader: impl BufRead,
    cwd: &Path,
    fallback_dirs: &[&Path],
    operation: &Operation,
) -> io::Result<(
    Option<PathBuf>,
    Vec<PathBuf>,
    BTreeMap<PathBuf, PathBuf>,
    usize,
)> {
    let mut inputs = BTreeMap::new();
    let mut raw_aliases = BTreeMap::new();
    let mut first = None;
    let mut row = String::new();
    let mut bytes = 0;
    let mut lines = 0usize;
    let mut postamble = false;
    let mut count_seen = false;
    let mut unavailable = 0;
    loop {
        if lines & 1023 == 0 {
            operation.check()?;
        }
        lines = lines.wrapping_add(1);
        row.clear();
        let count = reader.read_line(&mut row)?;
        bytes += count;
        if bytes as u64 > MAX_SOURCE_MAP_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SyncTeX source list exceeds 128 MiB",
            ));
        }
        if count == 0 {
            break;
        }
        let row = row.trim_end_matches(['\r', '\n']);
        if let Some(input) = row.strip_prefix("Input:") {
            let (tag, path) = input.split_once(':').ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed SyncTeX input declaration",
                )
            })?;
            let tag = tag
                .parse::<u32>()
                .ok()
                .filter(|tag| *tag > 0)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid SyncTeX input tag")
                })?;
            if path.is_empty() || path.as_bytes().contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid SyncTeX input path",
                ));
            }
            let raw_path = Path::new(path);
            let absolute = normalize_absolute(raw_path, cwd)?;
            let ignored = tag != 1
                && first.is_some()
                && (is_compiler_sidecar_input(&absolute)
                    || !absolute.starts_with(cwd)
                        && absolute.components().any(|component| {
                            component.as_os_str().as_bytes().starts_with(b"texmf")
                        }));
            let path = if ignored {
                absolute
            } else {
                match fs::canonicalize(&absolute) {
                    Ok(path) => {
                        if path != absolute {
                            raw_aliases.insert(absolute, path.clone());
                        }
                        path
                    }
                    Err(_) => {
                        let fallback = (!raw_path.is_absolute())
                            .then(|| {
                                fallback_dirs.iter().find_map(|dir| {
                                    let cand = normalize_absolute(raw_path, dir).ok()?;
                                    let resolved = fs::canonicalize(&cand).ok()?;
                                    Some((cand, resolved))
                                })
                            })
                            .flatten();
                        if let Some((cand, found)) = fallback {
                            if cand != found {
                                raw_aliases.insert(cand, found.clone());
                            }
                            if absolute != found {
                                raw_aliases.insert(absolute, found.clone());
                            }
                            found
                        } else {
                            unavailable += 1;
                            absolute
                        }
                    }
                }
            };
            match inputs.entry(tag) {
                std::collections::btree_map::Entry::Occupied(previous)
                    if previous.get() != &path =>
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "conflicting SyncTeX input tag",
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(path);
                }
            }
            first.get_or_insert(tag);
        } else if row == "Postamble:" {
            postamble = true;
        } else if postamble && let Some(count) = row.strip_prefix("Count:") {
            count.parse::<u32>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid SyncTeX postamble Count",
                )
            })?;
            count_seen = true;
        }
    }
    if !count_seen {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete SyncTeX source list",
        ));
    }
    let main = inputs
        .get(&1)
        .or_else(|| first.and_then(|tag| inputs.get(&tag)))
        .cloned();
    let mut paths: Vec<_> = inputs
        .into_values()
        .filter(|path| {
            Some(path) == main.as_ref()
                || !is_compiler_sidecar_input(path)
                    && (path.starts_with(cwd)
                        || fallback_dirs.iter().any(|dir| path.starts_with(dir))
                        || !path.components().any(|component| {
                            component.as_os_str().as_bytes().starts_with(b"texmf")
                        }))
        })
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let aliases = raw_aliases
        .into_iter()
        .filter(|(alias, target)| {
            alias != target && (paths.contains(target) || Some(target) == main.as_ref())
        })
        .collect();
    Ok((main, paths, aliases, unavailable))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    pdf: PathBuf,
    revision: DocumentRevision,
    cwd: PathBuf,
    main: PathBuf,
    #[serde(default)]
    aliases: BTreeMap<PathBuf, PathBuf>,
    files: BTreeMap<PathBuf, String>,
    digests: BTreeMap<PathBuf, String>,
    warnings: BTreeMap<PathBuf, String>,
}

/// An immutable, revision-bound copy of the compiler's original source inputs.
/// Original identities remain valid after the live file is renamed or deleted.
#[derive(Debug)]
pub struct SourceSnapshot {
    pub path: PathBuf,
    pub pdf: PathBuf,
    pub revision: DocumentRevision,
    pub cwd: PathBuf,
    pub main: PathBuf,
    fallback_dirs: Vec<PathBuf>,
    directory: PathBuf,
    aliases: BTreeMap<PathBuf, PathBuf>,
    files: BTreeMap<PathBuf, String>,
    digests: BTreeMap<PathBuf, [u8; 32]>,
    warnings: BTreeMap<PathBuf, String>,
}

#[derive(Debug, thiserror::Error)]
#[error("required compiled source cache: {0}")]
pub(crate) struct SnapshotReadError(#[source] pub(crate) io::Error);

pub(crate) fn required_cache_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|error| error.is::<SnapshotReadError>())
}

impl SourceSnapshot {
    pub fn load(path: &Path, pdf: &Path) -> io::Result<Self> {
        Self::load_in(path, pdf, None)
    }

    pub fn load_in(path: &Path, pdf: &Path, build_directory: Option<&Path>) -> io::Result<Self> {
        if !valid_absolute_path(path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source snapshot path must be absolute",
            ));
        }
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "source snapshot has no cache directory",
            )
        })?;
        let directory = fs::canonicalize(parent)?;
        let metadata = fs::metadata(&directory)?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "source snapshot requires a private, current-user cache directory",
            ));
        }
        let mut bytes = Vec::new();
        open_regular(path, true)?
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source snapshot manifest exceeds 4 MiB",
            ));
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        let pdf = fs::canonicalize(pdf)?;
        if !valid_absolute_path(&manifest.pdf) || manifest.pdf != pdf {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source snapshot belongs to a different PDF",
            ));
        }
        for original in [&manifest.cwd, &manifest.main]
            .into_iter()
            .chain(manifest.files.keys())
            .chain(manifest.warnings.keys())
        {
            if !valid_absolute_path(original) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source snapshot contains an invalid original path",
                ));
            }
        }
        for filename in manifest.files.values() {
            if filename.is_empty()
                || filename == "."
                || filename == ".."
                || filename.contains(['/', '\\', '\0'])
                || Path::new(filename).components().count() != 1
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source snapshot cache filenames must be single relative components",
                ));
            }
        }
        if manifest.files.keys().ne(manifest.digests.keys()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source snapshot digests must cover exactly the captured inputs",
            ));
        }
        let digests = manifest
            .digests
            .into_iter()
            .map(|(original, digest)| decode_digest(&digest).map(|digest| (original, digest)))
            .collect::<io::Result<_>>()?;
        for warning in manifest.warnings.values() {
            if !valid_warning(warning) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid source snapshot provenance warning",
                ));
            }
        }
        for (alias, target) in &manifest.aliases {
            if !valid_absolute_path(alias)
                || !valid_absolute_path(target)
                || alias == target
                || !(manifest.files.contains_key(target)
                    || manifest.warnings.contains_key(target)
                    || target == &manifest.main)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source snapshot contains an invalid input alias",
                ));
            }
        }
        manifest.revision.check_in(&pdf, build_directory)?;
        let sidecar_dir = sidecar_dir_for(&pdf, build_directory);
        let mut fallback_dirs = Vec::new();
        for dir in [
            pdf.parent().map(Path::to_owned),
            sidecar_dir,
            manifest.main.parent().map(Path::to_owned),
        ]
        .into_iter()
        .flatten()
        {
            if dir != manifest.cwd && !fallback_dirs.contains(&dir) {
                fallback_dirs.push(dir);
            }
        }
        let filename = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "source snapshot has no manifest filename",
            )
        })?;
        Ok(Self {
            path: directory.join(filename),
            pdf,
            revision: manifest.revision,
            cwd: manifest.cwd,
            main: manifest.main,
            fallback_dirs,
            directory,
            aliases: manifest.aliases,
            files: manifest.files,
            digests,
            warnings: manifest.warnings,
        })
    }

    fn is_recorded(&self, path: &Path) -> bool {
        self.files.contains_key(path) || self.warnings.contains_key(path) || path == self.main
    }

    pub fn normalize(&self, original: &Path) -> io::Result<PathBuf> {
        let absolute = normalize_absolute(original, &self.cwd)?;
        // Do not consult the live filesystem before checking old compiler
        // identities: a deleted file or changed symlink is still this input.
        if self.is_recorded(&absolute) {
            return Ok(absolute);
        }
        if let Some(target) = self.aliases.get(&absolute) {
            return Ok(target.clone());
        }
        if !original.is_absolute() {
            for base in &self.fallback_dirs {
                if let Ok(cand) = normalize_absolute(original, base) {
                    if self.is_recorded(&cand) {
                        return Ok(cand);
                    }
                    if let Some(target) = self.aliases.get(&cand) {
                        return Ok(target.clone());
                    }
                    if let Ok(canon) = fs::canonicalize(&cand)
                        && self.is_recorded(&canon)
                    {
                        return Ok(canon);
                    }
                }
            }
        }
        if let Ok(canon) = fs::canonicalize(&absolute) {
            return Ok(canon);
        }
        if !original.is_absolute() {
            for base in &self.fallback_dirs {
                if let Ok(cand) = normalize_absolute(original, base)
                    && let Ok(canon) = fs::canonicalize(&cand)
                {
                    return Ok(canon);
                }
            }
        }
        Ok(absolute)
    }

    pub fn read(&self, original: &Path) -> io::Result<String> {
        let (original, filename) = match self.files.get_key_value(original) {
            Some(entry) => entry,
            None => self
                .files
                .get_key_value(&self.normalize(original)?)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "compiled source input is not captured in snapshot",
                    )
                })?,
        };
        let expected_digest = &self.digests[original];
        // O_NOFOLLOW prevents a cache entry from being replaced by a link to
        // edited live text. Never fall back to the original on-disk source.
        let result = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(self.directory.join(filename))
            .and_then(synctex::read_source_file)
            .and_then(|source| {
                let digest: [u8; 32] = Sha256::digest(source.as_bytes()).into();
                if &digest != expected_digest {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "cached source changed after publication",
                    ));
                }
                Ok(source)
            });
        result.map_err(|error| io::Error::new(error.kind(), SnapshotReadError(error)))
    }

    pub fn warning(&self, original: &Path) -> Option<&str> {
        if let Some(warning) = self.warnings.get(original) {
            return Some(warning);
        }
        if self.files.contains_key(original) {
            return None;
        }
        let original = self.normalize(original).ok()?;
        self.warnings
            .get(&original)
            .map(String::as_str)
            .or_else(|| {
                (!self.files.contains_key(&original))
                    .then_some("compiled source input is not captured in snapshot")
            })
    }
}

fn decode_digest(value: &str) -> io::Result<[u8; 32]> {
    let bytes = value.as_bytes();
    if bytes.len() != 64
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source snapshot digest must be lowercase SHA-256 hex",
        ));
    }
    let nibble = |byte: u8| {
        if byte.is_ascii_digit() {
            byte - b'0'
        } else {
            byte - b'a' + 10
        }
    };
    Ok(std::array::from_fn(|index| {
        nibble(bytes[index * 2]) << 4 | nibble(bytes[index * 2 + 1])
    }))
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
    // Components remove redundant separators and `.` without collapsing
    // symlink-sensitive `..` components against an unavailable original file.
    Ok(absolute)
}

pub(crate) fn sidecar_dir_for(pdf: &Path, build_directory: Option<&Path>) -> Option<PathBuf> {
    synctex::locate_synctex_in(pdf, build_directory).and_then(|sidecar| {
        sidecar
            .parent()
            .and_then(|dir| fs::canonicalize(dir).ok())
            .or_else(|| sidecar.parent().map(Path::to_owned))
    })
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

pub(crate) fn open_regular(path: &Path, no_follow: bool) -> io::Result<fs::File> {
    let flags = libc::O_NONBLOCK | if no_follow { libc::O_NOFOLLOW } else { 0 };
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source metadata requires a regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn snapshot(directory: &Path, pdf: &Path, source: &Path) -> PathBuf {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(directory.join("1.txt"), "old compiled text\n").unwrap();
        let mut digest = String::with_capacity(64);
        for byte in Sha256::digest(b"old compiled text\n").iter() {
            write!(digest, "{byte:02x}").unwrap();
        }
        let path = directory.join("manifest.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "pdf": pdf,
                "revision": DocumentRevision::read(pdf).unwrap(),
                "cwd": source.parent().unwrap(),
                "main": source,
                "files": BTreeMap::from([(source.to_owned(), "1.txt")]),
                "digests": BTreeMap::from([(source.to_owned(), digest)]),
                "warnings": {},
            }))
            .unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn old_identity_reads_only_revision_bound_cached_text() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let pdf = root.join("document.pdf");
        let source = root.join("original.tex");
        fs::write(&pdf, b"pdf revision").unwrap();
        fs::write(&source, "edited text").unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let path = snapshot(&cache, &pdf, &source);
        let snapshot = SourceSnapshot::load(&path, &pdf).unwrap();
        fs::rename(&source, root.join("renamed.tex")).unwrap();
        assert_eq!(
            snapshot.normalize(Path::new("./original.tex")).unwrap(),
            source
        );
        assert_eq!(snapshot.read(&source).unwrap(), "old compiled text\n");
        fs::remove_file(cache.join("1.txt")).unwrap();
        symlink(root.join("renamed.tex"), cache.join("1.txt")).unwrap();
        assert!(snapshot.read(&source).is_err());
        fs::write(&pdf, "new revision").unwrap();
        assert!(SourceSnapshot::load(&path, &pdf).is_err());
    }

    #[test]
    fn cached_utf8_changes_are_required_cache_errors_on_cold_and_warm_reads() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let pdf = root.join("document.pdf");
        let source = root.join("original.tex");
        fs::write(&pdf, b"pdf revision").unwrap();
        fs::write(&source, "edited live text\n").unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let path = snapshot(&cache, &pdf, &source);
        let loaded = SourceSnapshot::load(&path, &pdf).unwrap();
        assert_eq!(loaded.read(&source).unwrap(), "old compiled text\n");
        fs::write(cache.join("1.txt"), "new compiled text\n").unwrap();
        for snapshot in [&loaded, &SourceSnapshot::load(&path, &pdf).unwrap()] {
            let error = snapshot.read(&source).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(required_cache_error(&error));
            assert!(
                error
                    .to_string()
                    .contains("cached source changed after publication")
            );
        }
        fs::write(cache.join("1.txt"), "old compiled text\n").unwrap();
        assert_eq!(loaded.read(&source).unwrap(), "old compiled text\n");
    }

    #[test]
    fn manifest_requires_exact_digest_coverage_and_lowercase_sha256() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let pdf = root.join("document.pdf");
        let source = root.join("original.tex");
        fs::write(&pdf, b"pdf revision").unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let path = snapshot(&cache, &pdf, &source);
        let baseline: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let reject = |manifest: serde_json::Value| {
            fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert!(SourceSnapshot::load(&path, &pdf).is_err());
        };
        let mut missing = baseline.clone();
        missing.as_object_mut().unwrap().remove("digests");
        reject(missing);
        for digests in [
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let mut invalid = baseline.clone();
            invalid["digests"] = digests;
            reject(invalid);
        }
        let mut extra = baseline.clone();
        extra["digests"][root.join("uncaptured.tex").to_str().unwrap()] =
            serde_json::Value::String("0".repeat(64));
        reject(extra);
        for digest in [
            serde_json::json!(0),
            serde_json::json!("0".repeat(63)),
            serde_json::json!("0".repeat(65)),
            serde_json::json!("A".repeat(64)),
            serde_json::json!("g".repeat(64)),
        ] {
            let mut invalid = baseline.clone();
            invalid["digests"][source.to_str().unwrap()] = digest;
            reject(invalid);
        }
        let mut valid = baseline;
        valid["warnings"][root.join("uncaptured.tex").to_str().unwrap()] =
            serde_json::Value::String("compiled input was unavailable".into());
        fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
        assert_eq!(
            SourceSnapshot::load(&path, &pdf)
                .unwrap()
                .read(&source)
                .unwrap(),
            "old compiled text\n"
        );
    }

    #[test]
    fn source_info_retains_late_relative_inputs_without_decoding_geometry() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = fs::canonicalize(directory.path()).unwrap();
        fs::write(cwd.join("main.tex"), "compiled").unwrap();
        fs::write(cwd.join("main.aux"), "sidecar").unwrap();
        fs::write(cwd.join("main.toc"), "sidecar").unwrap();
        fs::write(cwd.join("main.nav"), "sidecar").unwrap();
        fs::write(cwd.join("main.out"), "sidecar").unwrap();
        let text = "Input:1:./main.tex\nInput:2:/usr/local/texlive/2026/texmf-dist/tex/latex/base/article.cls\nInput:3:./main.aux\nInput:4:./main.toc\nInput:5:./main.nav\nInput:6:./main.out\nInput:7:./main.w18\nContent:\n{1\n(not geometry\n}1\nPostamble:\nCount:2\nInput:8:missing.tex\n";
        let (main, inputs, missing) =
            read_inputs(text.as_bytes(), &cwd, &Operation::default()).unwrap();
        assert_eq!(main, Some(cwd.join("main.tex")));
        assert_eq!(inputs, [cwd.join("main.tex"), cwd.join("missing.tex")]);
        assert_eq!(missing, 1);
        for invalid in [
            "Input:0:main.tex\nPostamble:\nCount:1\n",
            "Input:1:a\nInput:1:b\nPostamble:\nCount:1\n",
            "Input:1:a\n",
        ] {
            assert!(read_inputs(invalid.as_bytes(), &cwd, &Operation::default()).is_err());
        }
    }

    #[test]
    fn manifest_rejects_path_escape_and_nonprivate_cache() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let pdf = root.join("document.pdf");
        let source = root.join("original.tex");
        fs::write(&pdf, b"pdf revision").unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let path = snapshot(&cache, &pdf, &source);
        let original = fs::read_to_string(&path).unwrap();
        fs::write(&path, original.replace("1.txt", "../original.tex")).unwrap();
        assert!(SourceSnapshot::load(&path, &pdf).is_err());
        fs::write(&path, original).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(SourceSnapshot::load(&path, &pdf).is_err());
    }
}
