#![doc = include_str!("../README.md")]

mod math;
mod parser;
mod symbols;
#[cfg(test)]
mod tests;
mod tex;

use parser::*;
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use std::{
    fs,
    io::{self, BufRead, BufReader, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
use tex::*;
pub use tex::{is_spacing_diacritic, word_index_at, words};
use unicode_normalization::char::is_combining_mark;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchRect {
    pub bottom: f32,
    pub left: f32,
    pub top: f32,
    pub right: f32,
}

pub const MAX_WARNING_BYTES: usize = 4096;

pub const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Operation {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
impl Operation {
    pub fn new(timeout: Duration) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + timeout,
        }
    }
    /// Keep the parent's cancellation while applying a shorter helper deadline.
    pub fn limited(&self, timeout: Duration) -> Self {
        Self {
            cancelled: Arc::clone(&self.cancelled),
            deadline: self.deadline.min(Instant::now() + timeout),
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub fn remaining(&self) -> io::Result<Duration> {
        self.check()?;
        Ok(self.deadline.saturating_duration_since(Instant::now()))
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "navigation cancelled",
            ))
        } else if Instant::now() >= self.deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "navigation timed out",
            ))
        } else {
            Ok(())
        }
    }
}
impl Default for Operation {
    fn default() -> Self {
        Self::new(NAVIGATION_TIMEOUT)
    }
}

/// Source coordinates: one-based line, zero-based UTF-8 byte offset;
/// column is one-based UTF-16 (VS Code), column_char is one-based Unicode scalar.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub byte_column: usize,
    pub column: usize,
    pub column_char: usize,
    /// Accepted native-compatible column hint, not proof of printed-occurrence ownership.
    pub precise: bool,
}

/// Identity of a local PDF revision, including atomic replacement and in-place writes.
/// Not a content digest: the protocol assumes a non-adversarial local filesystem.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PdfRevision {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl PdfRevision {
    pub fn read(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    pub fn check(self, path: &Path) -> io::Result<()> {
        if Self::read(path)? != self {
            return Err(io::Error::other(
                "PDF revision changed; repeat forward search",
            ));
        }
        Ok(())
    }
}

static BUILD_DIRECTORY: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Process-wide default build directory; resolved once at operation entry.
pub fn set_build_directory(path: Option<PathBuf>) {
    *BUILD_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) = path;
}

/// An explicit build directory, else the process-wide default, else `SYNCTEX_BUILD_DIRECTORY`;
/// resolved once at operation entry.
pub(crate) fn resolve_build_directory(build_directory: Option<&Path>) -> Option<PathBuf> {
    build_directory.map(Path::to_path_buf).or_else(|| {
        BUILD_DIRECTORY
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .or_else(|| {
                std::env::var_os("SYNCTEX_BUILD_DIRECTORY")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            })
    })
}

pub(crate) fn synctex_candidates(pdf: &Path, build_directory: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_pair = |base: &Path| {
        let mut push = |candidate: PathBuf| {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        };
        for extension in ["synctex.gz", "synctex"] {
            push(base.with_extension(extension));
        }
        if base.extension().is_some() {
            for suffix in [".synctex.gz", ".synctex"] {
                let mut os = base.as_os_str().to_os_string();
                os.push(suffix);
                push(PathBuf::from(os));
            }
        }
    };
    push_pair(pdf);
    if let Some(dir) = build_directory
        && let Some(name) = pdf.file_name()
    {
        if dir.is_relative()
            && let Some(parent) = pdf.parent().filter(|p| !p.as_os_str().is_empty())
        {
            push_pair(&parent.join(dir).join(name));
        }
        push_pair(&dir.join(name));
    }
    candidates
}

/// Metadata identity of both files observed when PDFium opens a document.
/// Stability does not prove a common build; producers must publish a completed
/// PDF/SyncTeX pair before requesting navigation.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentRevision {
    pub pdf: PdfRevision,
    companion: Option<PdfRevision>,
}
pub(crate) fn companion_revision_and_path_in(
    path: &Path,
    build_directory: Option<&Path>,
) -> io::Result<Option<(PdfRevision, PathBuf)>> {
    for candidate in synctex_candidates(path, build_directory) {
        match PdfRevision::read(&candidate) {
            Ok(revision) => return Ok(Some((revision, candidate))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

pub(crate) fn companion_revision_in(
    path: &Path,
    build_directory: Option<&Path>,
) -> io::Result<Option<PdfRevision>> {
    Ok(companion_revision_and_path_in(path, build_directory)?.map(|(companion, _)| companion))
}

impl DocumentRevision {
    pub fn read(path: &Path) -> io::Result<Self> {
        let build_directory = resolve_build_directory(None);
        Self::read_resolved(path, build_directory.as_deref())
    }
    pub(crate) fn read_resolved(path: &Path, build_directory: Option<&Path>) -> io::Result<Self> {
        let pdf = PdfRevision::read(path)?;
        let companion = companion_revision_in(path, build_directory)?;
        Ok(Self { pdf, companion })
    }
    pub fn check(self, path: &Path) -> io::Result<()> {
        let build_directory = resolve_build_directory(None);
        self.check_resolved(path, build_directory.as_deref())
    }
    pub(crate) fn check_resolved(
        self,
        path: &Path,
        build_directory: Option<&Path>,
    ) -> io::Result<()> {
        if Self::read_resolved(path, build_directory)? != self {
            return Err(io::Error::other(
                "displayed PDF/SyncTeX revision changed; wait for reload and click again",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct InverseResolution {
    pub location: SourceLocation,
    pub warning: Option<String>,
}

/// Literal source word and nearby prose tokens; not TeX macro expansion.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForwardWord {
    pub words: Vec<String>,
    pub selected: usize,
}

impl ForwardWord {
    fn at(text: &str, column: u32) -> Option<Self> {
        let text = source_line_text(text);
        let byte = text.char_indices().nth(column.checked_sub(1)? as usize)?.0;
        let hidden = nonprinting_source_ranges(text);
        let words: Vec<_> = words(text)
            .into_iter()
            .filter(|(start, _)| {
                !tex::has_unescaped_backslash_before(text, *start)
                    && !hidden.iter().any(|range| range.contains(start))
            })
            .collect();
        Self::from_tokens(&words, byte)
    }

    fn from_tokens(words: &[(usize, &str)], byte: usize) -> Option<Self> {
        let selected = words
            .iter()
            .position(|(start, word)| *start <= byte && byte < start + word.len())?;
        let start = selected.saturating_sub(3);
        let end = (selected + 4).min(words.len());
        if words[start..end].iter().any(|(_, word)| word.len() > 128) {
            return None;
        }
        Some(Self {
            words: words[start..end]
                .iter()
                .map(|(_, word)| (*word).to_owned())
                .collect(),
            selected: selected - start,
        })
    }

    fn valid(&self) -> bool {
        self.words.len() <= 7
            && self.selected < self.words.len()
            && self.words.iter().all(|word| {
                word.len() <= 128
                    && word.chars().next().is_some_and(char::is_alphanumeric)
                    && word.chars().all(|ch| {
                        ch.is_alphanumeric() || is_combining_mark(ch) || is_spacing_diacritic(ch)
                    })
            })
    }
}

/// Compact raw or qualified forward geometry in original compiler coordinates.
#[derive(Clone, Copy, Debug)]
pub struct ForwardCandidate {
    pub page: u32,
    /// Compiler input line matched by the geometry lookup.
    pub source_line: u32,
    pub rect: SearchRect,
}

// Editor adapters can supply a literal saved line and UTF-8 cursor offset.
// Tokenize here so every adapter shares PDF matching's Unicode word rules.
fn deserialize_forward_word<'de, D>(deserializer: D) -> Result<Option<ForwardWord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Literal {
        text: String,
        byte_column: usize,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Context(ForwardWord),
        Literal(Literal),
    }
    match Option::<Input>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Input::Context(word)) => Ok(Some(word)),
        Some(Input::Literal(Literal { text, byte_column })) => {
            if !text.is_char_boundary(byte_column) {
                return Err(serde::de::Error::custom("invalid source word byte column"));
            }
            Ok(ForwardWord::from_tokens(&words(&text), byte_column))
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForwardRequest {
    pub pdf: std::path::PathBuf,
    pub revision: PdfRevision,
    pub page: u32,
    pub h: f32,
    pub v: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default, deserialize_with = "deserialize_forward_word")]
    pub word: Option<ForwardWord>,
    /// Revision-bound compiler source-map service supplied by an editor adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse_search: Option<String>,
    /// Explicit opt-in to warned inverse anchors; a refinement warning alone is not opt-in.
    #[serde(default)]
    pub best_guess: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl ForwardRequest {
    pub fn rect(&self) -> SearchRect {
        SearchRect {
            left: self.h,
            top: self.v - self.height,
            right: self.h + self.width,
            bottom: self.v,
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if self
            .warning
            .as_ref()
            .is_some_and(|warning| !valid_warning(warning))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid forward navigation warning",
            ));
        }
        if self
            .inverse_search
            .as_ref()
            .is_some_and(|path| !Path::new(path).is_absolute() || path.as_bytes().contains(&0))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "inverse source-map endpoint must be an absolute Unix socket path",
            ));
        }
        if self.word.as_ref().is_some_and(|word| !word.valid()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid forward word context",
            ));
        }
        if !valid_absolute_path(&self.pdf)
            || !forward_geometry_valid(self.page, self.h, self.v, self.width, self.height)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "forward request requires an absolute PDF path, positive page, and finite nonnegative box dimensions",
            ));
        }
        Ok(())
    }
}

pub fn parse_forward_request(payload: &str) -> io::Result<ForwardRequest> {
    let request: ForwardRequest = serde_json::from_str(payload)?;
    request.validate()?;
    Ok(request)
}

/// Explicit editor context; standalone navigation keeps the strict default.
#[derive(Clone, Copy, Debug, Default)]
pub struct ForwardSource<'a> {
    pub best_guess: bool,
    pub anchor_only: bool,
    pub page_hint: Option<u32>,
    pub build_directory: Option<&'a Path>,
}

/// No usable compiler geometry exists for this otherwise valid request.
/// Explicit editor best-guess commands distinguish this from operational errors.
#[derive(Debug)]
pub(crate) struct ForwardUnavailable(&'static str);

impl std::fmt::Display for ForwardUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ForwardUnavailable {}

pub fn is_forward_unavailable(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|error| error.is::<ForwardUnavailable>())
}

