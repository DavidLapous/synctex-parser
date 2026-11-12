//! Conservative lexical matching, not a TeX macro expander.
use crate::symbols::{
    MathCommand, canonical, classify_command, is_math_alphabet_wrapper, is_text_font_wrapper,
    negate_char, negated_symbol,
};
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MacroDef<'a> {
    pub(super) arity: u8,
    pub(super) used_params: u16,
    /// Bit `i` set: parameter `#i+1` is a bracketed optional argument a call
    /// may omit. Such a replacement text is never expanded inline.
    pub(crate) opt_params: u16,
    /// Expandable replacement text; `None` when only the signature is trusted.
    pub(super) body: Option<&'a str>,
    /// The replacement text invokes `\note` (directly or via another wrapper),
    /// so words in a call's discarded arguments are note-owned, not absent.
    pub(crate) calls_note: bool,
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

pub(super) fn group(source: &str, start: usize) -> Option<(Range<usize>, usize)> {
    let start = super::skip_source_space(source, start);
    if source.as_bytes().get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 1;
    let mut i = start + 1;
    while i < source.len() {
        match source.as_bytes()[i] {
            b'\\' => {
                i = command(source, i).1;
                continue;
            }
            b'%' => {
                i = super::comment_end(source, i);
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
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
        b'{' => group(source, pos).filter(|(_, end)| *end <= limit),
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

pub(super) fn quoted_token_end(source: &str, name: &str, mut at: usize) -> Option<usize> {
    if !matches!(name, "string" | "meaning" | "noexpand") {
        return None;
    }
    let mut at_line_start = false;
    while at < source.len() {
        match source.as_bytes()[at] {
            b'%' => {
                at += source[at..].find('\n').map_or(source.len() - at, |n| n + 1);
                at_line_start = true;
            }
            b' ' | b'\t' | b'\r' => at += 1,
            b'\n' => {
                at += 1;
                if at_line_start {
                    return Some(at);
                }
                at_line_start = true;
            }
            _ => break,
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
        "string" | "meaning" | "noexpand" => {
            quoted_token_end(source, name, at).map(|end| (end, false))
        }
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
            let (body, end) = group(source, at)?;
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
    is_text_font_wrapper(name) || is_math_alphabet_wrapper(name)
}

pub(super) fn collected_text_arguments(
    source: &str,
    name: &str,
    mut at: usize,
) -> Option<(Option<Range<usize>>, Range<usize>, usize)> {
    match name {
        name if collected_formatting_command(name) => {
            let (body, end) = group(source, at)?;
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
            let (body, end) = group(source, at)?;
            Some((None, body, end))
        }
        "captionof" => {
            at += usize::from(source.as_bytes().get(at) == Some(&b'*'));
            let (_, end) = group(source, at)?;
            at = end;
            if let Some(opt_end) = super::optional_argument_end(source, at) {
                at = opt_end;
            }
            let (body, end) = group(source, at)?;
            Some((None, body, end))
        }
        "subcaptionbox" | "captionbox" => {
            at += usize::from(source.as_bytes().get(at) == Some(&b'*'));
            if let Some(opt_end) = super::optional_argument_end(source, at) {
                at = opt_end;
            }
            let (heading, mut at) = group(source, at)?;
            for _ in 0..2 {
                if let Some(opt_end) = super::optional_argument_end(source, at) {
                    at = opt_end;
                }
            }
            let (contents, end) = group(source, at)?;
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
            let (contents, end) = group(source, at)?;
            Some((opts.pop(), contents, end))
        }
        "begin" | "item" => multiline_printed_header(source, name, at),
        _ => None,
    }
}

pub(super) fn printed_header_argument(source: &str, name: &str, at: usize) -> Option<Range<usize>> {
    let at = match name {
        "begin" => {
            let (env, at) = group(source, at)?;
            let env = source[env].trim();
            if math_environment(env)
                || super::classify_environment_header(env) == super::EnvHeaderKind::NoPrintedHeader
            {
                return None;
            }
            at
        }
        "item" => at,
        _ => return None,
    };
    super::header_optional_argument(source, super::skip_overlay_arguments(source, at))
}

// TeX scans `[...]` whole; a single line does not establish an exclusive search region.
fn multiline_printed_header(
    source: &str,
    name: &str,
    at: usize,
) -> Option<(Option<Range<usize>>, Range<usize>, usize)> {
    let header = printed_header_argument(source, name, at)?;
    let body = header.start + 1..header.end - 1;
    source[body.clone()]
        .contains('\n')
        .then_some((None, body, header.end))
}

// Closing-line anchors alone are exclusive: inner material may ship on earlier lines.
fn closing_anchor_collector(name: &str) -> bool {
    collected_formatting_command(name) || matches!(name, "begin" | "item")
}

/// Find the outer collected printed argument owning this literal cursor byte.
pub(super) fn collected_text_argument_at_with_spans(
    source: &str,
    byte: usize,
    known_spans: &[crate::tex::Span],
) -> Option<Range<usize>> {
    collected_text_owner_at_with_spans(source, byte, known_spans)
        .and_then(|(body, printed)| printed.then_some(body))
}

/// Find the outer collected argument around this byte and whether the byte itself prints.
pub(super) fn collected_text_owner_at_with_spans(
    source: &str,
    byte: usize,
    known_spans: &[crate::tex::Span],
) -> Option<(Range<usize>, bool)> {
    if byte >= source.len() || !source.is_char_boundary(byte) {
        return None;
    }
    let mut hidden = known_spans.iter().peekable();
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
                return body.map(|body| (body, false));
            }
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => {
                at = super::comment_end(source, at);
                if byte < at {
                    return body.map(|body| (body, false));
                }
            }
            b'\\' => {
                let (name, end) = command(source, at);
                at = end;
                if let Some((end, _)) = opaque_command_end(source, name, at) {
                    at = end;
                    if byte < at {
                        return body.map(|body| (body, false));
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
                            return body.map(|body| (body, false));
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
    body.map(|body| (body, true))
}

/// Find the last printed header closing on `row` and whether its `[...]` spans multiple lines.
pub(super) fn printed_header_bounds_in(
    source: &str,
    row: Range<usize>,
    known_spans: &[crate::tex::Span],
) -> Option<(Range<usize>, bool)> {
    let limit = row.end.min(source.len());
    if !source[row.start.min(limit)..limit].contains(']') {
        return None;
    }
    let mut hidden = known_spans.iter().peekable();
    let mut found = None;
    let mut at = 0;
    while at < limit {
        while hidden.peek().is_some_and(|(_, range, _)| range.end <= at) {
            hidden.next();
        }
        if let Some((start, range, _)) = hidden.peek()
            && *start <= at
        {
            at = range.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => at += source[at..limit].find('\n').unwrap_or(limit - at),
            b'\\' => {
                let cmd_start = at;
                let (name, end) = command(source, at);
                at = end;
                if let Some((end, _)) = opaque_command_end(source, name, at) {
                    at = end;
                } else if let Some(header) = printed_header_argument(source, name, at) {
                    let close = header.end - 1;
                    let mut after = super::skip_overlay_arguments(source, header.end);
                    while let Some(next) = source[after.min(limit)..limit]
                        .bytes()
                        .position(|b| !matches!(b, b' ' | b'\t'))
                        .map(|off| after + off)
                    {
                        if source.as_bytes()[next] == b'\\'
                            && let ("label", cmd_end) = command(source, next)
                            && let Some((_, grp_end)) = group(source, cmd_end)
                            && grp_end <= limit
                        {
                            after = super::skip_overlay_arguments(source, grp_end);
                        } else {
                            break;
                        }
                    }
                    if row.contains(&close) {
                        let start = if header.start < row.start {
                            row.start
                        } else {
                            cmd_start.max(row.start)
                        };
                        let multiline = source[header.start + 1..close].contains('\n');
                        found = Some((start..after, multiline));
                    }
                    at = after;
                }
            }
            _ => {
                let next_special = source.as_bytes()[at..limit]
                    .iter()
                    .position(|b| matches!(*b, b'%' | b'\\'))
                    .map_or(limit, |pos| at + pos);
                let stop = hidden
                    .peek()
                    .map_or(next_special, |(start, _, _)| next_special.min(*start));
                at = stop.max(at + source[at..].chars().next()?.len_utf8());
            }
        }
    }
    found
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

/// Closed, literal math regions; escaped dollars and comments do not qualify.
pub(super) fn regions_with_spans(
    source: &str,
    known_spans: &[crate::tex::Span],
) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut hidden = known_spans.iter().peekable();
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
            '%' => i = super::comment_end(source, i),
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

fn display_regions<'a>(
    source: &'a str,
    known_spans: &[crate::tex::Span],
) -> impl Iterator<Item = DisplayRegion<'a>> {
    let mut counted_to = 0;
    let mut counted_lines = 0;
    regions_with_spans(source, known_spans)
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
pub(super) fn math_lines_with_spans(
    source: &str,
    line: u32,
    known_spans: &[crate::tex::Span],
) -> Option<Range<usize>> {
    let target = line.checked_sub(1)? as usize;
    for region in display_regions(source, known_spans) {
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
pub(super) fn math_closing_line_with_spans(
    source: &str,
    line: u32,
    column: u32,
    known_spans: &[crate::tex::Span],
) -> Option<u32> {
    let target = line.checked_sub(1)? as usize;
    for region in display_regions(source, known_spans) {
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
                        if let Some((_, grp_end)) = group(source, next) {
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
pub(super) fn collected_text_lines_with_spans(
    source: &str,
    line: u32,
    known_spans: &[crate::tex::Span],
    ignored: &[Range<usize>],
) -> Option<Range<usize>> {
    let target = line.checked_sub(1)? as usize;
    let mut hidden = known_spans.iter().peekable();
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
            i = super::comment_end(source, i);
        } else if ch == '\\' {
            let (name, end) = command(source, i);
            i = end;
            if let Some((end, _)) = opaque_command_end(source, name, i) {
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
                if closing_anchor_collector(name) && (first..last).contains(&target) {
                    // Nonclosing anchors retain ordinary proximity and nested collector discovery.
                    i = start;
                    continue;
                }
                counted_to = end;
                counted_lines = last;
                if (first..=last).contains(&target) {
                    if closing_anchor_collector(name) {
                        // A trailing style wrapper must not hide the rest of a collected body.
                        if let Some(enclosing) =
                            super::collected_lines(source, line, "frame", ignored)
                                .or_else(|| {
                                    super::collected_lines(source, line, "tabularx", ignored)
                                })
                                .or_else(|| math_lines_with_spans(source, line, known_spans))
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
    // `\csname` and `\expandafter` targets are dynamic: no static macro is named.
    let named = |name: &str| !name.is_empty() && !matches!(name, "csname" | "expandafter");
    if let Some((grp, next)) = group(source, at) {
        let target = &source[grp];
        let start = super::skip_source_space(target, 0);
        if target.as_bytes().get(start) != Some(&b'\\') {
            return None;
        }
        let (name, end) = command(target, start);
        (named(name) && super::skip_source_space(target, end) == target.len())
            .then_some((name, next))
    } else if source.as_bytes().get(at) == Some(&b'\\') {
        let (name, next) = command(source, at);
        named(name).then_some((name, next))
    } else {
        None
    }
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
            | "pgfmathsetmacro"
            | "pgfmathtruncatemacro"
    )
}

pub(super) type MacroTable<'a> = BTreeMap<&'a str, Option<MacroDef<'a>>>;

pub(super) fn macros_from_sources_with_mode<'a>(
    sources: &[&'a str],
    guess: bool,
    known_spans: &[(&'a str, &[crate::tex::Span])],
    operation: &crate::Operation,
) -> MacroTable<'a> {
    let (mut table, bodies) =
        scan_macros(sources, guess, known_spans, operation, &Default::default());
    // A wrapper may call a note macro defined after it (`\outer` -> `\inner`
    // -> `\note`): propagate over every replacement text, then rescan.
    let mut callers: std::collections::BTreeSet<&'a str> = table
        .iter()
        .filter(|(_, def)| def.as_ref().is_some_and(|def| def.calls_note))
        .map(|(name, _)| *name)
        .collect();
    loop {
        if operation.check().is_err() {
            return table;
        }
        let before = callers.len();
        for &(name, body, _) in &bodies {
            if !callers.contains(name) && body_calls_note(body, &table, &callers) {
                callers.insert(name);
            }
        }
        if callers.len() == before {
            break;
        }
    }
    if bodies
        .iter()
        .any(|&(name, _, had_note)| !had_note && callers.contains(name))
    {
        table = scan_macros(sources, guess, known_spans, operation, &callers).0;
    }
    // Parameters a later-defined wrapper discards are not printed either.
    while table
        .values()
        .flatten()
        .any(|def| (0..def.arity).any(|idx| def.used_params & (1 << idx) == 0))
    {
        let mut changed = false;
        for &(name, body, _) in &bodies {
            if operation.check().is_err() {
                return table;
            }
            let Some(Some(def)) = table.get(name).cloned().filter(|_| body.contains('\\')) else {
                continue;
            };
            if def.used_params == 0 {
                continue;
            }
            let used = printed_parameters(body, def.arity, &table, operation).0 & def.used_params;
            if used != def.used_params {
                table.insert(
                    name,
                    Some(MacroDef {
                        used_params: used,
                        ..def
                    }),
                );
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    table
}

/// Record a (re)definition. Conflicting meanings stay unknown, except that a
/// note-calling meaning keeps the widest signature with no argument printed.
fn record_definition<'a>(table: &mut MacroTable<'a>, name: &'a str, def: Option<MacroDef<'a>>) {
    let merged = match (table.get(name).cloned(), def) {
        (None, def) => def,
        (Some(Some(existing)), Some(def)) if existing == def && def.body.is_some() => return,
        (Some(old), new) => {
            let signature = |def: &Option<MacroDef<'_>>| {
                def.as_ref()
                    .map_or((0, 0), |def| (def.arity, def.opt_params))
            };
            let (arity, opt_params) = signature(&old).max(signature(&new));
            let calls_note = [old, new].into_iter().flatten().any(|def| def.calls_note);
            calls_note.then_some(MacroDef {
                arity,
                used_params: 0,
                opt_params,
                body: None,
                calls_note,
            })
        }
    };
    table.insert(name, merged);
}

/// Scan standard LaTeX and xparse definitions, returning the table plus every
/// `(name, replacement text, calls_note)` for forward note propagation.
#[allow(clippy::type_complexity)]
fn scan_macros<'a>(
    sources: &[&'a str],
    guess: bool,
    known_spans: &[(&'a str, &[crate::tex::Span])],
    operation: &crate::Operation,
    forward: &std::collections::BTreeSet<&str>,
) -> (MacroTable<'a>, Vec<(&'a str, &'a str, bool)>) {
    let mut table = MacroTable::new();
    let mut bodies = Vec::new();
    for &source in sources {
        let known = known_spans
            .iter()
            .find_map(|(s, spans)| std::ptr::eq(*s, source).then_some(*spans));
        let mut masked: std::borrow::Cow<'_, [crate::tex::Span]> = match known {
            Some(spans) => spans.into(),
            None => super::nonprinting_source_spans(source)
                .collect::<Vec<_>>()
                .into(),
        };
        let mut next_mask = 0usize;
        let current_nonprinting =
            |masked: &[crate::tex::Span], next_mask: &mut usize, limit: usize| {
                while masked
                    .get(*next_mask)
                    .is_some_and(|(_, span, _)| span.end <= limit)
                {
                    *next_mask += 1;
                }
                masked.get(*next_mask).cloned()
            };
        let mut in_preamble = true;
        let mut brace_depth = 0usize;
        let mut group_depth = 0usize;
        let mut conditionals = super::ConditionalStack::default();
        let mut at = 0usize;
        while at < source.len() {
            if operation.check().is_err() {
                return (table, bodies);
            }
            if conditionals.skipping() {
                let ifthenelse_opens = !guess || table.contains_key("ifthenelse");
                at = conditionals.skip_token(source, at, ifthenelse_opens);
                if !conditionals.skipping() {
                    super::resume_nonprinting_spans(source, at, &mut masked, &mut next_mask);
                }
                continue;
            }
            match source.as_bytes()[at] {
                b'%' => {
                    at = super::comment_end(source, at);
                }
                b'{' | b'}' => {
                    let reachable = !current_nonprinting(&masked, &mut next_mask, at)
                        .is_some_and(|(_, span, _)| span.contains(&at));
                    if reachable {
                        if source.as_bytes()[at] == b'{' {
                            brace_depth += 1;
                        } else {
                            brace_depth = brace_depth.saturating_sub(1);
                        }
                    }
                    at += 1;
                }
                b'$' => {
                    if brace_depth == 0
                        && group_depth == 0
                        && conditionals.is_empty()
                        && !current_nonprinting(&masked, &mut next_mask, at)
                            .is_some_and(|(_, span, _)| span.contains(&at))
                    {
                        in_preamble = false;
                    }
                    at += 1;
                }
                b'\\' => {
                    let start = at;
                    let current_span = current_nonprinting(&masked, &mut next_mask, start);
                    let (cmd, end) = command(source, at);
                    at = end;
                    let executing = !current_span
                        .as_ref()
                        .is_some_and(|(_, span, _)| span.contains(&start));
                    if executing
                        && !table.contains_key(cmd)
                        && let Some((opaque_end, _)) = opaque_command_end(source, cmd, at)
                    {
                        at = opaque_end;
                        continue;
                    }
                    // Strict mode keeps an unknown `\ifthenelse` as an unbalanced conditional.
                    if executing
                        && conditionals.apply(cmd, !guess || table.contains_key("ifthenelse"))
                    {
                        continue;
                    }
                    if executing && matches!(cmd, "(" | "[") {
                        if brace_depth == 0 && group_depth == 0 && conditionals.is_empty() {
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
                    if executing && cmd == "begin" {
                        if let Some((env, next)) = group(source, at) {
                            if &source[env] == "document"
                                || (brace_depth == 0 && group_depth == 0 && conditionals.is_empty())
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
                    let reachable = executing
                        || current_span
                            .as_ref()
                            .is_some_and(|(head, _, _)| *head == start);
                    // A definition inside text TeX never runs keeps earlier meanings.
                    if !reachable || !is_macro_definition_command(cmd) {
                        continue;
                    }
                    // Untracked bindings only forget their target's meaning.
                    if matches!(
                        cmd,
                        "let" | "futurelet" | "pgfmathsetmacro" | "pgfmathtruncatemacro"
                    ) {
                        if let Some((name, next)) = macro_target_name(source, at) {
                            record_definition(&mut table, name, None);
                            at = next;
                        }
                        continue;
                    }
                    let top_level = in_preamble
                        && brace_depth == 0
                        && group_depth == 0
                        && conditionals.is_empty();
                    // Bodies expanded at definition time or by xparse are never
                    // expanded here; they keep only a signature.
                    let expandable = !matches!(cmd, "edef" | "xdef") && !cmd.contains("Document");
                    at += usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
                    let target = super::skip_definition_overlay(source, at);
                    let Some((name, mut next)) = macro_target_name(source, target) else {
                        continue;
                    };
                    let signature = if matches!(cmd, "def" | "gdef" | "edef" | "xdef") {
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
                        if arity.is_none() {
                            // Delimited parameters: the replacement text still follows.
                            next = super::macro_definition_body_start(source, cmd, next);
                        }
                        arity.map(|arity| (arity, 0u16))
                    } else if cmd == "DeclareMathOperator" {
                        Some((0u8, 0u16))
                    } else if cmd.contains("DocumentCommand") {
                        let Some((spec, end)) = group(source, next) else {
                            record_definition(&mut table, name, None);
                            continue;
                        };
                        next = end;
                        spec_signature(&source[spec])
                    } else {
                        let mut signature = (cmd != "providecommand").then_some((0u8, 0u16));
                        if let Some(opt_end) = super::optional_argument_end(source, next) {
                            let opt = source
                                [super::skip_source_space(source, next) + 1..opt_end - 1]
                                .trim();
                            signature = signature
                                .filter(|_| opt.len() == 1 && opt.as_bytes()[0].is_ascii_digit())
                                .map(|_| (opt.as_bytes()[0] - b'0', 0));
                            next = opt_end;
                            while let Some(extra_end) = super::optional_argument_end(source, next) {
                                // `[arity][default]`: the first parameter is optional.
                                signature = signature
                                    .filter(|&(arity, opt_params)| arity > 0 && opt_params == 0)
                                    .map(|(arity, _)| (arity, 1));
                                next = extra_end;
                            }
                        }
                        signature
                    };
                    at = next;
                    let Some((body_range, body_end)) = group(source, at) else {
                        record_definition(&mut table, name, None);
                        continue;
                    };
                    let body = &source[body_range];
                    // The replacement text is stored, not run.
                    at = body_end;
                    let calls_note = body_calls_note(body, &table, forward);
                    bodies.push((name, body, calls_note));
                    let expanded = top_level && expandable;
                    let Some((arity, opt_params)) = signature.filter(|_| expanded || calls_note)
                    else {
                        let def = calls_note.then_some(MacroDef {
                            arity: 0,
                            used_params: 0,
                            opt_params: 0,
                            body: None,
                            calls_note,
                        });
                        record_definition(&mut table, name, def);
                        continue;
                    };
                    let (used_params, valid) = printed_parameters(body, arity, &table, operation);
                    // A call may omit an optional argument: never expand it inline.
                    let inline = expanded && opt_params == 0 && cmd != "providecommand";
                    let def = MacroDef {
                        arity,
                        used_params,
                        opt_params,
                        body: (inline && valid && !body.contains('$') && !body.contains("^^"))
                            .then_some(body),
                        calls_note,
                    };
                    record_definition(&mut table, name, Some(def));
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
    (table, bodies)
}

/// Whether `body` invokes `\note`, directly or through a note wrapper recorded
/// so far or (in `forward`) defined later.
fn body_calls_note(
    body: &str,
    table: &MacroTable<'_>,
    forward: &std::collections::BTreeSet<&str>,
) -> bool {
    super::control_words(body, 0..body.len()).any(|(_, name, _)| {
        forward.contains(name)
            || match table.get(name) {
                None => name == "note",
                Some(def) => def.as_ref().is_some_and(|def| def.calls_note),
            }
    })
}

/// `(arity, opt_params)` of an xparse signature made only of mandatory
/// `m`/`+m` and optional `o`/`+o`/`O{default}`/`+O{default}` arguments: bit
/// `i` of `opt_params` marks parameter `#i+1` as a bracketed optional argument.
pub(crate) fn spec_signature(spec: &str) -> Option<(u8, u16)> {
    let bytes = spec.as_bytes();
    let (mut arity, mut opt_params) = (0u8, 0u16);
    let mut at = 0;
    loop {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if at >= bytes.len() {
            return Some((arity, opt_params));
        }
        if arity == 9 {
            return None;
        }
        at += usize::from(bytes[at] == b'+');
        match bytes.get(at)? {
            b'm' => at += 1,
            b'o' => {
                opt_params |= 1 << arity;
                at += 1;
            }
            b'O' => {
                let (_, end) = group(spec, at + 1)?;
                opt_params |= 1 << arity;
                at = end;
            }
            _ => return None,
        }
        arity += 1;
    }
}

/// Parameters `#1..#arity` printed outside arguments the replacement text
/// discards, and whether each appears at most once with none out of range.
pub(crate) fn printed_parameters(
    body: &str,
    arity: u8,
    table: &MacroTable<'_>,
    operation: &crate::Operation,
) -> (u16, bool) {
    let hidden =
        crate::tex::discarded_macro_args(body, true, 0..body.len(), table, None, None, operation)
            .map(|(hidden, _)| hidden)
            .unwrap_or_default();
    let mut valid = true;
    let mut seen_params = 0u16;
    let mut used_params = 0u16;
    let mut line_offset = 0usize;
    for line in body.split_inclusive('\n') {
        let visible = crate::tex::source_line_text(line);
        for (rel_idx, _) in visible.match_indices('#') {
            if crate::tex::has_unescaped_backslash_before(visible, rel_idx) {
                continue;
            }
            let idx = line_offset + rel_idx;
            if let Some(d) = visible
                .as_bytes()
                .get(rel_idx + 1)
                .filter(|b| (b'1'..=b'0' + arity).contains(b))
                .map(|b| b - b'1')
            {
                if hidden.iter().any(|range| range.contains(&idx)) {
                    continue;
                }
                used_params |= 1 << d;
                // `#d` after `#d` or a later parameter would print argument text twice or out
                // of source order; `MacroDef.body` inlines only when atoms keep source order.
                if (seen_params >> d) != 0 {
                    valid = false;
                }
                seen_params |= 1 << d;
            } else {
                valid = false;
            }
        }
        line_offset += line.len();
    }
    (used_params, valid)
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
                // `^^` is a TeX character escape (`^^61` or `^^A`), not a script.
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
                    let base = if environment == "tabular*" {
                        environment
                    } else {
                        environment.trim_end_matches('*')
                    };
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
                let macro_entry = macros.get(name);
                if let Some(macro_entry) = macro_entry {
                    if let Some(def) = macro_entry
                        && def.opt_params == 0
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
                    let negated = if let Some(c) = source[next..range.end]
                        .chars()
                        .next()
                        .filter(|c| matches!(c, '=' | '<' | '>' | '|'))
                    {
                        negate_char(c).map(|negated| (negated, next + 1))
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
                    kind @ (MathCommand::Sqrt | MathCommand::ExtensibleArrow(_)) => {
                        // A root index or arrow label prints apart, between gap atoms.
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
                        if let MathCommand::ExtensibleArrow(arrow) = kind {
                            append(&mut result, arrow, span.clone());
                            append(&mut result, '\0', span);
                        } else {
                            append(&mut result, '√', span);
                        }
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
///
/// Composites reduce on a stack of adjacent atoms, so a composite closes over an earlier
/// one (`⎯`, `⎯`, `⎯`, `⇢` is one `⇢`) in a single in-place pass.
fn collapse_pdf_composites(atoms: &mut Vec<Atom>) {
    let triple = |a, b, c| match (a, b, c) {
        ('⍿', '-', '→') => Some('⟼'),
        ('⍿', '=', '⇒') => Some('⟾'),
        ('←', '-', '⎮') => Some('⟻'),
        ('⇐', '=', '⎮') => Some('⟽'),
        _ => None,
    };
    let pair = |a, b| match (a, b) {
        ('\u{20d2}', c) | (c, '\u{0338}') => negate_char(c),
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
    let adjacent = |a: &Atom, b: &Atom| a.span.end == b.span.start;
    // `atoms[..len]` is the reduced prefix; atoms between it and `read` are spent.
    let mut len = 0;
    for read in 0..atoms.len() {
        atoms.swap(len, read);
        len += 1;
        loop {
            let top = &atoms[..len];
            let reduced = match top {
                [.., a, b, c] if adjacent(a, b) && adjacent(b, c) => {
                    triple(a.value, b.value, c.value).map(|value| (3, value))
                }
                _ => None,
            }
            .or_else(|| match top {
                [.., a, b] if adjacent(a, b) => pair(a.value, b.value).map(|value| (2, value)),
                _ => None,
            });
            let Some((width, value)) = reduced else {
                break;
            };
            let end = atoms[len - 1].span.end;
            len -= width - 1;
            let merged = &mut atoms[len - 1];
            merged.value = value;
            merged.span.end = end;
            merged.word_join = false;
        }
    }
    atoms.truncate(len);
}

/// Atoms of the PDF `context`, the atom under `offset`, and the atoms of the PDF word
/// around it (only the selected atom unless it is alphanumeric).
type PdfWord = (Vec<Atom>, usize, Range<usize>);

fn pdf_context_word(context: &str, offset: usize) -> Option<PdfWord> {
    let mut pdf = Vec::new();
    for (i, c) in context.char_indices() {
        let join = pdf
            .last()
            .is_some_and(|prev: &Atom| prev.value.is_alphanumeric() && prev.span.end == i);
        append_joined(&mut pdf, c, i..i + c.len_utf8(), join);
    }
    collapse_pdf_composites(&mut pdf);
    let selected = pdf.iter().position(|atom| atom.span.contains(&offset))?;
    let mut word = selected..selected + 1;
    if pdf[selected].value.is_alphanumeric() {
        while word.start > 0 && pdf[word.start].word_join {
            word.start -= 1;
        }
        while word.end < pdf.len() && pdf[word.end].word_join {
            word.end += 1;
        }
    }
    Some((pdf, selected, word))
}

/// Gap and private-use glyphs carry no identity a source atom could match.
fn unmatchable_pdf_atom(value: char) -> bool {
    matches!(value, '\0'..='\x03') || private_use(value)
}

/// Start of the source `atoms` spelling the PDF word when `atoms[index]` stands for its
/// selected atom; `None` when they differ or sit inside a longer source word.
fn source_word_start(
    atoms: &[Atom],
    index: usize,
    (pdf, selected, word): &PdfWord,
) -> Option<usize> {
    let begin = index.checked_sub(selected - word.start)?;
    let candidate = atoms.get(begin..begin + word.len())?;
    if !candidate
        .iter()
        .zip(&pdf[word.clone()])
        .all(|(a, b)| a.value == b.value)
    {
        return None;
    }
    // A PDF word cannot be a substring of a longer source word.
    let inside_longer_word = pdf[*selected].value.is_alphanumeric()
        && (atoms[begin].word_join
            || atoms
                .get(begin + word.len())
                .is_some_and(|next| next.word_join));
    (!inside_longer_word).then_some(begin)
}

/// Whether a formula in `text` (note-owned TeX) renders the clicked PDF word.
/// Rendered math (`α`) never equals its source spelling (`\alpha`), so
/// note-owned formulas are compared as atoms rather than as source words.
pub(super) fn text_math_renders_word(
    text: &str,
    macros: &MacroTable<'_>,
    operation: &crate::Operation,
    (context, offset): (&str, usize),
) -> bool {
    let Some(pdf_word) = pdf_context_word(context, offset) else {
        return false;
    };
    let value = pdf_word.0[pdf_word.1].value;
    if unmatchable_pdf_atom(value) {
        return false;
    }
    let spans: Vec<_> = super::nonprinting_source_spans(text).collect();
    for range in regions_with_spans(text, &spans) {
        let mut unknown = false;
        let Some(atoms) =
            atoms_with_macros(text, range, (macros, operation), 0, None, &[], &mut unknown)
        else {
            // Unreadable note math cannot be ruled out.
            return true;
        };
        if unknown {
            // Neither can note math through an unexpanded command.
            return true;
        }
        if (0..atoms.len()).any(|index| {
            atoms[index].value == value && source_word_start(&atoms, index, &pdf_word).is_some()
        }) {
            return true;
        }
    }
    false
}

/// Whether `body` invokes `\note`, directly or through a recorded wrapper.
pub(crate) fn calls_note(body: &str, table: &MacroTable<'_>) -> bool {
    body_calls_note(body, table, &std::collections::BTreeSet::new())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MathOutcome {
    Located(u32, usize),
    Absent,
    Opaque,
}

pub(super) fn source_location_with_context_mode(
    analysis: &crate::tex::SourceAnalysis<'_>,
    scope: &crate::tex::WordRangeScope<'_>,
    within_frame: bool,
    (context, offset): (&str, usize),
    prose: crate::tex::ProseRank,
    regions: &[Range<usize>],
    mode: crate::tex::WordLocationMode,
) -> MathOutcome {
    let source = analysis.source;
    let starts = &analysis.line_starts;
    let operation = analysis.operation;
    let line = scope.line;
    let lines = &scope.lines;
    let foreach_loops = scope.foreach_loops;
    let execution_scope = scope.execution_scope;
    if operation.check().is_err() {
        return MathOutcome::Opaque;
    }
    let (prose, tied_score) = match prose {
        crate::tex::ProseRank::Absent => (None, None),
        crate::tex::ProseRank::Unique(row, byte, score) => (Some((row, byte, score)), None),
        crate::tex::ProseRank::Tied(score) => (None, Some(score)),
        crate::tex::ProseRank::Opaque => return MathOutcome::Opaque,
    };
    let prose = prose.filter(|(row, byte, _)| {
        !crate::tex::contains_byte(regions, starts[*row as usize - 1] + byte)
    });
    // A letter inside an ordinary PDF word must match that entire word, not a
    // nearby single-letter variable with the same spelling.
    let Some(pdf_word) = pdf_context_word(context, offset) else {
        return prose.map_or(MathOutcome::Absent, |(row, byte, _)| {
            MathOutcome::Located(row, byte)
        });
    };
    let (pdf, selected, word) = (&pdf_word.0, pdf_word.1, &pdf_word.2);
    if unmatchable_pdf_atom(pdf[selected].value) {
        return MathOutcome::Absent;
    }
    let scope_start = execution_scope.map_or_else(
        || *starts.get(lines.start).unwrap_or(&source.len()),
        |scope| scope.start,
    );
    let scope_end = execution_scope.map_or_else(
        || *starts.get(lines.end).unwrap_or(&source.len()),
        |scope| scope.end,
    );
    let start_idx = regions.partition_point(|range| range.end <= scope_start);
    let end_idx = regions.partition_point(|range| range.start < scope_end);
    let active_regions = &regions[start_idx..end_idx.max(start_idx)];
    if active_regions.is_empty() {
        return prose.map_or(MathOutcome::Absent, |(row, byte, _)| {
            MathOutcome::Located(row, byte)
        });
    }
    let macros = analysis.macros(mode);
    let mut best = None;
    let mut tied = false;
    let mut any_unknown = false;
    let mut line_unknown = false;
    let line_start = *starts.get(line as usize - 1).unwrap_or(&source.len());
    let line_end = *starts.get(line as usize).unwrap_or(&source.len());
    // Loop bindings shadow nothing, so one table copy serves every pass: each binding is
    // inserted for its pass and removed afterwards.
    let mut local_macros = None;
    for range in active_regions {
        if operation.check().is_err() {
            return MathOutcome::Opaque;
        }
        if execution_scope.is_some() && (range.start < scope_start || range.end > scope_end) {
            // A compiler-proven body never borrows a formula crossing its header boundary.
            return MathOutcome::Opaque;
        }
        let Some(bindings) = crate::tex::foreach_bindings_for_range(foreach_loops, range) else {
            return MathOutcome::Absent;
        };
        let mut region_unknown = false;
        let mut passes = Vec::with_capacity(bindings.len().max(1));
        if bindings.is_empty() {
            let Some(atoms) = atoms_with_macros(
                source,
                range.clone(),
                (macros, operation),
                0,
                None,
                &[],
                &mut region_unknown,
            ) else {
                return MathOutcome::Absent;
            };
            passes.push(atoms);
        } else {
            let local_macros = local_macros.get_or_insert_with(|| macros.clone());
            for binding in &bindings {
                for &(var, val) in binding {
                    if macros.contains_key(var) {
                        return MathOutcome::Absent;
                    }
                    local_macros.insert(
                        var,
                        Some(MacroDef {
                            arity: 0,
                            body: Some(val),
                            used_params: 0,
                            opt_params: 0,
                            calls_note: false,
                        }),
                    );
                }
                let mut pass_unknown = false;
                let atoms = atoms_with_macros(
                    source,
                    range.clone(),
                    (local_macros, operation),
                    0,
                    None,
                    &[],
                    &mut pass_unknown,
                );
                for &(var, _) in binding {
                    local_macros.remove(var);
                }
                let Some(atoms) = atoms else {
                    return MathOutcome::Absent;
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
            let mut atoms = inline_context(source, range, atoms);
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
                let Some(begin) = source_word_start(&atoms, index, &pdf_word) else {
                    continue;
                };
                // Stop at the first mismatch and at this expression's boundary.
                // Context from a different formula must not break an ambiguity tie.
                let joined = |slice: &[Atom], prev: usize, direction: isize| {
                    if direction < 0 {
                        slice[prev].word_join
                    } else {
                        slice.get(prev + 1).is_some_and(|next| next.word_join)
                    }
                };
                let score_with = |segments: &[&[Atom]], begin: usize| {
                    let get_atom = |mut idx: usize| -> Option<&Atom> {
                        for seg in segments {
                            if let Some(atom) = seg.get(idx) {
                                return Some(atom);
                            }
                            idx -= seg.len();
                        }
                        None
                    };
                    let joined_src = |prev: usize, direction: isize| {
                        if direction < 0 {
                            get_atom(prev).is_some_and(|atom| atom.word_join)
                        } else {
                            get_atom(prev + 1).is_some_and(|next| next.word_join)
                        }
                    };
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
                                let join_a = joined_src(prev_src, direction);
                                let join_b = joined(pdf, prev_pdf, direction);
                                if !join_a && !join_b {
                                    committed = (score, lexical_score);
                                    word_start = distance;
                                }
                                join_a
                                    && (join_b
                                        || get_atom(prev_src)
                                            .is_some_and(|a| range.contains(&a.span.start)))
                            } else {
                                false
                            };
                            match (
                                source_edge.checked_add_signed(delta).and_then(get_atom),
                                pdf_edge.checked_add_signed(delta).and_then(|i| pdf.get(i)),
                            ) {
                                (Some(a), Some(b))
                                    if !matches!(a.value, '\0'..='\x03')
                                        && a.value == b.value
                                        && (!in_word
                                            || joined(
                                                pdf,
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
                let (mut score, mut lexical_score, mut contradictions) =
                    score_with(&[&atoms], begin);
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
                        for (segments, candidate_begin) in [
                            ([before, op, s1, s2, after], before.len()),
                            ([before, op, s2, s1, after], before.len()),
                            ([before, s1, op, s2, after], before.len() + s1.len()),
                            ([before, s2, op, s1, after], before.len() + s2.len()),
                        ] {
                            let (s, l, c) = score_with(&segments, candidate_begin);
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
        return prose.map_or(MathOutcome::Absent, |(row, byte, _)| {
            MathOutcome::Located(row, byte)
        });
    }
    if prose.is_some() && best.is_some() {
        return MathOutcome::Absent;
    }
    // Tied prose competes with a formula just as unique prose does above, unless
    // no tied occurrence has evidence and an uncontradicted formula has lexical evidence.
    let math_beats_tie = tied_score.is_none_or(|score| {
        score == 0
            && best.is_some_and(|((score, has_lexical, contradictions, _, _), _)| {
                score > 0 && has_lexical && contradictions.0 == 0
            })
    });
    prose
        .map(|(row, byte, _)| (row, byte))
        .or(mathematical.filter(|_| math_beats_tie))
        .map_or(MathOutcome::Absent, |(row, byte)| {
            MathOutcome::Located(row, byte)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{collected_text_argument_at, collected_text_lines};
    use crate::tex::{ProseRank, SourceAnalysis, WordLocationMode, WordRangeScope};

    /// Strict math refinement of the PDF word at `offset` within `lines`, given its unique
    /// prose `(row, byte, score)` candidate.
    fn source_location(
        source: &str,
        line: u32,
        lines: Range<usize>,
        within_frame: bool,
        context: &str,
        offset: usize,
        prose: Option<(u32, usize, isize)>,
    ) -> Option<(u32, usize)> {
        let operation = crate::Operation::default();
        let analysis = SourceAnalysis::new(source, &[], &operation);
        let end = |row: usize| *analysis.line_starts.get(row).unwrap_or(&source.len());
        let limit = prose.map_or(end(lines.end), |(row, ..)| {
            end(lines.end).max(end(row as usize))
        });
        let scope = WordRangeScope {
            line,
            lines,
            foreach_loops: &[],
            execution_scope: None,
        };
        let prose = prose.map_or(ProseRank::Absent, |(row, byte, score)| {
            ProseRank::Unique(row, byte, score)
        });
        let all_math = analysis.math_regions();
        let regions = &all_math[..all_math.partition_point(|r| r.start < limit)];
        match source_location_with_context_mode(
            &analysis,
            &scope,
            within_frame,
            (context, offset),
            prose,
            regions,
            WordLocationMode::Strict,
        ) {
            MathOutcome::Located(row, byte) => Some((row, byte)),
            MathOutcome::Absent | MathOutcome::Opaque => None,
        }
    }

    #[test]
    fn pdf_composites_collapse_chained_pieces_in_one_pass() {
        let collapse = |text: &str| {
            let mut atoms = Vec::new();
            for (i, c) in text.char_indices() {
                append(&mut atoms, c, i..i + c.len_utf8());
            }
            collapse_pdf_composites(&mut atoms);
            atoms
                .iter()
                .map(|atom| (atom.value, atom.span.clone()))
                .collect::<Vec<_>>()
        };
        let dashed = "x ⎯⎯⎯⇢ y";
        let shaft = dashed.find('⎯').unwrap();
        assert_eq!(
            collapse(dashed),
            [
                ('x', 0..1),
                ('⇢', shaft..dashed.find(" y").unwrap()),
                ('y', dashed.len() - 1..dashed.len())
            ]
        );
        let back = "⇠⎯⎯⎯";
        assert_eq!(collapse(back), [('⇠', 0..back.len())]);
        // Separated pieces stay apart; a negation overlay still folds into its relation.
        assert_eq!(collapse("⎯ ⇢"), [('⎯', 0..3), ('⇢', 4..7)]);
        assert_eq!(collapse("=\u{0338}<"), [('≠', 0..3), ('<', 3..4)]);
        assert_eq!(collapse("⍿-→"), [('⟼', 0.."⍿-→".len())]);
    }

    /// [`source_location`] without a prose candidate.
    fn loc(
        source: &str,
        line: u32,
        lines: Range<usize>,
        within: bool,
        context: &str,
        offset: usize,
    ) -> Option<(u32, usize)> {
        source_location(source, line, lines, within, context, offset, None)
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
                loc(source, 2, 0..4, false, word, 0),
                Some((2, source.lines().nth(1).unwrap().find(word).unwrap()))
            );
        }
        for word in ["ignored", "hidden", "unterminated"] {
            assert_eq!(loc(source, 2, 0..4, false, word, 0), None);
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
            assert_eq!(loc(source, 1, 0..1, false, glyph, 0), None);
        }
        let source = r"\begin{alignat}{2}\alpha&=\beta\end{alignat}";
        assert_eq!(
            loc(source, 1, 0..1, false, "α=β", 0),
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
                loc(
                    source,
                    1,
                    0..1,
                    false,
                    context,
                    context.find(selected).unwrap()
                ),
                Some((1, source.find(needle).unwrap()))
            );
        }
        let matrix = "\\begin{align*}\\dot g(u)&=\\begin{pmatrix}\\cos\\beta_1(u)\\\\\\sin\\beta_2(u)\\end{pmatrix}\\end{align*}";
        let context = "g(u)=cosβ1(u) sinβ2(u)";
        assert_eq!(
            loc(
                matrix,
                1,
                0..1,
                false,
                context,
                context.find("sin").unwrap()
            ),
            Some((1, matrix.find("\\sin").unwrap()))
        );
    }

    #[test]
    fn unknown_commands_insert_gap_atoms_without_poisoning_known_tokens() {
        let source = "$x\\longmapsto y+\\custom{z}$\n$x\\longmapsto y$";
        assert_eq!(loc(source, 2, 0..2, true, "x⟼y", 1), None);
        let source = "$x\\longmapsto\\custom y$\n$x\\longmapsto h$";
        assert_eq!(loc(source, 2, 0..2, true, "x⟼y", 1), Some((1, 2)));
        assert_eq!(loc(source, 2, 0..2, true, "x⟼h", 1), Some((2, 2)));
        let source = "$\\alpha\\custom{z}$\n$\\beta$";
        assert_eq!(loc(source, 2, 0..2, true, "β", 0), None);
        let source = "$\\alpha\\custom{z}$\n$\\beta+1$";
        assert_eq!(loc(source, 2, 0..2, true, "β+1", 0), Some((2, 1)));
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
                loc(bad_source, 1, 0..line_count, true, context, 0),
                None,
                "{bad_source}"
            );
        }
        assert_eq!(loc("$x\\kern1pty$", 1, 0..1, false, "xy", 1), Some((1, 10)));
        assert_eq!(
            loc("$x\\kern1truept y$", 1, 0..1, false, "xy", 1),
            Some((1, 15))
        );
        assert_eq!(
            loc("$A_\\custom$\n$x$", 2, 0..2, false, "x", 0),
            Some((2, 1))
        );
        let shared_suffix = "\\emph{plover} $Q$:\n\\item a bramble kettle $Q_x = y$";
        assert_eq!(
            loc(
                shared_suffix,
                1,
                0..2,
                true,
                "plover Q: a bramble kettle",
                "plover ".len()
            ),
            Some((1, 15))
        );
    }

    #[test]
    fn duplicate_equations_unknown_commands_and_private_glyphs() {
        let source = "$\\alpha\\leq\\beta$\n$\\gamma\\geq\\delta$\n$\\alpha\\leq\\beta$";
        assert_eq!(loc(source, 3, 0..3, true, "α≤β γ≥δ α≤β", 0), None);
        assert_eq!(
            loc("$\\custom{x}+y$", 1, 0..1, false, "x+y", 0),
            Some((1, 9))
        );
        assert_eq!(loc("$\\mathcal H$", 1, 0..1, false, "\u{e234}", 0), None);
        assert_eq!(loc("$x$", 1, 0..1, false, "example", 1), None);
        assert_eq!(loc("$X+x$", 1, 0..1, false, "𝑋+𝑥", 0), Some((1, 1)));
    }

    #[test]
    fn unsupported_neighbors_and_unanchored_punctuation_remain_coarse() {
        for (source, context, offset) in [
            ("Hello.\n$x.y$", "Hello. x.y", 5),
            ("$\\identity{\\alpha}$\n$\\alpha$", "α α", 0),
            ("$\\alpha_a^b$\n$\\alpha_b^a$", "α\r\nba\r\nα\r\nab", 0),
        ] {
            assert_eq!(
                loc(source, 1, 0..2, false, context, offset),
                None,
                "{source}"
            );
        }
        assert_eq!(loc("$x.y$", 1, 0..1, false, "x.y", 1), Some((1, 2)));
    }

    #[test]
    fn repeated_inline_math_across_soft_wraps_preserves_occurrence_ownership() {
        let source = "\
  \\begin{enumerate}
    \\item when \\(v\\in\\mathcal F_m(c_0,c_1)\\) wobbles and
      \\(z_*\\) is its pebble kettle turnip, then \\(z_*\\) nudges \\(\\Lambda_m\\) near
      \\(K_r^{\\le m}\\);
    \\item likewise, when \\(z_*\\in K_r^{\\le m}\\) nudges
      \\(\\Lambda_m\\), then the
      barrow of each damp gravel plover
  \\end{enumerate}";
        let context = "likewise, when z* ∈ Kr≤m nudges Λm,";
        let offset = context.find("Λm").unwrap();
        assert_eq!(
            loc(source, 6, 1..8, false, context, offset),
            Some((
                6,
                source.lines().nth(5).unwrap().find("\\Lambda_m").unwrap()
            ))
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
        let wrappers = "text texttt textnormal textit textbf textrm textsf textup textsc textsl \
                        textmd mathrm mathit mathbf mathsf mathtt mathcal mathbb mathfrak mathscr \
                        mathnormal symrm symit symbf symsf symtt symbb symcal symscr symfrak symnormal";
        for wrapper in wrappers.split_whitespace() {
            let source = format!("$\\{wrapper}{{K}}+y$");
            assert_eq!(
                loc(&source, 1, 0..1, false, "K+y", 0),
                Some((1, source.find("{K}").unwrap() + 1)),
                "{wrapper}"
            );
        }
        // Each `(source, pdf, "needle tex …")` maps a PDF needle to its TeX token.
        for (source, pdf, pairs) in [
            (
                "$\\varkappa+\\varepsilon+\\varphi+\\vartheta+\\varrho+\\varpi+\\varsigma+\\varGamma+\\varDelta+\\varOmega$",
                "ϰ+ϵ+ϕ+ϑ+ϱ+ϖ+ς+Γ+∆+Ω",
                "ϰ \\varkappa ϵ \\varepsilon ϕ \\varphi ϑ \\vartheta ϱ \\varrho ϖ \\varpi ς \\varsigma \
                 Γ \\varGamma ∆ \\varDelta Ω \\varOmega",
            ),
            (
                "$a\\le b\\ge c\\ni d\\owns e\\bigotimes f\\frown g\\smallsetminus h\\neg p\\not\\in S\\not=0\\dots\\ast\\|v\\|\\mid w$",
                "a≤b≥c∋d∋e⨂f⌢g∖h¬p∉S≠0⋯*‖v‖|w",
                "≤ \\le ≥ \\ge ⨂ \\bigotimes ⌢ \\frown ∖ \\smallsetminus ¬ \\neg ∉ \\not\\in \
                 ≠ \\not= ⋯ \\dots * \\ast |w \\mid",
            ),
            (
                "$x\\sin\\theta + \\Pr(A) + \\gcd(m,n)$",
                "x sin θ + Pr(A) + gcd(m,n)",
                "x x sin \\sin θ \\theta Pr \\Pr gcd \\gcd",
            ),
        ] {
            let pairs: Vec<&str> = pairs.split_whitespace().collect();
            for pair in pairs.chunks(2) {
                let (needle, tex) = (pair[0], pair[1]);
                let found = loc(source, 1, 0..1, false, pdf, pdf.find(needle).unwrap());
                assert_eq!(found, Some((1, source.find(tex).unwrap())), "{tex}");
            }
        }
        assert_eq!(loc("$\\sin x$", 1, 0..1, false, "s", 0), None);

        let liminf_frame = "$\\liminf x$\n$\\lim x$";
        assert_eq!(
            loc(liminf_frame, 1, 0..2, true, "lim inf x", 0),
            Some((1, 1))
        );
        assert_eq!(
            loc(liminf_frame, 1, 0..2, true, "lim inf x", 4),
            Some((1, 1))
        );

        let redefined_not = "\\renewcommand{\\in}{=}\n$a\\not\\in b$\n$a\\not=b$";
        assert_eq!(loc(redefined_not, 2, 1..3, true, "a≠b", 0), None);
        assert_eq!(
            loc(
                "$a\\not<b\\not>c\\not\\leq d\\not\\geq e$",
                1,
                0..1,
                false,
                "a≮b≯c≰d≱e",
                "a≮b≯c".len()
            ),
            Some((1, 14))
        );
        let not_ll = "Frame prose\n$x\\not\\ll y + z$\n$\\hspace{0.5\\textwidth}w$";
        assert_eq!(
            loc(not_ll, 2, 0..3, true, "x/≪y + z", "x/".len()),
            Some((2, 6))
        );
        assert_eq!(loc(not_ll, 3, 0..3, true, "w", 0), Some((3, 23)));
        assert_eq!(
            source_location(not_ll, 3, 0..3, true, "prose", 0, Some((1, 6, 5))),
            Some((1, 6))
        );
    }

    #[test]
    fn big_operators_with_simultaneous_limits_resolve_in_inline_and_display_orders() {
        let display =
            "\\[\n  A = \\bigoplus_{i=1}^n U_i\n\\]\n\\[\n  B = \\bigoplus_{k=1}^m V_k\n\\]";
        let visual = "A = ⊕n\r\ni=1 Ui B = ⊕m\r\nk=1 Vk";
        let stream = "A = n ⊕ i=1 Ui B = m ⊕ k=1 Vk";
        let inline = "Consider $\\bigoplus_{i=1}^n V_i$ and $\\bigoplus$.";
        let inline_ctx = "Consider ⊕n\r\ni=1 Vi and ⊕.";
        let nested = "\\[\n  A = \\bigoplus_{i=1}^{n_j^k} U_i\n\\]\n\\[\n  B = \\bigoplus_{i=1}^{m_j^k} V_i\n\\]";
        let refs = "\\[\n  \\bigoplus\\ref{a}x\\ref{b}y\\ref{c}z\n\\]\n\\[\n  \\bigoplus x y\n\\]";
        let isolated = "\\[\\bigoplus\\]\n\\ensuremath{\\oplus}";
        // `(source, line, lines, context, last ⊕, expected)`.
        for (source, line, lines, ctx, last, expected) in [
            (display, 2, 0..6, visual, false, Some((2, 6))),
            (display, 5, 0..6, visual, true, Some((5, 6))),
            (display, 2, 0..6, stream, false, Some((2, 6))),
            (display, 5, 0..6, stream, true, Some((5, 6))),
            (inline, 1, 0..1, inline_ctx, false, Some((1, 10))),
            (inline, 1, 0..1, inline_ctx, true, Some((1, 38))),
            (
                nested,
                5,
                0..6,
                "A = ⊕njk i=1 Ui B = ⊕mjk i=1 Vi",
                true,
                Some((5, 6)),
            ),
            (refs, 5, 0..6, "⊕1x2y3z ⊕xy", true, Some((5, 2))),
            (isolated, 2, 0..2, "⊕ ⊕", true, None),
        ] {
            let offset = if last {
                ctx.rfind('⊕')
            } else {
                ctx.find('⊕')
            }
            .unwrap();
            let within = source != inline;
            assert_eq!(
                loc(source, line, lines, within, ctx, offset),
                expected,
                "{source}"
            );
        }

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
                loc(composites, 5, 0..6, true, composites_ctx, offset),
                Some((5, col)),
                "failed for {needle}"
            );
        }

        let non_collapsing = "\\[\n  f \\mapsto \\rightarrow g \\quad \\rightharpoonup\\leftharpoondown \\quad a = \\not= b \\quad x \\notin \\notin y\n\\]";
        let ctx = "f\u{237f}\u{2192}\u{2192}g \u{21c0}\u{21bd} a=\u{20d2}=b x\u{2208}\u{0338}\u{2208}\u{0338}y";
        for (offset, col) in [
            (ctx.rfind('\u{2192}'), 12),
            (ctx.find('\u{21bd}'), 47),
            (ctx.find('='), 72),
            (ctx.rfind('\u{2208}'), 97),
        ] {
            let found = loc(non_collapsing, 2, 0..3, true, ctx, offset.unwrap());
            assert_eq!(found, Some((2, col)), "{col}");
        }

        let stmary = "\\[\n  \\llbracket x \\rrbracket \\sslash y \\interleave z \\trianglelefteqslant w\n\\]\n\\[\n  \\llbracket a \\rrbracket \\sslash b \\interleave c \\trianglelefteqslant d\n\\]";
        let ctx = "⟦x⟧ ⫽ y ⫴ z ⊴ w ⟦a⟧ ⫽ b ⫴ c ⊴ d";
        let found = loc(stmary, 5, 0..6, true, ctx, ctx.rfind('⫴').unwrap());
        assert_eq!(found, Some((5, 36)));

        let variants = "\\[\n  \\vartheta + 7 + \\lgroup x \\rgroup + a = b + \\not\\rightarrow c + u \\coloneqq v + p \\bbslash q \\setminus r + m \\Mapsto n \\Longmapsto k\n\\]";
        let ctx = "\u{1D6DD} + \u{1D7DF} + ⟮x⦘ + a\u{0338}=b + \u{20d2}→ c + u ≔ v + p ⧵ q \\ r + m \u{237f}⇒ n \u{237f}=⇒ k";
        for (needle, col) in [
            ("\u{1D6DD}", 2),
            ("\u{1D7DF}", 14),
            ("⦘", 28),
            ("=", 40),
            ("→ c", 46),
            ("≔", 68),
            ("⧵", 84),
            ("\\ r", 95),
            ("\u{237f}⇒ n", 111),
            ("\u{237f}=⇒ k", 121),
        ] {
            let found = loc(variants, 2, 0..3, true, ctx, ctx.find(needle).unwrap());
            assert_eq!(found, Some((2, col)), "{needle}");
        }
        let chained = "$A \\dashrightarrow B \\dashleftarrow C$";
        let chained_ctx = "A \u{23af}\u{23af}\u{23af}\u{21e2} B \u{21e0}\u{23af}\u{23af}\u{23af} C";
        assert_eq!(
            loc(
                chained,
                1,
                0..1,
                true,
                chained_ctx,
                chained_ctx.find('\u{21e2}').unwrap()
            ),
            Some((1, 3))
        );
        assert_eq!(
            loc(
                chained,
                1,
                0..1,
                true,
                chained_ctx,
                chained_ctx.rfind('\u{23af}').unwrap()
            ),
            Some((1, 21))
        );
        for delim in ["lvert", "rvert", "lVert", "rVert", "bigparallel"] {
            assert_eq!(crate::symbols::negated_symbol(delim), None, "{delim}");
        }
        assert_eq!(crate::symbols::negated_symbol("mid"), Some('∤'));
        assert_eq!(crate::symbols::negated_symbol("parallel"), Some('∦'));
        let foreach_src = "\\newcommand{\\idx}{9}\\newcommand{\\pair}{\\alpha}\n\
                           $\\foreach \\idx/\\pair in {1/\\beta, 2/\\gamma}{x_{\\idx}\\pair} + x_{\\idx}\\pair$";
        let foreach_ctx = "x1βx2γ + x9α";
        assert_eq!(
            loc(
                foreach_src,
                2,
                0..2,
                true,
                foreach_ctx,
                foreach_ctx.find('9').unwrap(),
            ),
            Some((
                2,
                foreach_src.lines().nth(1).unwrap().rfind("\\idx").unwrap()
            ))
        );
        assert_eq!(
            loc(
                foreach_src,
                2,
                0..2,
                true,
                foreach_ctx,
                foreach_ctx.find('α').unwrap(),
            ),
            Some((
                2,
                foreach_src.lines().nth(1).unwrap().rfind("\\pair").unwrap()
            ))
        );
    }
}
