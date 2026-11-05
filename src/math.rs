//! Conservative lexical matching, not a TeX macro expander.
use crate::symbols::{MathCommand, canonical, classify_command, negated_symbol};
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MacroDef<'a> {
    pub(super) arity: u8,
    pub(super) used_params: u16,
    pub(super) body_source: &'a str,
    pub(super) body: Option<&'a str>,
}

#[derive(Clone, Debug)]
struct Atom {
    value: char,
    span: Range<usize>,
    word_join: bool,
}

fn append_joined(atoms: &mut Vec<Atom>, value: char, span: Range<usize>, mut join: bool) {
    for value in canonical(value).filter(|c| !c.is_whitespace()) {
        let word_join = join && value.is_alphanumeric();
        atoms.push(Atom {
            value,
            span: span.clone(),
            word_join,
        });
        join = value.is_alphanumeric();
    }
}

fn append(atoms: &mut Vec<Atom>, value: char, span: Range<usize>) {
    append_joined(atoms, value, span, false);
}

fn append_words(atoms: &mut Vec<Atom>, text: &str, span: Range<usize>) {
    for word in text.split_whitespace() {
        let mut join = false;
        for c in word.chars() {
            let before = atoms.len();
            append_joined(atoms, c, span.clone(), join);
            join |= atoms.len() > before;
        }
    }
}

pub(super) fn command(source: &str, start: usize) -> (&str, usize) {
    let after = start + 1;
    let end = if source
        .as_bytes()
        .get(after)
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
    {
        after
            + source[after..]
                .bytes()
                .take_while(|b| b.is_ascii_alphabetic() || *b == b'@')
                .count()
    } else {
        after + source[after..].chars().next().map_or(0, char::len_utf8)
    };
    (&source[after..end], end)
}

fn literal_csname(source: &str) -> Option<&str> {
    // Only blanks skipped after the control word are absent from the name.
    let name = source.trim_start_matches([' ', '\t', '\r', '\n']);
    (!name.is_empty()
        && !name.contains("^^")
        && !name
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '\\' | '#' | '%' | '{' | '}')))
    .then_some(name)
}

// Name-only scans must not repeatedly balance unresolved constructs.
pub(super) fn static_command(source: &str, start: usize) -> (&str, usize) {
    let (raw, end) = command(source, start);
    if raw != "csname" {
        return (raw, end);
    }
    let Some(close) = source[end..].find('\\').map(|at| end + at) else {
        return (raw, end);
    };
    let Some(name) = literal_csname(&source[end..close]) else {
        return (raw, end);
    };
    let (closing, closing_end) = command(source, close);
    if closing == "endcsname" {
        (name, closing_end)
    } else {
        (raw, end)
    }
}

pub(super) fn control_sequence(source: &str, start: usize) -> (&str, usize) {
    let (name, mut at) = command(source, start);
    if name != "csname" {
        return (name, at);
    }
    let begin = at;
    let mut depth = 1;
    while at < source.len() {
        if matches!(source.as_bytes()[at], b'{' | b'}') {
            return ("csname", begin);
        }
        if source.as_bytes()[at] == b'%' {
            at += source[at..].find('\n').map_or(source.len() - at, |n| n + 1);
            continue;
        }
        if source.as_bytes()[at] == b'\\' {
            let (token, end) = command(source, at);
            if matches!(token, "string" | "meaning" | "noexpand")
                && source.as_bytes().get(super::skip_source_space(source, end)) == Some(&b'\\')
                && let Some(next) = quoted_token_end(source, token, end)
            {
                at = next;
                continue;
            }
            if token == "endcsname" {
                depth -= 1;
                if depth == 0 {
                    return (literal_csname(&source[begin..at]).unwrap_or("csname"), end);
                }
            } else if token == "csname" {
                depth += 1;
            }
            at = end;
        } else {
            at += source[at..].chars().next().unwrap().len_utf8();
        }
    }
    ("csname", begin)
}

pub(super) fn group(source: &str, start: usize) -> Option<(Range<usize>, usize)> {
    group_with_boundaries(source, start, None, None)
}

fn group_with_boundaries(
    source: &str,
    start: usize,
    mut boundaries: Option<&mut BTreeMap<usize, usize>>,
    operation: Option<&crate::Operation>,
) -> Option<(Range<usize>, usize)> {
    if operation.is_some_and(|operation| operation.check().is_err()) {
        return None;
    }
    let start = super::skip_source_space(source, start);
    if source.as_bytes().get(start) != Some(&b'{') {
        return None;
    }
    // Byte addresses identify the same borrowed group across root/body slices.
    let base = source.as_ptr() as usize;
    if let Some(end) = boundaries
        .as_ref()
        .and_then(|groups| groups.get(&(base + start)))
    {
        let end = *end - base;
        return (end <= source.len()).then_some((start + 1..end - 1, end));
    }
    let mut openings = Vec::new();
    if boundaries.is_some() {
        openings.push(base + start);
    }
    let mut depth = 1;
    let mut i = start + 1;
    while i < source.len() {
        if operation.is_some_and(|operation| operation.check().is_err()) {
            return None;
        }
        match source.as_bytes()[i] {
            b'\\' => {
                i = command(source, i).1;
                continue;
            }
            b'%' => {
                i += source[i..].find('\n').unwrap_or(source.len() - i);
                continue;
            }
            b'{' => {
                depth += 1;
                if boundaries.is_some() {
                    openings.push(base + i);
                }
            }
            b'}' => {
                depth -= 1;
                if let Some(groups) = &mut boundaries {
                    groups.insert(openings.pop().unwrap(), base + i + 1);
                }
                if depth == 0 {
                    return Some((start + 1..i, i + 1));
                }
            }
            _ => {}
        }
        i += source[i..].chars().next()?.len_utf8();
    }
    None
}

pub(super) fn argument(
    source: &str,
    pos: usize,
    limit: usize,
    parameters: bool,
) -> Option<(Range<usize>, usize)> {
    argument_with_boundaries(source, pos, limit, parameters, None, None)
}

fn argument_with_boundaries(
    source: &str,
    pos: usize,
    limit: usize,
    parameters: bool,
    boundaries: Option<&mut BTreeMap<usize, usize>>,
    operation: Option<&crate::Operation>,
) -> Option<(Range<usize>, usize)> {
    let mut pos = pos;
    let mut newline = false;
    while pos < limit {
        match source.as_bytes()[pos] {
            b'%' => {
                pos += source[pos..limit].find('\n').map_or(limit - pos, |n| n + 1);
                newline = true;
            }
            b'\n' if newline => return None,
            b' ' | b'\t' | b'\r' | b'\n' => {
                newline |= source.as_bytes()[pos] == b'\n';
                pos += 1;
            }
            _ => break,
        }
    }
    match *source.as_bytes()[..limit].get(pos)? {
        b'{' => group_with_boundaries(source, pos, boundaries, operation)
            .filter(|(_, end)| *end <= limit),
        b'\\' => {
            let (_, end) = command(source, pos);
            (end <= limit).then_some((pos..end, end))
        }
        b'#' if parameters => source.as_bytes()[..limit]
            .get(pos + 1)
            .is_some_and(|b| matches!(b, b'1'..=b'9'))
            .then_some((pos..pos + 2, pos + 2)),
        _ => source[pos..limit]
            .chars()
            .next()
            .map(|ch| (pos..pos + ch.len_utf8(), pos + ch.len_utf8())),
    }
}

pub(super) fn quoted_token_end(source: &str, name: &str, at: usize) -> Option<usize> {
    if !matches!(name, "string" | "meaning" | "noexpand") {
        return None;
    }
    let mut at = at;
    let mut newline = false;
    while let Some(ch) = source[at..].chars().next() {
        if ch == '%' {
            at += source[at..].find('\n').map_or(source.len() - at, |n| n + 1);
            newline = true;
        } else if matches!(ch, ' ' | '\t' | '\r' | '\n') {
            at += ch.len_utf8();
            // A blank physical line supplies the quoted \par token.
            if ch == '\n' {
                if newline {
                    return Some(at);
                }
                newline = true;
            }
        } else {
            break;
        }
    }
    if source.as_bytes().get(at) == Some(&b'\\') {
        let (token, end) = command(source, at);
        if name == "noexpand"
            && matches!(
                token,
                "def"
                    | "gdef"
                    | "edef"
                    | "xdef"
                    | "let"
                    | "futurelet"
                    | "setbox"
                    | "begingroup"
                    | "endgroup"
                    | "bgroup"
                    | "egroup"
                    | "endcsname"
            )
        {
            return None;
        }
        Some(end)
    } else if name == "noexpand" {
        None
    } else {
        Some(at + source[at..].chars().next().map_or(0, char::len_utf8))
    }
}

/// Return the opaque command's end and whether its entire span is nonprinting.
pub(super) fn opaque_command_end(source: &str, name: &str, mut at: usize) -> Option<(usize, bool)> {
    match name {
        "verb" => {
            at += usize::from(source.as_bytes().get(at) == Some(&b'*'));
            let delimiter = source[at..].chars().next()?;
            at += delimiter.len_utf8();
            Some((
                at + source[at..]
                    .find(delimiter)
                    .map_or(source.len() - at, |n| n + delimiter.len_utf8()),
                false,
            ))
        }
        "lstinline" => {
            if let Some(opt_end) = super::optional_argument_end(source, at) {
                at = opt_end;
            }
            let at = super::skip_source_space(source, at);
            if source.as_bytes().get(at) == Some(&b'{') {
                return group(source, at).map(|(_, end)| (end, false));
            }
            let delimiter = source[at..].chars().next()?;
            let after = at + delimiter.len_utf8();
            Some((
                after
                    + source[after..]
                        .find(delimiter)
                        .map_or(source.len() - after, |n| n + delimiter.len_utf8()),
                false,
            ))
        }
        "begin" => {
            let (body, end) = group(source, super::skip_source_space(source, at))?;
            let environment = &source[body];
            if !matches!(
                environment.trim_end_matches('*'),
                "verbatim"
                    | "Verbatim"
                    | "BVerbatim"
                    | "LVerbatim"
                    | "lstlisting"
                    | "minted"
                    | "comment"
            ) {
                return None;
            }
            if environment.trim_end_matches('*') == "comment" {
                // comment.sty compares entire subsequent input lines, not
                // substrings; TeX removes trailing ASCII spaces on input.
                let Some(newline) = source[end..].find('\n') else {
                    return Some((source.len(), true));
                };
                let mut at = end + newline + 1;
                for row in source[at..].split_inclusive('\n') {
                    let text = row.strip_suffix('\n').unwrap_or(row);
                    let text = text
                        .strip_suffix('\r')
                        .unwrap_or(text)
                        .trim_end_matches(' ');
                    at += row.len();
                    if text
                        .strip_prefix("\\end{")
                        .and_then(|text| text.strip_suffix('}'))
                        == Some(environment)
                    {
                        return Some((at, true));
                    }
                }
                return Some((source.len(), true));
            }
            if matches!(
                environment.trim_end_matches('*'),
                "Verbatim" | "BVerbatim" | "LVerbatim" | "minted"
            ) {
                // Fancyvrb checks only the first \end{ marker on each subsequent line.
                let Some(newline) = source[end..].find('\n') else {
                    return Some((source.len(), false));
                };
                let mut at = end + newline + 1;
                for row in source[at..].split_inclusive('\n') {
                    at += row.len();
                    if row
                        .find("\\end{")
                        .and_then(|offset| row[offset + 5..].strip_prefix(environment))
                        .is_some_and(|rest| rest.starts_with('}'))
                    {
                        return Some((at, false));
                    }
                }
                return Some((source.len(), false));
            }
            Some((
                source[end..]
                    .match_indices("\\end{")
                    .find_map(|(offset, _)| {
                        source[end + offset + 5..]
                            .strip_prefix(environment)?
                            .strip_prefix('}')?;
                        Some(end + offset + 5 + environment.len() + 1)
                    })
                    .unwrap_or(source.len()),
                false,
            ))
        }
        _ => None,
    }
}

fn collected_formatting_command(name: &str) -> bool {
    matches!(
        name,
        "text"
            | "textbf"
            | "textit"
            | "textrm"
            | "textsf"
            | "texttt"
            | "textup"
            | "textnormal"
            | "textsc"
            | "textsl"
            | "textmd"
            | "textlf"
            | "textulc"
            | "emph"
            | "bm"
            | "boldsymbol"
            | "mathcal"
            | "mathbb"
            | "mathbbm"
            | "mathbbb"
            | "mathds"
            | "mathfrak"
            | "mathscr"
            | "mathrm"
            | "mathbf"
            | "mathbfit"
            | "mathsf"
            | "mathsfit"
            | "mathbfsfit"
            | "mathbfcal"
            | "mathbfscr"
            | "mathtt"
            | "mathit"
            | "mathnormal"
            | "mathbold"
            | "symrm"
            | "symit"
            | "symsf"
            | "symtt"
            | "symbb"
            | "symcal"
            | "symscr"
            | "symfrak"
            | "symnormal"
            | "symbf"
            | "symbfit"
            | "symbfsfit"
            | "symsfit"
            | "symbfcal"
            | "symbffrak"
            | "symbfscr"
            | "symbfsf"
            | "symbftt"
    )
}