pub fn resolve_forward_with_selector(
    pdf: &Path,
    file: &Path,
    line: u32,
    column: u32,
    context: ForwardSource<'_>,
    select_line_candidate: impl FnOnce(
        &Path,
        &[ForwardCandidate],
        &ForwardWord,
        ForwardCandidate,
    ) -> Result<ForwardCandidate, String>,
) -> io::Result<ForwardRequest> {
    if line == 0 || column == 0 {
        return Err(io::Error::other("source line and column must be positive"));
    }
    let pdf = fs::canonicalize(pdf)?;
    let resolved_build_directory = resolve_build_directory(context.build_directory);
    let build_directory = resolved_build_directory.as_deref();
    let allow_anchor = context.best_guess || context.anchor_only;
    drop(open_regular(&pdf)?);
    let document_revision = DocumentRevision::read_resolved(&pdf, build_directory)?;
    if document_revision.companion.is_none() {
        return Err(if allow_anchor {
            io::Error::other(ForwardUnavailable(
                "no SyncTeX sidecar is available for source positioning",
            ))
        } else {
            open_synctex_with_path_in(&pdf, build_directory)
                .err()
                .unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "no SyncTeX companion file")
                })
        });
    }
    let file = if context.anchor_only {
        match fs::canonicalize(file) {
            Ok(file) => file,
            Err(_) => normalize_absolute(file, &std::env::current_dir()?)?,
        }
    } else {
        fs::canonicalize(file)?
    };
    let revision = document_revision.pdf;
    let mut warning = context.anchor_only.then(|| {
        "best-guess navigation: using saved source and the compiler line anchor".to_owned()
    });
    let source_revision = (!context.anchor_only)
        .then(|| PdfRevision::read(&file))
        .transpose()?;
    let source = if context.anchor_only {
        None
    } else {
        match read_source(&file) {
            Ok(source) => Some(source),
            Err(error)
                if allow_anchor
                    && matches!(
                        error.kind(),
                        io::ErrorKind::InvalidData | io::ErrorKind::FileTooLarge
                    ) =>
            {
                warning = Some(format!(
                    "best-guess navigation: saved source word hint unavailable: {error}; using the compiler line anchor"
                ));
                None
            }
            Err(error) => return Err(error),
        }
    };
    let original_line = match source
        .as_deref()
        .map(|source| validate_forward_source_position(source, line, column))
    {
        Some(Ok(text)) => Some(text),
        Some(Err(error)) => return Err(error),
        None => None,
    };
    if let Some(source_revision) = source_revision
        && document_revision.companion.is_some_and(|companion| {
            (
                source_revision.modified_seconds,
                source_revision.modified_nanoseconds,
            ) > (companion.modified_seconds, companion.modified_nanoseconds)
        })
    {
        warning = Some(
            "line-only navigation: saved source is newer than SyncTeX; using the compiler line anchor"
                .into(),
        );
    }
    let page_hint = if context.anchor_only || warning.is_some() {
        None
    } else {
        original_line.and_then(|text| ForwardWord::at(text, column))
    };
    // Collected frame/tabularx/text/math bodies use their closing compiler line.
    // Keep the original source position for the page hint, and give Beamer priority.
    let file_index = source
        .as_deref()
        .map(|src| tex::SourceFileIndex::new(src, &Operation::default()));
    let view_line = source
        .as_deref()
        .zip(file_index.as_ref())
        .and_then(|(source, file_index)| {
            collected_closing_line(source, (line, column), file_index, |spans| {
                original_line.and_then(|_| {
                    tex::argument_closing_line_with_spans(source, line, column, spans)
                })
            })
        })
        .unwrap_or(line);
    let candidates = crate::parser::forward_candidates_resolved(
        &pdf,
        &file,
        view_line,
        context.page_hint,
        build_directory,
    )?;
    let line_candidate = |hint: Option<&ForwardWord>| {
        let anchor = best_guess_forward_candidate(&candidates)?;
        match hint {
            Some(hint) => {
                select_line_candidate(&pdf, &candidates, hint, anchor).map_err(io::Error::other)
            }
            None => Ok(anchor),
        }
    };
    let (selected, word) = if context.anchor_only || warning.is_some() {
        warning.get_or_insert_with(|| "line-only navigation: using the SyncTeX line anchor".into());
        (best_guess_forward_candidate(&candidates)?, None)
    } else {
        let selected = line_candidate(page_hint.as_ref())?;
        let word = if selected.source_line == view_line
            && selected.rect.right > selected.rect.left
            && selected.rect.bottom > selected.rect.top
        {
            page_hint.filter(|_| {
                file_index
                    .as_ref()
                    .zip(original_line)
                    .is_some_and(|(index, text)| {
                        let Some((byte, _)) = text.char_indices().nth(column as usize - 1) else {
                            return false;
                        };
                        let Some(&line_start) = index.line_starts.get(line as usize - 1) else {
                            return false;
                        };
                        let absolute = line_start + byte;
                        !contains_byte(&index.strict_hidden, absolute)
                    })
            })
        } else {
            None
        };
        warning = word
            .is_some()
            .then(|| "assuming saved source is unchanged since compilation".into());
        (selected, word)
    };
    let result = ForwardRequest {
        pdf,
        revision,
        page: selected.page,
        h: selected.rect.left,
        v: selected.rect.bottom,
        width: selected.rect.right - selected.rect.left,
        height: selected.rect.bottom - selected.rect.top,
        word,
        inverse_search: None,
        best_guess: context.best_guess,
        warning,
    };
    result.validate()?;
    if result.word.is_some()
        && let Some(source_revision) = source_revision
    {
        source_revision.check(&file)?;
    }
    document_revision.check_resolved(&result.pdf, build_directory)?;
    Ok(result)
}

fn best_guess_forward_candidate(candidates: &[ForwardCandidate]) -> io::Result<ForwardCandidate> {
    // SyncTeX orders results by its nearest source anchor. Preserve that order,
    // skipping zero-height pre-display boxes and empty geometry.
    candidates
        .iter()
        .find(|candidate| {
            candidate.rect.right > candidate.rect.left && candidate.rect.bottom > candidate.rect.top
        })
        .or_else(|| {
            candidates.iter().find(|candidate| {
                candidate.rect.right > candidate.rect.left
                    || candidate.rect.bottom > candidate.rect.top
            })
        })
        .copied()
        .ok_or_else(|| {
            io::Error::other(ForwardUnavailable(
                "synctex view returned no usable compiler anchor geometry",
            ))
        })
}

#[derive(Clone, Copy, Debug)]
pub struct InversePoint {
    pub page: u32,
    pub x: f32,
    pub y_from_top: f32,
    pub page_height_pt: f32,
}

pub fn resolve_inverse(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    best_guess: bool,
) -> io::Result<InverseResolution> {
    resolve_inverse_with_cache(pdf, point, word, radius, operation, best_guess, None)
}

/// `(page, generated, working directory, strict input resolution)`: both
/// resolution modes consult the working directory for relative inputs.
type StructuralMapKey = (u32, PathBuf, Option<PathBuf>, bool);

fn structural_map_key(
    page: u32,
    generated: &Path,
    current_dir: Option<&Path>,
    best_guess: bool,
) -> StructuralMapKey {
    (
        page,
        generated.to_owned(),
        current_dir.map(Path::to_owned),
        best_guess,
    )
}

#[derive(Default)]
pub struct SyncTeXCache {
    entry: Option<CachedSyncTeX>,
}

type StructuralMaps = std::collections::HashMap<StructuralMapKey, Result<Rc<SourceMap>, String>>;

struct CachedSyncTeX {
    pdf: PathBuf,
    companion: PdfRevision,
    sidecar_dir: Option<PathBuf>,
    /// The decoded sidecar, so a newly clicked page parses without decompressing again.
    raw: Vec<u8>,
    /// Single-sheet parses, each carrying the source provenance of its sheet.
    pages: std::collections::HashMap<u32, parser::ParsedSynctex>,
    structural_maps: StructuralMaps,
}

impl SyncTeXCache {
    fn candidates_and_entry(
        &mut self,
        pdf: &Path,
        point: InversePoint,
        operation: &Operation,
        build_directory: Option<&Path>,
    ) -> io::Result<(Vec<RawInverseCandidate>, PdfRevision, &mut CachedSyncTeX)> {
        operation.check()?;
        parser::validate_inverse_point(point)?;
        let Some((companion, sidecar_path)) = companion_revision_and_path_in(pdf, build_directory)?
        else {
            return Err(open_synctex_with_path_in(pdf, build_directory)
                .err()
                .unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "no SyncTeX companion file")
                }));
        };
        let sidecar_dir = canonical_sidecar_dir(&sidecar_path);
        if self
            .entry
            .as_ref()
            .is_none_or(|entry| entry.pdf != pdf || entry.companion != companion)
        {
            let mut reader = decode_synctex_file(
                open_regular(&sidecar_path)?,
                parser::MAX_SYNCTEX_BYTES as u64 + 1,
            )?;
            let raw = parser::read_remaining_rows(&mut reader, 0, Some(operation))?;
            operation.check()?;
            if PdfRevision::read(&sidecar_path)? != companion {
                return Err(io::Error::other(
                    "displayed PDF/SyncTeX revision changed; wait for reload and click again",
                ));
            }
            self.entry = Some(CachedSyncTeX {
                pdf: pdf.to_owned(),
                companion,
                sidecar_dir,
                raw,
                pages: std::collections::HashMap::new(),
                structural_maps: std::collections::HashMap::new(),
            });
        } else if let Some(entry) = self.entry.as_mut()
            && entry.sidecar_dir != sidecar_dir
        {
            entry.sidecar_dir = sidecar_dir;
            entry.structural_maps.clear();
        }
        let entry = self.entry.as_mut().expect("cache entry initialized");
        if !entry.pages.contains_key(&point.page) {
            let parsed = parser::parse_synctex_document_with_options(
                std::io::Cursor::new(&entry.raw),
                true,
                Some(point.page),
                Some(operation),
            )?;
            operation.check()?;
            if entry.pages.len() >= 32 {
                entry.pages.clear();
            }
            entry.pages.insert(point.page, parsed);
        }
        let targets = parser::inverse_candidates_in_doc(&entry.pages[&point.page], point)?;
        Ok((targets, companion, entry))
    }
}

pub fn resolve_inverse_with_cache(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    best_guess: bool,
    cache: Option<&mut SyncTeXCache>,
) -> io::Result<InverseResolution> {
    let resolved_build_directory = resolve_build_directory(None);
    resolve_inverse_resolved(
        InverseRequest {
            pdf,
            point,
            word,
            radius,
            operation,
            best_guess,
            build_directory: resolved_build_directory.as_deref(),
        },
        cache,
    )
}

#[derive(Clone, Copy)]
pub(crate) struct InverseRequest<'a> {
    pub(crate) pdf: &'a Path,
    pub(crate) point: InversePoint,
    pub(crate) word: Option<(&'a str, usize)>,
    pub(crate) radius: u32,
    pub(crate) operation: &'a Operation,
    pub(crate) best_guess: bool,
    pub(crate) build_directory: Option<&'a Path>,
}

