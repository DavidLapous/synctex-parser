use crate::math::{self, MacroTable};
use crate::validate_forward_source_position;
use std::ops::Range;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// TeX comments start at an unescaped percent sign.
pub(crate) fn source_line_text(text: &str) -> &str {
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

/// Opaque (verbatim/quoted) command ranges merged with `hidden`: the sorted,
/// disjoint ranges where an environment boundary cannot occur.
pub(crate) fn environment_ignored_ranges(
    source: &str,
    hidden: &[Range<usize>],
) -> Vec<Range<usize>> {
    let mut ignored = Vec::with_capacity(hidden.len());
    let mut scan = 0;
    let mut next_hidden = 0;
    while scan < source.len() {
        // Text TeX never executes (comment environments, stored macro bodies)
        // cannot open a `\verb` or other opaque token.
        while next_hidden < hidden.len() && hidden[next_hidden].end <= scan {
            next_hidden += 1;
        }
        if let Some(range) = hidden.get(next_hidden)
            && range.contains(&scan)
        {
            scan = range.end;
            continue;
        }
        match source.as_bytes()[scan] {
            b'%' => scan = comment_end(source, scan),
            b'\\' => {
                let start = scan;
                let (command, end) = math::command(source, scan);
                scan = end;
                if end > start + 1
                    && let Some((opaque_end, _)) = math::opaque_command_end(source, command, end)
                {
                    ignored.push(start..opaque_end);
                    scan = opaque_end;
                }
            }
            _ => {
                scan += source.as_bytes()[scan + 1..]
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'\\'))
                    .map_or(source.len() - scan, |pos| pos + 1);
            }
        }
    }
    ignored.extend_from_slice(hidden);
    merge_hidden_ranges(&mut ignored);
    ignored
}

/// Zero-based line range of the collected `environment` enclosing the zero-based `anchor`
/// line, given pre-merged `environment_ignored_ranges`.
///
/// Collected environments (Beamer `frame`, `tabularx`) attribute their body to
/// the closing line: forward search refines against the closing line, inverse
/// search against the enclosing range.
pub(crate) fn environment_bounds_with_ignored(
    source: &str,
    anchor: usize,
    environment: &str,
    ignored: &[Range<usize>],
) -> Option<Range<usize>> {
    if !source.contains(environment) {
        return None;
    }
    let mut ignored = ignored.iter().peekable();
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
            while ignored.peek().is_some_and(|range| range.end <= abs) {
                ignored.next();
            }
            if ignored.peek().is_some_and(|range| range.contains(&abs)) {
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
                        return Some(begin..index + 1);
                    }
                } else if environment == "frame" && anchor <= index {
                    return Some(index..index + 1);
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

/// Lines of the collected `environment` enclosing the one-based `line`.
pub(crate) fn collected_lines(
    source: &str,
    line: u32,
    environment: &str,
    ignored: &[Range<usize>],
) -> Option<Range<usize>> {
    environment_bounds_with_ignored(source, line.checked_sub(1)? as usize, environment, ignored)
}

/// Closing compiler line of the Beamer frame, else `tabularx`, enclosing the one-based `line`.
pub(crate) fn collected_environment_closing_line(
    source: &str,
    line: u32,
    ignored: &[Range<usize>],
) -> Option<u32> {
    collected_lines(source, line, "frame", ignored)
        .or_else(|| collected_lines(source, line, "tabularx", ignored))
        .map(|lines| lines.end as u32)
}

pub(crate) fn collected_text_closing_line_with_spans(
    source: &str,
    line: u32,
    column: u32,
    known_spans: &[Span],
) -> Option<u32> {
    let target = forward_cursor_byte(source, line, column)?;
    let body = math::collected_text_argument_at_with_spans(source, target, known_spans)?;
    closing_line(source, (line, target), body.end)
}

fn forward_cursor_byte(source: &str, line: u32, column: u32) -> Option<usize> {
    let row = validate_forward_source_position(source, line, column).ok()?;
    // End-of-line is a cursor boundary, not a character in the argument.
    let byte = row.char_indices().nth(column.checked_sub(1)? as usize)?.0;
    // `row` borrows `source`, so its offset is the line start.
    Some(row.as_ptr() as usize - source.as_ptr() as usize + byte)
}

/// One-based line of byte `end`, counted from the cursor byte `target` on one-based `line`.
fn closing_line(source: &str, (line, target): (u32, usize), end: usize) -> Option<u32> {
    let rows = source
        .get(target..end)?
        .bytes()
        .filter(|ch| *ch == b'\n')
        .count();
    line.checked_add(u32::try_from(rows).ok()?)
}

/// Find the compiler line of a macro argument or reference under a source position.
pub(crate) fn argument_closing_line_with_spans(
    source: &str,
    line: u32,
    column: u32,
    known_spans: &[Span],
) -> Option<u32> {
    collected_text_closing_line_with_spans(source, line, column, known_spans).max(
        reference_argument_closing_line_with_spans(source, line, column, known_spans),
    )
}

pub(crate) fn reference_argument_closing_line_with_spans(
    source: &str,
    line: u32,
    column: u32,
    known_spans: &[Span],
) -> Option<u32> {
    let target = forward_cursor_byte(source, line, column)?;
    let body = reference_argument_body_at_with_spans(source, target, known_spans)?;
    // A reference ships with the collected argument around it; its hidden label cannot say so.
    let end = math::collected_text_owner_at_with_spans(source, target, known_spans)
        .map_or(body.end, |(owner, _)| owner.end.max(body.end));
    closing_line(source, (line, target), end)
}

fn tikzpicture_exclusive_bounds(
    source: &str,
    line_starts: &LineIndex,
    anchor: usize,
    start: usize,
    end: usize,
) -> bool {
    // The empty row after a final newline holds no `\end{tikzpicture}`.
    if anchor > end || line_starts.get(end).is_none_or(|&row| row >= source.len()) {
        return false;
    }
    let row_bytes = |row: usize| line_starts[row]..line_starts.row_start(row + 1, source.len());
    for bytes in (anchor..start).map(row_bytes) {
        if source_line_text(&source[bytes]).trim().is_empty() {
            return false;
        }
    }
    let scan_start = line_starts[anchor.min(start)];
    let start_line_byte = line_starts[start];
    let end_line_byte = line_starts[end];
    let end_line_end = row_bytes(end).end;
    let Some(begin_rel) = source_line_text(&source[row_bytes(start)]).find("\\begin{tikzpicture}")
    else {
        return false;
    };
    let tikz_start = start_line_byte + begin_rel;
    let end_marker = "\\end{tikzpicture}";
    let Some(end_rel) = source_line_text(&source[end_line_byte..end_line_end]).find(end_marker)
    else {
        return false;
    };
    let tikz_end = end_line_byte + end_rel + end_marker.len();
    let mut at = scan_start;
    let mut saw_wrapper = false;
    let mut outer_end = tikz_end;
    while at < tikz_start {
        let next_at = skip_source_space(source, at);
        if next_at > at {
            at = next_at;
            continue;
        }
        match source.as_bytes()[at] {
            b'{' => {
                let Some((_, grp_end)) = math::group(source, at) else {
                    return false;
                };
                if grp_end < tikz_end {
                    return false;
                }
                outer_end = outer_end.max(grp_end);
                saw_wrapper = true;
                at += 1;
            }
            b'\\' => {
                let (cmd, next) = math::command(source, at);
                if cmd.is_empty() {
                    return false;
                }
                saw_wrapper = true;
                enum WrapperArg {
                    Star,
                    OptOne,
                    OptMany,
                    Req,
                }
                use WrapperArg::{OptMany, OptOne, Req, Star};
                let prefix: &[WrapperArg] = match cmd {
                    "centering" | "raggedright" | "raggedleft" | "noindent" => {
                        at = next;
                        continue;
                    }
                    "hbox" | "vbox" | "vcenter" | "centerline" => &[],
                    "makebox" | "framebox" | "subfloat" | "subfigure" => &[OptMany],
                    "scalebox" => &[Req, OptOne],
                    "resizebox" => &[Star, Req, Req],
                    "adjustbox" => &[OptOne, Req],
                    _ => return false,
                };
                let mut pos = next;
                for step in prefix {
                    match step {
                        Star => {
                            pos = skip_source_space(source, pos);
                            if source.as_bytes().get(pos) == Some(&b'*') {
                                pos += 1;
                            }
                        }
                        OptOne => {
                            if let Some(opt_end) = optional_argument_end(source, pos) {
                                if opt_end > tikz_start {
                                    return false;
                                }
                                pos = opt_end;
                            }
                        }
                        OptMany => {
                            while let Some(opt_end) = optional_argument_end(source, pos) {
                                if opt_end > tikz_start {
                                    return false;
                                }
                                pos = opt_end;
                            }
                        }
                        Req => {
                            let Some((_, arg_end)) = math::group(source, pos) else {
                                return false;
                            };
                            if arg_end > tikz_start {
                                return false;
                            }
                            pos = arg_end;
                        }
                    }
                }
                let Some((grp, grp_end)) = math::group(source, pos) else {
                    return false;
                };
                if grp.start > tikz_start || grp_end < tikz_end {
                    return false;
                }
                outer_end = outer_end.max(grp_end);
                at = grp.start;
            }
            _ => return false,
        }
    }
    if anchor < start && !saw_wrapper {
        return false;
    }
    let mut tail = tikz_end;
    while tail < outer_end {
        tail = skip_source_space(source, tail);
        if tail >= outer_end {
            break;
        }
        if source.as_bytes()[tail] == b'}' {
            tail += 1;
        } else {
            return false;
        }
    }
    if anchor <= start || anchor == end {
        let bytes = source.as_bytes();
        let mut pos = outer_end;
        while pos < source.len() {
            match bytes[pos] {
                b' ' | b'\t' | b'\r' => pos += 1,
                b'%' => {
                    pos = comment_end(source, pos);
                }
                b'\n' => {
                    pos += 1;
                    let mut line_pos = pos;
                    while line_pos < source.len() && matches!(bytes[line_pos], b' ' | b'\t' | b'\r')
                    {
                        line_pos += 1;
                    }
                    if line_pos >= source.len() || bytes[line_pos] == b'\n' {
                        break;
                    }
                }
                b'}' => pos += 1,
                b'\\' => {
                    if bytes.get(pos + 1) == Some(&b'\\') {
                        break;
                    }
                    let (cmd, next) = math::command(source, pos);
                    if vertical_break_command(cmd) {
                        break;
                    }
                    if layout_noop_command(cmd) {
                        pos = next;
                        continue;
                    }
                    if cmd == "label" {
                        let mut lpos = next;
                        if let Some(opt_end) = optional_argument_end(source, lpos) {
                            lpos = opt_end;
                        }
                        let Some((_, grp_end)) = math::group(source, lpos) else {
                            return false;
                        };
                        pos = grp_end;
                        continue;
                    }
                    return false;
                }
                _ => return false,
            }
        }
    }
    true
}

pub(crate) fn source_tikzpicture_range_with_ignored(
    source: &str,
    line_starts: &LineIndex,
    line: u32,
    ignored: &[Range<usize>],
) -> Option<Range<usize>> {
    if !source.contains("tikzpicture") {
        return None;
    }
    let anchor = line.checked_sub(1)? as usize;
    if let Some(lines) = environment_bounds_with_ignored(source, anchor, "tikzpicture", ignored) {
        return tikzpicture_exclusive_bounds(
            source,
            line_starts,
            anchor,
            lines.start,
            lines.end - 1,
        )
        .then_some(lines);
    }
    for offset in 1..=3 {
        let probe = anchor + offset;
        if let Some(lines) = environment_bounds_with_ignored(source, probe, "tikzpicture", ignored)
            && lines.start == probe
            && tikzpicture_exclusive_bounds(source, line_starts, anchor, lines.start, lines.end - 1)
        {
            return Some(lines);
        }
    }
    None
}

pub(crate) struct ForeachLoop<'a> {
    pub header: Range<usize>,
    pub body: Range<usize>,
    pub vars: Vec<&'a str>,
    pub items: Option<Vec<Vec<String>>>,
}

fn split_top_level(text: &str, sep: u8) -> Vec<&str> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut at = 0usize;
    while at < text.len() {
        match text.as_bytes()[at] {
            b'\\' => {
                at += 1;
                at += text[at..].chars().next().map_or(0, char::len_utf8);
            }
            b'%' => {
                at = comment_end(text, at);
            }
            b'{' => {
                depth += 1;
                at += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                at += 1;
            }
            ch if ch == sep && depth == 0 => {
                parts.push(text[start..at].trim());
                at += 1;
                start = at;
            }
            _ => {
                at += text[at..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    parts.push(text[start..].trim());
    parts
}

fn strip_outer_group(item: &str) -> &str {
    let trimmed = item.trim();
    match math::group(trimmed, 0) {
        Some((inner, end)) if end == trimmed.len() => trimmed[inner].trim(),
        _ => trimmed,
    }
}

fn valid_foreach_value(item: &str) -> bool {
    !item.is_empty() && !item.contains(['{', '}', '$', '#', '%'])
}

fn parse_foreach_items(list: &str, var_count: usize) -> Option<Vec<Vec<String>>> {
    let clean: String = list.split_inclusive('\n').map(source_line_text).collect();
    let raw = split_top_level(&clean, b',');
    if raw.is_empty() || raw.iter().any(|s| s.is_empty()) {
        return None;
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut idx = 0usize;
    while idx < raw.len() {
        let stripped = strip_outer_group(raw[idx]);
        if matches!(stripped, "..." | "\\dots" | "\\ldots") {
            if var_count != 1 || (idx != 1 && idx != 2) || idx + 2 != raw.len() || out.is_empty() {
                return None;
            }
            let prev = out.last()?.first()?.clone();
            let next = strip_outer_group(raw[idx + 1]);
            if let (Ok(first), Ok(last)) = (prev.parse::<i32>(), next.parse::<i32>())
                && (-1000..=1000).contains(&first)
                && (-1000..=1000).contains(&last)
            {
                let step = if idx == 2 {
                    let prev2 = out.get(out.len() - 2)?.first()?.parse::<i32>().ok()?;
                    if !(-1000..=1000).contains(&prev2) {
                        return None;
                    }
                    first.checked_sub(prev2)?
                } else if last >= first {
                    1
                } else {
                    -1
                };
                if step == 0 || (last != first && (last - first).signum() != step.signum()) {
                    return None;
                }
                let mut cur = first.checked_add(step)?;
                while (step > 0 && cur <= last) || (step < 0 && cur >= last) {
                    if out.len() >= 32 {
                        return None;
                    }
                    out.push(vec![cur.to_string()]);
                    cur = cur.checked_add(step)?;
                }
                idx += 2;
                continue;
            } else if idx == 1
                && prev.len() == 1
                && next.len() == 1
                && let (a, b) = (prev.as_bytes()[0], next.as_bytes()[0])
                && ((a.is_ascii_lowercase() && b.is_ascii_lowercase())
                    || (a.is_ascii_uppercase() && b.is_ascii_uppercase()))
            {
                let step: i16 = if b >= a { 1 } else { -1 };
                let mut cur = i16::from(a) + step;
                while (step > 0 && cur <= i16::from(b)) || (step < 0 && cur >= i16::from(b)) {
                    if out.len() >= 32 {
                        return None;
                    }
                    out.push(vec![char::from(cur as u8).to_string()]);
                    cur += step;
                }
                idx += 2;
                continue;
            }
            return None;
        }
        if out.len() >= 32 {
            return None;
        }
        if var_count == 1 {
            if !valid_foreach_value(stripped) {
                return None;
            }
            out.push(vec![stripped.to_owned()]);
        } else {
            let raw_parts = split_top_level(raw[idx], b'/');
            if raw_parts.is_empty() || raw_parts.len() > var_count {
                return None;
            }
            let mut parts = Vec::with_capacity(var_count);
            for p in raw_parts {
                let sp = strip_outer_group(p);
                if !valid_foreach_value(sp) || sp.contains('/') {
                    return None;
                }
                parts.push(sp.to_owned());
            }
            let last = parts.last()?.clone();
            parts.resize(var_count, last);
            out.push(parts);
        }
        idx += 1;
    }
    (!out.is_empty()).then_some(out)
}

#[allow(clippy::type_complexity)]
fn parse_foreach_header(
    source: &str,
    mut pos: usize,
) -> Option<(bool, Vec<&str>, Option<Range<usize>>, usize)> {
    let mut has_options = false;
    while let Some(opt_end) = optional_argument_end(source, pos) {
        has_options = true;
        pos = opt_end;
    }
    let mut vars = Vec::new();
    loop {
        pos = skip_source_space(source, pos);
        if source.as_bytes().get(pos) != Some(&b'\\') {
            break;
        }
        let (var, var_end) = math::command(source, pos);
        if var.is_empty() || !var.bytes().all(|b| b.is_ascii_alphabetic()) {
            return None;
        }
        vars.push(var);
        pos = skip_source_space(source, var_end);
        if source.as_bytes().get(pos) == Some(&b'/') {
            pos += 1;
        } else {
            break;
        }
    }
    if vars.is_empty() {
        return None;
    }
    while let Some(opt_end) = optional_argument_end(source, pos) {
        has_options = true;
        pos = opt_end;
    }
    pos = skip_source_space(source, pos);
    let after_in = source[pos..].strip_prefix("in")?;
    if after_in
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
    {
        return None;
    }
    pos = skip_source_space(source, pos + 2);
    let (list_range, after_list) = if let Some((range, end)) = math::group(source, pos) {
        (Some(range), end)
    } else if source.as_bytes().get(pos) == Some(&b'\\') {
        let (list_cmd, end) = math::command(source, pos);
        if list_cmd.is_empty() {
            return None;
        }
        (None, end)
    } else {
        return None;
    };
    Some((has_options, vars, list_range, after_list))
}

fn foreach_body_range(source: &str, body_start: usize, depth: usize) -> Option<Range<usize>> {
    if depth > 4 || body_start >= source.len() {
        return None;
    }
    if source.as_bytes()[body_start] == b'{' {
        return math::group(source, body_start).map(|(range, _)| range);
    }
    if source.as_bytes()[body_start] != b'\\' {
        return None;
    }
    let (bcmd, after_cmd) = math::command(source, body_start);
    if bcmd == "foreach" {
        let (_, _, _, after_list) = parse_foreach_header(source, after_cmd)?;
        let inner_start = skip_source_space(source, after_list);
        let inner_body = foreach_body_range(source, inner_start, depth + 1)?;
        let inner_end =
            inner_body.end + usize::from(source.as_bytes().get(inner_body.end) == Some(&b'}'));
        return Some(body_start..inner_end);
    }
    if bcmd.is_empty() {
        return None;
    }
    let mut scan = body_start;
    while scan < source.len() {
        match source.as_bytes()[scan] {
            b'\\' => {
                let (scmd, snext) = math::command(source, scan);
                if scmd == "end" {
                    return None;
                }
                scan = snext.max(scan + 1);
            }
            b'%' => {
                scan = comment_end(source, scan);
            }
            b'{' => {
                let (_, grp_end) = math::group(source, scan)?;
                scan = grp_end;
            }
            b';' => return Some(body_start..scan),
            _ => {
                scan += source[scan..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    None
}

pub(crate) fn foreach_loops<'a>(
    analysis: &SourceAnalysis<'a>,
    lines: &Range<usize>,
) -> Vec<ForeachLoop<'a>> {
    let source = analysis.source;
    if analysis.operation.check().is_err() || !source.contains("\\foreach") {
        return Vec::new();
    }
    let macros = analysis.macros(WordLocationMode::Strict);
    let (start_byte, end_byte) = analysis.line_byte_range(lines);
    let mut loops = Vec::new();
    let mut at = 0usize;
    while at < end_byte {
        match source.as_bytes()[at] {
            b'%' => {
                at = comment_end(source, at);
            }
            b'\\' => {
                let cmd_start = at;
                let (cmd, next) = math::command(source, at);
                at = next;
                if let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, at) {
                    at = opaque_end;
                    continue;
                }
                if cmd != "foreach" {
                    continue;
                }
                let Some((has_options, vars, list_range, after_list)) =
                    parse_foreach_header(source, at)
                else {
                    if cmd_start >= start_byte {
                        loops.push(ForeachLoop {
                            header: cmd_start..at,
                            body: cmd_start..end_byte,
                            vars: Vec::new(),
                            items: None,
                        });
                    }
                    continue;
                };
                let body_start = skip_source_space(source, after_list);
                let Some(body) = foreach_body_range(source, body_start, 0) else {
                    if cmd_start >= start_byte {
                        loops.push(ForeachLoop {
                            header: cmd_start..after_list,
                            body: body_start.min(end_byte)..end_byte,
                            vars,
                            items: None,
                        });
                    }
                    continue;
                };
                if body.end <= start_byte || body.start >= end_byte {
                    continue;
                }
                let body_src = &source[body.clone()];
                let mut body_redefines = false;
                let mut bpos = 0usize;
                while let Some(rel) = body_src[bpos..].find('\\') {
                    let slash = bpos + rel;
                    let (bcmd, bnext) = math::command(body_src, slash);
                    bpos = bnext.max(slash + 1);
                    if math::is_macro_definition_command(bcmd) {
                        body_redefines = true;
                        break;
                    }
                }
                let clashes_with_macro = vars.iter().any(|v| macros.contains_key(v));
                let items = if has_options || vars.len() > 4 || body_redefines || clashes_with_macro
                {
                    None
                } else {
                    list_range.and_then(|r| parse_foreach_items(&source[r], vars.len()))
                };
                loops.push(ForeachLoop {
                    header: cmd_start..after_list,
                    body,
                    vars,
                    items,
                });
            }
            _ => {
                let rest = &source.as_bytes()[at + 1..end_byte];
                let step = rest
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'\\'))
                    .map_or(rest.len() + 1, |p| p + 1);
                at += step;
            }
        }
    }
    loops
}

pub(crate) fn foreach_bindings_for_range<'a>(
    loops: &'a [ForeachLoop<'a>],
    range: &Range<usize>,
) -> Option<Vec<Vec<(&'a str, &'a str)>>> {
    if loops.len() > 2 || loops.iter().any(|lp| lp.items.is_none()) {
        return None;
    }
    let enclosing: Vec<_> = loops
        .iter()
        .filter(|lp| lp.body.start <= range.start && range.end <= lp.body.end)
        .collect();
    if enclosing.is_empty() {
        return Some(Vec::new());
    }
    if enclosing.len() > 2 {
        return None;
    }
    foreach_combinations(enclosing, |_, var, val| Some((var, val)))
}

/// Cartesian product of the loops' variable bindings, mapped through `bind`. `None` when a
/// loop has unparsed items, past 32 combinations, or when `bind` rejects a binding.
fn foreach_combinations<'l, 'a: 'l, T: Clone>(
    loops: impl IntoIterator<Item = &'l ForeachLoop<'a>>,
    mut bind: impl FnMut(&'l ForeachLoop<'a>, &'a str, &'l str) -> Option<T>,
) -> Option<Vec<Vec<T>>> {
    let mut combinations = vec![Vec::new()];
    for lp in loops {
        let items = lp.items.as_ref()?;
        if combinations.len().saturating_mul(items.len()) > 32 {
            return None;
        }
        let mut next = Vec::with_capacity(combinations.len() * items.len());
        for prefix in &combinations {
            for item in items {
                let mut combined = prefix.clone();
                for (&var, val) in lp.vars.iter().zip(item) {
                    combined.push(bind(lp, var, val)?);
                }
                next.push(combined);
            }
        }
        combinations = next;
    }
    Some(combinations)
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
    let symbol_accent = matches!(cmd, b'\'' | b'`' | b'^' | b'"' | b'~' | b'=' | b'.');
    let letter_accent = matches!(
        cmd,
        b'u' | b'v' | b'H' | b'c' | b'k' | b'r' | b'b' | b'd' | b't'
    );
    // Letter accents need a braced argument; symbol accents also take a bare one.
    let (arg, braced) = match rest.strip_prefix('{') {
        Some(braced) if symbol_accent || letter_accent => (braced, true),
        None if symbol_accent => (rest, false),
        _ => return None,
    };
    // A braced argument closes at `}`; a bare control word ends before any letter.
    let close = |tail: &'_ str| -> Option<usize> {
        let tail = if braced {
            tail.strip_prefix('}')?
        } else if tail
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
        {
            return None;
        } else {
            tail
        };
        Some(text.len() - tail.len())
    };
    for (dotless, control) in [('i', "\\i"), ('j', "\\j")] {
        if let Some(end) = arg.strip_prefix(control).and_then(close) {
            return Some((dotless, end));
        }
    }
    let ch = arg.chars().next().filter(|ch| ch.is_alphabetic())?;
    let tail = arg.get(ch.len_utf8()..)?;
    let end = if braced {
        close(tail)?
    } else {
        text.len() - tail.len()
    };
    Some((ch, end))
}

