#![doc = include_str!("../README.md")]

mod math;
mod parser;
pub mod source_snapshot;
mod symbols;
#[cfg(test)]
mod tests;
mod tex;

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
use tex::*;
pub use tex::{
    frame_closing_line, is_spacing_diacritic, tabularx_closing_line, word_index_at, words,
};
use unicode_normalization::char::is_combining_mark;

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
    let (root_source, cached_root, cached_sidecar) = match &snapshot {
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
            (root_source, root_source.then_some(root), None)
        }
        None => {
            let (inputs, sidecar) = crate::source_snapshot::SourceInfo::read_with_sidecar_in(
                &pdf,
                context.build_directory,
            )?;
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
            (main == &file, None, sidecar)
        }
    };
    let mut warning = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.warning(&file))
        .map(str::to_owned);
    if snapshot.is_none() && context.anchor_only {
        warning =
            Some("best-guess navigation: using saved source and the compiler line anchor".into());
    }
    let source_revision = if snapshot.is_none() && !context.anchor_only {
        Some(PdfRevision::read(&file)?)
    } else {
        None
    };
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
        None => match read_source(&file) {
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
        },
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
    if let Some(source_revision) = source_revision
        && document_revision
            .and_then(|revision| revision.companion)
            .is_some_and(|(_, companion)| {
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
    // Collected frame/tabularx/caption/math bodies use their closing compiler line.
    // Keep the original source position for the page hint, and give Beamer priority.
    let view_line = source
        .as_deref()
        .and_then(|source| {
            frame_closing_line(source, line)
                .or_else(|| tabularx_closing_line(source, line))
                .or_else(|| original_line.and_then(|_| caption_closing_line(source, line, column)))
                .or_else(|| math::math_closing_line(source, line, column))
        })
        .unwrap_or(line);
    let candidates = match &cached_sidecar {
        Some((bytes, sidecar_path)) => {
            crate::parser::forward_candidates_from_reader_with_page_hint_in(
                io::Cursor::new(bytes),
                &pdf,
                &file,
                snapshot.as_ref(),
                view_line,
                true,
                context.page_hint,
                sidecar_path.parent(),
            )?
        }
        None => forward_candidates_in(
            &pdf,
            &file,
            snapshot.as_ref(),
            view_line,
            context.page_hint,
            context.build_directory,
        )?,
    };
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
                source
                    .as_deref()
                    .zip(original_line)
                    .is_some_and(|(source, text)| {
                        let Some((byte, _)) = text.char_indices().nth(column as usize - 1) else {
                            return false;
                        };
                        let absolute = source
                            .split_inclusive('\n')
                            .take(line as usize - 1)
                            .map(str::len)
                            .sum::<usize>()
                            + byte;
                        !tex::nonprinting_source_spans(source)
                            .any(|(_, span, _)| span.contains(&absolute))
                    })
            })
        } else {
            None
        };
        warning = if word.is_some() && snapshot.is_none() {
            Some("assuming saved source is unchanged since compilation".into())
        } else {
            None
        };
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
    if let Some(revision) = document_revision {
        revision.check_in(&result.pdf, context.build_directory)?;
    } else {
        revision.check(&result.pdf)?;
    }
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
    if !parser::inverse_point_valid(point) {
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
    if !parser::inverse_point_valid(point) {
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
        spliced_preamble: None,
    };
    let mut resolutions = Vec::with_capacity(targets.len());
    for target in targets {
        operation.check()?;
        resolutions.push(query.resolve_target(target)?);
    }
    operation.check()?;
    let mut supported = resolutions
        .iter()
        .enumerate()
        .filter(|(_, (result, _))| result.location.precise);
    let mut ambiguous = None;
    let selected = if let Some((index, (first, _))) = supported.next() {
        if supported.any(|(_, (other, _))| !same_source_position(&first.location, &other.location))
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
    if assume_unchanged {
        warnings.insert(
            "best-guess navigation: assuming saved source is unchanged since compilation".into(),
        );
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
    graphics: Option<GraphicsHit>,
    generated: bool,
    warning: Option<String>,
    source_map: Option<io::Result<Rc<SourceMap>>>,
}

fn is_generated_sidecar(path: &Path, pdf: &Path) -> bool {
    path.file_stem() == pdf.file_stem() && crate::source_snapshot::is_compiler_sidecar_input(path)
}

fn is_generated_or_sidecar_input(path: &Path) -> bool {
    crate::source_snapshot::is_compiler_sidecar_input(path)
        || path
            .to_str()
            .is_some_and(crate::parser::is_generated_graphics_tex)
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
    spliced_preamble: Option<Option<Rc<SplicedSource>>>,
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
            graphics: raw.graphics,
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
        let verbatim_only = Path::new(&target.file)
            .extension()
            .is_some_and(|ext| ext == "vrb");
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
                    original_source_from_generated(
                        map,
                        target.line,
                        &mut self.sources,
                        self.operation,
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
                if crate::source_snapshot::required_cache_error(&error) {
                    return Err(error);
                }
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
            graphics,
            generated,
            mut warning,
            mut source_map,
            ..
        } = anchor;
        let is_generated_graphics = crate::parser::is_generated_graphics_tex(&target.file);
        let anchor = (!is_generated_graphics && (self.word.is_some() || graphics.is_some()))
            .then(|| target.clone());
        let refine_graphics = match graphics {
            Some(GraphicsHit::Unique { index, count }) => Some((index, count)),
            Some(GraphicsHit::Ambiguous) => None,
            None => None,
        };
        if !is_generated_graphics
            && (refine_graphics.is_some() || (graphics.is_none() && self.word.is_some()))
        {
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
            let frame_slide = source_map
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(|map| map.frame_slide);
            let spliced_preamble = if let Some(Ok(map)) = &source_map {
                match &self.spliced_preamble {
                    Some(cached) => cached.clone(),
                    None => {
                        let loaded = macro_preamble_source(
                            &map.main,
                            Some(&map.inputs),
                            self.pdf.parent(),
                            &mut self.sources,
                            self.operation,
                        )?
                        .map(Rc::new);
                        self.spliced_preamble = Some(loaded.clone());
                        loaded
                    }
                }
            } else {
                None
            };
            let spliced_ref = spliced_preamble
                .as_deref()
                .map(|spliced| spliced.source.as_str());
            let extra_sources: &[&str] = spliced_ref.as_slice();
            let execution_root = || {
                spliced_preamble
                    .as_deref()
                    .map(|spliced| tex::ExecutionRoot {
                        source: &spliced.source,
                        package_initialization: &spliced.package_initialization,
                    })
            };
            let fragile_body_proven = scope_proven
                && fragile_frame.is_some()
                && source_map
                    .as_ref()
                    .and_then(|map| map.as_ref().ok())
                    .is_some_and(|map| map.generated.extension().is_some_and(|ext| ext == "vrb"));
            let mut opaque_word = false;
            let mut word_location = |candidate| match candidate {
                Ok(location) => location,
                Err(()) => {
                    opaque_word = true;
                    None
                }
            };
            let mut location = if let Some((index, count)) = refine_graphics {
                source_graphics_location_with_context(
                    &source,
                    target.line,
                    index,
                    count,
                    scope_proven,
                    frame_slide,
                    extra_sources,
                    tex::WordLocationMode::Strict,
                    None,
                    self.operation,
                )
            } else if let Some((context, offset)) = self.word {
                if fragile_body_proven {
                    word_location(source_word_location_in_frame_body_with_sources(
                        &source,
                        target.line,
                        context,
                        offset,
                        extra_sources,
                        self.operation,
                    ))
                } else if scope_proven {
                    word_location(source_word_location_with_sources(
                        &source,
                        target.line,
                        context,
                        offset,
                        self.radius,
                        extra_sources,
                        self.operation,
                    ))
                } else {
                    word_location(source_word_location_in_range_with_sources(
                        &source,
                        target.line,
                        context,
                        offset,
                        target.line.saturating_sub(1) as usize..target.line as usize,
                        false,
                        extra_sources,
                        self.operation,
                    ))
                }
            } else {
                None
            };
            self.operation.check()?;
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
                            let frame_slide = map.frame_slide;
                            let candidate = if let Some((index, count)) = refine_graphics {
                                source_graphics_location_with_context(
                                    &original,
                                    anchor,
                                    index,
                                    count,
                                    true,
                                    frame_slide,
                                    extra_sources,
                                    tex::WordLocationMode::Strict,
                                    None,
                                    self.operation,
                                )
                            } else if let Some((context, offset)) = self.word {
                                word_location(
                                    if map.generated.extension().is_some_and(|ext| ext == "vrb") {
                                        source_word_location_in_frame_body_with_sources(
                                            &original,
                                            anchor,
                                            context,
                                            offset,
                                            extra_sources,
                                            self.operation,
                                        )
                                    } else {
                                        source_word_location_with_sources(
                                            &original,
                                            anchor,
                                            context,
                                            offset,
                                            self.radius,
                                            extra_sources,
                                            self.operation,
                                        )
                                    },
                                )
                            } else {
                                None
                            };
                            self.operation.check()?;
                            if self.sources.warning(Path::new(&file)).is_none()
                                && let Some(candidate) = candidate
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
            if refine_graphics.is_none()
                && !opaque_word
                && let Some((context, offset)) = self.word
                && !generated
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
                    self.metadata = Some(document_metadata_word_location_with_context(
                        &map.main,
                        Some(&map.inputs),
                        self.pdf.parent(),
                        context,
                        offset,
                        &mut self.sources,
                        self.operation,
                    )?);
                }
                if let Some((file, original, line, byte, score)) =
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
                            source_word_compiler_line(
                                original,
                                *line,
                                (prefix.chars().count() + 1) as u32,
                            ) == anchor.line
                        }
                    } else {
                        false
                    }
                    && (location.is_none()
                        || *score >= 6
                            && metadata_beats_frame(
                                &source,
                                target.line,
                                (context, offset),
                                self.point.y_from_top,
                                self.point.page_height_pt,
                                *score,
                                self.operation,
                            ))
                {
                    target.file = file.clone();
                    source = Rc::clone(original);
                    location = Some((*line, *byte));
                }
            }
            self.operation.check()?;
            if location.is_none()
                && self.sources.best_guess
                && !spliced_preamble
                    .as_deref()
                    .is_some_and(|root| root.truncated)
                && !generated
                && warning.is_none()
            {
                let mut guess_root = execution_root();
                if let (Some(spliced), Some(root)) = (spliced_preamble.as_deref(), &mut guess_root)
                {
                    let file = self.sources.normalize(Path::new(&target.file))?;
                    root.source = spliced.guess_source(&file);
                }
                let guess_source = guess_root.map(|root| root.source);
                let guess_sources = guess_source.as_slice();
                let candidate = if let Some((index, count)) = refine_graphics {
                    Ok(source_graphics_location_with_context(
                        &source,
                        target.line,
                        index,
                        count,
                        scope_proven,
                        frame_slide,
                        guess_sources,
                        tex::WordLocationMode::Guess,
                        guess_root,
                        self.operation,
                    )
                    .map(|(line, byte)| tex::UnverifiedWordLocation { line, byte }))
                } else if let Some((context, offset)) = self.word {
                    if scope_proven {
                        tex::source_word_guess_with_sources(
                            &source,
                            target.line,
                            context,
                            offset,
                            self.radius,
                            guess_sources,
                            self.operation,
                        )
                    } else {
                        tex::source_word_guess_in_range_with_sources(
                            &source,
                            target.line,
                            context,
                            offset,
                            target.line.saturating_sub(1) as usize..target.line as usize,
                            false,
                            guess_sources,
                            self.operation,
                        )
                    }
                } else {
                    Ok(None)
                };
                self.operation.check()?;
                if let Ok(Some(candidate)) = candidate {
                    location = Some((candidate.line, candidate.byte));
                }
            }
            if let Some((line, byte)) = location {
                let text = validate_inverse_source_line(&source, line, Path::new(&target.file))?;
                let prefix = text
                    .get(..byte)
                    .ok_or_else(|| io::Error::other("source column is not a UTF-8 boundary"))?;
                let column_char = prefix.chars().count() + 1;
                let compiler_line = source_word_compiler_line(&source, line, column_char as u32);
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
        Ok((
            InverseResolution {
                location: target,
                warning,
            },
            anchor,
        ))
    }
}