fn resolve_inverse_resolved(
    req: InverseRequest<'_>,
    mut cache: Option<&mut SyncTeXCache>,
) -> io::Result<InverseResolution> {
    let InverseRequest {
        pdf,
        point,
        operation,
        best_guess,
        build_directory,
        ..
    } = req;
    operation.check()?;
    parser::validate_inverse_point(point)?;
    let pdf = std::path::absolute(pdf)?;
    let raw_revision = if best_guess {
        drop(open_regular(&pdf)?);
        Some(DocumentRevision::read_resolved(&pdf, build_directory)?)
    } else {
        None
    };
    operation.check()?;
    let (targets, cached_companion, sidecar_dir, sheet) = if let Some(cache) = cache.as_deref_mut()
    {
        let (targets, companion, entry) =
            cache.candidates_and_entry(&pdf, point, operation, build_directory)?;
        let sidecar_dir = entry.sidecar_dir.clone();
        (
            targets,
            Some(companion),
            sidecar_dir,
            SheetSource::Cached(entry),
        )
    } else {
        let (reader, sidecar_path) = open_synctex_with_path_in(&pdf, build_directory)?;
        let parsed = parser::parse_synctex_document_with_options(
            reader,
            true,
            Some(point.page),
            Some(operation),
        )?;
        operation.check()?;
        let targets = parser::inverse_candidates_in_doc(&parsed, point)?;
        let sidecar_dir = (!targets.is_empty())
            .then(|| canonical_sidecar_dir(&sidecar_path))
            .flatten();
        (
            targets,
            None,
            sidecar_dir,
            SheetSource::Parsed(Box::new(parsed)),
        )
    };
    let sources = QuerySources {
        best_guess,
        sidecar_dir,
        ..QuerySources::default()
    };
    let result = InverseQuery::new(InverseRequest { pdf: &pdf, ..req }, sources, sheet)
        .resolve_targets(targets)?;
    if let Some(revision) = raw_revision {
        revision.check_resolved(&pdf, build_directory)?;
    }
    if let Some(companion) = cached_companion
        && companion_revision_in(&pdf, build_directory)? != Some(companion)
    {
        if let Some(cache) = cache {
            cache.entry = None;
        }
        return Err(io::Error::other(
            "displayed PDF/SyncTeX revision changed; wait for reload and click again",
        ));
    }
    operation.check()?;
    Ok(result)
}

fn same_source_position(a: &SourceLocation, b: &SourceLocation) -> bool {
    Path::new(&a.file) == Path::new(&b.file) && a.line == b.line && a.byte_column == b.byte_column
}

#[derive(Default)]
struct QuerySources<'a> {
    best_guess: bool,
    pdf_dir: Option<&'a Path>,
    sidecar_dir: Option<PathBuf>,
    files: std::collections::HashMap<PathBuf, Result<Rc<String>, String>>,
}

impl QuerySources<'_> {
    fn normalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path).or_else(|err| {
            resolve_relative_input(path, None, self.pdf_dir, self.sidecar_dir.as_deref()).ok_or(err)
        })
    }

    fn read(&mut self, path: &Path, operation: &Operation) -> io::Result<Rc<String>> {
        operation.check()?;
        if let Some(result) = self.files.get(path) {
            return result.clone().map_err(io::Error::other);
        }
        let result = read_source(path)
            .map(Rc::new)
            .map_err(|error| error.to_string());
        operation.check()?;
        let value = result.clone().map_err(io::Error::other);
        self.files.insert(path.to_owned(), result);
        value
    }
}

type MetadataLocation = (
    String,
    Rc<String>,
    u32,
    usize,
    isize,
    Vec<std::ops::Range<usize>>,
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InverseFailure {
    NoMatch,
    Ambiguous(&'static str),
    InvalidSourceAnchor {
        file: PathBuf,
        line: u32,
        available: usize,
    },
}

impl std::fmt::Display for InverseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMatch => f.write_str("synctex edit returned no match"),
            Self::Ambiguous(reason) => f.write_str(reason),
            Self::InvalidSourceAnchor {
                file,
                line,
                available,
            } => write!(
                f,
                "invalid source anchor: {}:{} is out of range (available lines: {})",
                file.display(),
                line,
                available
            ),
        }
    }
}

impl std::error::Error for InverseFailure {}

struct InverseAnchor {
    target: SourceLocation,
    compiler_column: Option<usize>,
    graphics: Option<GraphicsHit>,
    generated: bool,
    warning: Option<String>,
    source_map: Option<io::Result<Rc<SourceMap>>>,
}

fn is_generated_sidecar(path: &Path, pdf: &Path) -> bool {
    path.file_stem() == pdf.file_stem() && is_compiler_sidecar_input(path)
}

fn is_generated_or_sidecar_input(path: &Path) -> bool {
    is_compiler_sidecar_input(path)
        || path
            .to_str()
            .is_some_and(crate::parser::is_generated_graphics_tex)
}

#[derive(Default)]
struct SourceArena {
    slots: [std::cell::OnceCell<Rc<String>>; 16],
    next: std::cell::OnceCell<Box<SourceArena>>,
}

impl SourceArena {
    fn pin(&self, rc: &Rc<String>) -> &str {
        let mut node = self;
        loop {
            for slot in &node.slots {
                if let Some(existing) = slot.get() {
                    if Rc::ptr_eq(existing, rc) {
                        return existing.as_str();
                    }
                } else {
                    return slot.get_or_init(|| Rc::clone(rc)).as_str();
                }
            }
            node = node.next.get_or_init(Box::default);
        }
    }
}

#[derive(Clone, Copy)]
struct QueryAnalysisScope<'q> {
    arena: &'q SourceArena,
    analyses: &'q tex::AnalysisCache<'q>,
}

impl<'q> QueryAnalysisScope<'q> {
    fn file_index(self, source: &Rc<String>, operation: &'q Operation) -> Rc<tex::SourceFileIndex> {
        self.analyses.file_index(self.arena.pin(source), operation)
    }

    fn get(
        self,
        source: &Rc<String>,
        extra_sources: &[&'q str],
        operation: &'q Operation,
    ) -> Rc<tex::SourceAnalysis<'q>> {
        self.analyses
            .get(self.arena.pin(source), extra_sources, operation)
    }
}

/// Where an inverse query reads the clicked sheet's source map from.
pub(crate) enum SheetSource<'a> {
    /// The query's own parse of the clicked sheet.
    Parsed(Box<parser::ParsedSynctex>),
    /// A cached parse of the clicked sheet; structural maps persist across queries.
    Cached(&'a mut CachedSyncTeX),
}

pub(crate) struct InverseQuery<'a> {
    pdf: &'a Path,
    point: InversePoint,
    word: Option<(&'a str, usize)>,
    radius: u32,
    operation: &'a Operation,
    sources: QuerySources<'a>,
    sheet: SheetSource<'a>,
    /// Structural maps of an uncached sheet; a cached sheet keeps its own.
    structural_maps: StructuralMaps,
    geometry_map: Option<Result<Rc<SourceMap>, String>>,
    metadata: Option<Option<MetadataLocation>>,
    spliced_preamble: Option<Option<Rc<String>>>,
}

impl<'a> InverseQuery<'a> {
    pub(crate) fn new(
        req: InverseRequest<'a>,
        mut sources: QuerySources<'a>,
        sheet: SheetSource<'a>,
    ) -> Self {
        sources.pdf_dir = req.pdf.parent();
        Self {
            pdf: req.pdf,
            point: req.point,
            word: req.word,
            radius: req.radius,
            operation: req.operation,
            sources,
            sheet,
            structural_maps: StructuralMaps::new(),
            geometry_map: None,
            metadata: None,
            spliced_preamble: None,
        }
    }