pub(crate) fn has_unescaped_backslash_before(text: &str, index: usize) -> bool {
    !text[..index]
        .bytes()
        .rev()
        .take_while(|&b| b == b'\\')
        .count()
        .is_multiple_of(2)
}

fn tex_accent_at(text: &str, index: usize) -> Option<(char, usize)> {
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
    let mut escaped = false;
    while index < text.len() {
        if !escaped && let Some((ch, next)) = tex_accent_at(text, index) {
            start.get_or_insert(index);
            word.push(ch);
            index = next;
            escaped = false;
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
        escaped = ch == '\\' && !escaped;
        index += ch.len_utf8();
    }
    if let Some(start) = start {
        result.push((start, text.len(), normalized_word(&word)));
    }
    result
}

pub(crate) fn normalized_word(word: &str) -> String {
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

pub(crate) fn ordinary_word_source_start(
    analysis: &SourceAnalysis<'_>,
    (context, offset): (&str, usize),
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
    let source = analysis.source;
    let row = (line as usize).checked_sub(1)?;
    let line_start = analysis.line_starts.row_start(row, source.len());
    if row < analysis.line_starts.len() {
        let text = &source[line_start..analysis.line_starts.row_start(row + 1, source.len())];
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let visible = source_line_text(text);
        if let Some((start, _, word)) = source_words(visible).into_iter().find(|(start, end, _)| {
            *start <= byte && byte < *end && !has_unescaped_backslash_before(visible, *start)
        }) && word == pdf_word
        {
            return Some(start);
        }
    }
    let abs = line_start + byte;
    (contains_byte(analysis.math_regions(), abs) || source.as_bytes().get(abs) == Some(&b'\\'))
        .then_some(byte)
}

/// A word search result; `Tied` found competing evidence, `Absent` found none,
/// and `Opaque` indicates macro/note uncertainty or cancellation/timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WordOutcome {
    Located(u32, usize),
    Tied,
    Absent,
    Opaque,
}

impl WordOutcome {
    pub(crate) fn location(self) -> Option<(u32, usize)> {
        match self {
            Self::Located(line, byte) => Some((line, byte)),
            Self::Tied | Self::Absent | Self::Opaque => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WordLocationMode {
    Strict,
    Guess,
}

/// Byte offsets of each source line start, shared by every line/byte lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LineIndex(Vec<usize>);

impl LineIndex {
    pub(crate) fn new(source: &str) -> Self {
        Self(
            std::iter::once(0)
                .chain(source.match_indices('\n').map(|(i, _)| i + 1))
                .collect(),
        )
    }

    /// Zero-based row containing `byte`.
    pub(crate) fn row_of(&self, byte: usize) -> usize {
        self.0
            .partition_point(|&start| start <= byte)
            .saturating_sub(1)
    }

    /// Zero-based row and byte column of `byte`.
    pub(crate) fn line_col(&self, byte: usize) -> (usize, usize) {
        let row = self.row_of(byte);
        (row, byte - self.0.get(row).copied().unwrap_or(0))
    }

    /// Byte of a zero-based row start, clamped to `len` past the last row.
    pub(crate) fn row_start(&self, row: usize, len: usize) -> usize {
        self.0.get(row).copied().unwrap_or(len)
    }
}

impl std::ops::Deref for LineIndex {
    type Target = [usize];

    fn deref(&self) -> &[usize] {
        &self.0
    }
}

pub(crate) type Span = (usize, Range<usize>, Option<Range<usize>>);

pub(crate) struct SourceFileIndex {
    pub(crate) line_starts: LineIndex,
    pub(crate) strict_spans: Vec<Span>,
    pub(crate) strict_hidden: Vec<Range<usize>>,
    /// `strict_hidden` merged with opaque (verbatim/quoted) command ranges, so
    /// environment-boundary scans never rescan the file for opaque tokens.
    pub(crate) strict_ignored: Vec<Range<usize>>,
}

impl SourceFileIndex {
    pub(crate) fn new(source: &str, operation: &crate::Operation) -> Self {
        let line_starts = LineIndex::new(source);
        let strict_spans: Vec<Span> = nonprinting_source_spans_with_mode(
            source,
            None,
            None,
            WordLocationMode::Strict,
            Some(operation),
        )
        .collect();
        let strict_hidden: Vec<_> = strict_spans
            .iter()
            .map(|(_, range, _)| range.clone())
            .collect();
        let strict_ignored = environment_ignored_ranges(source, &strict_hidden);
        Self {
            line_starts,
            strict_spans,
            strict_hidden,
            strict_ignored,
        }
    }
}

pub(crate) struct SourceAnalysis<'a> {
    pub(crate) source: &'a str,
    pub(crate) extra_sources: Vec<&'a str>,
    pub(crate) extra_indices: Vec<std::rc::Rc<SourceFileIndex>>,
    pub(crate) operation: &'a crate::Operation,
    pub(crate) index: std::rc::Rc<SourceFileIndex>,
    strict_macros: std::cell::OnceCell<MacroTable<'a>>,
    guess_macros: std::cell::OnceCell<MacroTable<'a>>,
    strict_mode_spans: std::cell::OnceCell<Vec<Span>>,
    guess_mode_spans: std::cell::OnceCell<Vec<Span>>,
    environments: std::cell::OnceCell<EnvTable<'a>>,
    math_regions: std::cell::OnceCell<Vec<Range<usize>>>,
}

impl std::ops::Deref for SourceAnalysis<'_> {
    type Target = SourceFileIndex;

    fn deref(&self) -> &Self::Target {
        &self.index
    }
}

impl<'a> SourceAnalysis<'a> {
    #[cfg(test)]
    pub(crate) fn new(
        source: &'a str,
        extra_sources: &[&'a str],
        operation: &'a crate::Operation,
    ) -> Self {
        let extra_indices = extra_sources
            .iter()
            .map(|s| std::rc::Rc::new(SourceFileIndex::new(s, operation)))
            .collect();
        Self::with_index(
            source,
            std::rc::Rc::new(SourceFileIndex::new(source, operation)),
            extra_sources,
            extra_indices,
            operation,
        )
    }

    pub(crate) fn with_index(
        source: &'a str,
        index: std::rc::Rc<SourceFileIndex>,
        extra_sources: &[&'a str],
        extra_indices: Vec<std::rc::Rc<SourceFileIndex>>,
        operation: &'a crate::Operation,
    ) -> Self {
        Self {
            source,
            extra_sources: extra_sources.to_vec(),
            extra_indices,
            operation,
            index,
            strict_macros: std::cell::OnceCell::new(),
            guess_macros: std::cell::OnceCell::new(),
            strict_mode_spans: std::cell::OnceCell::new(),
            guess_mode_spans: std::cell::OnceCell::new(),
            environments: std::cell::OnceCell::new(),
            math_regions: std::cell::OnceCell::new(),
        }
    }

    pub(super) fn known_span_entries(&self) -> Vec<(&'a str, &[Span])> {
        let mut entries = Vec::with_capacity(self.extra_indices.len() + 1);
        for (&src, idx) in self.extra_sources.iter().zip(&self.extra_indices) {
            entries.push((src, idx.strict_spans.as_slice()));
        }
        entries.push((self.source, self.strict_spans.as_slice()));
        entries
    }

    pub(crate) fn line_byte_range(&self, lines: &Range<usize>) -> (usize, usize) {
        let start = self.line_starts.row_start(lines.start, self.source.len());
        let end = self
            .line_starts
            .row_start(lines.end, self.source.len())
            .max(start);
        (start, end)
    }

    pub(crate) fn macros(&self, mode: WordLocationMode) -> &MacroTable<'a> {
        let cell = match mode {
            WordLocationMode::Strict => &self.strict_macros,
            WordLocationMode::Guess => &self.guess_macros,
        };
        cell.get_or_init(|| {
            let mut sources = Vec::with_capacity(self.extra_sources.len() + 1);
            sources.extend_from_slice(&self.extra_sources);
            sources.push(self.source);
            let known = self.known_span_entries();
            math::macros_from_sources_with_mode(
                &sources,
                mode == WordLocationMode::Guess,
                &known,
                self.operation,
            )
        })
    }

    /// Sorted, disjoint closed literal math regions of the whole source.
    pub(crate) fn math_regions(&self) -> &[Range<usize>] {
        self.math_regions
            .get_or_init(|| math::regions_with_spans(self.source, &self.strict_spans))
    }

    pub(crate) fn environments(&self) -> &EnvTable<'a> {
        self.environments.get_or_init(|| {
            environment_definitions(
                &self.known_span_entries(),
                self.macros(WordLocationMode::Strict),
                self.operation,
            )
        })
    }

    pub(super) fn mode_spans(&self, mode: WordLocationMode) -> &[Span] {
        let macros = self.macros(mode);
        if mode == WordLocationMode::Strict && macros.is_empty() {
            return &self.strict_spans;
        }
        let cell = match mode {
            WordLocationMode::Strict => &self.strict_mode_spans,
            WordLocationMode::Guess => &self.guess_mode_spans,
        };
        cell.get_or_init(|| {
            nonprinting_source_spans_with_mode(
                self.source,
                Some(macros),
                Some(&self.strict_spans),
                mode,
                Some(self.operation),
            )
            .collect()
        })
    }
}

#[derive(Default)]
pub(crate) struct AnalysisCache<'a> {
    indices: std::cell::RefCell<Vec<(*const u8, usize, std::rc::Rc<SourceFileIndex>)>>,
    analyses: std::cell::RefCell<Vec<std::rc::Rc<SourceAnalysis<'a>>>>,
}

impl<'a> AnalysisCache<'a> {
    pub(crate) fn file_index(
        &self,
        source: &'a str,
        operation: &'a crate::Operation,
    ) -> std::rc::Rc<SourceFileIndex> {
        let key = (source.as_ptr(), source.len());
        if let Some((_, _, idx)) = self
            .indices
            .borrow()
            .iter()
            .find(|(ptr, len, _)| *ptr == key.0 && *len == key.1)
        {
            return std::rc::Rc::clone(idx);
        }
        let idx = std::rc::Rc::new(SourceFileIndex::new(source, operation));
        self.indices
            .borrow_mut()
            .push((key.0, key.1, std::rc::Rc::clone(&idx)));
        idx
    }

    pub(crate) fn get(
        &self,
        source: &'a str,
        extra_sources: &[&'a str],
        operation: &'a crate::Operation,
    ) -> std::rc::Rc<SourceAnalysis<'a>> {
        if let Some(found) = self.analyses.borrow().iter().find(|entry| {
            std::ptr::eq(entry.source, source)
                && entry.extra_sources.len() == extra_sources.len()
                && entry
                    .extra_sources
                    .iter()
                    .zip(extra_sources)
                    .all(|(a, b)| std::ptr::eq(*a, *b))
        }) {
            return std::rc::Rc::clone(found);
        }
        let index = self.file_index(source, operation);
        let extra_indices = extra_sources
            .iter()
            .map(|&s| self.file_index(s, operation))
            .collect();
        let created = std::rc::Rc::new(SourceAnalysis::with_index(
            source,
            index,
            extra_sources,
            extra_indices,
            operation,
        ));
        self.analyses
            .borrow_mut()
            .push(std::rc::Rc::clone(&created));
        created
    }
}

pub(crate) struct WordRangeScope<'a> {
    pub line: u32,
    pub lines: Range<usize>,
    pub foreach_loops: &'a [ForeachLoop<'a>],
    pub execution_scope: Option<&'a Range<usize>>,
}

pub(crate) fn run_in_heading_compiler_line_with_spans(
    source: &str,
    line: u32,
    column: u32,
    known_spans: &[Span],
) -> Option<u32> {
    let row = source
        .split_inclusive('\n')
        .nth(line.checked_sub(1)? as usize)?;
    // `row` borrows `source`, so its offset is the line start.
    let row_start = row.as_ptr() as usize - source.as_ptr() as usize;
    let byte = row_start + row.char_indices().nth(column.checked_sub(1)? as usize)?.0;
    let mut skip_until = 0;
    let mut hidden = known_spans.iter().peekable();
    for (at, _) in source[..byte].match_indices('\\') {
        if at < skip_until {
            continue;
        }
        while hidden.peek().is_some_and(|(_, range, _)| range.end <= at) {
            hidden.next();
        }
        if let Some((start, range, _)) = hidden.peek()
            && *start <= at
        {
            skip_until = range.end;
            continue;
        }
        let row_start = source[..at].rfind('\n').map_or(0, |at| at + 1);
        let prefix = &source[row_start..at];
        if source_line_text(prefix).len() != prefix.len() {
            continue;
        }
        let (name, mut end) = math::command(source, at);
        skip_until = math::opaque_command_end(source, name, end).map_or(end, |(end, _)| end);
        if skip_until > byte {
            return None;
        }
        if !matches!(name, "paragraph" | "subparagraph") {
            continue;
        }
        end = skip_source_space(source, end);
        end += usize::from(source.as_bytes().get(end) == Some(&b'*'));
        end = optional_argument_end(source, end).unwrap_or(end);
        let Some((title, end)) = math::group(source, end) else {
            continue;
        };
        if !title.contains(&byte) {
            skip_until = end;
            continue;
        }
        let mut body = skip_source_space(source, end);
        while source.as_bytes().get(body) == Some(&b'\\') {
            let (name, end) = math::command(source, body);
            if matches!(name, "label" | "index") {
                let (_, end) = math::group(source, end)?;
                body = skip_source_space(source, end);
                continue;
            }
            if let Some((end, true)) = math::opaque_command_end(source, name, end) {
                body = skip_source_space(source, end);
                continue;
            }
            while hidden.peek().is_some_and(|(_, range, _)| range.end <= body) {
                hidden.next();
            }
            if stored_definition_command(name)
                && let Some((start, range, _)) = hidden.peek()
                && *start == body
            {
                body = skip_source_space(source, range.end);
                continue;
            }
            break;
        }
        if body == source.len()
            || (source.as_bytes()[body] == b'\\'
                && (math::command(source, body).0 == "end"
                    || sectioning_command(math::command(source, body).0)))
        {
            return None;
        }
        return Some(source[..body].bytes().filter(|&b| b == b'\n').count() as u32 + 1);
    }
    None
}

pub(crate) fn line_start_word_compiler_line_with_spans(
    source: &str,
    line: u32,
    column: u32,
    known_spans: &[Span],
) -> Option<u32> {
    let previous_line = line.checked_sub(1)?;
    let mut rows = source.lines();
    let previous_row = rows.nth(previous_line.checked_sub(1)? as usize)?;
    let previous = source_line_text(previous_row);
    let row = rows.next()?;
    let word = row.trim_start_matches([' ', '\t']);
    if column as usize != row.len() - word.len() + 1
        || !word.chars().next()?.is_alphanumeric()
        || previous.trim().is_empty()
    {
        return None;
    }
    let previous_start = previous_row.as_ptr() as usize - source.as_ptr() as usize;
    let previous_end = previous_start + previous_row.len();
    // A closed printed header is proven scope; only tokens outside it can hide a boundary.
    let boundary = if let Some((header, _)) =
        math::printed_header_bounds_in(source, previous_start..previous_end, known_spans)
    {
        if !valid_printed_header_prefix(source, previous_start, header.start) {
            return None;
        }
        source_line_text(&source[header.end.min(previous_end)..previous_end])
    } else {
        previous
    };
    // shortcut: grouped or non-math command boundaries need scope proof for read-ahead.
    if boundary.contains(['{', '}']) {
        return None;
    }
    if boundary.contains('\\') {
        // The boundary is a slice of one line; its own spans and math regions apply.
        let boundary_spans: Vec<_> = nonprinting_source_spans(boundary).collect();
        let regions = math::regions_with_spans(boundary, &boundary_spans);
        if boundary
            .match_indices('\\')
            .any(|(at, _)| !contains_byte(&regions, at))
        {
            return None;
        }
    }
    let mut skip_until = 0;
    let mut hidden = known_spans.iter().peekable();
    for (at, _) in source[..previous_end].match_indices('\\') {
        if at < skip_until {
            continue;
        }
        while hidden.peek().is_some_and(|(_, range, _)| range.end <= at) {
            hidden.next();
        }
        if let Some((start, range, _)) = hidden.peek()
            && *start <= at
        {
            skip_until = range.end;
            continue;
        }
        let row_start = source[..at].rfind('\n').map_or(0, |at| at + 1);
        let prefix = &source[row_start..at];
        if source_line_text(prefix).len() != prefix.len() {
            continue;
        }
        let (name, mut end) = math::command(source, at);
        skip_until = math::opaque_command_end(source, name, end).map_or(end, |(end, _)| end);
        if !sectioning_command(name) {
            continue;
        }
        end = skip_source_space(source, end);
        end += usize::from(source.as_bytes().get(end) == Some(&b'*'));
        end = optional_argument_end(source, end).unwrap_or(end);
        let (_, end) = math::argument(source, end, source.len(), false)?;
        skip_until = end;
        // A generated heading label must not borrow a body literal across its title.
        if end > previous_start {
            return None;
        }
    }
    // TeX can tag a continued line's first literal word before advancing its input line.
    Some(previous_line)
}

