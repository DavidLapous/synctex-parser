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
    if !source.contains(environment) {
        return None;
    }
    let hidden = nonprinting_source_ranges(source);
    environment_bounds_with_hidden(source, anchor, environment, &hidden)
}

fn environment_bounds_with_hidden(
    source: &str,
    anchor: usize,
    environment: &str,
    hidden: &[std::ops::Range<usize>],
) -> Option<(usize, usize)> {
    if !source.contains(environment) {
        return None;
    }
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
    ignored.extend_from_slice(hidden);
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

pub(crate) fn collected_text_closing_line(source: &str, line: u32, column: u32) -> Option<u32> {
    let row = validate_forward_source_position(source, line, column).ok()?;
    // End-of-line is a cursor boundary, not a character in the argument.
    let byte = row.char_indices().nth(column.checked_sub(1)? as usize)?.0;
    let start = source
        .split_inclusive('\n')
        .take(line.checked_sub(1)? as usize)
        .map(str::len)
        .sum::<usize>();
    let body = math::collected_text_argument_at(source, start + byte)?;
    u32::try_from(source[..body.end].bytes().filter(|ch| *ch == b'\n').count())
        .ok()?
        .checked_add(1)
}

pub(crate) fn reference_argument_closing_line(source: &str, line: u32, column: u32) -> Option<u32> {
    let row = validate_forward_source_position(source, line, column).ok()?;
    let byte = row.char_indices().nth(column.checked_sub(1)? as usize)?.0;
    let start = source
        .split_inclusive('\n')
        .take(line.checked_sub(1)? as usize)
        .map(str::len)
        .sum::<usize>();
    let body = reference_argument_body_at(source, start + byte)?;
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

fn tikzpicture_exclusive_bounds(source: &str, anchor: usize, start: usize, end: usize) -> bool {
    let mut line_starts = Vec::with_capacity(end + 2);
    line_starts.push(0usize);
    for (idx, line) in source.split_inclusive('\n').enumerate() {
        line_starts.push(line_starts.last().unwrap() + line.len());
        if idx >= end {
            break;
        }
    }
    if end + 1 >= line_starts.len() || anchor > end {
        return false;
    }
    for row in anchor..start {
        let row_str = &source[line_starts[row]..line_starts[row + 1]];
        if source_line_text(row_str).trim().is_empty() {
            return false;
        }
    }
    let scan_start = line_starts[anchor.min(start)];
    let start_line_byte = line_starts[start];
    let end_line_byte = line_starts[end];
    let end_line_end = line_starts[end + 1];
    let Some(begin_rel) = source_line_text(&source[start_line_byte..line_starts[start + 1]])
        .find("\\begin{tikzpicture}")
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
                match cmd {
                    "centering" | "raggedright" | "raggedleft" | "noindent" => {
                        at = next;
                    }
                    "hbox" | "vbox" | "vcenter" | "centerline" => {
                        let grp = skip_source_space(source, next);
                        let Some((_, grp_end)) = math::group(source, grp) else {
                            return false;
                        };
                        if grp >= tikz_start || grp_end < tikz_end {
                            return false;
                        }
                        outer_end = outer_end.max(grp_end);
                        at = grp + 1;
                    }
                    "makebox" | "framebox" | "subfloat" | "subfigure" => {
                        let mut pos = next;
                        while let Some(opt_end) = optional_argument_end(source, pos) {
                            if opt_end > tikz_start {
                                return false;
                            }
                            pos = opt_end;
                        }
                        let grp = skip_source_space(source, pos);
                        let Some((_, grp_end)) = math::group(source, grp) else {
                            return false;
                        };
                        if grp >= tikz_start || grp_end < tikz_end {
                            return false;
                        }
                        outer_end = outer_end.max(grp_end);
                        at = grp + 1;
                    }
                    "scalebox" => {
                        let arg1 = skip_source_space(source, next);
                        let Some((_, mut pos)) = math::group(source, arg1) else {
                            return false;
                        };
                        if pos > tikz_start {
                            return false;
                        }
                        if let Some(opt_end) = optional_argument_end(source, pos) {
                            if opt_end > tikz_start {
                                return false;
                            }
                            pos = opt_end;
                        }
                        let grp = skip_source_space(source, pos);
                        let Some((_, grp_end)) = math::group(source, grp) else {
                            return false;
                        };
                        if grp >= tikz_start || grp_end < tikz_end {
                            return false;
                        }
                        outer_end = outer_end.max(grp_end);
                        at = grp + 1;
                    }
                    "resizebox" => {
                        let mut pos = skip_source_space(source, next);
                        if source.as_bytes().get(pos) == Some(&b'*') {
                            pos += 1;
                        }
                        for _ in 0..2 {
                            let arg = skip_source_space(source, pos);
                            let Some((_, arg_end)) = math::group(source, arg) else {
                                return false;
                            };
                            if arg_end > tikz_start {
                                return false;
                            }
                            pos = arg_end;
                        }
                        let grp = skip_source_space(source, pos);
                        let Some((_, grp_end)) = math::group(source, grp) else {
                            return false;
                        };
                        if grp >= tikz_start || grp_end < tikz_end {
                            return false;
                        }
                        outer_end = outer_end.max(grp_end);
                        at = grp + 1;
                    }
                    "adjustbox" => {
                        let mut pos = next;
                        if let Some(opt_end) = optional_argument_end(source, pos) {
                            if opt_end > tikz_start {
                                return false;
                            }
                            pos = opt_end;
                        }
                        let arg1 = skip_source_space(source, pos);
                        let Some((_, arg1_end)) = math::group(source, arg1) else {
                            return false;
                        };
                        if arg1_end > tikz_start {
                            return false;
                        }
                        let grp = skip_source_space(source, arg1_end);
                        let Some((_, grp_end)) = math::group(source, grp) else {
                            return false;
                        };
                        if grp >= tikz_start || grp_end < tikz_end {
                            return false;
                        }
                        outer_end = outer_end.max(grp_end);
                        at = grp + 1;
                    }
                    _ => return false,
                }
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
                    pos += source[pos..].find('\n').unwrap_or(source.len() - pos);
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
                    if matches!(
                        cmd,
                        "par"
                            | "newline"
                            | "linebreak"
                            | "pagebreak"
                            | "newpage"
                            | "clearpage"
                            | "cleardoublepage"
                            | "vfill"
                            | "vfil"
                            | "vspace"
                            | "bigskip"
                            | "medskip"
                            | "smallskip"
                            | "hline"
                            | "toprule"
                            | "midrule"
                            | "bottomrule"
                            | "item"
                            | "caption"
                            | "subcaption"
                            | "captionof"
                            | "begin"
                            | "end"
                    ) {
                        break;
                    }
                    if matches!(
                        cmd,
                        "relax"
                            | "hfill"
                            | "hfil"
                            | "hss"
                            | "centering"
                            | "raggedright"
                            | "raggedleft"
                            | "noindent"
                            | "egroup"
                            | "endgroup"
                    ) {
                        pos = next;
                        continue;
                    }
                    if cmd == "label" {
                        let mut lpos = next;
                        if let Some(opt_end) = optional_argument_end(source, lpos) {
                            lpos = opt_end;
                        }
                        let Some((_, grp_end)) =
                            math::group(source, skip_source_space(source, lpos))
                        else {
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

/// Find a `tikzpicture` environment enclosing the one-based SyncTeX line, or
/// immediately following up to 3 box/layout wrapper lines (`\centering`, `\resizebox`).
#[cfg(test)]
pub(crate) fn source_tikzpicture_range(source: &str, line: u32) -> Option<std::ops::Range<usize>> {
    if !source.contains("tikzpicture") {
        return None;
    }
    let hidden = nonprinting_source_ranges(source);
    source_tikzpicture_range_with_hidden(source, line, &hidden)
}

fn source_tikzpicture_range_with_hidden(
    source: &str,
    line: u32,
    hidden: &[std::ops::Range<usize>],
) -> Option<std::ops::Range<usize>> {
    if !source.contains("tikzpicture") {
        return None;
    }
    let anchor = line.checked_sub(1)? as usize;
    if let Some((start, end)) =
        environment_bounds_with_hidden(source, anchor, "tikzpicture", hidden)
    {
        return tikzpicture_exclusive_bounds(source, anchor, start, end).then_some(start..end + 1);
    }
    for offset in 1..=3 {
        let probe = anchor + offset;
        if let Some((start, end)) =
            environment_bounds_with_hidden(source, probe, "tikzpicture", hidden)
            && start == probe
            && tikzpicture_exclusive_bounds(source, anchor, start, end)
        {
            return Some(start..end + 1);
        }
    }
    None
}

pub(crate) struct ForeachLoop<'a> {
    pub header: std::ops::Range<usize>,
    pub body: std::ops::Range<usize>,
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
                at += text[at..].find('\n').unwrap_or(text.len() - at);
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
    if trimmed.starts_with('{')
        && let Some((inner, end)) = math::group(trimmed, 0)
        && end == trimmed.len()
    {
        return trimmed[inner].trim();
    }
    trimmed
}

fn valid_foreach_value(item: &str) -> bool {
    !item.is_empty() && !item.contains(['{', '}', '$', '#', '%']) && !item.contains("^^")
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
) -> Option<(bool, Vec<&str>, Option<std::ops::Range<usize>>, usize)> {
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

fn foreach_body_range(
    source: &str,
    body_start: usize,
    depth: usize,
) -> Option<std::ops::Range<usize>> {
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
                scan += source[scan..].find('\n').unwrap_or(source.len() - scan);
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
    source: &'a str,
    lines: &std::ops::Range<usize>,
    extra_sources: &[&'a str],
    operation: &crate::Operation,
) -> Vec<ForeachLoop<'a>> {
    if operation.check().is_err() {
        return Vec::new();
    }
    if !source.contains("\\foreach") {
        return Vec::new();
    }
    let mut macro_sources = Vec::with_capacity(extra_sources.len() + 1);
    macro_sources.extend_from_slice(extra_sources);
    macro_sources.push(source);
    let macros = math::macros_from_sources(&macro_sources, operation);
    let start_byte: usize = source
        .split_inclusive('\n')
        .take(lines.start)
        .map(str::len)
        .sum();
    let end_byte: usize = start_byte
        + source[start_byte..]
            .split_inclusive('\n')
            .take(lines.end.saturating_sub(lines.start))
            .map(str::len)
            .sum::<usize>();
    let mut loops = Vec::new();
    let mut at = 0usize;
    while at < end_byte {
        match source.as_bytes()[at] {
            b'%' => {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
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
    range: &std::ops::Range<usize>,
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
    let mut combinations: Vec<Vec<(&'a str, &'a str)>> = vec![Vec::new()];
    for lp in enclosing {
        let items = lp.items.as_ref()?;
        if combinations.len().saturating_mul(items.len()) > 32 {
            return None;
        }
        let mut next = Vec::new();
        for prefix in &combinations {
            for item in items {
                let mut combined = prefix.clone();
                for (&var, val) in lp.vars.iter().zip(item.iter()) {
                    combined.push((var, val.as_str()));
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
        let visible = source_line_text(text);
        source_words(visible).into_iter().find(|(start, end, _)| {
            *start <= byte && byte < *end && !has_unescaped_backslash_before(visible, *start)
        })
    }) && word == pdf_word
    {
        return Some(start);
    }
    let line_start: usize = source
        .split_inclusive('\n')
        .take(line as usize - 1)
        .map(str::len)
        .sum();
    let abs = line_start + byte;
    (math::in_region(source, abs) || source.as_bytes().get(abs) == Some(&b'\\')).then_some(byte)
}

/// Refine prose and mathematical atoms without expanding arbitrary TeX macros.
#[cfg(test)]
pub(crate) fn source_word_location(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    radius: u32,
) -> Option<(u32, usize)> {
    source_word_location_with_sources(
        source,
        line,
        context,
        offset,
        radius,
        &[],
        &crate::Operation::default(),
    )
    .ok()
    .flatten()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UnverifiedWordLocation {
    pub line: u32,
    pub byte: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WordLocationMode {
    Strict,
    Guess,
}

impl WordLocationMode {
    pub(super) fn macros<'a>(
        self,
        sources: &[&'a str],
        operation: &crate::Operation,
    ) -> std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>> {
        match self {
            Self::Strict => math::macros_from_sources(sources, operation),
            Self::Guess => math::macros_from_sources_for_guess(sources, operation),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ExecutionRoot<'a> {
    pub(crate) source: &'a str,
    pub(crate) package_initialization: &'a [std::ops::Range<usize>],
}

impl ExecutionRoot<'_> {
    fn package_initialization_at(self, source: &str, start: usize) -> bool {
        (source.as_ptr() as usize + start)
            .checked_sub(self.source.as_ptr() as usize)
            .is_some_and(|offset| {
                self.package_initialization
                    .iter()
                    .any(|range| range.contains(&offset))
            })
    }
}

// This result is never occurrence ownership, even when the lexical match is unique.
#[allow(clippy::too_many_arguments)]
pub(crate) fn source_word_guess_with_sources(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    radius: u32,
    extra_sources: &[&str],
    operation: &crate::Operation,
) -> Result<Option<UnverifiedWordLocation>, ()> {
    source_word_location_with_sources_mode(
        source,
        line,
        context,
        offset,
        radius,
        extra_sources,
        WordLocationMode::Guess,
        operation,
    )
    .map(|location| location.map(|(line, byte)| UnverifiedWordLocation { line, byte }))
}

/// Run-in headings are shipped when TeX starts their following body paragraph.
pub(crate) fn run_in_heading_compiler_line(source: &str, line: u32, column: u32) -> Option<u32> {
    let row = source
        .split_inclusive('\n')
        .nth(line.checked_sub(1)? as usize)?;
    let row_start: usize = source
        .split_inclusive('\n')
        .take(line as usize - 1)
        .map(str::len)
        .sum();
    let byte = row_start + row.char_indices().nth(column.checked_sub(1)? as usize)?.0;
    let mut skip_until = 0;
    let mut hidden = nonprinting_source_spans(source).peekable();
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
        skip_until = math::quoted_token_end(source, name, end)
            .or_else(|| math::opaque_command_end(source, name, end).map(|(end, _)| end))
            .unwrap_or(end);
        if skip_until > byte {
            return None;
        }
        if !matches!(name, "paragraph" | "subparagraph") {
            continue;
        }
        end += usize::from(source.as_bytes().get(end) == Some(&b'*'));
        end = optional_argument_end(source, end).unwrap_or(end);
        let Some((title, end)) = math::group(source, skip_source_space(source, end)) else {
            continue;
        };
        if !title.contains(&byte) {
            skip_until = end;
            continue;
        }
        let mut body = skip_source_space(source, end);
        while source.as_bytes().get(body) == Some(&b'\\') {
            let (name, end) = math::command(source, body);
            if !matches!(name, "label" | "index") {
                break;
            }
            let (_, end) = math::group(source, skip_source_space(source, end))?;
            body = skip_source_space(source, end);
        }
        if body == source.len()
            || (source.as_bytes()[body] == b'\\'
                && matches!(
                    math::command(source, body).0,
                    "end"
                        | "part"
                        | "chapter"
                        | "section"
                        | "subsection"
                        | "subsubsection"
                        | "paragraph"
                        | "subparagraph"
                ))
        {
            return None;
        }
        return Some(source[..body].bytes().filter(|&b| b == b'\n').count() as u32 + 1);
    }
    None
}

pub(crate) fn line_start_word_compiler_line(source: &str, line: u32, column: u32) -> Option<u32> {
    let previous_line = line.checked_sub(1)?;
    let mut rows = source.lines();
    let previous_row = rows.nth(previous_line.checked_sub(1)? as usize)?;
    let previous = source_line_text(previous_row);
    let row = rows.next()?;
    let word = row.trim_start_matches([' ', '\t']);
    if column as usize != row.len() - word.len() + 1
        || !word.chars().next()?.is_alphanumeric()
        || previous.trim().is_empty()
        // shortcut: grouped or non-math command boundaries need scope proof for read-ahead.
        || previous.contains(['{', '}'])
        || previous
            .match_indices('\\')
            .any(|(at, _)| !math::in_region(previous, at))
    {
        return None;
    }
    let previous_start = previous_row.as_ptr() as usize - source.as_ptr() as usize;
    let previous_end = previous_start + previous_row.len();
    let mut skip_until = 0;
    let mut hidden = nonprinting_source_spans(source).peekable();
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
        skip_until = math::quoted_token_end(source, name, end)
            .or_else(|| math::opaque_command_end(source, name, end).map(|(end, _)| end))
            .unwrap_or(end);
        if !matches!(
            name,
            "part"
                | "chapter"
                | "section"
                | "subsection"
                | "subsubsection"
                | "paragraph"
                | "subparagraph"
        ) {
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

// Opaque word ownership also prohibits document-metadata fallback.
#[allow(clippy::too_many_arguments)]
pub(crate) fn source_word_location_with_sources(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    radius: u32,
    extra_sources: &[&str],
    operation: &crate::Operation,
) -> Result<Option<(u32, usize)>, ()> {
    source_word_location_with_sources_mode(
        source,
        line,
        context,
        offset,
        radius,
        extra_sources,
        WordLocationMode::Strict,
        operation,
    )
}

#[allow(clippy::too_many_arguments)]
fn source_word_location_with_sources_mode(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    radius: u32,
    extra_sources: &[&str],
    mode: WordLocationMode,
    operation: &crate::Operation,
) -> Result<Option<(u32, usize)>, ()> {
    operation.check().map_err(|_| ())?;
    let hidden = nonprinting_source_ranges(source);
    if let Some(tikz) = source_tikzpicture_range_with_hidden(source, line, &hidden)
        && let Some(loc) = source_word_location_in_range_with_hidden(
            source,
            line,
            context,
            offset,
            tikz,
            true,
            extra_sources,
            mode,
            operation,
            None,
        )?
    {
        return Ok(Some(loc));
    }
    let anchor = line.checked_sub(1).map(|l| l as usize);
    let scope = math::collected_text_lines(source, line)
        .or_else(|| {
            environment_bounds_with_hidden(source, anchor?, "frame", &hidden)
                .map(|(start, end)| start..end + 1)
        })
        .or_else(|| {
            environment_bounds_with_hidden(source, anchor?, "tabularx", &hidden)
                .map(|(start, end)| start..end + 1)
        })
        .or_else(|| math::math_lines(source, line));
    let within_scope = scope.is_some();
    let lines = scope.unwrap_or_else(|| {
        line.saturating_sub(radius.saturating_add(1)) as usize
            ..(line as usize).saturating_add(radius as usize)
    });
    source_word_location_in_range_with_hidden(
        source,
        line,
        context,
        offset,
        lines,
        within_scope,
        extra_sources,
        mode,
        operation,
        None,
    )
}

struct MacroArgRanges {
    hidden: Vec<std::ops::Range<usize>>,
    opaque: Vec<std::ops::Range<usize>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InvocationKind {
    Command,
    Environment,
    EnvironmentEnd,
    MathDelimiter,
    LiteralOutput,
    GlobalDeclaration,
}

fn class_package_declaration(command: &str) -> bool {
    matches!(command, "documentclass" | "usepackage" | "RequirePackage")
}

fn class_package_declaration_end(source: &str, mut at: usize) -> Option<usize> {
    while let Some(end) = optional_argument_end(source, at) {
        at = end;
    }
    let (_, mut end) = math::group(source, skip_source_space(source, at))?;
    while let Some(optional_end) = optional_argument_end(source, end) {
        end = optional_end;
    }
    Some(end)
}

// Potential dependencies may overapproximate; output and discards retain semantic extents.
fn invoked_commands<'a>(
    source: &'a str,
    potential: bool,
    macros: &std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    operation: &crate::Operation,
) -> Vec<(usize, &'a str, usize, InvocationKind)> {
    invoked_commands_with_literals(source, potential, macros, false, operation)
}

fn invoked_commands_with_literals<'a>(
    source: &'a str,
    potential: bool,
    macros: &std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    include_literals: bool,
    operation: &crate::Operation,
) -> Vec<(usize, &'a str, usize, InvocationKind)> {
    let mode = if include_literals {
        WordLocationMode::Guess
    } else {
        WordLocationMode::Strict
    };
    let hidden: Vec<_> =
        nonprinting_source_spans_with_mode(source, Some(macros), mode, Some(operation)).collect();
    let mut hidden = hidden.iter().peekable();
    let mut commands = Vec::new();
    let mut at = 0;
    while at < source.len() {
        if operation.check().is_err() {
            return commands;
        }
        while hidden.peek().is_some_and(|(_, range, _)| range.end <= at) {
            hidden.next();
        }
        if let Some((_, range, _)) = hidden.peek()
            && range.contains(&at)
        {
            at = range.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => at += source[at..].find('\n').unwrap_or(source.len() - at),
            b'\\' => {
                let start = at;
                let (name, end) = if potential {
                    math::static_command(source, at)
                } else {
                    math::control_sequence(source, at)
                };
                at = end;
                // Lexical builtin behavior cannot override an explicitly changed meaning.
                if macros.contains_key(name)
                    || mode == WordLocationMode::Strict
                        && uncertain_note_invocation(name, macros, WordLocationMode::Strict)
                {
                    commands.push((start, name, end, InvocationKind::Command));
                    continue;
                }
                if include_literals
                    && class_package_declaration(name)
                    && let Some((head, span, _)) = hidden.peek()
                    && *head == start
                    && span.start == end
                {
                    commands.push((start, name, span.end, InvocationKind::GlobalDeclaration));
                    at = span.end;
                    continue;
                }
                if name == "detokenize" {
                    if let Some((_, end)) = math::group(source, at) {
                        at = end;
                    }
                    if include_literals {
                        commands.push((start, name, at, InvocationKind::LiteralOutput));
                    }
                    continue;
                }
                if let Some(quoted_end) = math::quoted_token_end(source, name, at) {
                    if name == "noexpand" {
                        commands.push((start, name, end, InvocationKind::Command));
                    } else if include_literals {
                        commands.push((start, name, quoted_end, InvocationKind::LiteralOutput));
                    }
                    at = quoted_end;
                } else if let Some((end, nonprinting)) = math::opaque_command_end(source, name, at)
                {
                    if include_literals && !nonprinting {
                        commands.push((start, name, end, InvocationKind::LiteralOutput));
                    }
                    at = end;
                } else if matches!(name, "begin" | "end")
                    && let Some((environment, group_end)) =
                        math::group(source, skip_source_space(source, at))
                {
                    let environment = source[environment].trim();
                    let (environment, kind) = if name == "end" {
                        (
                            math::environment_end_name(macros, environment).unwrap_or(environment),
                            InvocationKind::EnvironmentEnd,
                        )
                    } else {
                        (environment, InvocationKind::Environment)
                    };
                    commands.push((start, environment, group_end, kind));
                    at = group_end;
                } else {
                    commands.push((start, name, end, InvocationKind::Command));
                }
            }
            b'$' if potential => {
                commands.push((
                    at,
                    &source[at..at + 1],
                    at + 1,
                    InvocationKind::MathDelimiter,
                ));
                at += 1;
            }
            _ => {
                at += source.as_bytes()[at + 1..]
                    .iter()
                    .position(|byte| matches!(byte, b'%' | b'\\' | b'$'))
                    .map_or(source.len() - at, |offset| offset + 1);
            }
        }
    }
    commands
}

#[derive(Clone, Copy)]
pub(super) struct MacroContext<'a, 'b> {
    pub macros: &'b std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    pub definitions: &'b std::collections::BTreeMap<&'a str, Vec<StoredBody<'a>>>,
    pub operation: &'b crate::Operation,
}

#[derive(Clone, Copy)]
pub(super) struct StoredBody<'a> {
    source: &'a str,
    package_initialization: bool,
}

fn note_macro_definitions<'a>(
    sources: &[&'a str],
    note_macros: &mut std::collections::BTreeSet<&'a str>,
    execution_root: Option<ExecutionRoot<'_>>,
    operation: Option<&crate::Operation>,
) -> std::collections::BTreeMap<&'a str, Vec<StoredBody<'a>>> {
    let mut definitions = std::collections::BTreeMap::<_, Vec<_>>::new();
    let mut pending = sources.to_vec();
    while let Some(source) = pending.pop() {
        if operation.is_some_and(|op| op.check().is_err()) {
            return definitions;
        }
        for (start, span, _) in nonprinting_source_spans(source) {
            if operation.is_some_and(|op| op.check().is_err()) {
                return definitions;
            }
            if source.as_bytes().get(start) != Some(&b'\\') {
                continue;
            }
            let package_initialization =
                execution_root.is_some_and(|root| root.package_initialization_at(source, start));
            let (command, mut at, expanded_target) = math::definition_command(source, start);
            if let Some((name, body, _)) = environment_definition(source, command, at) {
                definitions.entry(name).or_default().push(StoredBody {
                    source: body,
                    package_initialization,
                });
                pending.push(body);
                continue;
            }
            if !math::is_macro_definition_command(command) || command == "csname" {
                continue;
            }
            let (name, body) = if matches!(command, "let" | "futurelet") {
                let alias = math::macro_alias(source, command, at, expanded_target);
                let body = &source[alias.value];
                if body.starts_with('\\') {
                    let (value, end) = math::control_sequence(body, 0);
                    if end == body.len() && matches!(value, "note" | "csname") {
                        // Aliases snapshot meaning before a later note redefinition.
                        note_macros.insert(alias.name);
                    }
                }
                (alias.name, body)
            } else {
                at = skip_source_space(source, at);
                at += usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
                at = skip_source_space(source, at);
                let (name, next) = math::macro_target_name(source, at)
                    .unwrap_or(("**", math::macro_target_end(source, at).unwrap_or(at)));
                (name, &source[next..span.end])
            };
            definitions.entry(name).or_default().push(StoredBody {
                source: body,
                package_initialization,
            });
            pending.push(body);
        }
    }
    definitions
}

fn uncertain_note_call_end(
    source: &str,
    mut at: usize,
    limit: usize,
    definitions: Option<&Vec<StoredBody<'_>>>,
) -> usize {
    if definitions.is_none() {
        // shortcut: a missing signature cannot bound consumption within this query scope.
        return limit;
    }
    // Delimited primitive definitions expose their terminator before the body.
    if let Some(definitions) = definitions {
        for definition in definitions {
            let body = definition.source.trim();
            if body.starts_with('\\') && math::control_sequence(body, 0).1 == body.len() {
                // An alias token alone does not establish its captured signature.
                return limit;
            }
            let prefix = definition.source.split('{').next().unwrap_or("");
            let Some(parameter) = prefix.rfind('#') else {
                continue;
            };
            let Some(delimiter) = prefix.get(parameter + 2..).map(str::trim) else {
                continue;
            };
            if delimiter.is_empty() {
                continue;
            }
            let mut cursor = at;
            while cursor < limit {
                if source[cursor..].starts_with(delimiter)
                    && !(delimiter.starts_with('\\')
                        && delimiter
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphabetic)
                        && source
                            .as_bytes()
                            .get(cursor + delimiter.len())
                            .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == b'@'))
                {
                    return cursor + delimiter.len();
                }
                if let Some((_, end)) = math::group(source, cursor) {
                    cursor = end;
                } else if source.as_bytes()[cursor] == b'%' {
                    cursor += source[cursor..].find('\n').unwrap_or(source.len() - cursor);
                } else {
                    cursor += source[cursor..].chars().next().unwrap().len_utf8();
                }
            }
            return limit;
        }
    }
    let arguments = definitions
        .into_iter()
        .flatten()
        .flat_map(|definition| {
            definition
                .source
                .match_indices('#')
                .filter_map(move |(at, _)| {
                    definition
                        .source
                        .as_bytes()
                        .get(at + 1)
                        .copied()
                        .filter(|digit| (b'1'..=b'9').contains(digit))
                        .map(|digit| usize::from(digit - b'0'))
                })
        })
        .max()
        .unwrap_or(1);
    for _ in 0..arguments {
        let open = skip_source_space(source, at).min(limit);
        if open == limit {
            break;
        }
        at = if let Some(end) = optional_argument_end(source, open)
            .or_else(|| overlay_argument_end(source, open))
            .or_else(|| math::group(source, open).map(|(_, end)| end))
        {
            end
        } else if source.as_bytes().get(open) == Some(&b'\\') {
            math::command(source, open).1
        } else {
            open + source[open..].chars().next().map_or(0, char::len_utf8)
        };
    }
    at.min(limit)
}

pub(super) fn known_formatting_switch(name: &str) -> bool {
    matches!(
        name,
        "normalfont"
            | "rmfamily"
            | "sffamily"
            | "ttfamily"
            | "bfseries"
            | "mdseries"
            | "lfseries"
            | "itshape"
            | "slshape"
            | "scshape"
            | "upshape"
            | "rm"
            | "bf"
            | "it"
            | "sl"
            | "sf"
            | "tt"
            | "sc"
            | "tiny"
            | "scriptsize"
            | "footnotesize"
            | "small"
            | "normalsize"
            | "large"
            | "Large"
            | "LARGE"
            | "huge"
            | "Huge"
    )
}

fn known_note_independent_command(name: &str, mode: WordLocationMode) -> bool {
    !matches!(
        crate::symbols::classify_command(name),
        crate::symbols::MathCommand::Unknown
    ) || matches!(
        name,
        "begin"
            | "end"
            | "item"
            | "frametitle"
            | "framesubtitle"
            | "title"
            | "subtitle"
            | "author"
            | "date"
            | "institute"
            | "maketitle"
            | "titlepage"
            | "par"
            | "pause"
            | "centering"
            | "raggedright"
            | "raggedleft"
            | "textbf"
            | "textit"
            | "emph"
            | "textrm"
            | "textsf"
            | "texttt"
            | "textnormal"
            | "textcolor"
            | "href"
            | "url"
            | "caption"
            | "footnote"
            | "label"
            | "ref"
            | "eqref"
            | "pageref"
            | "autoref"
            | "cref"
            | "Cref"
            | "crefname"
            | "Crefname"
            | "crefformat"
            | "Crefformat"
            | "newtheorem"
            | "theoremstyle"
            | "cite"
    ) || mode == WordLocationMode::Guess
        && (known_formatting_switch(name)
            || matches!(
                name,
                "begingroup"
                    | "endgroup"
                    | "bgroup"
                    | "egroup"
                    | "smallskip"
                    | "medskip"
                    | "bigskip"
                    | "part"
                    | "chapter"
                    | "section"
                    | "subsection"
                    | "subsubsection"
                    | "paragraph"
                    | "subparagraph"
                    | "subref"
                    | "noindent"
                    | "rule"
                    | "vspace"
                    | "hspace"
                    | "index"
                    | "hypertarget"
                    | "clearpage"
                    | "newpage"
                    | "pagebreak"
                    | "pagestyle"
                    | "thispagestyle"
            ))
        || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
}

fn uncertain_note_invocation(
    name: &str,
    macros: &std::collections::BTreeMap<&str, Option<math::MacroDef<'_>>>,
    mode: WordLocationMode,
) -> bool {
    match mode {
        WordLocationMode::Strict => false,
        WordLocationMode::Guess => {
            !matches!(macros.get(name), Some(Some(_)))
                && (macros.contains_key(name)
                    || name != "note" && !known_note_independent_command(name, mode))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn discarded_macro_arg_ranges(
    source: &str,
    lines: &std::ops::Range<usize>,
    extra_sources: &[&str],
    mode: WordLocationMode,
    operation: &crate::Operation,
    execution_scope: Option<&std::ops::Range<usize>>,
) -> Option<MacroArgRanges> {
    operation.check().ok()?;
    let (start_byte, end_byte) = if let Some(scope) = execution_scope {
        (scope.start, scope.end)
    } else {
        let start = source
            .split_inclusive('\n')
            .take(lines.start)
            .map(str::len)
            .sum();
        let end = start
            + source[start..]
                .split_inclusive('\n')
                .take(lines.end.saturating_sub(lines.start))
                .map(str::len)
                .sum::<usize>();
        (start, end)
    };
    let mut macro_sources = Vec::with_capacity(extra_sources.len() + 1);
    macro_sources.extend_from_slice(extra_sources);
    macro_sources.push(source);
    let mut macros = mode.macros(&macro_sources, operation);
    let mut note_macros = std::collections::BTreeSet::new();
    if !matches!(macros.get("note"), Some(Some(_))) {
        note_macros.insert("note");
    }
    let definitions =
        note_macro_definitions(&macro_sources, &mut note_macros, None, Some(operation));
    let mut discarded = Vec::new();
    let mut updates = Vec::new();
    for notes in [false, true] {
        loop {
            for (&name, entry) in &macros {
                operation.check().ok()?;
                let Some(def) = entry else { continue };
                if def.used_params == 0 || !def.body_source.contains('\\') {
                    continue;
                }
                let body = def.body_source;
                let Some(hidden) = discarded_macro_args_with_macros(
                    body,
                    true,
                    0..body.len(),
                    MacroContext {
                        macros: &macros,
                        definitions: &definitions,
                        operation,
                    },
                    notes,
                    &note_macros,
                    mode,
                ) else {
                    continue;
                };
                let mut used_params = 0u16;
                let mut line_start = 0;
                for line in body.split_inclusive('\n') {
                    operation.check().ok()?;
                    for (at, _) in source_line_text(line).match_indices('#') {
                        if has_unescaped_backslash_before(line, at) {
                            continue;
                        }
                        if !hidden
                            .iter()
                            .any(|range| range.contains(&(line_start + at)))
                            && let Some(digit) = line.as_bytes().get(at + 1)
                            && (b'1'..=b'9').contains(digit)
                        {
                            used_params |= 1 << (digit - b'1');
                        }
                    }
                    line_start += line.len();
                }
                used_params &= def.used_params;
                if used_params != def.used_params {
                    updates.push((name, used_params));
                }
            }
            if updates.is_empty() {
                break;
            }
            // Visibility only decreases, including through nested wrappers.
            for (name, used_params) in updates.drain(..) {
                macros.get_mut(name).unwrap().as_mut().unwrap().used_params = used_params;
            }
        }
        if !notes {
            loop {
                let mut changed = false;
                for (&name, bodies) in &definitions {
                    operation.check().ok()?;
                    if note_macros.contains(name) {
                        continue;
                    }
                    for body in bodies {
                        let body = body.source;
                        let discarded = discarded_macro_args_with_macros(
                            body,
                            true,
                            0..body.len(),
                            MacroContext {
                                macros: &macros,
                                definitions: &definitions,
                                operation,
                            },
                            false,
                            &note_macros,
                            mode,
                        )?;
                        if invoked_commands(body, true, &macros, operation).iter().any(
                            |(start, cmd, _, kind)| {
                                *kind != InvocationKind::MathDelimiter
                                    && note_macros.contains(cmd)
                                    && !discarded.iter().any(|span| span.contains(start))
                            },
                        ) {
                            note_macros.insert(name);
                            changed = true;
                            break;
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            discarded = discarded_macro_args_with_macros(
                source,
                false,
                start_byte..end_byte,
                MacroContext {
                    macros: &macros,
                    definitions: &definitions,
                    operation,
                },
                false,
                &note_macros,
                mode,
            )?;
        }
    }
    let mut opaque = discarded_macro_args_with_macros(
        source,
        false,
        start_byte..end_byte,
        MacroContext {
            macros: &macros,
            definitions: &definitions,
            operation,
        },
        true,
        &note_macros,
        mode,
    )?;
    // A discarded outer argument cannot execute a note nested inside it.
    for hidden in &discarded {
        let mut remaining = Vec::new();
        for range in opaque.drain(..) {
            if range.start < hidden.start {
                remaining.push(range.start..range.end.min(hidden.start));
            }
            if range.end > hidden.end {
                remaining.push(range.start.max(hidden.end)..range.end);
            }
        }
        opaque = remaining;
    }
    let mut hidden: Vec<_> =
        nonprinting_source_spans_with_mode(source, Some(&macros), mode, Some(operation))
            .map(|(_, range, _)| range)
            .collect();
    hidden.extend(discarded);
    hidden.sort_unstable_by_key(|range| (range.start, range.end));
    Some(MacroArgRanges { hidden, opaque })
}

fn note_argument(
    source: &str,
    mut at: usize,
    limit: usize,
    parameters: bool,
) -> Option<(std::ops::Range<usize>, usize)> {
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
                _ => {}
            }
        }
        return Some((arg, end));
    }
}

fn discarded_macro_args_with_macros(
    source: &str,
    parameters: bool,
    range: std::ops::Range<usize>,
    context: MacroContext<'_, '_>,
    notes: bool,
    note_macros: &std::collections::BTreeSet<&str>,
    mode: WordLocationMode,
) -> Option<Vec<std::ops::Range<usize>>> {
    let MacroContext {
        macros,
        definitions,
        operation,
    } = context;
    operation.check().ok()?;
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    // Without a known unused parameter, this pass cannot produce a discarded range.
    if !notes
        && !source.contains("phantom")
        && !source.contains("hspace")
        && !source.contains("vspace")
        && !source.contains("index")
        && !macros
            .values()
            .flatten()
            .any(|def| (0..def.arity).any(|idx| def.used_params & (1 << idx) == 0))
    {
        return Some(ranges);
    }
    let mut note_end = range.start;
    for (start, cmd, mut at, kind) in invoked_commands_with_literals(
        source,
        false,
        macros,
        mode == WordLocationMode::Guess,
        operation,
    ) {
        operation.check().ok()?;
        if !range.contains(&start)
            || start < note_end
            || ranges.iter().any(|range| range.contains(&start))
        {
            continue;
        }
        if kind == InvocationKind::EnvironmentEnd {
            continue;
        }
        if !notes
            && kind == InvocationKind::Command
            && matches!(
                cmd,
                "phantom" | "hphantom" | "vphantom" | "hspace" | "vspace" | "index"
            )
            && !macros.contains_key(cmd)
        {
            let mut arg_at =
                at + usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
            while let Some(opt_end) =
                optional_argument_end(source, arg_at).or_else(|| overlay_argument_end(source, arg_at))
            {
                arg_at = opt_end;
            }
            if let Some((_, end)) = note_argument(source, arg_at, range.end, parameters) {
                ranges.push(start..end);
            }
            continue;
        }
        if kind == InvocationKind::Command && cmd == "note" && !macros.contains_key("note") {
            let (_, end) = note_argument(source, at, range.end, parameters)?;
            // The note owns these tokens; their heads do not execute in place.
            note_end = end;
            if notes {
                ranges.push(start..end);
            }
            continue;
        }
        if notes && cmd == "csname" {
            ranges.push(start..range.end);
            continue;
        }
        if notes
            && !matches!(macros.get(cmd), Some(Some(_)))
            && (note_macros.contains(cmd)
                || kind == InvocationKind::Command && uncertain_note_invocation(cmd, macros, mode))
        {
            let end = uncertain_note_call_end(source, at, range.end, definitions.get(cmd));
            ranges.push(start..end);
        }
        if let Some(Some(def)) = macros.get(cmd)
            && def.arity > 0
        {
            if notes && note_macros.contains(cmd) {
                ranges.push(start..at);
            }
            for idx in 0..def.arity {
                let Some((_, end)) = math::argument(source, at, range.end, parameters) else {
                    break;
                };
                if (def.used_params & (1 << idx)) == 0 {
                    ranges.push(at..end);
                }
                at = end;
            }
        } else if notes && note_macros.contains(cmd) && matches!(macros.get(cmd), Some(Some(_))) {
            ranges.push(start..at);
        }
    }
    Some(ranges)
}

/// Only a compiler-proven fragile body may exclude the syntactic frame header.
pub(crate) fn source_word_location_in_frame_body_with_sources(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    extra_sources: &[&str],
    operation: &crate::Operation,
) -> Result<Option<(u32, usize)>, ()> {
    operation.check().map_err(|_| ())?;
    let frame = source_frame_range(source, line).ok_or(())?;
    let mut body_start = frame_header_after_begin(source, frame.start).ok_or(())?;
    loop {
        operation.check().map_err(|_| ())?;
        let Some(at) = skip_frame_header_space(source, body_start) else {
            break;
        };
        if let Some(end) =
            overlay_argument_end(source, at).or_else(|| optional_argument_end(source, at))
        {
            body_start = end;
        } else {
            break;
        }
    }
    for _ in 0..2 {
        operation.check().map_err(|_| ())?;
        let Some(at) = skip_frame_header_space(source, body_start) else {
            break;
        };
        if source.as_bytes().get(at) != Some(&b'{') {
            break;
        }
        body_start = math::group(source, at).ok_or(())?.1;
    }
    let end_row_start: usize = source
        .split_inclusive('\n')
        .take(frame.end.checked_sub(1).ok_or(())?)
        .map(str::len)
        .sum();
    let end_row = source_line_text(source[end_row_start..].split('\n').next().ok_or(())?);
    let hidden = nonprinting_source_ranges(source);
    let body_end = end_row
        .match_indices("\\end")
        .filter(|(at, _)| {
            !has_unescaped_backslash_before(end_row, *at)
                && !hidden
                    .iter()
                    .any(|range| range.contains(&(end_row_start + at)))
        })
        .find_map(|(at, _)| {
            let start = end_row_start + at;
            let (env, _) = math::group(source, start + "\\end".len())?;
            (&source[env] == "frame").then_some(start)
        })
        .ok_or(())?;
    if body_start > body_end {
        return Err(());
    }
    source_word_location_in_range_with_hidden(
        source,
        line,
        context,
        offset,
        frame,
        true,
        extra_sources,
        WordLocationMode::Strict,
        operation,
        Some(&(body_start..body_end)),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn source_word_location_in_range_with_sources(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_scope: bool,
    extra_sources: &[&str],
    operation: &crate::Operation,
) -> Result<Option<(u32, usize)>, ()> {
    source_word_location_in_range_with_hidden(
        source,
        line,
        context,
        offset,
        lines,
        within_scope,
        extra_sources,
        WordLocationMode::Strict,
        operation,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn source_word_guess_in_range_with_sources(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_scope: bool,
    extra_sources: &[&str],
    operation: &crate::Operation,
) -> Result<Option<UnverifiedWordLocation>, ()> {
    source_word_location_in_range_with_hidden(
        source,
        line,
        context,
        offset,
        lines,
        within_scope,
        extra_sources,
        WordLocationMode::Guess,
        operation,
        None,
    )
    .map(|location| location.map(|(line, byte)| UnverifiedWordLocation { line, byte }))
}

#[allow(clippy::too_many_arguments)]
fn source_word_location_in_range_with_hidden(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_scope: bool,
    extra_sources: &[&str],
    mode: WordLocationMode,
    operation: &crate::Operation,
    execution_scope: Option<&std::ops::Range<usize>>,
) -> Result<Option<(u32, usize)>, ()> {
    operation.check().map_err(|_| ())?;
    let MacroArgRanges { mut hidden, opaque } = discarded_macro_arg_ranges(
        source,
        &lines,
        extra_sources,
        mode,
        operation,
        execution_scope,
    )
    .ok_or(())?;
    if let Some(scope) = execution_scope {
        hidden.extend([0..scope.start, scope.end..source.len()]);
        hidden.sort_unstable_by_key(|range| range.start);
    }
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
    if !within_scope && closing_layout {
        return Ok(None);
    }
    let loops = foreach_loops(source, &lines, extra_sources, operation);
    let prose = source_prose_scored_location_with_mode(
        source,
        line,
        context,
        offset,
        lines.clone(),
        within_scope,
        &hidden,
        &opaque,
        None,
        &loops,
        extra_sources,
        mode,
        operation,
    )?;
    let mut candidate = math::source_location_with_context_mode(
        source,
        line,
        lines.clone(),
        within_scope,
        context,
        offset,
        prose,
        extra_sources,
        &loops,
        &opaque,
        &[],
        mode,
        operation,
        execution_scope,
    )?
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
    });
    let prose_score = prose.map_or(0, |(_, _, score)| score);
    match source_reference_argument_location(
        source,
        line,
        &lines,
        within_scope,
        context,
        offset,
        extra_sources,
        &hidden,
        &loops,
        &opaque,
        mode,
        operation,
    ) {
        Ok(Some((ref_row, ref_byte, ref_score, ref_both_sides)))
            if candidate.is_none()
                || (prose_score == 0
                    && candidate.is_some_and(|(row, _)| row != line)
                    && ref_score > 0)
                || (ref_both_sides && ref_score > prose_score) =>
        {
            candidate = Some((ref_row, ref_byte));
        }
        Ok(_) => {}
        Err(()) => {
            operation.check().map_err(|_| ())?;
            if candidate.is_none() {
                return Err(());
            }
        }
    }
    if candidate.is_none() && !opaque.is_empty() {
        // An opaque tie is not evidence for document-metadata ownership.
        return Err(());
    }
    if let Some((row, byte)) = candidate {
        let absolute = source
            .split_inclusive('\n')
            .take(row as usize - 1)
            .map(str::len)
            .sum::<usize>()
            + byte;
        // Removing opaque candidates can make an unrelated literal look unique.
        if opaque.iter().any(|range| range.contains(&absolute)) {
            return Err(());
        }
    }
    Ok(candidate)
}

fn is_direct_reference_command(name: &str) -> bool {
    matches!(
        name,
        "ref" | "eqref" | "pageref" | "autoref" | "cref" | "Cref"
    )
}

fn valid_reference_label_start(
    source: &str,
    span: &std::ops::Range<usize>,
) -> Option<(usize, Vec<String>)> {
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
                at += source[at..span.end].find('\n').map_or(span.end - at, |p| p + 1);
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

fn reference_argument_body_at(source: &str, target: usize) -> Option<std::ops::Range<usize>> {
    if target > source.len()
        || !["\\ref", "\\eqref", "\\pageref", "\\autoref", "\\cref", "\\Cref"]
            .iter()
            .any(|needle| source[..target].contains(needle))
    {
        return None;
    }
    for (cmd_start, span, _) in nonprinting_source_spans(source) {
        if cmd_start > target {
            break;
        }
        let (name, _) = math::command(source, cmd_start);
        if is_direct_reference_command(name)
            && span.start > cmd_start
            && source.as_bytes().get(span.start - 1) == Some(&b'{')
            && source.as_bytes().get(span.end) == Some(&b'}')
            && (span.start..=span.end).contains(&target)
            && valid_reference_label_start(source, &span).is_some()
        {
            return Some(span.start - 1..span.end + 1);
        }
    }
    None
}

fn transparent_reference_wrapper(name: &str) -> bool {
    matches!(
        name,
        "textbf"
            | "textit"
            | "emph"
            | "textrm"
            | "textsf"
            | "texttt"
            | "textnormal"
            | "textcolor"
            | "underline"
            | "mbox"
            | "hbox"
            | "vbox"
            | "vtop"
            | "vcenter"
            | "footnote"
            | "caption"
            | "frametitle"
            | "framesubtitle"
            | "title"
            | "subtitle"
            | "author"
            | "date"
            | "institute"
            | "part"
            | "chapter"
            | "section"
            | "subsection"
            | "subsubsection"
            | "paragraph"
            | "subparagraph"
    )
}

#[allow(clippy::too_many_arguments)]
fn source_reference_argument_location(
    source: &str,
    line: u32,
    lines: &std::ops::Range<usize>,
    within_scope: bool,
    context: &str,
    offset: usize,
    extra_sources: &[&str],
    hidden: &[std::ops::Range<usize>],
    loops: &[ForeachLoop<'_>],
    opaque: &[std::ops::Range<usize>],
    mode: WordLocationMode,
    operation: &crate::Operation,
) -> Result<Option<(u32, usize, isize, bool)>, ()> {
    operation.check().map_err(|_| ())?;
    if !loops.is_empty() || !opaque.is_empty() {
        return Ok(None);
    }
    let raw_pdf = words(context);
    let Some(selected) = word_index_at(context, &raw_pdf, offset).or_else(|| {
        match context.as_bytes().get(offset).copied() {
            Some(b'(' | b'[') => word_index_at(context, &raw_pdf, offset + 1),
            Some(b')' | b']') if offset > 0 => word_index_at(context, &raw_pdf, offset - 1),
            _ => None,
        }
    }) else {
        return Ok(None);
    };
    let pdf: Vec<String> = raw_pdf.iter().map(|(_, word)| normalized_word(word)).collect();
    if pdf.get(selected).is_none_or(String::is_empty) {
        return Ok(None);
    }
    let anchor = usize::try_from(line.checked_sub(1).ok_or(())?).map_err(|_| ())?;
    let mut line_starts = Vec::with_capacity(source.lines().count() + 1);
    let mut acc = 0usize;
    for raw in source.split_inclusive('\n') {
        line_starts.push(acc);
        acc += raw.len();
    }
    if line_starts.is_empty() {
        return Ok(None);
    }
    let scope_start = *line_starts.get(lines.start).unwrap_or(&source.len());
    let scope_end = if lines.end < line_starts.len() {
        line_starts[lines.end]
    } else {
        source.len()
    };
    if scope_start >= scope_end {
        return Ok(None);
    }
    let stream_lines = if within_scope {
        lines.clone()
    } else {
        lines.start.min(anchor.saturating_sub(4))
            ..lines.end.max(anchor.saturating_add(5)).min(line_starts.len())
    };
    let stream_start = *line_starts.get(stream_lines.start).unwrap_or(&scope_start);
    let stream_end = if stream_lines.end < line_starts.len() {
        line_starts[stream_lines.end]
    } else {
        source.len()
    };
    if !["\\ref", "\\eqref", "\\pageref", "\\autoref", "\\cref", "\\Cref"]
        .iter()
        .any(|needle| source[stream_start..stream_end].contains(needle))
    {
        return Ok(None);
    }
    if custom_env_prints_word(source, lines, hidden, extra_sources, &pdf[selected], operation) {
        return Ok(None);
    }
    let mut macro_sources = Vec::with_capacity(extra_sources.len() + 1);
    macro_sources.extend_from_slice(extra_sources);
    macro_sources.push(source);
    let macros = mode.macros(&macro_sources, operation);

    #[derive(Clone)]
    struct RefCandidate {
        cmd_start: usize,
        full_end: usize,
        label_line: u32,
        label_byte: usize,
        start_line: usize,
        end_line: usize,
        key_words: Vec<String>,
        footnote_body: Option<std::ops::Range<usize>>,
    }

    let mut span_start = stream_start;
    let mut span_end = stream_end;
    let mut footnote_bodies = Vec::new();
    let mut non_transparent_groups = Vec::new();
    let mut scan = 0usize;
    while scan < stream_end {
        operation.check().map_err(|_| ())?;
        match source.as_bytes()[scan] {
            b'%' => {
                scan += source[scan..stream_end]
                    .find('\n')
                    .map_or(stream_end - scan, |p| p + 1);
            }
            b'\\' => {
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
                let mut arg_at =
                    scan + usize::from(matches!(source.as_bytes().get(scan), Some(b'*' | b'+')));
                while let Some(opt_end) =
                    optional_argument_end(source, arg_at).or_else(|| overlay_argument_end(source, arg_at))
                {
                    if opt_end > stream_start {
                        non_transparent_groups.push(arg_at..opt_end);
                    }
                    arg_at = opt_end;
                }
                // An override may print around its argument, so no built-in role survives it.
                let overridden = macros.contains_key(cmd);
                if !overridden
                    && cmd == "footnote"
                    && let Some((body, _)) = math::group(source, skip_source_space(source, arg_at))
                {
                    if body.end > stream_start {
                        let fn_line = line_starts
                            .partition_point(|&s| s <= scan)
                            .saturating_sub(1);
                        span_start = span_start.min(line_starts[fn_line]).min(body.start);
                        span_end = span_end.max(body.end);
                        footnote_bodies.push(body);
                    }
                } else if !is_direct_reference_command(cmd)
                    && (overridden
                        || (!transparent_reference_wrapper(cmd)
                            && !known_formatting_switch(cmd)
                            && !matches!(
                                cmd,
                                "begin"
                                    | "end"
                                    | "href"
                                    | "label"
                                    | "item"
                                    | "par"
                                    | "noindent"
                                    | "centering"
                                    | "raggedright"
                                    | "raggedleft"
                                    | "pause"
                                    | "begingroup"
                                    | "endgroup"
                                    | "bgroup"
                                    | "egroup"
                                    | "smallskip"
                                    | "medskip"
                                    | "bigskip"
                                    | "allowbreak"
                                    | "linebreak"
                                    | "pagebreak"
                                    | "newpage"
                                    | "clearpage"
                                    | "cleardoublepage"
                                    | "vfill"
                                    | "hfill"
                                    | "quad"
                                    | "qquad"
                                    | "thinspace"
                                    | "enspace"
                                    | "slash"
                                    | "maketitle"
                                    | "titlepage"
                            )))
                    && let Some((body, end)) =
                        math::group(source, skip_source_space(source, arg_at))
                    && end > stream_start
                {
                    non_transparent_groups.push(body.start.saturating_sub(1)..end);
                }
            }
            _ => {
                scan += source[scan..stream_end]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
            }
        }
    }

    let mut refs = Vec::new();
    let mut all_spans = Vec::new();
    for (cmd_start, span, _) in
        nonprinting_source_spans_with_mode(source, Some(&macros), mode, Some(operation))
    {
        if span.end <= span_start || cmd_start >= span_end {
            continue;
        }
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
            || opaque.iter().any(|r| r.start < full_end && cmd_start < r.end)
        {
            continue;
        }
        let Some((label_start, key_words)) = valid_reference_label_start(source, &span) else {
            continue;
        };
        let start_line = line_starts
            .partition_point(|&s| s <= cmd_start)
            .saturating_sub(1);
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
        let label_line_idx = line_starts
            .partition_point(|&s| s <= label_start)
            .saturating_sub(1);
        if hidden.iter().any(|r| {
            r.contains(&label_start) && !(r.start == span.start && r.end == span.end)
        }) {
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
        return Ok(None);
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum StreamItem {
        Word(String),
        Ref(usize),
        OtherGenerated,
        Boundary,
        PageBreak(usize, usize, bool),
    }

    let build_stream =
        |region: std::ops::Range<usize>, inside_footnote: bool| -> Result<Vec<StreamItem>, ()> {
            let mut items = Vec::new();
            let mut bracket_ends = Vec::new();
            if inside_footnote {
                items.push(StreamItem::Boundary);
                items.push(StreamItem::OtherGenerated);
            }
            let mut at = region.start;
            while at < region.end {
                operation.check().map_err(|_| ())?;
                if !inside_footnote
                    && let Some(fn_body) = footnote_bodies
                        .iter()
                        .find(|b| b.start.saturating_sub(1) <= at && at <= b.end)
                {
                    items.push(StreamItem::OtherGenerated);
                    at = (fn_body.end + 1).min(region.end);
                    continue;
                }
                if let Some((ref_idx, cand)) = refs
                    .iter()
                    .enumerate()
                    .find(|(_, r)| r.cmd_start == at && r.footnote_body.is_some() == inside_footnote)
                {
                    items.push(StreamItem::Ref(ref_idx));
                    at = cand.full_end.min(region.end);
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
                                at += source[at..region.end]
                                    .chars()
                                    .next()
                                    .map_or(1, char::len_utf8);
                            }
                        }
                    }
                    b'\\' => {
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
                        let cmd_line = line_starts.partition_point(|&s| s <= cmd_pos).saturating_sub(1);
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
                            non_transparent_groups
                                .iter()
                                .any(|g| g.contains(&cmd_pos))
                                || opaque.iter().any(|r| r.contains(&cmd_pos))
                        };
                        let mut arg_at =
                            at + usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
                        if macros.contains_key(cmd) {
                            // An override may print anything, whatever the built-in does.
                            items.push(StreamItem::OtherGenerated);
                            while let Some(opt_end) = optional_argument_end(source, arg_at)
                                .or_else(|| overlay_argument_end(source, arg_at))
                            {
                                arg_at = opt_end;
                            }
                            while let Some((_, end)) =
                                math::group(source, skip_source_space(source, arg_at))
                            {
                                arg_at = end;
                            }
                            at = arg_at.min(region.end);
                            continue;
                        }
                        if cmd == "item" {
                            while let Some(ov_end) = overlay_argument_end(source, arg_at) {
                                arg_at = ov_end;
                            }
                            items.push(StreamItem::OtherGenerated);
                            if let Some(opt_end) = optional_argument_end(source, arg_at) {
                                bracket_ends.push(opt_end - 1);
                                at = (skip_source_space(source, arg_at) + 1).min(region.end);
                            } else {
                                at = arg_at.min(region.end);
                            }
                            continue;
                        }
                        while let Some(opt_end) = optional_argument_end(source, arg_at)
                            .or_else(|| overlay_argument_end(source, arg_at))
                        {
                            arg_at = opt_end;
                        }
                        if matches!(cmd, "begin" | "end") {
                            if let Some((body, mut group_end)) =
                                math::group(source, skip_source_space(source, arg_at))
                            {
                                let env = source[body].trim();
                                if matches!(env, "document" | "frame") && !in_non_transparent() {
                                    items.push(StreamItem::PageBreak(cmd_line, cmd_pos, true));
                                } else if cmd == "begin"
                                    && !matches!(
                                        env,
                                        "frame"
                                            | "center"
                                            | "flushleft"
                                            | "flushright"
                                            | "minipage"
                                            | "tabular"
                                            | "tabular*"
                                            | "tabularx"
                                            | "array"
                                            | "figure"
                                            | "figure*"
                                            | "table"
                                            | "table*"
                                            | "itemize"
                                            | "enumerate"
                                            | "description"
                                    )
                                {
                                    items.push(StreamItem::OtherGenerated);
                                    while let Some(ov_end) = overlay_argument_end(source, group_end)
                                    {
                                        group_end = ov_end;
                                    }
                                    if let Some(opt_end) = optional_argument_end(source, group_end)
                                    {
                                        bracket_ends.push(opt_end - 1);
                                        at = (skip_source_space(source, group_end) + 1)
                                            .min(region.end);
                                        continue;
                                    }
                                } else {
                                    items.push(StreamItem::Boundary);
                                }
                                while let Some(opt_end) = optional_argument_end(source, group_end)
                                    .or_else(|| overlay_argument_end(source, group_end))
                                {
                                    group_end = opt_end;
                                }
                                if cmd == "begin"
                                    && matches!(
                                        env,
                                        "tabular" | "tabular*" | "tabularx" | "array" | "minipage"
                                    )
                                {
                                    while let Some((_, end)) =
                                        math::group(source, skip_source_space(source, group_end))
                                    {
                                        group_end = end;
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
                        if matches!(
                            cmd,
                            "caption"
                                | "section"
                                | "subsection"
                                | "subsubsection"
                                | "paragraph"
                                | "subparagraph"
                                | "part"
                                | "chapter"
                                | "frametitle"
                                | "framesubtitle"
                                | "maketitle"
                                | "titlepage"
                        ) {
                            items.push(StreamItem::OtherGenerated);
                        }
                        if !inside_footnote
                            && cmd == "footnote"
                            && let Some((body, end)) =
                                math::group(source, skip_source_space(source, arg_at))
                            && footnote_bodies.contains(&body)
                        {
                            items.push(StreamItem::OtherGenerated);
                            at = end.min(region.end);
                            continue;
                        }
                        if let Some((_, span)) = all_spans.iter().find(|(s, _)| *s == cmd_pos) {
                            if is_direct_reference_command(cmd)
                                || matches!(
                                    cmd,
                                    "cite"
                                        | "citep"
                                        | "citet"
                                        | "parencite"
                                        | "textcite"
                                        | "autocite"
                                        | "footcite"
                                        | "input"
                                        | "include"
                                        | "includegraphics"
                                        | "includesvg"
                                        | "verb"
                                        | "lstinline"
                                )
                            {
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
                            if let Some((_, end)) =
                                math::group(source, skip_source_space(source, arg_at))
                            {
                                arg_at = end;
                            }
                            at = arg_at.min(region.end);
                            continue;
                        }
                        if transparent_reference_wrapper(cmd)
                            || known_formatting_switch(cmd)
                            || matches!(
                                cmd,
                                "label"
                                    | "smallskip"
                                    | "medskip"
                                    | "bigskip"
                                    | "noindent"
                                    | "centering"
                                    | "raggedright"
                                    | "raggedleft"
                                    | "pause"
                                    | "begingroup"
                                    | "endgroup"
                                    | "bgroup"
                                    | "egroup"
                                    | "allowbreak"
                                    | "linebreak"
                                    | "vfill"
                                    | "hfill"
                                    | "quad"
                                    | "qquad"
                                    | "thinspace"
                                    | "enspace"
                                    | "slash"
                                    | "href"
                            )
                        {
                            at = arg_at;
                            continue;
                        }
                        items.push(StreamItem::OtherGenerated);
                        while let Some((_, end)) =
                            math::group(source, skip_source_space(source, arg_at))
                        {
                            arg_at = end;
                        }
                        at = arg_at.min(region.end);
                    }
                    b']' if bracket_ends.contains(&at) => {
                        items.push(StreamItem::OtherGenerated);
                        at += 1;
                    }
                    b'{' | b'}' => {
                        at += 1;
                    }
                    _ => {
                        if let Some((_, span)) = all_spans
                            .iter()
                            .find(|(_, s)| s.start <= at && at < s.end)
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
            Ok(items)
        };

    let main_stream = build_stream(span_start..stream_end, false)?;
    let mut footnote_streams: Vec<(std::ops::Range<usize>, Vec<StreamItem>)> = Vec::new();
    for fn_body in &footnote_bodies {
        if !footnote_streams.iter().any(|(b, _)| b == fn_body) {
            footnote_streams.push((fn_body.clone(), build_stream(fn_body.clone(), true)?));
        }
    }
    let selected_word = &pdf[selected];
    let mut best: Option<((bool, isize, bool, std::cmp::Reverse<u32>), (u32, usize, isize, bool))> =
        None;
    let mut tied = false;

    fn flanks_around(
        stream: &[StreamItem],
        left_end: usize,
        right_start: usize,
    ) -> (Vec<&String>, bool, bool, Vec<&String>, bool, bool) {
        let mut left_words = Vec::new();
        let mut left_adjacent_generated = false;
        let mut left_crossed_boundary = false;
        for item in stream[..left_end].iter().rev() {
            match item {
                StreamItem::Word(w) => {
                    left_words.push(w);
                    if left_words.len() >= 3 {
                        break;
                    }
                }
                StreamItem::Ref(_) | StreamItem::OtherGenerated => {
                    if left_words.is_empty() {
                        left_adjacent_generated = true;
                    }
                    break;
                }
                StreamItem::Boundary => {
                    if left_words.is_empty() {
                        left_crossed_boundary = true;
                    } else {
                        break;
                    }
                }
                StreamItem::PageBreak(_, _, _) => {
                    if left_words.is_empty() {
                        left_crossed_boundary = true;
                    }
                    break;
                }
            }
        }
        let mut right_words = Vec::new();
        let mut right_adjacent_generated = false;
        let mut right_crossed_boundary = false;
        for item in &stream[right_start..] {
            match item {
                StreamItem::Word(w) => {
                    right_words.push(w);
                    if right_words.len() >= 3 {
                        break;
                    }
                }
                StreamItem::Ref(_) | StreamItem::OtherGenerated => {
                    if right_words.is_empty() {
                        right_adjacent_generated = true;
                    }
                    break;
                }
                StreamItem::Boundary => {
                    if right_words.is_empty() {
                        right_crossed_boundary = true;
                    } else {
                        break;
                    }
                }
                StreamItem::PageBreak(_, _, _) => {
                    if right_words.is_empty() {
                        right_crossed_boundary = true;
                    }
                    break;
                }
            }
        }
        (
            left_words,
            left_adjacent_generated,
            left_crossed_boundary,
            right_words,
            right_adjacent_generated,
            right_crossed_boundary,
        )
    }

    for (ref_idx, cand) in refs.iter().enumerate() {
        operation.check().map_err(|_| ())?;
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
            .rposition(|item| matches!(item, StreamItem::PageBreak(_, pb_pos, true) if *pb_pos < cand_pos))
            .unwrap_or(0);
        let main_page_end = main_stream
            .iter()
            .position(|item| matches!(item, StreamItem::PageBreak(_, pb_pos, true) if *pb_pos > cand_pos))
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
        let stream: &[StreamItem] = cand
            .footnote_body
            .as_ref()
            .and_then(|fn_body| {
                footnote_streams
                    .iter()
                    .find(|(b, _)| b == fn_body)
                    .map(|(_, s)| s.as_slice())
            })
            .unwrap_or(main_page_stream);
        let Some(pos) = stream
            .iter()
            .position(|item| *item == StreamItem::Ref(ref_idx))
        else {
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
        let same_line_pagebreak = stream
            .iter()
            .chain(main_page_stream.iter())
            .any(|item| {
                matches!(
                    item,
                    StreamItem::PageBreak(pb_line, _, _)
                        if (cand.start_line..=cand.end_line).contains(pb_line) || *pb_line == anchor
                )
            });
        let footnote_starts_at_left =
            cand.footnote_body.is_some() && pos == 2 + left_words.len();

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
        for &(
            g_start,
            g_end,
            both_sides,
            score,
            vis_left_len,
            vis_right_len,
            footnote_head_word,
        ) in &pdf_spans
        {
            if !(g_start..=g_end).contains(&selected) {
                continue;
            }
            let mut total_slots = 0usize;
            let mut ref_only_slots = 0usize;
            let mut matches_self = false;
            for (s_body, s) in std::iter::once((None, main_page_stream)).chain(
                footnote_streams
                    .iter()
                    .filter(|(b, _)| {
                        pb_before.is_none_or(|pb| b.start > pb)
                            && pb_after.is_none_or(|pb| b.start < pb)
                    })
                    .map(|(b, s)| (Some(b), s.as_slice())),
            ) {
                let is_cand_stream = s_body == cand.footnote_body.as_ref();
                if vis_left_len > 0 && vis_right_len > 0 {
                    for s_start in 0..s.len() {
                        if !matches!(&s[s_start], StreamItem::Word(rw) if right_words.first().copied() == Some(rw)) {
                            continue;
                        }
                        let (s_left, s_left_adj, _, s_right, s_right_adj, _) =
                            flanks_around(s, s_start, s_start);
                        if !s_left_adj
                            && !s_right_adj
                            && s_left.len() >= vis_left_len
                            && s_left[..vis_left_len] == left_words[..vis_left_len]
                            && (!footnote_head_word
                                || if s_left.len() >= left_words.len() {
                                    s_left[vis_left_len] == left_words[vis_left_len]
                                        || pdf.get(g_start - left_words.len())
                                            == Some(s_left[vis_left_len])
                                } else {
                                    !(s_body.is_some() && s_start == 2 + s_left.len())
                                })
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
                            s_left.len() >= vis_left_len
                                && s_left[..vis_left_len] == left_words[..vis_left_len]
                                && (!footnote_head_word
                                    || if s_left.len() >= left_words.len() {
                                        s_left[vis_left_len] == left_words[vis_left_len]
                                            || pdf.get(g_start - left_words.len())
                                                == Some(s_left[vis_left_len])
                                    } else {
                                        !(s_body.is_some() && s_start == 2 + s_left.len())
                                    })
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
                if rank == best_rank
                    && (best_target.0, best_target.1) != (target.0, target.1) =>
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
            WordLocationMode::Strict => Ok(None),
            WordLocationMode::Guess => Err(()),
        };
    }
    Ok(best.map(|(_, target)| target))
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

// TeX's next-token header checks stop at a paragraph, not at comment newlines.
fn skip_frame_header_space(source: &str, mut at: usize) -> Option<usize> {
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
    optional_argument_boundary(source, start).ok()
}

// Leave an enclosing stored-body delimiter for the outer scanner on failure.
pub(crate) fn optional_argument_boundary(source: &str, start: usize) -> Result<usize, usize> {
    let start = skip_source_space(source, start);
    if source.as_bytes().get(start) != Some(&b'[') {
        return Err(start);
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
                at = math::group(source, at).ok_or(source.len())?.1;
                continue;
            }
            b']' => return Ok(at + 1),
            b'}' => return Err(at),
            _ => {}
        }
        at += source[at..].chars().next().ok_or(source.len())?.len_utf8();
    }
    Err(source.len())
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

pub(crate) fn frame_header_after_begin(source: &str, frame_start_line: usize) -> Option<usize> {
    let row = source.lines().nth(frame_start_line).map(source_line_text)?;
    let row_start: usize = source
        .split_inclusive('\n')
        .take(frame_start_line)
        .map(str::len)
        .sum();
    row.match_indices("\\begin")
        .filter(|(at, _)| !has_unescaped_backslash_before(row, *at))
        .find_map(|(at, _)| {
            let (env, end) = math::group(source, row_start + at + "\\begin".len())?;
            (&source[env] == "frame").then_some(end)
        })
}

#[allow(clippy::type_complexity)]
fn filter_frame_graphics_for_slide(
    source: &str,
    frame_range: &std::ops::Range<usize>,
    masked: &[std::ops::Range<usize>],
    matched: &[(std::ops::Range<usize>, std::ops::Range<usize>, usize, usize)],
    slide: u32,
    extra_sources: &[&str],
) -> Option<Vec<(std::ops::Range<usize>, std::ops::Range<usize>, usize, usize)>> {
    let frame_start: usize = source
        .split_inclusive('\n')
        .take(frame_range.start)
        .map(str::len)
        .sum();
    let frame_end: usize = frame_start
        + source[frame_start..]
            .split_inclusive('\n')
            .take(frame_range.end.saturating_sub(frame_range.start))
            .map(str::len)
            .sum::<usize>();
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
    if let Some(end) = frame_header_after_begin(source, frame_range.start) {
        let mut cursor = end;
        loop {
            let after = skip_source_space(source, cursor);
            if source.as_bytes().get(after) == Some(&b'<') {
                return None;
            }
            let Some(opt_end) = optional_argument_end(source, after) else {
                break;
            };
            let opts = &source[after + 1..opt_end - 1];
            for raw_opt in opts.split(',') {
                let o = source_line_text(raw_opt).trim();
                if o == "allowframebreaks"
                    || o.starts_with("allowframebreaks=")
                    || o == "label"
                    || o.starts_with("label=")
                    || (!o.is_empty()
                        && o.bytes().all(|b| {
                            b.is_ascii_digit()
                                || matches!(
                                    b,
                                    b'-' | b'+' | b'*' | b':' | b'@' | b'(' | b')' | b'|' | b' '
                                )
                        }))
                {
                    return None;
                }
            }
            cursor = opt_end;
        }
    }
    let mut only_groups = Vec::new();
    let mut at = frame_start;
    while at < frame_end {
        match source.as_bytes()[at] {
            b'%' => {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
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
                if matches!(
                    cmd,
                    "pause"
                        | "onslide"
                        | "uncover"
                        | "visible"
                        | "invisible"
                        | "alt"
                        | "temporal"
                        | "overprint"
                        | "overlayarea"
                        | "framebreak"
                        | "note"
                        | "againframe"
                ) {
                    return None;
                }
                if cmd == "begin"
                    && let Some((env, _)) = math::group(source, skip_source_space(source, at))
                    && matches!(
                        &source[env],
                        "onlyenv"
                            | "uncoverenv"
                            | "visibleenv"
                            | "invisibleenv"
                            | "altenv"
                            | "overprint"
                            | "overlayarea"
                            | "note"
                    )
                {
                    return None;
                }
                if cmd == "only" {
                    let first = skip_source_space(source, at);
                    if let Some(spec_end) = overlay_argument_end(source, first) {
                        let spec_match = overlay_spec_matches(&source[first..spec_end], slide)?;
                        let grp_start = skip_source_space(source, spec_end);
                        let (grp, _) = math::group(source, grp_start)?;
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

#[cfg(test)]
pub(crate) fn source_graphics_location(
    source: &str,
    line: u32,
    index: usize,
    count: usize,
    scope_proven: bool,
) -> Option<(u32, usize)> {
    source_graphics_location_with_context(
        source,
        line,
        index,
        count,
        scope_proven,
        None,
        &[],
        WordLocationMode::Strict,
        None,
        &crate::Operation::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn source_graphics_location_with_context(
    source: &str,
    line: u32,
    index: usize,
    count: usize,
    scope_proven: bool,
    frame_slide: Option<u32>,
    extra_sources: &[&str],
    mode: WordLocationMode,
    execution_root: Option<ExecutionRoot<'_>>,
    operation: &crate::Operation,
) -> Option<(u32, usize)> {
    operation.check().ok()?;
    if count == 0 || index >= count {
        return None;
    }
    let anchor = usize::try_from(line.checked_sub(1)?).ok()?;
    let frame_scope = scope_proven
        .then(|| source_frame_range(source, line).filter(|range| line == range.end as u32))
        .flatten();
    let scope = scope_proven
        .then(|| {
            frame_scope
                .clone()
                .or_else(|| source_tabularx_range(source, line))
                .or_else(|| math::collected_text_lines(source, line))
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
    if matched.len() > count
        && let (Some(frame_range), Some(slide)) = (&frame_scope, frame_slide)
        && let Some(filtered) = filter_frame_graphics_for_slide(
            source,
            frame_range,
            &masked,
            &matched,
            slide,
            extra_sources,
        )
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
    let mut macro_sources = Vec::with_capacity(extra_sources.len() + 1);
    macro_sources.extend_from_slice(extra_sources);
    macro_sources.push(source);
    let mut macros = mode.macros(&macro_sources, operation);
    if mode == WordLocationMode::Guess {
        let definitions = note_macro_definitions(
            &macro_sources,
            &mut std::collections::BTreeSet::new(),
            execution_root,
            Some(operation),
        );
        for name in ["includegraphics", "includesvg"] {
            if definitions
                .get(name)
                .is_some_and(|bodies| bodies.iter().all(|body| body.package_initialization))
            {
                macros.remove(name);
            }
        }
    }
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
                        let mut stack = vec![cmd];
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
                                let (sub, next) = math::command(body, pos + rel);
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
    let prefix = &source[..path_start];
    let target_line = u32::try_from(prefix.bytes().filter(|&ch| ch == b'\n').count()).ok()? + 1;
    let line_start = prefix.rfind('\n').map_or(0, |pos| pos + 1);
    Some((target_line, path_start - line_start))
}

pub(super) fn environment_definition_command(command: &str) -> bool {
    matches!(
        command,
        "newenvironment"
            | "renewenvironment"
            | "provideenvironment"
            | "NewDocumentEnvironment"
            | "RenewDocumentEnvironment"
            | "ProvideDocumentEnvironment"
            | "DeclareDocumentEnvironment"
    )
}

fn stored_definition_command(command: &str) -> bool {
    (math::is_macro_definition_command(command) && !matches!(command, "csname" | "newif"))
        || environment_definition_command(command)
}

pub(super) fn environment_definition<'a>(
    source: &'a str,
    command: &str,
    mut at: usize,
) -> Option<(&'a str, &'a str, usize)> {
    if !environment_definition_command(command) {
        return None;
    }
    at += usize::from(matches!(source.as_bytes().get(at), Some(b'*' | b'+')));
    let (name, contents_start) = math::group(source, skip_source_space(source, at))?;
    at = contents_start;
    while let Some(end) = optional_argument_end(source, at) {
        at = end;
    }
    if command.contains("Document") {
        at = math::group(source, skip_source_space(source, at))?.1;
    }
    let (_, end) = math::group(source, skip_source_space(source, at))?;
    let (_, end) = math::group(source, skip_source_space(source, end))?;
    let name = source[name].trim_ascii();
    let name = if !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'*' | b'-' | b'_' | b':' | b'.')
        }) {
        name
    } else {
        "**"
    };
    Some((name, &source[contents_start..end], end))
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
    nonprinting_source_spans_with_mode(source, None, WordLocationMode::Strict, None)
}

fn source_document_start(
    source: &str,
    macros: Option<&std::collections::BTreeMap<&str, Option<math::MacroDef<'_>>>>,
    definitions: &std::collections::BTreeMap<&str, Vec<StoredBody<'_>>>,
    operation: Option<&crate::Operation>,
) -> Option<usize> {
    let mut hidden =
        nonprinting_source_spans_with_mode(source, macros, WordLocationMode::Strict, operation)
            .peekable();
    let mut at = 0;
    while at < source.len() {
        if operation.is_some_and(|operation| operation.check().is_err()) {
            return None;
        }
        while hidden.peek().is_some_and(|(_, span, _)| span.end <= at) {
            hidden.next();
        }
        if let Some((head, span, _)) = hidden.peek()
            && span.contains(&at)
        {
            if source.as_bytes().get(*head) == Some(&b'\\') {
                let (raw, _) = math::command(source, *head);
                let (command, _, _) = math::definition_command(source, *head);
                if definitions.contains_key(raw)
                    || definitions.contains_key(command)
                    || macros.is_some_and(|macros| {
                        macros.contains_key(raw) || macros.contains_key(command)
                    })
                {
                    return None;
                }
            }
            at = span.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => at += source[at..].find('\n').unwrap_or(source.len() - at),
            b'{' => return None,
            b'\\' => {
                let start = at;
                let (command, end) = math::command(source, at);
                if definitions.contains_key(command)
                    || macros.is_some_and(|macros| macros.contains_key(command))
                {
                    return None;
                }
                if let Some(quoted_end) = math::quoted_token_end(source, command, end) {
                    at = quoted_end;
                } else if let Some((opaque_end, _)) = math::opaque_command_end(source, command, end)
                {
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

fn nonprinting_source_spans_with_mode<'a>(
    source: &'a str,
    macros: Option<&'a std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>>,
    mode: WordLocationMode,
    operation: Option<&'a crate::Operation>,
) -> impl Iterator<
    Item = (
        usize,
        std::ops::Range<usize>,
        Option<std::ops::Range<usize>>,
    ),
> + 'a {
    let preamble_end = if mode == WordLocationMode::Guess
        && operation.is_none_or(|operation| operation.check().is_ok())
        && !macros.is_some_and(|macros| macros.contains_key("begin"))
    {
        let definitions = note_macro_definitions(
            &[source],
            &mut std::collections::BTreeSet::new(),
            None,
            operation,
        );
        (!definitions.contains_key("**")
            && operation.is_none_or(|operation| operation.check().is_ok()))
        .then(|| source_document_start(source, macros, &definitions, operation))
        .flatten()
    } else {
        None
    };
    let mut at = 0;
    let mut pending_opt_after = None;
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
                let (raw_command, raw_end) = math::command(source, start);
                let (command, end, expanded_target) = math::definition_command(source, start);
                let definition = stored_definition_command(command);
                let overridden = !definition
                    && command != "newif"
                    && macros.is_some_and(|macros| {
                        macros.contains_key(command)
                            || mode == WordLocationMode::Strict
                                && uncertain_note_invocation(
                                    command,
                                    macros,
                                    WordLocationMode::Strict,
                                )
                    });
                if raw_command == "csname" && !definition && command != "newif" {
                    at = if overridden {
                        end
                    } else {
                        math::quoted_token_end(source, command, end).unwrap_or(end)
                    };
                    if command == "csname" {
                        continue;
                    }
                    let hidden_end = if at > end
                        && source.as_bytes().get(skip_source_space(source, end)) == Some(&b'\\')
                    {
                        at
                    } else {
                        end
                    };
                    return Some((start, raw_end..hidden_end, None));
                }
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
                if let Some(quoted_end) = math::quoted_token_end(source, command, end) {
                    at = quoted_end;
                    continue;
                }
                if let Some((end, nonprinting)) = math::opaque_command_end(source, command, end) {
                    at = end;
                    if nonprinting {
                        return Some((start, start..end, None));
                    }
                    continue;
                }
                if let Some((_, target_end)) = math::macro_mutation_target(source, command, end) {
                    at = target_end;
                    return Some((start, end..target_end, None));
                }
                if command == "newif" {
                    at = math::macro_target_end(source, end).unwrap_or(end);
                    return Some((start, start..at, None));
                }
                if matches!(command, "let" | "futurelet") {
                    at = math::macro_alias(source, command, end, expanded_target).nonprinting_end;
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
                    if let Some((_, _, end)) = environment_definition(source, command, end) {
                        at = end;
                        return Some((start, start..end, None));
                    }
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
                        if let Some(end) = math::macro_target_end(source, at) {
                            at = end;
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
                    if command.ends_with("environment") || command.ends_with("Environment") {
                        let end_start = skip_source_space(source, at);
                        if let Some((_, end)) = math::group(source, end_start) {
                            at = end;
                        } else if source.as_bytes().get(end_start) == Some(&b'{') {
                            at = source.len();
                        }
                    }
                    return Some((start, start..at, None));
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
                        | "bibitem"
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
                } else if matches!(
                    command,
                    "crefformat"
                        | "Crefformat"
                        | "crefrangeformat"
                        | "Crefrangeformat"
                        | "crefmultiformat"
                        | "Crefmultiformat"
                        | "crefrangemultiformat"
                        | "Crefrangemultiformat"
                        | "crefname"
                        | "Crefname"
                        | "creflabelformat"
                        | "crefeqlabelformat"
                ) {
                    let arity = match command {
                        "crefname" | "Crefname" => 3,
                        "crefmultiformat"
                        | "Crefmultiformat"
                        | "crefrangemultiformat"
                        | "Crefrangemultiformat" => 5,
                        _ => 2,
                    };
                    let mut group_end = at;
                    let mut consumed = 0usize;
                    while consumed < arity {
                        if let Some((_, end)) =
                            math::group(source, skip_source_space(source, group_end))
                        {
                            group_end = end;
                            consumed += 1;
                        } else {
                            break;
                        }
                    }
                    if group_end > at {
                        at = group_end;
                        return Some((start, start..group_end, None));
                    }
                } else if matches!(command, "begin" | "end")
                    && let Some((body, mut group_end)) =
                        math::group(source, skip_source_space(source, at))
                    && (!math::math_environment(&source[body.clone()])
                        || &source[body.clone()] == "array")
                {
                    let env = &source[body.clone()];
                    if command == "begin"
                        && matches!(env, "tabularx" | "tabular*" | "tabular" | "array")
                    {
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
                    // Beamer frame overlays and options configure the frame and never print;
                    // the mandatory title and subtitle groups after them stay visible. A paragraph
                    // ends the header, so a bracket after a blank line is printed body text.
                    if command == "begin" && env == "frame" {
                        while let Some(option_end) = skip_frame_header_space(source, group_end)
                            .and_then(|next| {
                                overlay_argument_end(source, next)
                                    .or_else(|| optional_argument_end(source, next))
                            })
                        {
                            group_end = option_end;
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
    operation: &crate::Operation,
) -> Option<(u32, usize, isize)> {
    source_prose_scored_location_with_loops(
        source,
        line,
        context,
        offset,
        lines,
        within_frame,
        hidden,
        &[],
        allowed_bytes,
        &[],
        &[],
        operation,
    )
    .ok()
    .flatten()
}

fn body_prints_word<'a>(
    body: &'a str,
    target_word: &str,
    macros: &std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    visited: &mut std::collections::BTreeSet<&'a str>,
    operation: &crate::Operation,
) -> bool {
    // Exact reachability: every helper is scanned once, including cyclic graphs.
    let mut pending = vec![body];
    while let Some(body) = pending.pop() {
        if operation.check().is_err() {
            return true;
        }
        if body.lines().any(|line| {
            source_words(source_line_text(line))
                .into_iter()
                .any(|(_, _, word)| word == target_word)
        }) {
            return true;
        }
        let mut pos = 0;
        while pos < body.len() {
            if operation.check().is_err() {
                return true;
            }
            match body.as_bytes()[pos] {
                b'%' => pos += body[pos..].find('\n').unwrap_or(body.len() - pos),
                b'\\' => {
                    let (command, next) = math::command(body, pos);
                    if visited.insert(command)
                        && let Some(def) = macros.get(command)
                    {
                        let Some(body) = def.as_ref().and_then(|def| def.body) else {
                            return true;
                        };
                        pending.push(body);
                    }
                    pos = next.max(pos + 1);
                }
                _ => {
                    pos += body.as_bytes()[pos + 1..]
                        .iter()
                        .position(|byte| matches!(byte, b'%' | b'\\'))
                        .map_or(body.len() - pos, |offset| offset + 1)
                }
            }
        }
    }
    false
}

fn custom_env_prints_word(
    source: &str,
    lines: &std::ops::Range<usize>,
    hidden: &[std::ops::Range<usize>],
    extra_sources: &[&str],
    target_word: &str,
    operation: &crate::Operation,
) -> bool {
    let start_byte: usize = source
        .split_inclusive('\n')
        .take(lines.start)
        .map(str::len)
        .sum();
    let end_byte: usize = start_byte
        + source[start_byte..]
            .split_inclusive('\n')
            .take(lines.end.saturating_sub(lines.start))
            .map(str::len)
            .sum::<usize>();
    let mut macro_sources = Vec::with_capacity(extra_sources.len() + 1);
    macro_sources.extend_from_slice(extra_sources);
    macro_sources.push(source);
    let macros = math::macros_from_sources(&macro_sources, operation);
    let definitions = note_macro_definitions(
        &macro_sources,
        &mut std::collections::BTreeSet::new(),
        None,
        Some(operation),
    );
    let mut invoked = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut pending = vec![(source, start_byte..end_byte, true)];
    while let Some((body, range, original)) = pending.pop() {
        if operation.check().is_err() {
            return true;
        }
        let Some(discarded) = discarded_macro_args_with_macros(
            body,
            !original,
            range.clone(),
            MacroContext {
                macros: &macros,
                definitions: &definitions,
                operation,
            },
            false,
            &std::collections::BTreeSet::new(),
            WordLocationMode::Strict,
        ) else {
            return true;
        };
        for (start, name, _, kind) in invoked_commands(body, true, &macros, operation) {
            if !range.contains(&start)
                || kind == InvocationKind::MathDelimiter
                || discarded.iter().any(|span| span.contains(&start))
                || original && hidden.iter().any(|span| span.contains(&start))
            {
                continue;
            }
            if kind != InvocationKind::Command
                && let Some(def) = macros.get(name)
                && def.as_ref().and_then(|def| def.body).is_none_or(|body| {
                    body_prints_word(body, target_word, &macros, &mut visited, operation)
                })
            {
                return true;
            }
            if invoked.insert(name)
                && let Some(bodies) = definitions.get(name)
            {
                pending.extend(
                    bodies
                        .iter()
                        .map(|body| (body.source, 0..body.source.len(), false)),
                );
            }
        }
    }
    for src in macro_sources
        .iter()
        .copied()
        .chain(definitions.values().flatten().map(|body| body.source))
    {
        for (start, _, _) in nonprinting_source_spans(src) {
            if operation.check().is_err() {
                return true;
            }
            if src.as_bytes().get(start) != Some(&b'\\') {
                continue;
            }
            let (command, at, _) = math::definition_command(src, start);
            let Some((name, body, _)) = environment_definition(src, command, at) else {
                continue;
            };
            if (invoked.contains(name)
                || invoked
                    .iter()
                    .any(|command| command.strip_prefix("end") == Some(name)))
                && body_prints_word(body, target_word, &macros, &mut visited, operation)
            {
                return true;
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn source_prose_scored_location_with_loops(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_frame: bool,
    hidden: &[std::ops::Range<usize>],
    opaque: &[std::ops::Range<usize>],
    allowed_bytes: Option<std::ops::Range<usize>>,
    loops: &[ForeachLoop<'_>],
    extra_sources: &[&str],
    operation: &crate::Operation,
) -> Result<Option<(u32, usize, isize)>, ()> {
    source_prose_scored_location_with_mode(
        source,
        line,
        context,
        offset,
        lines,
        within_frame,
        hidden,
        opaque,
        allowed_bytes,
        loops,
        extra_sources,
        WordLocationMode::Strict,
        operation,
    )
}

// shortcut: 64 KiB fixed prose macro expansion cap, raise if legitimate zero-arg prose macros exceed 64 KiB.
const MAX_FIXED_PROSE_MACRO_BYTES: usize = 65_536;

fn append_fixed_prose_macro_text<'a>(
    text: &'a str,
    macros: &std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    operation: &crate::Operation,
    depth: u8,
    out: &mut String,
) -> Result<bool, ()> {
    operation.check().map_err(|_| ())?;
    if depth >= 8 {
        return Ok(false);
    }
    let mut at = 0usize;
    while at < text.len() {
        operation.check().map_err(|_| ())?;
        if let Some((ch, next)) = tex_accent_at(text, at) {
            if out.len() + ch.len_utf8() > MAX_FIXED_PROSE_MACRO_BYTES {
                return Ok(false);
            }
            out.push(ch);
            at = next;
            continue;
        }
        match text.as_bytes()[at] {
            b'%' | b'$' | b'#' | b'&' | b'~' | b'^' | b'_' => return Ok(false),
            b'{' => {
                let Some((inner, next)) = math::group(text, at) else {
                    return Ok(false);
                };
                if !append_fixed_prose_macro_text(&text[inner], macros, operation, depth + 1, out)?
                {
                    return Ok(false);
                }
                at = next;
            }
            b'}' => return Ok(false),
            b'\\' => {
                let (cmd, next) = math::command(text, at);
                if cmd.is_empty() {
                    return Ok(false);
                }
                if let Some(entry) = macros.get(cmd) {
                    let Some(sub) = entry.as_ref() else {
                        return Ok(false);
                    };
                    if sub.arity != 0 || sub.used_params != 0 {
                        return Ok(false);
                    }
                    let Some(body) = sub.body else {
                        return Ok(false);
                    };
                    if !append_fixed_prose_macro_text(body, macros, operation, depth + 1, out)? {
                        return Ok(false);
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
                } else if matches!(
                    cmd,
                    "textbf"
                        | "textit"
                        | "emph"
                        | "textrm"
                        | "textsf"
                        | "texttt"
                        | "textnormal"
                        | "underline"
                        | "mbox"
                ) {
                    let Some((inner, ge)) = math::group(text, skip_source_space(text, next)) else {
                        return Ok(false);
                    };
                    if !append_fixed_prose_macro_text(
                        &text[inner],
                        macros,
                        operation,
                        depth + 1,
                        out,
                    )? {
                        return Ok(false);
                    }
                    at = ge;
                } else {
                    return Ok(false);
                }
            }
            _ => {
                let ch = text[at..].chars().next().unwrap();
                if out.len() + ch.len_utf8() > MAX_FIXED_PROSE_MACRO_BYTES {
                    return Ok(false);
                }
                out.push(ch);
                at += ch.len_utf8();
            }
        }
    }
    Ok(true)
}

fn fixed_prose_macro_words<'a>(
    def: &math::MacroDef<'a>,
    macros: &std::collections::BTreeMap<&'a str, Option<math::MacroDef<'a>>>,
    operation: &crate::Operation,
) -> Result<Option<Vec<String>>, ()> {
    operation.check().map_err(|_| ())?;
    if def.arity != 0 || def.used_params != 0 {
        return Ok(None);
    }
    let Some(body) = def.body else {
        return Ok(None);
    };
    let mut expanded = String::new();
    if !append_fixed_prose_macro_text(body, macros, operation, 0, &mut expanded)? {
        return Ok(None);
    }
    let words: Vec<String> = source_words(&expanded)
        .into_iter()
        .map(|(_, _, w)| w)
        .filter(|w| !w.is_empty())
        .collect();
    Ok((!words.is_empty()).then_some(words))
}

#[allow(clippy::too_many_arguments)]
fn source_prose_scored_location_with_mode(
    source: &str,
    line: u32,
    context: &str,
    offset: usize,
    lines: std::ops::Range<usize>,
    within_frame: bool,
    hidden: &[std::ops::Range<usize>],
    opaque: &[std::ops::Range<usize>],
    allowed_bytes: Option<std::ops::Range<usize>>,
    loops: &[ForeachLoop<'_>],
    extra_sources: &[&str],
    mode: WordLocationMode,
    operation: &crate::Operation,
) -> Result<Option<(u32, usize, isize)>, ()> {
    operation.check().map_err(|_| ())?;
    enum ProseToken<'a> {
        Word(u32, usize, String, bool),
        LoopVar(u32, usize, usize, &'a str),
    }

    if loops.len() > 2 || loops.iter().any(|lp| lp.items.is_none()) {
        return Ok(None);
    }
    let pdf = words(context);
    let Some(selected) = word_index_at(context, &pdf, offset) else {
        return Ok(None);
    };
    let pdf: Vec<_> = pdf.iter().map(|(_, word)| normalized_word(word)).collect();
    if custom_env_prints_word(
        source,
        &lines,
        hidden,
        extra_sources,
        &pdf[selected],
        operation,
    ) {
        return Ok(None);
    }
    let mut macro_sources = Vec::with_capacity(extra_sources.len() + 1);
    macro_sources.extend_from_slice(extra_sources);
    macro_sources.push(source);
    let mut macros = None;
    let mut expanded_macros: std::collections::BTreeMap<&str, Option<Vec<String>>> =
        std::collections::BTreeMap::new();
    let frame_header = within_frame
        && source
            .lines()
            .nth(lines.start)
            .is_some_and(|row| source_line_text(row).contains("\\begin{frame}"));
    let mut hidden = hidden.iter().peekable();
    let mut tokens = Vec::new();
    let mut has_loop_var = false;
    let mut line_start = 0;
    let mut phantom_until = 0usize;
    for (index, raw_row) in source.split_inclusive('\n').enumerate().take(lines.end) {
        operation.check().map_err(|_| ())?;
        if index >= lines.start {
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
                if !has_unescaped_backslash_before(text, byte) {
                    // A better body-context score does not disambiguate displayed note text.
                    if word == pdf[selected] && opaque.iter().any(|range| range.contains(&abs)) {
                        return Err(());
                    }
                    tokens.push(ProseToken::Word(index as u32 + 1, byte, word, false));
                } else {
                    let macros =
                        macros.get_or_insert_with(|| mode.macros(&macro_sources, operation));
                    if matches!(
                        &text[byte..end],
                        "phantom" | "hphantom" | "vphantom" | "hspace" | "vspace" | "index"
                    ) && !macros.contains_key(&text[byte..end])
                    {
                        let mut arg_at = line_start
                            + end
                            + usize::from(matches!(
                                text.as_bytes().get(end),
                                Some(b'*' | b'+')
                            ));
                        while let Some(opt_end) = optional_argument_end(source, arg_at)
                            .or_else(|| overlay_argument_end(source, arg_at))
                        {
                            arg_at = opt_end;
                        }
                        if let Some((_, arg_end)) =
                            note_argument(source, arg_at, source.len(), false)
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
                        ));
                    } else if let (cmd, cmd_end) = math::command(text, byte - 1)
                        && cmd_end == end
                        && text.as_bytes().get(end) != Some(&b'[')
                        && (text.as_bytes().get(end) != Some(&b'{')
                            || math::group(text, end).is_some_and(|(g, _)| g.is_empty()))
                        && let Some((&macro_name, Some(def))) = macros.get_key_value(cmd)
                    {
                        let macro_words = if let Some(cached) = expanded_macros.get(macro_name) {
                            cached.clone()
                        } else {
                            let computed = fixed_prose_macro_words(def, macros, operation)?;
                            expanded_macros.insert(macro_name, computed.clone());
                            computed
                        };
                        if let Some(macro_words) = macro_words {
                            if macro_words.iter().any(|w| *w == pdf[selected])
                                && opaque.iter().any(|range| range.contains(&(abs - 1)))
                            {
                                return Err(());
                            }
                            for w in macro_words {
                                tokens.push(ProseToken::Word(index as u32 + 1, byte - 1, w, true));
                            }
                        }
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
                ProseToken::Word(r, b, w, m) => Some((r, b, w, m)),
                ProseToken::LoopVar(..) => None,
            })
            .collect();
        passes.push(candidates);
    } else {
        let active_loops: Vec<_> = loops
            .iter()
            .filter(|lp| {
                tokens.iter().any(|t| {
                    matches!(t, ProseToken::LoopVar(_, _, abs, v) if lp.body.contains(abs) && lp.vars.contains(v))
                })
            })
            .collect();
        if active_loops.is_empty() || active_loops.len() > 2 {
            return Ok(None);
        }
        let mut combinations: Vec<Vec<(std::ops::Range<usize>, &str, &str)>> = vec![Vec::new()];
        for lp in active_loops {
            let Some(items) = lp.items.as_ref() else {
                return Ok(None);
            };
            if combinations.len().saturating_mul(items.len()) > 32 {
                return Ok(None);
            }
            let mut next = Vec::new();
            for prefix in &combinations {
                for item in items {
                    let mut combined = prefix.clone();
                    for (&var, val) in lp.vars.iter().zip(item.iter()) {
                        if val.contains('\\') {
                            return Ok(None);
                        }
                        combined.push((lp.body.clone(), var, val.as_str()));
                    }
                    next.push(combined);
                }
            }
            combinations = next;
        }
        for combo in combinations {
            let mut candidates = Vec::with_capacity(tokens.len());
            for t in &tokens {
                match t {
                    ProseToken::Word(r, b, w, m) => candidates.push((*r, *b, w.clone(), *m)),
                    ProseToken::LoopVar(r, b, abs, var) => {
                        if let Some((_, _, val)) = combo
                            .iter()
                            .rev()
                            .find(|(body, v, _)| body.contains(abs) && v == var)
                        {
                            for (_, _, w) in source_words(val) {
                                candidates.push((*r, *b, w, false));
                            }
                        }
                    }
                }
            }
            passes.push(candidates);
        }
    }
    let mut best: Option<((isize, bool, isize), u32, u32, usize, bool)> = None;
    let mut tied = false;
    let mut saw_uncorroborated_macro = false;
    for candidates in &passes {
        for (index, (row, byte, word, from_macro)) in candidates.iter().enumerate() {
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
            if *from_macro && score == 0 {
                saw_uncorroborated_macro = true;
                continue;
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
                    && candidates.iter().filter(|(r, _, _, _)| *r == *row).count() <= 2,
                context_bonus,
            );
            match best {
                Some((old_base, _, _, _, _)) if base_rank < old_base => {}
                Some((old_base, old_prox, prev_row, prev_byte, prev_macro))
                    if base_rank == old_base =>
                {
                    if *from_macro || prev_macro {
                        tied |= (prev_row, prev_byte) != (*row, *byte);
                    } else if proximity < old_prox {
                        best = Some((base_rank, proximity, *row, *byte, *from_macro));
                        tied = false;
                    } else if proximity == old_prox {
                        tied |= (prev_row, prev_byte) != (*row, *byte);
                    }
                }
                _ => {
                    best = Some((base_rank, proximity, *row, *byte, *from_macro));
                    tied = false;
                }
            }
        }
    }
    if mode == WordLocationMode::Guess && (tied || (best.is_none() && saw_uncorroborated_macro)) {
        return Err(());
    }
    Ok(best
        .filter(|_| !tied)
        .map(|(rank, _, row, byte, _)| (row, byte, rank.0)))
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

pub(crate) fn host_owns_generated_sidecar(
    source: &str,
    host_path: &std::path::Path,
    generated: &std::path::Path,
    main_dir: Option<&std::path::Path>,
) -> bool {
    let ext = generated.extension().and_then(|e| e.to_str()).unwrap_or("");
    let generated_clean = clean_lexical_path(generated);
    let mut masked = nonprinting_source_spans(source).peekable();
    let mut if_stack: Vec<Option<bool>> = Vec::new();
    let mut at = 0usize;
    let skip_arg = |cur: usize| -> Option<usize> {
        let tok = skip_source_space(source, cur);
        if tok >= source.len() {
            return None;
        }
        if let Some((_, grp_end)) = math::group(source, tok) {
            return Some(grp_end);
        }
        if source.as_bytes()[tok] == b'\\' {
            let (_, cmd_end) = math::command(source, tok);
            return Some(if cmd_end == tok + 1 {
                cmd_end + source[cmd_end..].chars().next().map_or(0, char::len_utf8)
            } else {
                cmd_end
            });
        }
        Some(tok + source[tok..].chars().next()?.len_utf8())
    };
    while at < source.len() {
        if if_stack.last() == Some(&Some(false)) {
            return false;
        }
        while masked.peek().is_some_and(|(_, span, _)| span.end <= at) {
            masked.next();
        }
        if let Some((_, span, _)) = masked.peek()
            && span.start <= at
        {
            at = span.end;
            continue;
        }
        match source.as_bytes()[at] {
            b'%' => {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
            }
            b'=' => {
                if let Some((_, grp_end)) = math::group(source, skip_source_space(source, at + 1)) {
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
                if matches!(cmd, "gobble" | "@gobble")
                    && let Some(arg_end) = skip_arg(next)
                {
                    at = arg_end;
                    continue;
                }
                if let Some(quoted_end) = math::quoted_token_end(source, cmd, next) {
                    at = quoted_end;
                    continue;
                }
                if matches!(
                    cmd,
                    "detokenize"
                        | "unexpanded"
                        | "typeout"
                        | "message"
                        | "errmessage"
                        | "wlog"
                        | "pdfescapestring"
                        | "phantom"
                        | "hphantom"
                        | "vphantom"
                        | "sbox"
                        | "savebox"
                ) {
                    let mut cur = next;
                    while let Some(opt_end) = optional_argument_end(source, cur) {
                        cur = opt_end;
                    }
                    if let Some((_, grp_end)) = math::group(source, skip_source_space(source, cur))
                    {
                        at = grp_end;
                        continue;
                    }
                }
                let skip_args = if matches!(
                    cmd,
                    "@firstoftwo"
                        | "@secondoftwo"
                        | "@gobbletwo"
                        | "IfBooleanT"
                        | "IfBooleanF"
                        | "IfValueT"
                        | "IfValueF"
                        | "IfNoValueT"
                        | "IfNoValueF"
                ) {
                    2usize
                } else if matches!(
                    cmd,
                    "IfBooleanTF" | "IfNoValueTF" | "IfValueTF" | "@ifnextchar"
                ) {
                    3usize
                } else {
                    0usize
                };
                if skip_args > 0 {
                    let mut cur = next;
                    for _ in 0..skip_args {
                        let Some(arg_end) = skip_arg(cur) else {
                            break;
                        };
                        cur = arg_end;
                    }
                    at = cur;
                    continue;
                }
                if cmd.starts_with("toks") || cmd.starts_with("every") {
                    let mut cur = skip_source_space(source, next);
                    while source.as_bytes().get(cur).is_some_and(u8::is_ascii_digit) {
                        cur += 1;
                    }
                    cur = skip_source_space(source, cur);
                    if source.as_bytes().get(cur) == Some(&b'=') {
                        cur = skip_source_space(source, cur + 1);
                    }
                    if let Some((_, grp_end)) = math::group(source, cur) {
                        at = grp_end;
                        continue;
                    }
                }
                if cmd == "iffalse" {
                    if_stack.push(Some(false));
                    at = next;
                    continue;
                }
                if cmd == "iftrue" {
                    if_stack.push(Some(true));
                    at = next;
                    continue;
                }
                if cmd.starts_with("if") && cmd != "iff" {
                    if_stack.push(None);
                    at = next;
                    continue;
                }
                if cmd == "else" {
                    if let Some(last) = if_stack.last_mut() {
                        *last = last.map(|b| !b);
                    }
                    at = next;
                    continue;
                }
                if cmd == "fi" {
                    if_stack.pop();
                    at = next;
                    continue;
                }
                if if_stack.iter().all(|&b| b == Some(true)) {
                    if cmd == "endinput" {
                        break;
                    }
                    if matches!(cmd, "begin" | "end")
                        && let Some((_, grp_end)) =
                            math::group(source, skip_source_space(source, next))
                    {
                        at = grp_end;
                        continue;
                    }
                    let arg_start = skip_source_space(source, next);
                    if matches!(cmd, "input" | "include" | "InputIfFileExists")
                        && math::group(source, arg_start).is_some_and(|(g, _)| {
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

#[cfg(test)]
mod remaining_review_tests {
    use super::*;

    #[test]
    fn proven_fragile_body_excludes_only_its_syntactic_header() {
        for header in [
            r"\begin{frame}[fragile]{\DocTitle}",
            r"\begin{frame}<1->[<+->][fragile]{\DocTitle}{Subtitle}",
            "\\begin{frame}\n[fragile]\n{\\DocTitle}\n{Subtitle}",
            r"\begin{frame}[fragile]{FooterZephyr}",
        ] {
            for separator in ["", "\n"] {
                let source = format!(
                    "\\newcommand{{\\DocTitle}}{{FooterZephyr}}\n\\csname changed\\endcsname\n\\newcommand{{\\unused}}{{Quartz}}\\pretocmd{{\\unused}}{{Amber}}{{}}{{}}\n{header}{separator}Body repeats FooterZephyr as a literal word.\\end{{frame}}"
                );
                let word_byte = source.rfind("FooterZephyr").unwrap();
                let row = source[..word_byte].bytes().filter(|&b| b == b'\n').count() + 1;
                let byte = word_byte - source[..word_byte].rfind('\n').map_or(0, |at| at + 1);
                let operation = crate::Operation::default();
                assert_eq!(
                    source_word_location_in_frame_body_with_sources(
                        &source,
                        source.lines().count() as u32,
                        "FooterZephyr",
                        0,
                        &[],
                        &operation,
                    ),
                    Ok(Some((row as u32, byte))),
                    "{header:?} separator {separator:?}",
                );
            }
        }
        let source = "\\begin{frame}[fragile]{$x$}\nBody $x$\\end{frame}";
        assert_eq!(
            source_word_location_in_frame_body_with_sources(
                source,
                2,
                "x",
                0,
                &[],
                &crate::Operation::default(),
            ),
            Ok(Some((2, 6))),
        );
    }

    #[test]
    fn fragile_header_checks_stop_at_blank_lines_but_not_comment_lines() {
        for (gap, body_group) in [
            ("\n\n", true),
            ("\n \t\n", true),
            ("%comment\n\n", true),
            ("\n%comment\n\n", true),
            ("\n", false),
            ("%comment\n", false),
            ("\n%comment\n", false),
            ("\n%comment\n%comment\n", false),
        ] {
            for newline in ["\n", "\r\n"] {
                let source = format!(
                    "\\begin{{frame}}[fragile]{gap}{{\\small Amber note}} first sentence.\n\
                     Second Amber line.\n\\end{{frame}}\n"
                )
                .replace('\n', newline);
                let group_byte = source.find("Amber note").unwrap();
                let owner = if body_group {
                    group_byte
                } else {
                    source.rfind("Amber").unwrap()
                };
                let row = source[..owner]
                    .bytes()
                    .filter(|&byte| byte == b'\n')
                    .count()
                    + 1;
                let byte = owner - source[..owner].rfind('\n').map_or(0, |at| at + 1);
                assert_eq!(
                    source_word_location_in_frame_body_with_sources(
                        &source,
                        source.lines().count() as u32,
                        if body_group {
                            "Amber note first sentence. Second Amber line."
                        } else {
                            "first sentence. Second Amber line."
                        },
                        if body_group {
                            0
                        } else {
                            "first sentence. Second ".len()
                        },
                        &[],
                        &crate::Operation::default(),
                    ),
                    Ok(Some((row as u32, byte))),
                    "{gap:?} newline={newline:?}",
                );
            }
        }
    }

    #[test]
    fn raw_mutation_targets_consume_tokens_and_preserve_balanced_unknown_group_ends() {
        let raw = r"\csname \pretocmd\csname ";
        assert_eq!(
            math::macro_mutation_target(raw, "pretocmd", 0),
            Some(("**", 7))
        );
        for n in [128, 512] {
            let target = format!("{{{}{}}}", r"\cspreto{".repeat(n), "}".repeat(n));
            for command in ["pretocmd", "cspreto", "AtBeginEnvironment"] {
                assert_eq!(
                    math::macro_mutation_target(&target, command, 0),
                    Some(("**", target.len())),
                    "{command} at {n}",
                );
            }
            for body in [
                r"\pretocmd\csname ".repeat(n),
                format!("{}{}", r"\cspreto{".repeat(n), "}".repeat(n)),
                format!("{}{}", r"\appto{\x}{".repeat(n), "}".repeat(n)),
            ] {
                let source = format!("\\def\\unused{{{body}}}\nQuartz");
                assert_eq!(
                    source_word_location(&source, 2, "Quartz", 0, 0),
                    Some((2, 0))
                );
            }
        }
    }

    #[test]
    fn failed_mutator_optionals_leave_stored_delimiters_and_following_bindings() {
        for command in ["patchcmd", "AtBeginEnvironment"] {
            for n in [128, 512] {
                let body = format!(r"\{command}[").repeat(n);
                let operand = format!("[{body}}}\\newcommand{{\\later}}[1]{{#1}}");
                let boundary = body.len() + 1;
                assert_eq!(optional_argument_boundary(&operand, 0), Err(boundary));
                assert_eq!(
                    math::macro_mutation_target(&operand, command, 0),
                    Some(("**", boundary)),
                );
                let source = format!(
                    "\\def\\unused{{{body}}}\n\\newcommand{{\\emitter}}[1]{{#1}}\n\\let\\alias\\emitter\n\\renewcommand{{\\emitter}}[1]{{Copper}}\n\\newcommand{{\\fresh}}[1]{{#1}}\nQuartz"
                );
                let macros =
                    math::macros_from_sources_for_guess(&[&source], &crate::Operation::default());
                assert!(macros["alias"].is_none());
                assert!(macros["emitter"].is_none());
                let fresh = macros["fresh"].as_ref().unwrap();
                assert_eq!((fresh.arity, fresh.used_params), (1, 1));
                assert_eq!(
                    source_word_location(&source, 6, "Quartz", 0, 0),
                    Some((6, 0))
                );
            }
        }
    }

    #[test]
    fn unresolved_local_mutation_vetoes_stored_and_builtin_guesses() {
        for mutation in [
            r"\cspreto{\dynamicName}{Amber}",
            r"\pretocmd{\csname\dynamicName\endcsname}{Amber}{}{}",
            r"\AddToHook{\dynamicHook}{Amber}",
        ] {
            for body in [r"\emit{Copper}", r"\textit{Copper}", r"$\alpha$"] {
                let source = format!("\\newcommand{{\\emit}}[1]{{#1}}\n{mutation}\n{body}");
                let word = if body.contains("alpha") {
                    "α"
                } else {
                    "Copper"
                };
                assert_eq!(
                    source_word_guess_with_sources(
                        &source,
                        3,
                        word,
                        0,
                        4,
                        &[],
                        &crate::Operation::default(),
                    ),
                    Err(()),
                    "{mutation}: {body}",
                );
            }
        }
    }

    #[test]
    fn unused_unknown_mutation_does_not_poison_an_unrelated_reached_known_mutation() {
        for known in [
            r"\def\unused{Ghost}",
            r"\def\unused{\cspreto{\Name}{Ghost}}",
            r"\appto{\unused}{\cspreto{\Name}{Ghost}}",
            r"\pretocmd{\unused}{\cspreto{\Name}{Ghost}}{}{}",
            r"\patchcmd{\unused}{Ghost}{\cspreto{\Name}{Ghost}}{}{}",
            r"\let\alias\Unknown",
        ] {
            let source = format!(
                "\\documentclass{{beamer}}\n\\usepackage{{opaque}}\n\\def\\Name{{textit}}\n\\newcommand{{\\Unknown}}{{\\cspreto{{\\Name}}{{Amber\\space}}}}\n\\newcommand{{\\Known}}{{{known}}}\n\\Known\n\\begin{{document}}\n\\begin{{frame}}\nBody Copper.\n\\end{{frame}}\n\\end{{document}}"
            );
            assert_eq!(
                source_word_guess_with_sources(
                    &source,
                    10,
                    "Copper",
                    0,
                    4,
                    &[],
                    &crate::Operation::default(),
                ),
                Ok(Some(UnverifiedWordLocation { line: 9, byte: 5 })),
                "{known}",
            );
        }
    }

    #[test]
    fn environment_reachability_does_not_cache_cycle_negatives() {
        let a = r"\b";
        let b = r"\a \c";
        let c = "Amber";
        let macros = [("a", a), ("b", b), ("c", c)]
            .into_iter()
            .map(|(name, body)| {
                (
                    name,
                    Some(math::MacroDef {
                        arity: 0,
                        used_params: 0,
                        body_source: body,
                        body: Some(body),
                    }),
                )
            })
            .collect();
        let operation = crate::Operation::default();
        assert!(body_prints_word(
            r"\a",
            "amber",
            &macros,
            &mut std::collections::BTreeSet::new(),
            &operation
        ));
        assert!(!body_prints_word(
            r"\a",
            "copper",
            &macros,
            &mut std::collections::BTreeSet::new(),
            &operation
        ));
    }

    #[test]
    fn guess_nonprinting_and_preamble_discovery_stop_on_cancellation_or_deadline() {
        let source = "\\documentclass{beamer}\n\\def\\dormant{\\def\\nested{Copper}}\n\\begin{document}\nCopper nimbus.\n\\end{document}\n";
        let operation = crate::Operation::default();
        assert!(
            nonprinting_source_spans_with_mode(
                source,
                None,
                WordLocationMode::Guess,
                Some(&operation),
            )
            .next()
            .is_some(),
        );
        for cancelled in [false, true] {
            let operation = crate::Operation::new(if cancelled {
                std::time::Duration::from_secs(10)
            } else {
                std::time::Duration::ZERO
            });
            if cancelled {
                operation.cancel();
            }
            assert!(
                nonprinting_source_spans_with_mode(
                    source,
                    None,
                    WordLocationMode::Guess,
                    Some(&operation),
                )
                .next()
                .is_none(),
            );
            assert_eq!(
                source_document_start(
                    source,
                    None,
                    &std::collections::BTreeMap::new(),
                    Some(&operation),
                ),
                None,
            );
            assert_eq!(
                operation.check().unwrap_err().kind(),
                if cancelled {
                    std::io::ErrorKind::Interrupted
                } else {
                    std::io::ErrorKind::TimedOut
                },
            );
        }
    }
}