    pub(crate) fn resolve_targets(
        mut self,
        targets: Vec<RawInverseCandidate>,
    ) -> io::Result<InverseResolution> {
        if targets.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                InverseFailure::NoMatch,
            ));
        }
        let best_guess = self.sources.best_guess;
        let operation = self.operation;
        let arena = SourceArena::default();
        let analyses = tex::AnalysisCache::default();
        let scope = QueryAnalysisScope {
            arena: &arena,
            analyses: &analyses,
        };
        let mut resolutions = Vec::with_capacity(targets.len());
        for target in targets {
            operation.check()?;
            resolutions.push(self.resolve_target(target, scope)?);
        }
        operation.check()?;
        let mut supported = resolutions
            .iter()
            .enumerate()
            .filter(|(_, (result, _, _))| result.location.precise);
        let mut ambiguous = None;
        let selected = if let Some((index, (first, _, _))) = supported.next() {
            if supported
                .any(|(_, (other, _, _))| !same_source_position(&first.location, &other.location))
            {
                ambiguous = Some("ambiguous inverse search: multiple precise source locations");
            } else if resolutions.iter().any(|(other, _, word_tied)| {
                *word_tied && !same_source_position(&first.location, &other.location)
            }) {
                // An anchor with tied words is unresolved, unlike one with no matching word.
                ambiguous = Some("ambiguous inverse search: tied words oppose a precise location");
            }
            index
        } else {
            if resolutions[1..].iter().any(|(other, _, _)| {
                !same_source_position(&resolutions[0].0.location, &other.location)
            }) {
                ambiguous = Some("ambiguous inverse search: multiple coarse source anchors");
            }
            0
        };
        if let Some(reason) = ambiguous
            && !best_guess
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                InverseFailure::Ambiguous(reason),
            ));
        }
        // The executable's first result is its nearest compiler anchor. Explicit
        // best-guess mode cannot turn an ambiguous word refinement into precision.
        let (mut chosen, raw, _) =
            resolutions.swap_remove(if ambiguous.is_some() { 0 } else { selected });
        let mut warnings = std::collections::BTreeSet::new();
        if let Some(reason) = ambiguous {
            if let Some(raw) = raw {
                chosen.location = raw;
            }
            chosen.location.precise = false;
            warnings.insert(format!(
                "best-guess navigation: {reason}; using the first compiler anchor"
            ));
        }
        if let Some(warning) = chosen.warning {
            warnings.insert(warning);
        }
        for (other, _, _) in resolutions {
            if same_source_position(&chosen.location, &other.location)
                && let Some(warning) = other.warning
            {
                warnings.insert(warning);
            }
        }
        if best_guess && !chosen.location.precise && warnings.is_empty() {
            warnings.insert("best-guess navigation: no literal source position was qualified; using the compiler line anchor".into());
        }
        if best_guess {
            warnings.insert(
                "best-guess navigation: assuming saved source is unchanged since compilation"
                    .into(),
            );
        }
        operation.check()?;
        Ok(InverseResolution {
            location: chosen.location,
            warning: (!warnings.is_empty())
                .then(|| warnings.into_iter().collect::<Vec<_>>().join("; ")),
        })
    }

    fn load_source_map(
        &self,
        generated: &Path,
        geometry_required: bool,
        input_cwd: Option<&Path>,
    ) -> Result<Rc<SourceMap>, String> {
        let request = parser::SourceMapRequest {
            pdf: self.pdf,
            point: self.point,
            generated,
            geometry_required,
            input_cwd,
            sidecar_dir: self.sources.sidecar_dir.as_deref(),
            operation: self.operation,
        };
        let loaded = match &self.sheet {
            SheetSource::Parsed(parsed) => parsed.source_map(request),
            SheetSource::Cached(cache) => cache.pages.get(&self.point.page).map_or_else(
                || Err(io::Error::other("SyncTeX cache lost the clicked sheet")),
                |parsed| parsed.source_map(request),
            ),
        };
        loaded.map(Rc::new).map_err(|error| error.to_string())
    }

    /// Best-guess navigation fails outright when the clicked sheet's source map cannot load.
    fn require_source_map_for_best_guess(
        &self,
        source_map: &Option<io::Result<Rc<SourceMap>>>,
    ) -> io::Result<()> {
        if self.sources.best_guess
            && let Some(Err(error)) = source_map
        {
            self.operation.check()?;
            return Err(io::Error::other(error.to_string()));
        }
        Ok(())
    }

    fn structural_maps(&mut self) -> &mut StructuralMaps {
        match &mut self.sheet {
            SheetSource::Cached(cache) => &mut cache.structural_maps,
            _ => &mut self.structural_maps,
        }
    }

    fn source_map(&mut self, generated: Option<&Path>) -> io::Result<Rc<SourceMap>> {
        self.operation.check()?;
        let best_guess = self.sources.best_guess;
        let current_dir = if best_guess {
            Some(std::env::current_dir()?)
        } else {
            // Non-strict input resolution still tries the working directory.
            std::env::current_dir().ok()
        };
        let input_cwd = current_dir.as_deref().filter(|_| best_guess);
        let default_vrb = self.pdf.with_extension("vrb");
        let page = self.point.page;
        let key = |generated: &Path| {
            structural_map_key(page, generated, current_dir.as_deref(), best_guess)
        };
        let result = match generated {
            Some(generated) if generated == default_vrb && self.geometry_map.is_some() => {
                self.geometry_map.clone().unwrap()
            }
            Some(generated) => {
                let key = key(generated);
                if let Some(existing) = self.structural_maps().get(&key) {
                    existing.clone()
                } else {
                    let loaded = self.load_source_map(generated, false, input_cwd);
                    self.operation.check()?;
                    let maps = self.structural_maps();
                    if maps.len() >= 32 {
                        maps.clear();
                    }
                    maps.insert(key, loaded.clone());
                    loaded
                }
            }
            None => {
                // A structural map without fragile-frame provenance is also the geometry map.
                let default_key = key(&default_vrb);
                if self.geometry_map.is_none()
                    && let Some(Ok(map)) = self.structural_maps().get(&default_key)
                    && map.page_source.is_none()
                {
                    self.geometry_map = Some(Ok(Rc::clone(map)));
                }
                if self.geometry_map.is_none() {
                    self.geometry_map = Some(self.load_source_map(&default_vrb, true, input_cwd));
                }
                self.geometry_map.clone().unwrap()
            }
        };
        self.operation.check()?;
        result.map_err(io::Error::other)
    }

    fn resolve_target<'q>(
        &mut self,
        target: RawInverseCandidate,
        scope: QueryAnalysisScope<'q>,
    ) -> io::Result<(InverseResolution, Option<SourceLocation>, bool)>
    where
        'a: 'q,
    {
        let mut anchor = self.normalize_anchor(target)?;
        let Some(source) = self.qualify_source(&mut anchor, scope)? else {
            return Ok((
                InverseResolution {
                    location: anchor.target,
                    warning: anchor.warning,
                },
                None,
                false,
            ));
        };
        self.refine_target(anchor, source, scope)
    }

    fn normalize_anchor(&mut self, raw: RawInverseCandidate) -> io::Result<InverseAnchor> {
        let path = Path::new(&raw.file);
        let pdf_dir = self.pdf.parent().unwrap_or(Path::new("."));
        let sidecar_dir = self.sources.sidecar_dir.as_deref();
        let mut warning = None;
        let resolved = if self.sources.best_guess {
            let absolute: PathBuf = std::path::absolute(path)?.components().collect();
            if !path.is_absolute() && fs::symlink_metadata(&absolute).is_err() {
                resolve_relative_input(path, None, Some(pdf_dir), sidecar_dir).unwrap_or(absolute)
            } else {
                absolute
            }
        } else {
            let current_dir = std::env::current_dir()
                .ok()
                .and_then(|dir| fs::canonicalize(&dir).ok().or(Some(dir)));
            if !path.is_absolute()
                && let Some(found) =
                    resolve_relative_input(path, current_dir.as_deref(), Some(pdf_dir), sidecar_dir)
            {
                found
            } else {
                let absolute = if path.is_absolute() {
                    path.to_owned()
                } else {
                    pdf_dir.join(path)
                };
                match fs::canonicalize(&absolute) {
                    Ok(path) => path,
                    Err(error) => {
                        warning = Some(format!(
                            "line-only navigation: source path unavailable: {error}"
                        ));
                        absolute
                    }
                }
            }
        };
        let file = resolved
            .into_os_string()
            .into_string()
            .map_err(|_| io::Error::other("source path is not UTF-8"))?;
        let generated = is_generated_sidecar(Path::new(&file), self.pdf);
        // Known generated hits use structural sheet provenance before any
        // source bounds, and never read the final reused .vrb / sidecar file.
        let source_map = generated.then(|| self.source_map(Some(Path::new(&file))));
        self.require_source_map_for_best_guess(&source_map)?;
        Ok(InverseAnchor {
            target: SourceLocation {
                file,
                line: raw.line,
                byte_column: 0,
                column: 1,
                column_char: 1,
                precise: false,
            },
            compiler_column: (!generated).then_some(raw.compiler_column).flatten(),
            graphics: raw.graphics,
            generated,
            warning,
            source_map,
        })
    }

    // Authority and bounds are settled before word ownership. A coarse result
    // carries its compiler anchor unchanged; only a qualified source is refined.
    fn qualify_source<'q>(
        &mut self,
        anchor: &mut InverseAnchor,
        scope: QueryAnalysisScope<'q>,
    ) -> io::Result<Option<Rc<String>>>
    where
        'a: 'q,
    {
        let target = &mut anchor.target;
        let generated = anchor.generated;
        let source_map = &anchor.source_map;
        let warning = &mut anchor.warning;
        let raw_column = anchor.compiler_column;
        let verbatim_only = Path::new(&target.file)
            .extension()
            .is_some_and(|ext| ext == "vrb");
        let source = if generated {
            let Some(source) = self.remap_generated_source(target, source_map, warning, scope)?
            else {
                return Ok(None);
            };
            source
        } else {
            match self.sources.read(Path::new(&target.file), self.operation) {
                Ok(source) => source,
                Err(error) => {
                    self.operation.check()?;
                    *warning = Some(format!(
                        "line-only navigation: source refinement unavailable: {error}"
                    ));
                    return Ok(None);
                }
            }
        };
        let text = match validate_inverse_source_line(&source, target.line, Path::new(&target.file))
        {
            Ok(text) => text,
            Err(error) => {
                if !self.sources.best_guess {
                    return Err(error);
                }
                *warning = Some(format!(
                    "best-guess navigation: {error}; preserving the original compiler anchor for editor mapping"
                ));
                return Ok(None);
            }
        };
        if generated && !verbatim_only {
            return Ok(None);
        }
        if let Some(column) = raw_column {
            apply_compiler_column(target, text, column);
        }
        Ok(Some(source))
    }

    // The structural sheet anchor must identify the enclosing source frame for
    // generated sidecars (.vrb, .toc, .nav, .out, .aux, .snm).
    fn remap_generated_source<'q>(
        &mut self,
        target: &mut SourceLocation,
        source_map: &Option<io::Result<Rc<SourceMap>>>,
        warning: &mut Option<String>,
        scope: QueryAnalysisScope<'q>,
    ) -> io::Result<Option<Rc<String>>>
    where
        'a: 'q,
    {
        let verbatim_only = Path::new(&target.file)
            .extension()
            .is_some_and(|ext| ext == "vrb");
        let remapped = source_map
            .as_ref()
            .and_then(|map| map.as_ref().ok())
            .ok_or_else(|| {
                io::Error::other(
                    source_map
                        .as_ref()
                        .and_then(|map| map.as_ref().err())
                        .map_or_else(
                            || "original source map is unavailable".to_owned(),
                            ToString::to_string,
                        ),
                )
            })
            .and_then(|map| {
                if verbatim_only {
                    original_source_from_verbatim_with_scope(
                        map,
                        &mut self.sources,
                        self.operation,
                        scope,
                    )
                } else {
                    original_source_from_generated(
                        map,
                        target.line,
                        &mut self.sources,
                        self.operation,
                        scope,
                    )
                }
            });
        match remapped {
            Ok((file, original, anchor)) => {
                *warning = None;
                target.file = file;
                target.line = anchor;
                Ok(Some(original))
            }
            Err(error) => {
                self.operation.check()?;
                if self.sources.best_guess
                    && let Some(map) = source_map.as_ref().and_then(|map| map.as_ref().ok())
                {
                    let fallback = if verbatim_only {
                        Some(
                            map.page_source
                                .as_ref()
                                .map(|(path, line)| (path.as_path(), *line))
                                .unwrap_or((&map.main, 1)),
                        )
                    } else {
                        map.page_source
                            .as_ref()
                            .filter(|(path, _)| !is_generated_or_sidecar_input(path))
                            .map(|(path, line)| (path.as_path(), *line))
                    };
                    if let Some((original, line)) = fallback {
                        if line == 0 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "SyncTeX original compiler anchor line must be positive",
                            ));
                        }
                        let original =
                            fs::canonicalize(original).unwrap_or_else(|_| original.to_owned());
                        target.file = original
                            .into_os_string()
                            .into_string()
                            .map_err(|_| io::Error::other("source path is not UTF-8"))?;
                        target.line = line;
                        self.operation.check()?;
                    }
                }
                *warning = Some(format!(
                    "line-only navigation: original fragile frame unavailable: {error}"
                ));
                Ok(None)
            }
        }
    }

    fn refine_target<'q>(
        &mut self,
        anchor: InverseAnchor,
        mut source: Rc<String>,
        scope: QueryAnalysisScope<'q>,
    ) -> io::Result<(InverseResolution, Option<SourceLocation>, bool)>
    where
        'a: 'q,
    {
        let InverseAnchor {
            mut target,
            graphics,
            generated,
            mut warning,
            mut source_map,
            ..
        } = anchor;
        let is_generated_graphics = crate::parser::is_generated_graphics_tex(&target.file);
        let anchor = (!is_generated_graphics && (self.word.is_some() || graphics.is_some()))
            .then(|| target.clone());
        let refine_graphics = if let Some(GraphicsHit::Unique { index, count }) = graphics {
            Some((index, count))
        } else {
            None
        };
        let mut word_tied = false;
        let mut frame_owns_click = false;
        if !is_generated_graphics
            && (refine_graphics.is_some() || (graphics.is_none() && self.word.is_some()))
        {
            let mut source_index = scope.file_index(&source, self.operation);
            let mut target_frame =
                tex::collected_lines(&source, target.line, "frame", &source_index.strict_ignored);
            let fragile_frame = target_frame
                .as_ref()
                .filter(|frame| is_fragile_frame(&source, &source_index.line_starts, frame))
                .cloned();
            if !generated {
                // Ordinary source refinement and document metadata need only
                // structural provenance, not fragile-body geometry.
                let default_vrb = self.pdf.with_extension("vrb");
                source_map = Some(self.source_map(fragile_frame.is_none().then_some(&default_vrb)));
            }
            self.require_source_map_for_best_guess(&source_map)?;
            self.operation.check()?;
            // A fragile sheet also contains nongenerated headings and footers.
            // Sheet-wide .vrb participation does not prove this point is body text.
            let scope_proven = match (&fragile_frame, &target_frame) {
                (Some(frame), _) => {
                    generated
                        || source_map
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .is_some_and(|map| {
                                map.generated_at_point
                                    && map.page_source.as_ref().is_some_and(|(path, line)| {
                                        *line == frame.end as u32
                                            && self
                                                .sources
                                                .normalize(path)
                                                .is_ok_and(|path| path == Path::new(&target.file))
                                    })
                            })
                }
                (None, Some(frame)) if !generated => source_map
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .and_then(|map| map.page_source.as_ref())
                    .is_none_or(|(path, line)| {
                        *line == frame.end as u32
                            || !self
                                .sources
                                .normalize(path)
                                .is_ok_and(|path| path == Path::new(&target.file))
                    }),
                _ => true,
            };
            let frame_slide = source_map
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(|map| map.frame_slide);
            let spliced_preamble = if let Some(Ok(map)) = &source_map {
                match &self.spliced_preamble {
                    Some(cached) => cached.clone(),
                    None => {
                        let loaded = macro_preamble_source_with_scope(
                            &map.main,
                            Some(&map.inputs),
                            self.pdf.parent(),
                            &mut self.sources,
                            self.operation,
                            scope,
                        )?
                        .map(Rc::new);
                        self.spliced_preamble = Some(loaded.clone());
                        loaded
                    }
                }
            } else {
                None
            };
            let spliced_ref = spliced_preamble.as_ref().map(|rc| scope.arena.pin(rc));
            let extra_sources: &[&'q str] = spliced_ref.as_slice();
            let fragile_body_proven = scope_proven
                && fragile_frame.is_some()
                && source_map
                    .as_ref()
                    .and_then(|map| map.as_ref().ok())
                    .is_some_and(|map| map.generated.extension().is_some_and(|ext| ext == "vrb"));
            let mut opaque_word = false;
            let mut word_outcome = |outcome: tex::WordOutcome| {
                if outcome == tex::WordOutcome::Opaque {
                    opaque_word = true;
                }
                outcome
            };
            let initial_source = Rc::clone(&source);
            let analysis = scope.get(&initial_source, extra_sources, self.operation);
            let mut location = if let Some((index, count)) = refine_graphics {
                source_graphics_location_with_context(
                    &analysis,
                    target.line,
                    (index, count),
                    scope_proven,
                    frame_slide,
                    tex::WordLocationMode::Strict,
                )
            } else if let Some((context, offset)) = self.word {
                let outcome = word_outcome(scoped_word_outcome(
                    &analysis,
                    target.line,
                    (context, offset),
                    self.radius,
                    (fragile_body_proven, scope_proven),
                    tex::WordLocationMode::Strict,
                ));
                word_tied = outcome == tex::WordOutcome::Tied;
                outcome.location()
            } else {
                None
            };
            self.operation.check()?;
            // A generated box at the point belongs to the fragile frame its sheet names, so a
            // matching word on a row outside that frame cannot claim the click.
            if location.is_some()
                && refine_graphics.is_none()
                && !generated
                && source_map
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .and_then(|map| map.page_source.as_ref())
                    .is_some_and(|(path, line)| {
                        self.sources
                            .normalize(path)
                            .is_ok_and(|path| path == Path::new(&target.file))
                            && fragile_frame_range_with_index(&source, *line, &source_index)
                                .is_some_and(|frame| {
                                    *line == frame.end as u32
                                        && !frame
                                            .contains(&(target.line.saturating_sub(1) as usize))
                                })
                    })
            {
                let geometry = self.source_map(None)?;
                if geometry.generated_at_point {
                    location = None;
                    frame_owns_click = true;
                    source_map = Some(Ok(geometry));
                }
            }
            self.operation.check()?;
            if location.is_none() && !generated {
                if fragile_frame.is_none()
                    && source_map
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .is_some_and(|map| map.page_source.is_some())
                {
                    source_map = Some(self.source_map(None));
                    self.require_source_map_for_best_guess(&source_map)?;
                }
                if let Some(map) = source_map
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .filter(|map| map.generated_at_point && map.page_source.is_some())
                {
                    match original_source_from_verbatim_with_scope(
                        map,
                        &mut self.sources,
                        self.operation,
                        scope,
                    ) {
                        Ok((file, original, anchor)) => {
                            let frame_slide = map.frame_slide;
                            let mut original_tied = false;
                            let original_analysis =
                                scope.get(&original, extra_sources, self.operation);
                            let candidate = if let Some((index, count)) = refine_graphics {
                                source_graphics_location_with_context(
                                    &original_analysis,
                                    anchor,
                                    (index, count),
                                    true,
                                    frame_slide,
                                    tex::WordLocationMode::Strict,
                                )
                            } else if let Some((context, offset)) = self.word {
                                let outcome = word_outcome(
                                    if map.generated.extension().is_some_and(|ext| ext == "vrb") {
                                        source_word_location_in_frame_body_with_analysis(
                                            &original_analysis,
                                            anchor,
                                            (context, offset),
                                        )
                                    } else {
                                        source_word_location_with_analysis(
                                            &original_analysis,
                                            anchor,
                                            (context, offset),
                                            self.radius,
                                            tex::WordLocationMode::Strict,
                                        )
                                    },
                                );
                                original_tied = outcome == tex::WordOutcome::Tied;
                                outcome.location()
                            } else {
                                None
                            };
                            self.operation.check()?;
                            // The original frame's result replaces the host search's evidence.
                            word_tied = original_tied;
                            if let Some(candidate) = candidate {
                                target.file = file;
                                target.line = anchor;
                                source = original;
                                source_index = Rc::clone(&original_analysis.index);
                                target_frame = tex::collected_lines(
                                    &source,
                                    target.line,
                                    "frame",
                                    &source_index.strict_ignored,
                                );
                                location = Some(candidate);
                            }
                        }
                        Err(error) => {
                            self.operation.check()?;
                            warning = Some(format!(
                                "line-only navigation: original fragile frame unavailable: {error}"
                            ));
                        }
                    }
                }
            }
            if let Some(Err(error)) = &source_map {
                self.operation.check()?;
                warning = Some(format!("source-map refinement unavailable: {error}"));
            }
            if refine_graphics.is_none()
                && !opaque_word
                && !frame_owns_click
                && let Some((context, offset)) = self.word
                && !generated
                && let Some(map) = source_map.as_ref().and_then(|result| result.as_ref().ok())
                && if location.is_some() {
                    target_frame.is_some()
                } else {
                    target_frame.is_some()
                        || allows_metadata_fallback_outside_frame(
                            &source,
                            &source_index.line_starts,
                            target.line,
                            self.radius,
                            self.point.y_from_top,
                            self.point.page_height_pt,
                        )
                }
            {
                if self.metadata.is_none() {
                    self.metadata = Some(document_metadata_word_location_with_context(
                        &map.main,
                        Some(&map.inputs),
                        self.pdf.parent(),
                        context,
                        offset,
                        &mut self.sources,
                        self.operation,
                        scope,
                    )?);
                }
                if let Some((file, original, line, byte, score, arguments)) =
                    self.metadata.as_ref().and_then(Option::as_ref)
                    && if let Some(anchor) = &anchor
                        && *file == anchor.file
                    {
                        *line == anchor.line || {
                            let text =
                                validate_inverse_source_line(original, *line, Path::new(file))?;
                            let prefix = text.get(..*byte).ok_or_else(|| {
                                io::Error::other("source column is not a UTF-8 boundary")
                            })?;
                            let metadata_index = scope.file_index(original, self.operation);
                            source_word_compiler_line_with_index(
                                original,
                                *line,
                                (prefix.chars().count() + 1) as u32,
                                &metadata_index,
                            ) == anchor.line
                                // A shared compiler line separates metadata from a tie only when
                                // blanking its arguments leaves no located word or prose tie.
                                && (!word_tied || {
                                    let blanked = Rc::new(blank_source_words(original, arguments));
                                    let blanked_analysis =
                                        scope.get(&blanked, extra_sources, self.operation);
                                    scoped_word_outcome(
                                        &blanked_analysis,
                                        target.line,
                                        (context, offset),
                                        self.radius,
                                        (fragile_body_proven, scope_proven),
                                        tex::WordLocationMode::Strict,
                                    ) == tex::WordOutcome::Absent
                                })
                        }
                    } else {
                        false
                    }
                    && (location.is_none()
                        || *score >= 6 && {
                            let frame_analysis = scope.get(&source, &[], self.operation);
                            metadata_beats_frame(
                                &frame_analysis,
                                target.line,
                                (context, offset),
                                self.point.y_from_top,
                                self.point.page_height_pt,
                                *score,
                            )
                        })
                {
                    target.file = file.clone();
                    source = Rc::clone(original);
                    source_index = scope.file_index(&source, self.operation);
                    location = Some((*line, *byte));
                }
            }
            self.operation.check()?;
            if location.is_none()
                && !frame_owns_click
                && self.sources.best_guess
                && !generated
                && warning.is_none()
            {
                let candidate = if let Some((index, count)) = refine_graphics {
                    source_graphics_location_with_context(
                        &analysis,
                        target.line,
                        (index, count),
                        scope_proven,
                        frame_slide,
                        tex::WordLocationMode::Guess,
                    )
                } else if let Some((context, offset)) = self.word {
                    scoped_word_outcome(
                        &analysis,
                        target.line,
                        (context, offset),
                        self.radius,
                        (false, scope_proven),
                        tex::WordLocationMode::Guess,
                    )
                    .location()
                } else {
                    None
                };
                self.operation.check()?;
                if let Some(candidate) = candidate {
                    location = Some(candidate);
                }
            }
            if let Some((line, byte)) = location {
                let text = validate_inverse_source_line(&source, line, Path::new(&target.file))?;
                let prefix = text
                    .get(..byte)
                    .ok_or_else(|| io::Error::other("source column is not a UTF-8 boundary"))?;
                let column_char = prefix.chars().count() + 1;
                let compiler_line = source_word_compiler_line_with_index(
                    &source,
                    line,
                    column_char as u32,
                    &source_index,
                );
                if anchor.as_ref().is_some_and(|anchor| {
                    target.file == anchor.file
                        && (line == anchor.line
                            || compiler_line == anchor.line
                            || refine_graphics.is_some())
                }) {
                    target.line = line;
                    target.byte_column = byte;
                    target.column = prefix.encode_utf16().count() + 1;
                    target.column_char = column_char;
                    target.precise = true;
                } else if let Some(anchor) = &anchor {
                    target = anchor.clone();
                }
            }
        }
        self.operation.check()?;
        // A word displaced by the owning frame still competes with that frame's location.
        let word_tied = (word_tied || frame_owns_click) && !target.precise;
        Ok((
            InverseResolution {
                location: target,
                warning,
            },
            anchor,
            word_tied,
        ))
    }
}