fn valid_printed_header_prefix(source: &str, mut at: usize, header_start: usize) -> bool {
    while at < header_start {
        at += source[at..header_start].len()
            - source[at..header_start]
                .trim_start_matches([' ', '\t'])
                .len();
        if at >= header_start {
            return true;
        }
        if source.as_bytes()[at] != b'\\' {
            return false;
        }
        let (cmd, end) = math::command(source, at);
        match cmd {
            "begin" | "item" => {
                at = if let Some(header) = math::printed_header_argument(source, cmd, end) {
                    skip_overlay_arguments(source, header.end)
                } else if cmd == "item" {
                    skip_overlay_arguments(source, end)
                } else {
                    let Some((env, end)) = math::group(source, end) else {
                        return false;
                    };
                    skip_nonprinting_environment_arguments(source, source[env].trim(), end)
                };
            }
            "label" => {
                let Some((_, end)) = math::group(source, end) else {
                    return false;
                };
                at = end;
            }
            _ => return false,
        }
    }
    at == header_start
}

pub(crate) fn source_word_location_with_analysis(
    analysis: &SourceAnalysis<'_>,
    line: u32,
    (context, offset): (&str, usize),
    radius: u32,
    mode: WordLocationMode,
) -> WordOutcome {
    if analysis.operation.check().is_err() {
        return WordOutcome::Opaque;
    }
    if mode == WordLocationMode::Guess && unscoped_import_output(analysis) {
        return WordOutcome::Opaque;
    }
    let source = analysis.source;
    let spans = &analysis.strict_spans;
    let mut tikz_tied = false;
    if let Some(tikz) = source_tikzpicture_range_with_ignored(
        source,
        &analysis.line_starts,
        line,
        &analysis.strict_ignored,
    ) {
        match source_word_location_in_range_with_analysis(
            analysis,
            line,
            (context, offset),
            tikz,
            true,
            mode,
            None,
        ) {
            WordOutcome::Located(row, byte) => return WordOutcome::Located(row, byte),
            WordOutcome::Tied => tikz_tied = true,
            WordOutcome::Absent => {}
            WordOutcome::Opaque => return WordOutcome::Opaque,
        }
    }
    let collected =
        math::collected_text_lines_with_spans(source, line, spans, &analysis.strict_ignored);
    let read_ahead = collected
        .as_ref()
        .and_then(|lines| printed_header_read_ahead(analysis, lines, line));
    let scope = collected
        .or_else(|| collected_lines(source, line, "frame", &analysis.strict_ignored))
        .or_else(|| collected_lines(source, line, "tabularx", &analysis.strict_ignored))
        .or_else(|| math::math_lines_with_spans(source, line, spans));
    let within_scope = scope.is_some();
    let lines = scope.unwrap_or_else(|| {
        line.saturating_sub(radius.saturating_add(1)) as usize
            ..(line as usize).saturating_add(radius as usize)
    });
    let (lines, execution_scope) =
        read_ahead.map_or((lines, None), |(lines, bytes)| (lines, Some(bytes)));
    let outcome = source_word_location_in_range_with_analysis(
        analysis,
        line,
        (context, offset),
        lines,
        within_scope,
        mode,
        execution_scope.as_ref(),
    );
    if tikz_tied && outcome == WordOutcome::Absent {
        WordOutcome::Tied
    } else {
        outcome
    }
}

/// A class or package this file loads may print anywhere (`\AtBeginDocument`,
/// class code) unless its declaration is proven to precede the document body.
/// Unresolved import output is a competing owner no warned guess may override.
fn unscoped_import_output(analysis: &SourceAnalysis<'_>) -> bool {
    let source = analysis.source;
    let hidden = &analysis.strict_hidden;
    let declares = control_words(source, 0..source.len())
        .any(|(at, name, _)| class_package_declaration(name) && !contains_byte(hidden, at));
    if !declares {
        return false;
    }
    let macros = analysis.macros(WordLocationMode::Guess);
    macros.contains_key("begin")
        || source_document_start(
            source,
            Some(macros),
            Some(&analysis.strict_spans),
            Some(analysis.operation),
        )
        .is_none()
}

/// Extend a printed header's scope through the next-line body word TeX tags on its closing line.
pub(super) fn printed_header_read_ahead(
    analysis: &SourceAnalysis<'_>,
    lines: &Range<usize>,
    line: u32,
) -> Option<(Range<usize>, Range<usize>)> {
    let close = line as usize;
    if lines.end != close {
        return None;
    }
    let source = analysis.source;
    let row_start = *analysis.line_starts.get(close.checked_sub(1)?)?;
    let next_start = *analysis.line_starts.get(close)?;
    let spans = &analysis.strict_spans;
    if !math::printed_header_bounds_in(source, row_start..next_start, spans)
        .is_some_and(|(_, multiline)| multiline)
    {
        return None;
    }
    let next = source_line_text(source[next_start..].lines().next()?);
    let indent = next.len() - next.trim_start_matches([' ', '\t']).len();
    let (_, end, _) = source_words(next)
        .into_iter()
        .find(|(byte, _, word)| *byte == indent && !word.is_empty())?;
    let column = u32::try_from(next[..indent].chars().count())
        .ok()?
        .checked_add(1)?;
    (line_start_word_compiler_line_with_spans(source, line.checked_add(1)?, column, spans)
        == Some(line))
    .then_some((
        lines.start..close + 1,
        analysis.line_starts[lines.start]..next_start + end,
    ))
}

fn class_package_declaration(command: &str) -> bool {
    matches!(command, "documentclass" | "usepackage" | "RequirePackage")
}

fn class_package_declaration_end(source: &str, mut at: usize) -> Option<usize> {
    while let Some(end) = optional_argument_end(source, at) {
        at = end;
    }
    let (_, mut end) = math::group(source, at)?;
    while let Some(optional_end) = optional_argument_end(source, end) {
        end = optional_end;
    }
    Some(end)
}

/// A user environment's signature and stored begin/end code. `header` holds
/// `(arity, optional #1..#9 mask, printed #1..#9 mask)` for simple signatures.
pub(crate) struct EnvDef<'a> {
    header: Option<(u8, u16, u16)>,
    begin_code: &'a str,
    end_code: &'a str,
    /// The begin or end code calls `\note` (or a note wrapper): the
    /// environment's unprinted arguments are note-owned.
    calls_note: bool,
}

pub(crate) type EnvTable<'a> = std::collections::BTreeMap<&'a str, EnvDef<'a>>;

/// Read environment declarations from their nonprinting spans.
fn environment_definitions<'a>(
    sources: &[(&'a str, &[Span])],
    macros: &MacroTable<'a>,
    operation: &crate::Operation,
) -> EnvTable<'a> {
    let mut table = EnvTable::new();
    for &(source, spans) in sources {
        for (start, span, _) in spans {
            if operation.check().is_err() {
                return table;
            }
            if source.as_bytes().get(*start) != Some(&b'\\') {
                continue;
            }
            let (command, end) = math::command(source, *start);
            if !environment_definition_command(command) {
                continue;
            }
            let at = end + usize::from(matches!(source.as_bytes().get(end), Some(b'*' | b'+')));
            let Some((name, mut at)) = math::group(source, skip_definition_overlay(source, at))
            else {
                continue;
            };
            let name = source[name].trim();
            let xparse = command.contains("Document");
            let header = if xparse {
                let Some((spec, end)) = math::group(source, at) else {
                    continue;
                };
                at = end;
                math::spec_signature(&source[spec])
            } else if let Some(end) = optional_argument_end(source, at) {
                let open = skip_source_space(source, at);
                let arity = source[open + 1..end - 1].trim().parse::<u8>().ok();
                at = end;
                // `[arity][default]`: the first parameter is optional.
                let optional = optional_argument_end(source, at).inspect(|&end| at = end);
                arity
                    .filter(|arity| *arity <= 9)
                    .map(|arity| (arity, u16::from(optional.is_some() && arity > 0)))
            } else {
                Some((0, 0))
            };
            let code = |at: usize| math::group(source, at).filter(|(_, end)| *end <= span.end);
            let Some((begin, at)) = code(at) else {
                continue;
            };
            let begin_code = &source[begin];
            let end_code = code(at).map_or("", |(body, _)| &source[body]);
            let mut calls_note =
                math::calls_note(begin_code, macros) || math::calls_note(end_code, macros);
            let mut header = header.map(|(arity, opt_params)| {
                let printed = |code| math::printed_parameters(code, arity, macros, operation).0;
                // xparse environments also pass their arguments to the end code.
                let used = printed(begin_code) | if xparse { printed(end_code) } else { 0 };
                (arity, opt_params, used)
            });
            if let Some(old) = table.get(name) {
                calls_note |= old.calls_note;
                if old.header != header {
                    // Conflicting redefinitions leave the header unknown; with a
                    // note call, keep the widest one with no argument printed.
                    header = header
                        .max(old.header)
                        .filter(|_| calls_note)
                        .map(|(arity, opt_params, _)| (arity, opt_params, 0));
                }
            }
            table.insert(
                name,
                EnvDef {
                    header,
                    begin_code,
                    end_code,
                    calls_note,
                },
            );
        }
    }
    table
}

/// A declared environment invoked in the range may print `target` from its
/// begin or end code.
fn custom_environment_prints_word(
    analysis: &SourceAnalysis<'_>,
    lines: &Range<usize>,
    hidden: &[Range<usize>],
    target: &str,
    mode: WordLocationMode,
) -> bool {
    let source = analysis.source;
    let (start, end) = analysis.line_byte_range(lines);
    let macros = analysis.macros(mode);
    let visible = |hidden: &[Range<usize>], at: usize| !contains_byte(hidden, at);
    let prints = |code: &str| {
        let Some((mut code_hidden, _)) = discarded_macro_args(
            code,
            true,
            0..code.len(),
            macros,
            None,
            None,
            analysis.operation,
        ) else {
            return true;
        };
        code_hidden.extend(
            nonprinting_source_spans_with_mode(
                code,
                Some(macros),
                None,
                WordLocationMode::Strict,
                Some(analysis.operation),
            )
            .map(|(_, span, _)| span),
        );
        merge_hidden_ranges(&mut code_hidden);
        let mut offset = 0;
        code.split_inclusive('\n').any(|row| {
            let text = source_line_text(row);
            let found = source_words(text).into_iter().any(|(byte, _, word)| {
                !has_unescaped_backslash_before(text, byte)
                    && !(byte > 0
                        && text.as_bytes()[byte - 1] == b'#'
                        && !has_unescaped_backslash_before(text, byte - 1))
                    && word == target
                    && visible(&code_hidden, offset + byte)
            });
            offset += row.len();
            found
        })
    };
    control_words(source, start..end).any(|(at, name, name_end)| {
        matches!(name, "begin" | "end")
            && visible(hidden, at)
            && math::group(source, name_end)
                .and_then(|(env, _)| analysis.environments().get(source[env].trim()))
                .is_some_and(|env| prints(env.begin_code) || prints(env.end_code))
    })
}

/// Beamer overlay commands that make slide content conditional.
#[rustfmt::skip]
fn beamer_overlay_command(name: &str) -> bool {
    matches!(
        name,
        "pause" | "onslide" | "uncover" | "visible" | "invisible" | "alt" | "temporal"
        | "overprint" | "overlayarea" | "framebreak" | "note" | "againframe"
    )
}

/// Beamer overlay environments that make slide content conditional.
#[rustfmt::skip]
fn beamer_overlay_environment(name: &str) -> bool {
    matches!(
        name,
        "onlyenv" | "uncoverenv" | "visibleenv" | "invisibleenv" | "altenv" | "overprint"
        | "overlayarea" | "note"
    )
}

/// Commands whose printed text is generated, not reference prose.
#[rustfmt::skip]
fn generated_heading_command(name: &str) -> bool {
    matches!(
        name,
        "caption" | "section" | "subsection" | "subsubsection" | "paragraph" | "subparagraph"
        | "part" | "chapter" | "frametitle" | "framesubtitle" | "maketitle" | "titlepage"
    )
}

/// Layout commands whose group arguments are not reference text.
#[rustfmt::skip]
fn generated_layout_command(name: &str) -> bool {
    matches!(
        name,
        "begin" | "end" | "item" | "par" | "pagebreak" | "newpage" | "clearpage"
        | "cleardoublepage" | "maketitle" | "titlepage"
    )
}

/// Commands that print generated text around a reference.
#[rustfmt::skip]
fn generated_text_command(name: &str) -> bool {
    matches!(
        name,
        "cite" | "citep" | "citet" | "parencite" | "textcite" | "autocite" | "footcite" | "input"
        | "include" | "includegraphics" | "includesvg" | "verb" | "lstinline"
    )
}

/// Spacing and alignment commands a wrapper line may contain.
#[rustfmt::skip]
fn layout_noop_command(name: &str) -> bool {
    matches!(
        name,
        "relax" | "hfill" | "hfil" | "hss" | "centering" | "raggedright" | "raggedleft"
        | "noindent" | "egroup" | "endgroup"
    )
}

/// Commands whose first argument is a key, path, or color, not prose.
#[rustfmt::skip]
fn nonprinting_argument_command(name: &str) -> bool {
    is_direct_reference_command(name)
        || matches!(
            name,
            "label" | "cite" | "citep" | "citet" | "parencite" | "textcite" | "autocite"
            | "footcite" | "nocite" | "href" | "textcolor" | "color" | "includegraphics"
            | "includesvg" | "input" | "include" | "bibliography" | "addbibresource" | "bibitem"
        )
}

/// Text style wrappers whose argument prints as-is.
#[rustfmt::skip]
fn text_style_wrapper(name: &str) -> bool {
    matches!(
        name,
        "textbf" | "textit" | "emph" | "textrm" | "textsf" | "texttt" | "textnormal" | "underline"
        | "mbox"
    )
}

/// Commands whose argument is logged, measured, or quoted, not executed.
#[rustfmt::skip]
fn unexecuted_argument_command(name: &str) -> bool {
    matches!(
        name,
        "detokenize" | "unexpanded" | "typeout" | "message" | "errmessage" | "wlog"
        | "pdfescapestring" | "phantom" | "hphantom" | "vphantom" | "sbox" | "savebox"
    )
}

/// Commands that end a wrapper line before a `tikzpicture`.
#[rustfmt::skip]
fn vertical_break_command(name: &str) -> bool {
    matches!(
        name,
        "par" | "newline" | "linebreak" | "pagebreak" | "newpage" | "clearpage"
        | "cleardoublepage" | "vfill" | "vfil" | "vspace" | "bigskip" | "medskip" | "smallskip"
        | "hline" | "toprule" | "midrule" | "bottomrule" | "item" | "caption" | "subcaption"
        | "captionof" | "begin" | "end"
    )
}

/// `\cref` format commands and their group count: all are configuration.
#[rustfmt::skip]
fn cref_format_arity(name: &str) -> Option<usize> {
    Some(match name {
        "crefname" | "Crefname" => 3,
        "crefmultiformat" | "Crefmultiformat" | "crefrangemultiformat" | "Crefrangemultiformat" => 5,
        "crefformat" | "Crefformat" | "crefrangeformat" | "Crefrangeformat" | "creflabelformat"
        | "crefeqlabelformat" => 2,
        _ => return None,
    })
}

/// Boxed or looped material may print a graphic elsewhere or repeatedly.
#[rustfmt::skip]
fn boxes_or_loops(name: &str) -> bool {
    matches!(
        name,
        "csname" | "savebox" | "sbox" | "setbox" | "usebox" | "phantom" | "hphantom" | "vphantom"
        | "multiput" | "foreach"
    )
}

/// Sectioning commands that start a heading.
#[rustfmt::skip]
fn sectioning_command(name: &str) -> bool {
    matches!(
        name,
        "part" | "chapter" | "section" | "subsection" | "subsubsection" | "paragraph"
        | "subparagraph"
    )
}

/// Font and size switches, which format what follows without printing.
#[rustfmt::skip]
pub(super) fn known_formatting_switch(name: &str) -> bool {
    crate::symbols::is_font_switch(name)
        || matches!(
            name,
            "tiny" | "scriptsize" | "footnotesize" | "small" | "normalsize" | "large" | "Large"
            | "LARGE" | "huge" | "Huge"
        )
}

/// Whether `byte` lies in one of the sorted, disjoint `ranges`.
pub(crate) fn contains_byte(ranges: &[Range<usize>], byte: usize) -> bool {
    let index = ranges.partition_point(|range| range.start <= byte);
    index > 0 && ranges[index - 1].contains(&byte)
}

fn merge_hidden_ranges(ranges: &mut Vec<Range<usize>>) {
    ranges.sort_unstable_by_key(|range| (range.start, range.end));
    let mut count = 0;
    for index in 0..ranges.len() {
        if count > 0 && ranges[index].start <= ranges[count - 1].end {
            ranges[count - 1].end = ranges[count - 1].end.max(ranges[index].end);
        } else {
            ranges[count] = ranges[index].clone();
            count += 1;
        }
    }
    ranges.truncate(count);
}

/// `(hidden, opaque)` for the lines or execution scope: hidden merges the
/// mode's nonprinting spans with discarded and note-owned arguments.
#[allow(clippy::type_complexity)]
fn discarded_macro_arg_ranges(
    analysis: &SourceAnalysis<'_>,
    lines: &Range<usize>,
    mode: WordLocationMode,
    execution_scope: Option<&Range<usize>>,
) -> Option<(Vec<Range<usize>>, Vec<Range<usize>>)> {
    analysis.operation.check().ok()?;
    let (start_byte, end_byte) = if let Some(scope) = execution_scope {
        (scope.start, scope.end)
    } else {
        analysis.line_byte_range(lines)
    };
    let mode_spans = analysis.mode_spans(mode);
    let (discarded, opaque) = discarded_macro_args(
        analysis.source,
        false,
        start_byte..end_byte,
        analysis.macros(mode),
        Some(analysis.environments()),
        Some(mode_spans),
        analysis.operation,
    )?;
    let mut hidden: Vec<_> = mode_spans
        .iter()
        .map(|(_, range, _)| range.clone())
        .chain(discarded)
        .chain(opaque.iter().cloned())
        .collect();
    merge_hidden_ranges(&mut hidden);
    Some((hidden, opaque))
}

/// Commands whose argument TeX measures, spaces, or indexes but never prints.
fn discarding_command(name: &str) -> bool {
    matches!(
        name,
        "phantom" | "hphantom" | "vphantom" | "hspace" | "vspace" | "index"
    )
}

/// End of a discarding command's argument after its name ends at `at`.
fn discarded_argument_end(
    source: &str,
    at: usize,
    limit: usize,
    parameters: bool,
) -> Option<usize> {
    let mut at = at + usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
    while let Some(end) =
        optional_argument_end(source, at).or_else(|| overlay_argument_end(source, at))
    {
        at = end;
    }
    note_argument(source, at, limit, parameters).map(|(_, end)| end)
}

fn note_argument(
    source: &str,
    mut at: usize,
    limit: usize,
    parameters: bool,
) -> Option<(Range<usize>, usize)> {
    loop {
        let (arg, end) = math::argument(source, at, limit, parameters)?;
        if end == arg.end {
            match source.as_bytes().get(arg.start) {
                Some(b'[') => {
                    at = optional_argument_end(source, arg.start)?;
                    continue;
                }
                Some(b'<') => {
                    at = overlay_argument_end(source, arg.start)?;
                    continue;
                }
                Some(b'#')
                    if parameters
                        && matches!(
                            source.as_bytes().get(skip_source_space(source, end)),
                            Some(b'{' | b'[' | b'<')
                        ) =>
                {
                    at = end;
                    continue;
                }
                _ => {}
            }
        }
        return Some((arg, end));
    }
}

/// End of a call's next argument from `at`: `None` when a mandatory one is
/// missing, `Some(None)` when an optional `[...]` one is omitted.
fn macro_argument_end(
    source: &str,
    at: usize,
    optional: bool,
    limit: usize,
    parameters: bool,
) -> Option<Option<usize>> {
    if optional {
        return Some(optional_argument_end(source, at));
    }
    math::argument(source, at, limit, parameters).map(|(_, end)| Some(end))
}

