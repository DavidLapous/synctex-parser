use super::*;

#[derive(Clone)]
struct ForwardNode {
    kind: u8,
    page: u32,
    parent: Option<usize>,
    target: Option<usize>,
    children: Vec<usize>,
    tag: u32,
    line: i64,
    column: i64,
    h: i64,
    v: i64,
    width: i64,
    height: i64,
    depth: i64,
    h_v: i64,
    v_v: i64,
    width_v: i64,
    height_v: i64,
    depth_v: i64,
    mean_line: i64,
    weight: i64,
    friend_weight: usize,
}

impl ForwardNode {
    fn new(
        kind: u8,
        page: u32,
        parent: Option<usize>,
        tag: u32,
        line: i64,
        column: i64,
        coords: [i64; 5],
    ) -> Self {
        let [h, v, width, height, depth] = coords;
        Self {
            kind,
            page,
            parent,
            target: None,
            children: Vec::new(),
            tag,
            line,
            column,
            h,
            v,
            width,
            height,
            depth,
            h_v: h,
            v_v: v,
            width_v: width,
            height_v: height,
            depth_v: depth,
            mean_line: line,
            weight: 1,
            friend_weight: 0,
        }
    }

    fn contain_point(&mut self, ph: i64, pv: i64) {
        if self.kind != b'(' {
            return;
        }
        if self.width_v < 0 {
            let max_h = self.h_v;
            let min_h = max_h.saturating_add(self.width_v);
            if ph < min_h {
                self.width_v = ph.saturating_sub(max_h);
            } else if ph > max_h {
                self.h_v = ph;
                self.width_v = min_h.saturating_sub(ph);
            }
        } else {
            let min_h = self.h_v;
            let max_h = min_h.saturating_add(self.width_v);
            if ph < min_h {
                self.h_v = ph;
                self.width_v = max_h.saturating_sub(ph);
            } else if ph > max_h {
                self.width_v = ph.saturating_sub(min_h);
            }
        }
        let min_v = self.v_v.saturating_sub(self.height_v);
        let max_v = self.v_v.saturating_add(self.depth_v);
        if pv < min_v {
            self.height_v = self.v_v.saturating_sub(pv);
        } else if pv > max_v {
            self.depth_v = pv.saturating_sub(self.v_v);
        }
    }

    fn contain_box(&mut self, bounds: [i64; 4], fix_both_sides: bool) {
        if self.kind != b'(' {
            return;
        }
        let [min_h, max_h, min_v, max_v] = bounds;
        let (cur_min_h, cur_max_h) = if self.width_v < 0 {
            (self.h_v.saturating_add(self.width_v), self.h_v)
        } else {
            (self.h_v, self.h_v.saturating_add(self.width_v))
        };
        let (new_min_h, new_max_h) = if fix_both_sides {
            (cur_min_h.min(min_h), cur_max_h.max(max_h))
        } else if min_h < cur_min_h {
            (min_h, cur_max_h)
        } else if max_h > cur_max_h {
            (cur_min_h, max_h)
        } else {
            (cur_min_h, cur_max_h)
        };
        if self.width_v < 0 {
            self.h_v = new_max_h;
            self.width_v = new_min_h.saturating_sub(new_max_h);
        } else {
            self.h_v = new_min_h;
            self.width_v = new_max_h.saturating_sub(new_min_h);
        }
        let mn = self.v_v.saturating_sub(self.height_v);
        let mx = self.v_v.saturating_add(self.depth_v);
        if min_v < mn {
            self.height_v = self.v_v.saturating_sub(min_v);
        }
        if (fix_both_sides || min_v >= mn) && max_v > mx {
            self.depth_v = max_v.saturating_sub(self.v_v);
        }
    }

    fn data_box(&self) -> [i64; 4] {
        let (min_h, max_h) = if self.width < 0 {
            (self.h.saturating_add(self.width), self.h)
        } else {
            (self.h, self.h.saturating_add(self.width))
        };
        [
            min_h,
            max_h,
            self.v.saturating_sub(self.height),
            self.v.saturating_add(self.depth),
        ]
    }

    fn data_xob(&self) -> [i64; 4] {
        let (min_h, max_h) = if self.width > 0 {
            (self.h.saturating_sub(self.width), self.h)
        } else {
            (self.h, self.h.saturating_sub(self.width))
        };
        [
            min_h,
            max_h,
            self.v.saturating_sub(self.height),
            self.v.saturating_add(self.depth),
        ]
    }

    fn data_box_v(&self) -> [i64; 4] {
        let (min_h, max_h) = if self.width_v < 0 {
            (self.h_v.saturating_add(self.width_v), self.h_v)
        } else {
            (self.h_v, self.h_v.saturating_add(self.width_v))
        };
        [
            min_h,
            max_h,
            self.v_v.saturating_sub(self.height_v),
            self.v_v.saturating_add(self.depth_v),
        ]
    }
}

fn parse_synctex_int_prefix(text: &str) -> Option<(i64, &str)> {
    let bytes = text.as_bytes();
    let mut index = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    Some((text.get(..index)?.parse().ok()?, text.get(index..)?))
}

const MAX_PROXY_DEPTH: usize = MAX_BOX_DEPTH;
pub(crate) const MAX_BOX_DEPTH: usize = 512;
const MAX_FORWARD_NODES: usize = 500_000;
const MAX_SYNCTEX_BYTES: usize = 128 * 1024 * 1024;

#[allow(clippy::too_many_arguments)]
fn clone_synctex_proxy_subtree(
    nodes: &mut Vec<ForwardNode>,
    source: usize,
    parent: Option<usize>,
    page: u32,
    dh: i64,
    dv: i64,
    depth: usize,
    register_friends: &mut Option<&mut [Vec<usize>; 1024]>,
    sheet_hboxes: &mut Option<&mut Vec<usize>>,
) -> io::Result<usize> {
    if depth > MAX_PROXY_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SyncTeX form proxy depth exceeds limit",
        ));
    }
    if nodes.len() >= MAX_FORWARD_NODES {
        return Err(io::Error::other("SyncTeX node count exceeds limit"));
    }
    if nodes[source].kind == b'f' {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SyncTeX form proxy references unresolved form",
        ));
    }
    let mut clone = nodes[source].clone();
    let children = std::mem::take(&mut clone.children);
    clone.page = page;
    clone.parent = parent;
    clone.target = nodes[source].target.or(Some(source));
    clone.h = clone.h.saturating_add(dh);
    clone.v = clone.v.saturating_add(dv);
    clone.h_v = clone.h_v.saturating_add(dh);
    clone.v_v = clone.v_v.saturating_add(dv);
    let index = nodes.len();
    nodes.push(clone);
    if let Some(friends) = register_friends.as_deref_mut()
        && matches!(
            nodes[index].kind,
            b'[' | b'(' | b'v' | b'h' | b'b' | b'k' | b'g' | b'r' | b'$' | b'x'
        )
    {
        let bucket = ((i64::from(nodes[index].tag).saturating_add(nodes[index].line)).unsigned_abs()
            as usize)
            % 1024;
        friends[bucket].push(index);
    }
    if let Some(hboxes) = sheet_hboxes.as_deref_mut()
        && nodes[index].kind == b'('
    {
        hboxes.push(index);
    }
    for child in children {
        let child_idx = clone_synctex_proxy_subtree(
            nodes,
            child,
            Some(index),
            page,
            dh,
            dv,
            depth + 1,
            register_friends,
            sheet_hboxes,
        )?;
        nodes[index].children.push(child_idx);
    }
    Ok(index)
}

struct SheetEntry {
    page: u32,
    root: usize,
    hboxes: Vec<usize>,
}

pub(crate) struct ParsedSynctex {
    inputs: Vec<(u32, String)>,
    max_lines: std::collections::HashMap<u32, i64>,
    nodes: Vec<ForwardNode>,
    friends: Box<[Vec<usize>; 1024]>,
    sheets: std::collections::BTreeMap<u32, usize>,
    sheet_list: Vec<SheetEntry>,
    unit: f64,
    x_off: f64,
    y_off: f64,
    bound: i64,
    fix_cross_file_tags: bool,
}

#[cfg(test)]
pub(crate) fn parse_synctex_document(reader: impl BufRead) -> io::Result<ParsedSynctex> {
    parse_synctex_document_with_options(reader, false, None, None)
}

pub(crate) fn synctex_unit_scale(unit: &str) -> Option<f64> {
    match unit {
        "in" => Some(72.27 * 65536.0),
        "cm" => Some(72.27 * 65536.0 / 2.54),
        "mm" => Some(72.27 * 65536.0 / 25.4),
        "pt" => Some(65536.0),
        "bp" => Some(72.27 / 72.0 * 65536.0),
        "pc" => Some(12.0 * 65536.0),
        "sp" => Some(1.0),
        "dd" => Some(1238.0 / 1157.0 * 65536.0),
        "cc" => Some(14856.0 / 1157.0 * 65536.0),
        "nd" => Some(685.0 / 642.0 * 65536.0),
        "nc" => Some(1370.0 / 107.0 * 65536.0),
        _ => None,
    }
}