fn scoped_word_outcome(
    analysis: &tex::SourceAnalysis<'_>,
    line: u32,
    (context, offset): (&str, usize),
    radius: u32,
    (fragile_body_proven, scope_proven): (bool, bool),
    mode: tex::WordLocationMode,
) -> tex::WordOutcome {
    if fragile_body_proven {
        source_word_location_in_frame_body_with_analysis(analysis, line, (context, offset))
    } else if scope_proven {
        source_word_location_with_analysis(analysis, line, (context, offset), radius, mode)
    } else {
        source_word_location_in_range_with_analysis(
            analysis,
            line,
            (context, offset),
            line.saturating_sub(1) as usize..line as usize,
            false,
            mode,
            None,
        )
    }
}

/// Removes words from ordered spans while keeping byte offsets and TeX delimiters.
fn blank_source_words(source: &str, spans: &[std::ops::Range<usize>]) -> String {
    let mut blanked = String::with_capacity(source.len());
    let mut at = 0;
    for span in spans {
        blanked.push_str(&source[at..span.start]);
        for ch in source[span.clone()].chars() {
            if ch.is_alphanumeric() {
                blanked.extend(std::iter::repeat_n(' ', ch.len_utf8()));
            } else {
                blanked.push(ch);
            }
        }
        at = span.end;
    }
    blanked.push_str(&source[at..]);
    blanked
}