/// One pass over `range` returning `(hidden, opaque)`. Arguments TeX discards
/// are hidden; `\note` calls and the unprinted arguments of note-calling
/// wrappers and environments are also opaque (note-owned).
#[allow(clippy::type_complexity)]
pub(crate) fn discarded_macro_args(
    source: &str,
    parameters: bool,
    range: Range<usize>,
    macros: &MacroTable<'_>,
    environments: Option<&EnvTable<'_>>,
    known_spans: Option<&[Span]>,
    operation: &crate::Operation,
) -> Option<(Vec<Range<usize>>, Vec<Range<usize>>)> {
    operation.check().ok()?;
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut opaque: Vec<Range<usize>> = Vec::new();
    let has_unused = |arity: u8, used: u16| (0..arity).any(|idx| used & (1 << idx) == 0);
    if !source.contains('\\')
        || (known_spans.is_some()
            && !source.contains("phantom")
            && !source.contains("hspace")
            && !source.contains("vspace")
            && !source.contains("index")
            && !source.contains("\\note")
            && !macros
                .values()
                .flatten()
                .any(|def| def.calls_note || has_unused(def.arity, def.used_params))
            && !environments.is_some_and(|environments| {
                environments.values().any(|env| {
                    env.calls_note
                        || env
                            .header
                            .is_some_and(|(arity, _, used)| has_unused(arity, used))
                })
            }))
    {
        return Some((ranges, opaque));
    }
    let computed: Vec<Span>;
    let spans: &[Span] = if let Some(spans) = known_spans {
        spans
    } else {
        computed = nonprinting_source_spans_with_mode(
            source,
            Some(macros),
            None,
            WordLocationMode::Strict,
            Some(operation),
        )
        .collect();
        &computed
    };
    let mut hidden = spans.iter().peekable();
    // Unused arguments may be discovered inside a visible earlier argument.
    // Keep future exclusions ordered without rescanning all preceding ranges.
    let mut pending_hidden = std::collections::BinaryHeap::new();
    let mut at = 0;
    let limit = range.end.min(source.len());
    let source = &source[..limit];
    while at < limit {
        operation.check().ok()?;
        while let Some(&std::cmp::Reverse((start, end))) = pending_hidden.peek()
            && start <= at
        {
            pending_hidden.pop();
            at = at.max(end);
        }
        if at >= limit {
            break;
        }
        while hidden.peek().is_some_and(|(_, span, _)| span.end <= at) {
            hidden.next();
        }
        if let Some((_, span, _)) = hidden.peek()
            && span.contains(&at)
        {
            at = span.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => at = comment_end(source, at),
            b'\\' => {
                let start = at;
                let (cmd, end) = math::command(source, at);
                at = end;
                if !macros.contains_key(cmd)
                    && let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, at)
                {
                    at = opaque_end;
                    continue;
                }
                if !range.contains(&start) {
                    continue;
                }
                if discarding_command(cmd) && !macros.contains_key(cmd) {
                    if let Some(arg_end) = discarded_argument_end(source, at, limit, parameters) {
                        ranges.push(start..arg_end);
                        at = arg_end;
                    }
                    continue;
                }
                if cmd == "note" && !macros.contains_key("note") {
                    if let Some((arg, arg_end)) = note_argument(source, at, limit, parameters) {
                        ranges.push(start..arg_end);
                        opaque.push(arg);
                        at = arg_end;
                    }
                    continue;
                }
                // A user macro or environment with `(arity, opt_params, used_params, calls_note)`.
                let (signature, mut arg_at) = if let Some(Some(def)) = macros.get(cmd)
                    && (def.arity > 0 || def.calls_note)
                {
                    (
                        (def.arity, def.opt_params, def.used_params, def.calls_note),
                        at,
                    )
                } else if cmd == "begin"
                    && !macros.contains_key("begin")
                    && let Some((name, arg_at)) = math::group(source, at)
                    && let Some(env) =
                        environments.and_then(|environments| environments.get(source[name].trim()))
                {
                    let (arity, opt_params, used) = env.header.unwrap_or_default();
                    ((arity, opt_params, used, env.calls_note), arg_at)
                } else {
                    continue;
                };
                let (arity, opt_params, used_params, calls_note) = signature;
                for idx in 0..arity {
                    while let Some(end) = overlay_argument_end(source, arg_at) {
                        ranges.push(arg_at..end);
                        arg_at = end;
                    }
                    let Some(arg_end) = macro_argument_end(
                        source,
                        arg_at,
                        opt_params & (1 << idx) != 0,
                        limit,
                        parameters,
                    ) else {
                        break;
                    };
                    let Some(arg_end) = arg_end else {
                        continue;
                    };
                    if used_params & (1 << idx) == 0 {
                        ranges.push(arg_at..arg_end);
                        pending_hidden.push(std::cmp::Reverse((arg_at, arg_end)));
                        if calls_note {
                            opaque.push(arg_at..arg_end);
                        }
                    }
                    arg_at = arg_end;
                }
            }
            _ => {
                at += source.as_bytes()[at + 1..limit]
                    .iter()
                    .position(|byte| matches!(byte, b'%' | b'\\'))
                    .map_or(limit - at, |offset| offset + 1);
            }
        }
    }
    Some((ranges, opaque))
}

pub(crate) fn source_word_location_in_frame_body_with_analysis(
    analysis: &SourceAnalysis<'_>,
    line: u32,
    (context, offset): (&str, usize),
) -> WordOutcome {
    let source = analysis.source;
    let operation = analysis.operation;
    if operation.check().is_err() {
        return WordOutcome::Opaque;
    }
    let Some(anchor) = line.checked_sub(1).map(|l| l as usize) else {
        return WordOutcome::Opaque;
    };
    let Some(frame) =
        environment_bounds_with_ignored(source, anchor, "frame", &analysis.strict_ignored)
    else {
        return WordOutcome::Opaque;
    };
    let Some(header_start) = frame_header_after_begin(
        source,
        analysis.line_starts.row_start(frame.start, source.len()),
    ) else {
        return WordOutcome::Opaque;
    };
    let mut body_start = skip_nonprinting_environment_arguments(source, "frame", header_start);
    for _ in 0..2 {
        if operation.check().is_err() {
            return WordOutcome::Opaque;
        }
        let Some(at) = skip_header_space(source, body_start) else {
            break;
        };
        if source.as_bytes().get(at) != Some(&b'{') {
            break;
        }
        let Some((_, end)) = math::group(source, at) else {
            return WordOutcome::Opaque;
        };
        body_start = end;
    }
    let Some(end_row_idx) = frame.end.checked_sub(1) else {
        return WordOutcome::Opaque;
    };
    let end_row_start = *analysis
        .line_starts
        .get(end_row_idx)
        .unwrap_or(&source.len());
    let Some(raw_end_row) = source[end_row_start..].split('\n').next() else {
        return WordOutcome::Opaque;
    };
    let end_row = source_line_text(raw_end_row);
    let Some(body_end) = end_row
        .match_indices("\\end")
        .filter(|(at, _)| {
            !has_unescaped_backslash_before(end_row, *at)
                && !contains_byte(&analysis.strict_ignored, end_row_start + at)
        })
        .find_map(|(at, _)| {
            let start = end_row_start + at;
            let (env, _) = math::group(source, start + "\\end".len())?;
            (&source[env] == "frame").then_some(start)
        })
    else {
        return WordOutcome::Opaque;
    };
    if body_start > body_end {
        return WordOutcome::Opaque;
    }
    source_word_location_in_range_with_analysis(
        analysis,
        line,
        (context, offset),
        frame,
        true,
        WordLocationMode::Strict,
        Some(&(body_start..body_end)),
    )
}

pub(crate) fn source_word_location_in_range_with_analysis(
    analysis: &SourceAnalysis<'_>,
    line: u32,
    (context, offset): (&str, usize),
    lines: Range<usize>,
    within_scope: bool,
    mode: WordLocationMode,
    execution_scope: Option<&Range<usize>>,
) -> WordOutcome {
    let source = analysis.source;
    let operation = analysis.operation;
    if operation.check().is_err() {
        return WordOutcome::Opaque;
    }
    let Some((mut hidden, opaque)) =
        discarded_macro_arg_ranges(analysis, &lines, mode, execution_scope)
    else {
        return WordOutcome::Opaque;
    };
    // Note-owned words and formulas veto any visible competitor; the note's
    // own text stays hidden from the visible candidates below.
    let macros = analysis.macros(mode);
    let mode_spans = analysis.mode_spans(mode);
    let context_words = words(context);
    let target = word_index_at(context, &context_words, offset)
        .map(|selected| normalized_word(context_words[selected].1));
    if opaque.iter().any(|range| {
        let text = &source[range.clone()];
        target.as_ref().is_some_and(|target| {
            let mut row_offset = range.start;
            text.split_inclusive('\n').any(|raw_row| {
                let row = source_line_text(raw_row);
                let found = source_words(row).into_iter().any(|(byte, _, word)| {
                    let abs = row_offset + byte;
                    let span_idx = mode_spans.partition_point(|(_, span, _)| span.start <= abs);
                    !has_unescaped_backslash_before(row, byte)
                        && (span_idx == 0 || !mode_spans[span_idx - 1].1.contains(&abs))
                        && &word == target
                });
                row_offset += raw_row.len();
                found
            })
        }) || math::text_math_renders_word(text, macros, operation, (context, offset))
    }) {
        return WordOutcome::Opaque;
    }
    if let Some(scope) = execution_scope {
        hidden.extend([0..scope.start, scope.end..source.len()]);
        merge_hidden_ranges(&mut hidden);
    }
    // A closing layout environment can alias every row to one source line.
    // A search radius alone does not identify neighboring prose or math;
    // independently established frame/caption and math scopes remain eligible.
    let closing_row = analysis
        .line_starts
        .row_start(line.saturating_sub(1) as usize, source.len());
    let closing_layout = source[closing_row..]
        .lines()
        .next()
        .map(source_line_text)
        .and_then(|text| text.trim_start().strip_prefix("\\end"))
        .and_then(|rest| math::group(rest, 0).map(|(name, _)| !math::math_environment(&rest[name])))
        .unwrap_or(false);
    if !within_scope && closing_layout {
        return WordOutcome::Absent;
    }
    let loops = foreach_loops(analysis, &lines);
    let (_, range_end_byte) = analysis.line_byte_range(&lines);
    let all_math = analysis.math_regions();
    let math_regions: Vec<_> = all_math[..all_math.partition_point(|r| r.start < range_end_byte)]
        .iter()
        .filter(|range| {
            let index = hidden.partition_point(|hidden| hidden.start <= range.start);
            index == 0 || hidden[index - 1].end < range.end
        })
        .cloned()
        .collect();
    let scope = WordRangeScope {
        line,
        lines,
        foreach_loops: &loops,
        execution_scope,
    };
    let prose = source_prose_scored_location_with_mode(
        analysis,
        &scope,
        within_scope,
        (context, offset),
        (&hidden, None),
        &math_regions,
        mode,
    );
    if matches!(prose, ProseRank::Opaque) {
        return WordOutcome::Opaque;
    }
    let math_outcome = math::source_location_with_context_mode(
        analysis,
        &scope,
        within_scope,
        (context, offset),
        prose,
        &math_regions,
        mode,
    );
    let mut candidate = match math_outcome {
        math::MathOutcome::Opaque => return WordOutcome::Opaque,
        math::MathOutcome::Absent => None,
        math::MathOutcome::Located(row, byte) => Some((row, byte)),
    }
    .and_then(|(row, byte)| {
        if contains_byte(&hidden, analysis.line_starts[row as usize - 1] + byte) {
            return None;
        }
        if !within_scope && row != line && crosses_paragraph_boundary(analysis, line, row) {
            return None;
        }
        ordinary_word_source_start(analysis, (context, offset), row, byte).map(|start| (row, start))
    });
    let (prose_score, prose_tied) = match prose {
        ProseRank::Absent | ProseRank::Opaque => (0, false),
        ProseRank::Unique(_, _, score) => (score, false),
        ProseRank::Tied(score) => (score, true),
    };
    match source_reference_argument_location(
        analysis,
        &scope,
        within_scope,
        (context, offset),
        &hidden,
        mode,
    ) {
        RefOutcome::Located(ref_row, ref_byte, ref_score, ref_both_sides)
            if (candidate.is_none() && !prose_tied)
                || (prose_score == 0
                    && (candidate.is_some_and(|(row, _)| row != line)
                        || (prose_tied && candidate.is_none()))
                    && ref_score > 0)
                || (ref_both_sides && ref_score > prose_score) =>
        {
            candidate = Some((ref_row, ref_byte));
        }
        RefOutcome::Located(..) | RefOutcome::Absent => {}
        RefOutcome::Opaque => {
            if operation.check().is_err() || candidate.is_none() {
                return WordOutcome::Opaque;
            }
        }
    }
    match candidate {
        Some((row, byte)) => WordOutcome::Located(row, byte),
        None if prose_tied => WordOutcome::Tied,
        None => WordOutcome::Absent,
    }
}

/// Commands whose one braced argument is a label key printed as a generated reference.
const REFERENCE_COMMANDS: [&str; 6] = ["ref", "eqref", "pageref", "autoref", "cref", "Cref"];

pub(crate) fn is_direct_reference_command(name: &str) -> bool {
    REFERENCE_COMMANDS.contains(&name)
}

/// Whether `text` spells any `REFERENCE_COMMANDS` control sequence (or a longer one it prefixes).
fn mentions_reference_command(text: &str) -> bool {
    text.split('\\')
        .skip(1)
        .any(|rest| REFERENCE_COMMANDS.iter().any(|name| rest.starts_with(name)))
}

fn valid_reference_label_start(source: &str, span: &Range<usize>) -> Option<(usize, Vec<String>)> {
    if span.start >= span.end || span.end > source.len() {
        return None;
    }
    let label_start = skip_source_space(&source[..span.end], span.start);
    if label_start >= span.end {
        return None;
    }
    let mut visible = String::with_capacity(span.end - label_start);
    let mut at = label_start;
    while at < span.end {
        match source.as_bytes()[at] {
            b'%' => {
                at += source[at..span.end]
                    .find('\n')
                    .map_or(span.end - at, |p| p + 1);
            }
            b'\\' | b'{' | b'}' | b'#' => return None,
            _ => {
                let ch = source[at..span.end].chars().next()?;
                visible.push(ch);
                at += ch.len_utf8();
            }
        }
    }
    if visible.split(',').any(|item| item.trim().is_empty()) {
        return None;
    }
    let key_items = visible
        .split(',')
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty())
        .collect();
    Some((label_start, key_items))
}

fn reference_argument_body_at_with_spans(
    source: &str,
    target: usize,
    known_spans: &[Span],
) -> Option<Range<usize>> {
    if target > source.len() || !mentions_reference_command(source) {
        return None;
    }
    for &(cmd_start, ref span, _) in known_spans {
        if cmd_start > target {
            break;
        }
        let (name, _) = math::command(source, cmd_start);
        if is_direct_reference_command(name)
            && span.start > cmd_start
            && source.as_bytes().get(span.start - 1) == Some(&b'{')
            && source.as_bytes().get(span.end) == Some(&b'}')
            && (cmd_start..=span.end).contains(&target)
            && valid_reference_label_start(source, span).is_some()
        {
            return Some(span.start - 1..span.end + 1);
        }
    }
    None
}

#[rustfmt::skip]
fn is_transparent_reference_control(name: &str) -> bool {
    text_style_wrapper(name)
        || sectioning_command(name)
        || known_formatting_switch(name)
        || matches!(
            name,
            "textcolor" | "hbox" | "vbox" | "vtop" | "vcenter" | "footnote" | "caption"
            | "frametitle" | "framesubtitle" | "title" | "subtitle" | "author" | "date"
            | "institute" | "label" | "smallskip" | "medskip" | "bigskip" | "noindent"
            | "centering" | "raggedright" | "raggedleft" | "pause" | "begingroup" | "endgroup"
            | "bgroup" | "egroup" | "allowbreak" | "linebreak" | "vfill" | "hfill" | "quad"
            | "qquad" | "thinspace" | "enspace" | "slash" | "href"
        )
}

#[rustfmt::skip]
fn command_takes_optional_argument(name: &str) -> bool {
    !known_formatting_switch(name)
        && !is_direct_reference_command(name)
        && !matches!(
            name,
            "begin" | "end" | "label" | "noindent" | "par" | "centering" | "raggedright"
            | "raggedleft" | "smallskip" | "medskip" | "bigskip" | "vfill" | "hfill" | "quad"
            | "qquad" | "thinspace" | "enspace" | "slash" | "begingroup" | "endgroup" | "bgroup"
            | "egroup" | "allowbreak" | "maketitle" | "titlepage" | "clearpage"
            | "cleardoublepage" | "newpage"
        )
}

fn skip_optional_and_overlay_arguments(source: &str, mut at: usize) -> usize {
    while let Some(end) = header_optional_argument(source, at)
        .map(|h| h.end)
        .or_else(|| {
            skip_header_space(source, at).and_then(|next| overlay_argument_end(source, next))
        })
    {
        at = end;
    }
    at
}

fn skip_required_groups(source: &str, mut at: usize) -> usize {
    while let Some((_, end)) = math::group(source, at) {
        at = end;
    }
    at
}

// Scanning continues inside a printed header, recording the `]` that ends its label.
fn enter_printed_header(source: &str, at: usize, bracket_ends: &mut Vec<usize>) -> usize {
    let Some(header) = header_optional_argument(source, at) else {
        return at;
    };
    bracket_ends.push(header.end - 1);
    header.start + 1
}

#[derive(Clone, Copy)]
enum RefOutcome {
    Located(u32, usize, isize, bool),
    Absent,
    Opaque,
}