pub(crate) fn parse_synctex_document_with_options(
    reader: impl BufRead,
    fix_cross_file_tags: bool,
    target_page: Option<u32>,
    operation: Option<&Operation>,
) -> io::Result<ParsedSynctex> {
    let mut reader = reader.take(MAX_SYNCTEX_BYTES as u64 + 1);
    let track_friends = target_page.is_none();
    let mut inputs: Vec<(u32, String)> = Vec::new();
    let mut max_lines = std::collections::HashMap::<u32, i64>::new();
    let mut pre_magnification = 1000i64;
    let mut pre_unit = 8192i64;
    let mut pre_x_offset = 578i64;
    let mut pre_y_offset = 578i64;
    let mut post_magnification = None::<f64>;
    let mut post_x_offset = None::<f64>;
    let mut post_y_offset = None::<f64>;
    let mut in_post_scriptum = false;

    let mut nodes = Vec::<ForwardNode>::new();
    let mut friends: Box<[Vec<usize>; 1024]> = Box::new(std::array::from_fn(|_| Vec::new()));
    let mut sheets = std::collections::BTreeMap::<u32, usize>::new();
    let mut sheet_list = Vec::<SheetEntry>::new();
    let mut forms = std::collections::HashMap::<u32, usize>::new();
    let mut ref_in_form = Vec::<(usize, usize, u32, i64, i64)>::new();
    let mut ref_in_sheet = Vec::<(usize, usize, u32, Option<usize>, u32, i64, i64)>::new();

    let mut current_page = None::<u32>;
    let mut current_sheet_idx = None::<usize>;
    let mut form_stack = Vec::<usize>::new();
    let mut ignored_form_depth = 0usize;
    let mut parent = None::<usize>;
    let mut x_stack = Vec::<Vec<usize>>::new();
    let mut last_k = None::<usize>;
    let mut last_g = None::<usize>;
    let mut last_v = -1i64;

    let make_friend = |friends: &mut [Vec<usize>; 1024], nodes: &[ForwardNode], idx: usize| {
        if !track_friends {
            return;
        }
        let bucket = ((i64::from(nodes[idx].tag).saturating_add(nodes[idx].line)).unsigned_abs()
            as usize)
            % 1024;
        friends[bucket].push(idx);
    };
    let record_max_line =
        |max_lines: &mut std::collections::HashMap<u32, i64>, tag: u32, line: i64| {
            if track_friends {
                max_lines
                    .entry(tag)
                    .and_modify(|max| *max = (*max).max(line))
                    .or_insert(line);
            }
        };
    let flush_x_set = |x_stack: &mut [Vec<usize>],
                       friends: &mut [Vec<usize>; 1024],
                       nodes: &mut [ForwardNode],
                       in_form: bool,
                       source_idx: usize| {
        if let Some(pending) = x_stack.last_mut()
            && !pending.is_empty()
        {
            let (tag, line, col) = (
                nodes[source_idx].tag,
                nodes[source_idx].line,
                nodes[source_idx].column,
            );
            for x_idx in pending.drain(..).rev() {
                nodes[x_idx].tag = tag;
                nodes[x_idx].line = line;
                nodes[x_idx].column = col;
                if !in_form {
                    make_friend(friends, nodes, x_idx);
                }
            }
        }
    };
    let flush_x_make = |x_stack: &mut [Vec<usize>],
                        friends: &mut [Vec<usize>; 1024],
                        nodes: &[ForwardNode],
                        in_form: bool| {
        if let Some(pending) = x_stack.last_mut()
            && !pending.is_empty()
        {
            for x_idx in pending.drain(..).rev() {
                if !in_form {
                    make_friend(friends, nodes, x_idx);
                }
            }
        }
    };
    fn parse_link_pos<'a>(
        body: &'a str,
        last_v: &mut i64,
    ) -> Option<(u32, i64, i64, i64, i64, &'a str)> {
        let (link, rest) = body.split_once(':')?;
        let (tag_str, line_rest) = link.split_once(',')?;
        let tag = tag_str.parse::<u32>().ok()?;
        let (line, col) = match line_rest.split_once(',') {
            Some((l_str, c_str)) => (
                l_str.parse::<i64>().ok()?,
                parse_synctex_int_prefix(c_str).map_or(-1, |(c, _)| c),
            ),
            None => (line_rest.parse::<i64>().ok()?, -1),
        };
        let (pos_str, rem) = rest.split_once(':').unwrap_or((rest, ""));
        let (x_str, y_str) = pos_str.split_once(',')?;
        let x = x_str.parse::<i64>().ok()?;
        let y = if y_str.starts_with('=') {
            *last_v
        } else {
            let (parsed_y, _) = parse_synctex_int_prefix(y_str)?;
            *last_v = parsed_y;
            parsed_y
        };
        Some((tag, line, col, x, y, rem))
    }
    let check_geom = |vals: &[i64]| -> io::Result<()> {
        if vals.contains(&i64::MIN) {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported SyncTeX geometry value",
            ))
        } else {
            Ok(())
        }
    };
    let parse_whd = |rem: &str| -> Option<(i64, i64, i64)> {
        let mut parts = rem.splitn(3, ',');
        let w = parts.next()?.parse::<i64>().ok()?;
        let h = parts.next()?.parse::<i64>().ok()?;
        let (d, _) = parse_synctex_int_prefix(parts.next()?)?;
        Some((w, h, d))
    };
    let parse_post_dim = |val: &str| -> Option<f64> {
        let s = val.trim();
        let unit_start = s.len().checked_sub(2)?;
        let num = s
            .get(..unit_start)?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())?;
        Some(num * synctex_unit_scale(s.get(unit_start..)?)?)
    };

    let mut row = String::new();
    let mut bytes_read = 0usize;
    let mut line_count = 0usize;
    let mut saw_preamble = false;
    let mut saw_postamble = false;
    let mut saw_count = false;
    loop {
        if line_count & 1023 == 0
            && let Some(op) = operation
        {
            op.check()?;
        }
        line_count = line_count.saturating_add(1);
        row.clear();
        let count = reader.read_line(&mut row)?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(count);
        if bytes_read >= MAX_SYNCTEX_BYTES {
            return Err(io::Error::other("SyncTeX source map exceeds 128 MiB"));
        }
        let trimmed = row.trim_end_matches(['\r', '\n']);
        if ignored_form_depth > 0 {
            if trimmed.starts_with('<') {
                ignored_form_depth += 1;
            } else if trimmed.starts_with('>') {
                ignored_form_depth -= 1;
            }
            continue;
        }
        if trimmed.starts_with("SyncTeX Version:") {
            saw_preamble = true;
        } else if trimmed == "Postamble:" {
            if current_page.is_some()
                || !form_stack.is_empty()
                || ignored_form_depth > 0
                || parent.is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unclosed SyncTeX container before postamble",
                ));
            }
            saw_postamble = true;
        } else if saw_postamble && let Some(count_str) = trimmed.strip_prefix("Count:") {
            if count_str.trim().parse::<u32>().is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid SyncTeX Count",
                ));
            }
            saw_count = true;
        } else if let Some(input) = trimmed.strip_prefix("Input:")
            && let Some((tag_str, path)) = input.split_once(':')
            && let Ok(tag) = tag_str.parse::<u32>()
        {
            inputs.push((tag, path.trim_end_matches(' ').to_string()));
        } else if let Some(val) = trimmed.strip_prefix("Magnification:") {
            if in_post_scriptum {
                let mag = val
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|&m| m.is_finite() && m > 0.0)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid postscript Magnification",
                        )
                    })?;
                post_magnification = Some(mag);
            } else {
                let mag = val
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|&m| m > 0)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid SyncTeX Magnification")
                    })?;
                pre_magnification = mag;
            }
        } else if let Some(val) = trimmed.strip_prefix("Unit:") {
            let unit = val
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|&u| u > 0)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid SyncTeX Unit")
                })?;
            pre_unit = unit;
        } else if let Some(val) = trimmed.strip_prefix("X Offset:") {
            if in_post_scriptum {
                post_x_offset = Some(parse_post_dim(val).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid postscript X Offset")
                })?);
            } else {
                pre_x_offset = val.trim().parse::<i64>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid SyncTeX X Offset")
                })?;
            }
        } else if let Some(val) = trimmed.strip_prefix("Y Offset:") {
            if in_post_scriptum {
                post_y_offset = Some(parse_post_dim(val).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid postscript Y Offset")
                })?);
            } else {
                pre_y_offset = val.trim().parse::<i64>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid SyncTeX Y Offset")
                })?;
            }
        } else if trimmed.starts_with("Post scriptum:") {
            in_post_scriptum = true;
        } else if trimmed.starts_with("Content:") {
            last_v = -1;
        } else if let Some(page_str) = trimmed.strip_prefix('{')
            && form_stack.is_empty()
            && current_page.is_none()
            && let Ok((page, _)) = parse_synctex_int_prefix(page_str).ok_or(())
            && let Ok(page) = u32::try_from(page)
        {
            if target_page.is_some_and(|want| want != page) {
                current_page = Some(page);
                current_sheet_idx = None;
                parent = None;
            } else {
                if nodes.len() >= MAX_FORWARD_NODES {
                    return Err(io::Error::other("SyncTeX node count exceeds limit"));
                }
                current_page = Some(page);
                let root = nodes.len();
                nodes.push(ForwardNode::new(b'{', page, None, 0, 0, 0, [0; 5]));
                sheets.insert(page, root);
                sheet_list.push(SheetEntry {
                    page,
                    root,
                    hboxes: Vec::new(),
                });
                current_sheet_idx = Some(sheet_list.len() - 1);
                parent = Some(root);
            }
            x_stack.clear();
            last_k = None;
            last_g = None;
        } else if let Some(page_str) = trimmed.strip_prefix('}')
            && form_stack.is_empty()
            && current_page.is_some()
        {
            if let Some((close_page, _)) = parse_synctex_int_prefix(page_str)
                && u32::try_from(close_page).ok() != current_page
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "mismatched SyncTeX sheet close tag",
                ));
            }
            if let Some(p_idx) = parent
                && nodes[p_idx].kind != b'{'
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unclosed SyncTeX box at sheet end",
                ));
            }
            current_page = None;
            current_sheet_idx = None;
            parent = None;
            x_stack.clear();
            last_k = None;
            last_g = None;
        } else if let Some(tag_str) = trimmed.strip_prefix('<') {
            if let Some((tag, _)) = parse_synctex_int_prefix(tag_str)
                && let Ok(tag) = u32::try_from(tag)
                && !forms.contains_key(&tag)
            {
                if nodes.len() >= MAX_FORWARD_NODES || form_stack.len() >= MAX_BOX_DEPTH {
                    return Err(io::Error::other("SyncTeX form nesting exceeds limit"));
                }
                let root = nodes.len();
                nodes.push(ForwardNode::new(b'<', 0, parent, tag, 0, 0, [0; 5]));
                forms.insert(tag, root);
                form_stack.push(root);
                parent = Some(root);
                last_k = None;
                last_g = None;
            } else {
                ignored_form_depth = 1;
            }
        } else if trimmed.starts_with('>') && !form_stack.is_empty() {
            let form_root = form_stack.pop().unwrap();
            if parent != Some(form_root) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unclosed SyncTeX box at form end",
                ));
            }
            parent = nodes[form_root]
                .parent
                .or_else(|| current_sheet_idx.map(|s_idx| sheet_list[s_idx].root));
            last_k = None;
            last_g = None;
        } else if (current_page.is_some() || !form_stack.is_empty())
            && let Some(&kind) = trimmed.as_bytes().first()
        {
            let in_form = !form_stack.is_empty();
            if !in_form && current_sheet_idx.is_none() {
                match kind {
                    b'[' | b'(' | b'v' | b'h' | b'r' | b'k' | b'g' | b'$' | b'x' => {
                        let _ = parse_link_pos(&trimmed[1..], &mut last_v);
                    }
                    b'f' => {
                        if let Some((tag_str, rest)) = trimmed[1..].split_once(':')
                            && tag_str.parse::<u32>().is_ok()
                            && let Some((x_str, y_str)) = rest.split_once(',')
                            && x_str.parse::<i64>().is_ok()
                            && !y_str.starts_with('=')
                            && let Some((py, _)) = parse_synctex_int_prefix(y_str)
                        {
                            last_v = py;
                        }
                    }
                    _ => {}
                }
                row.clear();
                continue;
            }
            if nodes.len() + 2 >= MAX_FORWARD_NODES {
                return Err(io::Error::other("SyncTeX node count exceeds limit"));
            }
            let page = if in_form {
                0
            } else {
                current_page.unwrap_or(0)
            };
            match kind {
                b'[' | b'(' => {
                    if x_stack.len() >= MAX_BOX_DEPTH {
                        return Err(io::Error::other("SyncTeX box nesting depth exceeds limit"));
                    }
                    if let Some((tag, line, col, x, y, rem)) =
                        parse_link_pos(&trimmed[1..], &mut last_v)
                        && let Some((w, h, d)) = parse_whd(rem)
                    {
                        check_geom(&[x, y, w, h, d])?;
                        let idx = nodes.len();
                        nodes.push(ForwardNode::new(
                            kind,
                            page,
                            parent,
                            tag,
                            line,
                            col,
                            [x, y, w, h, d],
                        ));
                        if let Some(p_idx) = parent {
                            nodes[p_idx].children.push(idx);
                        }
                        parent = Some(idx);
                        x_stack.push(Vec::new());
                        if kind == b'(' {
                            let bdry = nodes.len();
                            nodes.push(ForwardNode::new(
                                b'b',
                                page,
                                Some(idx),
                                tag,
                                line,
                                col,
                                [x, y, 0, 0, 0],
                            ));
                            nodes[idx].children.push(bdry);
                            if !in_form {
                                make_friend(&mut friends, &nodes, bdry);
                            }
                        }
                        record_max_line(&mut max_lines, tag, line);
                        last_k = None;
                        last_g = None;
                    }
                }
                b']' => {
                    if let Some(p_idx) = parent
                        && nodes[p_idx].kind == b'['
                    {
                        if nodes[p_idx].children.is_empty() && !in_form {
                            make_friend(&mut friends, &nodes, p_idx);
                        }
                        let next_parent = nodes[p_idx].parent;
                        flush_x_make(&mut x_stack, &mut friends, &nodes, in_form);
                        x_stack.pop();
                        flush_x_set(&mut x_stack, &mut friends, &mut nodes, in_form, p_idx);
                        parent = next_parent;
                        last_k = None;
                        last_g = None;
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "mismatched SyncTeX vbox close",
                        ));
                    }
                }
                b')' => {
                    if let Some(p_idx) = parent
                        && nodes[p_idx].kind == b'('
                    {
                        if !in_form && let Some(s_idx) = current_sheet_idx {
                            sheet_list[s_idx].hboxes.push(p_idx);
                        }
                        let ch_len = nodes[p_idx].children.len();
                        if ch_len > 1 {
                            let first_child = nodes[p_idx].children[0];
                            let second_child = nodes[p_idx].children[1];
                            let (second_tag, second_line) =
                                (nodes[second_child].tag, nodes[second_child].line);
                            if fix_cross_file_tags {
                                let changed = nodes[first_child].tag != second_tag
                                    || nodes[first_child].line != second_line;
                                nodes[first_child].tag = second_tag;
                                nodes[first_child].line = second_line;
                                if changed && !in_form {
                                    make_friend(&mut friends, &nodes, first_child);
                                }
                            } else {
                                nodes[first_child].line = second_line;
                            }
                            let mut nw = 0i64;
                            let mut cum = 0i64;
                            for i in 1..ch_len {
                                let c_idx = nodes[p_idx].children[i];
                                if nodes[c_idx].kind == b'(' {
                                    let cw = nodes[c_idx].weight.max(1);
                                    nw = nw.saturating_add(cw);
                                    cum = cum
                                        .saturating_add(nodes[c_idx].mean_line.saturating_mul(cw));
                                } else {
                                    nw = nw.saturating_add(1);
                                    cum = cum.saturating_add(nodes[c_idx].line);
                                }
                            }
                            nodes[p_idx].mean_line = cum.saturating_add(nw / 2) / nw.max(1);
                            nodes[p_idx].weight = nw;
                        } else {
                            nodes[p_idx].mean_line = nodes[p_idx].line;
                            nodes[p_idx].weight = 1;
                        }
                        let last_c = nodes[p_idx]
                            .children
                            .iter()
                            .rev()
                            .copied()
                            .find(|&c| nodes[c].kind != b'f')
                            .unwrap_or(nodes[p_idx].children[0]);
                        let (last_tag, last_line, last_col) =
                            (nodes[last_c].tag, nodes[last_c].line, nodes[last_c].column);
                        let (end_h, end_v) = (
                            nodes[p_idx].h_v.saturating_add(nodes[p_idx].width_v),
                            nodes[p_idx].v_v,
                        );
                        let bdry2 = nodes.len();
                        nodes.push(ForwardNode::new(
                            b'b',
                            page,
                            Some(p_idx),
                            last_tag,
                            last_line,
                            last_col,
                            [end_h, end_v, 0, 0, 0],
                        ));
                        nodes[p_idx].children.push(bdry2);
                        let first_c = nodes[p_idx].children[0];
                        let (start_h, start_v) = (nodes[p_idx].h_v, nodes[p_idx].v_v);
                        nodes[first_c].h = start_h;
                        nodes[first_c].v = start_v;
                        if let (Some(k_idx), Some(g_idx)) = (last_k, last_g) {
                            for i in 0..nodes[p_idx].children.len().saturating_sub(1) {
                                if nodes[p_idx].children[i + 1] == k_idx {
                                    let prev_idx = nodes[p_idx].children[i];
                                    let (ptag, pline, pcol) = (
                                        nodes[prev_idx].tag,
                                        nodes[prev_idx].line,
                                        nodes[prev_idx].column,
                                    );
                                    nodes[k_idx].tag = ptag;
                                    nodes[k_idx].line = pline;
                                    nodes[k_idx].column = pcol;
                                    nodes[g_idx].tag = ptag;
                                    nodes[g_idx].line = pline;
                                    nodes[g_idx].column = pcol;
                                    if fix_cross_file_tags && !in_form {
                                        make_friend(&mut friends, &nodes, k_idx);
                                        make_friend(&mut friends, &nodes, g_idx);
                                    }
                                    break;
                                }
                            }
                        }
                        let next_parent = nodes[p_idx].parent;
                        let box_v = nodes[p_idx].data_box_v();
                        flush_x_make(&mut x_stack, &mut friends, &nodes, in_form);
                        x_stack.pop();
                        flush_x_set(&mut x_stack, &mut friends, &mut nodes, in_form, p_idx);
                        if let Some(np_idx) = next_parent {
                            nodes[np_idx].contain_box(box_v, fix_cross_file_tags);
                        }
                        parent = next_parent;
                        last_k = None;
                        last_g = None;
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "mismatched SyncTeX hbox close",
                        ));
                    }
                }
                b'v' | b'h' | b'r' => {
                    if let Some((mut tag, mut line, col, x, y, rem)) =
                        parse_link_pos(&trimmed[1..], &mut last_v)
                        && let Some((w, h, d)) = parse_whd(rem)
                    {
                        check_geom(&[x, y, w, h, d])?;
                        if fix_cross_file_tags
                            && kind == b'r'
                            && w == 0
                            && let Some(p_idx) = parent
                            && tag != nodes[p_idx].tag
                        {
                            tag = nodes[p_idx].tag;
                            line = nodes[p_idx].line;
                        }
                        let idx = nodes.len();
                        nodes.push(ForwardNode::new(
                            kind,
                            page,
                            parent,
                            tag,
                            line,
                            col,
                            [x, y, w, h, d],
                        ));
                        if let Some(p_idx) = parent {
                            nodes[p_idx].children.push(idx);
                            if kind == b'h' {
                                let bounds = nodes[idx].data_box();
                                nodes[p_idx].contain_box(bounds, fix_cross_file_tags);
                            }
                        }
                        if kind == b'r' && !in_form {
                            make_friend(&mut friends, &nodes, idx);
                        }
                        flush_x_set(&mut x_stack, &mut friends, &mut nodes, in_form, idx);
                        record_max_line(&mut max_lines, tag, line);
                        last_k = None;
                        last_g = None;
                    }
                }
                b'k' => {
                    if let Some((tag, line, col, x, y, rem)) =
                        parse_link_pos(&trimmed[1..], &mut last_v)
                        && let Some((w, _)) = parse_synctex_int_prefix(rem)
                    {
                        check_geom(&[x, y, w])?;
                        let idx = nodes.len();
                        nodes.push(ForwardNode::new(
                            b'k',
                            page,
                            parent,
                            tag,
                            line,
                            col,
                            [x, y, w, 0, 0],
                        ));
                        if let Some(p_idx) = parent {
                            nodes[p_idx].children.push(idx);
                            let bounds = nodes[idx].data_xob();
                            nodes[p_idx].contain_box(bounds, fix_cross_file_tags);
                        }
                        if !in_form {
                            make_friend(&mut friends, &nodes, idx);
                        }
                        flush_x_set(&mut x_stack, &mut friends, &mut nodes, in_form, idx);
                        record_max_line(&mut max_lines, tag, line);
                        last_k = Some(idx);
                        last_g = None;
                    }
                }
                b'g' | b'$' => {
                    if let Some((tag, line, col, x, y, _)) =
                        parse_link_pos(&trimmed[1..], &mut last_v)
                    {
                        check_geom(&[x, y])?;
                        let idx = nodes.len();
                        nodes.push(ForwardNode::new(
                            kind,
                            page,
                            parent,
                            tag,
                            line,
                            col,
                            [x, y, 0, 0, 0],
                        ));
                        if let Some(p_idx) = parent {
                            nodes[p_idx].children.push(idx);
                            nodes[p_idx].contain_point(x, y);
                        }
                        if !in_form {
                            make_friend(&mut friends, &nodes, idx);
                        }
                        flush_x_set(&mut x_stack, &mut friends, &mut nodes, in_form, idx);
                        record_max_line(&mut max_lines, tag, line);
                        if kind == b'g' && last_k.is_some() {
                            last_g = Some(idx);
                        } else {
                            last_k = None;
                            last_g = None;
                        }
                    }
                }
                b'x' => {
                    if let Some((tag, line, col, x, y, _)) =
                        parse_link_pos(&trimmed[1..], &mut last_v)
                    {
                        check_geom(&[x, y])?;
                        let idx = nodes.len();
                        nodes.push(ForwardNode::new(
                            b'x',
                            page,
                            parent,
                            tag,
                            line,
                            col,
                            [x, y, 0, 0, 0],
                        ));
                        let prev_is_bdry = parent.is_some_and(|p_idx| {
                            nodes[p_idx].children.len() == 1
                                && nodes[nodes[p_idx].children[0]].kind == b'b'
                        });
                        if let Some(p_idx) = parent {
                            nodes[p_idx].children.push(idx);
                            nodes[p_idx].contain_point(x, y);
                        }
                        if prev_is_bdry || x_stack.last().is_some_and(|s| !s.is_empty()) {
                            if let Some(pending) = x_stack.last_mut() {
                                pending.push(idx);
                            }
                        } else if !in_form {
                            make_friend(&mut friends, &nodes, idx);
                        }
                        record_max_line(&mut max_lines, tag, line);
                        last_k = None;
                        last_g = None;
                    }
                }
                b'f' => {
                    if let Some((tag_str, rest)) = trimmed[1..].split_once(':')
                        && let Ok(ftag) = tag_str.parse::<u32>()
                        && let Some((x_str, y_str)) = rest.split_once(',')
                        && let Ok(fx) = x_str.parse::<i64>()
                    {
                        let fy = if y_str.starts_with('=') {
                            last_v
                        } else if let Some((py, _)) = parse_synctex_int_prefix(y_str) {
                            last_v = py;
                            py
                        } else {
                            row.clear();
                            continue;
                        };
                        if let Some(p_idx) = parent {
                            check_geom(&[fx, fy])?;
                            let ref_idx = nodes.len();
                            nodes.push(ForwardNode::new(
                                b'f',
                                page,
                                Some(p_idx),
                                0,
                                0,
                                0,
                                [fx, fy, 0, 0, 0],
                            ));
                            nodes[p_idx].children.push(ref_idx);
                            if in_form {
                                ref_in_form.push((ref_idx, p_idx, ftag, fx, fy));
                            } else {
                                ref_in_sheet.push((
                                    ref_idx,
                                    p_idx,
                                    page,
                                    current_sheet_idx,
                                    ftag,
                                    fx,
                                    fy,
                                ));
                            }
                        }
                        last_k = None;
                        last_g = None;
                    }
                }
                _ => {
                    last_k = None;
                    last_g = None;
                }
            }
        }
        row.clear();
    }

    if current_page.is_some()
        || !form_stack.is_empty()
        || ignored_form_depth > 0
        || parent.is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unclosed SyncTeX sheet, form, or box at EOF",
        ));
    }
    if (saw_preamble || saw_postamble) && !(saw_postamble && saw_count) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated SyncTeX postamble",
        ));
    }

    for &(ref_idx, p_idx, ftag, fx, fy) in &ref_in_form {
        if let Some(op) = operation {
            op.check()?;
        }
        let Some(&form_root) = forms.get(&ftag) else {
            nodes[p_idx].children.retain(|&c| c != ref_idx);
            continue;
        };
        let target = nodes[form_root].children.first().copied().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "SyncTeX form reference targets empty form",
            )
        })?;
        let (dh, dv) = (fx, fy.saturating_sub(nodes[target].height));
        let mut none_friends = None;
        let mut none_hboxes = None;
        let proxy = clone_synctex_proxy_subtree(
            &mut nodes,
            target,
            Some(p_idx),
            0,
            dh,
            dv,
            0,
            &mut none_friends,
            &mut none_hboxes,
        )?;
        if let Some(pos) = nodes[p_idx].children.iter().position(|&c| c == ref_idx) {
            nodes[p_idx].children[pos] = proxy;
        }
    }
    for &(ref_idx, p_idx, page, sheet_idx, ftag, fx, fy) in &ref_in_sheet {
        if let Some(op) = operation {
            op.check()?;
        }
        let Some(&form_root) = forms.get(&ftag) else {
            nodes[p_idx].children.retain(|&c| c != ref_idx);
            continue;
        };
        let target = nodes[form_root].children.first().copied().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "SyncTeX form reference targets empty form",
            )
        })?;
        let (dh, dv) = (fx, fy.saturating_sub(nodes[target].height));
        let mut reg = track_friends.then_some(&mut *friends);
        let mut hboxes = sheet_idx.map(|s_idx| &mut sheet_list[s_idx].hboxes);
        let proxy = clone_synctex_proxy_subtree(
            &mut nodes,
            target,
            Some(p_idx),
            page,
            dh,
            dv,
            0,
            &mut reg,
            &mut hboxes,
        )?;
        if let Some(pos) = nodes[p_idx].children.iter().position(|&c| c == ref_idx) {
            nodes[p_idx].children[pos] = proxy;
        }
    }

    let base_unit = (pre_unit as f64) / 65781.76;
    let mag_factor = (pre_magnification as f64) / 1000.0;
    let unit = post_magnification.map_or(base_unit, |m| m * base_unit) * mag_factor;
    let x_off = post_x_offset.map_or((pre_x_offset as f64) * base_unit, |x| x / 65781.76);
    let y_off = post_y_offset.map_or((pre_y_offset as f64) * base_unit, |y| y / 65781.76);
    let bound = (1_500_000.0 / mag_factor) as i64;

    Ok(ParsedSynctex {
        inputs,
        max_lines,
        nodes,
        friends,
        sheets,
        sheet_list,
        unit,
        x_off,
        y_off,
        bound,
        fix_cross_file_tags,
    })
}