fn source_word_compiler_line(source: &str, line: u32, column_char: u32) -> u32 {
    frame_closing_line(source, line)
        .or_else(|| tabularx_closing_line(source, line))
        .or_else(|| caption_closing_line(source, line, column_char))
        .or_else(|| math::math_closing_line(source, line, column_char))
        .unwrap_or(line)
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
    (context, offset): (&str, usize),
    y_from_top: f32,
    page_height_pt: f32,
    metadata_score: isize,
    operation: &Operation,
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
            None,
            operation,
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
    let trimmed = value.trim();
    if let Some(unit_start) = trimmed.len().checked_sub(2)
        && let Some(scale) = trimmed.get(unit_start..).and_then(synctex_unit_scale)
        && let Some(number) = trimmed.get(..unit_start)
    {
        return source_map_number(number).map(|number| number * scale);
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
    let mut active_sheet: Option<(u32, Option<(u32, u32)>)> = None;
    let mut sheet_origins: Vec<(u32, (u32, u32))> = Vec::new();
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
                        if input_cwd.is_none() && !page_complete {
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
            let parsed_page = page.parse::<u32>().ok();
            in_page = parsed_page == Some(point.page);
            if in_page && page_complete {
                return Err(io::Error::other(
                    "SyncTeX source map repeats the clicked sheet",
                ));
            }
            active_sheet = parsed_page.map(|p| (p, None));
        } else if in_page && row.starts_with('}') {
            if row[1..].parse::<u32>() != Ok(point.page) || !stack.is_empty() {
                return Err(io::Error::other(
                    "SyncTeX source map has mismatched sheet bounds",
                ));
            }
            page_complete = true;
            in_page = false;
            active_sheet = None;
            if let Some(orig) = origin {
                sheet_origins.push((point.page, orig));
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
        } else if let Some((_, sheet_origin)) = &mut active_sheet {
            if let Some(page) = row.strip_prefix('}') {
                if let Some((p, Some(orig))) = active_sheet.take()
                    && page.parse::<u32>() == Ok(p)
                {
                    sheet_origins.push((p, orig));
                }
            } else if sheet_origin.is_none() && matches!(row.as_bytes().first(), Some(b'[' | b'('))
            {
                *sheet_origin = synctex_node_link(row);
            }
        }
    }
    operation.check()?;
    if !page_complete || ((geometry_required || input_cwd.is_some()) && !postamble_count) {
        return Err(io::Error::other("SyncTeX source map is incomplete"));
    }
    let is_vrb = generated.extension().is_some_and(|ext| ext == "vrb");
    let is_sidecar_tag = |tag: u32| {
        generated_tags.contains(&tag)
            || inputs
                .get(&tag)
                .is_some_and(|path| is_generated_or_sidecar_input(path))
    };
    let adjacent_tags: Vec<(u32, u32)> = match origin {
        Some((tag, _)) if is_sidecar_tag(tag) && !is_vrb => {
            let prev = sheet_origins
                .iter()
                .rev()
                .find(|&&(p, (t, _))| p < point.page && !is_sidecar_tag(t))
                .map(|&(_, orig)| orig);
            let next = sheet_origins
                .iter()
                .find(|&&(p, (t, _))| p > point.page && !is_sidecar_tag(t))
                .map(|&(_, orig)| orig);
            match (prev, next) {
                (Some(p), Some(n)) if n.0 == p.0 => vec![n, p],
                (Some(p), Some(n)) => vec![p, n],
                (Some(p), None) => vec![p],
                (None, Some(n)) => vec![n],
                (None, None) => Vec::new(),
            }
        }
        _ => Vec::new(),
    };
    let frame_slide = origin
        .and_then(|orig| {
            u32::try_from(
                sheet_origins
                    .iter()
                    .filter(|&&(p, o)| p <= point.page && o == orig)
                    .count(),
            )
            .ok()
        })
        .filter(|&n| n > 0);
    let build_map = |inputs: std::collections::HashMap<u32, PathBuf>,
                     generated_on_page: bool,
                     generated_at_point: bool|
     -> io::Result<SourceMap> {
        let main = inputs
            .get(&1)
            .cloned()
            .ok_or_else(|| io::Error::other("SyncTeX source map has no main input"))?;
        let page_source = origin
            .filter(|_| generated_on_page)
            .and_then(|(tag, line)| inputs.get(&tag).cloned().map(|path| (path, line)));
        let adjacent_origins = if generated_on_page {
            adjacent_tags
                .iter()
                .filter_map(|&(tag, line)| inputs.get(&tag).cloned().map(|path| (path, line)))
                .collect()
        } else {
            Vec::new()
        };
        Ok(SourceMap {
            main,
            inputs: inputs.into_values().collect(),
            page_source,
            generated_at_point,
            frame_slide,
            generated: generated.to_path_buf(),
            adjacent_origins,
        })
    };
    if !geometry_required {
        generated_on_page |= page_tags.iter().any(|tag| generated_tags.contains(tag));
        return build_map(inputs, generated_on_page, false);
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
    let generated_at_point = generated_box_at_point(
        &boxes,
        (f64::from(point.x) - x_offset) / visible_unit,
        (f64::from(point.y_from_top) - y_offset) / visible_unit,
        operation,
    )?;
    build_map(inputs, generated_on_page, generated_at_point)
}

/// Beamer reopens the same .vrb for successive fragile frames. The clicked
/// sheet's enclosing source-map box identifies the original frame; the final
/// generated file's contents cannot establish that identity.
fn original_source_from_verbatim(
    map: &SourceMap,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<(String, Rc<String>, u32)> {
    let (original, line) = map.page_source.as_ref().ok_or_else(|| {
        io::Error::other("clicked sheet has no original generated-file provenance")
    })?;
    if is_generated_or_sidecar_input(original) {
        return Err(io::Error::other(
            "clicked sheet does not identify a fragile frame's closing line",
        ));
    }
    let original = sources.normalize(original)?;
    let source = sources.read(&original, operation)?;
    validate_inverse_source_line(&source, *line, &original)?;
    fragile_frame_range(&source, *line)
        .filter(|frame| *line == frame.end as u32)
        .ok_or_else(|| {
            io::Error::other("clicked sheet does not identify a fragile frame's closing line")
        })?;
    let file = original
        .to_str()
        .ok_or_else(|| io::Error::other("original fragile source path is not UTF-8"))?
        .to_owned();
    Ok((file, source, *line))
}

fn original_source_from_generated(
    map: &SourceMap,
    raw_line: u32,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
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
        let original = sources.normalize(candidate)?;
        let source = sources.read(&original, operation)?;
        validate_inverse_source_line(&source, *line, &original)?;
        if host_owns_generated_sidecar(&source, &original, &generated_norm, main_dir.as_deref()) {
            let file = original
                .to_str()
                .ok_or_else(|| io::Error::other("original fragile source path is not UTF-8"))?
                .to_owned();
            return Ok((file, source, *line));
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
    let original = sources.normalize(target_path)?;
    let source = sources.read(&original, operation)?;
    validate_inverse_source_line(&source, line, &original)?;
    let file = original
        .to_str()
        .ok_or_else(|| io::Error::other("original fragile source path is not UTF-8"))?
        .to_owned();
    Ok((file, source, line))
}

fn fragile_frame_range(source: &str, line: u32) -> Option<std::ops::Range<usize>> {
    source_frame_range(source, line).filter(|frame| is_fragile_frame(source, frame))
}

fn is_fragile_frame(source: &str, frame: &std::ops::Range<usize>) -> bool {
    let Some(mut at) = crate::tex::frame_header_after_begin(source, frame.start) else {
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

fn preamble_sources(
    main: &Path,
    compiled_inputs: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<Vec<(PathBuf, Rc<String>)>> {
    operation.check()?;
    let Ok(main) = sources.normalize(main) else {
        return Ok(Vec::new());
    };
    if sources.warning(&main).is_some() {
        return Ok(Vec::new());
    }
    let main_source = match sources.read(&main, operation) {
        Ok(source) => source,
        Err(error) if crate::source_snapshot::required_cache_error(&error) => return Err(error),
        Err(_) => {
            operation.check()?;
            return Ok(Vec::new());
        }
    };
    let compiled: Option<Vec<PathBuf>> = compiled_inputs.map(|inputs| {
        inputs
            .iter()
            .filter_map(|p| sources.normalize(p).ok())
            .filter(|p| p != &main)
            .collect()
    });
    let main_dir = main.parent().unwrap_or_else(|| Path::new(""));
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
        let clean_name = source_line_text(&name).trim();
        if clean_name.is_empty() || clean_name.contains('\\') {
            continue;
        }
        let candidates = input_candidates(
            clean_name,
            "tex",
            &[
                Some(main_dir),
                pdf_dir,
                sources.sidecar_dir.as_deref(),
                sources.snapshot.map(|s| s.cwd.as_path()),
            ],
            compiled.as_deref(),
            sources,
        );
        let mut readable = Vec::with_capacity(candidates.len());
        for cand in candidates {
            if files.iter().any(|(known, _)| *known == cand) || sources.warning(&cand).is_some() {
                continue;
            }
            match sources.read(&cand, operation) {
                Ok(source) => readable.push((cand, source)),
                Err(error) if crate::source_snapshot::required_cache_error(&error) => {
                    return Err(error);
                }
                Err(_) => operation.check()?,
            }
        }
        if readable.len() == 1 {
            files.push(readable.pop().unwrap());
        }
    }
    Ok(files)
}

struct SplicedSource {
    source: String,
    inputs: Vec<PathBuf>,
    input_ends: Vec<Option<usize>>,
    ambiguous_inputs: bool,
    package_initialization: Vec<std::ops::Range<usize>>,
    truncated: bool,
    /// Normalized SyncTeX inputs: one entry per file opening, including reruns.
    compiled: Option<Vec<PathBuf>>,
}

impl SplicedSource {
    fn opaque() -> Self {
        Self {
            source: "\\csname **\\endcsname\n".to_owned(),
            inputs: Vec::new(),
            input_ends: Vec::new(),
            ambiguous_inputs: true,
            package_initialization: Vec::new(),
            truncated: false,
            compiled: None,
        }
    }

    fn guess_source(&self, file: &Path) -> &str {
        if !self.ambiguous_inputs {
            let mut occurrences = self
                .inputs
                .iter()
                .enumerate()
                .filter(|(_, path)| *path == file);
            if let Some((index, _)) = occurrences.next()
                && index > 0
                && occurrences.next().is_none()
                && let Some(end) = self.input_ends[index]
                && match &self.compiled {
                    // Every execution of the clicked file opens it, however it is spelled.
                    Some(compiled) => compiled_once(compiled, file),
                    None => !may_rerun_later(&self.source[end..], file),
                }
            {
                return &self.source[..end];
            }
        }
        &self.source
    }
}

fn compiled_once(compiled: &[PathBuf], file: &Path) -> bool {
    use unicode_normalization::UnicodeNormalization;
    // Default macOS/Windows filesystems fold case and Unicode normalization.
    let fold = |path: &Path| {
        path.to_string_lossy()
            .nfc()
            .collect::<String>()
            .to_lowercase()
    };
    let name = |path: &Path| path.file_name().map(|name| fold(Path::new(name)));
    // Spellings may not resolve here (e.g. output-directory builds); count every
    // opening of the same file name so none can hide a rerun.
    let file_name = name(file);
    compiled
        .iter()
        .filter(|path| name(path) == file_name)
        .count()
        == 1
}

/// Without compiled inputs, unfollowed commands (`\subfile`, `\import`, `\@input`, ...) may execute the
/// clicked file again, directly or through another file, after later mutations.
/// Any such loader, encoded token, or later mention of its name keeps full context.
fn may_rerun_later(rest: &str, file: &Path) -> bool {
    use unicode_normalization::UnicodeNormalization;
    const LOADERS: [&str; 14] = [
        "subfile",
        "import",
        "subimport",
        "inputfrom",
        "includefrom",
        "subinputfrom",
        "subincludefrom",
        "InputIfFileExists",
        "IfFileExists",
        "@input",
        "@@input",
        "@iinput",
        "includestandalone",
        "loadsubfile",
    ];
    let Some(stem) = file.file_stem().and_then(|stem| stem.to_str()) else {
        return true;
    };
    if rest.contains("^^")
        || LOADERS
            .iter()
            .any(|name| rest.contains(&format!("\\{name}")))
    {
        return true;
    }
    // Default macOS/Windows filesystems fold case and Unicode normalization.
    let fold = |text: &str| text.nfc().collect::<String>().to_lowercase();
    let (rest, stem) = (fold(rest), fold(stem));
    let word = |byte: Option<&u8>| byte.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    rest.match_indices(&stem).any(|(start, _)| {
        !word(
            start
                .checked_sub(1)
                .and_then(|before| rest.as_bytes().get(before)),
        ) && !word(rest.as_bytes().get(start + stem.len()))
    })
}

#[derive(Default)]
struct SpliceState {
    visited: Vec<PathBuf>,
    ancestors: [usize; 9],
    inputs: Vec<PathBuf>,
    input_ends: Vec<Option<usize>>,
    ambiguous_inputs: bool,
    local_definitions: std::collections::BTreeSet<String>,
    package_definitions: std::collections::BTreeSet<String>,
    package_ranges: Vec<std::ops::Range<usize>>,
    truncated: bool,
    out: String,
}

fn macro_preamble_source(
    main: &Path,
    compiled_inputs: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<Option<SplicedSource>> {
    operation.check()?;
    let Ok(main) = sources.normalize(main) else {
        return Ok(Some(SplicedSource::opaque()));
    };
    if sources.warning(&main).is_some() {
        return Ok(Some(SplicedSource::opaque()));
    }
    let main_source = match sources.read(&main, operation) {
        Ok(source) => source,
        Err(error) if crate::source_snapshot::required_cache_error(&error) => return Err(error),
        Err(_) => {
            operation.check()?;
            return Ok(Some(SplicedSource::opaque()));
        }
    };
    let mut unresolved = Vec::new();
    let compiled: Option<Vec<PathBuf>> = compiled_inputs.map(|inputs| {
        let mut compiled = Vec::new();
        for path in inputs {
            match sources.normalize(path) {
                Ok(normalized) if normalized != main => compiled.push(normalized),
                Ok(_) => {}
                // Unresolvable spellings still record an opening; keep them for rerun counts.
                Err(_) => unresolved.push(path.clone()),
            }
        }
        compiled
    });
    let main_dir = main.parent().unwrap_or_else(|| Path::new(""));
    let mut state = SpliceState {
        visited: vec![main.clone()],
        out: String::with_capacity(main_source.len()),
        ..Default::default()
    };
    let mut in_preamble = true;
    append_spliced_source(
        &main,
        &main_source,
        main_dir,
        compiled.as_deref(),
        pdf_dir,
        sources,
        operation,
        &mut state,
        0,
        &mut in_preamble,
        false,
        false,
        false,
        true,
    )?;
    Ok((!state.out.is_empty()).then_some(SplicedSource {
        source: state.out,
        inputs: state.inputs,
        input_ends: state.input_ends,
        ambiguous_inputs: state.ambiguous_inputs,
        package_initialization: state.package_ranges,
        truncated: state.truncated,
        compiled: compiled.map(|mut compiled| {
            compiled.extend(unresolved);
            compiled
        }),
    }))
}

// An open input scope invalidates later package-origin classification, not source bytes.
#[allow(clippy::too_many_arguments)]
fn append_spliced_source(
    path: &Path,
    source: &str,
    main_dir: &Path,
    compiled: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
    state: &mut SpliceState,
    depth: usize,
    in_preamble: &mut bool,
    poisoned_scope: bool,
    package_initialization: bool,
    mut imported_scope_poisoned: bool,
    transparent: bool,
) -> io::Result<bool> {
    operation.check()?;
    if depth >= state.ancestors.len() || state.visited.len() >= 32 {
        state.truncated = true;
        state.out.push_str("\\csname **\\endcsname\n");
        return Ok(true);
    }
    state.ancestors[depth] = state.visited.len() - 1;
    let input_index = state.inputs.len();
    state.inputs.push(path.to_owned());
    state.input_ends.push(None);
    let macros = math::macros_from_sources(&[source], operation);
    operation.check()?;
    if package_initialization {
        for &name in macros.keys() {
            if tex::known_formatting_switch(name) && !state.local_definitions.contains(name) {
                state.local_definitions.insert(name.to_owned());
            }
            // Readable package signatures may be delimited or redefine kernel shapes.
            if !state.package_definitions.contains(name) {
                state.package_definitions.insert(name.to_owned());
            }
        }
        let mut commands = source
            .match_indices('\\')
            .map(|(start, _)| math::command(source, start));
        if commands.any(|(cmd, end)| {
            matches!(
                cmd,
                "catcode"
                    | "active"
                    | "newunicodechar"
                    | "DeclareUnicodeCharacter"
                    | "defineshorthand"
                    | "useshorthands"
                    | "declare"
                    | "initiate"
                    | "MakeShortVerb"
            ) || (math::is_macro_definition_command(cmd) && cmd != "csname")
                && !defines_control_sequence(source, end)
        }) {
            state.package_definitions.insert("~".to_owned());
        }
        // Constructed or expl3 definitions are invisible to the signature table.
        if [
            "csname",
            "ExplSyntax",
            "ProvidesExpl",
            "cs_new",
            "cs_set",
            "cs_gset",
            "namedef",
            "\\csdef",
            "\\csgdef",
            "\\csedef",
            "\\csxdef",
            "\\letcs",
            "\\cslet",
            "\\expandafter",
        ]
        .iter()
        .any(|marker| source.contains(marker))
        {
            state.package_definitions.insert("**".to_owned());
        }
    }
    // Rebound environment delimiters require preserving the raw imported text.
    let raw_source = macros.contains_key("begin") && macros.contains_key("end");
    let mut raw_preamble = *in_preamble;
    let in_preamble = if raw_source {
        &mut raw_preamble
    } else {
        in_preamble
    };
    let guess_macros = ((transparent && !raw_source) || (depth > 0 && !package_initialization))
        .then(|| math::macros_from_sources_for_guess(&[source], operation));
    operation.check()?;
    let signatures = if raw_source {
        &macros
    } else {
        guess_macros.as_ref().unwrap_or(&macros)
    };
    let declared_environments: std::collections::BTreeSet<_> =
        if depth > 0 && !package_initialization {
            tex::nonprinting_source_spans(source)
                .filter_map(|(start, _, _)| {
                    operation.check().ok()?;
                    if source.as_bytes().get(start) != Some(&b'\\') {
                        return None;
                    }
                    let (command, end) = math::command(source, start);
                    tex::environment_definition_command(command).then(|| {
                        tex::environment_definition(source, command, end)
                            .map_or("**", |(name, _, _)| name)
                    })
                })
                .collect()
        } else {
            Default::default()
        };
    // shortcut: prove only same-file leaf bodies; model local dependencies before widening.
    let mut body_budget = source.len();
    let benign_local_calls: std::collections::BTreeSet<_> = if depth > 0 && !package_initialization
    {
        guess_macros
            .as_ref()
            .unwrap_or(&macros)
            .iter()
            .filter_map(|(&name, definition)| {
                let body = definition.as_ref()?.body?;
                body_budget = body_budget.checked_sub(body.len())?;
                if state.local_definitions.contains(name)
                    || declared_environments.contains("**")
                    || body.contains('^')
                {
                    return None;
                }
                body.match_indices('\\')
                    .all(|(start, _)| {
                        if operation.check().is_err() {
                            return false;
                        }
                        let (command, _) = math::command(body, start);
                        tex::known_formatting_switch(command)
                            && !macros.contains_key(command)
                            && !state.local_definitions.contains(command)
                            && !declared_environments.contains(command)
                    })
                    .then_some(name)
            })
            .collect()
    } else {
        Default::default()
    };
    operation.check()?;
    // shortcut: control-sequence bodies may consume extra tokens; keep full context until modeled.
    let opaque_calls: Option<std::collections::BTreeSet<_>> =
        (transparent && !raw_source).then(|| {
            signatures
                .iter()
                .filter(|(_, definition)| {
                    definition.is_none_or(|definition| {
                        definition.body.is_none_or(|body| body.contains('\\'))
                    })
                })
                .map(|(&name, _)| name)
                .collect()
        });
    let mut local_environments = std::collections::BTreeSet::new();
    let mut argument_end = 0;
    let mut argument_budget = source.len();
    let mut document_scope = false;
    if poisoned_scope && !raw_source {
        state.out.push_str("{\n");
    }
    let mut nonprinting = tex::nonprinting_source_spans(source).peekable();
    let mut brace_depth = 0usize;
    let mut group_depth = 0usize;
    let mut if_stack: Vec<Option<bool>> = Vec::new();
    let mut raw_inactive_depth = 0;
    let mut unknown_depth = 0;
    let mut uncertain_scope = false;
    let mut cursor = 0usize;
    let mut at = 0usize;
    while at < source.len() {
        if raw_source {
            operation.check()?;
            if if_stack.last() == Some(&Some(false)) {
                if raw_inactive_depth == 0 {
                    raw_inactive_depth = if_stack.len();
                }
                // Only conditional delimiters affect scope inside an inactive branch.
                match source.as_bytes()[at] {
                    b'%' => {
                        at += source[at..].find('\n').unwrap_or(source.len() - at);
                    }
                    b'\\' => {
                        let (cmd, end) = math::command(source, at);
                        at = end;
                        if cmd == "fi" {
                            if_stack.pop();
                            if if_stack.len() < raw_inactive_depth {
                                raw_inactive_depth = 0;
                            }
                            if if_stack.len() < unknown_depth {
                                unknown_depth = 0;
                            }
                        } else if cmd == "else" {
                            if if_stack.len() == raw_inactive_depth {
                                *if_stack.last_mut().unwrap() = Some(true);
                                raw_inactive_depth = 0;
                            }
                        } else if cmd.starts_with("if")
                            && cmd != "iff"
                            && (cmd != "ifthenelse" || macros.contains_key(cmd))
                        {
                            if_stack.push(Some(false));
                        }
                    }
                    _ => at += source[at..].chars().next().unwrap().len_utf8(),
                }
                continue;
            }
        }
        if if_stack.last() == Some(&Some(false)) {
            state.out.push_str("\n\\csname **\\endcsname\n");
            break;
        }
        match source.as_bytes()[at] {
            b'%' => {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
            }
            b'{' | b'}' => {
                if unknown_depth > 0
                    && !nonprinting
                        .peek()
                        .is_some_and(|(_, span, _)| span.contains(&at))
                {
                    uncertain_scope = true;
                }
                if source.as_bytes()[at] == b'{' {
                    brace_depth += 1;
                } else {
                    brace_depth = brace_depth.saturating_sub(1);
                }
                at += 1;
            }
            b'$' => {
                if brace_depth == 0
                    && group_depth == 0
                    && if_stack.is_empty()
                    && !nonprinting
                        .peek()
                        .is_some_and(|(_, span, _)| span.contains(&at))
                {
                    *in_preamble = false;
                }
                at += 1;
            }
            b'\\' => {
                let cmd_start = at;
                while nonprinting
                    .peek()
                    .is_some_and(|(_, span, _)| span.end <= cmd_start)
                {
                    nonprinting.next();
                }
                let (cmd, end, expanded_target) = math::definition_command(source, at);
                at = end;
                let executing = !nonprinting
                    .peek()
                    .is_some_and(|(_, span, _)| span.contains(&cmd_start));
                if executing
                    || nonprinting
                        .peek()
                        .is_some_and(|(start, _, _)| *start == cmd_start)
                {
                    let environment = if transparent || (depth > 0 && !package_initialization) {
                        tex::environment_definition(source, cmd, at)
                    } else {
                        None
                    };
                    if transparent
                        && tex::environment_definition_command(cmd)
                        && environment.is_none_or(|(name, _, _)| name == "**")
                    {
                        argument_end = source.len();
                    }
                    if transparent && let Some((name, _, _)) = environment {
                        local_environments.insert(name);
                    }
                    if depth > 0
                        && !package_initialization
                        && (math::is_macro_definition_command(cmd)
                            || matches!(cmd, "let" | "futurelet")
                            || tex::environment_definition_command(cmd))
                    {
                        // Imported signatures are not in this file's table; only their calls block.
                        let name = if let Some((name, _, _)) = environment {
                            name
                        } else if tex::environment_definition_command(cmd) {
                            "**"
                        } else if matches!(cmd, "let" | "futurelet") {
                            math::macro_alias(source, cmd, at, expanded_target).name
                        } else {
                            let target = skip_source_space(source, at);
                            let target = target
                                + usize::from(matches!(
                                    source.as_bytes().get(target),
                                    Some(b'*' | b'+')
                                ));
                            math::macro_target_name(source, target).map_or("**", |(name, _)| name)
                        };
                        if !state.local_definitions.contains(name) {
                            state.local_definitions.insert(name.to_owned());
                        }
                        if environment.is_some() && name != "**" {
                            state.local_definitions.insert(format!("end{name}"));
                        }
                    }
                    if executing
                        && depth > 0
                        && !state.local_definitions.contains("**")
                        && state.local_definitions.contains(cmd)
                    {
                        let mut benign = benign_local_calls.contains(cmd);
                        if benign {
                            let definition = guess_macros
                                .as_ref()
                                .unwrap_or(&macros)
                                .get(cmd)
                                .and_then(Option::as_ref)
                                .unwrap();
                            let mut end = at;
                            for _ in 0..definition.arity {
                                let Some((argument, next)) =
                                    math::argument(source, end, source.len(), false)
                                else {
                                    benign = false;
                                    break;
                                };
                                let Some(remaining) = argument_budget.checked_sub(next - end)
                                else {
                                    benign = false;
                                    break;
                                };
                                argument_budget = remaining;
                                end = next;
                                if source[argument].contains(['\\', '^']) {
                                    benign = false;
                                    break;
                                }
                            }
                        }
                        if !benign {
                            // Unknown calls can install nested targets absent from the local table.
                            state.local_definitions.insert("**".to_owned());
                        }
                    }
                    if transparent && !raw_source && argument_end < source.len() {
                        if expanded_target
                            || matches!(
                                cmd,
                                "afterassignment"
                                    | "aftergroup"
                                    | "expandafter"
                                    | "futurelet"
                                    | "csname"
                            )
                            || executing
                                && (state.local_definitions.contains(cmd)
                                    || opaque_calls
                                        .as_ref()
                                        .is_some_and(|calls| calls.contains(cmd)))
                        {
                            argument_end = source.len();
                        } else if executing && let Some(Some(definition)) = signatures.get(cmd) {
                            let mut end = at;
                            for _ in 0..definition.arity {
                                let Some((_, next)) =
                                    math::argument(source, end, source.len(), false)
                                else {
                                    end = source.len();
                                    break;
                                };
                                // Bound overlapping scans; exhaustion only retains the full context.
                                let Some(remaining) = argument_budget.checked_sub(next - end)
                                else {
                                    end = source.len();
                                    break;
                                };
                                argument_budget = remaining;
                                end = next;
                            }
                            argument_end = argument_end.max(end);
                        } else if !signatures.contains_key(cmd)
                            && (math::is_macro_definition_command(cmd)
                                || matches!(cmd, "let" | "futurelet"))
                        {
                            // A redefined active character can consume later inputs unseen.
                            if !defines_control_sequence(source, at) {
                                argument_end = source.len();
                            }
                        } else if executing
                            && !signatures.contains_key(cmd)
                            && !tex::environment_definition_command(cmd)
                            && !matches!(
                                cmd,
                                "input"
                                    | "include"
                                    | "usepackage"
                                    | "RequirePackage"
                                    | "documentclass"
                                    | "begin"
                                    | "end"
                            )
                        {
                            // Without a local signature a call may take up to nine
                            // undelimited arguments, so it can reorder a following input
                            // and later tokens. Known kernel shapes bound their reach;
                            // unbound package commands otherwise remain the warned policy.
                            argument_end = if state.package_definitions.contains(cmd)
                                || state.package_definitions.contains("**")
                            {
                                source.len()
                            } else {
                                argument_end.max(
                                    unsigned_call_reach(source, cmd, at, &mut argument_budget)
                                        .unwrap_or(source.len()),
                                )
                            };
                        }
                    }
                }
                if !macros.contains_key(cmd)
                    && let Some(quoted_end) = math::quoted_token_end(source, cmd, at)
                {
                    // Stored token lists balance braces before quoted tokens execute.
                    if executing
                        || !matches!(
                            source.as_bytes().get(skip_source_space(source, at)),
                            Some(b'{' | b'}')
                        )
                    {
                        at = quoted_end;
                    }
                    continue;
                }
                if executing
                    && !macros.contains_key(cmd)
                    && let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, at)
                {
                    at = opaque_end;
                    continue;
                }
                if executing && matches!(cmd, "(" | "[") {
                    if brace_depth == 0 && group_depth == 0 && if_stack.is_empty() {
                        *in_preamble = false;
                    }
                    continue;
                }
                if unknown_depth > 0
                    && executing
                    && matches!(
                        cmd,
                        "begingroup" | "bgroup" | "endgroup" | "egroup" | "begin" | "end"
                    )
                {
                    uncertain_scope = true;
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
                    if let Some((env, next)) = math::group(source, skip_source_space(source, at)) {
                        let name = &source[env.clone()];
                        if local_environments.contains(name)
                            || state.local_definitions.contains(name)
                        {
                            if depth > 0
                                && !state.local_definitions.contains("**")
                                && state.local_definitions.contains(name)
                            {
                                state.local_definitions.insert("**".to_owned());
                            }
                            argument_end = source.len();
                        }
                        if name == "document"
                            && brace_depth == 0
                            && group_depth == 0
                            && if_stack.is_empty()
                            && !signatures.contains_key("begin")
                        {
                            document_scope = true;
                        }
                        if &source[env] == "document"
                            || (brace_depth == 0 && group_depth == 0 && if_stack.is_empty())
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
                    if let Some((env, next)) = math::group(source, skip_source_space(source, at)) {
                        let name = &source[env];
                        if local_environments.contains(name)
                            || state.local_definitions.contains(name)
                        {
                            if depth > 0
                                && !state.local_definitions.contains("**")
                                && state.local_definitions.contains(name)
                            {
                                state.local_definitions.insert("**".to_owned());
                            }
                            argument_end = source.len();
                        }
                        if name == "document" {
                            document_scope = false;
                        }
                        at = next;
                    }
                    continue;
                }
                if matches!(cmd, "csname" | "**") {
                    continue;
                }
                if matches!(cmd, "let" | "futurelet") {
                    if executing {
                        at = math::macro_alias(source, cmd, at, expanded_target).nonprinting_end;
                    }
                    continue;
                }
                if math::is_macro_definition_command(cmd) {
                    let mut next =
                        at + usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
                    next = skip_source_space(source, next);
                    if let Some(target_end) = math::macro_target_end(source, next) {
                        at = target_end;
                    }
                    continue;
                }
                if brace_depth == 0 && executing {
                    if cmd == "iffalse" {
                        if_stack.push(Some(false));
                        continue;
                    }
                    if cmd == "iftrue" {
                        if_stack.push(Some(true));
                        continue;
                    }
                    if cmd.starts_with("if")
                        && cmd != "iff"
                        && (cmd != "ifthenelse" || macros.contains_key(cmd))
                    {
                        if_stack.push(None);
                        if unknown_depth == 0 {
                            unknown_depth = if_stack.len();
                        }
                        continue;
                    }
                    if cmd == "else" {
                        if let Some(last) = if_stack.last_mut() {
                            *last = last.map(|b| !b);
                        }
                        continue;
                    }
                    if cmd == "fi" {
                        if_stack.pop();
                        if if_stack.len() < unknown_depth {
                            unknown_depth = 0;
                        }
                        continue;
                    }
                }
                if executing && matches!(cmd, "input" | "include" | "usepackage" | "RequirePackage")
                {
                    let is_tex_input = matches!(cmd, "input" | "include");
                    if raw_source && is_tex_input {
                        // Raw local inputs remain unexpanded and cannot prove a closed scope.
                        uncertain_scope = true;
                        continue;
                    }
                    let mut imports: Vec<(&str, &str)> = Vec::new();
                    if is_tex_input {
                        let arg_pos = skip_source_space(source, at);
                        if let Some((grp, next)) = math::group(source, arg_pos) {
                            at = next;
                            imports.push((&source[grp], "tex"));
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
                                imports.push((&source[arg_pos..next], "tex"));
                            }
                        }
                    } else {
                        let mut pos = at;
                        while let Some(opt_end) = optional_argument_end(source, pos) {
                            pos = opt_end;
                        }
                        if let Some((grp, mut next)) =
                            math::group(source, skip_source_space(source, pos))
                        {
                            while let Some(opt_end) = optional_argument_end(source, next) {
                                next = opt_end;
                            }
                            at = next;
                            for pkg in source[grp].split(',') {
                                imports.push((pkg, "sty"));
                            }
                        }
                    }
                    if imports.is_empty() {
                        if raw_source {
                            uncertain_scope = true;
                            continue;
                        }
                        if is_tex_input {
                            state.ambiguous_inputs = true;
                            state.out.push_str(&source[cursor..cmd_start]);
                            state.out.push_str("\n\\csname **\\endcsname\n");
                            cursor = at;
                        }
                        continue;
                    }
                    if !raw_source {
                        let segment_start = state.out.len();
                        state.out.push_str(&source[cursor..cmd_start]);
                        if package_initialization
                            && *in_preamble
                            && !imported_scope_poisoned
                            && !uncertain_scope
                            && segment_start < state.out.len()
                        {
                            state.package_ranges.push(segment_start..state.out.len());
                        }
                        state.out.push('\n');
                        cursor = at;
                    }
                    let child_poisoned = poisoned_scope
                        || !*in_preamble
                        || brace_depth > 0
                        || group_depth > 0
                        || !if_stack.is_empty();
                    for (raw_name, ext) in imports {
                        operation.check()?;
                        if raw_source
                            && !raw_name.trim_ascii().bytes().all(|byte| {
                                byte.is_ascii_alphanumeric()
                                    || matches!(byte, b'.' | b'_' | b'-' | b'/')
                            })
                        {
                            // Encoded/active/commented filenames cannot prove an absent local load.
                            uncertain_scope = true;
                            continue;
                        }
                        let clean_name = source_line_text(raw_name).trim();
                        if clean_name.is_empty() || clean_name.contains('\\') {
                            if raw_source {
                                uncertain_scope = true;
                                continue;
                            }
                            if is_tex_input {
                                state.ambiguous_inputs = true;
                            }
                            if ext == "tex" || clean_name.contains('\\') {
                                state.out.push_str("\\csname **\\endcsname\n");
                            }
                            continue;
                        }
                        let candidates = input_candidates(
                            clean_name,
                            ext,
                            &[
                                Some(main_dir),
                                path.parent(),
                                pdf_dir,
                                sources.sidecar_dir.as_deref(),
                                sources.snapshot.map(|s| s.cwd.as_path()),
                            ],
                            compiled,
                            sources,
                        );
                        let mut readable = Vec::with_capacity(candidates.len());
                        for cand in candidates {
                            match sources.read(&cand, operation) {
                                Ok(child_src) => readable.push((cand, child_src)),
                                Err(err) if crate::source_snapshot::required_cache_error(&err) => {
                                    return Err(err);
                                }
                                Err(_) => operation.check()?,
                            }
                        }
                        if raw_source {
                            // Reuse normal package resolution without appending local package bytes.
                            uncertain_scope |= !readable.is_empty();
                            continue;
                        }
                        if readable.is_empty() {
                            if ext == "tex" {
                                state.ambiguous_inputs = true;
                                state.out.push_str("\\csname **\\endcsname\n");
                            }
                            continue;
                        }
                        let ambiguous_cwd = readable.len() > 1;
                        if ambiguous_cwd {
                            state.ambiguous_inputs = true;
                            state.out.push_str("\\csname **\\endcsname\n");
                        }
                        for (resolved, child_src) in readable {
                            if !is_tex_input && state.visited.contains(&resolved) {
                                continue;
                            }
                            // Repeated inputs execute again; only ancestors form a cycle.
                            if state.visited.len() >= 32
                                || state.ancestors[..=depth]
                                    .iter()
                                    .any(|&index| state.visited[index] == resolved)
                            {
                                state.truncated = true;
                                state.out.push_str("\\csname **\\endcsname\n");
                                continue;
                            }
                            state.visited.push(resolved.clone());
                            let warn_poisoned = sources.warning(&resolved).is_some();
                            if warn_poisoned {
                                state.ambiguous_inputs = true;
                                state.out.push_str("\\csname **\\endcsname\n");
                            }
                            let child_transparent = transparent
                                && is_tex_input
                                && resolved.extension().is_some_and(|ext| ext == "tex")
                                && !raw_source
                                && !imported_scope_poisoned
                                && !uncertain_scope
                                && !state.local_definitions.contains("**")
                                && !ambiguous_cwd
                                && !warn_poisoned
                                && brace_depth == 0
                                && group_depth == usize::from(document_scope)
                                && if_stack.is_empty()
                                && !state.package_definitions.contains("~")
                                && cmd_start >= argument_end;
                            let child_open_scope = append_spliced_source(
                                &resolved,
                                &child_src,
                                main_dir,
                                compiled,
                                pdf_dir,
                                sources,
                                operation,
                                state,
                                depth + 1,
                                in_preamble,
                                child_poisoned || ambiguous_cwd || warn_poisoned,
                                !child_poisoned
                                    && !imported_scope_poisoned
                                    && !uncertain_scope
                                    && !ambiguous_cwd
                                    && !warn_poisoned
                                    && (package_initialization || !is_tex_input),
                                imported_scope_poisoned,
                                child_transparent,
                            )?;
                            imported_scope_poisoned |= child_open_scope;
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
    let segment_start = state.out.len();
    state.out.push_str(&source[cursor..]);
    if package_initialization
        && (raw_source || *in_preamble)
        && !imported_scope_poisoned
        && !uncertain_scope
        && segment_start < state.out.len()
    {
        state.package_ranges.push(segment_start..state.out.len());
    }
    state.out.push('\n');
    if poisoned_scope && !raw_source {
        state.out.push_str("}\n");
    }
    if transparent {
        state.input_ends[input_index] = Some(state.out.len());
    }
    Ok(imported_scope_poisoned
        || uncertain_scope
        || brace_depth > 0
        || group_depth > 0
        || !if_stack.is_empty())
}

/// Whether a definition command at `at` names a control sequence, not an active character.
fn defines_control_sequence(source: &str, at: usize) -> bool {
    let mut target = skip_source_space(source, at);
    if matches!(source.as_bytes().get(target), Some(b'*' | b'+')) {
        target = skip_source_space(source, target + 1);
    }
    if source.as_bytes().get(target) == Some(&b'{') {
        target = skip_source_space(source, target + 1);
    }
    source.as_bytes().get(target) == Some(&b'\\')
}

/// Source end of arguments an executed call without a local signature may take.
/// Unknown calls reach nine undelimited arguments; delimited parameters of
/// unreadable package commands are outside this bound.
fn unsigned_call_reach(source: &str, cmd: &str, at: usize, budget: &mut usize) -> Option<usize> {
    let arity = match cmd {
        "par" | "relax" | "maketitle" | "tableofcontents" | "listoffigures" | "listoftables"
        | "clearpage" | "cleardoublepage" | "newpage" | "frontmatter" | "mainmatter"
        | "backmatter" | "appendix" | "noindent" | "centering" | "smallskip" | "medskip"
        | "bigskip" | "makeatletter" | "makeatother" => return Some(at),
        "part" | "chapter" | "section" | "subsection" | "subsubsection" | "paragraph"
        | "subparagraph" => {
            let mut end = skip_source_space(source, at);
            if source.as_bytes().get(end) == Some(&b'*') {
                end += 1;
            }
            let end = tex::optional_argument_boundary(source, end).unwrap_or(end);
            let (_, end) = math::group(source, skip_source_space(source, end))?;
            return Some(end);
        }
        "label" | "title" | "author" | "date" | "pagestyle" | "thispagestyle" => {
            let (_, end) = math::group(source, skip_source_space(source, at))?;
            return Some(end);
        }
        _ => 9,
    };
    let mut end = at;
    for _ in 0..arity {
        let Some((_, next)) = math::argument(source, end, source.len(), false) else {
            return Some(source.len());
        };
        *budget = budget.checked_sub(next - end)?;
        end = next;
    }
    Some(end)
}

/// Title pages and running headers can be produced from declarations far
/// outside SyncTeX's reported frame. Search only literal document metadata,
/// including direct preamble inputs, and require neighboring PDF words.
#[cfg(test)]
fn document_metadata_word_location(
    main: &Path,
    context: &str,
    offset: usize,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<Option<MetadataLocation>> {
    document_metadata_word_location_with_context(
        main, None, None, context, offset, sources, operation,
    )
}

fn document_metadata_word_location_with_context(
    main: &Path,
    compiled_inputs: Option<&[PathBuf]>,
    pdf_dir: Option<&Path>,
    context: &str,
    offset: usize,
    sources: &mut QuerySources<'_>,
    operation: &Operation,
) -> io::Result<Option<MetadataLocation>> {
    let mut files = preamble_sources(main, compiled_inputs, pdf_dir, sources, operation)?;
    if files.is_empty() {
        return Ok(None);
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
                        let candidate = source_prose_scored_location(
                            source,
                            1,
                            context,
                            offset,
                            first..last + 1,
                            true,
                            &hidden,
                            Some(span),
                            operation,
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
                    graphics: None,
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