fn source_reference_argument_location(
    analysis: &SourceAnalysis<'_>,
    scope: &WordRangeScope<'_>,
    within_scope: bool,
    (context, offset): (&str, usize),
    hidden: &[Range<usize>],
    mode: WordLocationMode,
) -> RefOutcome {
    let (source, operation) = (analysis.source, analysis.operation);
    let (line, lines) = (scope.line, &scope.lines);
    if operation.check().is_err() {
        return RefOutcome::Opaque;
    }
    if !scope.foreach_loops.is_empty() {
        return RefOutcome::Absent;
    }
    let raw_pdf = words(context);
    let Some(selected) = word_index_at(context, &raw_pdf, offset).or_else(|| {
        match context.as_bytes().get(offset).copied() {
            Some(b'(' | b'[') => word_index_at(context, &raw_pdf, offset + 1),
            Some(b')' | b']') if offset > 0 => word_index_at(context, &raw_pdf, offset - 1),
            _ => None,
        }
    }) else {
        return RefOutcome::Absent;
    };
    let pdf: Vec<String> = raw_pdf
        .iter()
        .map(|(_, word)| normalized_word(word))
        .collect();
    if pdf.get(selected).is_none_or(String::is_empty) {
        return RefOutcome::Absent;
    }
    let Some(anchor) = line.checked_sub(1).and_then(|l| usize::try_from(l).ok()) else {
        return RefOutcome::Opaque;
    };
    let line_starts = &analysis.line_starts;
    if line_starts.is_empty() {
        return RefOutcome::Absent;
    }
    let (scope_start, scope_end) = analysis.line_byte_range(lines);
    if scope_start >= scope_end {
        return RefOutcome::Absent;
    }
    let stream_lines = if within_scope {
        lines.clone()
    } else {
        lines.start.min(anchor.saturating_sub(4))
            ..lines
                .end
                .max(anchor.saturating_add(5))
                .min(line_starts.len())
    };
    let (stream_start, stream_end) = analysis.line_byte_range(&stream_lines);
    if !mentions_reference_command(&source[stream_start..stream_end]) {
        return RefOutcome::Absent;
    }
    let macros = analysis.macros(mode);

    #[derive(Clone)]
    struct RefCandidate {
        cmd_start: usize,
        full_end: usize,
        label_line: u32,
        label_byte: usize,
        start_line: usize,
        end_line: usize,
        key_words: Vec<String>,
        footnote_body: Option<Range<usize>>,
    }

    let mut span_start = stream_start;
    let mut span_end = stream_end;
    let mut footnote_bodies = Vec::new();
    let mut non_transparent_groups = Vec::new();
    let mut item_labels = Vec::new();
    let mut scan = 0usize;
    while scan < stream_end {
        if operation.check().is_err() {
            return RefOutcome::Opaque;
        }
        match source.as_bytes()[scan] {
            b'%' => {
                scan += source[scan..stream_end]
                    .find('\n')
                    .map_or(stream_end - scan, |p| p + 1);
            }
            b'\\' => {
                let cmd_pos = scan;
                let (cmd, next) = math::command(source, scan);
                scan = next;
                if !cmd
                    .as_bytes()
                    .first()
                    .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == b'@')
                {
                    continue;
                }
                if let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, scan) {
                    if opaque_end > stream_start {
                        non_transparent_groups.push(scan..opaque_end);
                    }
                    scan = opaque_end;
                    continue;
                }
                // An owning command on an earlier line still prefixes a header inside the stream.
                let overridden = macros.contains_key(cmd);
                if !overridden
                    && let Some(end) = math::printed_header_argument(source, cmd, scan)
                        .map(|h| h.end)
                        .or_else(|| {
                            math::collected_text_arguments(source, cmd, scan).map(|(_, _, end)| end)
                        })
                    && end > stream_start
                {
                    span_start = span_start.min(cmd_pos);
                }
                let mut arg_at =
                    scan + usize::from(matches!(source.as_bytes().get(scan), Some(b'*' | b'+')));
                if overridden || command_takes_optional_argument(cmd) {
                    while let Some(opt_end) = header_optional_argument(source, arg_at)
                        .map(|h| h.end)
                        .or_else(|| {
                            skip_header_space(source, arg_at)
                                .and_then(|next| overlay_argument_end(source, next))
                        })
                    {
                        if opt_end > stream_start {
                            // A printed label keeps its references eligible but stays page-opaque.
                            if cmd == "item" && !overridden {
                                item_labels.push(arg_at..opt_end);
                            } else {
                                non_transparent_groups.push(arg_at..opt_end);
                            }
                        }
                        arg_at = opt_end;
                    }
                } else {
                    arg_at = skip_overlay_arguments(source, arg_at);
                }
                if !overridden
                    && cmd == "begin"
                    && let Some((body, group_end)) = math::group(source, arg_at)
                {
                    let env = source[body].trim();
                    if let Some(header) = math::printed_header_argument(source, cmd, scan) {
                        if header.end > stream_start {
                            item_labels.push(header);
                        }
                    } else {
                        let opt_end =
                            skip_nonprinting_environment_arguments(source, env, group_end);
                        if opt_end > group_end && opt_end > stream_start {
                            non_transparent_groups.push(group_end..opt_end);
                        }
                    }
                }
                // An override may print around its argument, so no built-in role survives it.
                if !overridden
                    && cmd == "footnote"
                    && let Some((body, _)) = math::group(source, arg_at)
                {
                    if body.end > stream_start {
                        let fn_line = line_starts.row_of(scan);
                        span_start = span_start.min(line_starts[fn_line]).min(body.start);
                        span_end = span_end.max(body.end);
                        footnote_bodies.push(body);
                    }
                } else if !is_direct_reference_command(cmd)
                    && (overridden
                        || (!is_transparent_reference_control(cmd)
                            && !generated_layout_command(cmd)))
                    && let Some((body, end)) = math::group(source, arg_at)
                    && end > stream_start
                {
                    non_transparent_groups.push(body.start.saturating_sub(1)..end);
                }
            }
            _ => {
                // Only comments and commands matter here; jump straight to the next one.
                scan = source.as_bytes()[scan..stream_end]
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'\\'))
                    .map_or(stream_end, |p| scan + p);
            }
        }
    }

    let mut refs = Vec::new();
    let mut all_spans = Vec::new();
    let mode_spans = analysis.mode_spans(mode);
    let first_span = mode_spans.partition_point(|(_, span, _)| span.end <= span_start);
    let last_span = mode_spans.partition_point(|(cmd_start, _, _)| *cmd_start < span_end);
    let mode_spans = &mode_spans[first_span..last_span.max(first_span)];
    // Binary searches over `refs` and `all_spans` rely on sorted, disjoint spans.
    debug_assert!(
        mode_spans
            .windows(2)
            .all(|w| w[0].0 < w[1].0 && w[0].1.end <= w[1].1.start)
    );
    for &(cmd_start, ref span, _) in mode_spans {
        all_spans.push((cmd_start, span.clone()));
        let (name, _) = math::command(source, cmd_start);
        if !is_direct_reference_command(name)
            || macros.contains_key(name)
            || span.start <= cmd_start
            || source.as_bytes().get(span.start - 1) != Some(&b'{')
            || source.as_bytes().get(span.end) != Some(&b'}')
        {
            continue;
        }
        let full_end = span.end + 1;
        if non_transparent_groups
            .iter()
            .any(|g| g.start <= cmd_start && full_end <= g.end)
        {
            continue;
        }
        let Some((label_start, key_words)) = valid_reference_label_start(source, span) else {
            continue;
        };
        let start_line = line_starts.row_of(cmd_start);
        let end_line = line_starts
            .partition_point(|&s| s < full_end)
            .saturating_sub(1);
        let in_scope = if within_scope {
            lines.contains(&start_line) && lines.contains(&end_line)
        } else {
            (start_line..=end_line).contains(&anchor)
        };
        if !in_scope {
            continue;
        }
        let label_line_idx = line_starts.row_of(label_start);
        if hidden
            .iter()
            .any(|r| r.contains(&label_start) && !(r.start == span.start && r.end == span.end))
        {
            continue;
        }
        let footnote_body = footnote_bodies
            .iter()
            .find(|b| b.start <= cmd_start && full_end <= b.end)
            .cloned();
        refs.push(RefCandidate {
            cmd_start,
            full_end,
            label_line: label_line_idx as u32 + 1,
            label_byte: label_start - line_starts[label_line_idx],
            start_line,
            end_line,
            key_words,
            footnote_body,
        });
    }
    if refs.is_empty() {
        return RefOutcome::Absent;
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum StreamItem {
        Word(String),
        Ref(usize),
        OtherGenerated,
        Boundary,
        PageBreak(usize, usize, bool),
    }

    // Each stream records where it emitted every reference, so candidates never rescan it.
    let build_stream = |region: Range<usize>,
                        inside_footnote: bool|
     -> Option<(Vec<StreamItem>, Vec<Option<usize>>)> {
        let mut items = Vec::new();
        let mut ref_positions = vec![None; refs.len()];
        let mut bracket_ends = Vec::new();
        if inside_footnote {
            items.push(StreamItem::Boundary);
            items.push(StreamItem::OtherGenerated);
        }
        let mut at = region.start;
        while at < region.end {
            operation.check().ok()?;
            if !inside_footnote
                && let Some(fn_body) = footnote_bodies
                    .iter()
                    .find(|b| b.start.saturating_sub(1) <= at && at <= b.end)
            {
                items.push(StreamItem::OtherGenerated);
                at = (fn_body.end + 1).min(region.end);
                continue;
            }
            match source.as_bytes()[at] {
                b'%' => {
                    at += source[at..region.end]
                        .find('\n')
                        .map_or(region.end - at, |p| p + 1);
                }
                b'\n' => {
                    let next_line_end = source[at + 1..region.end]
                        .find('\n')
                        .map_or(region.end, |p| at + 1 + p);
                    if source[at + 1..next_line_end].trim().is_empty() {
                        items.push(StreamItem::Boundary);
                    }
                    at += 1;
                }
                b'&' => {
                    items.push(StreamItem::Boundary);
                    at += 1;
                }
                b'$' => {
                    items.push(StreamItem::OtherGenerated);
                    let delim = if source.as_bytes().get(at + 1) == Some(&b'$') {
                        2
                    } else {
                        1
                    };
                    at += delim;
                    while at < region.end {
                        if source.as_bytes()[at] == b'\\' {
                            at += 1 + source[at + 1..region.end]
                                .chars()
                                .next()
                                .map_or(0, char::len_utf8);
                        } else if source.as_bytes()[at] == b'%' {
                            at += source[at..region.end]
                                .find('\n')
                                .map_or(region.end - at, |p| p + 1);
                        } else if source.as_bytes()[at] == b'$' {
                            at += if delim == 2 && source.as_bytes().get(at + 1) == Some(&b'$') {
                                2
                            } else {
                                1
                            };
                            break;
                        } else {
                            at = source.as_bytes()[at..region.end]
                                .iter()
                                .position(|&b| matches!(b, b'\\' | b'%' | b'$'))
                                .map_or(region.end, |p| at + p);
                        }
                    }
                }
                b'\\' => {
                    // Every reference candidate starts at its command's backslash.
                    if let Ok(ref_idx) = refs.binary_search_by_key(&at, |r| r.cmd_start)
                        && refs[ref_idx].footnote_body.is_some() == inside_footnote
                    {
                        ref_positions[ref_idx] = Some(items.len());
                        items.push(StreamItem::Ref(ref_idx));
                        at = refs[ref_idx].full_end.min(region.end);
                        continue;
                    }
                    if source.as_bytes().get(at + 1) == Some(&b'\\') {
                        items.push(StreamItem::Boundary);
                        at += 2;
                        if let Some(opt_end) = optional_argument_end(source, at) {
                            at = opt_end;
                        }
                        continue;
                    }
                    if matches!(source.as_bytes().get(at + 1), Some(b'(' | b'[')) {
                        let close = if source.as_bytes()[at + 1] == b'(' {
                            "\\)"
                        } else {
                            "\\]"
                        };
                        items.push(StreamItem::OtherGenerated);
                        at = source[at + 2..region.end]
                            .find(close)
                            .map_or(region.end, |p| at + 2 + p + 2);
                        continue;
                    }
                    let cmd_pos = at;
                    let cmd_line = line_starts.row_of(cmd_pos);
                    let (cmd, next) = math::command(source, at);
                    at = next;
                    if !cmd
                        .as_bytes()
                        .first()
                        .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == b'@')
                    {
                        continue;
                    }
                    if let Some((opaque_end, nonprinting)) =
                        math::opaque_command_end(source, cmd, at)
                    {
                        if !nonprinting {
                            items.push(StreamItem::OtherGenerated);
                        }
                        at = opaque_end.min(region.end);
                        continue;
                    }
                    let in_non_transparent = || {
                        non_transparent_groups.iter().any(|g| g.contains(&cmd_pos))
                            || item_labels.iter().any(|g| g.contains(&cmd_pos))
                    };
                    let mut arg_at =
                        at + usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
                    if macros.contains_key(cmd) {
                        // An override may print anything, whatever the built-in does.
                        items.push(StreamItem::OtherGenerated);
                        arg_at = skip_optional_and_overlay_arguments(source, arg_at);
                        arg_at = skip_required_groups(source, arg_at);
                        at = arg_at.min(region.end);
                        continue;
                    }
                    if cmd == "item" {
                        arg_at = skip_overlay_arguments(source, arg_at);
                        // A bracketed label replaces the generated bullet or number.
                        items.push(if header_optional_argument(source, arg_at).is_some() {
                            StreamItem::Boundary
                        } else {
                            StreamItem::OtherGenerated
                        });
                        at =
                            enter_printed_header(source, arg_at, &mut bracket_ends).min(region.end);
                        continue;
                    }
                    arg_at = if command_takes_optional_argument(cmd) {
                        skip_optional_and_overlay_arguments(source, arg_at)
                    } else {
                        skip_overlay_arguments(source, arg_at)
                    };
                    if matches!(cmd, "begin" | "end") {
                        if let Some((body, mut group_end)) = math::group(source, arg_at) {
                            let env = source[body].trim();
                            let page_break =
                                matches!(env, "document" | "frame") && !in_non_transparent();
                            if page_break {
                                items.push(StreamItem::PageBreak(cmd_line, cmd_pos, true));
                            }
                            if cmd == "end" {
                                if !page_break {
                                    items.push(StreamItem::Boundary);
                                }
                                at = group_end.min(region.end);
                                continue;
                            }
                            let kind = classify_environment_header(env);
                            match kind {
                                EnvHeaderKind::PrintedReplacement | EnvHeaderKind::PrintedNote => {
                                    group_end = skip_overlay_arguments(source, group_end);
                                    // `amsthm` proof optional header replaces the generated "Proof" word.
                                    let replaces_label = kind == EnvHeaderKind::PrintedReplacement
                                        && header_optional_argument(source, group_end).is_some();
                                    items.push(if replaces_label {
                                        StreamItem::Boundary
                                    } else {
                                        StreamItem::OtherGenerated
                                    });
                                    at = enter_printed_header(source, group_end, &mut bracket_ends)
                                        .min(region.end);
                                    continue;
                                }
                                EnvHeaderKind::NoPrintedHeader => {
                                    if !page_break {
                                        items.push(StreamItem::Boundary);
                                    }
                                    group_end = skip_nonprinting_environment_arguments(
                                        source, env, group_end,
                                    );
                                }
                            }
                            at = group_end.min(region.end);
                            continue;
                        }
                        items.push(StreamItem::Boundary);
                        at = arg_at.min(region.end);
                        continue;
                    }
                    if matches!(
                        cmd,
                        "clearpage" | "cleardoublepage" | "newpage" | "pagebreak"
                    ) {
                        if !in_non_transparent() {
                            items.push(StreamItem::PageBreak(
                                cmd_line,
                                cmd_pos,
                                matches!(cmd, "clearpage" | "cleardoublepage"),
                            ));
                        }
                        at = arg_at.min(region.end);
                        continue;
                    }
                    if cmd == "par" {
                        items.push(StreamItem::Boundary);
                        at = arg_at.min(region.end);
                        continue;
                    }
                    if generated_heading_command(cmd) {
                        items.push(StreamItem::OtherGenerated);
                    }
                    if !inside_footnote
                        && cmd == "footnote"
                        && let Some((body, end)) = math::group(source, arg_at)
                        && footnote_bodies.contains(&body)
                    {
                        items.push(StreamItem::OtherGenerated);
                        at = end.min(region.end);
                        continue;
                    }
                    if let Ok(span_idx) = all_spans.binary_search_by_key(&cmd_pos, |(s, _)| *s) {
                        let span = &all_spans[span_idx].1;
                        if is_direct_reference_command(cmd) || generated_text_command(cmd) {
                            items.push(StreamItem::OtherGenerated);
                        }
                        let span_after = if source.as_bytes().get(span.end) == Some(&b'}') {
                            span.end + 1
                        } else {
                            span.end
                        };
                        at = span_after.max(arg_at).min(region.end);
                        continue;
                    }
                    if matches!(cmd, "vspace" | "hspace" | "index" | "hypertarget") {
                        if let Some((_, end)) = math::group(source, arg_at) {
                            arg_at = end;
                        }
                        at = arg_at.min(region.end);
                        continue;
                    }
                    if is_transparent_reference_control(cmd) {
                        at = arg_at;
                        continue;
                    }
                    items.push(StreamItem::OtherGenerated);
                    at = skip_required_groups(source, arg_at).min(region.end);
                }
                // Generated header punctuation is wordless, so `]` only bounds a reference's flank.
                b']' if bracket_ends.contains(&at) => {
                    items.push(StreamItem::Boundary);
                    at = skip_overlay_arguments(source, at + 1).min(region.end);
                }
                b'{' | b'}' => {
                    at += 1;
                }
                _ => {
                    let span_idx = all_spans.partition_point(|(_, s)| s.start <= at);
                    if let Some((_, span)) = span_idx
                        .checked_sub(1)
                        .map(|idx| &all_spans[idx])
                        .filter(|(_, s)| s.contains(&at))
                    {
                        at = span.end.min(region.end);
                        continue;
                    }
                    let line_end = source[at..region.end]
                        .find(['\n', '%', '\\', '$', '&', '{', '}', ']'])
                        .map_or(region.end, |p| at + p);
                    if line_end <= at {
                        at += source[at..region.end]
                            .chars()
                            .next()
                            .map_or(1, char::len_utf8);
                        continue;
                    }
                    for (_, _, word) in source_words(&source[at..line_end]) {
                        if !word.is_empty() {
                            items.push(StreamItem::Word(word));
                        }
                    }
                    at = line_end;
                }
            }
        }
        Some((items, ref_positions))
    };

    let Some((main_stream, main_ref_positions)) = build_stream(span_start..stream_end, false)
    else {
        return RefOutcome::Opaque;
    };
    type FootnoteStream = (Range<usize>, Vec<StreamItem>, Vec<Option<usize>>);
    let mut footnote_streams: Vec<FootnoteStream> = Vec::new();
    for fn_body in &footnote_bodies {
        if !footnote_streams.iter().any(|(b, _, _)| b == fn_body) {
            let Some((stream, ref_positions)) = build_stream(fn_body.clone(), true) else {
                return RefOutcome::Opaque;
            };
            footnote_streams.push((fn_body.clone(), stream, ref_positions));
        }
    }
    let selected_word = &pdf[selected];
    type Rank = (bool, isize, bool, std::cmp::Reverse<u32>);
    let mut best: Option<(Rank, (u32, usize, isize, bool))> = None;
    let mut tied = false;

    fn collect_flank<'a>(
        items: impl Iterator<Item = &'a StreamItem>,
    ) -> (Vec<&'a String>, bool, bool) {
        let mut words = Vec::new();
        let mut adjacent_generated = false;
        let mut crossed_boundary = false;
        for item in items {
            match item {
                StreamItem::Word(w) => {
                    words.push(w);
                    if words.len() >= 3 {
                        break;
                    }
                }
                StreamItem::Ref(_) | StreamItem::OtherGenerated => {
                    if words.is_empty() && !crossed_boundary {
                        adjacent_generated = true;
                    }
                    break;
                }
                StreamItem::Boundary => {
                    if words.is_empty() {
                        crossed_boundary = true;
                    } else {
                        break;
                    }
                }
                StreamItem::PageBreak(_, _, _) => {
                    if words.is_empty() {
                        crossed_boundary = true;
                    }
                    break;
                }
            }
        }
        (words, adjacent_generated, crossed_boundary)
    }

    fn flanks_around(
        stream: &[StreamItem],
        left_end: usize,
        right_start: usize,
    ) -> (Vec<&String>, bool, bool, Vec<&String>, bool, bool) {
        let (left_words, left_adj, left_crossed) = collect_flank(stream[..left_end].iter().rev());
        let (right_words, right_adj, right_crossed) = collect_flank(stream[right_start..].iter());
        (
            left_words,
            left_adj,
            left_crossed,
            right_words,
            right_adj,
            right_crossed,
        )
    }

    for (ref_idx, cand) in refs.iter().enumerate() {
        if operation.check().is_err() {
            return RefOutcome::Opaque;
        }
        if pdf.len() == 1
            && cand
                .key_words
                .iter()
                .any(|w| w == selected_word && !w.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let cand_pos = cand
            .footnote_body
            .as_ref()
            .map_or(cand.cmd_start, |b| b.start);
        let main_page_start = main_stream
            .iter()
            .rposition(
                |item| matches!(item, StreamItem::PageBreak(_, pb_pos, true) if *pb_pos < cand_pos),
            )
            .unwrap_or(0);
        let main_page_end = main_stream
            .iter()
            .position(
                |item| matches!(item, StreamItem::PageBreak(_, pb_pos, true) if *pb_pos > cand_pos),
            )
            .map_or(main_stream.len(), |idx| idx + 1);
        let main_page_stream = &main_stream[main_page_start..main_page_end];
        let pb_before = match main_page_stream.first() {
            Some(&StreamItem::PageBreak(_, pb_pos, true)) if pb_pos < cand_pos => Some(pb_pos),
            _ => None,
        };
        let pb_after = match main_page_stream.last() {
            Some(&StreamItem::PageBreak(_, pb_pos, true)) if pb_pos > cand_pos => Some(pb_pos),
            _ => None,
        };
        let footnote_stream = cand.footnote_body.as_ref().and_then(|fn_body| {
            footnote_streams
                .iter()
                .find(|(b, _, _)| b == fn_body)
                .map(|(_, s, positions)| (s.as_slice(), positions[ref_idx]))
        });
        let (stream, pos) = footnote_stream.unwrap_or_else(|| {
            let pos = main_ref_positions[ref_idx]
                .filter(|pos| (main_page_start..main_page_end).contains(pos))
                .map(|pos| pos - main_page_start);
            (main_page_stream, pos)
        });
        let Some(pos) = pos else {
            continue;
        };
        let (
            left_words,
            left_adjacent_generated,
            left_crossed_boundary,
            right_words,
            right_adjacent_generated,
            right_crossed_boundary,
        ) = flanks_around(stream, pos, pos + 1);
        if left_adjacent_generated || right_adjacent_generated {
            continue;
        }
        let same_line_pagebreak = stream.iter().chain(main_page_stream.iter()).any(|item| {
            matches!(
                item,
                StreamItem::PageBreak(pb_line, _, _)
                    if (cand.start_line..=cand.end_line).contains(pb_line) || *pb_line == anchor
            )
        });
        let footnote_starts_at_left = cand.footnote_body.is_some() && pos == 2 + left_words.len();

        let mut pdf_spans = Vec::new();
        for g_start in 0..pdf.len() {
            'span: for g_end in g_start..(g_start + 6).min(pdf.len()) {
                let span_words = &pdf[g_start..=g_end];
                if left_words
                    .first()
                    .copied()
                    .is_some_and(|lw| span_words.iter().any(|w| w == lw))
                    || right_words
                        .first()
                        .copied()
                        .is_some_and(|rw| span_words.iter().any(|w| w == rw))
                {
                    continue;
                }
                let left_matched = if g_start > 0 {
                    if left_words.first().copied() != pdf.get(g_start - 1) {
                        continue;
                    }
                    true
                } else {
                    if cand.footnote_body.is_some()
                        || same_line_pagebreak
                        || left_words
                            .first()
                            .copied()
                            .is_some_and(|lw| pdf[g_end + 1..].iter().any(|w| w == lw))
                    {
                        continue;
                    }
                    left_words.is_empty() || left_crossed_boundary
                };
                let right_matched = if g_end + 1 < pdf.len() {
                    if right_words.first().copied() != pdf.get(g_end + 1) {
                        continue;
                    }
                    true
                } else {
                    if same_line_pagebreak
                        || right_words
                            .first()
                            .copied()
                            .is_some_and(|rw| pdf[..g_start].iter().any(|w| w == rw))
                    {
                        continue;
                    }
                    right_words.is_empty() || right_crossed_boundary
                };
                let mut score = 0isize;
                let mut vis_left_len = 0usize;
                let mut footnote_head_word = false;
                for distance in 1..=3 {
                    if g_start >= distance
                        && let Some(expected) = left_words.get(distance - 1).copied()
                    {
                        if pdf.get(g_start - distance) == Some(expected) {
                            score += (4 - distance) as isize;
                            vis_left_len = distance;
                        } else if footnote_starts_at_left
                            && distance >= 2
                            && distance == left_words.len()
                            && g_end + 1 < pdf.len()
                        {
                            footnote_head_word = true;
                            break;
                        } else {
                            continue 'span;
                        }
                    }
                }
                for distance in 1..=3 {
                    if g_end + distance < pdf.len()
                        && let Some(expected) = right_words.get(distance - 1).copied()
                    {
                        if pdf.get(g_end + distance) == Some(expected) {
                            score += (4 - distance) as isize;
                        } else {
                            continue 'span;
                        }
                    }
                }
                if score == 0 || (cand.footnote_body.is_some() && vis_left_len == 0) {
                    continue;
                }
                let both_sides = left_matched && right_matched;
                let vis_right_len = (pdf.len() - 1 - g_end).min(right_words.len()).min(3);
                pdf_spans.push((
                    g_start,
                    g_end,
                    both_sides,
                    score,
                    vis_left_len,
                    vis_right_len,
                    footnote_head_word,
                ));
            }
        }
        let mut best_for_ref: Option<(bool, isize)> = None;
        for &(g_start, g_end, both_sides, score, vis_left_len, vis_right_len, footnote_head_word) in
            &pdf_spans
        {
            if !(g_start..=g_end).contains(&selected) {
                continue;
            }
            let mut total_slots = 0usize;
            let mut ref_only_slots = 0usize;
            let mut matches_self = false;
            // A stream slot's left flank shows the PDF's visible left words, and a footnote
            // head word only where its stream also starts the footnote there.
            let left_flank_matches = |s_left: &[&String], in_footnote: bool, s_start: usize| {
                s_left.len() >= vis_left_len
                    && s_left[..vis_left_len] == left_words[..vis_left_len]
                    && (!footnote_head_word
                        || if s_left.len() >= left_words.len() {
                            s_left[vis_left_len] == left_words[vis_left_len]
                                || pdf.get(g_start - left_words.len()) == Some(s_left[vis_left_len])
                        } else {
                            !(in_footnote && s_start == 2 + s_left.len())
                        })
            };
            for (s_body, s) in std::iter::once((None, main_page_stream)).chain(
                footnote_streams
                    .iter()
                    .filter(|(b, _, _)| {
                        pb_before.is_none_or(|pb| b.start > pb)
                            && pb_after.is_none_or(|pb| b.start < pb)
                    })
                    .map(|(b, s, _)| (Some(b), s.as_slice())),
            ) {
                let is_cand_stream = s_body == cand.footnote_body.as_ref();
                if vis_left_len > 0 && vis_right_len > 0 {
                    for s_start in 0..s.len() {
                        if !matches!(&s[s_start], StreamItem::Word(rw) if right_words.first().copied() == Some(rw))
                        {
                            continue;
                        }
                        let (s_left, s_left_adj, _, s_right, s_right_adj, _) =
                            flanks_around(s, s_start, s_start);
                        if !s_left_adj
                            && !s_right_adj
                            && left_flank_matches(&s_left, s_body.is_some(), s_start)
                            && s_right.len() >= vis_right_len
                            && s_right[..vis_right_len] == right_words[..vis_right_len]
                        {
                            total_slots += 1;
                        }
                    }
                }
                for s_start in 0..s.len() {
                    if matches!(
                        s[s_start],
                        StreamItem::Boundary | StreamItem::PageBreak(_, _, _)
                    ) {
                        continue;
                    }
                    if vis_left_len == 0
                        && !matches!(s[s_start], StreamItem::Ref(_) | StreamItem::OtherGenerated)
                    {
                        continue;
                    }
                    for s_end in s_start..(s_start + 6).min(s.len()) {
                        let seg = &s[s_start..=s_end];
                        if seg
                            .iter()
                            .any(|item| matches!(item, StreamItem::PageBreak(_, _, _)))
                            || (seg.iter().any(|item| matches!(item, StreamItem::Boundary))
                                && (vis_left_len == 0 || vis_right_len == 0))
                        {
                            break;
                        }
                        if matches!(seg.last(), Some(StreamItem::Boundary))
                            || (seg.iter().any(|item| matches!(item, StreamItem::Boundary))
                                && !seg.iter().any(|item| {
                                    matches!(item, StreamItem::Ref(_) | StreamItem::OtherGenerated)
                                }))
                        {
                            continue;
                        }
                        if vis_right_len == 0
                            && !matches!(s[s_end], StreamItem::Ref(_) | StreamItem::OtherGenerated)
                        {
                            continue;
                        }
                        if vis_left_len == 0 || vis_right_len == 0 {
                            if seg.iter().any(|item| matches!(item, StreamItem::Word(_))) {
                                break;
                            }
                        } else if seg.iter().any(|item| {
                            matches!(
                                item,
                                StreamItem::Word(w)
                                    if left_words.first().copied() == Some(w)
                                        || right_words.first().copied() == Some(w)
                            )
                        }) {
                            continue;
                        }
                        let (
                            s_left,
                            s_left_adj,
                            s_left_crossed,
                            s_right,
                            s_right_adj,
                            s_right_crossed,
                        ) = flanks_around(s, s_start, s_end + 1);
                        if s_left_adj || s_right_adj {
                            continue;
                        }
                        let left_compat = if vis_left_len > 0 {
                            left_flank_matches(&s_left, s_body.is_some(), s_start)
                        } else if left_words.is_empty() || left_crossed_boundary {
                            s_left.is_empty() || s_left_crossed
                        } else {
                            true
                        };
                        let right_compat = if vis_right_len > 0 {
                            s_right.len() >= vis_right_len
                                && s_right[..vis_right_len] == right_words[..vis_right_len]
                        } else if right_words.is_empty() || right_crossed_boundary {
                            s_right.is_empty() || s_right_crossed
                        } else {
                            true
                        };
                        if left_compat && right_compat {
                            total_slots += 1;
                            if s_start == s_end && matches!(s[s_start], StreamItem::Ref(_)) {
                                ref_only_slots += 1;
                            }
                            if is_cand_stream && s_start == pos && s_end == pos {
                                matches_self = true;
                            }
                        }
                    }
                }
            }
            if !matches_self {
                continue;
            }
            let occurrence_proven = pdf_spans.len() == 1 && total_slots == 1;
            if !occurrence_proven {
                if best.is_none()
                    && total_slots > 1
                    && pdf_spans.len() < total_slots
                    && ref_only_slots == total_slots
                {
                    tied = true;
                }
                continue;
            }
            let span_rank = (both_sides, score);
            if best_for_ref.is_none_or(|prev| span_rank > prev) {
                best_for_ref = Some(span_rank);
            }
        }
        let Some((both_sides, score)) = best_for_ref else {
            continue;
        };
        let on_anchor = (cand.start_line..=cand.end_line).contains(&anchor);
        let proximity = if within_scope || mode == WordLocationMode::Guess {
            0
        } else {
            cand.label_line.abs_diff(line)
        };
        let rank = (both_sides, score, on_anchor, std::cmp::Reverse(proximity));
        let target = (cand.label_line, cand.label_byte, score, both_sides);
        match best {
            Some((best_rank, _)) if rank > best_rank => {
                best = Some((rank, target));
                tied = false;
            }
            Some((best_rank, best_target))
                if rank == best_rank && (best_target.0, best_target.1) != (target.0, target.1) =>
            {
                tied = true;
            }
            None => {
                best = Some((rank, target));
                tied = false;
            }
            _ => {}
        }
    }
    if tied {
        return match mode {
            WordLocationMode::Strict => RefOutcome::Absent,
            WordLocationMode::Guess => RefOutcome::Opaque,
        };
    }
    match best {
        Some((_, (row, byte, score, both_sides))) => {
            RefOutcome::Located(row, byte, score, both_sides)
        }
        None => RefOutcome::Absent,
    }
}