fn source_word_compiler_line_with_index(
    source: &str,
    line: u32,
    column: u32,
    index: &tex::SourceFileIndex,
) -> u32 {
    collected_closing_line(source, (line, column), index, |spans| {
        // An enclosing collector, math body or run-in heading ships after inner wrappers.
        [
            tex::argument_closing_line_with_spans,
            tex::run_in_heading_compiler_line_with_spans,
            tex::line_start_word_compiler_line_with_spans,
        ]
        .into_iter()
        .filter_map(|closing| closing(source, line, column, spans))
        .max()
    })
    .unwrap_or(line)
}

/// Compiler line shipping a collected frame/`tabularx` body, else the latest of the
/// enclosing display-math body and the `inner` collectors' closing lines.
fn collected_closing_line(
    source: &str,
    (line, column): (u32, u32),
    index: &tex::SourceFileIndex,
    inner: impl FnOnce(&[Span]) -> Option<u32>,
) -> Option<u32> {
    tex::collected_environment_closing_line(source, line, &index.strict_ignored).or_else(|| {
        let spans = &index.strict_spans;
        let math = math::math_closing_line_with_spans(source, line, column, spans);
        inner(spans).max(math)
    })
}

fn validate_source_line(source: &str, line: u32) -> io::Result<&str> {
    line.checked_sub(1)
        .and_then(|index| source.lines().nth(index as usize))
        .ok_or_else(|| io::Error::other(format!("source line {line} is out of range")))
}

fn validate_inverse_source_line<'a>(
    source: &'a str,
    line: u32,
    file: &Path,
) -> io::Result<&'a str> {
    line.checked_sub(1)
        .and_then(|index| source.lines().nth(index as usize))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                InverseFailure::InvalidSourceAnchor {
                    file: file.to_owned(),
                    line,
                    available: source.lines().count(),
                },
            )
        })
}

pub(crate) fn validate_forward_source_position(
    source: &str,
    line: u32,
    column: u32,
) -> io::Result<&str> {
    let text = validate_source_line(source, line)?;
    if column == 0 || column as usize > text.chars().count() + 1 {
        return Err(io::Error::other("source scalar column is out of range"));
    }
    Ok(text)
}

fn allows_metadata_fallback_outside_frame(
    source: &str,
    line_starts: &tex::LineIndex,
    line: u32,
    radius: u32,
    y_from_top: f32,
    page_height_pt: f32,
) -> bool {
    if page_height_pt > 0.0
        && (y_from_top <= page_height_pt * 0.12 || y_from_top >= page_height_pt * 0.88)
    {
        return true;
    }
    let start = line.saturating_sub(radius.saturating_add(1)) as usize;
    let end = (line as usize).saturating_add(radius as usize);
    source[line_starts.row_start(start, source.len())..]
        .lines()
        .take(end.saturating_sub(start))
        .any(|row| {
            let text = source_line_text(row);
            text.contains("\\maketitle") || text.contains("\\titlepage")
        })
}

/// A frame title and the running document title may contain the same words.
/// Preserve a local frame hit except when a footer has stronger metadata context.
fn metadata_beats_frame(
    analysis: &tex::SourceAnalysis<'_>,
    line: u32,
    (context, offset): (&str, usize),
    y_from_top: f32,
    page_height_pt: f32,
    metadata_score: isize,
) -> bool {
    let Some(frame) =
        tex::collected_lines(analysis.source, line, "frame", &analysis.strict_ignored)
    else {
        return false;
    };
    y_from_top >= page_height_pt / 2.0
        && source_prose_scored_location_with_analysis(
            analysis,
            line,
            (context, offset),
            frame,
            true,
            None,
        )
        .is_some_and(|(_, _, local_score)| metadata_score >= local_score)
}

fn read_source(path: &Path) -> io::Result<String> {
    let file = open_regular(path).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            io::Error::other("source refinement requires a regular file")
        } else {
            e
        }
    })?;
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "source exceeds 2 MiB refinement limit",
        ));
    }
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

struct SourceMap {
    main: PathBuf,
    inputs: Vec<PathBuf>,
    page_source: Option<(PathBuf, u32)>,
    generated_at_point: bool,
    frame_slide: Option<u32>,
    generated: PathBuf,
    adjacent_origins: Vec<(PathBuf, u32)>,
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

/// The path of a declared `Input:`. `Ok(cwd)` resolves strictly against the compiler's working
/// directory; `Err(cwd)` resolves leniently, ignoring the trailing blanks some engines pad
/// `Input:` rows with, and falls back to the PDF directory.
pub(crate) fn resolve_declared_input(
    raw: &str,
    cwd: Result<&Path, Option<&Path>>,
    pdf_dir: Option<&Path>,
    sidecar_dir: Option<&Path>,
) -> Result<PathBuf, &'static str> {
    let raw = Path::new(if cwd.is_ok() {
        raw
    } else {
        raw.trim_end_matches(' ')
    });
    match cwd {
        Ok(_) if raw.as_os_str().is_empty() => Err("invalid SyncTeX input declaration"),
        Ok(_) if raw.is_absolute() && !valid_absolute_path(raw) => {
            Err("invalid SyncTeX input path")
        }
        _ if raw.is_absolute() => Ok(raw.to_owned()),
        Ok(cwd) => {
            let joined = cwd.join(raw);
            if !valid_absolute_path(&joined) {
                return Err("invalid SyncTeX input path");
            }
            Ok(if fs::symlink_metadata(&joined).is_err() {
                resolve_relative_input(raw, None, pdf_dir, sidecar_dir).unwrap_or(joined)
            } else {
                joined
            })
        }
        Err(cwd) => Ok(resolve_relative_input(raw, cwd, pdf_dir, sidecar_dir)
            .unwrap_or_else(|| pdf_dir.unwrap_or(Path::new(".")).join(raw))),
    }
}