pub fn forward_candidates(
    pdf: &Path,
    file: &Path,
    snapshot: Option<&SourceSnapshot>,
    view_line: u32,
) -> io::Result<Vec<ForwardCandidate>> {
    forward_candidates_with_page_hint(pdf, file, snapshot, view_line, None)
}

pub fn forward_candidates_with_page_hint(
    pdf: &Path,
    file: &Path,
    snapshot: Option<&SourceSnapshot>,
    view_line: u32,
    page_hint: Option<u32>,
) -> io::Result<Vec<ForwardCandidate>> {
    forward_candidates_in(pdf, file, snapshot, view_line, page_hint, None)
}

pub fn forward_candidates_in(
    pdf: &Path,
    file: &Path,
    snapshot: Option<&SourceSnapshot>,
    view_line: u32,
    page_hint: Option<u32>,
    build_directory: Option<&Path>,
) -> io::Result<Vec<ForwardCandidate>> {
    let (reader, sidecar_path) = crate::open_synctex_with_path_in(pdf, build_directory)?;
    forward_candidates_from_reader_with_page_hint_in(
        reader,
        pdf,
        file,
        snapshot,
        view_line,
        true,
        page_hint,
        sidecar_path.parent(),
    )
}

pub fn forward_candidates_from_reader(
    reader: impl BufRead,
    pdf: &Path,
    file: &Path,
    snapshot: Option<&SourceSnapshot>,
    view_line: u32,
) -> io::Result<Vec<ForwardCandidate>> {
    forward_candidates_from_reader_with_options(reader, pdf, file, snapshot, view_line, true)
}