fn has_control_word(text: &str, name: &str) -> bool {
    let mut rest = text;
    while let Some(pos) = rest.find('\\') {
        let after_slash = &rest[pos + 1..];
        if let Some(after_break) = after_slash.strip_prefix('\\') {
            rest = after_break;
            continue;
        }
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

fn crosses_paragraph_boundary(analysis: &SourceAnalysis<'_>, line: u32, row: u32) -> bool {
    let start = line.min(row).saturating_sub(1) as usize;
    let end = line.max(row).saturating_sub(1) as usize;
    let from = analysis.line_starts.row_start(start, analysis.source.len());
    for (offset, raw) in analysis.source[from..]
        .lines()
        .take(end - start + 1)
        .enumerate()
    {
        let idx = start + offset;
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

// TeX's next-token header checks stop at a paragraph, not at comment newlines.
pub(crate) fn skip_header_space(source: &str, mut at: usize) -> Option<usize> {
    let mut newline = false;
    loop {
        match source.as_bytes().get(at) {
            Some(b' ' | b'\t' | b'\r') => at += 1,
            Some(b'\n') if newline => return None,
            Some(b'\n') => {
                newline = true;
                at += 1;
            }
            Some(b'%') => {
                if let Some(end) = source[at..].find('\n') {
                    at += end + 1;
                    newline = true;
                } else {
                    return Some(source.len());
                }
            }
            _ => return Some(at),
        }
    }
}

pub(crate) fn skip_overlay_arguments(source: &str, mut at: usize) -> usize {
    while let Some(end) =
        skip_header_space(source, at).and_then(|next| overlay_argument_end(source, next))
    {
        at = end;
    }
    at
}

/// Find a bracketed header as `[` through past `]`; a paragraph ends the search.
pub(crate) fn header_optional_argument(source: &str, at: usize) -> Option<Range<usize>> {
    let open = skip_header_space(source, at)?;
    (source.as_bytes().get(open) == Some(&b'['))
        .then(|| optional_argument_end(source, open).map(|end| open..end))
        .flatten()
}

// Beamer accepts at most `[<default overlay>]` before one `[options]` group.
pub(crate) fn next_frame_option_group(
    source: &str,
    at: usize,
    saw_default_overlay: &mut bool,
    saw_options: &mut bool,
) -> Option<(Range<usize>, usize)> {
    if *saw_options {
        return None;
    }
    let header = header_optional_argument(source, skip_overlay_arguments(source, at))?;
    let is_default_overlay = !*saw_default_overlay
        && skip_header_space(source, header.start + 1)
            .is_some_and(|inner| source.as_bytes().get(inner) == Some(&b'<'));
    if is_default_overlay {
        *saw_default_overlay = true;
    } else {
        *saw_options = true;
    }
    let end = skip_overlay_arguments(source, header.end);
    Some((header, end))
}

// TeX drops a comment with its newline and the next line's indentation.
fn strip_option_comments(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    let mut trim_line_start = false;
    for raw_line in raw.lines() {
        let line = if trim_line_start {
            raw_line.trim_start_matches([' ', '\t'])
        } else {
            raw_line
        };
        let text = source_line_text(line);
        cleaned.push_str(text);
        trim_line_start = text.len() < line.len();
        if !trim_line_start {
            cleaned.push(' ');
        }
    }
    cleaned
}

/// Split `key=value` options at top-level commas and each option's first top-level `=`.
fn top_level_options(text: &str) -> Vec<(Range<usize>, Option<Range<usize>>)> {
    let bytes = text.as_bytes();
    let mut options = Vec::new();
    let (mut start, mut content_end, mut equals, mut depth) = (0, 0, None, 0usize);
    let mut finish = |start: usize, content_end: usize, equals: Option<usize>| {
        let content_end = content_end.max(start);
        options.push(match equals {
            Some(equals) => (
                start..equals,
                Some(skip_source_space(text, equals + 1).min(content_end)..content_end),
            ),
            None => (start..content_end, None),
        });
    };
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                at = comment_end(text, at);
                continue;
            }
            b',' if depth == 0 => {
                finish(start, content_end, equals);
                (start, content_end, equals) = (at + 1, at + 1, None);
                at += 1;
                continue;
            }
            b'=' if depth == 0 && equals.is_none() => equals = Some(at),
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b'\\' => at += 1,
            byte if byte.is_ascii_whitespace() => {
                at += 1;
                continue;
            }
            _ => {}
        }
        at += 1;
        content_end = at.min(bytes.len());
    }
    finish(start, content_end, equals);
    options
}

/// Visit each `key[=value]` of a Beamer option list until `f` returns true.
pub(crate) fn for_each_frame_option(
    raw: &str,
    mut f: impl FnMut(&str, Option<&str>) -> bool,
) -> bool {
    let cleaned = strip_option_comments(raw);
    top_level_options(&cleaned).into_iter().any(|(key, value)| {
        f(
            cleaned[key].trim(),
            value.map(|value| strip_outer_group(&cleaned[value])),
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvHeaderKind {
    NoPrintedHeader,
    PrintedReplacement,
    PrintedNote,
}

pub(crate) fn classify_environment_header(env: &str) -> EnvHeaderKind {
    match env.trim_end_matches('*') {
        "document" | "center" | "flushleft" | "flushright" | "quote" | "quotation" | "verse"
        | "abstract" | "semiverbatim" | "frame" | "minipage" | "subfigure" | "subtable"
        | "tabular" | "tabularx" | "array" | "figure" | "table" | "algorithm" | "algorithm2e"
        | "algorithmic" | "columns" | "column" | "itemize" | "enumerate" | "description"
        | "tikzpicture" | "tcolorbox" | "axis" => EnvHeaderKind::NoPrintedHeader,
        "proof" => EnvHeaderKind::PrintedReplacement,
        _ => EnvHeaderKind::PrintedNote,
    }
}

pub(crate) fn skip_nonprinting_environment_arguments(
    source: &str,
    env: &str,
    mut at: usize,
) -> usize {
    let take_opt = |at: usize| header_optional_argument(source, at).map_or(at, |h| h.end);
    let take_req = |at: usize| {
        skip_header_space(source, at)
            .and_then(|next| math::group(source, next))
            .map_or(at, |(_, end)| end)
    };
    at = skip_overlay_arguments(source, at);
    // Only `tabular*` has a different signature from its unstarred form.
    let base = if env == "tabular*" {
        env
    } else {
        env.trim_end_matches('*')
    };
    match base {
        "frame" => {
            let mut saw_default_overlay = false;
            let mut saw_options = false;
            while let Some((_, end)) =
                next_frame_option_group(source, at, &mut saw_default_overlay, &mut saw_options)
            {
                at = end;
            }
            at
        }
        "tabularx" | "tabular*" => {
            at = take_req(at);
            at = take_opt(at);
            take_req(at)
        }
        "tabular" | "array" | "column" => {
            at = take_opt(at);
            skip_overlay_arguments(source, take_req(at))
        }
        "minipage" | "subfigure" | "subtable" => {
            for _ in 0..3 {
                at = take_opt(at);
            }
            take_req(at)
        }
        "figure" | "table" | "algorithm" | "algorithm2e" | "algorithmic" | "columns"
        | "itemize" | "enumerate" | "description" | "tikzpicture" | "tcolorbox" | "axis" => {
            skip_overlay_arguments(source, take_opt(at))
        }
        _ => at,
    }
}

pub(crate) fn skip_source_space(source: &str, mut at: usize) -> usize {
    loop {
        at += source[at..].len() - source[at..].trim_start().len();
        if source.as_bytes().get(at) != Some(&b'%') {
            return at;
        }
        at = comment_end(source, at);
    }
}

/// End of a TeX comment starting at `at`: its newline, or the end of input.
pub(crate) fn comment_end(source: &str, at: usize) -> usize {
    at + source[at..].find('\n').unwrap_or(source.len() - at)
}

/// Control sequences of `source[range]` outside comments as `(start, name, end)`.
pub(crate) fn control_words(
    source: &str,
    range: Range<usize>,
) -> impl Iterator<Item = (usize, &str, usize)> + '_ {
    let limit = range.end.min(source.len());
    let mut at = range.start;
    std::iter::from_fn(move || {
        while at < limit {
            let rel = source[at..limit].find(['\\', '%'])?;
            at += rel;
            if source.as_bytes()[at] == b'%' {
                at = comment_end(source, at);
                continue;
            }
            let start = at;
            let (name, end) = math::command(source, at);
            // Quoted and verbatim tokens (`\string\note`, `\verb|\note|`) never execute.
            at = math::opaque_command_end(source, name, end)
                .map_or(end, |(opaque_end, _)| opaque_end.max(end));
            return Some((start, name, end));
        }
        None
    })
}

/// Primitive `\if…`/`\else`/`\fi` nesting shared by the preamble, macro, and
/// sidecar-ownership scanners. Only `\iftrue`/`\iffalse` have a known branch.
#[derive(Default)]
pub(crate) struct ConditionalStack(Vec<Option<bool>>);

impl ConditionalStack {
    /// Apply a conditional control word, returning whether `cmd` was one.
    /// `\iff` is a relation; `\ifthenelse` is an ordinary `ifthen` command
    /// without a matching `\fi` unless the caller treats it as unknown.
    pub(crate) fn apply(&mut self, cmd: &str, ifthenelse_opens: bool) -> bool {
        match cmd {
            "else" => {
                if let Some(last) = self.0.last_mut() {
                    *last = last.map(|branch| !branch);
                }
            }
            "fi" => {
                self.0.pop();
            }
            "iftrue" => self.0.push(Some(true)),
            "iffalse" => self.0.push(Some(false)),
            "iff" => return false,
            "ifthenelse" if !ifthenelse_opens => return false,
            _ if cmd.starts_with("if") => self.0.push(None),
            _ => return false,
        }
        true
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Some enclosing branch is a literal false branch whose tokens TeX skips.
    pub(crate) fn skipping(&self) -> bool {
        self.0.contains(&Some(false))
    }

    /// Every enclosing branch is a literal true branch.
    pub(crate) fn certainly_active(&self) -> bool {
        self.0.iter().all(|branch| *branch == Some(true))
    }

    /// Skip one token of a literal false branch from `at`, returning where to
    /// continue. TeX pairs nested conditionals token by token here, so a
    /// static nonprinting span (`\let\X\iffalse`, `\def\X{\fi`) does not hide
    /// its conditionals. TeX does not run `\verb` or `\string` there either:
    /// only comments are skipped whole.
    pub(crate) fn skip_token(&mut self, source: &str, at: usize, ifthenelse_opens: bool) -> usize {
        let Some(rel) = source[at..].find(['%', '\\']) else {
            return source.len();
        };
        let at = at + rel;
        if source.as_bytes()[at] == b'%' {
            return comment_end(source, at);
        }
        let (cmd, next) = math::command(source, at);
        self.apply(cmd, ifthenelse_opens);
        next
    }
}

/// After a skipped branch ends at `at`, advance the `masked[*next..]` cursor;
/// a span opened inside the skipped text no longer applies, so spans are
/// recomputed from where TeX resumes.
pub(crate) fn resume_nonprinting_spans(
    source: &str,
    at: usize,
    masked: &mut std::borrow::Cow<'_, [Span]>,
    next: &mut usize,
) {
    while masked.get(*next).is_some_and(|(_, span, _)| span.end <= at) {
        *next += 1;
    }
    if masked
        .get(*next)
        .is_some_and(|(head, span, _)| *head < at || span.start < at)
    {
        let offset = |range: Range<usize>| range.start + at..range.end + at;
        *masked = nonprinting_source_spans(&source[at..])
            .map(|(head, span, inner)| (head + at, offset(span), inner.map(offset)))
            .collect::<Vec<_>>()
            .into();
        *next = 0;
    }
}

/// TeX optional arguments end at the first unbraced, unescaped closing bracket.
pub(crate) fn optional_argument_end(source: &str, start: usize) -> Option<usize> {
    let start = skip_source_space(source, start);
    if source.as_bytes().get(start) != Some(&b'[') {
        return None;
    }
    let mut at = start + 1;
    while at < source.len() {
        match source.as_bytes()[at] {
            b']' => return Some(at + 1),
            b'\\' => {
                at += 1;
                at += source[at..].chars().next().map_or(0, char::len_utf8);
                continue;
            }
            b'%' => {
                at = comment_end(source, at);
                continue;
            }
            b'{' => {
                at = math::group(source, at)?.1;
                continue;
            }
            b'}' => return None,
            _ => {}
        }
        at += source[at..].chars().next()?.len_utf8();
    }
    None
}

pub(crate) fn overlay_argument_end(source: &str, start: usize) -> Option<usize> {
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
    let has_action = body.contains([':', '@'])
        || body.split('|').all(|part| {
            matches!(
                part.trim(),
                "beamer" | "handout" | "trans" | "second" | "article" | "presentation" | "all"
            )
        });
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
pub(crate) fn nonprinting_source_ranges(source: &str) -> Vec<Range<usize>> {
    nonprinting_source_spans(source)
        .map(|(_, range, _)| range)
        .collect()
}

fn overlay_spec_matches(spec: &str, slide: u32) -> Option<bool> {
    let spec = spec.trim().strip_prefix('<')?.strip_suffix('>')?.trim();
    if spec.is_empty()
        || !spec
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b',' | b' '))
    {
        return None;
    }
    let mut matched = false;
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        if let Some((left, right)) = part.split_once('-') {
            let left = left.trim();
            let right = right.trim();
            if left.is_empty() && right.is_empty() {
                return None;
            }
            let min = if left.is_empty() {
                1
            } else {
                left.parse::<u32>().ok().filter(|&n| n > 0)?
            };
            let max = if right.is_empty() {
                u32::MAX
            } else {
                right.parse::<u32>().ok().filter(|&n| n >= min)?
            };
            if (min..=max).contains(&slide) {
                matched = true;
            }
        } else {
            let exact = part.parse::<u32>().ok().filter(|&n| n > 0)?;
            if slide == exact {
                matched = true;
            }
        }
    }
    Some(matched)
}

/// End of the `\begin{frame}` group on the frame's first row, which starts at byte `row_start`.
pub(crate) fn frame_header_after_begin(source: &str, row_start: usize) -> Option<usize> {
    let row = source[row_start..].lines().next().map(source_line_text)?;
    row.match_indices("\\begin")
        .filter(|(at, _)| !has_unescaped_backslash_before(row, *at))
        .find_map(|(at, _)| {
            let (env, end) = math::group(source, row_start + at + "\\begin".len())?;
            (&source[env] == "frame").then_some(end)
        })
}

#[allow(clippy::type_complexity)]
fn filter_frame_graphics_for_slide(
    analysis: &SourceAnalysis<'_>,
    frame_range: &Range<usize>,
    masked: &[Range<usize>],
    matched: &[(Range<usize>, Range<usize>, usize, usize)],
    slide: u32,
) -> Option<Vec<(Range<usize>, Range<usize>, usize, usize)>> {
    let source = analysis.source;
    let extra_sources = &analysis.extra_sources;
    let (frame_start, frame_end) = analysis.line_byte_range(frame_range);
    for text in [&source[..frame_start], &source[frame_end..]]
        .into_iter()
        .chain(extra_sources.iter().copied())
    {
        if text.contains("\\againframe")
            || text.contains("\\note")
            || text.contains("show notes")
            || text.contains("notes=")
            || text.contains("\\setbeamertemplate{background")
            || text.contains("\\usebackgroundtemplate")
        {
            return None;
        }
    }
    if let Some(end) = frame_header_after_begin(source, frame_start) {
        if skip_overlay_arguments(source, end) > end {
            return None;
        }
        let mut cursor = end;
        let mut saw_default_overlay = false;
        let mut saw_options = false;
        while let Some((header, next)) =
            next_frame_option_group(source, cursor, &mut saw_default_overlay, &mut saw_options)
        {
            if saw_default_overlay
                || next > header.end
                || for_each_frame_option(&source[header.start + 1..header.end - 1], |key, value| {
                    matches!(key, "allowframebreaks" | "label")
                        || (value.is_none()
                            && !key.is_empty()
                            && key.bytes().all(|b| {
                                b.is_ascii_digit()
                                    || matches!(
                                        b,
                                        b'-' | b'+'
                                            | b'*'
                                            | b':'
                                            | b'@'
                                            | b'('
                                            | b')'
                                            | b'|'
                                            | b' '
                                    )
                            }))
                })
            {
                return None;
            }
            cursor = next;
        }
    }
    let mut only_groups = Vec::new();
    let mut at = frame_start;
    while at < frame_end {
        match source.as_bytes()[at] {
            b'%' => {
                at = comment_end(source, at);
            }
            b'\\' => {
                let cmd_pos = at;
                let (cmd, next) = math::command(source, at);
                at = next;
                if let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, at) {
                    at = opaque_end;
                    continue;
                }
                if masked.iter().any(|r| {
                    r.start < cmd_pos && cmd_pos < r.end && !matched.iter().any(|m| m.0 == *r)
                }) {
                    continue;
                }
                if beamer_overlay_command(cmd) {
                    return None;
                }
                if cmd == "begin"
                    && let Some((env, _)) = math::group(source, at)
                    && beamer_overlay_environment(&source[env])
                {
                    return None;
                }
                if cmd == "only" {
                    let first = skip_source_space(source, at);
                    if let Some(spec_end) = overlay_argument_end(source, first) {
                        let spec_match = overlay_spec_matches(&source[first..spec_end], slide)?;
                        let (grp, _) = math::group(source, spec_end)?;
                        only_groups.push((grp, spec_match));
                    } else if let Some((grp, grp_end)) = math::group(source, first) {
                        let spec_start = skip_source_space(source, grp_end);
                        let spec_end = overlay_argument_end(source, spec_start)?;
                        let spec_match =
                            overlay_spec_matches(&source[spec_start..spec_end], slide)?;
                        only_groups.push((grp, spec_match));
                    } else {
                        return None;
                    }
                }
            }
            _ => {
                let rest = &source.as_bytes()[at + 1..frame_end];
                let step = rest
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'\\'))
                    .map_or(rest.len() + 1, |p| p + 1);
                at += step;
            }
        }
    }
    let mut any_governed = false;
    let mut filtered = Vec::new();
    for item in matched {
        let mut active = true;
        for (grp, spec_match) in &only_groups {
            if grp.start <= item.0.start && item.0.end <= grp.end {
                any_governed = true;
                active &= *spec_match;
            }
        }
        let (_, mut cursor) = math::command(source, item.0.start);
        cursor += usize::from(matches!(source.as_bytes().get(cursor), Some(b'*' | b'+')));
        while cursor < item.1.start {
            let next_pos = skip_source_space(source, cursor);
            if let Some(opt_end) = optional_argument_end(source, next_pos) {
                cursor = opt_end;
            } else if let Some(ov_end) = overlay_argument_end(source, next_pos) {
                any_governed = true;
                active &= overlay_spec_matches(&source[next_pos..ov_end], slide)?;
                cursor = ov_end;
            } else {
                break;
            }
        }
        if active {
            filtered.push(item.clone());
        }
    }
    any_governed.then_some(filtered)
}

