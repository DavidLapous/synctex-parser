mod math;
mod parser;
pub mod source_snapshot;
#[cfg(test)]
mod tests;

use crate::source_snapshot::{SourceSnapshot, valid_absolute_path, valid_warning};
use parser::*;
pub use parser::{
    forward_candidates, forward_candidates_from_reader,
    forward_candidates_from_reader_with_options, forward_candidates_from_reader_with_page_hint,
    forward_candidates_in, forward_candidates_with_page_hint, inverse_candidates,
    inverse_candidates_from_reader, inverse_candidates_from_reader_with_options,
    inverse_candidates_in,
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::process::Command;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use std::{
    fs,
    io::{self, BufRead, BufReader, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchRect {
    pub bottom: f32,
    pub left: f32,
    pub top: f32,
    pub right: f32,
}

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

pub fn set_build_directory(path: Option<PathBuf>) {
    *BUILD_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) = path;
}

pub fn build_directory() -> Option<PathBuf> {
    BUILD_DIRECTORY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .or_else(|| {
            std::env::var_os("SYNCTEX_BUILD_DIRECTORY")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

pub(crate) fn synctex_candidates(
    pdf: &Path,
    build_directory: Option<&Path>,
) -> Vec<(PathBuf, bool)> {
    let mut candidates = Vec::new();
    let mut push_pair = |base: &Path| {
        for (extension, compressed) in [("synctex.gz", true), ("synctex", false)] {
            let candidate = base.with_extension(extension);
            if !candidates
                .iter()
                .any(|(existing, _)| existing == &candidate)
            {
                candidates.push((candidate, compressed));
            }
        }
        if base.extension().is_some() {
            for (suffix, compressed) in [(".synctex.gz", true), (".synctex", false)] {
                let mut os = base.as_os_str().to_os_string();
                os.push(suffix);
                let candidate = PathBuf::from(os);
                if !candidates
                    .iter()
                    .any(|(existing, _)| existing == &candidate)
                {
                    candidates.push((candidate, compressed));
                }
            }
        }
    };
    push_pair(pdf);
    let effective = build_directory
        .map(Path::to_path_buf)
        .or_else(crate::build_directory);
    if let Some(dir) = effective.as_deref()
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

pub fn locate_synctex_in(pdf: &Path, build_directory: Option<&Path>) -> Option<PathBuf> {
    synctex_candidates(pdf, build_directory)
        .into_iter()
        .find_map(|(candidate, _)| PdfRevision::read(&candidate).ok().map(|_| candidate))
}

pub fn locate_synctex(pdf: &Path) -> Option<PathBuf> {
    locate_synctex_in(pdf, None)
}

/// Metadata identity of both files observed when PDFium opens a document.
/// Stability does not prove a common build; producers must publish a completed
/// PDF/SyncTeX pair before requesting navigation.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentRevision {
    pub pdf: PdfRevision,
    companion: Option<(bool, PdfRevision)>,
}
pub(crate) fn companion_revision_in(
    path: &Path,
    build_directory: Option<&Path>,
) -> io::Result<Option<(bool, PdfRevision)>> {
    for (candidate, compressed) in synctex_candidates(path, build_directory) {
        match PdfRevision::read(&candidate) {
            Ok(revision) => return Ok(Some((compressed, revision))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

impl DocumentRevision {
    pub fn read(path: &Path) -> io::Result<Self> {
        Self::read_in(path, None)
    }
    pub fn read_in(path: &Path, build_directory: Option<&Path>) -> io::Result<Self> {
        let pdf = PdfRevision::read(path)?;
        let companion = companion_revision_in(path, build_directory)?;
        Ok(Self { pdf, companion })
    }
    pub fn check(self, path: &Path) -> io::Result<()> {
        self.check_in(path, None)
    }
    pub fn check_in(self, path: &Path, build_directory: Option<&Path>) -> io::Result<()> {
        if Self::read_in(path, build_directory)? != self {
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
                (*start == 0 || !text[..*start].ends_with('\\'))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<PathBuf>,
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
            .source_snapshot
            .as_ref()
            .is_some_and(|path| !valid_absolute_path(path))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source snapshot must be an absolute UTF-8 path",
            ));
        }
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
    pub snapshot: Option<&'a Path>,
    pub best_guess: bool,
    pub anchor_only: bool,
    pub page_hint: Option<u32>,
    pub build_directory: Option<&'a Path>,
}

/// No usable compiler geometry exists for this otherwise valid request.
/// Explicit editor best-guess commands distinguish this from operational errors.
#[derive(Debug)]
pub struct ForwardUnavailable(&'static str);

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

pub fn resolve_forward(
    pdf: &Path,
    file: &Path,
    line: u32,
    column: u32,
) -> io::Result<ForwardRequest> {
    resolve_forward_with_selector(
        pdf,
        file,
        line,
        column,
        ForwardSource::default(),
        |_, _, _, anchor| Ok(anchor),
    )
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
    let snapshot = context
        .snapshot
        .map(|path| SourceSnapshot::load_in(path, &pdf, context.build_directory))
        .transpose()?;
    let allow_anchor = context.best_guess || context.anchor_only;
    let mut document_revision = match &snapshot {
        Some(snapshot) => Some(snapshot.revision),
        None if allow_anchor => {
            drop(crate::source_snapshot::open_regular(&pdf, false)?);
            Some(DocumentRevision::read_in(&pdf, context.build_directory)?)
        }
        None => None,
    };
    if snapshot.is_none()
        && allow_anchor
        && document_revision.is_some_and(|revision| revision.companion.is_none())
    {
        return Err(io::Error::other(ForwardUnavailable(
            "no SyncTeX sidecar is available for source positioning",
        )));
    }
    let file = match &snapshot {
        Some(snapshot) => snapshot.normalize(file)?,
        None if context.anchor_only => match fs::canonicalize(file) {
            Ok(file) => file,
            Err(_) => crate::source_snapshot::normalize_absolute(file, &std::env::current_dir()?)?,
        },
        None => fs::canonicalize(file)?,
    };
    let revision = match document_revision {
        Some(revision) => revision.pdf,
        None => PdfRevision::read(&pdf)?,
    };
    // Root identity is compiler metadata, never a basename or a live-source
    // guess. A published root cache remains required even for LINE fallback.
    let (root_source, cached_root) = match &snapshot {
        Some(snapshot) => {
            let root = snapshot.read(&snapshot.main).map_err(|error| {
                if crate::source_snapshot::required_cache_error(&error) {
                    error
                } else {
                    io::Error::new(
                        error.kind(),
                        crate::source_snapshot::SnapshotReadError(error),
                    )
                }
            })?;
            let root_source = snapshot.main == file;
            (root_source, root_source.then_some(root))
        }
        None => {
            let inputs =
                crate::source_snapshot::SourceInfo::read_in(&pdf, context.build_directory)?;
            let main = inputs
                .main
                .as_ref()
                .ok_or_else(|| io::Error::other("compiler root source identity is unavailable"))?;
            if inputs.revision.pdf != revision
                || document_revision.is_some_and(|revision| revision != inputs.revision)
            {
                return Err(io::Error::other(
                    "document changed while reading compiler root identity",
                ));
            }
            document_revision = Some(inputs.revision);
            (main == &file, None)
        }
    };
    let mut warning = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.warning(&file))
        .map(str::to_owned);
    if snapshot.is_none() && context.anchor_only {
        warning = Some(
            "best-guess navigation: compiled source history unavailable; using the compiler line anchor"
                .into(),
        );
    }
    let source = match &snapshot {
        Some(_) if root_source => cached_root,
        Some(snapshot) => match snapshot.read(&file) {
            Ok(source) => Some(source),
            Err(error) if allow_anchor && !crate::source_snapshot::required_cache_error(&error) => {
                warning = Some(format!(
                    "best-guess navigation: compiled source unavailable: {error}"
                ));
                None
            }
            Err(error) => return Err(error),
        },
        None if context.anchor_only => None,
        None => Some(read_source(&file)?),
    };
    if snapshot.is_some()
        && allow_anchor
        && document_revision.is_some_and(|revision| revision.companion.is_none())
    {
        return Err(io::Error::other(ForwardUnavailable(
            "no SyncTeX sidecar is available for source positioning",
        )));
    }
    if warning.is_some() && !allow_anchor {
        return Err(io::Error::other(
            "compiled source snapshot provenance is unverified",
        ));
    }
    let original_line = match source
        .as_deref()
        .map(|source| validate_forward_source_position(source, line, column))
    {
        Some(Ok(text)) => Some(text),
        Some(Err(_))
            if context.anchor_only && source.as_deref() == Some("") && line == 1 && column == 1 =>
        {
            return Err(io::Error::other(ForwardUnavailable(
                "compiled source is empty; no printed source position is available",
            )));
        }
        Some(Err(error)) if warning.is_some() && allow_anchor => {
            warning = Some(format!(
                "best-guess navigation: compiled source position cannot be qualified: {error}"
            ));
            None
        }
        Some(Err(error)) => return Err(error),
        None => None,
    };
    let page_hint = if context.anchor_only || warning.is_some() {
        None
    } else {
        original_line.and_then(|text| ForwardWord::at(text, column))
    };
    // Collected frame/tabularx/caption bodies use their closing compiler line.
    // Keep the original source position for the page hint, and give Beamer priority.
    let view_line = source
        .as_deref()
        .and_then(|source| {
            frame_closing_line(source, line)
                .or_else(|| tabularx_closing_line(source, line))
                .or_else(|| original_line.and_then(|_| caption_closing_line(source, line, column)))
        })
        .unwrap_or(line);
    let candidates = forward_candidates_in(
        &pdf,
        &file,
        snapshot.as_ref(),
        view_line,
        context.page_hint,
        context.build_directory,
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
    let selected = if context.anchor_only || warning.is_some() {
        warning.get_or_insert_with(|| "line-only navigation: using the SyncTeX line anchor".into());
        best_guess_forward_candidate(&candidates)?
    } else {
        // Saved bytes and input identities do not trace TeX's hooks, imported
        // code, or token execution. Text similarity cannot prove source ownership.
        // Keep the legacy overlay page policy without publishing a WORD hint.
        let selected = line_candidate(page_hint.as_ref())?;
        warning = Some(
            "line-only navigation: compiled TeX execution cannot be verified; using the SyncTeX line anchor"
                .into(),
        );
        selected
    };
    let result = ForwardRequest {
        pdf,
        revision,
        page: selected.page,
        h: selected.rect.left,
        v: selected.rect.bottom,
        width: selected.rect.right - selected.rect.left,
        height: selected.rect.bottom - selected.rect.top,
        word: None,
        inverse_search: None,
        source_snapshot: snapshot.as_ref().map(|snapshot| snapshot.path.clone()),
        best_guess: context.best_guess,
        warning,
    };
    result.validate()?;
    if let Some(revision) = document_revision {
        revision.check_in(&result.pdf, context.build_directory)?;
    } else {
        revision.check(&result.pdf)?;
    }
    Ok(result)
}

fn best_guess_forward_candidate(candidates: &[ForwardCandidate]) -> io::Result<ForwardCandidate> {
    // SyncTeX orders results by its nearest source anchor. Preserve that order,
    // skipping only empty geometry; never claim a particular printed word.
    candidates
        .iter()
        .find(|candidate| {
            candidate.rect.right > candidate.rect.left || candidate.rect.bottom > candidate.rect.top
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
    source_snapshot: Option<&Path>,
    best_guess: bool,
) -> io::Result<InverseResolution> {
    resolve_inverse_in(
        pdf,
        point,
        word,
        radius,
        operation,
        source_snapshot,
        best_guess,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_inverse_in(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    source_snapshot: Option<&Path>,
    best_guess: bool,
    build_directory: Option<&Path>,
) -> io::Result<InverseResolution> {
    operation.check()?;
    if point.page == 0
        || point.page_height_pt <= 0.0
        || ![point.x, point.y_from_top, point.page_height_pt]
            .into_iter()
            .all(f32::is_finite)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inverse search requires a positive page and finite compiler coordinates",
        ));
    }
    let pdf = std::path::absolute(pdf)?;
    let snapshot = source_snapshot
        .map(|path| SourceSnapshot::load_in(path, &pdf, build_directory))
        .transpose()?;
    resolve_inverse_with_snapshot_in(
        &pdf,
        point,
        word,
        radius,
        operation,
        snapshot.as_ref(),
        best_guess,
        build_directory,
    )
}

type StructuralMapKey = (u32, PathBuf, Option<PathBuf>, Option<PathBuf>);

#[derive(Default)]
pub struct SyncTeXCache {
    entry: Option<CachedSyncTeX>,
}

struct CachedSyncTeX {
    pdf: PathBuf,
    companion: (bool, PdfRevision),
    raw: Vec<u8>,
    pages: std::collections::HashMap<u32, parser::ParsedSynctex>,
    structural_maps: std::collections::HashMap<StructuralMapKey, Result<Rc<SourceMap>, String>>,
}

impl SyncTeXCache {
    fn candidates_and_entry(
        &mut self,
        pdf: &Path,
        point: InversePoint,
        operation: &Operation,
        build_directory: Option<&Path>,
    ) -> io::Result<(
        Vec<RawInverseCandidate>,
        (bool, PdfRevision),
        &mut CachedSyncTeX,
    )> {
        operation.check()?;
        parser::validate_inverse_point(point)?;
        let Some(companion) = companion_revision_in(pdf, build_directory)? else {
            return Err(open_synctex_in(pdf, build_directory)
                .err()
                .unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "no SyncTeX companion file")
                }));
        };
        if self
            .entry
            .as_ref()
            .is_none_or(|entry| entry.pdf != pdf || entry.companion != companion)
        {
            let mut raw = Vec::new();
            open_synctex_in(pdf, build_directory)?.read_to_end(&mut raw)?;
            if raw.len() >= 128 * 1024 * 1024 {
                return Err(io::Error::other("SyncTeX source map exceeds 128 MiB"));
            }
            operation.check()?;
            if companion_revision_in(pdf, build_directory)? != Some(companion) {
                return Err(io::Error::other(
                    "displayed PDF/SyncTeX revision changed; wait for reload and click again",
                ));
            }
            self.entry = Some(CachedSyncTeX {
                pdf: pdf.to_owned(),
                companion,
                raw,
                pages: std::collections::HashMap::new(),
                structural_maps: std::collections::HashMap::new(),
            });
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

pub fn resolve_inverse_with_snapshot(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    snapshot: Option<&SourceSnapshot>,
    best_guess: bool,
) -> io::Result<InverseResolution> {
    resolve_inverse_with_cache(
        pdf, point, word, radius, operation, snapshot, best_guess, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_inverse_with_snapshot_in(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    snapshot: Option<&SourceSnapshot>,
    best_guess: bool,
    build_directory: Option<&Path>,
) -> io::Result<InverseResolution> {
    resolve_inverse_with_cache_in(
        pdf,
        point,
        word,
        radius,
        operation,
        snapshot,
        best_guess,
        None,
        build_directory,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_inverse_with_cache(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    snapshot: Option<&SourceSnapshot>,
    best_guess: bool,
    cache: Option<&mut SyncTeXCache>,
) -> io::Result<InverseResolution> {
    resolve_inverse_with_cache_in(
        pdf, point, word, radius, operation, snapshot, best_guess, cache, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_inverse_with_cache_in(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    snapshot: Option<&SourceSnapshot>,
    best_guess: bool,
    mut cache: Option<&mut SyncTeXCache>,
    build_directory: Option<&Path>,
) -> io::Result<InverseResolution> {
    operation.check()?;
    if point.page == 0
        || point.page_height_pt <= 0.0
        || ![point.x, point.y_from_top, point.page_height_pt]
            .into_iter()
            .all(f32::is_finite)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inverse search requires a positive page and finite compiler coordinates",
        ));
    }
    let pdf = std::path::absolute(pdf)?;
    if let Some(snapshot) = snapshot {
        snapshot.revision.check_in(&pdf, build_directory)?;
    }
    let raw_revision = if best_guess && snapshot.is_none() {
        drop(crate::source_snapshot::open_regular(&pdf, false)?);
        Some(DocumentRevision::read_in(&pdf, build_directory)?)
    } else {
        None
    };
    operation.check()?;
    let (targets, cached_companion, cache_entry) = match cache.as_deref_mut() {
        Some(cache) => {
            let (targets, companion, entry) =
                cache.candidates_and_entry(&pdf, point, operation, build_directory)?;
            (targets, Some(companion), Some(entry))
        }
        None => (
            parser::inverse_raw_candidates_in(&pdf, point, build_directory, Some(operation))?,
            None,
            None,
        ),
    };
    let sources = QuerySources {
        snapshot,
        best_guess,
        ..QuerySources::default()
    };
    let result = resolve_inverse_targets(
        &pdf,
        point,
        word,
        radius,
        operation,
        targets,
        sources,
        cache_entry,
        build_directory,
    )?;
    if let Some(snapshot) = snapshot {
        snapshot.revision.check_in(&pdf, build_directory)?;
    }
    if let Some(revision) = raw_revision {
        revision.check_in(&pdf, build_directory)?;
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

#[cfg(test)]
fn resolve_inverse_output(
    pdf: &Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    stdout: &str,
    sources: QuerySources<'_>,
) -> io::Result<InverseResolution> {
    let targets = parse_synctex_edit_candidates(stdout);
    resolve_inverse_targets(
        pdf, point, word, radius, operation, targets, sources, None, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_inverse_targets<'a>(
    pdf: &'a Path,
    point: InversePoint,
    word: Option<(&str, usize)>,
    radius: u32,
    operation: &Operation,
    targets: Vec<RawInverseCandidate>,
    mut sources: QuerySources<'a>,
    synctex_cache: Option<&mut CachedSyncTeX>,
    build_directory: Option<&Path>,
) -> io::Result<InverseResolution> {
    if targets.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            InverseFailure::NoMatch,
        ));
    }
    sources.pdf_dir = pdf.parent();
    sources.sidecar_dir = crate::source_snapshot::sidecar_dir_for(pdf, build_directory);
    let best_guess = sources.best_guess;
    let assume_unchanged = best_guess && sources.snapshot.is_none();
    let mut query = InverseQuery {
        pdf,
        point,
        word,
        radius,
        operation,
        sources,
        synctex_cache,
        build_directory,
        structural_map: None,
        geometry_map: None,
        metadata: None,
    };
    let mut resolutions = Vec::with_capacity(targets.len());
    for target in targets {
        operation.check()?;
        resolutions.push(query.resolve_target(target)?);
    }
    operation.check()?;
    let mut precise = resolutions
        .iter()
        .enumerate()
        .filter(|(_, (result, _))| result.location.precise);
    let mut ambiguous = None;
    let selected = if let Some((index, (supported, _))) = precise.next() {
        if precise
            .any(|(_, (other, _))| !same_source_position(&supported.location, &other.location))
        {
            ambiguous = Some("ambiguous inverse search: multiple precise source locations");
        }
        index
    } else {
        if resolutions[1..]
            .iter()
            .any(|(other, _)| !same_source_position(&resolutions[0].0.location, &other.location))
        {
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
    let (mut chosen, raw) = resolutions.swap_remove(if ambiguous.is_some() { 0 } else { selected });
    let mut warnings = std::collections::BTreeSet::new();
    if assume_unchanged {
        warnings.insert(
            "best-guess navigation: compiled source history unavailable; assuming saved source is unchanged since compilation"
                .into(),
        );
    }
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
    for (other, _) in resolutions {
        if same_source_position(&chosen.location, &other.location)
            && let Some(warning) = other.warning
        {
            warnings.insert(warning);
        }
    }
    if best_guess && !chosen.location.precise && warnings.is_empty() {
        warnings.insert("best-guess navigation: no literal source position was qualified; using the compiler line anchor".into());
    }
    operation.check()?;
    Ok(InverseResolution {
        location: chosen.location,
        warning: (!warnings.is_empty())
            .then(|| warnings.into_iter().collect::<Vec<_>>().join("; ")),
    })
}

#[derive(Default)]
struct QuerySources<'a> {
    snapshot: Option<&'a SourceSnapshot>,
    best_guess: bool,
    pdf_dir: Option<&'a Path>,
    sidecar_dir: Option<PathBuf>,
    files: std::collections::HashMap<PathBuf, Result<Rc<String>, (String, bool)>>,
}

impl QuerySources<'_> {
    fn normalize(&self, path: &Path) -> io::Result<PathBuf> {
        match self.snapshot {
            Some(snapshot) => snapshot.normalize(path),
            None => fs::canonicalize(path).or_else(|err| {
                crate::source_snapshot::resolve_relative_input(
                    path,
                    None,
                    self.pdf_dir,
                    self.sidecar_dir.as_deref(),
                )
                .ok_or(err)
            }),
        }
    }

    fn warning(&self, path: &Path) -> Option<&str> {
        self.snapshot.and_then(|snapshot| snapshot.warning(path))
    }

    fn read_error((message, required): (String, bool)) -> io::Error {
        let error = io::Error::other(message);
        if required {
            io::Error::other(crate::source_snapshot::SnapshotReadError(error))
        } else {
            error
        }
    }

    fn read(&mut self, path: &Path, operation: &Operation) -> io::Result<Rc<String>> {
        operation.check()?;
        if let Some(result) = self.files.get(path) {
            return result.clone().map_err(Self::read_error);
        }
        let result = match self.snapshot {
            Some(snapshot) => snapshot.read(path),
            None => read_source(path),
        }
        .map(Rc::new)
        .map_err(|error| {
            let required = crate::source_snapshot::required_cache_error(&error);
            (error.to_string(), required)
        });
        operation.check()?;
        let value = result.clone().map_err(Self::read_error);
        self.files.insert(path.to_owned(), result);
        value
    }
}

type MetadataLocation = (String, Rc<String>, u32, usize, isize);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InverseFailure {
    #[error("synctex edit returned no match")]
    NoMatch,
    #[error("{0}")]
    Ambiguous(&'static str),
    #[error("invalid source anchor: {file}:{line} is out of range (available lines: {available})")]
    InvalidSourceAnchor {
        file: PathBuf,
        line: u32,
        available: usize,
    },
}

pub fn inverse_failure(error: &io::Error) -> Option<&InverseFailure> {
    error.get_ref()?.downcast_ref::<InverseFailure>()
}

struct InverseAnchor {
    target: SourceLocation,
    compiler_column: Option<usize>,
    generated: bool,
    warning: Option<String>,
    source_map: Option<io::Result<Rc<SourceMap>>>,
}

fn is_generated_sidecar(path: &Path, pdf: &Path) -> bool {
    path.file_stem() == pdf.file_stem() && crate::source_snapshot::is_compiler_sidecar_input(path)
}

struct InverseQuery<'a> {
    pdf: &'a Path,
    point: InversePoint,
    word: Option<(&'a str, usize)>,
    radius: u32,
    operation: &'a Operation,
    sources: QuerySources<'a>,
    synctex_cache: Option<&'a mut CachedSyncTeX>,
    build_directory: Option<&'a Path>,
    structural_map: Option<(PathBuf, Result<Rc<SourceMap>, String>)>,
    geometry_map: Option<Result<Rc<SourceMap>, String>>,
    metadata: Option<Option<MetadataLocation>>,
}

impl InverseQuery<'_> {
    fn source_map(&mut self, generated: Option<&Path>) -> io::Result<Rc<SourceMap>> {
        self.operation.check()?;
        let snapshot = self.sources.snapshot;
        let current_dir = if self.sources.best_guess && snapshot.is_none() {
            Some(std::env::current_dir()?)
        } else {
            None
        };
        let input_cwd = snapshot
            .map(|snapshot| snapshot.cwd.as_path())
            .or(current_dir.as_deref());
        let sidecar_dir = self.sources.sidecar_dir.as_deref();
        let default_vrb = self.pdf.with_extension("vrb");
        let generated_path = generated.unwrap_or(&default_vrb);
        let result = if let Some(generated) = generated {
            if generated == default_vrb
                && let Some(geometry) = &self.geometry_map
            {
                geometry.clone()
            } else if let Some(cache) = self.synctex_cache.as_deref_mut() {
                let key = (
                    self.point.page,
                    generated.to_owned(),
                    input_cwd.map(Path::to_owned),
                    snapshot.map(|s| s.path.clone()),
                );
                if let Some(existing) = cache.structural_maps.get(&key) {
                    existing.clone()
                } else {
                    let loaded = source_map_from_reader_in(
                        std::io::Cursor::new(&cache.raw),
                        self.pdf,
                        self.point,
                        generated_path,
                        false,
                        self.operation,
                        input_cwd,
                        snapshot,
                        sidecar_dir,
                    )
                    .map(Rc::new)
                    .map_err(|error| error.to_string());
                    self.operation.check()?;
                    if cache.structural_maps.len() >= 32 {
                        cache.structural_maps.clear();
                    }
                    cache.structural_maps.insert(key, loaded.clone());
                    loaded
                }
            } else {
                if self
                    .structural_map
                    .as_ref()
                    .is_none_or(|(cached, _)| cached != generated)
                {
                    let loaded = read_source_map_in(
                        self.pdf,
                        self.point,
                        generated_path,
                        false,
                        self.operation,
                        input_cwd,
                        snapshot,
                        self.build_directory,
                    )
                    .map(Rc::new)
                    .map_err(|error| error.to_string());
                    self.structural_map = Some((generated.to_owned(), loaded));
                }
                self.structural_map.as_ref().unwrap().1.clone()
            }
        } else {
            if self.geometry_map.is_none() {
                if let Some((cached, Ok(map))) = &self.structural_map
                    && cached == &default_vrb
                    && map.page_source.is_none()
                {
                    self.geometry_map = Some(Ok(Rc::clone(map)));
                } else if let Some(cache) = self.synctex_cache.as_deref()
                    && let Some(Ok(map)) = cache.structural_maps.get(&(
                        self.point.page,
                        default_vrb.clone(),
                        input_cwd.map(Path::to_owned),
                        snapshot.map(|s| s.path.clone()),
                    ))
                    && map.page_source.is_none()
                {
                    self.geometry_map = Some(Ok(Rc::clone(map)));
                }
            }
            let pdf = self.pdf;
            let point = self.point;
            let operation = self.operation;
            let build_directory = self.build_directory;
            let raw = self
                .synctex_cache
                .as_deref()
                .map(|cache| cache.raw.as_slice());
            self.geometry_map
                .get_or_insert_with(|| {
                    let loaded = match raw {
                        Some(bytes) => source_map_from_reader_in(
                            std::io::Cursor::new(bytes),
                            pdf,
                            point,
                            generated_path,
                            true,
                            operation,
                            input_cwd,
                            snapshot,
                            sidecar_dir,
                        ),
                        None => read_source_map_in(
                            pdf,
                            point,
                            generated_path,
                            true,
                            operation,
                            input_cwd,
                            snapshot,
                            build_directory,
                        ),
                    };
                    loaded.map(Rc::new).map_err(|error| error.to_string())
                })
                .clone()
        };
        self.operation.check()?;
        result.map_err(io::Error::other)
    }

    fn resolve_target(
        &mut self,
        target: RawInverseCandidate,
    ) -> io::Result<(InverseResolution, Option<SourceLocation>)> {
        let mut anchor = self.normalize_anchor(target)?;
        let Some(source) = self.qualify_source(&mut anchor)? else {
            return Ok((
                InverseResolution {
                    location: anchor.target,
                    warning: anchor.warning,
                },
                None,
            ));
        };
        self.refine_target(anchor, source)
    }

    fn normalize_anchor(&mut self, raw: RawInverseCandidate) -> io::Result<InverseAnchor> {
        let path = Path::new(&raw.file);
        let pdf_dir = self.pdf.parent().unwrap_or(Path::new("."));
        let sidecar_dir = self.sources.sidecar_dir.as_deref();
        let mut warning = None;
        let resolved = if let Some(snapshot) = self.sources.snapshot {
            snapshot.normalize(path)?.components().collect()
        } else if self.sources.best_guess {
            let absolute: PathBuf = std::path::absolute(path)?.components().collect();
            if !path.is_absolute() && fs::symlink_metadata(&absolute).is_err() {
                crate::source_snapshot::resolve_relative_input(
                    path,
                    None,
                    Some(pdf_dir),
                    sidecar_dir,
                )
                .unwrap_or(absolute)
            } else {
                absolute
            }
        } else {
            let current_dir = std::env::current_dir()
                .ok()
                .and_then(|dir| fs::canonicalize(&dir).ok().or(Some(dir)));
            if !path.is_absolute()
                && let Some(found) = crate::source_snapshot::resolve_relative_input(
                    path,
                    current_dir.as_deref(),
                    Some(pdf_dir),
                    sidecar_dir,
                )
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
        if self.sources.best_guess
            && let Some(Err(error)) = &source_map
        {
            self.operation.check()?;
            return Err(io::Error::other(error.to_string()));
        }
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
            generated,
            warning,
            source_map,
        })
    }

    // Authority and bounds are settled before word ownership. A coarse result
    // carries its compiler anchor unchanged; only a qualified source is refined.
    fn qualify_source(&mut self, anchor: &mut InverseAnchor) -> io::Result<Option<Rc<String>>> {
        let target = &mut anchor.target;
        let generated = anchor.generated;
        let source_map = &anchor.source_map;
        let warning = &mut anchor.warning;
        let raw_column = anchor.compiler_column;
        target.byte_column = 0;
        target.column = 1;
        target.column_char = 1;
        if !generated
            && let Some(reason) = self
                .sources
                .warning(Path::new(&target.file))
                .map(str::to_owned)
        {
            // A provenance warning cannot waive authority for a recorded input.
            // Uncaptured inputs still retain their original coarse anchor.
            if let Err(error) = self.sources.read(Path::new(&target.file), self.operation)
                && crate::source_snapshot::required_cache_error(&error)
            {
                return Err(error);
            }
            self.operation.check()?;
            *warning = Some(reason);
            return Ok(None);
        }
        let source = if generated {
            let Some(source) = self.remap_generated_source(target, source_map, warning)? else {
                return Ok(None);
            };
            source
        } else {
            match self.sources.read(Path::new(&target.file), self.operation) {
                Ok(source) => source,
                Err(error) => {
                    self.operation.check()?;
                    if crate::source_snapshot::required_cache_error(&error) {
                        return Err(error);
                    }
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
        if let Some(reason) = self.sources.warning(Path::new(&target.file)) {
            *warning = Some(reason.to_owned());
            return Ok(None);
        }
        if let Some(column) = raw_column {
            apply_compiler_column(target, text, column);
        }
        Ok(Some(source))
    }

    // The structural sheet anchor must identify the enclosing source frame for
    // generated sidecars (.vrb, .toc, .nav, .out, .aux, .snm).
    fn remap_generated_source(
        &mut self,
        target: &mut SourceLocation,
        source_map: &Option<io::Result<Rc<SourceMap>>>,
        warning: &mut Option<String>,
    ) -> io::Result<Option<Rc<String>>> {
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
                    original_source_from_verbatim(map, &mut self.sources, self.operation)
                } else {
                    original_source_from_generated(map, &mut self.sources, self.operation)
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
                if crate::source_snapshot::required_cache_error(&error) {
                    return Err(error);
                }
                if self.sources.best_guess
                    && let Some(map) = source_map.as_ref().and_then(|map| map.as_ref().ok())
                {
                    let (original, line) = map
                        .page_source
                        .as_ref()
                        .map(|(path, line)| (path.as_path(), *line))
                        .unwrap_or((&map.main, 1));
                    if line == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SyncTeX original compiler anchor line must be positive",
                        ));
                    }
                    let original = if let Some(snapshot) = self.sources.snapshot {
                        snapshot.normalize(original)?
                    } else {
                        fs::canonicalize(original).unwrap_or_else(|_| original.to_owned())
                    };
                    target.file = original
                        .into_os_string()
                        .into_string()
                        .map_err(|_| io::Error::other("source path is not UTF-8"))?;
                    target.line = line;
                    self.operation.check()?;
                }
                *warning = Some(format!(
                    "line-only navigation: original fragile frame unavailable: {error}"
                ));
                Ok(None)
            }
        }
    }

    fn refine_target(
        &mut self,
        anchor: InverseAnchor,
        mut source: Rc<String>,
    ) -> io::Result<(InverseResolution, Option<SourceLocation>)> {
        let InverseAnchor {
            mut target,
            generated,
            mut warning,
            mut source_map,
            ..
        } = anchor;
        let anchor = (self.sources.best_guess && self.word.is_some()).then(|| target.clone());
        if let Some((context, offset)) = self.word {
            let mut target_frame = source_frame_range(&source, target.line);
            let fragile_frame = target_frame
                .as_ref()
                .filter(|frame| is_fragile_frame(&source, frame))
                .cloned();
            if !generated {
                // Ordinary source refinement and document metadata need only
                // structural provenance, not fragile-body geometry.
                let default_vrb = self.pdf.with_extension("vrb");
                source_map = Some(self.source_map(fragile_frame.is_none().then_some(&default_vrb)));
            }
            if self.sources.best_guess
                && let Some(Err(error)) = &source_map
            {
                self.operation.check()?;
                return Err(io::Error::other(error.to_string()));
            }
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
            let mut location = if scope_proven {
                source_word_location(&source, target.line, context, offset, self.radius)
            } else {
                source_word_location_in_range(
                    &source,
                    target.line,
                    context,
                    offset,
                    target.line.saturating_sub(1) as usize..target.line as usize,
                    false,
                )
            };
            if location.is_none() && !generated {
                if fragile_frame.is_none()
                    && source_map
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .is_some_and(|map| map.page_source.is_some())
                {
                    source_map = Some(self.source_map(None));
                    if self.sources.best_guess
                        && let Some(Err(error)) = &source_map
                    {
                        self.operation.check()?;
                        return Err(io::Error::other(error.to_string()));
                    }
                }
                if let Some(map) = source_map
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .filter(|map| map.generated_at_point && map.page_source.is_some())
                {
                    match original_source_from_verbatim(map, &mut self.sources, self.operation) {
                        Ok((file, original, anchor)) => {
                            if self.sources.warning(Path::new(&file)).is_none()
                                && let Some(candidate) = source_word_location(
                                    &original,
                                    anchor,
                                    context,
                                    offset,
                                    self.radius,
                                )
                            {
                                target.file = file;
                                target.line = anchor;
                                source = original;
                                target_frame = source_frame_range(&source, target.line);
                                location = Some(candidate);
                            }
                        }
                        Err(error) => {
                            self.operation.check()?;
                            if crate::source_snapshot::required_cache_error(&error) {
                                return Err(error);
                            }
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
            if !generated
                && let Some(map) = source_map.as_ref().and_then(|result| result.as_ref().ok())
                && if location.is_some() {
                    target_frame.is_some()
                } else {
                    allows_metadata_fallback(
                        &source,
                        target.line,
                        self.radius,
                        self.point.y_from_top,
                        self.point.page_height_pt,
                    )
                }
            {
                if self.metadata.is_none() {
                    self.metadata = Some(document_metadata_word_location(
                        &map.main,
                        context,
                        offset,
                        &mut self.sources,
                        self.operation,
                    )?);
                }
                if let Some((file, original, line, byte, score)) =
                    self.metadata.as_ref().and_then(Option::as_ref)
                    && (location.is_none()
                        || *score >= 6
                            && metadata_beats_frame(
                                &source,
                                target.line,
                                context,
                                offset,
                                self.point.y_from_top,
                                self.point.page_height_pt,
                                *score,
                            ))
                {
                    target.file = file.clone();
                    source = Rc::clone(original);
                    location = Some((*line, *byte));
                }
            }
            if let Some((line, byte)) = location {
                let text = validate_inverse_source_line(&source, line, Path::new(&target.file))?;
                let prefix = text
                    .get(..byte)
                    .ok_or_else(|| io::Error::other("source column is not a UTF-8 boundary"))?;
                target.line = line;
                target.byte_column = byte;
                target.column = prefix.encode_utf16().count() + 1;
                target.column_char = prefix.chars().count() + 1;
                target.precise = true;
            }
        }
        self.operation.check()?;
        Ok((
            InverseResolution {
                location: target,
                warning,
            },
            anchor,
        ))
    }
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

fn validate_forward_source_position(source: &str, line: u32, column: u32) -> io::Result<&str> {
    let text = validate_source_line(source, line)?;
    if column == 0 || column as usize > text.chars().count() + 1 {
        return Err(io::Error::other("source scalar column is out of range"));
    }
    Ok(text)
}

fn allows_metadata_fallback(
    source: &str,
    line: u32,
    radius: u32,
    y_from_top: f32,
    page_height_pt: f32,
) -> bool {
    if source_frame_range(source, line).is_some() {
        return true;
    }
    if page_height_pt > 0.0
        && (y_from_top <= page_height_pt * 0.12 || y_from_top >= page_height_pt * 0.88)
    {
        return true;
    }
    let start = line.saturating_sub(radius.saturating_add(1)) as usize;
    let end = (line as usize).saturating_add(radius as usize);
    source
        .lines()
        .skip(start)
        .take(end.saturating_sub(start))
        .any(|row| {
            let text = source_line_text(row);
            text.contains("\\maketitle") || text.contains("\\titlepage")
        })
}

/// A frame title and the running document title may contain the same words.
/// Preserve a local frame hit except when a footer has stronger metadata context.
fn metadata_beats_frame(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    y_from_top: f32,
    page_height_pt: f32,
    metadata_score: isize,
) -> bool {
    let Some(frame) = source_frame_range(source, line) else {
        return false;
    };
    y_from_top >= page_height_pt / 2.0
        && source_prose_scored_location(
            source,
            line,
            context,
            offset,
            frame,
            true,
            &nonprinting_source_ranges(source),
        )
        .is_some_and(|(_, _, local_score)| metadata_score >= local_score)
}

fn read_source(path: &Path) -> io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    read_source_file(file)
}

pub(crate) fn read_source_file(file: fs::File) -> io::Result<String> {
    let mut bytes = Vec::new();
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "source refinement requires a regular file",
        ));
    }
    file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(io::Error::other("source exceeds 2 MiB refinement limit"));
    }
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

struct SourceMap {
    main: PathBuf,
    page_source: Option<(PathBuf, u32)>,
    generated_at_point: bool,
}

#[derive(Clone, Copy)]
struct SourceBox {
    bounds: [f64; 4],
    generated: bool,
}

fn source_box_bounds(row: &str, position: [f64; 2]) -> io::Result<Option<[f64; 4]>> {
    let parse = || {
        let size = row.split(':').nth(2)?;
        let mut size = size.split(',');
        let [width, height, depth] = [size.next()?, size.next()?, size.next()?]
            .map(|value| value.parse::<f64>().ok().filter(|value| value.is_finite()));
        Some([position[0], position[1], width?, height?, depth?])
    };
    let [x, y, width, height, depth] =
        parse().ok_or_else(|| io::Error::other("SyncTeX horizontal box has invalid geometry"))?;
    Ok((width > 0.0 && height + depth > 0.0).then_some([x, y - height, x + width, y + depth]))
}

fn source_map_position(row: &str, last_v: &mut f64) -> io::Result<[f64; 2]> {
    let position = row
        .split(':')
        .nth(1)
        .and_then(|position| position.split_once(','))
        .ok_or_else(|| io::Error::other("SyncTeX node has invalid position"))?;
    let x = source_map_number(position.0)?;
    let y = if position.1 == "=" {
        *last_v
    } else {
        let y = source_map_number(position.1)?;
        *last_v = y;
        y
    };
    Ok([x, y])
}

fn generated_box_at_point(
    boxes: &[SourceBox],
    x: f64,
    y: f64,
    operation: &Operation,
) -> io::Result<bool> {
    let contains =
        |a: [f64; 4], b: [f64; 4]| a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3];
    let mut nearest: Vec<SourceBox> = Vec::new();
    for candidate in boxes {
        operation.check()?;
        if !contains(candidate.bounds, [x, y, x, y]) {
            continue;
        }
        if let Some(equal) = nearest
            .iter_mut()
            .find(|old| old.bounds == candidate.bounds)
        {
            equal.generated &= candidate.generated;
        } else if !nearest
            .iter()
            .any(|old| contains(candidate.bounds, old.bounds))
        {
            nearest.retain(|old| !contains(old.bounds, candidate.bounds));
            nearest.push(*candidate);
        }
    }
    // Conflicting nonnested overlaps and equal boxes with mixed ownership abstain.
    Ok(nearest.len() == 1 && nearest[0].generated)
}

fn source_map_number(value: &str) -> io::Result<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| io::Error::other("SyncTeX transform has an invalid number"))
}

fn source_map_dimension(value: &str) -> io::Result<f64> {
    let units = [
        ("in", 72.27 * 65536.0),
        ("cm", 72.27 * 65536.0 / 2.54),
        ("mm", 72.27 * 65536.0 / 25.4),
        ("pt", 65536.0),
        ("bp", 72.27 / 72.0 * 65536.0),
        ("pc", 12.0 * 65536.0),
        ("sp", 1.0),
        ("dd", 1238.0 / 1157.0 * 65536.0),
        ("cc", 14856.0 / 1157.0 * 65536.0),
        ("nd", 685.0 / 642.0 * 65536.0),
        ("nc", 1370.0 / 107.0 * 65536.0),
    ];
    for (suffix, scale) in units {
        if let Some(number) = value.trim().strip_suffix(suffix) {
            return source_map_number(number).map(|number| number * scale);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "SyncTeX post dimension has an invalid unit",
    ))
}

pub(crate) fn decode_synctex_file(file: fs::File, limit: u64) -> io::Result<Box<dyn BufRead>> {
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    if reader.fill_buf()?.starts_with(&[0x1f, 0x8b]) {
        Ok(Box::new(BufReader::with_capacity(
            64 * 1024,
            flate2::read::GzDecoder::new(reader).take(limit),
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
    for (candidate, _) in synctex_candidates(pdf, build_directory) {
        match crate::source_snapshot::open_regular(&candidate, false) {
            Ok(file) => return Ok((decode_synctex_file(file, 128 * 1024 * 1024 + 1)?, candidate)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => last_not_found = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_not_found
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no SyncTeX companion file")))
}

pub(crate) fn open_synctex_in(
    pdf: &Path,
    build_directory: Option<&Path>,
) -> io::Result<Box<dyn BufRead>> {
    open_synctex_with_path_in(pdf, build_directory).map(|(reader, _)| reader)
}

fn synctex_node_link(row: &str) -> Option<(u32, u32)> {
    let (link, _) = row.get(1..)?.split_once(':')?;
    let (tag, rest) = link.split_once(',')?;
    Some((
        tag.parse::<u32>().ok()?,
        rest.split(',').next()?.parse::<u32>().ok()?,
    ))
}

#[allow(clippy::too_many_arguments)]
fn read_source_map_in(
    pdf: &Path,
    point: InversePoint,
    generated: &Path,
    geometry_required: bool,
    operation: &Operation,
    input_cwd: Option<&Path>,
    snapshot: Option<&SourceSnapshot>,
    build_directory: Option<&Path>,
) -> io::Result<SourceMap> {
    let (reader, sidecar_path) = open_synctex_with_path_in(pdf, build_directory)?;
    source_map_from_reader_in(
        reader,
        pdf,
        point,
        generated,
        geometry_required,
        operation,
        input_cwd,
        snapshot,
        sidecar_path.parent(),
    )
}

#[cfg(test)]
fn source_map_from_reader(
    reader: impl BufRead,
    pdf: &Path,
    point: InversePoint,
    generated: &Path,
    geometry_required: bool,
    operation: &Operation,
    input_cwd: Option<&Path>,
) -> io::Result<SourceMap> {
    source_map_from_reader_in(
        reader,
        pdf,
        point,
        generated,
        geometry_required,
        operation,
        input_cwd,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn source_map_from_reader_in(
    reader: impl BufRead,
    pdf: &Path,
    point: InversePoint,
    generated: &Path,
    geometry_required: bool,
    operation: &Operation,
    input_cwd: Option<&Path>,
    snapshot: Option<&SourceSnapshot>,
    sidecar_dir: Option<&Path>,
) -> io::Result<SourceMap> {
    let mut reader = reader.take(128 * 1024 * 1024 + 1);
    let mut inputs = std::collections::HashMap::new();
    let mut generated_tags = std::collections::HashSet::new();
    let mut row = String::new();
    let mut in_page = false;
    let mut page_complete = false;
    let mut origin = None;
    let mut generated_on_page = false;
    let mut page_tags = std::collections::HashSet::new();
    let mut stack: Vec<(u8, Option<[f64; 4]>, bool)> = Vec::new();
    let mut boxes = Vec::new();
    let mut defined_forms = std::collections::HashSet::new();
    let mut sheet_form_refs = std::collections::HashSet::new();
    let mut unit = None;
    let mut magnification = None;
    let mut x_offset = None;
    let mut y_offset = None;
    let mut post_magnification = 1.0;
    let mut post_x_offset = None;
    let mut post_y_offset = None;
    let mut post_scriptum = false;
    let mut postamble = false;
    let mut postamble_count = false;
    let mut bytes_read = 0;
    let mut lines = 0usize;
    let mut last_v = -1.0;
    loop {
        if lines & 1023 == 0 {
            operation.check()?;
        }
        lines = lines.wrapping_add(1);
        row.clear();
        let count = reader.read_line(&mut row)?;
        bytes_read += count;
        if bytes_read >= 128 * 1024 * 1024 {
            return Err(io::Error::other("SyncTeX source map exceeds 128 MiB"));
        }
        if count == 0 {
            break;
        }
        let row = if input_cwd.is_some() {
            row.trim_end_matches(['\r', '\n'])
        } else {
            row.trim_end()
        };
        if (geometry_required || input_cwd.is_some()) && row == "Postamble:" {
            postamble = true;
        } else if (geometry_required || input_cwd.is_some())
            && postamble
            && let Some(value) = row.strip_prefix("Count:")
        {
            value
                .parse::<u32>()
                .map_err(|_| io::Error::other("SyncTeX postamble has invalid Count"))?;
            postamble_count = true;
        }
        if geometry_required && row == "Post scriptum:" {
            post_scriptum = true;
        } else if geometry_required && let Some(value) = row.strip_prefix("Unit:") {
            unit = Some(source_map_number(value)?);
        } else if geometry_required && let Some(value) = row.strip_prefix("Magnification:") {
            let value = source_map_number(value)?;
            if post_scriptum {
                post_magnification = value;
            } else {
                magnification = Some(value);
            }
        } else if geometry_required && let Some(value) = row.strip_prefix("X Offset:") {
            if post_scriptum {
                post_x_offset = Some(source_map_dimension(value)?);
            } else {
                x_offset = Some(source_map_number(value)?);
            }
        } else if geometry_required && let Some(value) = row.strip_prefix("Y Offset:") {
            if post_scriptum {
                post_y_offset = Some(source_map_dimension(value)?);
            } else {
                y_offset = Some(source_map_number(value)?);
            }
        }
        // Compressed ",=" reuses the scanner's global last vertical value,
        // including records from earlier sheets and form definitions.
        // Later positions cannot affect this query's completed sheet, but the
        // remaining input tags, transforms, and postamble still require a scan.
        let position = if geometry_required
            && !page_complete
            && matches!(
                row.as_bytes().first(),
                Some(b'[' | b'(' | b'v' | b'h' | b'x' | b'k' | b'g' | b'r' | b'$' | b'f')
            ) {
            Some(source_map_position(row, &mut last_v)?)
        } else {
            None
        };
        if geometry_required && let Some(tag) = row.strip_prefix('<') {
            let tag = tag
                .parse::<u32>()
                .ok()
                .filter(|tag| *tag > 0)
                .ok_or_else(|| io::Error::other("SyncTeX form has an invalid tag"))?;
            defined_forms.insert(tag);
        }
        if let Some(input) = row.strip_prefix("Input:") {
            if let Some((tag, path)) = input.split_once(':')
                && let Ok(tag) = tag.parse::<u32>()
            {
                let path = Path::new(path);
                if input_cwd.is_some() && (tag == 0 || path.as_os_str().is_empty()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid SyncTeX input declaration",
                    ));
                }
                let path = if path.is_absolute() {
                    if input_cwd.is_some() && !valid_absolute_path(path) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid SyncTeX input path",
                        ));
                    }
                    path.to_owned()
                } else if let Some(cwd) = input_cwd {
                    let joined = cwd.join(path);
                    if !valid_absolute_path(&joined) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid SyncTeX input path",
                        ));
                    }
                    if let Some(snapshot) = snapshot {
                        snapshot.normalize(path)?
                    } else if fs::symlink_metadata(&joined).is_err() {
                        crate::source_snapshot::resolve_relative_input(
                            path,
                            None,
                            pdf.parent(),
                            sidecar_dir,
                        )
                        .unwrap_or(joined)
                    } else {
                        joined
                    }
                } else {
                    let current_dir = std::env::current_dir()
                        .ok()
                        .and_then(|dir| fs::canonicalize(&dir).ok().or(Some(dir)));
                    crate::source_snapshot::resolve_relative_input(
                        path,
                        current_dir.as_deref(),
                        pdf.parent(),
                        sidecar_dir,
                    )
                    .unwrap_or_else(|| pdf.parent().unwrap_or(Path::new(".")).join(path))
                };
                // TeX can emit .vrb in the build cwd while the PDF is in a
                // separate output directory. Jobname is the map identity;
                // the final generated file is neither read nor trusted.
                if path.file_name() == generated.file_name() {
                    generated_tags.insert(tag);
                }
                match inputs.entry(tag) {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if input_cwd.is_some() && entry.get() != &path {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "conflicting SyncTeX input tag",
                            ));
                        }
                        if input_cwd.is_none() {
                            entry.insert(path);
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(path);
                    }
                }
            } else if input_cwd.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed SyncTeX input declaration",
                ));
            }
        } else if let Some(page) = row.strip_prefix('{') {
            in_page = page.parse::<u32>() == Ok(point.page);
            if in_page && page_complete {
                return Err(io::Error::other(
                    "SyncTeX source map repeats the clicked sheet",
                ));
            }
        } else if in_page && row.starts_with('}') {
            if row[1..].parse::<u32>() != Ok(point.page) || !stack.is_empty() {
                return Err(io::Error::other(
                    "SyncTeX source map has mismatched sheet bounds",
                ));
            }
            page_complete = true;
            in_page = false;
            if !geometry_required && input_cwd.is_none() {
                operation.check()?;
                let main = inputs
                    .get(&1)
                    .cloned()
                    .ok_or_else(|| io::Error::other("SyncTeX source map has no main input"))?;
                let page_source = origin
                    .filter(|_| generated_on_page)
                    .and_then(|(tag, line)| inputs.remove(&tag).map(|path| (path, line)));
                return Ok(SourceMap {
                    main,
                    page_source,
                    generated_at_point: false,
                });
            }
        } else if in_page {
            if geometry_required && matches!(row.as_bytes().first(), Some(b'<' | b'>')) {
                return Err(io::Error::other(
                    "SyncTeX fragile body ownership cannot resolve sheet form transforms",
                ));
            }
            if geometry_required && let Some(reference) = row.strip_prefix('f') {
                let tag = reference
                    .split_once(':')
                    .and_then(|(tag, _)| tag.parse::<u32>().ok())
                    .filter(|tag| *tag > 0)
                    .ok_or_else(|| io::Error::other("SyncTeX form reference has an invalid tag"))?;
                sheet_form_refs.insert(tag);
            }
            let link = synctex_node_link(row);
            // Only the first box enclosing the whole sheet proves provenance.
            if origin.is_none() {
                if !matches!(row.as_bytes().first(), Some(b'[' | b'(')) || link.is_none() {
                    return Err(io::Error::other(
                        "SyncTeX sheet has no enclosing source box",
                    ));
                }
                origin = link;
            }
            let generated_node = link.is_some_and(|(tag, _)| generated_tags.contains(&tag));
            if geometry_required {
                match row.as_bytes().first() {
                    Some(kind @ (b'[' | b'(')) => {
                        let bounds = if *kind == b'(' {
                            source_box_bounds(
                                row,
                                position.ok_or_else(|| {
                                    io::Error::other("SyncTeX horizontal box has no position")
                                })?,
                            )?
                        } else {
                            None
                        };
                        stack.push((*kind, bounds, generated_node));
                    }
                    Some(b']' | b')') => {
                        let (kind, bounds, generated) = stack.pop().ok_or_else(|| {
                            io::Error::other("SyncTeX sheet has an unmatched box close")
                        })?;
                        if (kind == b'(') != row.starts_with(')') {
                            return Err(io::Error::other(
                                "SyncTeX sheet has mismatched box bounds",
                            ));
                        }
                        if let Some(bounds) = bounds {
                            boxes.push(SourceBox { bounds, generated });
                        }
                        if let Some(parent) = stack.last_mut() {
                            parent.2 |= generated;
                        }
                    }
                    _ => {
                        if generated_node && let Some(parent) = stack.last_mut() {
                            parent.2 = true;
                        }
                    }
                }
            }
            if matches!(
                row.as_bytes().first(),
                Some(b'[' | b'(' | b'v' | b'h' | b'k' | b'g' | b'x' | b'$' | b'r')
            ) && let Some((tag, _)) = link
            {
                generated_on_page |= generated_tags.contains(&tag);
                if input_cwd.is_some() && !geometry_required {
                    page_tags.insert(tag);
                }
            }
        }
    }
    operation.check()?;
    if !page_complete || !postamble_count {
        return Err(io::Error::other("SyncTeX source map is incomplete"));
    }
    if !geometry_required {
        generated_on_page |= page_tags.iter().any(|tag| generated_tags.contains(tag));
        let main = inputs
            .get(&1)
            .cloned()
            .ok_or_else(|| io::Error::other("SyncTeX source map has no main input"))?;
        let page_source = origin
            .filter(|_| generated_on_page)
            .and_then(|(tag, line)| inputs.remove(&tag).map(|path| (path, line)));
        return Ok(SourceMap {
            main,
            page_source,
            generated_at_point: false,
        });
    }
    // With form recording disabled, TeX still emits sheet references whose
    // definitions are absent. The native SyncTeX parser removes those refs:
    // they carry no source ownership. Do not invent geometry or source tags.
    if sheet_form_refs
        .iter()
        .any(|tag| defined_forms.contains(tag))
    {
        return Err(io::Error::other(
            "SyncTeX fragile body ownership cannot resolve recorded sheet form transforms",
        ));
    }
    // Match SyncTeX's effective preamble/Post scriptum transform. The offsets
    // are not magnified; post offsets are dimensions in scaled TeX points.
    let unit = unit
        .filter(|unit| *unit > 0.0)
        .ok_or_else(|| io::Error::other("SyncTeX source map has no positive Unit"))?;
    let magnification = magnification
        .filter(|magnification| *magnification > 0.0)
        .ok_or_else(|| io::Error::other("SyncTeX source map has no positive Magnification"))?;
    if post_magnification <= 0.0 {
        return Err(io::Error::other(
            "SyncTeX post magnification is not positive",
        ));
    }
    let pre_unit = unit / 65781.76;
    let visible_unit = pre_unit * magnification / 1000.0 * post_magnification;
    let (x_offset, y_offset) = if let Some(x) = post_x_offset {
        let y = post_y_offset
            .ok_or_else(|| io::Error::other("SyncTeX post X Offset has no Y Offset"))?;
        (x / 65781.76, y / 65781.76)
    } else {
        (
            x_offset.ok_or_else(|| io::Error::other("SyncTeX source map has no X Offset"))?
                * pre_unit,
            y_offset.ok_or_else(|| io::Error::other("SyncTeX source map has no Y Offset"))?
                * pre_unit,
        )
    };
    if !visible_unit.is_finite()
        || visible_unit <= 0.0
        || !x_offset.is_finite()
        || !y_offset.is_finite()
    {
        return Err(io::Error::other(
            "SyncTeX effective transform is not finite",
        ));
    }
    let main = inputs
        .get(&1)
        .cloned()
        .ok_or_else(|| io::Error::other("SyncTeX source map has no main input"))?;
    let page_source = origin
        .filter(|_| generated_on_page)
        .and_then(|(tag, line)| inputs.remove(&tag).map(|path| (path, line)));
    let generated_at_point = generated_box_at_point(
        &boxes,
        (f64::from(point.x) - x_offset) / visible_unit,
        (f64::from(point.y_from_top) - y_offset) / visible_unit,
        operation,
    )?;
    Ok(SourceMap {
        main,
        page_source,
        generated_at_point,
    })
}

/// Beamer reopens the same .vrb for successive fragile frames. The clicked
/// sheet's enclosing source-map box identifies the original frame; the final
/// generated file's contents cannot establish that identity.
fn original_source_from_verbatim(
    map: &SourceMap,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<(String, Rc<String>, u32)> {
    let (file, source, line) = original_source_from_generated(map, sources, operation)?;
    fragile_frame_range(&source, line)
        .filter(|frame| line == frame.end as u32)
        .ok_or_else(|| {
            io::Error::other("clicked sheet does not identify a fragile frame's closing line")
        })?;
    Ok((file, source, line))
}

fn original_source_from_generated(
    map: &SourceMap,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<(String, Rc<String>, u32)> {
    let (original, line) = map.page_source.as_ref().ok_or_else(|| {
        io::Error::other("clicked sheet has no original generated-file provenance")
    })?;
    let original = sources.normalize(original)?;
    let source = sources.read(&original, operation)?;
    validate_inverse_source_line(&source, *line, &original)?;
    let file = original
        .to_str()
        .ok_or_else(|| io::Error::other("original fragile source path is not UTF-8"))?
        .to_owned();
    Ok((file, source, *line))
}

fn fragile_frame_range(source: &str, line: u32) -> Option<std::ops::Range<usize>> {
    source_frame_range(source, line).filter(|frame| is_fragile_frame(source, frame))
}

fn is_fragile_frame(source: &str, frame: &std::ops::Range<usize>) -> bool {
    let Some(row) = source.lines().nth(frame.start).map(source_line_text) else {
        return false;
    };
    let row_start: usize = source
        .split_inclusive('\n')
        .take(frame.start)
        .map(str::len)
        .sum();
    let Some(mut at) = row
        .match_indices("\\begin")
        .filter(|(at, _)| {
            row[..*at]
                .bytes()
                .rev()
                .take_while(|ch| *ch == b'\\')
                .count()
                % 2
                == 0
        })
        .find_map(|(at, _)| {
            let (environment, end) = math::group(source, row_start + at + "\\begin".len())?;
            (&source[environment] == "frame").then_some(end)
        })
    else {
        return false;
    };
    at = skip_source_space(source, at);
    if source.as_bytes().get(at) == Some(&b'<') {
        let Some(end) = source[at..].find('>') else {
            return false;
        };
        at = skip_source_space(source, at + end + 1);
    }
    let Some(end) = optional_argument_end(source, at) else {
        return false;
    };
    let options = &source[at + 1..end - 1];
    options.split(',').flat_map(str::lines).any(|option| {
        let option = source_line_text(option).trim();
        option == "fragile" || option.starts_with("fragile=")
    })
}

/// Title pages and running headers can be produced from declarations far
/// outside SyncTeX's reported frame. Search only literal document metadata,
/// including direct preamble inputs, and require neighboring PDF words.
fn document_metadata_word_location(
    main: &Path,
    context: &str,
    offset: usize,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<Option<MetadataLocation>> {
    operation.check()?;
    let Ok(main) = sources.normalize(main) else {
        return Ok(None);
    };
    if sources.warning(&main).is_some() {
        return Ok(None);
    }
    let main_source = match sources.read(&main, operation) {
        Ok(source) => source,
        Err(error) if crate::source_snapshot::required_cache_error(&error) => return Err(error),
        Err(_) => {
            operation.check()?;
            return Ok(None);
        }
    };
    let mut files = vec![(main.clone(), main_source)];
    let inputs: Vec<_> = files[0]
        .1
        .lines()
        .take_while(|row| !source_line_text(row).contains("\\begin{document}"))
        .filter_map(|row| {
            source_line_text(row)
                .trim()
                .strip_prefix("\\input{")
                .and_then(|rest| rest.split_once('}'))
                .map(|(name, _)| name.to_owned())
        })
        .take(8)
        .collect();
    for name in inputs {
        operation.check()?;
        let Some(parent) = main.parent() else {
            return Ok(None);
        };
        let mut path = parent.join(name);
        if path.extension().is_none() {
            path.set_extension("tex");
        }
        if let Ok(path) = sources.normalize(&path)
            && !files.iter().any(|(known, _)| *known == path)
            && sources.warning(&path).is_none()
        {
            match sources.read(&path, operation) {
                Ok(source) => files.push((path, source)),
                Err(error) if crate::source_snapshot::required_cache_error(&error) => {
                    return Err(error);
                }
                Err(_) => operation.check()?,
            }
        }
    }
    let mut best = None;
    let mut tied = false;
    for (file_index, (_, source)) in files.iter().enumerate() {
        operation.check()?;
        let hidden = nonprinting_source_ranges(source);
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
                    if let Some((body, end)) =
                        math::group(source, skip_source_space(source, arg_at))
                    {
                        spans.push(body.start..end);
                    }
                    let mut cmd_best: Option<(isize, u32, usize)> = None;
                    for span in spans {
                        let first = source[..span.start]
                            .bytes()
                            .filter(|ch| *ch == b'\n')
                            .count();
                        let last = source[..span.end].bytes().filter(|ch| *ch == b'\n').count();
                        let mut span_hidden = hidden.clone();
                        span_hidden.push(0..span.start);
                        span_hidden.push(span.end..source.len());
                        let Some((line, byte, score)) = source_prose_scored_location(
                            source,
                            1,
                            context,
                            offset,
                            first..last + 1,
                            true,
                            &span_hidden,
                        ) else {
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
    Ok(Some((file.to_owned(), source, line, byte, score)))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RawInverseCandidate {
    pub file: String,
    pub line: u32,
    pub compiler_column: Option<usize>,
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

pub(crate) fn materialize_raw_candidates(
    candidates: Vec<RawInverseCandidate>,
    pdf: Option<&Path>,
    sidecar_dir: Option<&Path>,
) -> Vec<SourceLocation> {
    let current_dir = std::env::current_dir()
        .ok()
        .and_then(|dir| fs::canonicalize(&dir).ok().or(Some(dir)));
    let pdf_dir = pdf.and_then(Path::parent);
    let mut loaded_sources = std::collections::HashMap::<PathBuf, Option<String>>::new();
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::with_capacity(candidates.len());
    for cand in candidates {
        let mut loc = SourceLocation {
            file: cand.file,
            line: cand.line,
            byte_column: 0,
            column: 1,
            column_char: 1,
            precise: false,
        };
        if let Some(column) = cand.compiler_column.filter(|&col| col > 1) {
            let raw_path = Path::new(&loc.file);
            let resolved = if raw_path.is_absolute() {
                raw_path.to_owned()
            } else {
                crate::source_snapshot::resolve_relative_input(
                    raw_path,
                    current_dir.as_deref(),
                    pdf_dir,
                    sidecar_dir,
                )
                .unwrap_or_else(|| raw_path.to_owned())
            };
            if !crate::source_snapshot::is_compiler_sidecar_input(&resolved) {
                let source = loaded_sources
                    .entry(resolved.clone())
                    .or_insert_with(|| read_source(&resolved).ok());
                if let Some(source) = source.as_deref() {
                    if let Ok(text) = validate_inverse_source_line(source, loc.line, &resolved) {
                        apply_compiler_column(&mut loc, text, column);
                    }
                } else {
                    loc.byte_column = column - 1;
                    loc.column = column;
                    loc.column_char = column;
                }
            }
        }
        if seen.insert((loc.file.clone(), loc.line, loc.byte_column)) {
            results.push(loc);
        }
    }
    results
}

pub(crate) fn parse_synctex_edit_candidates(stdout: &str) -> Vec<RawInverseCandidate> {
    let mut input = None;
    let mut active = false;
    let mut raw = Vec::<RawInverseCandidate>::new();
    for row in stdout.lines() {
        if let Some(value) = row.strip_prefix("Input:") {
            input = Some(value.trim());
            active = false;
        } else if let Some(value) = row.strip_prefix("Line:") {
            active = false;
            if let Some(file) = input.take()
                && !file.is_empty()
                && let Ok(line) = value.trim().parse::<u32>()
                && line > 0
            {
                raw.push(RawInverseCandidate {
                    file: file.to_owned(),
                    line,
                    compiler_column: None,
                });
                active = true;
            }
        } else if active && let Some(value) = row.strip_prefix("Column:") {
            active = false;
            if let Ok(column) = value.trim().parse::<usize>()
                && column > 1
                && let Some(last) = raw.last_mut()
            {
                last.compiler_column = Some(column);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter(|cand| seen.insert((cand.file.clone(), cand.line, cand.compiler_column)))
        .collect()
}

pub fn parse_synctex_edit(stdout: &str) -> Vec<SourceLocation> {
    materialize_raw_candidates(parse_synctex_edit_candidates(stdout), None, None)
}
/// TeX comments start at an unescaped percent sign.
fn source_line_text(text: &str) -> &str {
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'%' {
            return &text[..index];
        }
    }
    text
}

/// Collected environments (Beamer `frame`, `tabularx`) attribute their body to
/// the closing line: forward search refines against the closing line, inverse
/// search against the enclosing range.
fn environment_bounds(source: &str, anchor: usize, environment: &str) -> Option<(usize, usize)> {
    let mut ignored = Vec::new();
    let mut scan = 0;
    while scan < source.len() {
        match source.as_bytes()[scan] {
            b'%' => {
                scan += source[scan..].find('\n').unwrap_or(source.len() - scan);
            }
            b'\\' => {
                let start = scan;
                let (command, end) = math::command(source, scan);
                if end == start + 1 {
                    scan = end;
                    continue;
                }
                if let Some((opaque_end, _)) = math::opaque_command_end(source, command, end) {
                    ignored.push(start..opaque_end);
                    scan = opaque_end;
                    continue;
                }
                scan = end;
            }
            _ => {
                scan += source.as_bytes()[scan + 1..]
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'\\'))
                    .map_or(source.len() - scan, |pos| pos + 1);
            }
        }
    }
    ignored.extend(nonprinting_source_ranges(source));
    let match_env = |text: &str, kind: &str| -> Option<usize> {
        let rest = text
            .strip_prefix(kind)?
            .trim_start()
            .strip_prefix('{')?
            .strip_prefix(environment)?
            .strip_prefix('}')?;
        Some(text.len() - rest.len())
    };
    let mut start = None;
    let mut line_offset = 0;
    for (index, raw_row) in source.split_inclusive('\n').enumerate() {
        let row = raw_row.strip_suffix('\n').unwrap_or(raw_row);
        let row = row.strip_suffix('\r').unwrap_or(row);
        let visible = source_line_text(row);
        let mut cursor = 0;
        while let Some(rel) = visible[cursor..].find('\\') {
            let slash = cursor + rel;
            let abs = line_offset + slash;
            cursor = slash + 1;
            if visible[cursor..].starts_with('\\') {
                cursor += 1;
                continue;
            }
            if ignored.iter().any(|range| range.contains(&abs)) {
                continue;
            }
            let text = &visible[cursor..];
            if let Some(consumed) = match_env(text, "begin") {
                cursor += consumed;
                // Nested collected environments are not a reliable source boundary.
                if start.replace(index).is_some() {
                    return None;
                }
            } else if let Some(consumed) = match_env(text, "end") {
                cursor += consumed;
                if let Some(begin) = start.take() {
                    if begin <= anchor && anchor <= index {
                        return Some((begin, index));
                    }
                } else if environment == "frame" && anchor <= index {
                    return Some((index, index));
                }
            }
        }
        line_offset += raw_row.len();
        if index >= anchor && start.is_none() {
            return None;
        }
    }
    None
}

fn frame_bounds(source: &str, anchor: usize) -> Option<(usize, usize)> {
    environment_bounds(source, anchor, "frame")
}

/// Find the literal frame whose body closes at or after the one-based
/// SyncTeX line. Beamer forwards map interior lines to the closing line.
pub fn frame_closing_line(source: &str, line: u32) -> Option<u32> {
    frame_bounds(source, line.checked_sub(1)? as usize).map(|(_, end)| end as u32 + 1)
}

/// Find the literal `tabularx` environment whose body closes at or after the
/// one-based SyncTeX line. `tabularx` collects its body before trial passes,
/// stamping interior records with the `\end{tabularx}` line.
pub fn tabularx_closing_line(source: &str, line: u32) -> Option<u32> {
    environment_bounds(source, line.checked_sub(1)? as usize, "tabularx")
        .map(|(_, end)| end as u32 + 1)
}

fn caption_closing_line(source: &str, line: u32, column: u32) -> Option<u32> {
    let row = validate_forward_source_position(source, line, column).ok()?;
    // End-of-line is a cursor boundary, not a character in the argument.
    let byte = row.char_indices().nth(column.checked_sub(1)? as usize)?.0;
    let start = source
        .split_inclusive('\n')
        .take(line.checked_sub(1)? as usize)
        .map(str::len)
        .sum::<usize>();
    let body = math::caption_argument_at(source, start + byte)?;
    u32::try_from(source[..body.end].bytes().filter(|ch| *ch == b'\n').count())
        .ok()?
        .checked_add(1)
}

/// Find a literal frame environment enclosing the one-based SyncTeX line.
fn source_frame_range(source: &str, line: u32) -> Option<std::ops::Range<usize>> {
    frame_bounds(source, line.checked_sub(1)? as usize).map(|(start, end)| start..end + 1)
}

/// Find a literal `tabularx` environment enclosing the one-based SyncTeX line.
fn source_tabularx_range(source: &str, line: u32) -> Option<std::ops::Range<usize>> {
    environment_bounds(source, line.checked_sub(1)? as usize, "tabularx")
        .map(|(start, end)| start..end + 1)
}

pub fn is_spacing_diacritic(ch: char) -> bool {
    matches!(
        ch,
        '\u{00a8}' | '\u{00af}' | '\u{00b4}' | '\u{00b8}' | '\u{02c6}' | '\u{02c7}' | '\u{02d8}'
            ..='\u{02dd}'
    )
}

pub fn words(text: &str) -> Vec<(usize, &str)> {
    let mut result = Vec::new();
    let mut start = None;
    for (index, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        let attached_spacing = is_spacing_diacritic(ch)
            && (start.is_some()
                || text
                    .get(index + ch.len_utf8()..)
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(char::is_alphanumeric));
        if ch.is_alphanumeric() || start.is_some() && is_combining_mark(ch) || attached_spacing {
            start.get_or_insert(index);
        } else if let Some(start) = start.take() {
            result.push((start, &text[start..index]));
        }
    }
    result
}

pub fn word_index_at(text: &str, words: &[(usize, &str)], offset: usize) -> Option<usize> {
    if let Some(index) = words
        .iter()
        .position(|(start, word)| *start <= offset && offset < start + word.len())
    {
        return Some(index);
    }
    let ch = text.get(offset..)?.chars().next()?;
    if matches!(
        ch,
        '-' | '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\'' | '\u{2019}'
    ) {
        let next = offset + ch.len_utf8();
        return words
            .iter()
            .position(|(start, word)| start + word.len() == offset || *start == next);
    }
    None
}

fn tex_accent_char(text: &str, at: usize) -> Option<(char, usize)> {
    let after = at.checked_add(1)?;
    let cmd = *text.as_bytes().get(after)?;
    let rest = text.get(after + 1..)?;
    if matches!(cmd, b'\'' | b'`' | b'^' | b'"' | b'~' | b'=' | b'.') {
        if let Some(braced) = rest.strip_prefix('{') {
            if let Some(tail) = braced
                .strip_prefix("\\i}")
                .or_else(|| braced.strip_prefix("i}"))
            {
                return Some(('i', text.len() - tail.len()));
            }
            if let Some(tail) = braced
                .strip_prefix("\\j}")
                .or_else(|| braced.strip_prefix("j}"))
            {
                return Some(('j', text.len() - tail.len()));
            }
            let ch = braced.chars().next().filter(|ch| ch.is_alphabetic())?;
            let tail = braced.get(ch.len_utf8()..)?.strip_prefix('}')?;
            return Some((ch, text.len() - tail.len()));
        }
        if let Some(tail) = rest.strip_prefix("\\i")
            && !tail
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
        {
            return Some(('i', text.len() - tail.len()));
        }
        if let Some(tail) = rest.strip_prefix("\\j")
            && !tail
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
        {
            return Some(('j', text.len() - tail.len()));
        }
        let ch = rest.chars().next().filter(|ch| ch.is_alphabetic())?;
        return Some((ch, after + 1 + ch.len_utf8()));
    }
    if matches!(
        cmd,
        b'u' | b'v' | b'H' | b'c' | b'k' | b'r' | b'b' | b'd' | b't'
    ) && let Some(braced) = rest.strip_prefix('{')
    {
        if let Some(tail) = braced.strip_prefix("\\i}") {
            return Some(('i', text.len() - tail.len()));
        }
        if let Some(tail) = braced.strip_prefix("\\j}") {
            return Some(('j', text.len() - tail.len()));
        }
        let ch = braced.chars().next().filter(|ch| ch.is_alphabetic())?;
        let tail = braced.get(ch.len_utf8()..)?.strip_prefix('}')?;
        return Some((ch, text.len() - tail.len()));
    }
    None
}

fn tex_accent_at(text: &str, index: usize) -> Option<(char, usize)> {
    if index > 0 && text.as_bytes()[index - 1] == b'\\' {
        return None;
    }
    match *text.as_bytes().get(index)? {
        b'\\' => tex_accent_char(text, index),
        b'{' => {
            let (ch, next) = tex_accent_char(text, index + 1)?;
            (text.as_bytes().get(next) == Some(&b'}')).then_some((ch, next + 1))
        }
        _ => None,
    }
}

fn source_words(text: &str) -> Vec<(usize, usize, String)> {
    let mut result = Vec::new();
    let mut start = None;
    let mut word = String::new();
    let mut index = 0;
    while index < text.len() {
        if let Some((ch, next)) = tex_accent_at(text, index) {
            start.get_or_insert(index);
            word.push(ch);
            index = next;
            continue;
        }
        let ch = text[index..].chars().next().unwrap();
        let attached_spacing = is_spacing_diacritic(ch)
            && (start.is_some()
                || text
                    .get(index + ch.len_utf8()..)
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(char::is_alphanumeric));
        if ch.is_alphanumeric() || start.is_some() && is_combining_mark(ch) || attached_spacing {
            start.get_or_insert(index);
            word.push(ch);
        } else if let Some(start) = start.take() {
            result.push((start, index, normalized_word(&word)));
            word.clear();
        }
        index += ch.len_utf8();
    }
    if let Some(start) = start {
        result.push((start, text.len(), normalized_word(&word)));
    }
    result
}

fn normalized_word(word: &str) -> String {
    if word.is_ascii() {
        return word.to_ascii_lowercase();
    }
    word.chars()
        .filter(|ch| !is_spacing_diacritic(*ch))
        .nfkd()
        .filter(|ch| !is_combining_mark(*ch))
        .flat_map(char::to_lowercase)
        .collect()
}

fn ordinary_word_source_start(
    source: &str,
    context: &str,
    offset: usize,
    line: u32,
    byte: usize,
) -> Option<usize> {
    let pdf = words(context);
    let Some(selected) = word_index_at(context, &pdf, offset) else {
        return Some(byte);
    };
    let pdf_word = normalized_word(pdf[selected].1);
    if pdf_word.len() < 2 || !pdf_word.bytes().all(|ch| ch.is_ascii_alphabetic()) {
        return Some(byte);
    }
    source
        .lines()
        .nth(line as usize - 1)
        .and_then(|text| {
            source_words(source_line_text(text))
                .into_iter()
                .find(|(start, end, _)| *start <= byte && byte < *end)
        })
        .and_then(|(start, _, word)| (word == pdf_word).then_some(start))
}

/// Refine prose and mathematical atoms without expanding arbitrary TeX macros.
fn source_word_location(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    radius: u32,
) -> Option<(u32, usize)> {
    let scope = math::caption_lines(source, line)
        .or_else(|| source_frame_range(source, line))
        .or_else(|| source_tabularx_range(source, line));
    let within_scope = scope.is_some();
    let lines = scope.unwrap_or_else(|| {
        line.saturating_sub(radius.saturating_add(1)) as usize
            ..(line as usize).saturating_add(radius as usize)
    });
    source_word_location_in_range(source, line, context, offset, lines, within_scope)
}

fn source_word_location_in_range(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_scope: bool,
) -> Option<(u32, usize)> {
    let hidden = nonprinting_source_ranges(source);
    // A closing layout environment can alias every row to one source line.
    // A search radius alone does not identify neighboring prose or math;
    // independently established frame/caption and math scopes remain eligible.
    let closing_layout = source
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .map(source_line_text)
        .and_then(|text| text.trim_start().strip_prefix("\\end"))
        .and_then(|rest| {
            let rest = rest.trim_start();
            math::group(rest, 0).map(|(name, _)| !math::math_environment(&rest[name]))
        })
        .unwrap_or(false);
    let lines = if !within_scope && closing_layout {
        line.saturating_sub(1) as usize..line as usize
    } else {
        lines
    };
    let prose = source_prose_scored_location(
        source,
        line,
        context,
        offset,
        lines.clone(),
        within_scope,
        &hidden,
    );
    let selected_norm = {
        let pdf = words(context);
        word_index_at(context, &pdf, offset).map(|idx| normalized_word(pdf[idx].1))
    };
    math::source_location(
        source,
        line,
        lines.clone(),
        within_scope,
        context,
        offset,
        prose,
    )
    .and_then(|(row, byte)| {
        if !hidden.is_empty() {
            let absolute = source
                .split_inclusive('\n')
                .take(row as usize - 1)
                .map(str::len)
                .sum::<usize>()
                + byte;
            if hidden.iter().any(|range| range.contains(&absolute)) {
                return None;
            }
        }
        if !within_scope && row != line && crosses_paragraph_boundary(source, line, row) {
            return None;
        }
        if let Some(ref target_word) = selected_norm {
            let check_lines = if within_scope && is_fragile_frame(source, &lines) {
                lines.start.saturating_add(1)..lines.end
            } else {
                lines.clone()
            };
            if defined_macro_conflict(source, check_lines, &hidden, target_word) {
                return None;
            }
        }
        ordinary_word_source_start(source, context, offset, row, byte).map(|start| (row, start))
    })
}

fn has_control_word(text: &str, name: &str) -> bool {
    let mut rest = text;
    while let Some(pos) = rest.find('\\') {
        let after_slash = &rest[pos + 1..];
        if let Some(tail) = after_slash.strip_prefix(name)
            && !tail
                .as_bytes()
                .first()
                .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == b'@')
        {
            return true;
        }
        rest = after_slash;
    }
    false
}

fn crosses_paragraph_boundary(source: &str, line: u32, row: u32) -> bool {
    let start = line.min(row).saturating_sub(1) as usize;
    let end = line.max(row).saturating_sub(1) as usize;
    for (idx, raw) in source.lines().enumerate().take(end + 1).skip(start) {
        let text = source_line_text(raw);
        if idx < end && has_control_word(text, "par") {
            return true;
        }
        if idx > start && idx < end && text.trim().is_empty() {
            return true;
        }
    }
    false
}

fn defined_macro_conflict(
    source: &str,
    lines: std::ops::Range<usize>,
    hidden: &[std::ops::Range<usize>],
    target_word: &str,
) -> bool {
    if target_word.is_empty() {
        return false;
    }
    let mut macros = Vec::<(&str, &str)>::new();
    for (start, span) in nonprinting_source_spans(source) {
        if source.as_bytes().get(start) != Some(&b'\\') {
            continue;
        }
        let (command, end) = math::command(source, start);
        let mut at = end + usize::from(matches!(source.as_bytes().get(end), Some(b'*' | b'+')));
        let name = if matches!(command, "def" | "gdef" | "edef" | "xdef") {
            at = skip_source_space(source, at);
            if source.as_bytes().get(at) != Some(&b'\\') {
                continue;
            }
            let (name, next) = math::command(source, at);
            at = next;
            while at < span.end && source.as_bytes()[at] != b'{' {
                if source.as_bytes()[at] == b'\\' {
                    at += 1;
                    at += source[at..].chars().next().map_or(0, char::len_utf8);
                } else if source.as_bytes()[at] == b'%' {
                    at = skip_source_space(source, at);
                } else {
                    at += source[at..].chars().next().map_or(1, char::len_utf8);
                }
            }
            name
        } else if matches!(
            command,
            "newcommand"
                | "renewcommand"
                | "providecommand"
                | "DeclareRobustCommand"
                | "NewDocumentCommand"
                | "RenewDocumentCommand"
                | "ProvideDocumentCommand"
                | "DeclareDocumentCommand"
        ) {
            at = skip_source_space(source, at);
            let name = if let Some((grp, next)) = math::group(source, at) {
                at = next;
                source[grp].trim().strip_prefix('\\').unwrap_or("")
            } else if source.as_bytes().get(at) == Some(&b'\\') {
                let (name, next) = math::command(source, at);
                at = next;
                name
            } else {
                continue;
            };
            while let Some(opt_end) = optional_argument_end(source, at) {
                at = opt_end;
            }
            if command.contains("Document") {
                let Some((_, spec_end)) = math::group(source, skip_source_space(source, at)) else {
                    continue;
                };
                at = spec_end;
            }
            name
        } else {
            continue;
        };
        let body_start = skip_source_space(source, at);
        if !name.is_empty()
            && let Some((body, _)) = math::group(source, body_start)
        {
            macros.push((name, &source[body]));
        }
    }
    let matching_macros: Vec<&str> = macros
        .into_iter()
        .filter_map(|(name, body)| {
            source_words(body)
                .into_iter()
                .any(|(_, _, word)| word == target_word)
                .then_some(name)
        })
        .collect();
    if matching_macros.is_empty() {
        return false;
    }
    let mut line_start = 0usize;
    for (index, raw_row) in source.split_inclusive('\n').enumerate().take(lines.end) {
        if index >= lines.start {
            let text = source_line_text(raw_row.trim_end_matches(['\r', '\n']));
            let mut offset = 0usize;
            while let Some(pos) = text[offset..].find('\\') {
                let slash = offset + pos;
                let abs = line_start + slash;
                let (cmd_name, next) = math::command(text, slash);
                if !cmd_name.is_empty()
                    && !hidden.iter().any(|range| range.contains(&abs))
                    && matching_macros.contains(&cmd_name)
                {
                    return true;
                }
                offset = next.max(slash + 1);
            }
        }
        line_start += raw_row.len();
    }
    false
}

fn skip_source_space(source: &str, mut at: usize) -> usize {
    loop {
        at += source[at..].len() - source[at..].trim_start().len();
        if source.as_bytes().get(at) != Some(&b'%') {
            return at;
        }
        at += source[at..].find('\n').unwrap_or(source.len() - at);
    }
}

/// TeX optional arguments end at the first unbraced, unescaped closing bracket.
fn optional_argument_end(source: &str, start: usize) -> Option<usize> {
    let start = skip_source_space(source, start);
    if source.as_bytes().get(start) != Some(&b'[') {
        return None;
    }
    let mut at = start + 1;
    while at < source.len() {
        match source.as_bytes()[at] {
            b'\\' => {
                at += 1;
                at += source[at..].chars().next().map_or(0, char::len_utf8);
                continue;
            }
            b'%' => {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
                continue;
            }
            b'{' => {
                at = math::group(source, at)?.1;
                continue;
            }
            b']' => return Some(at + 1),
            _ => {}
        }
        at += source[at..].chars().next()?.len_utf8();
    }
    None
}

fn overlay_argument_end(source: &str, start: usize) -> Option<usize> {
    if source.as_bytes().get(start) != Some(&b'<') {
        return None;
    }
    let rest = &source[start + 1..];
    let end = rest.find(['>', '\n'])?;
    if rest.as_bytes()[end] != b'>' {
        return None;
    }
    let body = &rest[..end];
    if body.is_empty() {
        return None;
    }
    let has_action = body.contains([':', '@']);
    body.bytes()
        .all(|ch| {
            ch.is_ascii_digit()
                || matches!(
                    ch,
                    b'+' | b'-' | b',' | b'.' | b'(' | b')' | b'|' | b'*' | b' '
                )
                || has_action && (ch.is_ascii_alphabetic() || matches!(ch, b':' | b'@'))
        })
        .then_some(start + 1 + end + 1)
}

/// Only known nonprinting arguments are masked. This is not macro expansion;
/// ordinary formatting/caption arguments and href display text remain literal.
fn nonprinting_source_ranges(source: &str) -> Vec<std::ops::Range<usize>> {
    nonprinting_source_spans(source)
        .map(|(_, body)| body)
        .collect()
}

fn nonprinting_source_spans(
    source: &str,
) -> impl Iterator<Item = (usize, std::ops::Range<usize>)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        loop {
            if at >= source.len() {
                return None;
            }
            if source.as_bytes()[at] == b'%' {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
            } else if source.as_bytes()[at] == b'<'
                && at > 0
                && matches!(source.as_bytes()[at - 1], b'}' | b']')
            {
                let start = at;
                if let Some(end) = overlay_argument_end(source, at) {
                    at = end;
                    return Some((start, start..end));
                }
                at += 1;
            } else if source.as_bytes()[at] == b'\\' {
                let start = at;
                at += 1;
                let end = at
                    + source[at..]
                        .bytes()
                        .take_while(|ch| ch.is_ascii_alphabetic() || *ch == b'@')
                        .count();
                if end == at {
                    at += source[at..].chars().next().map_or(0, char::len_utf8);
                    continue;
                }
                let command = &source[at..end];
                if let Some((end, nonprinting)) = math::opaque_command_end(source, command, end) {
                    at = end;
                    if nonprinting {
                        return Some((start, start..end));
                    }
                    continue;
                }
                if matches!(command, "let" | "futurelet") {
                    at = skip_source_space(source, end);
                    if source.as_bytes().get(at) == Some(&b'\\') {
                        at = math::command(source, at).1;
                    } else if at < source.len() {
                        at += source[at..].chars().next().map_or(1, char::len_utf8);
                    }
                    at = skip_source_space(source, at);
                    if command == "let" && source.as_bytes().get(at) == Some(&b'=') {
                        at = skip_source_space(source, at + 1);
                    }
                    if source.as_bytes().get(at) == Some(&b'\\') {
                        at = math::command(source, at).1;
                    } else if at < source.len() {
                        at += source[at..].chars().next().map_or(1, char::len_utf8);
                    }
                    return Some((start, start..at));
                }
                at = end + usize::from(matches!(source.as_bytes().get(end), Some(b'*' | b'+')));
                let definition = matches!(
                    command,
                    "newcommand"
                        | "renewcommand"
                        | "providecommand"
                        | "DeclareRobustCommand"
                        | "NewDocumentCommand"
                        | "RenewDocumentCommand"
                        | "ProvideDocumentCommand"
                        | "DeclareDocumentCommand"
                        | "newenvironment"
                        | "renewenvironment"
                        | "def"
                        | "gdef"
                        | "edef"
                        | "xdef"
                );
                if definition {
                    if matches!(command, "def" | "gdef" | "edef" | "xdef") {
                        while at < source.len() && source.as_bytes()[at] != b'{' {
                            if source.as_bytes()[at] == b'\\' {
                                at += 1;
                                at += source[at..].chars().next().map_or(0, char::len_utf8);
                            } else if source.as_bytes()[at] == b'%' {
                                at = skip_source_space(source, at);
                            } else {
                                at += source[at..].chars().next().unwrap().len_utf8();
                            }
                        }
                    } else {
                        at = skip_source_space(source, at);
                        if let Some((_, end)) = math::group(source, at) {
                            at = end;
                        } else if source.as_bytes().get(at) == Some(&b'\\') {
                            at = math::command(source, at).1;
                        }
                        while let Some(end) = optional_argument_end(source, at) {
                            at = end;
                        }
                        if command.contains("Document") {
                            let Some((_, end)) = math::group(source, skip_source_space(source, at))
                            else {
                                continue;
                            };
                            at = end;
                        }
                    }
                    let body_start = skip_source_space(source, at);
                    let Some((_, end)) = math::group(source, body_start) else {
                        if source.as_bytes().get(body_start) == Some(&b'{') {
                            let end = source.len();
                            at = end;
                            return Some((start, start..end));
                        }
                        at = body_start;
                        continue;
                    };
                    at = end;
                    if command.ends_with("environment")
                        && let Some((_, end)) = math::group(source, skip_source_space(source, at))
                    {
                        at = end;
                    }
                    return Some((start, start..at));
                } else if matches!(
                    command,
                    "label"
                        | "ref"
                        | "eqref"
                        | "pageref"
                        | "autoref"
                        | "cref"
                        | "Cref"
                        | "cite"
                        | "citep"
                        | "citet"
                        | "parencite"
                        | "textcite"
                        | "autocite"
                        | "footcite"
                        | "nocite"
                        | "href"
                        | "includegraphics"
                        | "includesvg"
                        | "input"
                        | "include"
                        | "bibliography"
                        | "addbibresource"
                ) {
                    while let Some(end) = optional_argument_end(source, at)
                        .or_else(|| overlay_argument_end(source, at))
                    {
                        at = end;
                    }
                    if let Some((body, end)) = math::group(source, skip_source_space(source, at)) {
                        at = end;
                        return Some((
                            start,
                            if matches!(command, "includegraphics" | "includesvg") {
                                start..end
                            } else {
                                body
                            },
                        ));
                    }
                } else if matches!(command, "begin" | "end")
                    && let Some((body, mut group_end)) =
                        math::group(source, skip_source_space(source, at))
                    && matches!(
                        &source[body.clone()],
                        "tabularx" | "tabular*" | "tabular" | "array"
                    )
                {
                    let env = &source[body.clone()];
                    if command == "begin" {
                        if matches!(env, "tabularx" | "tabular*") {
                            if let Some(opt_end) = optional_argument_end(source, group_end) {
                                group_end = opt_end;
                            }
                            if let Some((_, width_end)) =
                                math::group(source, skip_source_space(source, group_end))
                            {
                                group_end = width_end;
                            }
                        }
                        if let Some(opt_end) = optional_argument_end(source, group_end) {
                            group_end = opt_end;
                        }
                        if let Some((_, spec_end)) =
                            math::group(source, skip_source_space(source, group_end))
                        {
                            group_end = spec_end;
                        }
                    }
                    at = group_end;
                    return Some((start, body.start..group_end));
                } else if let Some(overlay_end) = overlay_argument_end(source, at) {
                    at = overlay_end;
                    return Some((start, end..overlay_end));
                }
            } else {
                at += source.as_bytes()[at + 1..]
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'<' | b'\\'))
                    .map_or(source.len() - at, |pos| pos + 1);
            }
        }
    })
}

fn source_prose_scored_location(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_frame: bool,
    hidden: &[std::ops::Range<usize>],
) -> Option<(u32, usize, isize)> {
    let pdf = words(context);
    let selected = word_index_at(context, &pdf, offset)?;
    let pdf: Vec<_> = pdf.iter().map(|(_, word)| normalized_word(word)).collect();
    let frame_header = within_frame
        && source
            .lines()
            .nth(lines.start)
            .is_some_and(|row| source_line_text(row).contains("\\begin{frame}"));
    let mut candidates = Vec::new();
    let mut line_start = 0;
    for (index, raw_row) in source.split_inclusive('\n').enumerate().take(lines.end) {
        let text = source_line_text(raw_row.trim_end_matches(['\r', '\n']));
        if index >= lines.start {
            for (byte, _, word) in source_words(text) {
                if (byte == 0 || !text[..byte].ends_with('\\'))
                    && !hidden
                        .iter()
                        .any(|range| range.contains(&(line_start + byte)))
                {
                    candidates.push((index as u32 + 1, byte, word));
                }
            }
        }
        line_start += raw_row.len();
    }
    let mut best = None;
    let mut tied = false;
    for (index, (row, byte, word)) in candidates.iter().enumerate() {
        if *word != pdf[selected] {
            continue;
        }
        let mut score = 0;
        for distance in 1..=3 {
            for direction in [-1isize, 1] {
                let delta = distance * direction;
                if let (Some(a), Some(b)) = (
                    selected.checked_add_signed(delta).and_then(|i| pdf.get(i)),
                    index
                        .checked_add_signed(delta)
                        .and_then(|i| candidates.get(i)),
                ) && *a == b.2
                {
                    score += 4 - distance;
                }
            }
        }
        // TeX math can insert source tokens that PDF text extraction omits.
        // Use nearby longer words as a tie break for repeated prose.
        let mut context_bonus = 0;
        for direction in [-1isize, 1] {
            for distance in 1..=8 {
                let Some(neighbor) = selected
                    .checked_add_signed(direction * distance)
                    .and_then(|i| pdf.get(i))
                else {
                    break;
                };
                if neighbor.chars().count() < 4 {
                    continue;
                }
                if (1..=10).any(|step| {
                    index
                        .checked_add_signed(direction * step)
                        .and_then(|i| candidates.get(i))
                        .is_some_and(|candidate| candidate.2 == *neighbor)
                }) {
                    context_bonus += 1;
                }
            }
        }
        // A collected frame/caption boundary does not favor its last occurrence.
        let proximity = if within_frame { 0 } else { row.abs_diff(line) };
        let rank = (
            score,
            frame_header && *row as usize == lines.start + 1,
            context_bonus,
            std::cmp::Reverse(proximity),
        );
        match best {
            Some((old, _, _)) if rank < old => {}
            Some((old, _, _)) if rank == old => tied = true,
            _ => {
                best = Some((rank, *row, *byte));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied)
        .map(|(rank, row, byte)| (row, byte, rank.0))
}