pub(super) fn collected_text_arguments(
    source: &str,
    name: &str,
    mut at: usize,
) -> Option<(Option<Range<usize>>, Range<usize>, usize)> {
    match name {
        name if collected_formatting_command(name) => {
            let (body, end) = group(source, super::skip_source_space(source, at))?;
            // Single-line formatting does not establish an exclusive search region.
            source[body.clone()]
                .contains('\n')
                .then_some((None, body, end))
        }
        "caption" | "subcaption" => {
            at += usize::from(source.as_bytes().get(at) == Some(&b'*'));
            if let Some(opt_end) = super::optional_argument_end(source, at) {
                at = opt_end;
            }
            let (body, end) = group(source, super::skip_source_space(source, at))?;
            Some((None, body, end))
        }
        "captionof" => {
            at += usize::from(source.as_bytes().get(at) == Some(&b'*'));
            let (_, end) = group(source, super::skip_source_space(source, at))?;
            at = end;
            if let Some(opt_end) = super::optional_argument_end(source, at) {
                at = opt_end;
            }
            let (body, end) = group(source, super::skip_source_space(source, at))?;
            Some((None, body, end))
        }
        "subcaptionbox" | "captionbox" => {
            at += usize::from(source.as_bytes().get(at) == Some(&b'*'));
            if let Some(opt_end) = super::optional_argument_end(source, at) {
                at = opt_end;
            }
            let (heading, mut at) = group(source, super::skip_source_space(source, at))?;
            for _ in 0..2 {
                if let Some(opt_end) = super::optional_argument_end(source, at) {
                    at = opt_end;
                }
            }
            let (contents, end) = group(source, super::skip_source_space(source, at))?;
            Some((Some(heading), contents, end))
        }
        "subfloat" => {
            let mut opts = Vec::with_capacity(2);
            for _ in 0..2 {
                let opt_start = super::skip_source_space(source, at);
                if let Some(opt_end) = super::optional_argument_end(source, opt_start) {
                    opts.push(opt_start + 1..opt_end - 1);
                    at = opt_end;
                } else {
                    break;
                }
            }
            let (contents, end) = group(source, super::skip_source_space(source, at))?;
            Some((opts.pop(), contents, end))
        }
        _ => None,
    }
}

/// Find the outer collected printed argument owning this literal cursor byte.
pub(super) fn collected_text_argument_at(source: &str, byte: usize) -> Option<Range<usize>> {
    if byte >= source.len() || !source.is_char_boundary(byte) {
        return None;
    }
    let mut hidden = super::nonprinting_source_spans(source).peekable();
    let mut body = None;
    let mut at = 0;
    while at <= byte {
        while hidden.peek().is_some_and(|(_, range, _)| range.end <= at) {
            hidden.next();
        }
        if let Some((start, range, _)) = hidden.peek()
            && *start <= at
        {
            at = range.end;
            if byte < at {
                return None;
            }
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
                if byte < at {
                    return None;
                }
            }
            b'\\' => {
                let (name, end) = command(source, at);
                at = end;
                if let Some(end) = quoted_token_end(source, name, at)
                    .or_else(|| opaque_command_end(source, name, at).map(|(end, _)| end))
                {
                    at = end;
                    if byte < at {
                        return None;
                    }
                } else if let Some((first, second, end)) =
                    collected_text_arguments(source, name, at)
                {
                    if let Some(first) = first.filter(|first| first.contains(&byte)) {
                        at = first.start;
                        body.get_or_insert(first.start..second.end);
                    } else if second.contains(&byte) {
                        at = second.start;
                        body.get_or_insert(second);
                    } else {
                        at = end;
                        if byte < at {
                            return None;
                        }
                    }
                }
            }
            _ => {
                let next_special = source.as_bytes()[at..]
                    .iter()
                    .position(|b| matches!(*b, b'%' | b'\\'))
                    .map_or(source.len(), |pos| at + pos);
                let limit = hidden
                    .peek()
                    .map_or(next_special, |(start, _, _)| next_special.min(*start));
                at = limit.max(at + source[at..].chars().next()?.len_utf8());
            }
        }
    }
    body
}

pub(super) fn math_environment(name: &str) -> bool {
    matches!(
        name.trim_end_matches('*'),
        "math"
            | "displaymath"
            | "equation"
            | "align"
            | "aligned"
            | "alignat"
            | "alignedat"
            | "flalign"
            | "gather"
            | "gathered"
            | "multline"
            | "eqnarray"
            | "xalignat"
            | "xxalignat"
            | "matrix"
            | "pmatrix"
            | "bmatrix"
            | "Bmatrix"
            | "vmatrix"
            | "Vmatrix"
            | "smallmatrix"
            | "cases"
            | "dcases"
            | "rcases"
            | "split"
    )
}

/// Only closed, literal math regions qualify; escaped dollars and comments do not.
pub(super) fn in_region(source: &str, byte: usize) -> bool {
    regions(source, byte.saturating_add(1))
        .into_iter()
        .any(|range| range.contains(&byte))
}