pub(crate) fn source_graphics_location_with_context(
    analysis: &SourceAnalysis<'_>,
    line: u32,
    (index, count): (usize, usize),
    scope_proven: bool,
    frame_slide: Option<u32>,
    mode: WordLocationMode,
) -> Option<(u32, usize)> {
    let source = analysis.source;
    let operation = analysis.operation;
    operation.check().ok()?;
    if count == 0 || index >= count {
        return None;
    }
    let anchor = usize::try_from(line.checked_sub(1)?).ok()?;
    let spans = &analysis.strict_spans;
    let ignored = &analysis.strict_ignored;
    let frame_scope = scope_proven
        .then(|| {
            collected_lines(source, line, "frame", ignored).filter(|range| line == range.end as u32)
        })
        .flatten();
    let scope = scope_proven
        .then(|| {
            frame_scope
                .clone()
                .or_else(|| collected_lines(source, line, "tabularx", ignored))
                .or_else(|| math::collected_text_lines_with_spans(source, line, spans, ignored))
                .filter(|range| line == range.end as u32)
        })
        .flatten();
    let mut matched = Vec::new();
    let mut masked = Vec::new();
    for &(start, ref full, ref graphics_body) in spans {
        let Some(body) = graphics_body.clone() else {
            masked.push(start..full.end);
            continue;
        };
        let start_line = analysis.line_starts.row_of(full.start);
        let end_line = analysis.line_starts.row_of(full.end);
        let in_scope = match &scope {
            Some(range) => range.contains(&start_line) && range.contains(&end_line),
            None => (start_line..=end_line).contains(&anchor),
        };
        if in_scope {
            matched.push((full.clone(), body, start_line, end_line));
        }
    }
    if matched.len() > count
        && let (Some(frame_range), Some(slide)) = (&frame_scope, frame_slide)
        && let Some(filtered) =
            filter_frame_graphics_for_slide(analysis, frame_range, &masked, &matched, slide)
    {
        matched = filtered;
    }
    if matched.len() != count {
        return None;
    }
    let check_lines = scope.unwrap_or_else(|| {
        let min_line = matched.iter().map(|m| m.2).min().unwrap_or(anchor);
        let max_line = matched.iter().map(|m| m.3).max().unwrap_or(anchor);
        min_line..max_line + 1
    });
    let (scope_start, scope_end) = analysis.line_byte_range(&check_lines);
    let macros = analysis.macros(mode);
    let mut safe_macros = Vec::new();
    let mut masked = masked.into_iter().peekable();
    let mut conditionals = ConditionalStack::default();
    let mut at = 0usize;
    while at < scope_end {
        if at >= scope_start && !conditionals.is_empty() {
            return None;
        }
        while masked.peek().is_some_and(|r| r.end <= at) {
            masked.next();
        }
        if let Some(r) = masked.peek().filter(|r| r.contains(&at)) {
            at = r.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => {
                at = comment_end(source, at);
            }
            b'\\' => {
                let in_scope = at >= scope_start;
                let (cmd, next) = math::command(source, at);
                at = next;
                if let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, at) {
                    at = opaque_end;
                    continue;
                }
                if conditionals.apply(cmd, true) {
                    if in_scope {
                        return None;
                    }
                    continue;
                }
                if !in_scope {
                    continue;
                }
                if boxes_or_loops(cmd)
                    || (cmd == "begin"
                        && math::group(source, at).is_some_and(|(env, _)| &source[env] == "lrbox"))
                {
                    return None;
                }
                if macros.contains_key(cmd) || (cmd.len() > 1 && cmd.contains('@')) {
                    let mut arg_end = at;
                    while let Some(end) = optional_argument_end(source, arg_end)
                        .or_else(|| overlay_argument_end(source, arg_end))
                        .or_else(|| math::group(source, arg_end).map(|(_, e)| e))
                    {
                        arg_end = end;
                    }
                    if matched
                        .iter()
                        .any(|(full, _, _, _)| full.start < arg_end && full.end > at)
                    {
                        return None;
                    }
                    if macros.contains_key(cmd) && !safe_macros.contains(&cmd) {
                        // Every macro reachable from `cmd` must neither box, loop, nor include.
                        let mut stack = vec![cmd];
                        let mut seen = Vec::new();
                        while let Some(name) = stack.pop() {
                            if safe_macros.contains(&name) || seen.contains(&name) {
                                continue;
                            }
                            seen.push(name);
                            let body = macros.get(name).and_then(|e| e.as_ref()?.body)?;
                            if body.contains("include") || body.contains("lrbox") {
                                return None;
                            }
                            for (_, sub, _) in control_words(body, 0..body.len()) {
                                if boxes_or_loops(sub) {
                                    return None;
                                }
                                if macros.contains_key(sub) || (sub.len() > 1 && sub.contains('@'))
                                {
                                    stack.push(sub);
                                }
                            }
                        }
                        safe_macros.extend(seen);
                    }
                }
            }
            _ => {
                let next_mask_end = masked.peek().map_or(scope_end, |r| r.start.min(scope_end));
                let rest = &source.as_bytes()[at + 1..next_mask_end];
                let step = rest
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'\\'))
                    .map_or(rest.len() + 1, |p| p + 1);
                at += step;
            }
        }
    }
    let (_, body, _, _) = matched.get(index)?;
    let path_start = skip_source_space(source, body.start);
    if path_start >= body.end {
        return None;
    }
    let (line_idx, column) = analysis.line_starts.line_col(path_start);
    Some((u32::try_from(line_idx).ok()? + 1, column))
}

#[rustfmt::skip]
pub(super) fn environment_definition_command(command: &str) -> bool {
    matches!(
        command,
        "newenvironment" | "renewenvironment" | "provideenvironment" | "NewDocumentEnvironment"
        | "RenewDocumentEnvironment" | "ProvideDocumentEnvironment" | "DeclareDocumentEnvironment"
    )
}

// Past a macro definition's target and parameters, where its replacement text should start.
pub(crate) fn skip_definition_overlay(source: &str, at: usize) -> usize {
    let at = skip_source_space(source, at);
    let end = if source[at..].starts_with("<>") {
        at + 2
    } else {
        skip_overlay_arguments(source, at)
    };
    skip_source_space(source, end)
}

pub(crate) fn macro_definition_body_start(source: &str, command: &str, mut at: usize) -> usize {
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
        return at;
    }
    at = skip_definition_overlay(source, at);
    if source.as_bytes().get(at) == Some(&b'\\') {
        at = math::command(source, at).1;
    } else if let Some((_, end)) = math::macro_target_name(source, at) {
        at = end;
    } else if let Some((_, end)) = math::group(source, at) {
        at = end;
    }
    at = skip_definition_overlay(source, at);
    while let Some(end) = optional_argument_end(source, at) {
        at = end;
    }
    if command.contains("Document")
        && let Some((_, end)) = math::group(source, at)
    {
        at = end;
    }
    at
}

fn stored_definition_command(command: &str) -> bool {
    math::is_macro_definition_command(command) || environment_definition_command(command)
}

fn environment_definition_end(source: &str, command: &str, mut at: usize) -> Option<usize> {
    if !environment_definition_command(command) {
        return None;
    }
    at += usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
    let (_, contents_start) = math::group(source, at)?;
    at = contents_start;
    while let Some(end) = optional_argument_end(source, at) {
        at = end;
    }
    if command.contains("Document") {
        at = math::group(source, at)?.1;
    }
    let (_, end) = math::group(source, at)?;
    let (_, end) = math::group(source, end)?;
    Some(end)
}

pub(crate) fn nonprinting_source_spans(
    source: &str,
) -> impl Iterator<Item = (usize, Range<usize>, Option<Range<usize>>)> + '_ {
    nonprinting_source_spans_with_mode(source, None, None, WordLocationMode::Strict, None)
}

pub(crate) fn source_document_start<'a>(
    source: &'a str,
    macros: Option<&'a std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>>,
    strict_spans: Option<&'a [Span]>,
    operation: Option<&'a crate::Operation>,
) -> Option<usize> {
    let hidden: Box<dyn Iterator<Item = Span> + 'a> =
        match strict_spans.filter(|_| macros.is_none_or(|m| m.is_empty())) {
            Some(spans) => Box::new(spans.iter().cloned()),
            None => Box::new(nonprinting_source_spans_with_mode(
                source,
                macros,
                None,
                WordLocationMode::Strict,
                operation,
            )),
        };
    let mut hidden = hidden.peekable();
    let rebound = |command: &str| macros.is_some_and(|macros| macros.contains_key(command));
    let mut at = 0;
    while at < source.len() {
        if operation.is_some_and(|operation| operation.check().is_err()) {
            return None;
        }
        while hidden.peek().is_some_and(|(_, span, _)| span.end <= at) {
            hidden.next();
        }
        if let Some((head, span, _)) = hidden.peek().cloned()
            && span.contains(&at)
        {
            if source.as_bytes().get(head) == Some(&b'\\') && rebound(math::command(source, head).0)
            {
                return None;
            }
            at = span.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => at = comment_end(source, at),
            b'{' => return None,
            b'\\' => {
                let start = at;
                let (command, end) = math::command(source, at);
                if rebound(command) {
                    return None;
                }
                if let Some((opaque_end, _)) = math::opaque_command_end(source, command, end) {
                    at = opaque_end;
                } else if class_package_declaration(command) {
                    at = class_package_declaration_end(source, end)?;
                } else if command == "begin" {
                    let (environment, _) = math::group(source, end)?;
                    return (&source[environment] == "document").then_some(start);
                } else {
                    // shortcut: unsupported preamble execution cannot establish a body boundary.
                    return None;
                }
            }
            byte if byte.is_ascii_whitespace() => at += 1,
            _ => return None,
        }
    }
    None
}

pub(crate) fn nonprinting_source_spans_with_mode<'a>(
    source: &'a str,
    macros: Option<&'a std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>>,
    strict_spans: Option<&'a [Span]>,
    mode: WordLocationMode,
    operation: Option<&'a crate::Operation>,
) -> impl Iterator<Item = (usize, Range<usize>, Option<Range<usize>>)> + 'a {
    let preamble_end = if mode == WordLocationMode::Guess
        && operation.is_none_or(|operation| operation.check().is_ok())
        && !macros.is_some_and(|macros| macros.contains_key("begin"))
    {
        source_document_start(source, macros, strict_spans, operation)
    } else {
        None
    };
    let mut at = 0;
    let mut pending_opt_after = None;
    let mut pending_overlay_after = None;
    std::iter::from_fn(move || {
        loop {
            if at >= source.len() || operation.is_some_and(|operation| operation.check().is_err()) {
                return None;
            }
            if pending_opt_after == Some(at) {
                pending_opt_after = None;
                let opt_start = skip_source_space(source, at);
                let mut opt_end = at;
                while let Some(next) = optional_argument_end(source, opt_end) {
                    opt_end = next;
                }
                if opt_end > opt_start {
                    at = opt_end;
                    return Some((opt_start, opt_start..opt_end, None));
                }
            }
            if pending_overlay_after == Some(at) {
                pending_overlay_after = None;
                let overlay_end = skip_overlay_arguments(source, at);
                if overlay_end > at {
                    let overlay_start = skip_source_space(source, at);
                    at = overlay_end;
                    return Some((overlay_start, overlay_start..overlay_end, None));
                }
            }
            if source.as_bytes()[at] == b'%' {
                at = comment_end(source, at);
            } else if source.as_bytes()[at] == b'<'
                && at > 0
                && matches!(source.as_bytes()[at - 1], b'}' | b']')
            {
                let start = at;
                if let Some(end) = overlay_argument_end(source, at) {
                    at = end;
                    return Some((start, start..end, None));
                }
                at += 1;
            } else if source.as_bytes()[at] == b'\\' {
                let start = at;
                let (command, end) = math::command(source, start);
                let definition = stored_definition_command(command);
                let overridden = !definition
                    && command != "newif"
                    && macros.is_some_and(|macros| macros.contains_key(command));
                if overridden {
                    at = end;
                    continue;
                }
                if !command
                    .as_bytes()
                    .first()
                    .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == b'@')
                {
                    at = end;
                    continue;
                }
                if let Some((end, nonprinting)) = math::opaque_command_end(source, command, end) {
                    at = end;
                    if nonprinting {
                        return Some((start, start..end, None));
                    }
                    continue;
                }
                if command == "newif" {
                    let mut next = skip_source_space(source, end);
                    if let Some((_, target_end)) = math::macro_target_name(source, next) {
                        next = target_end;
                    } else if let Some((_, target_end)) = math::group(source, next) {
                        next = target_end;
                    }
                    at = next;
                    return Some((start, start..at, None));
                }
                if matches!(command, "let" | "futurelet") {
                    // `\let\name=\token` binds without printing; `\futurelet\name`
                    // then runs its next tokens, which still print.
                    at = math::macro_target_name(source, end).map_or(end, |(_, e)| e);
                    if command == "let" {
                        at = skip_source_space(source, at);
                        at = skip_source_space(
                            source,
                            at + usize::from(source[at..].starts_with('=')),
                        );
                        at = match source[at..].chars().next() {
                            Some('\\') => math::command(source, at).1,
                            other => at + other.map_or(0, char::len_utf8),
                        };
                    }
                    return Some((start, start..at, None));
                }
                if preamble_end.is_some_and(|boundary| start < boundary)
                    && class_package_declaration(command)
                    && let Some(declaration_end) = class_package_declaration_end(source, end)
                {
                    at = declaration_end;
                    // Package effects stay globally unverified; their filenames are not prose.
                    return Some((start, end..declaration_end, None));
                }
                at = end + usize::from(matches!(source.as_bytes().get(end), Some(b'*' | b'+')));
                if definition {
                    if let Some(end) = environment_definition_end(source, command, end) {
                        at = end;
                        return Some((start, start..end, None));
                    }
                    at = macro_definition_body_start(source, command, at);
                    let body_start = skip_source_space(source, at);
                    let Some((_, end)) = math::group(source, body_start) else {
                        if source.as_bytes().get(body_start) == Some(&b'{') {
                            let end = source.len();
                            at = end;
                            return Some((start, start..end, None));
                        }
                        at = body_start;
                        continue;
                    };
                    at = end;
                    if environment_definition_command(command) {
                        let end_start = skip_source_space(source, at);
                        if let Some((_, end)) = math::group(source, end_start) {
                            at = end;
                        } else if source.as_bytes().get(end_start) == Some(&b'{') {
                            at = source.len();
                        }
                    }
                    return Some((start, start..at, None));
                } else if let Some(header) = math::printed_header_argument(source, command, end) {
                    // The span starts after the command so `.0`-driven scanners still reach it;
                    // this also hides the environment name and overlays around a printed header.
                    pending_overlay_after = Some(header.end);
                    let prefix_start = skip_source_space(source, at);
                    let prefix_end = skip_overlay_arguments(
                        source,
                        match command {
                            "begin" => math::group(source, prefix_start)
                                .map_or(prefix_start, |(_, end)| end),
                            _ => at,
                        },
                    );
                    if prefix_start < prefix_end {
                        at = prefix_end;
                        return Some((prefix_start, prefix_start..prefix_end, None));
                    }
                } else if let Some((heading, contents, full_end)) =
                    math::collected_text_arguments(source, command, end)
                {
                    let first_visible = heading.as_ref().unwrap_or(&contents);
                    let prefix_start = skip_source_space(source, at);
                    let prefix_end = first_visible.start.saturating_sub(1);
                    if let Some(heading) = heading
                        && matches!(command, "subcaptionbox" | "captionbox")
                        && heading.end + 1 < full_end
                    {
                        pending_opt_after = Some(heading.end + 1);
                    }
                    if prefix_start < prefix_end {
                        at = prefix_end;
                        return Some((prefix_start, prefix_start..prefix_end, None));
                    }
                } else if nonprinting_argument_command(command) {
                    at = skip_optional_and_overlay_arguments(source, at);
                    if let Some((body, end)) = math::group(source, at) {
                        at = end;
                        let is_graphics = matches!(command, "includegraphics" | "includesvg");
                        return Some((
                            start,
                            if is_graphics || matches!(command, "textcolor" | "color") {
                                start..end
                            } else {
                                body.clone()
                            },
                            is_graphics.then_some(body),
                        ));
                    }
                } else if let Some(arity) = cref_format_arity(command) {
                    let mut group_end = at;
                    for _ in 0..arity {
                        let Some((_, end)) = math::group(source, group_end) else {
                            break;
                        };
                        group_end = end;
                    }
                    if group_end > at {
                        at = group_end;
                        return Some((start, start..group_end, None));
                    }
                } else if matches!(command, "begin" | "end")
                    && let Some((body, mut group_end)) = math::group(source, at)
                    && (!math::math_environment(&source[body.clone()])
                        || &source[body.clone()] == "array")
                {
                    let env = source[body.clone()].trim();
                    if command == "begin" {
                        group_end = skip_nonprinting_environment_arguments(source, env, group_end);
                    }
                    at = group_end;
                    return Some((start, body.start..group_end, None));
                } else if command == "item" {
                    let overlay_end = skip_overlay_arguments(source, at);
                    if overlay_end > at {
                        let overlay_start = skip_source_space(source, at);
                        at = overlay_end;
                        return Some((start, overlay_start..overlay_end, None));
                    }
                } else if let Some(overlay_end) = overlay_argument_end(source, at) {
                    at = overlay_end;
                    return Some((start, end..overlay_end, None));
                }
            } else {
                let limit = pending_opt_after
                    .filter(|&p| p > at)
                    .into_iter()
                    .chain(pending_overlay_after.filter(|&p| p > at))
                    .min()
                    .unwrap_or(source.len());
                at += source.as_bytes()[at + 1..limit]
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'<' | b'\\'))
                    .map_or(limit - at, |pos| pos + 1);
            }
        }
    })
}

