use crate::{math, validate_forward_source_position};
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
    ignored.sort_unstable_by_key(|range| range.start);
    let mut ignored = ignored.into_iter().peekable();
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

pub(crate) fn frame_bounds(source: &str, anchor: usize) -> Option<(usize, usize)> {
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

pub(crate) fn caption_closing_line(source: &str, line: u32, column: u32) -> Option<u32> {
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
pub(crate) fn source_frame_range(source: &str, line: u32) -> Option<std::ops::Range<usize>> {
    frame_bounds(source, line.checked_sub(1)? as usize).map(|(start, end)| start..end + 1)
}

/// Find a literal `tabularx` environment enclosing the one-based SyncTeX line.
pub(crate) fn source_tabularx_range(source: &str, line: u32) -> Option<std::ops::Range<usize>> {
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
    if let Some((start, _, word)) = source.lines().nth(line as usize - 1).and_then(|text| {
        source_words(source_line_text(text))
            .into_iter()
            .find(|(start, end, _)| *start <= byte && byte < *end)
    }) && word == pdf_word
    {
        return Some(start);
    }
    let line_start: usize = source
        .split_inclusive('\n')
        .take(line as usize - 1)
        .map(str::len)
        .sum();
    math::in_region(source, line_start + byte).then_some(byte)
}

/// Refine prose and mathematical atoms without expanding arbitrary TeX macros.
pub(crate) fn source_word_location(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    radius: u32,
) -> Option<(u32, usize)> {
    let scope = math::caption_lines(source, line)
        .or_else(|| source_frame_range(source, line))
        .or_else(|| source_tabularx_range(source, line))
        .or_else(|| math::math_lines(source, line));
    let within_scope = scope.is_some();
    let lines = scope.unwrap_or_else(|| {
        line.saturating_sub(radius.saturating_add(1)) as usize
            ..(line as usize).saturating_add(radius as usize)
    });
    source_word_location_in_range(source, line, context, offset, lines, within_scope)
}

pub(crate) fn source_word_location_in_range(
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
        None,
    );
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

pub(crate) fn skip_source_space(source: &str, mut at: usize) -> usize {
    loop {
        at += source[at..].len() - source[at..].trim_start().len();
        if source.as_bytes().get(at) != Some(&b'%') {
            return at;
        }
        at += source[at..].find('\n').unwrap_or(source.len() - at);
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
pub(crate) fn nonprinting_source_ranges(source: &str) -> Vec<std::ops::Range<usize>> {
    nonprinting_source_spans(source)
        .map(|(_, range, _)| range)
        .collect()
}

pub(crate) fn source_graphics_location(
    source: &str,
    line: u32,
    index: usize,
    count: usize,
    scope_proven: bool,
) -> Option<(u32, usize)> {
    if count == 0 || index >= count {
        return None;
    }
    let anchor = usize::try_from(line.checked_sub(1)?).ok()?;
    let scope = scope_proven
        .then(|| {
            source_frame_range(source, line)
                .or_else(|| source_tabularx_range(source, line))
                .or_else(|| math::caption_lines(source, line))
                .filter(|range| line == range.end as u32)
        })
        .flatten();
    let mut matched = Vec::new();
    let mut masked = Vec::new();
    let mut line_cursor = 0usize;
    let mut line_count = 0usize;
    for (start, full, graphics_body) in nonprinting_source_spans(source) {
        let Some(body) = graphics_body else {
            masked.push(start..full.end);
            continue;
        };
        if full.start >= line_cursor {
            line_count += source[line_cursor..full.start]
                .bytes()
                .filter(|&ch| ch == b'\n')
                .count();
            line_cursor = full.start;
        }
        let start_line = line_count;
        if full.end >= line_cursor {
            line_count += source[line_cursor..full.end]
                .bytes()
                .filter(|&ch| ch == b'\n')
                .count();
            line_cursor = full.end;
        }
        let end_line = line_count;
        let in_scope = match &scope {
            Some(range) => range.contains(&start_line) && range.contains(&end_line),
            None => (start_line..=end_line).contains(&anchor),
        };
        if in_scope {
            matched.push((full, body, start_line, end_line));
        }
    }
    if matched.len() != count {
        return None;
    }
    let check_lines = scope.unwrap_or_else(|| {
        let min_line = matched.iter().map(|m| m.2).min().unwrap_or(anchor);
        let max_line = matched.iter().map(|m| m.3).max().unwrap_or(anchor);
        min_line..max_line + 1
    });
    let scope_start: usize = source
        .split_inclusive('\n')
        .take(check_lines.start)
        .map(str::len)
        .sum();
    let scope_end: usize = scope_start
        + source[scope_start..]
            .split_inclusive('\n')
            .take(check_lines.end.saturating_sub(check_lines.start))
            .map(str::len)
            .sum::<usize>();
    let macros = math::macros(source);
    let mut safe_macros = Vec::new();
    let mut masked = masked.into_iter().peekable();
    let mut if_depth = 0usize;
    let mut at = 0usize;
    while at < scope_end {
        if at >= scope_start && if_depth > 0 {
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
                at += source[at..].find('\n').unwrap_or(source.len() - at);
            }
            b'\\' => {
                let in_scope = at >= scope_start;
                let (cmd, next) = math::command(source, at);
                at = next;
                if let Some((opaque_end, _)) = math::opaque_command_end(source, cmd, at) {
                    at = opaque_end;
                    continue;
                }
                if (cmd.starts_with("if") && cmd != "iff") || matches!(cmd, "else" | "fi") {
                    if in_scope {
                        return None;
                    }
                    if cmd == "fi" {
                        if_depth = if_depth.saturating_sub(1);
                    } else if cmd != "else" {
                        if_depth += 1;
                    }
                    continue;
                }
                if !in_scope {
                    continue;
                }
                if matches!(
                    cmd,
                    "csname"
                        | "savebox"
                        | "sbox"
                        | "setbox"
                        | "usebox"
                        | "phantom"
                        | "hphantom"
                        | "vphantom"
                        | "multiput"
                        | "foreach"
                ) || (cmd == "begin"
                    && math::group(source, at).is_some_and(|(env, _)| &source[env] == "lrbox"))
                {
                    return None;
                }
                let cmd_start = at - cmd.len();
                let at_end = if source
                    .as_bytes()
                    .get(cmd_start)
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
                {
                    cmd_start
                        + source[cmd_start..]
                            .bytes()
                            .take_while(|b| b.is_ascii_alphabetic() || *b == b'@')
                            .count()
                } else {
                    at
                };
                let full_cmd = &source[cmd_start..at_end];
                if macros.contains_key(cmd)
                    || macros.contains_key(full_cmd)
                    || (full_cmd.len() > 1 && full_cmd.contains('@'))
                {
                    at = at_end;
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
                    for candidate in [full_cmd, cmd] {
                        if macros.contains_key(candidate) && !safe_macros.contains(&candidate) {
                            let mut stack = vec![candidate];
                            let mut seen = Vec::new();
                            while let Some(name) = stack.pop() {
                                if safe_macros.contains(&name) || seen.contains(&name) {
                                    continue;
                                }
                                seen.push(name);
                                let body = macros.get(name).and_then(|e| e.and_then(|d| d.body))?;
                                if body.contains("include") || body.contains("lrbox") {
                                    return None;
                                }
                                let mut pos = 0;
                                while let Some(rel) = body[pos..].find('\\') {
                                    let slash = pos + rel;
                                    let after = slash + 1;
                                    let (cmd_alpha, next_alpha) = math::command(body, slash);
                                    let at_len = body[after..]
                                        .bytes()
                                        .take_while(|b| b.is_ascii_alphabetic() || *b == b'@')
                                        .count();
                                    let (sub, next) = if at_len > 0 {
                                        (&body[after..after + at_len], after + at_len)
                                    } else {
                                        (cmd_alpha, next_alpha)
                                    };
                                    pos = next;
                                    if matches!(
                                        sub,
                                        "csname"
                                            | "savebox"
                                            | "sbox"
                                            | "setbox"
                                            | "usebox"
                                            | "phantom"
                                            | "hphantom"
                                            | "vphantom"
                                            | "multiput"
                                            | "foreach"
                                    ) {
                                        return None;
                                    }
                                    if macros.contains_key(sub)
                                        || (sub.len() > 1 && sub.contains('@'))
                                    {
                                        stack.push(sub);
                                    }
                                    if cmd_alpha != sub && macros.contains_key(cmd_alpha) {
                                        stack.push(cmd_alpha);
                                    }
                                }
                            }
                            safe_macros.extend(seen);
                        }
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
    let prefix = &source[..path_start];
    let target_line = u32::try_from(prefix.bytes().filter(|&ch| ch == b'\n').count()).ok()? + 1;
    let line_start = prefix.rfind('\n').map_or(0, |pos| pos + 1);
    Some((target_line, path_start - line_start))
}

pub(crate) fn nonprinting_source_spans(
    source: &str,
) -> impl Iterator<
    Item = (
        usize,
        std::ops::Range<usize>,
        Option<std::ops::Range<usize>>,
    ),
> + '_ {
    let mut at = 0;
    let mut pending_opt_after = None;
    std::iter::from_fn(move || {
        loop {
            if at >= source.len() {
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
            if source.as_bytes()[at] == b'%' {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
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
                        return Some((start, start..end, None));
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
                    return Some((start, start..at, None));
                }
                at = end + usize::from(matches!(source.as_bytes().get(end), Some(b'*' | b'+')));
                let definition = matches!(
                    command,
                    "newcommand"
                        | "renewcommand"
                        | "providecommand"
                        | "DeclareRobustCommand"
                        | "DeclareMathOperator"
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
                            return Some((start, start..end, None));
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
                    return Some((start, start..at, None));
                } else if let Some((heading, contents, full_end)) =
                    math::caption_arguments(source, command, end)
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
                        | "textcolor"
                        | "color"
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
                    return Some((start, body.start..group_end, None));
                } else if let Some(overlay_end) = overlay_argument_end(source, at) {
                    at = overlay_end;
                    return Some((start, end..overlay_end, None));
                }
            } else {
                let limit = pending_opt_after
                    .filter(|&p| p > at)
                    .unwrap_or(source.len());
                at += source.as_bytes()[at + 1..limit]
                    .iter()
                    .position(|&b| matches!(b, b'%' | b'<' | b'\\'))
                    .map_or(limit - at, |pos| pos + 1);
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn source_prose_scored_location(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_frame: bool,
    hidden: &[std::ops::Range<usize>],
    allowed_bytes: Option<std::ops::Range<usize>>,
) -> Option<(u32, usize, isize)> {
    let pdf = words(context);
    let selected = word_index_at(context, &pdf, offset)?;
    let pdf: Vec<_> = pdf.iter().map(|(_, word)| normalized_word(word)).collect();
    let frame_header = within_frame
        && source
            .lines()
            .nth(lines.start)
            .is_some_and(|row| source_line_text(row).contains("\\begin{frame}"));
    let mut hidden = hidden.iter().peekable();
    let mut candidates = Vec::new();
    let mut line_start = 0;
    for (index, raw_row) in source.split_inclusive('\n').enumerate().take(lines.end) {
        let text = source_line_text(raw_row.trim_end_matches(['\r', '\n']));
        if index >= lines.start {
            for (byte, _, word) in source_words(text) {
                let abs = line_start + byte;
                while hidden.peek().is_some_and(|range| range.end <= abs) {
                    hidden.next();
                }
                if (byte == 0 || !text[..byte].ends_with('\\'))
                    && allowed_bytes
                        .as_ref()
                        .is_none_or(|span| span.contains(&abs))
                    && !hidden.peek().is_some_and(|range| range.contains(&abs))
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