pub fn forward_candidates_from_reader_with_options(
    reader: impl BufRead,
    pdf: &Path,
    file: &Path,
    snapshot: Option<&SourceSnapshot>,
    view_line: u32,
    fix_readahead: bool,
) -> io::Result<Vec<ForwardCandidate>> {
    forward_candidates_from_reader_with_page_hint(
        reader,
        pdf,
        file,
        snapshot,
        view_line,
        fix_readahead,
        None,
    )
}

pub fn forward_candidates_from_reader_with_page_hint(
    reader: impl BufRead,
    pdf: &Path,
    file: &Path,
    snapshot: Option<&SourceSnapshot>,
    view_line: u32,
    fix_readahead: bool,
    page_hint: Option<u32>,
) -> io::Result<Vec<ForwardCandidate>> {
    forward_candidates_from_reader_with_page_hint_in(
        reader,
        pdf,
        file,
        snapshot,
        view_line,
        fix_readahead,
        page_hint,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn forward_candidates_from_reader_with_page_hint_in(
    reader: impl BufRead,
    pdf: &Path,
    file: &Path,
    snapshot: Option<&SourceSnapshot>,
    view_line: u32,
    fix_readahead: bool,
    page_hint: Option<u32>,
    sidecar_dir: Option<&Path>,
) -> io::Result<Vec<ForwardCandidate>> {
    let pdf_dir = pdf.parent().unwrap_or(Path::new("."));
    let current_dir = std::env::current_dir()
        .ok()
        .and_then(|dir| fs::canonicalize(&dir).ok().or(Some(dir)));
    let ParsedSynctex {
        inputs,
        max_lines,
        mut nodes,
        friends,
        sheets,
        unit,
        x_off,
        y_off,
        bound,
        ..
    } = parse_synctex_document_with_options(reader, fix_readahead, None, None)?;

    let mut matching_tags = std::collections::HashSet::new();
    let mut input_tag = None::<u32>;
    for (tag, raw_path) in inputs.iter().rev() {
        let raw = Path::new(raw_path);
        let normalized = match snapshot {
            Some(snapshot) => snapshot.normalize(raw).ok(),
            None if raw.is_absolute() => {
                Some(fs::canonicalize(raw).unwrap_or_else(|_| raw.to_owned()))
            }
            None => crate::source_snapshot::resolve_relative_input(
                raw,
                current_dir.as_deref(),
                Some(pdf_dir),
                sidecar_dir,
            )
            .or_else(|| {
                crate::source_snapshot::normalize_absolute(raw, pdf_dir)
                    .ok()
                    .map(|path| fs::canonicalize(&path).unwrap_or(path))
            }),
        };
        if normalized.as_deref() == Some(file) {
            matching_tags.insert(*tag);
            if input_tag.is_none() {
                input_tag = Some(*tag);
            }
        }
    }
    if input_tag.is_none()
        && snapshot.is_none()
        && let Some(name) = file.file_name()
    {
        let mut matched_path = None::<&str>;
        for (tag, raw_path) in inputs.iter().rev() {
            let raw = Path::new(raw_path);
            if raw.file_name() == Some(name)
                && crate::source_snapshot::resolve_relative_input(
                    raw,
                    current_dir.as_deref(),
                    Some(pdf_dir),
                    sidecar_dir,
                )
                .is_none()
            {
                match matched_path {
                    None => {
                        matched_path = Some(raw_path.as_str());
                        input_tag = Some(*tag);
                        matching_tags.insert(*tag);
                    }
                    Some(prev) if prev == raw_path.as_str() => {
                        matching_tags.insert(*tag);
                    }
                    Some(_) => {
                        input_tag = None;
                        matching_tags.clear();
                        break;
                    }
                }
            }
        }
    }
    let Some(input_tag) = input_tag else {
        return Err(io::Error::other(ForwardUnavailable(
            "synctex view returned no complete match",
        )));
    };

    let node_mean_line = |nodes: &[ForwardNode], idx: usize| -> i64 {
        let n = nodes[idx].target.unwrap_or(idx);
        if nodes[n].kind == b'(' && nodes[n].target.is_none() {
            nodes[n].mean_line
        } else if let Some(p_idx) = nodes[n].parent
            && nodes[p_idx].kind == b'('
            && nodes[p_idx].target.is_none()
        {
            nodes[p_idx].mean_line
        } else {
            nodes[idx].line
        }
    };

    let box_visible = |nodes: &[ForwardNode], mut idx: usize| -> usize {
        if !matches!(nodes[idx].kind, b'[' | b'(' | b'v' | b'h')
            && let Some(p_idx) = nodes[idx].parent
        {
            idx = p_idx;
        }
        let mean = node_mean_line(nodes, idx);
        let mut cursor = idx;
        while let Some(p_idx) = nodes[cursor].parent {
            cursor = p_idx;
            if matches!(nodes[cursor].kind, b'(' | b'h') {
                if mean
                    .saturating_sub(node_mean_line(nodes, cursor))
                    .saturating_abs()
                    > 1
                {
                    return idx;
                } else if nodes[cursor].width > bound
                    || nodes[cursor].height.saturating_add(nodes[cursor].depth) > bound
                {
                    return cursor;
                }
                idx = cursor;
            }
        }
        idx
    };

    let display_query = |nodes: &mut [ForwardNode],
                         tag: u32,
                         query_line: u32|
     -> Vec<ForwardCandidate> {
        let max_l = max_lines.get(&tag).copied().unwrap_or(0);
        let mut line = i64::from(query_line).min(max_l);
        let mut line_offset = 1i64;
        for _ in 0..100 {
            if line <= max_l {
                let bucket_idx =
                    ((i64::from(tag).saturating_add(line)).unsigned_abs() as usize) % 1024;
                for exclude_box in [true, false] {
                    let mut pages: Vec<(u32, Vec<usize>)> = Vec::new();
                    let mut first_page = None::<u32>;
                    for &target in friends[bucket_idx].iter().rev() {
                        if (exclude_box && matches!(nodes[target].kind, b'[' | b'(' | b'v' | b'h'))
                            || nodes[target].tag != tag
                            || nodes[target].line != line
                        {
                            continue;
                        }
                        let t_page = nodes[target].page;
                        if pages.is_empty() {
                            pages.push((t_page, vec![target]));
                            first_page = Some(t_page);
                        } else if pages.len() == 1 && Some(t_page) == first_page {
                            pages[0].1.insert(0, target);
                        } else if let Some(entry) = pages.iter_mut().find(|(p, _)| *p == t_page) {
                            let insert_idx = 1.min(entry.1.len());
                            entry.1.insert(insert_idx, target);
                        } else if pages.len() == 1 {
                            pages.push((t_page, vec![target]));
                        } else {
                            pages.insert(0, (t_page, vec![target]));
                        }
                    }
                    if !pages.is_empty() {
                        let mut sorted_pages = Vec::with_capacity(pages.len());
                        for (p_num, page_nodes) in pages {
                            for &n_idx in &page_nodes {
                                if let Some(p_idx) = nodes[n_idx].parent {
                                    nodes[p_idx].friend_weight = 0;
                                }
                            }
                            let mut weighted = Vec::with_capacity(page_nodes.len());
                            for n_idx in page_nodes {
                                if let Some(p_idx) = nodes[n_idx].parent {
                                    let has_weight_slot =
                                        nodes[p_idx].kind == b'(' && nodes[p_idx].target.is_none();
                                    let cached_w = if has_weight_slot {
                                        nodes[p_idx].friend_weight
                                    } else {
                                        0
                                    };
                                    if cached_w == 0 {
                                        let w = nodes[p_idx]
                                            .children
                                            .iter()
                                            .filter(|&&c| {
                                                nodes[c].tag == tag && nodes[c].line == line
                                            })
                                            .count();
                                        if has_weight_slot {
                                            nodes[p_idx].friend_weight = w;
                                        }
                                        weighted.push((w, n_idx));
                                    } else {
                                        weighted.push((0, n_idx));
                                    }
                                } else {
                                    weighted.push((0, n_idx));
                                }
                            }
                            let mut res_list = vec![weighted[0]];
                            for item in weighted.into_iter().skip(1) {
                                if item.0 > res_list[0].0 {
                                    res_list.insert(0, item);
                                } else if item.0 == 0 {
                                    continue;
                                } else {
                                    let mut idx = 0;
                                    while idx + 1 < res_list.len() {
                                        idx += 1;
                                        if item.0 < res_list[idx].0 {
                                            continue;
                                        }
                                        break;
                                    }
                                    res_list.insert(idx + 1, item);
                                }
                            }
                            if let Some(zero_pos) =
                                res_list.iter().skip(1).position(|(w, _)| *w == 0)
                            {
                                res_list.truncate(zero_pos + 1);
                            }
                            sorted_pages.push((
                                p_num,
                                res_list.into_iter().map(|(_, n)| n).collect::<Vec<_>>(),
                            ));
                        }
                        let hint = page_hint.unwrap_or(0);
                        let mut ordered = vec![sorted_pages[0].clone()];
                        let mut best_match = ordered[0].0;
                        for sib in sorted_pages.into_iter().skip(1) {
                            if sib.0.abs_diff(hint) < best_match.abs_diff(hint) {
                                best_match = sib.0;
                                ordered.insert(0, sib);
                            } else {
                                ordered.insert(1, sib);
                            }
                        }
                        let mut out = Vec::new();
                        for (p_num, page_nodes) in ordered {
                            if p_num == 0 {
                                continue;
                            }
                            for n_idx in page_nodes {
                                let bv = box_visible(nodes, n_idx);
                                let h = ((nodes[bv].h_v as f64) * unit + x_off) as f32;
                                let v = ((nodes[bv].v_v.saturating_add(nodes[bv].depth_v) as f64)
                                    * unit
                                    + y_off) as f32;
                                let width = ((nodes[bv].width_v as f64) * unit) as f32;
                                let height = ((nodes[bv].height_v.saturating_add(nodes[bv].depth_v)
                                    as f64)
                                    * unit) as f32;
                                if !forward_geometry_valid(p_num, h, v, width, height) {
                                    continue;
                                }
                                let candidate = ForwardCandidate {
                                    page: p_num,
                                    rect: SearchRect {
                                        left: h,
                                        top: v - height,
                                        right: h + width,
                                        bottom: v,
                                    },
                                };
                                if !out.iter().any(|old: &ForwardCandidate| {
                                    old.page == candidate.page && old.rect == candidate.rect
                                }) {
                                    out.push(candidate);
                                }
                            }
                        }
                        return out;
                    }
                }
                line += line_offset;
                line_offset = if line_offset < 0 {
                    -(line_offset - 1)
                } else {
                    -(line_offset + 1)
                };
                if line <= 0 {
                    line += line_offset;
                    line_offset = if line_offset < 0 {
                        -(line_offset - 1)
                    } else {
                        -(line_offset + 1)
                    };
                }
            } else {
                break;
            }
        }
        Vec::new()
    };

    let mut candidates = display_query(&mut nodes, input_tag, view_line);
    if fix_readahead
        && let Ok(first) = best_guess_forward_candidate(&candidates)
        && let Some((next_page, target_line)) = resolve_readahead_sheet_in_memory(
            &nodes,
            &sheets,
            &matching_tags,
            view_line,
            first.page,
        )
    {
        let mut overflow: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|candidate| candidate.page >= next_page)
            .collect();
        if best_guess_forward_candidate(&overflow).is_err() {
            overflow = display_query(&mut nodes, input_tag, target_line)
                .into_iter()
                .filter(|candidate| candidate.page >= next_page)
                .collect();
        }
        if best_guess_forward_candidate(&overflow).is_ok() {
            candidates = overflow;
        }
    }
    if candidates.is_empty() {
        return Err(io::Error::other(ForwardUnavailable(
            "synctex view returned no complete match",
        )));
    }
    Ok(candidates)
}

/// TeX stamps each sheet's outer vboxes, running header/footer, and rule nodes
/// with the input scanner position `(tag, oline)` at `\shipout`. When a page
/// break splits a paragraph or display before `oline`, SyncTeX can rank the
/// preceding sheet's header ahead of the overflow sheet where `view_line` is
/// actually rendered.
fn resolve_readahead_sheet_in_memory(
    nodes: &[ForwardNode],
    sheets: &std::collections::BTreeMap<u32, usize>,
    matching_tags: &std::collections::HashSet<u32>,
    view_line: u32,
    first_page: u32,
) -> Option<(u32, u32)> {
    let mut target_tag = None::<u32>;
    let mut readahead = None::<(u32, u32)>;
    let mut resolved = None::<(u32, u32)>;
    for (&page, &sheet_root) in sheets.range(first_page..) {
        let Some(&outer_idx) = nodes[sheet_root].children.first() else {
            continue;
        };
        if !matches!(nodes[outer_idx].kind, b'[' | b'(') {
            return resolved;
        }
        let otag = nodes[outer_idx].tag;
        let Ok(oline) = u32::try_from(nodes[outer_idx].line) else {
            return resolved;
        };
        if page == first_page {
            if !matching_tags.contains(&otag) {
                return None;
            }
            target_tag = Some(otag);
        }
        let mut s_max = 0u32;
        let mut min_next = None::<u32>;
        let mut hbox_means = Vec::<u32>::new();
        let mut stack = vec![(outer_idx, false)];
        let mut hbox_stats = Vec::<(u32, u64, u32)>::new();
        while let Some((idx, exiting)) = stack.pop() {
            let node = &nodes[idx];
            if exiting {
                if node.kind == b'('
                    && Some(node.tag) == target_tag
                    && let Some((box_line, sum, count)) = hbox_stats.pop()
                    && let Some((prev_max, _)) = readahead
                {
                    let mean = if count > 0 {
                        (sum / u64::from(count)) as u32
                    } else {
                        box_line
                    };
                    if mean > prev_max {
                        hbox_means.push(mean);
                    }
                }
                continue;
            }
            match node.kind {
                b'[' | b'(' => {
                    if node.kind == b'(' && Some(node.tag) == target_tag {
                        hbox_stats.push((u32::try_from(node.line).unwrap_or(0), 0, 0));
                    }
                    stack.push((idx, true));
                    for &child in node.children.iter().rev() {
                        stack.push((child, false));
                    }
                }
                b'v' | b'h' | b'x' | b'k' | b'g' | b'$' if Some(node.tag) == target_tag => {
                    if let Ok(line) = u32::try_from(node.line) {
                        if otag == node.tag && line < oline {
                            s_max = s_max.max(line);
                            if page == first_page && s_max >= view_line {
                                return None;
                            }
                        }
                        if let Some((prev_max, _)) = readahead {
                            if (otag, oline) != (node.tag, line) && line > prev_max {
                                min_next = Some(min_next.map_or(line, |old| old.min(line)));
                            }
                            if let Some(parent_idx) = node.parent
                                && nodes[parent_idx].kind == b'('
                                && Some(nodes[parent_idx].tag) == target_tag
                                && let Some(top) = hbox_stats.last_mut()
                            {
                                top.1 += u64::from(line);
                                top.2 += 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if page == first_page {
            if view_line > s_max && (s_max == 0 || oline.abs_diff(view_line) <= view_line - s_max) {
                readahead = Some((s_max, oline));
            } else {
                return None;
            }
        } else if let Some((_, prev_oline)) = readahead
            && let Some(min_line) = min_next
        {
            if min_line > prev_oline {
                return resolved;
            }
            let target_line = hbox_means
                .iter()
                .copied()
                .min_by_key(|line| (line.abs_diff(view_line), *line))
                .unwrap_or(min_line);
            resolved = Some((page, target_line));
            if Some(otag) == target_tag
                && view_line > s_max
                && (s_max == 0 || oline.abs_diff(view_line) <= view_line - s_max)
            {
                readahead = Some((s_max, oline));
            } else {
                return resolved;
            }
        }
    }
    resolved
}

pub(crate) fn forward_geometry_valid(page: u32, h: f32, v: f32, width: f32, height: f32) -> bool {
    page > 0
        && width >= 0.0
        && height >= 0.0
        && [h, v, width, height, h + width, v - height]
            .into_iter()
            .all(f32::is_finite)
}

pub(crate) fn validate_inverse_point(point: InversePoint) -> io::Result<()> {
    if point.page == 0
        || !point.x.is_finite()
        || !point.y_from_top.is_finite()
        || !point.page_height_pt.is_finite()
        || point.page_height_pt <= 0.0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SyncTeX inverse query point",
        ));
    }
    Ok(())
}

fn is_graphics_leaf_subtree(nodes: &[ForwardNode], root: usize) -> bool {
    let d = &nodes[root];
    if d.kind != b'(' || d.target.is_some() || d.width <= 0 || d.height <= 0 || d.depth != 0 {
        return false;
    }
    let mut stack = vec![root];
    while let Some(idx) = stack.pop() {
        let n = &nodes[idx];
        let mut hboxes = 0usize;
        for &ch in &n.children {
            let c = &nodes[ch];
            if c.target.is_some() || c.tag != d.tag || c.line != d.line {
                return false;
            }
            match c.kind {
                b'(' => {
                    hboxes += 1;
                    if hboxes > 1 {
                        return false;
                    }
                    stack.push(ch);
                }
                b'b' | b'g' | b'k' => {}
                b'r' if c.width == n.width && c.height == n.height && c.depth == 0 => {}
                _ => return false,
            }
        }
    }
    true
}

fn single_hbox_child(nodes: &[ForwardNode], idx: usize, allow_kern: bool) -> Option<usize> {
    let mut found = None;
    for &ch in &nodes[idx].children {
        match nodes[ch].kind {
            b'(' => {
                if found.replace(ch).is_some() {
                    return None;
                }
            }
            b'b' | b'g' => {}
            b'k' if allow_kern && nodes[ch].target.is_none() => {}
            _ => return None,
        }
    }
    found
}

pub(crate) fn is_generated_graphics_tex(path: &str) -> bool {
    let p = path.trim().as_bytes();
    [b".pdf_tex".as_slice(), b".pdftex_t", b".pstex_t"]
        .iter()
        .any(|ext| p.len() >= ext.len() && p[p.len() - ext.len()..].eq_ignore_ascii_case(ext))
}

fn tag_is_generated_graphics_tex(inputs: &[(u32, String)], tag: u32) -> bool {
    inputs
        .iter()
        .rev()
        .find(|(t, _)| *t == tag)
        .is_some_and(|(_, p)| is_generated_graphics_tex(p))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GraphicsBox {
    box_idx: usize,
    host_idx: usize,
    gen_root: Option<usize>,
}

/// Detect the outer bounding horizontal box of an `\includegraphics` /
/// `\includesvg` construct from its `\Gscale@box` zero-width hbox pair and its
/// enclosing `\Ginclude@...` / `\Gin@setfile` wrappers (`graphics.sty` routes
/// both scaled and unscaled `\includegraphics` through `\Gscale@box`).
fn graphics_outer_box(
    nodes: &[ForwardNode],
    inputs: &[(u32, String)],
    fix_cross_file_tags: bool,
    b_idx: usize,
) -> Option<GraphicsBox> {
    let b = &nodes[b_idx];
    if b.kind != b'('
        || b.target.is_some()
        || b.tag == 0
        || b.line <= 0
        || b.width != 0
        || b.height <= 0
        || b.depth != 0
    {
        return None;
    }
    let c_idx = single_hbox_child(nodes, b_idx, false)?;
    let c = &nodes[c_idx];
    if c.target.is_some()
        || c.tag != b.tag
        || c.line != b.line
        || c.width != 0
        || c.height <= 0
        || c.depth != 0
    {
        return None;
    }
    let d_idx = single_hbox_child(nodes, c_idx, true)?;
    let d = &nodes[d_idx];
    if d.tag != b.tag || d.line != b.line || !is_graphics_leaf_subtree(nodes, d_idx) {
        return None;
    }
    let compensated = c.children.iter().any(|&ch| {
        let child = &nodes[ch];
        child.tag == b.tag
            && child.line == b.line
            && (child.kind == b'g'
                || (child.kind == b'k' && child.width == d.width.saturating_neg()))
    });
    if !compensated {
        return None;
    }
    let mut cur = b_idx;
    let mut outer = b_idx;
    let mut wrappers = 0usize;
    while let Some(p_idx) = nodes[cur].parent {
        let p = &nodes[p_idx];
        let pure_wrapper = p
            .children
            .iter()
            .all(|&ch| ch == cur || nodes[ch].kind == b'b');
        if p.kind != b'('
            || p.target.is_some()
            || p.tag != b.tag
            || p.line != b.line
            || single_hbox_child(nodes, p_idx, true) != Some(cur)
            || !(pure_wrapper || p.width == 0 || nodes[cur].width == 0)
        {
            break;
        }
        wrappers += 1;
        cur = p_idx;
        if p.width > 0 && p.height.saturating_add(p.depth) > 0 {
            outer = p_idx;
        }
    }
    if wrappers < 2 || outer == b_idx {
        return None;
    }
    if tag_is_generated_graphics_tex(inputs, nodes[outer].tag) {
        if !fix_cross_file_tags {
            return None;
        }
        let gen_tag = nodes[outer].tag;
        while let Some(p_idx) = nodes[cur].parent {
            let p = &nodes[p_idx];
            if !matches!(p.kind, b'(' | b'[') || p.target.is_some() || p.tag != gen_tag {
                break;
            }
            cur = p_idx;
            if p.width > 0 && p.height.saturating_add(p.depth) > 0 {
                outer = p_idx;
            }
        }
        let host_idx = nodes[cur].parent?;
        let host = &nodes[host_idx];
        if !matches!(host.kind, b'(' | b'[')
            || host.target.is_some()
            || host.tag == 0
            || host.line <= 0
            || tag_is_generated_graphics_tex(inputs, host.tag)
        {
            return None;
        }
        return Some(GraphicsBox {
            box_idx: outer,
            host_idx,
            gen_root: Some(cur),
        });
    }
    Some(GraphicsBox {
        box_idx: outer,
        host_idx: outer,
        gen_root: None,
    })
}

fn sheet_graphics_boxes(
    nodes: &[ForwardNode],
    inputs: &[(u32, String)],
    fix_cross_file_tags: bool,
    root: usize,
) -> Vec<GraphicsBox> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(idx) = stack.pop() {
        if let Some(g) = graphics_outer_box(nodes, inputs, fix_cross_file_tags, idx) {
            if !out.iter().any(|existing: &GraphicsBox| {
                existing.box_idx == g.box_idx && existing.host_idx == g.host_idx
            }) {
                out.push(g);
            }
            continue;
        }
        for &ch in nodes[idx].children.iter().rev() {
            stack.push(ch);
        }
    }
    out
}

pub fn inverse_candidates(pdf: &Path, point: InversePoint) -> io::Result<Vec<SourceLocation>> {
    inverse_candidates_in(pdf, point, None)
}

pub fn inverse_candidates_in(
    pdf: &Path,
    point: InversePoint,
    build_directory: Option<&Path>,
) -> io::Result<Vec<SourceLocation>> {
    validate_inverse_point(point)?;
    let (reader, sidecar_path) = crate::open_synctex_with_path_in(pdf, build_directory)?;
    let parsed = parse_synctex_document_with_options(reader, true, Some(point.page), None)?;
    let raw = inverse_candidates_in_doc(&parsed, point)?;
    Ok(crate::materialize_raw_candidates(
        raw,
        Some(pdf),
        sidecar_path.parent(),
    ))
}

pub(crate) fn inverse_raw_candidates_in(
    pdf: &Path,
    point: InversePoint,
    build_directory: Option<&Path>,
    operation: Option<&Operation>,
) -> io::Result<Vec<RawInverseCandidate>> {
    validate_inverse_point(point)?;
    if let Some(op) = operation {
        op.check()?;
    }
    let reader = crate::open_synctex_in(pdf, build_directory)?;
    let parsed = parse_synctex_document_with_options(reader, true, Some(point.page), operation)?;
    inverse_candidates_in_doc(&parsed, point)
}

pub fn inverse_candidates_from_reader(
    reader: impl BufRead,
    point: InversePoint,
) -> io::Result<Vec<SourceLocation>> {
    inverse_candidates_from_reader_with_options(reader, point, false)
}

pub fn inverse_candidates_from_reader_with_options(
    reader: impl BufRead,
    point: InversePoint,
    fix_cross_file_tags: bool,
) -> io::Result<Vec<SourceLocation>> {
    validate_inverse_point(point)?;
    let parsed =
        parse_synctex_document_with_options(reader, fix_cross_file_tags, Some(point.page), None)?;
    let raw = inverse_candidates_in_doc(&parsed, point)?;
    Ok(crate::materialize_raw_candidates(raw, None, None))
}

pub(crate) fn inverse_candidates_in_doc(
    parsed: &ParsedSynctex,
    point: InversePoint,
) -> io::Result<Vec<RawInverseCandidate>> {
    validate_inverse_point(point)?;
    let Some(sheet) = parsed.sheet_list.iter().find(|s| s.page == point.page) else {
        return Ok(Vec::new());
    };
    let raw_h = (f64::from(point.x) - parsed.x_off) / parsed.unit;
    let raw_v = (f64::from(point.y_from_top) - parsed.y_off) / parsed.unit;
    if !raw_h.is_finite()
        || !raw_v.is_finite()
        || raw_h < f64::from(i32::MIN)
        || raw_h > f64::from(i32::MAX)
        || raw_v < f64::from(i32::MIN)
        || raw_v > f64::from(i32::MAX)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SyncTeX inverse query point out of scaled-point range",
        ));
    }
    let hit_h = i64::from(raw_h as i32);
    let hit_v = i64::from(raw_v as i32);
    let nodes = &parsed.nodes;

    let data_tag = |idx: usize| -> u32 {
        if nodes[idx].target.is_some() {
            0
        } else {
            nodes[idx].tag
        }
    };
    let data_line = |idx: usize| -> i64 {
        if nodes[idx].target.is_some() {
            0
        } else {
            nodes[idx].line
        }
    };
    let data_col = |idx: usize| -> i64 {
        if nodes[idx].target.is_some() {
            0
        } else {
            nodes[idx].column
        }
    };
    let data_width = |idx: usize| -> i64 {
        if nodes[idx].target.is_some() {
            0
        } else {
            nodes[idx].width
        }
    };
    let parent_hd = |idx: usize| -> (i64, i64) {
        nodes[idx].parent.map_or((0, 0), |p| {
            (
                nodes[p].height.saturating_abs(),
                nodes[p].depth.saturating_abs(),
            )
        })
    };

    let h_ordered_distance = |idx: usize| -> i64 {
        let n = &nodes[idx];
        match n.kind {
            b'[' | b'v' | b'h' => {
                let min = n.h;
                let max = min.saturating_add(n.width.saturating_abs());
                if hit_h < min {
                    min.saturating_sub(hit_h)
                } else if hit_h > max {
                    max.saturating_sub(hit_h)
                } else {
                    0
                }
            }
            b'(' => {
                let min = n.h_v;
                let max = min.saturating_add(n.width_v.saturating_abs());
                if hit_h < min {
                    min.saturating_sub(hit_h)
                } else if hit_h > max {
                    max.saturating_sub(hit_h)
                } else {
                    0
                }
            }
            b'k' => {
                let w = n.width;
                let (min, max) = if w < 0 {
                    (n.h, n.h.saturating_sub(w))
                } else {
                    (n.h.saturating_sub(w), n.h)
                };
                let med = min.saturating_add(max) / 2;
                if hit_h < min {
                    min.saturating_sub(hit_h).saturating_add(1)
                } else if hit_h > max {
                    max.saturating_sub(hit_h).saturating_sub(1)
                } else if hit_h > med {
                    max.saturating_sub(hit_h).saturating_add(1)
                } else {
                    min.saturating_sub(hit_h).saturating_sub(1)
                }
            }
            b'r' | b'g' | b'$' | b'x' | b'b' => n.h.saturating_sub(hit_h),
            _ => i64::from(i32::MAX),
        }
    };

    let v_ordered_distance = |idx: usize| -> i64 {
        let n = &nodes[idx];
        let (min, max) = match n.kind {
            b'[' | b'v' | b'h' => (
                n.v.saturating_sub(n.height.saturating_abs()),
                n.v.saturating_add(n.depth.saturating_abs()),
            ),
            b'(' => (
                n.v_v.saturating_sub(n.height_v.saturating_abs()),
                n.v_v.saturating_add(n.depth_v.saturating_abs()),
            ),
            b'r' | b'k' | b'g' | b'$' => {
                let (ph, pd) = parent_hd(idx);
                (n.v.saturating_sub(ph), n.v.saturating_add(pd))
            }
            _ => return i64::from(i32::MAX),
        };
        if hit_v < min {
            min.saturating_sub(hit_v)
        } else if hit_v > max {
            max.saturating_sub(hit_v)
        } else {
            0
        }
    };

    let point_in_box =
        |idx: usize| -> bool { h_ordered_distance(idx) == 0 && v_ordered_distance(idx) == 0 };

    let dist_to_box = |min_h: i64, max_h: i64, min_v: i64, max_v: i64| -> i64 {
        let dh = if hit_h < min_h {
            min_h.saturating_sub(hit_h)
        } else if hit_h <= max_h {
            0
        } else {
            hit_h.saturating_sub(max_h)
        };
        let dv = if hit_v < min_v {
            min_v.saturating_sub(hit_v)
        } else if hit_v <= max_v {
            0
        } else {
            hit_v.saturating_sub(max_v)
        };
        dh.saturating_add(dv)
    };

    let point_node_distance = |idx: usize| -> i64 {
        let n = &nodes[idx];
        match n.kind {
            b'[' => dist_to_box(
                n.h,
                n.h.saturating_add(n.width.saturating_abs()),
                n.v.saturating_sub(n.height.saturating_abs()),
                n.v.saturating_add(n.depth.saturating_abs()),
            ),
            b'(' => dist_to_box(
                n.h_v,
                n.h_v.saturating_add(n.width_v.saturating_abs()),
                n.v_v.saturating_sub(n.height_v.saturating_abs()),
                n.v_v.saturating_add(n.depth_v.saturating_abs()),
            ),
            b'v' | b'h' => {
                let min_v = n.v.saturating_sub(n.height.saturating_abs());
                let max_v = n.v.saturating_add(n.depth.saturating_abs());
                let right_h = n.h.saturating_add(n.width.saturating_abs());
                dist_to_box(n.h, n.h, min_v, max_v).min(dist_to_box(right_h, right_h, min_v, max_v))
            }
            b'k' => {
                let (ph, _) = parent_hd(idx);
                let min_v = n.v.saturating_sub(ph);
                let other_h = n.h.saturating_sub(n.width);
                dist_to_box(n.h, n.h, min_v, n.v).min(dist_to_box(other_h, other_h, min_v, n.v))
            }
            b'g' | b'$' | b'x' | b'b' => {
                let (ph, _) = parent_hd(idx);
                dist_to_box(n.h, n.h, n.v.saturating_sub(ph), n.v)
            }
            _ => i64::from(i32::MAX),
        }
    };

    let smallest_container = |node: usize, other: usize| -> usize {
        let dims = |idx: usize| -> (i64, i64) {
            if nodes[idx].kind == b'(' {
                let w = nodes[idx].width_v.saturating_abs();
                let th = nodes[idx]
                    .depth_v
                    .saturating_abs()
                    .saturating_add(nodes[idx].height_v.saturating_abs());
                (w.saturating_mul(th), th)
            } else {
                (0, 0)
            }
        };
        let (area, th) = dims(node);
        let (other_area, other_th) = dims(other);
        if area < other_area {
            return node;
        }
        if area > other_area {
            return other;
        }
        let dw = data_width(node).saturating_abs();
        let other_dw = data_width(other).saturating_abs();
        if dw > other_dw {
            return node;
        }
        if dw < other_dw {
            return other;
        }
        if th < other_th {
            return node;
        }
        if th > other_th {
            return other;
        }
        node
    };

    fn deepest_container_v2(
        nodes: &[ForwardNode],
        idx: usize,
        point_in_box: &impl Fn(usize) -> bool,
        point_node_distance: &impl Fn(usize) -> i64,
    ) -> Option<usize> {
        if nodes[idx].children.is_empty() {
            return None;
        }
        for &ch in &nodes[idx].children {
            if point_in_box(ch)
                && let Some(deep) =
                    deepest_container_v2(nodes, ch, point_in_box, point_node_distance)
            {
                return Some(deep);
            }
        }
        if nodes[idx].kind == b'[' {
            let mut best = None::<(usize, i64)>;
            for &ch in &nodes[idx].children {
                if !nodes[ch].children.is_empty() {
                    let d = point_node_distance(ch);
                    if best.is_none_or(|(_, bd)| d <= bd) {
                        best = Some((ch, d));
                    }
                }
            }
            if let Some((b_idx, _)) = best {
                return Some(b_idx);
            }
        }
        point_in_box(idx).then_some(idx)
    }

    fn deepest_container_v3(
        nodes: &[ForwardNode],
        idx: usize,
        point_in_box: &impl Fn(usize) -> bool,
        point_node_distance: &impl Fn(usize) -> i64,
    ) -> Option<(usize, i64)> {
        if nodes[idx].children.is_empty() {
            return None;
        }
        for &ch in &nodes[idx].children {
            if let Some(deep) = deepest_container_v3(nodes, ch, point_in_box, point_node_distance) {
                return Some(deep);
            }
        }
        if nodes[idx].kind == b'[' {
            let mut best = None::<(usize, i64)>;
            for &ch in &nodes[idx].children {
                if !nodes[ch].children.is_empty() {
                    let d = point_node_distance(ch);
                    if best.is_none_or(|(_, bd)| d < bd) {
                        best = Some((ch, d));
                    }
                }
            }
            if let Some(b) = best {
                return Some(b);
            }
        }
        point_in_box(idx).then_some((idx, 0))
    }

    fn closest_deep_child_v2(
        nodes: &[ForwardNode],
        idx: usize,
        point_node_distance: &impl Fn(usize) -> i64,
    ) -> (Option<usize>, i64) {
        if nodes[idx].children.is_empty() {
            return (None, i64::from(i32::MAX));
        }
        let mut best = (None::<usize>, i64::from(i32::MAX));
        for &ch in &nodes[idx].children {
            let nd = if matches!(nodes[ch].kind, b'[' | b'(' | b'v' | b'h') {
                closest_deep_child_v2(nodes, ch, point_node_distance)
            } else {
                (Some(ch), point_node_distance(ch))
            };
            if nd.1 < best.1
                || (nd.1 == best.1
                    && nd
                        .0
                        .is_some_and(|n| !(nodes[n].target.is_none() && nodes[n].kind == b'k')))
            {
                best = nd;
            }
        }
        best
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn get_closest_children_in_box_v2(
        nodes: &[ForwardNode],
        idx: usize,
        h_ordered_distance: &impl Fn(usize) -> i64,
        point_in_box: &impl Fn(usize) -> bool,
        point_node_distance: &impl Fn(usize) -> i64,
        data_tag: &impl Fn(usize) -> u32,
        data_line: &impl Fn(usize) -> i64,
        data_col: &impl Fn(usize) -> i64,
    ) -> (Option<(usize, i64)>, Option<(usize, i64)>) {
        if nodes[idx].children.is_empty() || nodes[idx].kind != b'(' {
            return (None, None);
        }
        let mut l = None::<(usize, i64)>;
        let mut r = None::<(usize, i64)>;
        for &ch in &nodes[idx].children {
            let d = h_ordered_distance(ch);
            if d > 0 {
                let replace = match r {
                    None => true,
                    Some((rn, rd)) => {
                        d < rd
                            || (d == rd
                                && data_tag(rn) == data_tag(ch)
                                && (data_line(rn) > data_line(ch)
                                    || (data_line(rn) == data_line(ch)
                                        && data_col(rn) > data_col(ch))))
                    }
                };
                if replace {
                    r = Some((ch, d));
                }
            } else if d == 0 {
                if !nodes[ch].children.is_empty() {
                    return get_closest_children_in_box_v2(
                        nodes,
                        ch,
                        h_ordered_distance,
                        point_in_box,
                        point_node_distance,
                        data_tag,
                        data_line,
                        data_col,
                    );
                }
                l = Some((ch, 0));
            } else {
                let d = d.saturating_neg();
                let replace = match l {
                    None => true,
                    Some((ln, ld)) => {
                        d < ld
                            || (d == ld
                                && data_tag(ln) == data_tag(ch)
                                && (data_line(ln) > data_line(ch)
                                    || (data_line(ln) == data_line(ch)
                                        && data_col(ln) > data_col(ch))))
                    }
                };
                if replace {
                    l = Some((ch, d));
                }
            }
        }
        if let Some(mut cur_l) = l {
            if let Some(nd) =
                deepest_container_v3(nodes, cur_l.0, point_in_box, point_node_distance)
            {
                cur_l = nd;
            }
            if matches!(nodes[cur_l.0].kind, b'[' | b'(' | b'v' | b'h')
                && let (Some(cn), _) = closest_deep_child_v2(nodes, cur_l.0, point_node_distance)
            {
                cur_l.0 = cn;
            }
            l = Some(cur_l);
        }
        if let Some(mut cur_r) = r {
            if let Some(nd) =
                deepest_container_v3(nodes, cur_r.0, point_in_box, point_node_distance)
            {
                cur_r = nd;
            }
            if matches!(nodes[cur_r.0].kind, b'[' | b'(' | b'v' | b'h')
                && let (Some(cn), _) = closest_deep_child_v2(nodes, cur_r.0, point_node_distance)
            {
                cur_r.0 = cn;
            }
            r = Some(cur_r);
        }
        (l, r)
    }

    let mut container = None::<usize>;
    let mut hbox_iter = sheet.hboxes.iter().rev().copied();
    for hbox in hbox_iter.by_ref() {
        if point_in_box(hbox) {
            let mut best = hbox;
            for next in hbox_iter {
                if point_in_box(next) {
                    best = smallest_container(next, best);
                }
            }
            container = Some(
                deepest_container_v2(nodes, best, &point_in_box, &point_node_distance)
                    .unwrap_or(best),
            );
            break;
        }
    }

    let (nds_l, nds_r) = if let Some(c_idx) = container {
        get_closest_children_in_box_v2(
            nodes,
            c_idx,
            &h_ordered_distance,
            &point_in_box,
            &point_node_distance,
            &data_tag,
            &data_line,
            &data_col,
        )
    } else if let Some(&root_child) = nodes[sheet.root].children.last() {
        let (cn, cd) = closest_deep_child_v2(nodes, root_child, &point_node_distance);
        (cn.map(|n| (n, cd)), None)
    } else {
        return Ok(Vec::new());
    };

    let selected_nodes: Vec<usize> = match (nds_l, nds_r) {
        (Some((mut l_idx, l_dist)), Some((mut r_idx, r_dist))) => {
            if (data_tag(r_idx), data_line(r_idx), data_col(r_idx))
                != (data_tag(l_idx), data_line(l_idx), data_col(l_idx))
            {
                if data_line(r_idx) < data_line(l_idx)
                    || (data_line(r_idx) == data_line(l_idx) && l_dist > r_dist)
                {
                    std::mem::swap(&mut l_idx, &mut r_idx);
                }
                vec![l_idx, r_idx]
            } else {
                if l_dist > r_dist {
                    l_idx = r_idx;
                }
                vec![l_idx]
            }
        }
        (_, Some((r_idx, _))) => vec![r_idx],
        (Some((l_idx, _)), None) => vec![l_idx],
        (None, None) => container.into_iter().collect(),
    };

    let graphics_boxes = sheet_graphics_boxes(
        nodes,
        &parsed.inputs,
        parsed.fix_cross_file_tags,
        sheet.root,
    );
    let point_in_data_box = |idx: usize| -> bool {
        let [min_h, max_h, min_v, max_v] = nodes[idx].data_box();
        min_h <= hit_h && hit_h <= max_h && min_v <= hit_v && hit_v <= max_v
    };
    let hit_graphics_total = graphics_boxes
        .iter()
        .filter(|g| point_in_data_box(g.box_idx))
        .count();

    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();
    for mut idx in selected_nodes {
        if parsed.fix_cross_file_tags
            && tag_is_generated_graphics_tex(&parsed.inputs, nodes[idx].tag)
        {
            let mut cur = idx;
            loop {
                if let Some(g) = graphics_boxes.iter().find(|g| {
                    g.gen_root.is_some_and(|gr| {
                        gr == cur
                            || (nodes[cur].kind == b'b'
                                && nodes[cur].parent == Some(g.host_idx)
                                && nodes[cur].tag == nodes[gr].tag)
                    })
                }) {
                    idx = g.host_idx;
                    break;
                }
                let Some(p_idx) = nodes[cur].parent else {
                    break;
                };
                cur = p_idx;
            }
        }
        let tag = nodes[idx].tag;
        let Some((_, raw_path)) = parsed.inputs.iter().rev().find(|(t, _)| *t == tag) else {
            continue;
        };
        let file = raw_path.trim();
        if file.is_empty() {
            continue;
        };
        let Ok(line) = u32::try_from(nodes[idx].line) else {
            continue;
        };
        if line == 0 {
            continue;
        }
        let compiler_column = usize::try_from(nodes[idx].column)
            .ok()
            .filter(|&col| col > 1);
        let mut same_anchor_count = 0usize;
        let mut hit_index = None;
        let mut hit_same_anchor = 0usize;
        for g in &graphics_boxes {
            if nodes[g.host_idx].tag == tag && nodes[g.host_idx].line == nodes[idx].line {
                if point_in_data_box(g.box_idx) {
                    hit_index = Some(same_anchor_count);
                    hit_same_anchor += 1;
                }
                same_anchor_count += 1;
            }
        }
        let graphics = if hit_same_anchor == 1 && hit_graphics_total == 1 {
            hit_index.map(|index| GraphicsHit::Unique {
                index,
                count: same_anchor_count,
            })
        } else if hit_same_anchor > 0 {
            Some(GraphicsHit::Ambiguous)
        } else {
            None
        };
        if seen.insert((file, line, compiler_column)) {
            results.push(RawInverseCandidate {
                file: file.to_owned(),
                line,
                compiler_column,
                graphics,
            });
        }
    }
    Ok(results)
}