pub(crate) fn source_prose_scored_location_with_analysis(
    analysis: &SourceAnalysis<'_>,
    line: u32,
    (context, offset): (&str, usize),
    lines: Range<usize>,
    within_frame: bool,
    allowed_bytes: Option<Range<usize>>,
) -> Option<(u32, usize, isize)> {
    let scope = WordRangeScope {
        line,
        lines,
        foreach_loops: &[],
        execution_scope: None,
    };
    match source_prose_scored_location_with_mode(
        analysis,
        &scope,
        within_frame,
        (context, offset),
        (&analysis.strict_hidden, allowed_bytes),
        &[],
        WordLocationMode::Strict,
    ) {
        ProseRank::Unique(row, byte, score) => Some((row, byte, score)),
        ProseRank::Absent | ProseRank::Tied(_) | ProseRank::Opaque => None,
    }
}

/// Prose evidence for the selected word, before math and reference ownership apply.
#[derive(Clone, Copy)]
pub(super) enum ProseRank {
    /// No occurrence outside math competes, so math ownership may decide.
    Absent,
    /// Line, byte and score of the single best occurrence.
    Unique(u32, usize, isize),
    /// Equal evidence outside math at this score; weaker ownership cannot become unique.
    Tied(isize),
    /// Unresolved macro/note tie or cancellation/timeout.
    Opaque,
}

// shortcut: 64 KiB fixed prose macro expansion cap, raise if legitimate zero-arg prose macros exceed 64 KiB.
const MAX_FIXED_PROSE_MACRO_BYTES: usize = 65_536;

fn append_fixed_prose_macro_text<'a>(
    text: &'a str,
    macros: &std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    operation: &crate::Operation,
    depth: u8,
    out: &mut String,
) -> Option<bool> {
    operation.check().ok()?;
    if depth >= 8 {
        return Some(false);
    }
    let mut at = 0usize;
    while at < text.len() {
        operation.check().ok()?;
        if let Some((ch, next)) = tex_accent_at(text, at) {
            if out.len() + ch.len_utf8() > MAX_FIXED_PROSE_MACRO_BYTES {
                return Some(false);
            }
            out.push(ch);
            at = next;
            continue;
        }
        match text.as_bytes()[at] {
            b'%' | b'$' | b'#' | b'&' | b'~' | b'^' | b'_' => return Some(false),
            b'{' => {
                let Some((inner, next)) = math::group(text, at) else {
                    return Some(false);
                };
                if !append_fixed_prose_macro_text(&text[inner], macros, operation, depth + 1, out)?
                {
                    return Some(false);
                }
                at = next;
            }
            b'}' => return Some(false),
            b'\\' => {
                let (cmd, next) = math::command(text, at);
                if cmd.is_empty() {
                    return Some(false);
                }
                if let Some(entry) = macros.get(cmd) {
                    let Some(sub) = entry.as_ref() else {
                        return Some(false);
                    };
                    if sub.arity != 0 || sub.used_params != 0 {
                        return Some(false);
                    }
                    let Some(body) = sub.body else {
                        return Some(false);
                    };
                    if !append_fixed_prose_macro_text(body, macros, operation, depth + 1, out)? {
                        return Some(false);
                    }
                    if let Some((g, ge)) = math::group(text, next)
                        && g.is_empty()
                    {
                        at = ge;
                    } else {
                        at = next;
                    }
                } else if known_formatting_switch(cmd) || cmd == "relax" {
                    if let Some((g, ge)) = math::group(text, next)
                        && g.is_empty()
                    {
                        at = ge;
                    } else {
                        at = next;
                    }
                } else if text_style_wrapper(cmd) {
                    let Some((inner, ge)) = math::group(text, next) else {
                        return Some(false);
                    };
                    if !append_fixed_prose_macro_text(
                        &text[inner],
                        macros,
                        operation,
                        depth + 1,
                        out,
                    )? {
                        return Some(false);
                    }
                    at = ge;
                } else {
                    return Some(false);
                }
            }
            _ => {
                let ch = text[at..].chars().next().unwrap();
                if out.len() + ch.len_utf8() > MAX_FIXED_PROSE_MACRO_BYTES {
                    return Some(false);
                }
                out.push(ch);
                at += ch.len_utf8();
            }
        }
    }
    Some(true)
}

fn fixed_prose_macro_words<'a>(
    def: &math::MacroDef<'a>,
    macros: &std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    operation: &crate::Operation,
) -> Option<Option<Vec<String>>> {
    operation.check().ok()?;
    if def.arity != 0 || def.used_params != 0 {
        return Some(None);
    }
    let Some(body) = def.body else {
        return Some(None);
    };
    let mut expanded = String::new();
    if !append_fixed_prose_macro_text(body, macros, operation, 0, &mut expanded)? {
        return Some(None);
    }
    let words: Vec<String> = source_words(&expanded)
        .into_iter()
        .map(|(_, _, w)| w)
        .filter(|w| !w.is_empty())
        .collect();
    Some((!words.is_empty()).then_some(words))
}

fn source_prose_scored_location_with_mode(
    analysis: &SourceAnalysis<'_>,
    scope: &WordRangeScope<'_>,
    within_frame: bool,
    (context, offset): (&str, usize),
    (hidden, allowed_bytes): (&[Range<usize>], Option<Range<usize>>),
    math_regions: &[Range<usize>],
    mode: WordLocationMode,
) -> ProseRank {
    let source = analysis.source;
    let operation = analysis.operation;
    let line = scope.line;
    let lines = &scope.lines;
    let loops = scope.foreach_loops;
    if operation.check().is_err() {
        return ProseRank::Opaque;
    }
    enum ProseToken<'a> {
        Word(u32, usize, String, bool, bool),
        LoopVar(u32, usize, usize, &'a str, bool),
    }

    if loops.len() > 2 || loops.iter().any(|lp| lp.items.is_none()) {
        return ProseRank::Absent;
    }
    let pdf = words(context);
    let Some(selected) = word_index_at(context, &pdf, offset) else {
        return ProseRank::Absent;
    };
    let pdf: Vec<_> = pdf.iter().map(|(_, word)| normalized_word(word)).collect();
    if custom_environment_prints_word(analysis, lines, hidden, &pdf[selected], mode) {
        return ProseRank::Opaque;
    }
    let mut expanded_macros: std::collections::BTreeMap<&str, Option<Vec<String>>> =
        std::collections::BTreeMap::new();
    let mut line_start = analysis.line_starts.row_start(lines.start, source.len());
    let frame_header = within_frame
        && source[line_start..]
            .lines()
            .next()
            .is_some_and(|row| source_line_text(row).contains("\\begin{frame}"));
    let mut hidden = hidden.iter().peekable();
    let mut math_regions = math_regions.iter().peekable();
    let mut tokens = Vec::new();
    let mut has_loop_var = false;
    let mut phantom_until = 0usize;
    let rows = source[line_start..]
        .split_inclusive('\n')
        .enumerate()
        .map(|(offset, row)| (lines.start + offset, row));
    for (index, raw_row) in rows.take(lines.end.saturating_sub(lines.start)) {
        if operation.check().is_err() {
            return ProseRank::Opaque;
        }
        let text = source_line_text(raw_row.trim_end_matches(['\r', '\n']));
        for (byte, end, word) in source_words(text) {
            let abs = line_start + byte;
            while hidden.peek().is_some_and(|range| range.end <= abs) {
                hidden.next();
            }
            if allowed_bytes
                .as_ref()
                .is_some_and(|span| !span.contains(&abs))
                || abs < phantom_until
                || hidden.peek().is_some_and(|range| range.contains(&abs))
                || loops.iter().any(|lp| lp.header.contains(&abs))
            {
                continue;
            }
            while math_regions.peek().is_some_and(|range| range.end <= abs) {
                math_regions.next();
            }
            let in_math = math_regions
                .peek()
                .is_some_and(|range| range.contains(&abs));
            if !has_unescaped_backslash_before(text, byte) {
                tokens.push(ProseToken::Word(
                    index as u32 + 1,
                    byte,
                    word,
                    false,
                    in_math,
                ));
            } else {
                let macros = analysis.macros(mode);
                if discarding_command(&text[byte..end]) && !macros.contains_key(&text[byte..end]) {
                    if let Some(arg_end) =
                        discarded_argument_end(source, line_start + end, source.len(), false)
                    {
                        phantom_until = phantom_until.max(arg_end);
                    }
                } else if let raw = &text[byte..end]
                    && loops
                        .iter()
                        .any(|lp| lp.body.contains(&(abs - 1)) && lp.vars.contains(&raw))
                {
                    has_loop_var = true;
                    tokens.push(ProseToken::LoopVar(
                        index as u32 + 1,
                        byte - 1,
                        abs - 1,
                        raw,
                        in_math,
                    ));
                } else if let (cmd, cmd_end) = math::command(text, byte - 1)
                    && cmd_end == end
                    && text.as_bytes().get(end) != Some(&b'[')
                    && (text.as_bytes().get(end) != Some(&b'{')
                        || math::group(text, end).is_some_and(|(g, _)| g.is_empty()))
                    && let Some((&macro_name, Some(def))) = macros.get_key_value(cmd)
                {
                    let macro_words = match expanded_macros.entry(macro_name) {
                        std::collections::btree_map::Entry::Occupied(cached) => cached.into_mut(),
                        std::collections::btree_map::Entry::Vacant(slot) => {
                            let Some(computed) = fixed_prose_macro_words(def, macros, operation)
                            else {
                                return ProseRank::Opaque;
                            };
                            slot.insert(computed)
                        }
                    };
                    for w in macro_words.iter().flatten() {
                        tokens.push(ProseToken::Word(
                            index as u32 + 1,
                            byte - 1,
                            w.clone(),
                            true,
                            in_math,
                        ));
                    }
                }
            }
        }
        line_start += raw_row.len();
    }
    let mut passes = Vec::new();
    if !has_loop_var {
        let candidates: Vec<_> = tokens
            .into_iter()
            .filter_map(|t| match t {
                ProseToken::Word(r, b, w, m, in_math) => Some((r, b, w, m, in_math)),
                ProseToken::LoopVar(..) => None,
            })
            .collect();
        passes.push(candidates);
    } else {
        let active_loops: Vec<_> = loops
            .iter()
            .filter(|lp| {
                tokens.iter().any(|t| {
                    matches!(t, ProseToken::LoopVar(_, _, abs, v, _) if lp.body.contains(abs) && lp.vars.contains(v))
                })
            })
            .collect();
        if active_loops.is_empty() || active_loops.len() > 2 {
            return ProseRank::Absent;
        }
        let Some(combinations) = foreach_combinations(active_loops, |lp, var, val| {
            (!val.contains('\\')).then(|| (lp.body.clone(), var, val))
        }) else {
            return ProseRank::Absent;
        };
        for combo in combinations {
            let mut candidates = Vec::with_capacity(tokens.len());
            for t in &tokens {
                match t {
                    ProseToken::Word(r, b, w, m, in_math) => {
                        candidates.push((*r, *b, w.clone(), *m, *in_math));
                    }
                    ProseToken::LoopVar(r, b, abs, var, in_math) => {
                        if let Some((_, _, val)) = combo
                            .iter()
                            .rev()
                            .find(|(body, v, _)| body.contains(abs) && v == var)
                        {
                            for (_, _, w) in source_words(val) {
                                candidates.push((*r, *b, w, false, *in_math));
                            }
                        }
                    }
                }
            }
            passes.push(candidates);
        }
    }
    type BaseRank = (isize, bool, isize);
    let mut best: Option<(BaseRank, u32, u32, usize, bool, bool)> = None;
    let mut tied = false;
    // Equal-evidence macro ambiguity is not resolved by literal proximity.
    let mut macro_tied = false;
    // Math context can only separate occurrences that are all inside formulas.
    let mut outside_math_at_rank = false;
    let mut literal_tie_outside_math = false;
    let mut unsupported_macro: Option<BaseRank> = None;
    let mut unsupported_macro_outside_math: Option<BaseRank> = None;
    for candidates in &passes {
        for (index, (row, byte, word, from_macro, in_math)) in candidates.iter().enumerate() {
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
            let proximity = if within_frame || mode == WordLocationMode::Guess {
                0
            } else {
                row.abs_diff(line)
            };
            let base_rank = (
                score,
                mode == WordLocationMode::Strict
                    && frame_header
                    && pdf.len() > 1
                    && score == 0
                    && *row as usize == lines.start + 1
                    && candidates
                        .iter()
                        .filter(|(r, _, _, _, _)| *r == *row)
                        .take(3)
                        .count()
                        <= 2,
                context_bonus,
            );
            if *from_macro && score == 0 {
                unsupported_macro = unsupported_macro.max(Some(base_rank));
                if !*in_math {
                    unsupported_macro_outside_math =
                        unsupported_macro_outside_math.max(Some(base_rank));
                }
                continue;
            }
            match best {
                Some((old_base, ..)) if base_rank < old_base => {}
                Some((old_base, old_prox, prev_row, prev_byte, prev_macro, prev_in_math))
                    if base_rank == old_base =>
                {
                    let distinct = (prev_row, prev_byte) != (*row, *byte);
                    outside_math_at_rank |= !*in_math;
                    if *from_macro || prev_macro {
                        tied |= distinct;
                        macro_tied |= distinct;
                    } else if proximity < old_prox {
                        best = Some((base_rank, proximity, *row, *byte, *from_macro, *in_math));
                        tied = macro_tied;
                        literal_tie_outside_math = false;
                    } else if proximity == old_prox {
                        tied |= distinct;
                        literal_tie_outside_math |= distinct && !(prev_in_math && *in_math);
                    }
                }
                _ => {
                    best = Some((base_rank, proximity, *row, *byte, *from_macro, *in_math));
                    tied = false;
                    macro_tied = false;
                    outside_math_at_rank = !*in_math;
                    literal_tie_outside_math = false;
                }
            }
        }
    }
    let best_rank = best.map(|(rank, ..)| rank);
    // An uncorroborated macro cannot claim its occurrence, but it still leaves
    // an equally unsupported literal non-unique.
    if unsupported_macro.is_some() && unsupported_macro >= best_rank {
        tied = true;
        macro_tied = true;
        outside_math_at_rank |=
            unsupported_macro_outside_math.is_some() && unsupported_macro_outside_math >= best_rank;
    }
    if mode == WordLocationMode::Guess && tied {
        return ProseRank::Opaque;
    }
    let tied_outside_math = if macro_tied {
        outside_math_at_rank
    } else {
        literal_tie_outside_math
    };
    match best {
        Some((rank, _, row, byte, _, _)) if !tied => ProseRank::Unique(row, byte, rank.0),
        _ if tied_outside_math => ProseRank::Tied(best_rank.map_or(0, |rank| rank.0)),
        _ => ProseRank::Absent,
    }
}

fn clean_lexical_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub(crate) fn host_owns_generated_sidecar_with_spans(
    source: &str,
    host_path: &std::path::Path,
    generated: &std::path::Path,
    main_dir: Option<&std::path::Path>,
    known_spans: &[Span],
) -> bool {
    let ext = generated.extension().and_then(|e| e.to_str()).unwrap_or("");
    let generated_clean = clean_lexical_path(generated);
    // Nonprinting spans are lexical; one opened inside a skipped branch is
    // recomputed from wherever TeX resumes rather than masking resumed tokens.
    let mut masked = std::borrow::Cow::Borrowed(known_spans);
    let mut next_mask = 0usize;
    let mut conditionals = ConditionalStack::default();
    let mut at = 0usize;
    while at < source.len() {
        if conditionals.skipping() {
            at = conditionals.skip_token(source, at, false);
            if !conditionals.skipping() {
                resume_nonprinting_spans(source, at, &mut masked, &mut next_mask);
            }
            continue;
        }
        while masked
            .get(next_mask)
            .is_some_and(|(_, span, _)| span.end <= at)
        {
            next_mask += 1;
        }
        if let Some((_, span, _)) = masked.get(next_mask)
            && span.start <= at
        {
            at = span.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => {
                at = comment_end(source, at);
            }
            b'=' => {
                if let Some((_, grp_end)) = math::group(source, at + 1) {
                    at = grp_end;
                } else {
                    at += 1;
                }
            }
            b'\\' => {
                let (cmd, next) = math::command(source, at);
                if next == at + 1 {
                    at = next + source[next..].chars().next().map_or(0, char::len_utf8);
                    continue;
                }
                if let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, next) {
                    at = opaque_end;
                    continue;
                }
                if unexecuted_argument_command(cmd) {
                    let mut cur = next;
                    while let Some(opt_end) = optional_argument_end(source, cur) {
                        cur = opt_end;
                    }
                    if let Some((_, grp_end)) = math::group(source, cur) {
                        at = grp_end;
                        continue;
                    }
                }
                if conditionals.apply(cmd, false) {
                    at = next;
                    continue;
                }
                if conditionals.certainly_active() {
                    if cmd == "endinput" {
                        break;
                    }
                    if matches!(cmd, "begin" | "end")
                        && let Some((_, grp_end)) = math::group(source, next)
                    {
                        at = grp_end;
                        continue;
                    }
                    if matches!(cmd, "input" | "include" | "InputIfFileExists")
                        && math::group(source, next).is_some_and(|(g, _)| {
                            let target = source[g].trim();
                            if target.is_empty() {
                                return false;
                            }
                            let target_path = std::path::Path::new(target);
                            let matches_cand = |cand: &std::path::Path| {
                                clean_lexical_path(cand) == generated_clean
                                    || (cand.extension().is_none()
                                        && clean_lexical_path(&cand.with_extension("tex"))
                                            == generated_clean)
                            };
                            if target_path.is_absolute() {
                                matches_cand(target_path)
                            } else {
                                [host_path.parent(), main_dir]
                                    .into_iter()
                                    .flatten()
                                    .any(|base| matches_cand(&base.join(target_path)))
                            }
                        })
                    {
                        return true;
                    }
                    match ext {
                        "bbl"
                            if matches!(
                                cmd,
                                "bibliography"
                                    | "printbibliography"
                                    | "nobibliography"
                                    | "putbib"
                                    | "bibbysection"
                            ) =>
                        {
                            return true;
                        }
                        "toc" if cmd == "tableofcontents" => return true,
                        _ => {}
                    }
                }
                at = next;
            }
            _ => {
                at += source.as_bytes()[at + 1..]
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'\\' | b'='))
                    .map_or(source.len() - at, |pos| pos + 1);
            }
        }
    }
    false
}