pub(super) fn regions(source: &str, limit: usize) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut hidden = super::nonprinting_source_spans(source).peekable();
    let mut open: Option<(&str, usize)> = None;
    let mut i = 0;
    while i < source.len() {
        let Some(rel) = source.as_bytes()[i..]
            .iter()
            .position(|b| matches!(*b, b'%' | b'$' | b'\\'))
        else {
            break;
        };
        i += rel;
        if open.is_none() {
            if i >= limit {
                break;
            }
            while hidden.peek().is_some_and(|(_, range, _)| range.end <= i) {
                hidden.next();
            }
            if let Some((start, range, _)) = hidden.peek()
                && *start <= i
            {
                i = range.end;
                continue;
            }
        }
        let start = i;
        let ch = source.as_bytes()[i] as char;
        i += 1;
        match ch {
            '%' => i += source[i..].find('\n').unwrap_or(source.len() - i),
            '$' => {
                let delimiter = if (open.is_none()
                    || open.is_some_and(|(expected, _)| expected == "$$"))
                    && source[i..].starts_with('$')
                {
                    i += 1;
                    "$$"
                } else {
                    "$"
                };
                match open {
                    None => open = Some((delimiter, i)),
                    Some((expected, begin)) if expected == delimiter => {
                        result.push(begin..start);
                        open = None;
                    }
                    _ => {}
                }
            }
            '\\' => {
                let (name, end) = command(source, start);
                i = end;
                if open.is_none()
                    && let Some((end, _)) = opaque_command_end(source, name, i)
                {
                    i = end;
                    continue;
                }
                match (name, open) {
                    ("(" | "[", None) => open = Some((if name == "(" { ")" } else { "]" }, i)),
                    (")" | "]", Some((expected, begin))) if name == expected => {
                        result.push(begin..start);
                        open = None;
                    }
                    ("begin" | "end", _) => {
                        if let Some((span, end)) = group(source, i) {
                            let environment = &source[span];
                            i = end;
                            if name == "begin" && open.is_none() && math_environment(environment) {
                                open = Some((environment, start));
                            } else if let Some((expected, begin)) = open
                                && name == "end"
                                && expected == environment
                            {
                                result.push(begin..start);
                                open = None;
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    result
}

struct DisplayRegion<'a> {
    range: Range<usize>,
    start: usize,
    first: usize,
    last: usize,
    env: Option<(&'a str, usize)>,
}

fn display_regions(source: &str) -> impl Iterator<Item = DisplayRegion<'_>> {
    let mut counted_to = 0;
    let mut counted_lines = 0;
    regions(source, source.len())
        .into_iter()
        .filter_map(move |range| {
            let (start, end, close_end, env) = if (source[counted_to..range.start].ends_with("\\[")
                && source[range.end..].starts_with("\\]"))
                || (source[counted_to..range.start].ends_with("$$")
                    && source[range.end..].starts_with("$$"))
            {
                (range.start - 2, range.end, range.end + 2, None)
            } else if source[range.end..].starts_with("\\end")
                && let Some(rest) = source[range.start..].strip_prefix("\\begin")
                && let Some((span, end)) = group(rest, 0)
            {
                let env = rest[span].trim_end_matches('*');
                if env == "math" {
                    let close_end = range.end.min(source.len());
                    counted_lines += source[counted_to..close_end]
                        .bytes()
                        .filter(|c| *c == b'\n')
                        .count();
                    counted_to = close_end;
                    return None;
                }
                (
                    range.start,
                    range.end,
                    range.end,
                    Some((env, range.start + "\\begin".len() + end)),
                )
            } else {
                let close_end = (range.end + 1).min(source.len());
                counted_lines += source[counted_to..close_end]
                    .bytes()
                    .filter(|c| *c == b'\n')
                    .count();
                counted_to = close_end;
                return None;
            };
            let first = counted_lines
                + source[counted_to..start]
                    .bytes()
                    .filter(|c| *c == b'\n')
                    .count();
            let last = first + source[start..end].bytes().filter(|c| *c == b'\n').count();
            counted_to = close_end.min(source.len());
            counted_lines = last;
            Some(DisplayRegion {
                range,
                start,
                first,
                last,
                env,
            })
        })
}

fn inner_halign_contains_line(
    source: &str,
    range: Range<usize>,
    start_line: usize,
    target: usize,
) -> bool {
    let mut open: Option<(&str, usize, usize)> = None;
    let mut i = range.start;
    let mut counted_to = range.start;
    let mut counted_lines = start_line;
    while i < range.end {
        let Some(rel) = source.as_bytes()[i..range.end]
            .iter()
            .position(|b| matches!(*b, b'%' | b'\\'))
        else {
            break;
        };
        i += rel;
        if source.as_bytes()[i] == b'%' {
            i += source[i..range.end].find('\n').unwrap_or(range.end - i);
            continue;
        }
        let (name, end) = command(source, i);
        i = end;
        if matches!(name, "begin" | "end")
            && let Some((span, end)) = group(source, i).filter(|(_, e)| *e <= range.end)
        {
            let env = &source[span];
            let base = env.trim_end_matches('*');
            i = end;
            let is_halign = matches!(
                base,
                "aligned"
                    | "alignedat"
                    | "gathered"
                    | "cases"
                    | "dcases"
                    | "rcases"
                    | "array"
                    | "subarray"
                    | "smallmatrix"
                    | "matrix"
                    | "pmatrix"
                    | "bmatrix"
                    | "Bmatrix"
                    | "vmatrix"
                    | "Vmatrix"
                    | "tabular"
            );
            if name == "begin" && open.is_none() && is_halign {
                let first = counted_lines
                    + source[counted_to..end]
                        .bytes()
                        .filter(|c| *c == b'\n')
                        .count();
                counted_to = end;
                counted_lines = first;
                open = Some((env, 1, first));
            } else if let Some((expected, ref mut depth, first)) = open
                && env == expected
            {
                if name == "begin" {
                    *depth += 1;
                } else {
                    *depth -= 1;
                    if *depth == 0 {
                        let last = counted_lines
                            + source[counted_to..end]
                                .bytes()
                                .filter(|c| *c == b'\n')
                                .count();
                        counted_to = end;
                        counted_lines = last;
                        open = None;
                        if first < target && target <= last {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Multi-line display and environment math can report its closing delimiter line
/// for every interior token. Restrict inverse refinement to the full math region.
pub(super) fn math_lines(source: &str, line: u32) -> Option<Range<usize>> {
    let target = line.checked_sub(1)? as usize;
    for region in display_regions(source) {
        if (region.first..=region.last).contains(&target) {
            if region.first == region.last {
                continue;
            }
            if region.env.is_some_and(|(env, _)| env == "eqnarray") {
                return None;
            }
            let closes_inner_env = target < region.last
                && source
                    .lines()
                    .nth(target)
                    .map(super::source_line_text)
                    .and_then(|row| row.trim_start().strip_prefix("\\end"))
                    .and_then(|rest| group(rest, 0).map(|(span, _)| math_environment(&rest[span])))
                    .unwrap_or(false);
            if target < region.last && !closes_inner_env {
                return None;
            }
            return Some(region.first..region.last + 1);
        }
        if region.first > target {
            return None;
        }
    }
    None
}

/// Forward search inside multi-line display math maps lines without usable
/// box records to the closing math line while preserving inner `\halign` rows.
pub(super) fn math_closing_line(source: &str, line: u32, column: u32) -> Option<u32> {
    let target = line.checked_sub(1)? as usize;
    for region in display_regions(source) {
        if (region.first..=region.last).contains(&target) {
            if region.first == region.last {
                continue;
            }
            if target == region.first {
                let line_start = source[..region.start].rfind('\n').map_or(0, |pos| pos + 1);
                if !source[line_start..region.start].trim().is_empty() {
                    let col_idx = column.saturating_sub(1) as usize;
                    let col_byte = source[line_start..]
                        .char_indices()
                        .nth(col_idx)
                        .map_or(source.len() - line_start, |(idx, _)| idx);
                    if line_start + col_byte < region.start {
                        continue;
                    }
                }
            }
            if let Some(("eqnarray", begin_end)) = region.env {
                if target != region.first {
                    return None;
                }
                let line_end = source[region.start..]
                    .find('\n')
                    .map_or(source.len(), |pos| region.start + pos);
                let mut cursor = begin_end;
                while cursor < line_end {
                    cursor = super::skip_source_space(source, cursor).min(line_end);
                    if cursor >= line_end || source.as_bytes()[cursor] == b'%' {
                        cursor = line_end;
                        break;
                    }
                    if source.as_bytes()[cursor] != b'\\' {
                        break;
                    }
                    let (cmd, mut next) = command(source, cursor);
                    if matches!(cmd, "nonumber" | "notag" | "relax") {
                        cursor = next;
                        continue;
                    }
                    if cmd == "label" {
                        while let Some(opt_end) = super::optional_argument_end(source, next) {
                            next = opt_end;
                        }
                        if let Some((_, grp_end)) =
                            group(source, super::skip_source_space(source, next))
                        {
                            cursor = grp_end;
                            continue;
                        }
                    }
                    break;
                }
                return (cursor >= line_end).then_some((region.first + 1) as u32 + 1);
            }
            let collected = matches!(
                region.env.map(|(env, _)| env),
                Some(
                    "align"
                        | "gather"
                        | "multline"
                        | "flalign"
                        | "alignat"
                        | "xalignat"
                        | "xxalignat"
                        | "split"
                )
            );
            if !collected && inner_halign_contains_line(source, region.range, region.first, target)
            {
                return None;
            }
            return Some(region.last as u32 + 1);
        }
        if region.first > target {
            return None;
        }
    }
    None
}

/// Find a collected argument's search scope, preserving enclosing scopes for formatting.
pub(super) fn collected_text_lines(source: &str, line: u32) -> Option<Range<usize>> {
    let target = line.checked_sub(1)? as usize;
    let mut hidden = super::nonprinting_source_spans(source).peekable();
    let mut i = 0;
    let mut counted_to = 0;
    let mut counted_lines = 0;
    while i < source.len() {
        while hidden.peek().is_some_and(|(_, range, _)| range.end <= i) {
            hidden.next();
        }
        if let Some((start, range, _)) = hidden.peek()
            && *start <= i
        {
            i = range.end;
            continue;
        }
        let ch = source[i..].chars().next()?;
        if ch == '%' {
            i += source[i..].find('\n').unwrap_or(source.len() - i);
        } else if ch == '\\' {
            let (name, end) = command(source, i);
            i = end;
            if let Some(end) = quoted_token_end(source, name, i)
                .or_else(|| opaque_command_end(source, name, i).map(|(end, _)| end))
            {
                i = end;
            } else if let Some((first_printed, second_printed, end)) =
                collected_text_arguments(source, name, i)
            {
                let start = first_printed.as_ref().unwrap_or(&second_printed).start;
                let first = counted_lines
                    + source[counted_to..start]
                        .bytes()
                        .filter(|c| *c == b'\n')
                        .count();
                let last = first + source[start..end].bytes().filter(|c| *c == b'\n').count();
                if collected_formatting_command(name) && (first..last).contains(&target) {
                    // Nonclosing anchors retain ordinary proximity and nested collector discovery.
                    i = start;
                    continue;
                }
                counted_to = end;
                counted_lines = last;
                if (first..=last).contains(&target) {
                    if collected_formatting_command(name) {
                        // A trailing style wrapper must not hide the rest of a collected body.
                        if let Some(enclosing) = super::source_frame_range(source, line)
                            .or_else(|| super::source_tabularx_range(source, line))
                            .or_else(|| math_lines(source, line))
                            .filter(|scope| scope.start <= first && last < scope.end)
                        {
                            return Some(enclosing);
                        }
                    }
                    return Some(first..last + 1);
                }
                if first > target {
                    return None;
                }
                i = end;
            }
        } else {
            let next_special = source.as_bytes()[i..]
                .iter()
                .position(|b| matches!(*b, b'%' | b'\\'))
                .map_or(source.len(), |pos| i + pos);
            let limit = hidden
                .peek()
                .map_or(next_special, |(start, _, _)| next_special.min(*start));
            i = limit.max(i + ch.len_utf8());
        }
    }
    None
}

/// Literal prose immediately beside inline math is evidence for which
/// occurrence was clicked. Stop at markup, comments, and blank-line
/// paragraph boundaries; never invent a TeX expansion.
fn inline_context(source: &str, range: &Range<usize>, atoms: Vec<Atom>) -> Vec<Atom> {
    let (before, after) =
        if source[..range.start].ends_with("\\(") && source[range.end..].starts_with("\\)") {
            (range.start - 2, range.end + 2)
        } else if source[..range.start].ends_with('$')
            && !source[..range.start].ends_with("$$")
            && source[range.end..].starts_with('$')
            && !source[range.end..].starts_with("$$")
        {
            (range.start - 1, range.end + 1)
        } else {
            return atoms;
        };
    let literal = |c: char| {
        !matches!(
            c,
            '\\' | '$' | '{' | '}' | '%' | '^' | '_' | '&' | '~' | '\n' | '\r'
        )
    };
    let mut prefix = Vec::with_capacity(8);
    let mut cursor = before;
    'backward: while prefix.len() < 8 {
        let line_start = source[..cursor].rfind('\n').map_or(0, |p| p + 1);
        for (i, c) in source[line_start..cursor].char_indices().rev() {
            if c == '\r' {
                continue;
            }
            if !literal(c) {
                break 'backward;
            }
            if !c.is_whitespace() {
                prefix.push((line_start + i, c));
                if prefix.len() == 8 {
                    break 'backward;
                }
            }
        }
        if line_start == 0 {
            break;
        }
        let nl = line_start - 1;
        let prev_start = source[..nl].rfind('\n').map_or(0, |p| p + 1);
        let prev_line = &source[prev_start..nl];
        if prev_line.trim().is_empty() || prev_line.contains('%') {
            break;
        }
        cursor = nl;
    }
    let mut result: Vec<Atom> = Vec::with_capacity(atoms.len() + 16);
    for (i, c) in prefix.into_iter().rev() {
        let join = result
            .last()
            .is_some_and(|prev| prev.value.is_alphanumeric() && prev.span.end == i);
        append_joined(&mut result, c, i..i + c.len_utf8(), join);
    }
    result.extend(atoms);
    let mut suffix_count = 0;
    let mut cursor = after;
    'forward: while suffix_count < 8 && cursor < source.len() {
        let line_end = source[cursor..]
            .find('\n')
            .map_or(source.len(), |p| cursor + p);
        for (i, c) in source[cursor..line_end].char_indices() {
            if c == '\r' {
                continue;
            }
            if !literal(c) {
                break 'forward;
            }
            if !c.is_whitespace() {
                let start = cursor + i;
                let join = result
                    .last()
                    .is_some_and(|prev| prev.value.is_alphanumeric() && prev.span.end == start);
                append_joined(&mut result, c, start..start + c.len_utf8(), join);
                suffix_count += 1;
                if suffix_count == 8 {
                    break 'forward;
                }
            }
        }
        if line_end >= source.len() {
            break;
        }
        let next_start = line_end + 1;
        let next_end = source[next_start..]
            .find('\n')
            .map_or(source.len(), |p| next_start + p);
        if source[next_start..next_end].trim().is_empty() {
            break;
        }
        cursor = next_start;
    }
    result
}

pub(super) fn macro_target_name(source: &str, mut at: usize) -> Option<(&str, usize)> {
    at = super::skip_source_space(source, at);
    if let Some((grp, next)) = group(source, at) {
        let target = &source[grp];
        let start = super::skip_source_space(target, 0);
        if target.as_bytes().get(start) != Some(&b'\\') {
            return None;
        }
        let (name, end) = command(target, start);
        (!name.is_empty()
            && !matches!(name, "csname" | "expandafter")
            && super::skip_source_space(target, end) == target.len())
        .then_some((name, next))
    } else if source.as_bytes().get(at) == Some(&b'\\') {
        let (name, next) = control_sequence(source, at);
        (!name.is_empty() && !matches!(name, "csname" | "expandafter")).then_some((name, next))
    } else {
        None
    }
}

pub(super) fn macro_target_end(source: &str, mut at: usize) -> Option<usize> {
    at = super::skip_source_space(source, at);
    at += usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
    at = super::skip_source_space(source, at);
    while source.as_bytes().get(at) == Some(&b'\\') && command(source, at).0 == "expandafter" {
        at = super::skip_source_space(source, command(source, at).1);
        at += usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
        at = super::skip_source_space(source, at);
    }
    if let Some((_, end)) = group(source, at) {
        Some(end)
    } else if source.as_bytes().get(at) == Some(&b'\\') {
        Some(control_sequence(source, at).1)
    } else {
        None
    }
}

/// Known mutation APIs invalidate their static target; unknown dispatch invalidates all targets.
pub(super) fn macro_mutation_target<'a>(
    source: &'a str,
    command: &str,
    at: usize,
) -> Option<(&'a str, usize)> {
    let command = command
        .strip_prefix("protected@")
        .filter(|name| {
            matches!(
                *name,
                "eappto"
                    | "xappto"
                    | "epreto"
                    | "xpreto"
                    | "cseappto"
                    | "csxappto"
                    | "csepreto"
                    | "csxpreto"
            )
        })
        .unwrap_or(command);
    let named = command.strip_prefix("cs");
    if matches!(
        named.unwrap_or(command),
        "appto" | "eappto" | "gappto" | "xappto" | "preto" | "epreto" | "gpreto" | "xpreto"
    ) || matches!(command, "patchcmd" | "apptocmd" | "pretocmd")
    {
        let at = if command == "patchcmd" {
            let start = super::skip_source_space(source, at);
            if source.as_bytes().get(start) == Some(&b'[') {
                match super::optional_argument_boundary(source, start) {
                    Ok(end) => end,
                    Err(end) => return Some(("**", end)),
                }
            } else {
                at
            }
        } else {
            at
        };
        let raw_target = |at| {
            if source.as_bytes().get(at) != Some(&b'\\') {
                return ("**", at);
            }
            let (name, end) = self::command(source, at);
            (
                if matches!(name, "" | "csname" | "expandafter") {
                    "**"
                } else {
                    name
                },
                end,
            )
        };
        let at = super::skip_source_space(source, at);
        if let Some((span, end)) = group(source, at) {
            let name = if named.is_some() {
                literal_csname(&source[span]).unwrap_or("**")
            } else {
                let (name, next) = raw_target(super::skip_source_space(source, span.start));
                if super::skip_source_space(source, next) == span.end {
                    name
                } else {
                    "**"
                }
            };
            return Some((name, end));
        }
        return Some(if named.is_some() {
            ("**", at)
        } else {
            raw_target(at)
        });
    }
    if matches!(
        command,
        "AtBeginEnvironment"
            | "AtEndEnvironment"
            | "BeforeBeginEnvironment"
            | "AfterEndEnvironment"
    ) {
        let start = super::skip_source_space(source, at);
        let at = if source.as_bytes().get(start) == Some(&b'[') {
            match super::optional_argument_boundary(source, start) {
                Ok(end) => end,
                Err(end) => return Some(("**", end)),
            }
        } else {
            at
        };
        return Some(
            group(source, at)
                .map(|(span, end)| (literal_csname(&source[span]).unwrap_or("**"), end))
                .unwrap_or(("**", at)),
        );
    }
    if matches!(
        command,
        "AddToHook" | "AddToHookNext" | "AddToHookWithArguments" | "AddToHookNextWithArguments"
    ) {
        let Some((span, end)) = group(source, at) else {
            return Some(("**", at));
        };
        let Some(hook) = literal_csname(&source[span]) else {
            return Some(("**", end));
        };
        if let Some(target) = hook
            .strip_prefix("cmd/")
            .or_else(|| hook.strip_prefix("env/"))
        {
            return Some(
                target
                    .rsplit_once('/')
                    .filter(|(name, phase)| !name.is_empty() && !phase.is_empty())
                    .map_or(("**", end), |(name, _)| (name, end)),
            );
        }
        if hook.starts_with("file/") {
            return Some(("input", end));
        }
        return Some(("**", end));
    }
    None
}

pub(crate) fn is_macro_definition_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "newcommand"
            | "renewcommand"
            | "providecommand"
            | "DeclareRobustCommand"
            | "DeclareMathOperator"
            | "NewDocumentCommand"
            | "RenewDocumentCommand"
            | "ProvideDocumentCommand"
            | "DeclareDocumentCommand"
            | "NewExpandableDocumentCommand"
            | "RenewExpandableDocumentCommand"
            | "ProvideExpandableDocumentCommand"
            | "DeclareExpandableDocumentCommand"
            | "def"
            | "gdef"
            | "edef"
            | "xdef"
            | "let"
            | "futurelet"
            | "csname"
            | "newif"
            | "pgfmathsetmacro"
            | "pgfmathtruncatemacro"
    )
}

pub(super) fn definition_command(source: &str, start: usize) -> (&str, usize, bool) {
    let (name, end) = static_command(source, start);
    if name == "csname" {
        return (name, start + name.len() + 1, false);
    }
    if name == "expandafter" {
        let next = super::skip_source_space(source, end);
        if source.as_bytes().get(next) == Some(&b'\\') {
            let (name, end) = command(source, next);
            if is_macro_definition_command(name) && !matches!(name, "csname" | "newif") {
                return (name, end, true);
            }
        }
    }
    (name, end, false)
}

pub(super) struct MacroAlias<'a> {
    pub name: &'a str,
    pub value: Range<usize>,
    pub nonprinting_end: usize,
}

fn alias_token(source: &str, mut at: usize, mut skip_blanks: bool) -> Option<(Range<usize>, bool)> {
    while at < source.len() {
        let ch = source[at..].chars().next()?;
        if ch == '%' {
            at += source[at..].find('\n').map_or(source.len() - at, |n| n + 1);
            skip_blanks = true;
        } else if ch.is_ascii_whitespace() {
            let start = at;
            at += source[at..]
                .bytes()
                .take_while(u8::is_ascii_whitespace)
                .count();
            if !skip_blanks {
                return Some((start..at, true));
            }
        } else if ch == '\\' {
            let (name, end) = command(source, at);
            return Some((
                at..end,
                name.as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@'),
            ));
        } else {
            return Some((at..at + ch.len_utf8(), false));
        }
    }
    None
}

pub(super) fn macro_alias<'a>(
    source: &'a str,
    cmd: &str,
    mut at: usize,
    expanded_target: bool,
) -> MacroAlias<'a> {
    at = super::skip_source_space(source, at);
    let mut expanded_value = false;
    let (mut name, target_end) = if source.as_bytes().get(at) == Some(&b'\\') {
        let (raw, end) = command(source, at);
        if raw == "expandafter" {
            let target = super::skip_source_space(source, end);
            if expanded_target && source.as_bytes().get(target) == Some(&b'\\') {
                let (name, end) = command(source, target);
                if !matches!(name, "csname" | "expandafter") {
                    expanded_value = true;
                    (name, end)
                } else {
                    ("**", macro_target_end(source, at).unwrap_or(end))
                }
            } else {
                ("**", macro_target_end(source, at).unwrap_or(end))
            }
        } else if raw == "csname" {
            if expanded_target {
                let (name, end) = control_sequence(source, at);
                (if name == "csname" { "**" } else { name }, end)
            } else {
                ("**", end)
            }
        } else {
            (raw, end)
        }
    } else {
        (
            "**",
            at + source[at..].chars().next().map_or(0, char::len_utf8),
        )
    };
    if cmd == "futurelet" {
        // The consumed endcsname controls blanks, not the constructed identifier.
        let skip_blanks = source.as_bytes()[..target_end]
            .last()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@');
        let value = alias_token(source, target_end, skip_blanks)
            .and_then(|(first, skip)| alias_token(source, first.end, skip))
            .map_or(target_end..target_end, |(value, _)| value);
        return MacroAlias {
            name,
            value,
            // Both lookahead tokens execute after the assignment.
            nonprinting_end: target_end,
        };
    }
    at = super::skip_source_space(source, target_end);
    if source.as_bytes().get(at) == Some(&b'=') {
        expanded_value = false;
        at = super::skip_source_space(source, at + 1);
    }
    let value = if expanded_value && source.as_bytes().get(at) == Some(&b'\\') {
        let (raw, _) = command(source, at);
        let (value, end) = control_sequence(source, at);
        if raw != "csname" || value == "csname" {
            name = "**";
        }
        at..end
    } else {
        alias_token(source, at, true).map_or(at..at, |(value, _)| value)
    };
    MacroAlias {
        name,
        nonprinting_end: value.end,
        value,
    }
}

pub(super) fn environment_end_name<'a>(
    table: &BTreeMap<&'a str, Option<MacroDef<'a>>>,
    environment: &str,
) -> Option<&'a str> {
    table
        .range("end"..)
        .take_while(|(name, _)| name.starts_with("end"))
        .find_map(|(&name, _)| (name.strip_prefix("end") == Some(environment)).then_some(name))
}

pub(crate) fn macros_from_sources<'a>(
    sources: &[&'a str],
    operation: &crate::Operation,
) -> BTreeMap<&'a str, Option<MacroDef<'a>>> {
    macros_from_sources_with_mode(sources, false, operation)
}

// Permissive definition hints; the native region decides whether a column is compatible.
pub(super) fn macros_from_sources_for_guess<'a>(
    sources: &[&'a str],
    operation: &crate::Operation,
) -> BTreeMap<&'a str, Option<MacroDef<'a>>> {
    macros_from_sources_with_mode(sources, true, operation)
}

type MacroTable<'a> = BTreeMap<&'a str, Option<MacroDef<'a>>>;

fn macros_from_sources_with_mode<'a>(
    sources: &[&'a str],
    guess: bool,
    operation: &crate::Operation,
) -> MacroTable<'a> {
    let mut table = MacroTable::new();
    for &source in sources {
        let mut nonprinting = crate::tex::nonprinting_source_spans(source).peekable();
        let mut in_preamble = true;
        let mut brace_depth = 0usize;
        let mut group_depth = 0usize;
        let mut if_stack: Vec<Option<bool>> = Vec::new();
        let mut at = 0usize;
        while at < source.len() {
            if operation.check().is_err() {
                return table;
            }
            match source.as_bytes()[at] {
                b'%' => {
                    at += source[at..].find('\n').unwrap_or(source.len() - at);
                }
                b'{' => {
                    brace_depth += 1;
                    at += 1;
                }
                b'}' => {
                    brace_depth = brace_depth.saturating_sub(1);
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
                        in_preamble = false;
                    }
                    at += 1;
                }
                b'\\' => {
                    let start = at;
                    while nonprinting
                        .peek()
                        .is_some_and(|(_, span, _)| span.end <= start)
                    {
                        nonprinting.next();
                    }
                    let (cmd, end, expanded_target) = definition_command(source, at);
                    at = end;
                    let executing = !nonprinting
                        .peek()
                        .is_some_and(|(_, span, _)| span.contains(&start));
                    if !table.contains_key(cmd)
                        && let Some(quoted_end) = quoted_token_end(source, cmd, at)
                    {
                        // Stored token lists balance braces before quoted tokens execute.
                        if executing
                            || !matches!(
                                source.as_bytes().get(super::skip_source_space(source, at)),
                                Some(b'{' | b'}')
                            )
                        {
                            at = quoted_end;
                        }
                        continue;
                    }
                    if executing
                        && !table.contains_key(cmd)
                        && let Some((opaque_end, _)) = opaque_command_end(source, cmd, at)
                    {
                        at = opaque_end;
                        continue;
                    }
                    if executing && matches!(cmd, "(" | "[") {
                        if brace_depth == 0 && group_depth == 0 && if_stack.is_empty() {
                            in_preamble = false;
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
                    if executing
                        && cmd.starts_with("if")
                        && cmd != "iff"
                        && !(guess && cmd == "ifthenelse" && !table.contains_key(cmd))
                    {
                        if_stack.push(match cmd {
                            "iftrue" => Some(true),
                            "iffalse" => Some(false),
                            _ => None,
                        });
                        continue;
                    }
                    if executing && cmd == "else" {
                        if let Some(last) = if_stack.last_mut() {
                            *last = last.map(|value| !value);
                        }
                        continue;
                    }
                    if executing && cmd == "fi" {
                        if_stack.pop();
                        continue;
                    }
                    if executing && cmd == "begin" {
                        if let Some((env, next)) = group(source, at) {
                            if &source[env] == "document"
                                || (brace_depth == 0 && group_depth == 0 && if_stack.is_empty())
                            {
                                in_preamble = false;
                            }
                            at = next;
                        }
                        group_depth += 1;
                        continue;
                    }
                    if executing && cmd == "end" {
                        group_depth = group_depth.saturating_sub(1);
                        if let Some((_, next)) = group(source, at) {
                            at = next;
                        }
                        continue;
                    }
                    if cmd == "csname" {
                        continue;
                    }
                    if cmd == "newif" {
                        if let Some((name, _)) = macro_target_name(source, at) {
                            table.insert(name, None);
                        }
                        at = macro_target_end(source, at).unwrap_or(at);
                        continue;
                    }
                    if let Some((name, next)) = macro_mutation_target(source, cmd, at) {
                        table.insert(name, None);
                        at = next;
                        continue;
                    }
                    if matches!(cmd, "let" | "futurelet") {
                        let alias = macro_alias(source, cmd, at, expanded_target);
                        table.insert(alias.name, None);
                        // Stored assignment tokens cannot consume their enclosing group delimiters.
                        if executing {
                            at = alias.nonprinting_end;
                        }
                        continue;
                    }
                    if let Some((name, _, _)) = crate::tex::environment_definition(source, cmd, at)
                    {
                        if let Some(end_name) = environment_end_name(&table, name) {
                            table.insert(end_name, None);
                        }
                        if let Some(entry) = table.get_mut(name) {
                            *entry = None;
                        }
                        continue;
                    }
                    let top_level = (executing
                        || nonprinting
                            .peek()
                            .is_some_and(|(head, _, _)| *head == start))
                        && in_preamble
                        && brace_depth == 0
                        && group_depth == 0
                        && if_stack.is_empty()
                        && (expanded_target
                            || !source[super::skip_source_space(source, at)..]
                                .starts_with("\\csname"));
                    if matches!(
                        cmd,
                        "edef"
                            | "xdef"
                            | "NewDocumentCommand"
                            | "RenewDocumentCommand"
                            | "ProvideDocumentCommand"
                            | "DeclareDocumentCommand"
                            | "NewExpandableDocumentCommand"
                            | "RenewExpandableDocumentCommand"
                            | "ProvideExpandableDocumentCommand"
                            | "DeclareExpandableDocumentCommand"
                            | "pgfmathsetmacro"
                            | "pgfmathtruncatemacro"
                    ) {
                        if let Some((name, _)) = macro_target_name(source, at) {
                            table.insert(name, None);
                        }
                        continue;
                    }
                    let mut optional_default = None;
                    let (name, arity) = if matches!(cmd, "def" | "gdef") {
                        let Some((name, mut next)) = macro_target_name(source, at) else {
                            continue;
                        };
                        let mut arity = Some(0u8);
                        loop {
                            next = super::skip_source_space(source, next);
                            if source.as_bytes().get(next) == Some(&b'{') {
                                break;
                            }
                            let Some(expected) = arity.and_then(|a| (a < 9).then_some(b'1' + a))
                            else {
                                arity = None;
                                break;
                            };
                            if source.as_bytes().get(next) == Some(&b'#')
                                && source.as_bytes().get(next + 1) == Some(&expected)
                            {
                                arity = arity.map(|a| a + 1);
                                next += 2;
                            } else {
                                arity = None;
                                break;
                            }
                        }
                        at = next;
                        (name, arity)
                    } else if cmd == "DeclareMathOperator" {
                        let Some((name, next)) = macro_target_name(source, at) else {
                            continue;
                        };
                        at = next;
                        (name, Some(0u8))
                    } else if matches!(
                        cmd,
                        "newcommand" | "renewcommand" | "providecommand" | "DeclareRobustCommand"
                    ) {
                        let Some((name, mut next)) = macro_target_name(source, at) else {
                            continue;
                        };
                        let mut arity = (cmd != "providecommand").then_some(0u8);
                        if let Some(opt_end) = super::optional_argument_end(source, next) {
                            let opt = source
                                [super::skip_source_space(source, next) + 1..opt_end - 1]
                                .trim();
                            arity = arity
                                .filter(|_| opt.len() == 1 && opt.as_bytes()[0].is_ascii_digit())
                                .map(|_| opt.as_bytes()[0] - b'0');
                            next = opt_end;
                            while let Some(extra_end) = super::optional_argument_end(source, next) {
                                optional_default.get_or_insert(next);
                                arity = None;
                                next = extra_end;
                            }
                        }
                        at = next;
                        (name, arity)
                    } else {
                        continue;
                    };
                    let body_open = super::skip_source_space(source, at);
                    let Some((body_range, _)) = group(source, body_open) else {
                        table.insert(name, None);
                        continue;
                    };
                    at = optional_default.unwrap_or(body_open);
                    let Some(arity) = arity.filter(|_| top_level) else {
                        table.insert(name, None);
                        continue;
                    };
                    let body = &source[body_range];
                    let mut valid = !body.contains('$') && !body.contains("^^");
                    let mut seen_params = 0u16;
                    let mut used_params = 0u16;
                    for (idx, _) in body.match_indices('#') {
                        if crate::tex::has_unescaped_backslash_before(body, idx) {
                            continue;
                        }
                        if let Some(d) = body
                            .as_bytes()
                            .get(idx + 1)
                            .filter(|b| (b'1'..=b'0' + arity).contains(b))
                            .map(|b| b - b'1')
                        {
                            used_params |= 1 << d;
                            if (seen_params >> d) != 0 {
                                valid = false;
                            }
                            seen_params |= 1 << d;
                        } else {
                            valid = false;
                        }
                    }
                    let def = MacroDef {
                        arity,
                        used_params,
                        body_source: body,
                        body: valid.then_some(body),
                    };
                    match table.get(name) {
                        Some(Some(existing)) if *existing == def && def.body.is_some() => {}
                        Some(_) => {
                            table.insert(name, None);
                        }
                        None => {
                            table.insert(name, Some(def));
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
    }
    table.remove("**");
    table
}

fn skip_tex_dimen(source: &str, mut pos: usize, limit: usize) -> Option<usize> {
    pos = super::skip_source_space(source, pos).min(limit);
    let mut group_end = None;
    let mut parse_limit = limit;
    if let Some((inner, end)) = group(source, pos).filter(|(_, e)| *e <= limit) {
        pos = super::skip_source_space(source, inner.start).min(inner.end);
        parse_limit = inner.end;
        group_end = Some(end);
    }
    while pos < parse_limit && matches!(source.as_bytes()[pos], b'+' | b'-') {
        pos = super::skip_source_space(source, pos + 1).min(parse_limit);
    }
    let num_start = pos;
    let mut digits = 0usize;
    while pos < parse_limit && source.as_bytes()[pos].is_ascii_digit() {
        pos += 1;
        digits += 1;
    }
    if pos < parse_limit && matches!(source.as_bytes()[pos], b'.' | b',') {
        pos += 1;
        while pos < parse_limit && source.as_bytes()[pos].is_ascii_digit() {
            pos += 1;
            digits += 1;
        }
    }
    if pos == num_start || digits == 0 {
        return None;
    }
    pos = super::skip_source_space(source, pos).min(parse_limit);
    if source[pos..parse_limit]
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("true"))
    {
        pos = super::skip_source_space(source, pos + 4).min(parse_limit);
    }
    for unit in [
        "filll", "fill", "fil", "pt", "pc", "in", "bp", "cm", "mm", "dd", "cc", "sp", "em", "ex",
        "mu", "nd", "nc", "px",
    ] {
        if source[pos..parse_limit]
            .get(..unit.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(unit))
        {
            let unit_end = pos + unit.len();
            if let Some(end) = group_end {
                return (super::skip_source_space(source, unit_end).min(parse_limit)
                    == parse_limit)
                    .then_some(end);
            }
            return Some(unit_end);
        }
    }
    None
}

/// Tokenize a math region into character atoms with source byte spans.
/// Unknown or reordered constructs emit `'\0'` gap atoms so adjacent tokens
/// stop context scoring at the unknown boundary while remaining addressable.
fn atoms_with_macros<'a>(
    source: &'a str,
    range: Range<usize>,
    context: (&BTreeMap<&str, Option<MacroDef<'a>>>, &crate::Operation),
    depth: u8,
    span_override: Option<Range<usize>>,
    args: &[(&'a str, Range<usize>)],
    has_unknown: &mut bool,
) -> Option<Vec<Atom>> {
    let (macros, operation) = context;
    if operation.check().is_err() || depth >= 64 {
        return None;
    }
    let mut result = Vec::new();
    let mut i = range.start;
    let mut prev_alnum_end = None;
    let script_arg = |pos| argument(source, pos, range.end, span_override.is_some());
    while i < range.end {
        operation.check().ok()?;
        let start = i;
        let continuing = prev_alnum_end == Some(start);
        prev_alnum_end = None;
        let ch = source[i..].chars().next()?;
        i += ch.len_utf8();
        let emit_span = |s: Range<usize>| span_override.clone().unwrap_or(s);
        match ch {
            '%' => i += source[i..range.end].find('\n').unwrap_or(range.end - i),
            '{' | '}' | '&' => {}
            '#' => {
                if let Some((arg_src, arg_rng)) = source
                    .as_bytes()
                    .get(i)
                    .filter(|b| i < range.end && (b'1'..=b'9').contains(b))
                    .and_then(|b| args.get(usize::from(b - b'1')))
                {
                    i += 1;
                    let sub = atoms_with_macros(
                        arg_src,
                        arg_rng.clone(),
                        (macros, operation),
                        depth + 1,
                        None,
                        &[],
                        has_unknown,
                    )?;
                    result.extend(sub);
                } else {
                    i += usize::from(
                        i < range.end && source.as_bytes().get(i).is_some_and(u8::is_ascii_digit),
                    );
                    *has_unknown = true;
                    append(&mut result, '\0', emit_span(start..i));
                }
            }
            '_' | '^' => {
                if ch == '^' && source[i..range.end].starts_with('^') {
                    i += 1;
                    let hex = source[i..range.end]
                        .bytes()
                        .take(2)
                        .take_while(u8::is_ascii_hexdigit)
                        .count();
                    i += if hex == 2 {
                        2
                    } else {
                        source[i..range.end]
                            .chars()
                            .next()
                            .map_or(0, char::len_utf8)
                    };
                    *has_unknown = true;
                    append(&mut result, '\0', emit_span(start..i));
                    continue;
                }
                // Simultaneous scripts on the same base can extract in visual
                // rather than source order; isolate them with '\0' boundaries.
                let Some((arg1, end1)) = script_arg(i) else {
                    *has_unknown = true;
                    append(&mut result, '\0', emit_span(start..i));
                    continue;
                };
                let after1 = super::skip_source_space(source, end1).min(range.end);
                if source[after1..range.end].starts_with(['_', '^'])
                    && !source[after1..range.end].starts_with("^^")
                {
                    append(&mut result, '\x01', emit_span(start..i));
                    let sub = atoms_with_macros(
                        source,
                        arg1,
                        (macros, operation),
                        depth + 1,
                        span_override.clone(),
                        args,
                        has_unknown,
                    )?;
                    result.extend(sub);
                    i = after1;
                    while i < range.end
                        && source[i..range.end].starts_with(['_', '^'])
                        && !source[i..range.end].starts_with("^^")
                    {
                        append(&mut result, '\x02', emit_span(i..i + 1));
                        let Some((arg_next, end_next)) = script_arg(i + 1) else {
                            *has_unknown = true;
                            i += 1;
                            break;
                        };
                        let sub = atoms_with_macros(
                            source,
                            arg_next,
                            (macros, operation),
                            depth + 1,
                            span_override.clone(),
                            args,
                            has_unknown,
                        )?;
                        result.extend(sub);
                        i = super::skip_source_space(source, end_next).min(range.end);
                    }
                    append(&mut result, '\x03', emit_span(start..i));
                }
            }
            '\\' => {
                let (name, end) = command(source, start);
                i = end;
                let span = emit_span(start..end);
                if (name.starts_with("if") && name != "iff")
                    || matches!(
                        name,
                        "else"
                            | "or"
                            | "fi"
                            | "mathchoice"
                            | "char"
                            | "mathchar"
                            | "Umathchar"
                            | "Umathcodenum"
                            | "Uchar"
                            | "delimiter"
                            | "Udelimiter"
                            | "radical"
                            | "Uradical"
                            | "mathaccent"
                            | "Umathaccent"
                            | "accent"
                            | "csname"
                            | "endcsname"
                            | "expandafter"
                            | "noexpand"
                            | "string"
                            | "meaning"
                            | "romannumeral"
                            | "number"
                            | "the"
                    )
                {
                    return None;
                }
                if name == "begin" {
                    let (environment, mut end) =
                        group(source, i).filter(|(_, e)| *e <= range.end)?;
                    let environment = &source[environment];
                    let base = environment.trim_end_matches('*');
                    if (matches!(
                        base,
                        "aligned" | "gathered" | "lgathered" | "rgathered" | "multlined"
                    ) || (environment.ends_with('*')
                        && matches!(
                            base,
                            "matrix"
                                | "pmatrix"
                                | "bmatrix"
                                | "Bmatrix"
                                | "vmatrix"
                                | "Vmatrix"
                                | "smallmatrix"
                        )))
                        && let Some(opt_end) =
                            super::optional_argument_end(source, end).filter(|e| *e <= range.end)
                    {
                        let opt_start = super::skip_source_space(source, end) + 1;
                        if matches!(
                            source[opt_start..opt_end - 1].trim(),
                            "t" | "b" | "c" | "l" | "r"
                        ) {
                            end = opt_end;
                        }
                    }
                    if matches!(
                        base,
                        "alignat"
                            | "alignedat"
                            | "xalignat"
                            | "xxalignat"
                            | "subarray"
                            | "array"
                            | "tabular"
                    ) {
                        let after_opt = if matches!(base, "alignedat" | "array" | "tabular") {
                            super::optional_argument_end(source, end)
                                .filter(|e| *e <= range.end)
                                .unwrap_or(end)
                        } else {
                            end
                        };
                        if let Some((_, e)) =
                            group(source, after_opt).filter(|(_, e)| *e <= range.end)
                        {
                            end = e;
                        }
                    } else if matches!(base, "tabular*" | "tabularx") {
                        if let Some((_, e)) = group(source, end).filter(|(_, e)| *e <= range.end) {
                            end = e;
                        }
                        if let Some(opt_end) =
                            super::optional_argument_end(source, end).filter(|e| *e <= range.end)
                        {
                            end = opt_end;
                        }
                        if let Some((_, e)) = group(source, end).filter(|(_, e)| *e <= range.end) {
                            end = e;
                        }
                    } else if !math_environment(environment) {
                        *has_unknown = true;
                        append(&mut result, '\0', span);
                    }
                    i = end;
                    continue;
                }
                if name == "end" {
                    let (_, end) = group(source, i).filter(|(_, e)| *e <= range.end)?;
                    i = end;
                    continue;
                }
                let macro_entry = macros.get(name).copied();
                if let Some(macro_entry) = macro_entry {
                    if let Some(def) = macro_entry
                        && depth + 16 <= 64
                    {
                        let mut call_args = Vec::with_capacity(usize::from(def.arity));
                        let mut arg_cursor = i;
                        for _ in 0..def.arity {
                            let Some((arg_rng, arg_end)) = script_arg(arg_cursor) else {
                                break;
                            };
                            arg_cursor = arg_end;
                            let arg = if span_override.is_some() {
                                let trimmed = source[arg_rng].trim();
                                (trimmed.len() == 2 && trimmed.starts_with('#'))
                                    .then(|| trimmed.as_bytes()[1].checked_sub(b'1'))
                                    .flatten()
                                    .and_then(|idx| args.get(usize::from(idx)))
                                    .cloned()
                            } else {
                                Some((source, arg_rng))
                            };
                            let Some(arg) = arg else {
                                break;
                            };
                            call_args.push(arg);
                        }
                        if call_args.len() == usize::from(def.arity) {
                            i = arg_cursor;
                            if let Some(body) = def.body {
                                let after = super::skip_source_space(source, i).min(range.end);
                                let boundary_script = body.contains(['_', '^', '\\'])
                                    && (source[after..range.end].starts_with(['_', '^'])
                                        || (body.contains(['_', '^'])
                                            && call_args.iter().any(|(src, rng)| {
                                                src[rng.clone()].contains(['_', '^'])
                                            })));
                                if boundary_script {
                                    append(&mut result, '\0', span.clone());
                                }
                                let sub = atoms_with_macros(
                                    body,
                                    0..body.len(),
                                    (macros, operation),
                                    depth + 16,
                                    Some(span.clone()),
                                    &call_args,
                                    has_unknown,
                                )?;
                                result.extend(sub);
                                if boundary_script {
                                    append(&mut result, '\0', span);
                                }
                                continue;
                            }
                            *has_unknown = true;
                            append(&mut result, '\0', span);
                            continue;
                        }
                        i = arg_cursor;
                    }
                    while let Some(end) = super::optional_argument_end(source, i)
                        .or_else(|| super::overlay_argument_end(source, i))
                        .or_else(|| group(source, i).map(|(_, e)| e))
                        .filter(|e| *e <= range.end)
                    {
                        i = end;
                    }
                    *has_unknown = true;
                    append(&mut result, '\0', span);
                    continue;
                }
                if name == "label" {
                    while let Some(opt_end) =
                        super::optional_argument_end(source, i).filter(|e| *e <= range.end)
                    {
                        i = opt_end;
                    }
                    if let Some((_, grp_end)) = script_arg(i) {
                        i = grp_end;
                    }
                    continue;
                }
                if name == "not" {
                    let next = super::skip_source_space(source, i).min(range.end);
                    let negated = if source[next..range.end].starts_with('=') {
                        Some(('≠', next + 1))
                    } else if source[next..range.end].starts_with('<') {
                        Some(('≮', next + 1))
                    } else if source[next..range.end].starts_with('>') {
                        Some(('≯', next + 1))
                    } else if source[next..range.end].starts_with('|') {
                        Some(('∤', next + 1))
                    } else if source[next..range.end].starts_with('\\') {
                        let (next_cmd, next_end) = command(source, next);
                        if macros.contains_key(next_cmd) {
                            return None;
                        }
                        negated_symbol(next_cmd)
                            .filter(|_| next_end <= range.end)
                            .map(|c| (c, next_end))
                    } else {
                        None
                    };
                    if let Some((c, next_end)) = negated {
                        i = next_end;
                        append(&mut result, c, emit_span(start..i));
                    } else {
                        append(&mut result, '\0', span);
                    }
                    continue;
                }
                match classify_command(name) {
                    MathCommand::Transparent => {
                        if name == "operatorname"
                            && i < range.end
                            && source.as_bytes().get(i) == Some(&b'*')
                        {
                            i += 1;
                        }
                    }
                    MathCommand::DelimiterPrefix => {
                        let following = source[i..range.end].trim_start();
                        if following.starts_with('.') {
                            i = range.end - following.len() + 1;
                        }
                    }
                    MathCommand::Dimension { allow_glue } => {
                        let mut dim_end = skip_tex_dimen(source, i, range.end)?;
                        if allow_glue {
                            let kw = super::skip_source_space(source, dim_end).min(range.end);
                            if source[kw..range.end]
                                .get(..4)
                                .is_some_and(|s| s.eq_ignore_ascii_case("plus"))
                            {
                                dim_end = skip_tex_dimen(source, kw + 4, range.end)?;
                            }
                            let kw = super::skip_source_space(source, dim_end).min(range.end);
                            if source[kw..range.end]
                                .get(..5)
                                .is_some_and(|s| s.eq_ignore_ascii_case("minus"))
                            {
                                dim_end = skip_tex_dimen(source, kw + 5, range.end)?;
                            }
                        }
                        i = dim_end;
                    }
                    MathCommand::DiscardArg {
                        emit_gap,
                        trailing_opt,
                    } => {
                        i += usize::from(i < range.end && source.as_bytes().get(i) == Some(&b'*'));
                        while let Some(opt_end) = super::optional_argument_end(source, i)
                            .or_else(|| super::overlay_argument_end(source, i))
                            .filter(|e| *e <= range.end)
                        {
                            i = opt_end;
                        }
                        if let Some((_, grp_end)) = script_arg(i) {
                            i = grp_end;
                        }
                        while trailing_opt
                            && let Some(opt_end) =
                                super::optional_argument_end(source, i).filter(|e| *e <= range.end)
                        {
                            i = opt_end;
                        }
                        if emit_gap {
                            *has_unknown = true;
                            append(&mut result, '\0', span);
                        }
                    }
                    MathCommand::Sqrt => {
                        if let Some(opt_end) =
                            super::optional_argument_end(source, i).filter(|e| *e <= range.end)
                        {
                            let opt_start = super::skip_source_space(source, i) + 1;
                            append(&mut result, '\0', span.clone());
                            let sub = atoms_with_macros(
                                source,
                                opt_start..opt_end - 1,
                                (macros, operation),
                                depth + 1,
                                span_override.clone(),
                                args,
                                has_unknown,
                            )?;
                            result.extend(sub);
                            append(&mut result, '\0', span.clone());
                            i = opt_end;
                        }
                        append(&mut result, '√', span);
                    }
                    MathCommand::ExtensibleArrow(arrow) => {
                        if let Some(opt_end) =
                            super::optional_argument_end(source, i).filter(|e| *e <= range.end)
                        {
                            let opt_start = super::skip_source_space(source, i) + 1;
                            append(&mut result, '\0', span.clone());
                            let sub = atoms_with_macros(
                                source,
                                opt_start..opt_end - 1,
                                (macros, operation),
                                depth + 1,
                                span_override.clone(),
                                args,
                                has_unknown,
                            )?;
                            result.extend(sub);
                            append(&mut result, '\0', span.clone());
                            i = opt_end;
                        }
                        append(&mut result, arrow, span.clone());
                        append(&mut result, '\0', span);
                    }
                    MathCommand::Symbol(c) => append(&mut result, c, span),
                    MathCommand::MultiSymbol(chars) => {
                        for &c in chars {
                            append(&mut result, c, span.clone());
                        }
                    }
                    MathCommand::Words(text) => append_words(&mut result, text, span),
                    MathCommand::Unknown => {
                        while let Some(end) = super::optional_argument_end(source, i)
                            .or_else(|| super::overlay_argument_end(source, i))
                            .filter(|e| *e <= range.end)
                        {
                            i = end;
                        }
                        *has_unknown = true;
                        append(&mut result, '\0', span);
                    }
                }
            }
            c if !c.is_whitespace() => {
                append_joined(&mut result, c, emit_span(start..i), continuing);
                prev_alnum_end = c.is_alphanumeric().then_some(i);
            }
            _ => {}
        }
    }
    Some(result)
}

fn private_use(c: char) -> bool {
    matches!(c as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd)
}

/// Collapse multi-glyph composite TeX symbols whose participating PDF pieces
/// (`\u{20d2}` prefix negation, `\u{0338}` negation overlay, `\mapstochar`
/// `\u{237f}`, `\mapsfromchar` `\u{23ae}`, `\lhook` `\u{297c}`, `\rhook`
/// `\u{297d}`, `\dasharrow` shaft `\u{23af}`, stacked harpoons `\u{296a}`/`\u{296b}`)
/// are dedicated non-symbol connector pieces that never occur as standalone math symbols.
fn collapse_pdf_composites(atoms: &mut Vec<Atom>) {
    let negated = |c: char| match c {
        '=' => Some('≠'),
        '∈' => Some('∉'),
        '∋' => Some('∌'),
        '∃' => Some('∄'),
        '≡' => Some('≢'),
        '<' => Some('≮'),
        '>' => Some('≯'),
        '≤' | '≦' => Some('≰'),
        '≥' | '≧' => Some('≱'),
        '≺' => Some('⊀'),
        '≻' => Some('⊁'),
        '⪯' => Some('⋠'),
        '⪰' => Some('⋡'),
        '⊂' => Some('⊄'),
        '⊃' => Some('⊅'),
        '⊆' | '⫅' => Some('⊈'),
        '⊇' | '⫆' => Some('⊉'),
        '⊑' => Some('⋢'),
        '⊒' => Some('⋣'),
        '~' | '∼' => Some('≁'),
        '≃' => Some('≄'),
        '≈' => Some('≉'),
        '≅' => Some('≇'),
        '≍' => Some('≭'),
        '⊢' => Some('⊬'),
        '⊨' => Some('⊭'),
        '⊩' => Some('⊮'),
        '⊪' => Some('⊯'),
        '◁' | '⊲' => Some('⋪'),
        '▷' | '⊳' => Some('⋫'),
        '⊴' => Some('⋬'),
        '⊵' => Some('⋭'),
        '|' => Some('∤'),
        '‖' => Some('∦'),
        '←' => Some('↚'),
        '→' => Some('↛'),
        '↔' => Some('↮'),
        '⇐' => Some('⇍'),
        '⇒' => Some('⇏'),
        '⇔' => Some('⇎'),
        _ => None,
    };
    let mut i = 0;
    while i + 1 < atoms.len() {
        if atoms[i].span.end != atoms[i + 1].span.start {
            i += 1;
            continue;
        }
        if i + 2 < atoms.len() && atoms[i + 1].span.end == atoms[i + 2].span.start {
            let triple = match (atoms[i].value, atoms[i + 1].value, atoms[i + 2].value) {
                ('⍿', '-', '→') => Some('⟼'),
                ('⍿', '=', '⇒') => Some('⟾'),
                ('←', '-', '⎮') => Some('⟻'),
                ('⇐', '=', '⎮') => Some('⟽'),
                ('⎯', '⎯', '⇢') => Some('⇢'),
                ('⇠', '⎯', '⎯') => Some('⇠'),
                _ => None,
            };
            if let Some(value) = triple {
                let end = atoms[i + 2].span.end;
                atoms[i].value = value;
                atoms[i].span.end = end;
                atoms[i].word_join = false;
                atoms.drain(i + 1..=i + 2);
                continue;
            }
        }
        let pair = match (atoms[i].value, atoms[i + 1].value) {
            ('\u{20d2}', c) | (c, '\u{0338}') => negated(c),
            ('⍿', '→') => Some('↦'),
            ('⍿', '⇒') => Some('⤇'),
            ('←', '⎮') | ('⎮', '←') => Some('↤'),
            ('⇐', '⎮') | ('⎮', '⇐') => Some('⤆'),
            ('⎯', '⇢') => Some('⇢'),
            ('⇠', '⎯') => Some('⇠'),
            ('⥼', '→') => Some('↪'),
            ('←', '⥽') => Some('↩'),
            ('⥪', '⥫') | ('⥫', '⥪') => Some('⇌'),
            _ => None,
        };
        if let Some(value) = pair {
            let end = atoms[i + 1].span.end;
            atoms[i].value = value;
            atoms[i].span.end = end;
            atoms[i].word_join = false;
            atoms.remove(i + 1);
            continue;
        }
        i += 1;
    }
}

#[cfg(test)]
pub(super) fn source_location(
    source: &str,
    line: u32,
    lines: Range<usize>,
    within_frame: bool,
    context: &str,
    offset: usize,
    prose: Option<(u32, usize, isize)>,
) -> Option<(u32, usize)> {
    let starts: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let limit = if let Some((row, _, _)) = prose {
        (*starts.get(lines.end).unwrap_or(&source.len()))
            .max(*starts.get(row as usize).unwrap_or(&source.len()))
    } else {
        *starts.get(lines.end).unwrap_or(&source.len())
    };
    source_location_with_context_mode(
        source,
        line,
        lines,
        within_frame,
        context,
        offset,
        prose.map_or(crate::tex::ProseRank::Absent, |(row, byte, score)| {
            crate::tex::ProseRank::Unique(row, byte, score)
        }),
        regions(source, limit),
        &[],
        &[],
        &[],
        &[],
        crate::tex::WordLocationMode::Strict,
        &crate::Operation::default(),
        None,
    )
    .ok()
    .flatten()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn source_location_with_context_mode(
    source: &str,
    line: u32,
    lines: Range<usize>,
    within_frame: bool,
    context: &str,
    offset: usize,
    prose: crate::tex::ProseRank,
    regions: Vec<Range<usize>>,
    extra_sources: &[&str],
    foreach_loops: &[crate::tex::ForeachLoop<'_>],
    opaque: &[Range<usize>],
    literal_owners: &[Range<usize>],
    mode: crate::tex::WordLocationMode,
    operation: &crate::Operation,
    execution_scope: Option<&Range<usize>>,
) -> Result<Option<(u32, usize)>, ()> {
    operation.check().map_err(|_| ())?;
    let starts: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let (prose, tied_score) = match prose {
        crate::tex::ProseRank::Absent => (None, None),
        crate::tex::ProseRank::Unique(row, byte, score) => (Some((row, byte, score)), None),
        crate::tex::ProseRank::Tied(score) => (None, Some(score)),
    };
    let prose = prose.filter(|(row, byte, _)| {
        let index = starts[*row as usize - 1] + byte;
        !regions.iter().any(|range| range.contains(&index))
    });
    let mut pdf = Vec::new();
    for (i, c) in context.char_indices() {
        let join = pdf
            .last()
            .is_some_and(|prev: &Atom| prev.value.is_alphanumeric() && prev.span.end == i);
        append_joined(&mut pdf, c, i..i + c.len_utf8(), join);
    }
    collapse_pdf_composites(&mut pdf);
    let Some(selected) = pdf.iter().position(|atom| atom.span.contains(&offset)) else {
        return Ok(prose.map(|(row, byte, _)| (row, byte)));
    };
    if matches!(pdf[selected].value, '\0'..='\x03') || private_use(pdf[selected].value) {
        return Ok(None);
    }
    // A letter inside an ordinary PDF word must match that entire word, not a
    // nearby single-letter variable with the same spelling.
    let mut word = selected..selected + 1;
    if pdf[selected].value.is_alphanumeric() {
        while word.start > 0 && pdf[word.start].word_join {
            word.start -= 1;
        }
        while word.end < pdf.len() && pdf[word.end].word_join {
            word.end += 1;
        }
    }
    let scope_start = execution_scope.map_or_else(
        || *starts.get(lines.start).unwrap_or(&source.len()),
        |scope| scope.start,
    );
    let scope_end = execution_scope.map_or_else(
        || *starts.get(lines.end).unwrap_or(&source.len()),
        |scope| scope.end,
    );
    let active_regions: Vec<_> = regions
        .into_iter()
        .filter(|range| range.start < scope_end && range.end > scope_start)
        .collect();
    if active_regions.is_empty() {
        return Ok(prose.map(|(row, byte, _)| (row, byte)));
    }
    let mut macro_sources = Vec::with_capacity(extra_sources.len() + 1);
    macro_sources.extend_from_slice(extra_sources);
    macro_sources.push(source);
    let macros = mode.macros(&macro_sources, operation);
    let mut best = None;
    let mut tied = false;
    let mut any_unknown = false;
    let mut line_unknown = false;
    let line_start = *starts.get(line as usize - 1).unwrap_or(&source.len());
    let line_end = *starts.get(line as usize).unwrap_or(&source.len());
    for range in active_regions {
        operation.check().map_err(|_| ())?;
        if execution_scope.is_some() && (range.start < scope_start || range.end > scope_end) {
            // A compiler-proven body never borrows a formula crossing its header boundary.
            return Err(());
        }
        let Some(bindings) = crate::tex::foreach_bindings_for_range(foreach_loops, &range) else {
            return Ok(None);
        };
        let mut region_unknown = false;
        let mut passes = Vec::with_capacity(bindings.len().max(1));
        if bindings.is_empty() {
            let Some(atoms) = atoms_with_macros(
                source,
                range.clone(),
                (&macros, operation),
                0,
                None,
                &[],
                &mut region_unknown,
            ) else {
                return Ok(None);
            };
            passes.push(atoms);
        } else {
            for binding in &bindings {
                let mut local_macros = macros.clone();
                for &(var, val) in binding {
                    if macros.contains_key(var) {
                        return Ok(None);
                    }
                    local_macros.insert(
                        var,
                        Some(MacroDef {
                            arity: 0,
                            body_source: val,
                            body: Some(val),
                            used_params: 0,
                        }),
                    );
                }
                let mut pass_unknown = false;
                let Some(atoms) = atoms_with_macros(
                    source,
                    range.clone(),
                    (&local_macros, operation),
                    0,
                    None,
                    &[],
                    &mut pass_unknown,
                ) else {
                    return Ok(None);
                };
                passes.push(atoms);
                region_unknown |= pass_unknown;
            }
        }
        any_unknown |= region_unknown;
        if range.start < line_end && range.end > line_start {
            line_unknown |= region_unknown;
        }
        for atoms in passes {
            let mut atoms = inline_context(source, &range, atoms);
            if let Some(scope) = execution_scope {
                atoms.retain(|atom| scope.start <= atom.span.start && atom.span.end <= scope.end);
            }
            for (index, atom) in atoms.iter().enumerate() {
                if !range.contains(&atom.span.start)
                    || matches!(atom.value, '\0'..='\x03')
                    || atom.value != pdf[selected].value
                {
                    continue;
                }
                let row = starts.partition_point(|start| *start <= atom.span.start);
                if !lines.contains(&(row - 1)) {
                    continue;
                }
                let Some(begin) = index.checked_sub(selected - word.start) else {
                    continue;
                };
                if atoms
                    .get(begin..begin + word.len())
                    .is_none_or(|source_word| {
                        !source_word
                            .iter()
                            .zip(&pdf[word.clone()])
                            .all(|(a, b)| a.value == b.value)
                    })
                {
                    continue;
                }
                // A PDF word cannot be a substring of a longer source word.
                if pdf[selected].value.is_alphanumeric()
                    && (atoms[begin].word_join
                        || atoms
                            .get(begin + word.len())
                            .is_some_and(|next| next.word_join))
                {
                    continue;
                }
                if opaque.iter().any(|range| range.contains(&atom.span.start))
                    && !literal_owners.iter().any(|owner| {
                        atoms[begin..begin + word.len()].iter().all(|atom| {
                            owner.start <= atom.span.start && atom.span.end <= owner.end
                        })
                    })
                {
                    return Err(());
                }
                // Stop at the first mismatch and at this expression's boundary.
                // Context from a different formula must not break an ambiguity tie.
                let joined = |slice: &[Atom], prev: usize, direction: isize| {
                    if direction < 0 {
                        slice[prev].word_join
                    } else {
                        slice.get(prev + 1).is_some_and(|next| next.word_join)
                    }
                };
                let score_with = |atoms: &[Atom], begin: usize| {
                    let mut score = 0;
                    let mut lexical_score = 0;
                    let mut contradictions = 0usize;
                    for direction in [-1isize, 1] {
                        let source_edge = if direction < 0 {
                            begin
                        } else {
                            begin + word.len() - 1
                        };
                        let pdf_edge = if direction < 0 {
                            word.start
                        } else {
                            word.end - 1
                        };
                        let mut committed = (score, lexical_score);
                        let mut word_start = 1;
                        for distance in 1..=8 {
                            let delta = direction * distance;
                            let in_word = if distance > 1 {
                                let prev_src = source_edge
                                    .checked_add_signed(direction * (distance - 1))
                                    .unwrap();
                                let prev_pdf = pdf_edge
                                    .checked_add_signed(direction * (distance - 1))
                                    .unwrap();
                                let join_a = joined(atoms, prev_src, direction);
                                let join_b = joined(&pdf, prev_pdf, direction);
                                if !join_a && !join_b {
                                    committed = (score, lexical_score);
                                    word_start = distance;
                                }
                                join_a && (join_b || range.contains(&atoms[prev_src].span.start))
                            } else {
                                false
                            };
                            match (
                                source_edge
                                    .checked_add_signed(delta)
                                    .and_then(|i| atoms.get(i)),
                                pdf_edge.checked_add_signed(delta).and_then(|i| pdf.get(i)),
                            ) {
                                (Some(a), Some(b))
                                    if !matches!(a.value, '\0'..='\x03')
                                        && a.value == b.value
                                        && (!in_word
                                            || joined(
                                                &pdf,
                                                pdf_edge
                                                    .checked_add_signed(direction * (distance - 1))
                                                    .unwrap(),
                                                direction,
                                            )) =>
                                {
                                    score += 9 - distance;
                                    lexical_score += usize::from(a.value.is_alphanumeric());
                                }
                                (Some(a), Some(_))
                                    if !matches!(a.value, '\0'..='\x03') && distance == 1 =>
                                {
                                    contradictions += 1;
                                    break;
                                }
                                _ => {
                                    if in_word {
                                        (score, lexical_score) = committed;
                                        if word_start == 1 {
                                            contradictions += 1;
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    (score, lexical_score, contradictions)
                };
                let (mut score, mut lexical_score, mut contradictions) = score_with(&atoms, begin);
                if !pdf[selected].value.is_alphanumeric()
                    && atoms
                        .get(begin + word.len())
                        .is_some_and(|a| a.value == '\x01')
                {
                    let end_op = begin + word.len();
                    let mut depth = 1usize;
                    let mut sep = None;
                    let mut multi_sep = false;
                    let mut close = None;
                    for (idx, a) in atoms.iter().enumerate().skip(end_op + 1) {
                        match a.value {
                            '\x01' => depth += 1,
                            '\x02' if depth == 1 => {
                                multi_sep |= sep.is_some();
                                sep = Some(idx);
                            }
                            '\x03' => {
                                depth -= 1;
                                if depth == 0 {
                                    close = Some(idx);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if let (false, Some(z2), Some(z3)) = (multi_sep, sep, close) {
                        let (before, op, s1, s2, after) = (
                            &atoms[..begin],
                            &atoms[begin..end_op],
                            &atoms[end_op + 1..z2],
                            &atoms[z2 + 1..z3],
                            &atoms[z3 + 1..],
                        );
                        for (left, right) in [
                            ([before, &[][..]], [s1, s2, after]),
                            ([before, &[][..]], [s2, s1, after]),
                            ([before, s1], [&[][..], s2, after]),
                            ([before, s2], [&[][..], s1, after]),
                        ] {
                            let candidate_begin = left[0].len() + left[1].len();
                            let seq: Vec<Atom> = left
                                .into_iter()
                                .chain([op])
                                .chain(right)
                                .flatten()
                                .cloned()
                                .collect();
                            let (s, l, c) = score_with(&seq, candidate_begin);
                            if (s, l > 0, std::cmp::Reverse(c), l)
                                > (
                                    score,
                                    lexical_score > 0,
                                    std::cmp::Reverse(contradictions),
                                    lexical_score,
                                )
                            {
                                (score, lexical_score, contradictions) = (s, l, c);
                            }
                        }
                    }
                }
                // A standalone punctuation match is not evidence that the clicked
                // glyph belongs to this formula rather than nearby prose.
                if !pdf[selected].value.is_alphanumeric() && score == 0 {
                    continue;
                }
                let location = (row as u32, atom.span.start - starts[row - 1]);
                let proximity =
                    if within_frame || score == 0 || mode == crate::tex::WordLocationMode::Guess {
                        0
                    } else {
                        location.0.abs_diff(line)
                    };
                let rank = (
                    score,
                    lexical_score > 0,
                    std::cmp::Reverse(contradictions),
                    std::cmp::Reverse(proximity),
                    lexical_score,
                );
                match best {
                    Some((old, _)) if rank < old => {}
                    Some((old, previous)) if rank == old => tied |= previous != location,
                    _ => {
                        best = Some((rank, location));
                        tied = false;
                    }
                }
            }
        }
    }
    let mathematical = best
        .filter(|((score, ..), location)| {
            let has_unknown = if within_frame || location.0 != line {
                any_unknown
            } else {
                line_unknown
            };
            !tied && (!has_unknown || word.len() > 1 || *score > 0)
        })
        .map(|(_, location)| location);
    let prose_evidence = mode == crate::tex::WordLocationMode::Strict
        && prose.is_some_and(|(_, _, score)| score >= 6)
        && best.is_some_and(|((_, has_lexical, _, _, _), _)| !has_lexical);
    if prose_evidence {
        return Ok(prose.map(|(row, byte, _)| (row, byte)));
    }
    if prose.is_some() && best.is_some() {
        return Ok(None);
    }
    // Tied prose competes with a formula just as unique prose does above, unless
    // no tied occurrence has evidence and an uncontradicted formula has lexical evidence.
    let math_beats_tie = tied_score.is_none_or(|score| {
        score == 0
            && best.is_some_and(|((score, has_lexical, contradictions, _, _), _)| {
                score > 0 && has_lexical && contradictions.0 == 0
            })
    });
    Ok(prose
        .map(|(row, byte, _)| (row, byte))
        .or(mathematical.filter(|_| math_beats_tie)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_group_boundaries_preserve_lexing_and_borrowed_slice_limits() {
        let source = "{α\\{literal% }\n{β} tail}";
        let mut boundaries = BTreeMap::new();
        assert_eq!(
            group_with_boundaries(source, 0, Some(&mut boundaries), None),
            group(source, 0),
        );
        assert_eq!(boundaries.len(), 2);
        let inner = source.find("{β}").unwrap();
        let slice = &source[inner..inner + "{β}".len()];
        assert_eq!(
            group_with_boundaries(slice, 0, Some(&mut boundaries), None),
            Some((1..3, 4)),
        );
        assert_eq!(
            group_with_boundaries(&slice[..slice.len() - 1], 0, Some(&mut boundaries), None),
            None,
        );
        let operation = crate::Operation::default();
        operation.cancel();
        assert_eq!(
            group_with_boundaries(source, 0, Some(&mut boundaries), Some(&operation)),
            None,
        );
    }

    #[test]
    fn caption_scopes_preserve_lines_after_skipped_captions() {
        let source = "prefix\n% ignored\n\\caption{\nfirst\n}\ntext\n\\caption*\n{\nsecond\n}";
        assert_eq!(collected_text_lines(source, 4), Some(2..5));
        assert_eq!(collected_text_lines(source, 9), Some(7..10));
        assert_eq!(collected_text_lines(source, 6), None);
        assert_eq!(collected_text_lines(source, 0), None);
        let source = "\\caption*[short {nested}]\n% between arguments\n{\nlong\n}";
        assert_eq!(collected_text_lines(source, 4), Some(2..5));
        assert_eq!(collected_text_lines(source, 1), None);
    }

    #[test]
    fn fancyvrb_closure_uses_only_the_first_end_marker_on_each_input_line() {
        for environment in ["Verbatim", "Verbatim*", "minted", "minted*"] {
            for newline in ["\n", "\r\n"] {
                let source = format!(
                    "\\begin{{{environment}}}{newline}\
                     \\end{{other}} ignored \\end{{{environment}}}{newline}\
                     \\caption{{LiteralCopper{newline}LiteralNickel{newline}}}{newline}\
                     prefix \\end{{{environment}}}   {newline}\
                     \\caption{{VisibleCopper{newline}VisibleNickel{newline}}}"
                );
                let visible = source.find("\\caption{VisibleCopper").unwrap();
                assert_eq!(
                    opaque_command_end(&source, "begin", 6),
                    Some((visible, false))
                );
                for word in ["LiteralCopper", "LiteralNickel"] {
                    assert_eq!(
                        collected_text_argument_at(&source, source.find(word).unwrap()),
                        None,
                        "{environment}, {word}"
                    );
                }
                let argument =
                    collected_text_argument_at(&source, source.find("VisibleNickel").unwrap())
                        .unwrap();
                assert_eq!(
                    &source[argument],
                    format!("VisibleCopper{newline}VisibleNickel{newline}")
                );
                let unclosed = &source[..source.find("prefix").unwrap()];
                assert_eq!(
                    opaque_command_end(unclosed, "begin", 6),
                    Some((unclosed.len(), false))
                );
            }
        }
    }

    #[test]
    fn other_printed_opaque_environments_keep_their_inline_closure() {
        for environment in ["verbatim", "verbatim*", "lstlisting", "lstlisting*"] {
            let source = format!(
                "\\begin{{{environment}}}\\end{{other}} \\end{{{environment}}}\
                 \\caption{{VisibleCopper}}"
            );
            let visible = source.find("\\caption").unwrap();
            assert_eq!(
                opaque_command_end(&source, "begin", 6),
                Some((visible, false))
            );
            assert!(
                collected_text_argument_at(&source, source.find("VisibleCopper").unwrap())
                    .is_some()
            );
        }
    }

    #[test]
    fn math_regions_respect_tex_delimiters_and_comments() {
        let source = "\\$ ignored % $not math$\n$one$ \\(two\\) \\[three\\] $$four$$ \\begin{align*}five\\end{align*}\n\\verb|$hidden$| \\begin{verbatim}$hidden$\\end{verbatim} $unterminated";
        for word in ["one", "two", "three", "four", "five"] {
            assert_eq!(
                source_location(source, 2, 0..4, false, word, 0, None),
                Some((2, source.lines().nth(1).unwrap().find(word).unwrap()))
            );
        }
        for word in ["ignored", "hidden", "unterminated"] {
            assert_eq!(source_location(source, 2, 0..4, false, word, 0, None), None);
        }
    }

    #[test]
    fn non_rendered_delimiters_and_environment_arguments_are_not_candidates() {
        for (source, glyph) in [
            (r"$\left. x\right|$", "."),
            (r"$\sqrt[3]{x}$", "["),
            (r"\begin{alignat}{2}x&=y\end{alignat}", "2"),
            (r"$\begin{array}{c}x\end{array}$", "c"),
        ] {
            assert_eq!(
                source_location(source, 1, 0..1, false, glyph, 0, None),
                None
            );
        }
        let source = r"\begin{alignat}{2}\alpha&=\beta\end{alignat}";
        assert_eq!(
            source_location(source, 1, 0..1, false, "α=β", 0, None),
            Some((1, source.find(r"\alpha").unwrap()))
        );
    }

    #[test]
    fn mathematical_commands_and_styles_keep_original_spans() {
        let source = r"$\alpha\longmapsto\mathcal H+\mathbf v$";
        for (context, selected, needle) in [
            ("𝛼⟼ℋ+𝐯", "𝛼", "\\alpha"),
            ("𝛼⟼ℋ+𝐯", "⟼", "\\longmapsto"),
            ("𝛼⟼ℋ+𝐯", "ℋ", "H"),
            ("𝛼⟼ℋ+𝐯", "𝐯", "v"),
        ] {
            assert_eq!(
                source_location(
                    source,
                    1,
                    0..1,
                    false,
                    context,
                    context.find(selected).unwrap(),
                    None
                ),
                Some((1, source.find(needle).unwrap()))
            );
        }
        let matrix = "\\begin{align*}\\dot f(t)&=\\begin{pmatrix}\\cos\\alpha_1(t)\\\\\\sin\\alpha_2(t)\\end{pmatrix}\\end{align*}";
        let context = "f(t)=cosα1(t) sinα2(t)";
        assert_eq!(
            source_location(
                matrix,
                1,
                0..1,
                false,
                context,
                context.find("sin").unwrap(),
                None
            ),
            Some((1, matrix.find("\\sin").unwrap()))
        );
    }

    #[test]
    fn unknown_commands_insert_gap_atoms_without_poisoning_known_tokens() {
        let source = "$x\\longmapsto y+\\custom{z}$\n$x\\longmapsto y$";
        assert_eq!(source_location(source, 2, 0..2, true, "x⟼y", 1, None), None);
        let source = "$x\\longmapsto\\custom y$\n$x\\longmapsto h$";
        assert_eq!(
            source_location(source, 2, 0..2, true, "x⟼y", 1, None),
            Some((1, 2))
        );
        assert_eq!(
            source_location(source, 2, 0..2, true, "x⟼h", 1, None),
            Some((2, 2))
        );
        let source = "$\\alpha\\custom{z}$\n$\\beta$";
        assert_eq!(source_location(source, 2, 0..2, true, "β", 0, None), None);
        let source = "$\\alpha\\custom{z}$\n$\\beta+1$";
        assert_eq!(
            source_location(source, 2, 0..2, true, "β+1", 0, None),
            Some((2, 1))
        );
        let source = "$\\custom{z}$\nA. Box in 2020";
        assert_eq!(
            source_location(source, 2, 0..2, true, "A. Box", 0, Some((2, 0, 3))),
            Some((2, 0))
        );
        assert_eq!(
            source_location(
                source,
                2,
                0..2,
                true,
                "in 2020",
                "in ".len(),
                Some((2, 10, 3))
            ),
            Some((2, 10))
        );
        for (bad_source, context) in [
            ("$\\mathchoice{x+a}{\\char120+a}{z+c}{w+d}$", "x+a"),
            ("$\\iffalse x+a\\else\\char120+a\\fi$", "x+a"),
            ("$\\phantom x+a$\n$\\char120+a$", "x+a"),
            ("\\def\\hide#1{}\n$\\hide x+a$\n$\\char120+a$", "x+a"),
            ("$\\sqrt[\\char110]{x}$\n$n$", "n"),
            ("$\\sqrt[\\custom]{x}$\n$n$", "n"),
        ] {
            let line_count = bad_source.lines().count();
            assert_eq!(
                source_location(bad_source, 1, 0..line_count, true, context, 0, None),
                None,
                "{bad_source}"
            );
        }
        assert_eq!(
            source_location("$x\\kern1pty$", 1, 0..1, false, "xy", 1, None),
            Some((1, 10))
        );
        assert_eq!(
            source_location("$x\\kern1truept y$", 1, 0..1, false, "xy", 1, None),
            Some((1, 15))
        );
        assert_eq!(
            source_location("$A_\\custom$\n$x$", 2, 0..2, false, "x", 0, None),
            Some((2, 1))
        );
        let shared_suffix = "\\emph{sample} $M$:\n\\item a local state $M_x = y$";
        assert_eq!(
            source_location(
                shared_suffix,
                1,
                0..2,
                true,
                "sample M: a local state",
                "sample ".len(),
                None,
            ),
            Some((1, 15))
        );
    }

    #[test]
    fn duplicate_equations_unknown_commands_and_private_glyphs() {
        let source = "$\\alpha\\leq\\beta$\n$\\gamma\\geq\\delta$\n$\\alpha\\leq\\beta$";
        assert_eq!(
            source_location(source, 3, 0..3, true, "α≤β γ≥δ α≤β", 0, None),
            None
        );
        assert_eq!(
            source_location("$\\custom{x}+y$", 1, 0..1, false, "x+y", 0, None),
            Some((1, 9))
        );
        assert_eq!(
            source_location("$\\mathcal H$", 1, 0..1, false, "\u{e234}", 0, None),
            None
        );
        assert_eq!(
            source_location("$x$", 1, 0..1, false, "example", 1, None),
            None
        );
        assert_eq!(
            source_location("$X+x$", 1, 0..1, false, "𝑋+𝑥", 0, None),
            Some((1, 1))
        );
    }

    #[test]
    fn unsupported_neighbors_and_unanchored_punctuation_remain_coarse() {
        for (source, context, offset) in [
            ("Hello.\n$x.y$", "Hello. x.y", 5),
            ("$\\identity{\\alpha}$\n$\\alpha$", "α α", 0),
            ("$\\alpha_a^b$\n$\\alpha_b^a$", "α\r\nba\r\nα\r\nab", 0),
        ] {
            assert_eq!(
                source_location(source, 1, 0..2, false, context, offset, None),
                None,
                "{source}"
            );
        }
        assert_eq!(
            source_location("$x.y$", 1, 0..1, false, "x.y", 1, None),
            Some((1, 2))
        );
    }

    #[test]
    fn repeated_inline_math_across_soft_wraps_preserves_occurrence_ownership() {
        let source = "\
  \\begin{enumerate}
    \\item if \\(u\\in\\mathcal M_k(a_0,a_1)\\) is regular and
      \\(q_*\\) is its initial sample value, then \\(q_*\\) scales \\(\\Omega_k\\) on
      \\(B_n^{\\le k}\\);
    \\item similarly, if \\(q_*\\in B_n^{\\le k}\\) scales
      \\(\\Omega_k\\), then the
      combination of any local sample interval
  \\end{enumerate}";
        let context = "similarly, if q* ∈ Bn≤k scales Ωk,";
        let offset = context.find("Ωk").unwrap();
        assert_eq!(
            source_location(source, 6, 1..8, false, context, offset, None),
            Some((6, source.lines().nth(5).unwrap().find("\\Omega_k").unwrap()))
        );
    }

    #[test]
    fn caption_lines_ignore_nonprinting_and_opaque_captions() {
        let source = "\
\\begin{comment}
\\caption{Hidden
comment
caption}
\\end{comment}
\\newcommand{\\mycap}{\\caption{Macro
caption}}
\\caption{Visible
caption}";
        assert_eq!(collected_text_lines(source, 3), None);
        assert_eq!(collected_text_lines(source, 6), None);
        assert_eq!(collected_text_lines(source, 9), Some(7..9));
    }

    #[test]
    fn text_math_modifiers_greek_variants_and_standard_symbols_match() {
        for wrapper in [
            "text",
            "texttt",
            "textnormal",
            "textit",
            "textbf",
            "textrm",
            "textsf",
            "textup",
            "textsc",
            "textsl",
            "textmd",
            "mathrm",
            "mathit",
            "mathbf",
            "mathsf",
            "mathtt",
            "mathcal",
            "mathbb",
            "mathfrak",
            "mathscr",
            "mathnormal",
            "symrm",
            "symit",
            "symbf",
            "symsf",
            "symtt",
            "symbb",
            "symcal",
            "symscr",
            "symfrak",
            "symnormal",
        ] {
            let source = format!("$\\{wrapper}{{K}}+y$");
            assert_eq!(
                source_location(&source, 1, 0..1, false, "K+y", 0, None),
                Some((1, source.find("{K}").unwrap() + 1)),
                "{wrapper}"
            );
        }

        let greek = "$\\varkappa+\\varepsilon+\\varphi+\\vartheta+\\varrho+\\varpi+\\varsigma+\\varGamma+\\varDelta+\\varOmega$";
        let greek_pdf = "ϰ+ϵ+ϕ+ϑ+ϱ+ϖ+ς+Γ+∆+Ω";
        for (needle, tex) in [
            ("ϰ", "\\varkappa"),
            ("ϵ", "\\varepsilon"),
            ("ϕ", "\\varphi"),
            ("ϑ", "\\vartheta"),
            ("ϱ", "\\varrho"),
            ("ϖ", "\\varpi"),
            ("ς", "\\varsigma"),
            ("Γ", "\\varGamma"),
            ("∆", "\\varDelta"),
            ("Ω", "\\varOmega"),
        ] {
            assert_eq!(
                source_location(
                    greek,
                    1,
                    0..1,
                    false,
                    greek_pdf,
                    greek_pdf.find(needle).unwrap(),
                    None,
                ),
                Some((1, greek.find(tex).unwrap())),
                "{tex}"
            );
        }

        let symbols = "$a\\le b\\ge c\\ni d\\owns e\\bigotimes f\\frown g\\smallsetminus h\\neg p\\not\\in S\\not=0\\dots\\ast\\|v\\|\\mid w$";
        let symbols_pdf = "a≤b≥c∋d∋e⨂f⌢g∖h¬p∉S≠0⋯*‖v‖|w";
        for (needle, tex) in [
            ("≤", "\\le"),
            ("≥", "\\ge"),
            ("⨂", "\\bigotimes"),
            ("⌢", "\\frown"),
            ("∖", "\\smallsetminus"),
            ("¬", "\\neg"),
            ("∉", "\\not\\in"),
            ("≠", "\\not="),
            ("⋯", "\\dots"),
            ("*", "\\ast"),
            ("|w", "\\mid"),
        ] {
            assert_eq!(
                source_location(
                    symbols,
                    1,
                    0..1,
                    false,
                    symbols_pdf,
                    symbols_pdf.find(needle).unwrap(),
                    None,
                ),
                Some((1, symbols.find(tex).unwrap())),
                "{tex}"
            );
        }

        let trig = "$x\\sin\\theta + \\Pr(A) + \\gcd(m,n)$";
        let trig_pdf = "x sin θ + Pr(A) + gcd(m,n)";
        for (needle, tex) in [
            ("x", "x"),
            ("sin", "\\sin"),
            ("θ", "\\theta"),
            ("Pr", "\\Pr"),
            ("gcd", "\\gcd"),
        ] {
            assert_eq!(
                source_location(
                    trig,
                    1,
                    0..1,
                    false,
                    trig_pdf,
                    trig_pdf.find(needle).unwrap(),
                    None,
                ),
                Some((1, trig.find(tex).unwrap())),
                "{tex}"
            );
        }
        assert_eq!(
            source_location("$\\sin x$", 1, 0..1, false, "s", 0, None),
            None
        );

        let liminf_frame = "$\\liminf x$\n$\\lim x$";
        assert_eq!(
            source_location(liminf_frame, 1, 0..2, true, "lim inf x", 0, None),
            Some((1, 1))
        );
        assert_eq!(
            source_location(liminf_frame, 1, 0..2, true, "lim inf x", 4, None),
            Some((1, 1))
        );

        let redefined_not = "\\renewcommand{\\in}{=}\n$a\\not\\in b$\n$a\\not=b$";
        assert_eq!(
            source_location(redefined_not, 2, 1..3, true, "a≠b", 0, None),
            None
        );
        assert_eq!(
            source_location(
                "$a\\not<b\\not>c\\not\\leq d\\not\\geq e$",
                1,
                0..1,
                false,
                "a≮b≯c≰d≱e",
                "a≮b≯c".len(),
                None,
            ),
            Some((1, 14))
        );
        let not_ll = "Frame prose\n$x\\not\\ll y + z$\n$\\hspace{0.5\\textwidth}w$";
        assert_eq!(
            source_location(not_ll, 2, 0..3, true, "x/≪y + z", "x/".len(), None),
            Some((2, 6))
        );
        assert_eq!(
            source_location(not_ll, 3, 0..3, true, "w", 0, None),
            Some((3, 23))
        );
        assert_eq!(
            source_location(not_ll, 3, 0..3, true, "prose", 0, Some((1, 6, 5))),
            Some((1, 6))
        );
    }

    #[test]
    fn big_operators_with_simultaneous_limits_resolve_in_inline_and_display_orders() {
        let display =
            "\\[\n  A = \\bigoplus_{i=1}^n U_i\n\\]\n\\[\n  B = \\bigoplus_{k=1}^m V_k\n\\]";
        let visual_ctx = "A = ⊕n\r\ni=1 Ui B = ⊕m\r\nk=1 Vk";
        let first_op = visual_ctx.find('⊕').unwrap();
        let second_op = visual_ctx.rfind('⊕').unwrap();
        assert_eq!(
            source_location(display, 2, 0..6, true, visual_ctx, first_op, None),
            Some((2, 6))
        );
        assert_eq!(
            source_location(display, 5, 0..6, true, visual_ctx, second_op, None),
            Some((5, 6))
        );

        let stream_ctx = "A = n ⊕ i=1 Ui B = m ⊕ k=1 Vk";
        assert_eq!(
            source_location(
                display,
                2,
                0..6,
                true,
                stream_ctx,
                stream_ctx.find('⊕').unwrap(),
                None
            ),
            Some((2, 6))
        );
        assert_eq!(
            source_location(
                display,
                5,
                0..6,
                true,
                stream_ctx,
                stream_ctx.rfind('⊕').unwrap(),
                None
            ),
            Some((5, 6))
        );

        let inline = "Consider $\\bigoplus_{i=1}^n V_i$ and $\\bigoplus$.";
        let inline_ctx = "Consider ⊕n\r\ni=1 Vi and ⊕.";
        assert_eq!(
            source_location(
                inline,
                1,
                0..1,
                false,
                inline_ctx,
                inline_ctx.find('⊕').unwrap(),
                None
            ),
            Some((1, 10))
        );
        assert_eq!(
            source_location(
                inline,
                1,
                0..1,
                false,
                inline_ctx,
                inline_ctx.rfind('⊕').unwrap(),
                None
            ),
            Some((1, 38))
        );

        let nested = "\\[\n  A = \\bigoplus_{i=1}^{n_j^k} U_i\n\\]\n\\[\n  B = \\bigoplus_{i=1}^{m_j^k} V_i\n\\]";
        let nested_ctx = "A = ⊕njk i=1 Ui B = ⊕mjk i=1 Vi";
        assert_eq!(
            source_location(
                nested,
                5,
                0..6,
                true,
                nested_ctx,
                nested_ctx.rfind('⊕').unwrap(),
                None
            ),
            Some((5, 6))
        );

        let refs = "\\[\n  \\bigoplus\\ref{a}x\\ref{b}y\\ref{c}z\n\\]\n\\[\n  \\bigoplus x y\n\\]";
        let refs_ctx = "⊕1x2y3z ⊕xy";
        assert_eq!(
            source_location(
                refs,
                5,
                0..6,
                true,
                refs_ctx,
                refs_ctx.rfind('⊕').unwrap(),
                None
            ),
            Some((5, 2))
        );

        let isolated = "\\[\\bigoplus\\]\n\\ensuremath{\\oplus}";
        let isolated_ctx = "⊕ ⊕";
        assert_eq!(
            source_location(
                isolated,
                2,
                0..2,
                true,
                isolated_ctx,
                isolated_ctx.rfind('⊕').unwrap(),
                None
            ),
            None
        );

        let composites = "\\[\n  A \\mapsto B \\longmapsto C \\hookleftarrow D \\hookrightarrow E \\dashrightarrow F \\dashleftarrow G \\rightleftharpoons H \\notin I \\nleqslant J\n\\]\n\\[\n  X \\mapsto Y \\longmapsto Z \\hookleftarrow W \\hookrightarrow K \\dashrightarrow U \\dashleftarrow V \\rightleftharpoons T \\notin S \\nleqslant R\n\\]";
        let composites_ctx = "A \u{237f}\u{2192} B \u{237f}-\u{2192} C \u{2190}\u{297d} D \u{297c}\u{2192} E \u{23af}\u{23af}\u{21e2} F \u{21e0}\u{23af}\u{23af} G \u{296a}\u{296b} H \u{20d2}\u{220a} I \u{2a7d}\u{0338} J X \u{237f}\u{2192} Y \u{237f}-\u{2192} Z \u{2190}\u{297d} W \u{297c}\u{2192} K \u{23af}\u{23af}\u{21e2} U \u{21e0}\u{23af}\u{23af} V \u{296a}\u{296b} T \u{2208}\u{0338} S \u{2264}\u{0338} R";
        for (needle, col) in [
            ("\u{237f}\u{2192} Y", 4),
            ("\u{237f}-\u{2192} Z", 14),
            ("\u{2190}\u{297d} W", 28),
            ("\u{297c}\u{2192} K", 45),
            ("\u{23af}\u{23af}\u{21e2} U", 63),
            ("\u{21e0}\u{23af}\u{23af} V", 81),
            ("\u{23af} V", 81),
            ("\u{296a}\u{296b} T", 98),
            ("\u{2208}\u{0338} S", 119),
            ("\u{2264}\u{0338} R", 128),
        ] {
            let offset = composites_ctx.find(needle).unwrap();
            assert_eq!(
                source_location(composites, 5, 0..6, true, composites_ctx, offset, None),
                Some((5, col)),
                "failed for {needle}"
            );
        }

        let non_collapsing = "\\[\n  f \\mapsto \\rightarrow g \\quad \\rightharpoonup\\leftharpoondown \\quad a = \\not= b \\quad x \\notin \\notin y\n\\]";
        let non_collapsing_ctx = "f\u{237f}\u{2192}\u{2192}g \u{21c0}\u{21bd} a=\u{20d2}=b x\u{2208}\u{0338}\u{2208}\u{0338}y";
        assert_eq!(
            source_location(
                non_collapsing,
                2,
                0..3,
                true,
                non_collapsing_ctx,
                non_collapsing_ctx.rfind('\u{2192}').unwrap(),
                None
            ),
            Some((2, 12))
        );
        assert_eq!(
            source_location(
                non_collapsing,
                2,
                0..3,
                true,
                non_collapsing_ctx,
                non_collapsing_ctx.find('\u{21bd}').unwrap(),
                None
            ),
            Some((2, 47))
        );
        assert_eq!(
            source_location(
                non_collapsing,
                2,
                0..3,
                true,
                non_collapsing_ctx,
                non_collapsing_ctx.find('=').unwrap(),
                None
            ),
            Some((2, 72))
        );
        assert_eq!(
            source_location(
                non_collapsing,
                2,
                0..3,
                true,
                non_collapsing_ctx,
                non_collapsing_ctx.rfind('\u{2208}').unwrap(),
                None
            ),
            Some((2, 97))
        );

        let stmary = "\\[\n  \\llbracket x \\rrbracket \\sslash y \\interleave z \\trianglelefteqslant w\n\\]\n\\[\n  \\llbracket a \\rrbracket \\sslash b \\interleave c \\trianglelefteqslant d\n\\]";
        let stmary_ctx = "⟦x⟧ ⫽ y ⫴ z ⊴ w ⟦a⟧ ⫽ b ⫴ c ⊴ d";
        assert_eq!(
            source_location(
                stmary,
                5,
                0..6,
                true,
                stmary_ctx,
                stmary_ctx.rfind('⫴').unwrap(),
                None
            ),
            Some((5, 36))
        );

        let variants = "\\[\n  \\vartheta + 7 + \\lgroup x \\rgroup + a = b + \\not\\rightarrow c + u \\coloneqq v + p \\bbslash q \\setminus r + m \\Mapsto n \\Longmapsto k\n\\]";
        let variants_ctx = "\u{1D6DD} + \u{1D7DF} + ⟮x⦘ + a\u{0338}=b + \u{20d2}→ c + u ≔ v + p ⧵ q \\ r + m \u{237f}⇒ n \u{237f}=⇒ k";
        assert_eq!(
            source_location(variants, 2, 0..3, true, variants_ctx, 0, None),
            Some((2, 2))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find('\u{1D7DF}').unwrap(),
                None
            ),
            Some((2, 14))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find('⦘').unwrap(),
                None
            ),
            Some((2, 28))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find('=').unwrap(),
                None
            ),
            Some((2, 40))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find("→ c").unwrap(),
                None
            ),
            Some((2, 46))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find('≔').unwrap(),
                None
            ),
            Some((2, 68))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find('⧵').unwrap(),
                None
            ),
            Some((2, 84))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find("\\ r").unwrap(),
                None
            ),
            Some((2, 95))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find("\u{237f}⇒ n").unwrap(),
                None
            ),
            Some((2, 111))
        );
        assert_eq!(
            source_location(
                variants,
                2,
                0..3,
                true,
                variants_ctx,
                variants_ctx.find("\u{237f}=⇒ k").unwrap(),
                None
            ),
            Some((2, 121))
        );
    }
}