/// The `Input:` tags naming `file`, latest declaration first: the tag a forward query uses and
/// every matching tag. An unresolvable relative input matches by file name only when all such
/// declarations name the same raw path.
pub(crate) fn forward_input_tags(
    inputs: &[(u32, String)],
    file: &Path,
    pdf: &Path,
    sidecar_dir: Option<&Path>,
) -> Option<(u32, std::collections::HashSet<u32>)> {
    let pdf_dir = pdf.parent().unwrap_or(Path::new("."));
    let cwd = std::env::current_dir().ok();
    let cwd = cwd.map(|dir| fs::canonicalize(&dir).unwrap_or(dir));
    let resolve =
        |raw: &Path| resolve_relative_input(raw, cwd.as_deref(), Some(pdf_dir), sidecar_dir);
    let mut matching = std::collections::HashSet::new();
    let mut input_tag = None;
    for (tag, raw_path) in inputs.iter().rev() {
        let raw = Path::new(raw_path);
        let normalized = if raw.is_absolute() {
            Some(fs::canonicalize(raw).unwrap_or_else(|_| raw.to_owned()))
        } else {
            resolve(raw).or_else(|| {
                let path = normalize_absolute(raw, pdf_dir).ok()?;
                Some(fs::canonicalize(&path).unwrap_or(path))
            })
        };
        if normalized.as_deref() == Some(file) {
            matching.insert(*tag);
            input_tag.get_or_insert(*tag);
        }
    }
    if input_tag.is_some() {
        return input_tag.map(|tag| (tag, matching));
    }
    let name = file.file_name()?;
    let mut matched_path = None::<&str>;
    for (tag, raw_path) in inputs.iter().rev() {
        let raw = Path::new(raw_path);
        if raw.file_name() != Some(name) || resolve(raw).is_some() {
            continue;
        }
        match matched_path {
            None => {
                matched_path = Some(raw_path);
                input_tag = Some(*tag);
            }
            Some(prev) if prev != raw_path => return None,
            Some(_) => {}
        }
        matching.insert(*tag);
    }
    input_tag.map(|tag| (tag, matching))
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

pub(crate) fn decode_synctex_file(file: fs::File, limit: u64) -> io::Result<Box<dyn BufRead>> {
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    if reader.fill_buf()?.starts_with(&[0x1f, 0x8b]) {
        Ok(Box::new(BufReader::with_capacity(
            64 * 1024,
            flate2::read::MultiGzDecoder::new(reader).take(limit),
        )))
    } else {
        Ok(Box::new(reader.take(limit)))
    }
}

pub(crate) fn open_synctex_with_path_in(
    pdf: &Path,
    build_directory: Option<&Path>,
) -> io::Result<(Box<dyn BufRead>, PathBuf)> {
    let mut last_not_found = None;
    for candidate in synctex_candidates(pdf, build_directory) {
        match open_regular(&candidate) {
            Ok(file) => {
                return Ok((
                    decode_synctex_file(file, crate::parser::MAX_SYNCTEX_BYTES as u64 + 1)?,
                    candidate,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => last_not_found = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_not_found
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no SyncTeX companion file")))
}

/// Beamer reopens the same .vrb for successive fragile frames. The clicked
/// sheet's enclosing source-map box identifies the original frame; the final
/// generated file's contents cannot establish that identity.
fn original_source_from_verbatim_with_scope<'q>(
    map: &SourceMap,
    sources: &mut QuerySources<'_>,
    operation: &'q Operation,
    scope: QueryAnalysisScope<'q>,
) -> io::Result<(String, Rc<String>, u32)> {
    let (original, line) = map.page_source.as_ref().ok_or_else(|| {
        io::Error::other("clicked sheet has no original generated-file provenance")
    })?;
    if is_generated_or_sidecar_input(original) {
        return Err(io::Error::other(
            "clicked sheet does not identify a fragile frame's closing line",
        ));
    }
    let (original, source) = read_original_source(original, *line, sources, operation)?;
    let index = scope.file_index(&source, operation);
    fragile_frame_range_with_index(&source, *line, &index)
        .filter(|frame| *line == frame.end as u32)
        .ok_or_else(|| {
            io::Error::other("clicked sheet does not identify a fragile frame's closing line")
        })?;
    Ok((original_source_file(original)?, source, *line))
}

/// Reads a normalized original source whose `line` must be in range.
fn read_original_source(
    path: &Path,
    line: u32,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<(PathBuf, Rc<String>)> {
    let original = sources.normalize(path)?;
    let source = sources.read(&original, operation)?;
    validate_inverse_source_line(&source, line, &original)?;
    Ok((original, source))
}

fn original_source_file(original: PathBuf) -> io::Result<String> {
    original
        .into_os_string()
        .into_string()
        .map_err(|_| io::Error::other("original fragile source path is not UTF-8"))
}

fn original_source_from_generated<'q>(
    map: &SourceMap,
    raw_line: u32,
    sources: &mut QuerySources<'_>,
    operation: &'q Operation,
    scope: QueryAnalysisScope<'q>,
) -> io::Result<(String, Rc<String>, u32)> {
    let generated_norm = sources
        .normalize(&map.generated)
        .unwrap_or_else(|_| map.generated.clone());
    let main_dir = sources
        .normalize(&map.main)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .or_else(|| map.main.parent().map(Path::to_path_buf));
    for (candidate, line) in &map.adjacent_origins {
        if is_generated_or_sidecar_input(candidate) {
            continue;
        }
        let (original, source) = read_original_source(candidate, *line, sources, operation)?;
        let index = scope.file_index(&source, operation);
        if tex::host_owns_generated_sidecar_with_spans(
            &source,
            &original,
            &generated_norm,
            main_dir.as_deref(),
            &index.strict_spans,
        ) {
            return Ok((original_source_file(original)?, source, *line));
        }
    }
    let (original, page_line) = map.page_source.as_ref().ok_or_else(|| {
        io::Error::other("clicked sheet has no original generated-file provenance")
    })?;
    let (target_path, line) = if is_generated_or_sidecar_input(original) {
        (&map.generated, raw_line)
    } else {
        (original, *page_line)
    };
    let (original, source) = read_original_source(target_path, line, sources, operation)?;
    Ok((original_source_file(original)?, source, line))
}

fn fragile_frame_range_with_index(
    source: &str,
    line: u32,
    index: &tex::SourceFileIndex,
) -> Option<std::ops::Range<usize>> {
    tex::collected_lines(source, line, "frame", &index.strict_ignored)
        .filter(|frame| is_fragile_frame(source, &index.line_starts, frame))
}

fn is_fragile_frame(
    source: &str,
    line_starts: &tex::LineIndex,
    frame: &std::ops::Range<usize>,
) -> bool {
    let Some(mut at) = crate::tex::frame_header_after_begin(
        source,
        line_starts.row_start(frame.start, source.len()),
    ) else {
        return false;
    };
    let mut saw_default_overlay = false;
    let mut saw_options = false;
    let mut fragile = false;
    while let Some((header, end)) =
        crate::tex::next_frame_option_group(source, at, &mut saw_default_overlay, &mut saw_options)
    {
        // A default-overlay group `[<...>]` holds no frame options.
        if saw_options {
            // A later assignment replaces an earlier one.
            crate::tex::for_each_frame_option(
                &source[header.start + 1..header.end - 1],
                |key, value| {
                    if key == "fragile" {
                        fragile = value != Some("false");
                    }
                    false
                },
            );
        }
        at = end;
    }
    fragile
}

fn input_candidates(
    clean_name: &str,
    extension: &str,
    bases: &[Option<&Path>],
    compiled: Option<&[PathBuf]>,
    sources: &QuerySources<'_>,
) -> Vec<PathBuf> {
    let mut relative = PathBuf::from(clean_name);
    if relative.extension().is_none() {
        relative.set_extension(extension);
    }
    let mut local_candidates = Vec::new();
    for base in bases.iter().flatten() {
        let candidate = if relative.is_absolute() {
            relative.clone()
        } else {
            base.join(&relative)
        };
        if let Ok(normalized) = sources.normalize(&candidate)
            && !local_candidates.contains(&normalized)
        {
            local_candidates.push(normalized);
        }
    }
    let Some(compiled) = compiled else {
        return local_candidates;
    };
    let local_matches: Vec<PathBuf> = local_candidates
        .into_iter()
        .filter(|candidate| compiled.contains(candidate))
        .collect();
    if !local_matches.is_empty() {
        return local_matches;
    }
    let suffix: PathBuf = relative
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .collect();
    compiled
        .iter()
        .filter(|path| {
            !suffix.as_os_str().is_empty()
                && path.ends_with(&suffix)
                && (extension != "sty"
                    || !path.components().any(|component| {
                        let name = component.as_os_str().to_string_lossy();
                        name.starts_with("texmf") || name.starts_with("texlive")
                    }))
        })
        .cloned()
        .collect()
}

/// The normalized main source of a preamble scan and the other compiled inputs, or `None`
/// when the main source cannot be normalized or read.
#[allow(clippy::type_complexity)]
fn preamble_root(
    main: &Path,
    compiled_inputs: Option<&[PathBuf]>,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<Option<(PathBuf, Rc<String>, Option<Vec<PathBuf>>)>> {
    operation.check()?;
    let Ok(main) = sources.normalize(main) else {
        return Ok(None);
    };
    let Ok(source) = sources.read(&main, operation) else {
        return operation.check().map(|()| None);
    };
    let compiled = compiled_inputs.map(|inputs| {
        let normalized = inputs
            .iter()
            .filter_map(|path| sources.normalize(path).ok());
        normalized.filter(|path| path != &main).collect()
    });
    Ok(Some((main, source, compiled)))
}

fn preamble_sources(
    main: &Path,
    compiled_inputs: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<Vec<(PathBuf, Rc<String>)>> {
    let Some((main, main_source, compiled)) =
        preamble_root(main, compiled_inputs, sources, operation)?
    else {
        return Ok(Vec::new());
    };
    let main_dir = main.parent().unwrap_or_else(|| Path::new("")).to_owned();
    let mut imports = Vec::new();
    for raw_row in main_source.split_inclusive('\n') {
        let row = raw_row.trim_end_matches(['\r', '\n']);
        let visible = source_line_text(row);
        if visible.contains("\\begin{document}") {
            break;
        }
        if let Some(rest) = visible.trim().strip_prefix("\\input{")
            && let Some((name, _)) = rest.split_once('}')
        {
            imports.push(name.to_owned());
            if imports.len() >= 8 {
                break;
            }
        }
    }
    let mut files = vec![(main, main_source)];
    for name in imports {
        operation.check()?;
        let clean_name = source_line_text(&name).trim().to_owned();
        if clean_name.is_empty() || clean_name.contains('\\') {
            continue;
        }
        let bases = [
            Some(&main_dir as &Path),
            pdf_dir,
            sources.sidecar_dir.as_deref(),
        ];
        let candidates = input_candidates(&clean_name, "tex", &bases, compiled.as_deref(), sources);
        let mut readable = Vec::new();
        for cand in candidates {
            if files.iter().any(|(known, _)| known == &cand) {
                continue;
            }
            match sources.read(&cand, operation) {
                Ok(source) => readable.push((cand, source)),
                Err(_) => operation.check()?,
            }
        }
        if readable.len() == 1 {
            files.push(readable.pop().unwrap());
        }
    }
    Ok(files)
}

fn macro_preamble_source_with_scope<'q>(
    main: &Path,
    compiled_inputs: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    sources: &mut QuerySources<'_>,
    operation: &'q Operation,
    scope: QueryAnalysisScope<'q>,
) -> io::Result<Option<String>> {
    let Some((main, main_source, compiled)) =
        preamble_root(main, compiled_inputs, sources, operation)?
    else {
        return Ok(None);
    };
    let main_dir = main.parent().unwrap_or_else(|| Path::new(""));
    let main_index = scope.file_index(&main_source, operation);
    let mut visited = vec![main.clone()];
    let mut out = String::with_capacity(main_source.len());
    let mut in_preamble = true;
    append_spliced_source(
        (&main, &main_source, &main_index.strict_spans),
        main_dir,
        compiled.as_deref(),
        pdf_dir,
        (0, false, &mut in_preamble),
        &mut visited,
        sources,
        (operation, scope),
        &mut out,
    )?;
    Ok((!out.is_empty()).then_some(out))
}

/// `spans` are the source's precomputed strict nonprinting spans; `scope` caches
/// them for spliced children.
#[allow(clippy::too_many_arguments)]
fn append_spliced_source<'q>(
    (path, source, spans): (&Path, &str, &[Span]),
    main_dir: &Path,
    compiled: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    (depth, poisoned_scope, in_preamble): (usize, bool, &mut bool),
    visited: &mut Vec<PathBuf>,
    sources: &mut QuerySources<'_>,
    (operation, scope): (&'q Operation, QueryAnalysisScope<'q>),
    out: &mut String,
) -> io::Result<()> {
    operation.check()?;
    if depth >= 8 || visited.len() >= 32 {
        return Ok(());
    }
    if poisoned_scope {
        out.push_str("{\n");
    }
    let mut nonprinting = std::borrow::Cow::Borrowed(spans);
    let mut next_span = 0usize;
    let mut brace_depth = 0usize;
    let mut group_depth = 0usize;
    let mut conditionals = tex::ConditionalStack::default();
    let mut cursor = 0usize;
    let mut at = 0usize;
    while at < source.len() {
        operation.check()?;
        if conditionals.skipping() {
            at = conditionals.skip_token(source, at, false);
            if !conditionals.skipping() {
                tex::resume_nonprinting_spans(source, at, &mut nonprinting, &mut next_span);
            }
            continue;
        }
        while nonprinting
            .get(next_span)
            .is_some_and(|(_, span, _)| span.end <= at)
        {
            next_span += 1;
        }
        let executing = !nonprinting
            .get(next_span)
            .is_some_and(|(_, span, _)| span.contains(&at));
        match source.as_bytes()[at] {
            b'%' => at = tex::comment_end(source, at),
            b'{' => {
                brace_depth += 1;
                at += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                at += 1;
            }
            b'$' => {
                if brace_depth == 0 && group_depth == 0 && conditionals.is_empty() && executing {
                    *in_preamble = false;
                }
                at += 1;
            }
            b'\\' => {
                let cmd_start = at;
                let (cmd, end) = math::command(source, at);
                at = end;
                if executing
                    && let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, at)
                {
                    at = opaque_end;
                    continue;
                }
                if brace_depth == 0 && executing && conditionals.apply(cmd, false) {
                    continue;
                }
                if executing && matches!(cmd, "(" | "[") {
                    if brace_depth == 0 && group_depth == 0 && conditionals.is_empty() {
                        *in_preamble = false;
                    }
                    continue;
                }
                if executing && matches!(cmd, "begingroup" | "bgroup") {
                    group_depth += 1;
                    continue;
                }
                if executing && matches!(cmd, "endgroup" | "egroup") {
                    group_depth = group_depth.saturating_sub(1);
                    continue;
                }
                if executing && cmd == "begin" {
                    if let Some((env, next)) = math::group(source, at) {
                        if &source[env] == "document"
                            || (brace_depth == 0 && group_depth == 0 && conditionals.is_empty())
                        {
                            *in_preamble = false;
                        }
                        at = next;
                    }
                    group_depth += 1;
                    continue;
                }
                if executing && cmd == "end" {
                    group_depth = group_depth.saturating_sub(1);
                    if let Some((_, next)) = math::group(source, at) {
                        at = next;
                    }
                    continue;
                }
                if math::is_macro_definition_command(cmd) {
                    let next =
                        at + usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
                    if let Some((_, target_end)) = math::macro_target_name(source, next) {
                        at = target_end;
                    }
                    continue;
                }
                if executing && matches!(cmd, "input" | "include" | "usepackage" | "RequirePackage")
                {
                    let is_tex_input = matches!(cmd, "input" | "include");
                    let mut targets: Vec<(&str, &str)> = Vec::new();
                    if is_tex_input {
                        let arg_pos = skip_source_space(source, at);
                        if let Some((grp, next)) = math::group(source, arg_pos) {
                            at = next;
                            targets.push((&source[grp], "tex"));
                        } else if cmd == "input" && arg_pos < source.len() {
                            let next = arg_pos
                                + source[arg_pos..]
                                    .bytes()
                                    .take_while(|b| {
                                        !b.is_ascii_whitespace()
                                            && !matches!(b, b'%' | b'{' | b'}' | b'\\')
                                    })
                                    .count();
                            if next > arg_pos {
                                at = next;
                                targets.push((&source[arg_pos..next], "tex"));
                            }
                        }
                    } else {
                        let mut pos = at;
                        while let Some(opt_end) = optional_argument_end(source, pos) {
                            pos = opt_end;
                        }
                        if let Some((grp, mut next)) = math::group(source, pos) {
                            while let Some(opt_end) = optional_argument_end(source, next) {
                                next = opt_end;
                            }
                            at = next;
                            for pkg in source[grp].split(',') {
                                targets.push((pkg, "sty"));
                            }
                        }
                    }
                    if targets.is_empty() {
                        continue;
                    }
                    out.push_str(&source[cursor..cmd_start]);
                    out.push('\n');
                    cursor = at;
                    let child_poisoned = poisoned_scope
                        || !*in_preamble
                        || brace_depth > 0
                        || group_depth > 0
                        || !conditionals.is_empty();
                    for (raw_name, ext) in targets {
                        operation.check()?;
                        let clean_name = source_line_text(raw_name).trim().to_owned();
                        if clean_name.is_empty() || clean_name.contains('\\') {
                            continue;
                        }
                        let bases = [
                            Some(main_dir),
                            path.parent(),
                            pdf_dir,
                            sources.sidecar_dir.as_deref(),
                        ];
                        let candidates =
                            input_candidates(&clean_name, ext, &bases, compiled, sources);
                        let mut readable = Vec::new();
                        for cand in candidates {
                            match sources.read(&cand, operation) {
                                Ok(source) => readable.push((cand, source)),
                                Err(_) => operation.check()?,
                            }
                        }
                        if readable.is_empty() {
                            continue;
                        }
                        let ambiguous_cwd = readable.len() > 1;
                        for (resolved, child_src) in readable {
                            if visited.len() >= 32 || visited.contains(&resolved) {
                                continue;
                            }
                            visited.push(resolved.clone());
                            let child_index = scope.file_index(&child_src, operation);
                            append_spliced_source(
                                (&resolved, &child_src, &child_index.strict_spans),
                                main_dir,
                                compiled,
                                pdf_dir,
                                (depth + 1, child_poisoned || ambiguous_cwd, in_preamble),
                                visited,
                                sources,
                                (operation, scope),
                                out,
                            )?;
                        }
                    }
                }
            }
            _ => {
                let rest = &source.as_bytes()[at + 1..];
                let step = rest
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'{' | b'}' | b'$' | b'\\'))
                    .map_or(rest.len() + 1, |p| p + 1);
                at += step;
            }
        }
    }
    out.push_str(&source[cursor..]);
    out.push('\n');
    if poisoned_scope {
        out.push_str("}\n");
    }
    Ok(())
}

/// Title pages and running headers can be produced from declarations far
/// outside SyncTeX's reported frame. Search only literal document metadata,
/// including direct preamble inputs, and require neighboring PDF words.
#[allow(clippy::too_many_arguments)]
fn document_metadata_word_location_with_context<'q>(
    main: &Path,
    compiled_inputs: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    context: &str,
    offset: usize,
    sources: &mut QuerySources<'_>,
    operation: &'q Operation,
    scope: QueryAnalysisScope<'q>,
) -> io::Result<Option<MetadataLocation>> {
    let mut files = preamble_sources(main, compiled_inputs, pdf_dir, sources, operation)?;
    if files.is_empty() {
        return Ok(None);
    }
    let mut best = None;
    let mut best_arguments = Vec::new();
    let mut tied = false;
    for (file_index, (_, source)) in files.iter().enumerate() {
        operation.check()?;
        let analysis = scope.get(source, &[], operation);
        let mut start = 0;
        for raw_row in source.split_inclusive('\n') {
            operation.check()?;
            let row = raw_row.strip_suffix('\n').unwrap_or(raw_row);
            let row = row.strip_suffix('\r').unwrap_or(row);
            let visible = source_line_text(row);
            for command in ["\\title", "\\subtitle", "\\author", "\\date", "\\institute"] {
                for (at, _) in visible.match_indices(command) {
                    let after_cmd = start + at + command.len();
                    if source
                        .as_bytes()
                        .get(after_cmd)
                        .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == b'@')
                    {
                        continue;
                    }
                    let mut arg_at =
                        after_cmd + usize::from(source.as_bytes().get(after_cmd) == Some(&b'*'));
                    let mut spans = Vec::with_capacity(2);
                    while let Some(opt_end) = optional_argument_end(source, arg_at) {
                        let opt_start = skip_source_space(source, arg_at) + 1;
                        spans.push(opt_start..opt_end - 1);
                        arg_at = opt_end;
                    }
                    if let Some((body, end)) = math::group(source, arg_at) {
                        spans.push(body.start..end);
                    }
                    let mut cmd_best: Option<(isize, u32, usize)> = None;
                    for span in &spans {
                        let first = analysis.line_starts.row_of(span.start);
                        let last = analysis.line_starts.row_of(span.end);
                        let candidate = source_prose_scored_location_with_analysis(
                            &analysis,
                            1,
                            (context, offset),
                            first..last + 1,
                            true,
                            Some(span.clone()),
                        );
                        operation.check()?;
                        let Some((line, byte, score)) = candidate else {
                            continue;
                        };
                        if score >= 3 && cmd_best.is_none_or(|(old, _, _)| score >= old) {
                            cmd_best = Some((score, line, byte));
                        }
                    }
                    if let Some((score, line, byte)) = cmd_best {
                        match best {
                            Some((old, _, _, _)) if score < old => {}
                            Some((old, index, old_line, old_byte)) if score == old => {
                                tied |= (file_index, line, byte) != (index, old_line, old_byte);
                            }
                            _ => {
                                best = Some((score, file_index, line, byte));
                                best_arguments = spans;
                                tied = false;
                            }
                        }
                    }
                }
            }
            start += raw_row.len();
        }
    }
    operation.check()?;
    let Some((score, index, line, byte)) = best.filter(|_| !tied) else {
        return Ok(None);
    };
    let (path, source) = files.swap_remove(index);
    let Some(file) = path.to_str() else {
        return Ok(None);
    };
    Ok(Some((
        file.to_owned(),
        source,
        line,
        byte,
        score,
        best_arguments,
    )))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphicsHit {
    Unique { index: usize, count: usize },
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RawInverseCandidate {
    pub file: String,
    pub line: u32,
    pub compiler_column: Option<usize>,
    pub graphics: Option<GraphicsHit>,
}

pub(crate) fn apply_compiler_column(
    target: &mut SourceLocation,
    text: &str,
    compiler_column: usize,
) {
    if compiler_column > 1
        && let Some(prefix) = text.get(..compiler_column - 1).or_else(|| {
            let end = text
                .char_indices()
                .nth(compiler_column - 1)
                .map_or(text.len(), |(idx, _)| idx);
            (compiler_column - 1 <= text.chars().count()).then_some(&text[..end])
        })
    {
        target.byte_column = prefix.len();
        target.column = prefix.encode_utf16().count() + 1;
        target.column_char = prefix.chars().count() + 1;
    }
}
