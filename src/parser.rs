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
        }
    }

    fn link(&self) -> (u32, i64, i64) {
        (self.tag, self.line, self.column)
    }

    fn set_link(&mut self, (tag, line, column): (u32, i64, i64)) {
        (self.tag, self.line, self.column) = (tag, line, column);
    }

    fn contain_point(&mut self, ph: i64, pv: i64) {
        self.contain_box([ph, ph, pv, pv]);
    }

    /// Grows a horizontal box's visible extent to cover `bounds`. Each new edge is one of the
    /// given bounds and each untouched edge keeps its value, so the visible edges stay within
    /// the coordinate bound of the box and its children (see [`MAX_COORD`]).
    fn contain_box(&mut self, bounds: [i64; 4]) {
        if self.kind != b'(' {
            return;
        }
        let [min_h, max_h, min_v, max_v] = bounds;
        if self.width_v < 0 {
            let mx = self.h_v;
            let mn = mx + self.width_v;
            if min_h < mn {
                self.width_v = min_h - mx;
            } else if max_h > mx {
                self.h_v = max_h;
                self.width_v = mn - max_h;
            }
        } else {
            let mn = self.h_v;
            let mx = mn + self.width_v;
            if min_h < mn {
                self.h_v = min_h;
                self.width_v = mx - min_h;
            } else if max_h > mx {
                self.width_v = max_h - mn;
            }
        }
        let mn = self.v_v - self.height_v;
        let mx = self.v_v + self.depth_v;
        if min_v < mn {
            self.height_v = self.v_v - min_v;
        } else if max_v > mx {
            self.depth_v = max_v - self.v_v;
        }
    }

    fn data_box(&self) -> [i64; 4] {
        let (min_h, max_h) = if self.width < 0 {
            (self.h + self.width, self.h)
        } else {
            (self.h, self.h + self.width)
        };
        [min_h, max_h, self.v - self.height, self.v + self.depth]
    }

    fn data_xob(&self) -> [i64; 4] {
        let (min_h, max_h) = if self.width > 0 {
            (self.h - self.width, self.h)
        } else {
            (self.h, self.h - self.width)
        };
        [min_h, max_h, self.v - self.height, self.v + self.depth]
    }

    fn data_box_v(&self) -> [i64; 4] {
        let (min_h, max_h) = if self.width_v < 0 {
            (self.h_v + self.width_v, self.h_v)
        } else {
            (self.h_v, self.h_v + self.width_v)
        };
        [
            min_h,
            max_h,
            self.v_v - self.height_v,
            self.v_v + self.depth_v,
        ]
    }

    /// Whether every anchor and box edge of this node lies within [`MAX_COORD`].
    fn within_coord_bound(&self) -> bool {
        let anchors = [self.h, self.v, self.h_v, self.v_v];
        anchors
            .into_iter()
            .chain(self.data_box())
            .chain(self.data_xob())
            .chain(self.data_box_v())
            .all(|value| value.abs() <= MAX_COORD)
    }
}

/// Coordinate invariant, established at parse time and kept by every transform:
///
/// * raw `width`, `height`, and `depth` and parsed positions fit in `i32` (TeX scaled points;
///   [`scaled_geometry`] rejects anything else);
/// * every node anchor (`h`, `v`, `h_v`, `v_v`) and every edge of `data_box`, `data_xob`, and
///   `data_box_v` has magnitude at most `MAX_COORD`. Parsed nodes satisfy this trivially
///   (`|i32| + |i32| < 2^33`); [`ForwardNode::contain_box`] only moves an edge onto another
///   in-bound edge; form proxies add an `i32` offset (and at most `2 * i32` vertically) to an
///   in-bound node and are rejected unless the result is still in bound.
///
/// Hence the visible sizes (`width_v`, `height_v`, `depth_v`) are differences of two in-bound
/// values (at most `2^41`), and every distance between an edge and an `i32` query point stays
/// far below `i64::MAX`, so geometry arithmetic needs no saturation.
const MAX_COORD: i64 = 1 << 40;

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Validates parsed SyncTeX geometry against the TeX scaled-point (`i32`) range.
fn scaled_geometry(values: &[i64]) -> io::Result<()> {
    if values.iter().all(|&value| i32::try_from(value).is_ok()) {
        Ok(())
    } else {
        Err(invalid_input("unsupported SyncTeX geometry value"))
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

/// The `friends` bucket of a `(tag, line)` link, as in SyncTeX's hash of input lines.
fn friend_bucket(tag: u32, line: i64) -> usize {
    (i64::from(tag).saturating_add(line).unsigned_abs() as usize) % 1024
}

const MAX_PROXY_DEPTH: usize = MAX_BOX_DEPTH;
pub(crate) const MAX_BOX_DEPTH: usize = 512;
const MAX_FORWARD_NODES: usize = 4_000_000;
pub(crate) const MAX_SYNCTEX_BYTES: usize = 128 * 1024 * 1024;

#[allow(clippy::too_many_arguments)]
fn clone_synctex_proxy_subtree(
    nodes: &mut Vec<ForwardNode>,
    source: usize,
    parent: Option<usize>,
    page: u32,
    dh: i64,
    dv: i64,
    depth: usize,
    register_friends: &mut Option<&mut [Vec<usize>]>,
    sheet_hboxes: &mut Option<&mut Vec<usize>>,
) -> io::Result<usize> {
    if depth > MAX_PROXY_DEPTH {
        return Err(invalid_data("SyncTeX form proxy depth exceeds limit"));
    }
    if nodes.len() >= MAX_FORWARD_NODES {
        return Err(io::Error::other("SyncTeX node count exceeds limit"));
    }
    if nodes[source].kind == b'f' {
        return Err(invalid_data(
            "SyncTeX form proxy references unresolved form",
        ));
    }
    let mut clone = nodes[source].clone();
    let children = std::mem::take(&mut clone.children);
    clone.page = page;
    clone.parent = parent;
    clone.target = nodes[source].target.or(Some(source));
    // `dh` and `dv` are bounded by `i32` and `2 * i32`, and the source node is in bound, so
    // the additions cannot overflow; the result must stay in bound for later geometry.
    clone.h += dh;
    clone.v += dv;
    clone.h_v += dh;
    clone.v_v += dv;
    if !clone.within_coord_bound() {
        return Err(invalid_data(
            "SyncTeX form transform exceeds coordinate range",
        ));
    }
    let index = nodes.len();
    nodes.push(clone);
    if let Some(friends) = register_friends.as_deref_mut()
        && matches!(
            nodes[index].kind,
            b'[' | b'(' | b'v' | b'h' | b'b' | b'k' | b'g' | b'r' | b'$' | b'x'
        )
    {
        friends[friend_bucket(nodes[index].tag, nodes[index].line)].push(index);
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

pub(crate) fn read_remaining_rows(
    reader: &mut impl BufRead,
    already_read: usize,
    operation: Option<&Operation>,
) -> io::Result<Vec<u8>> {
    let mut rows = Vec::new();
    loop {
        if let Some(op) = operation {
            op.check()?;
        }
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if chunk.is_empty() {
            return Ok(rows);
        }
        rows.extend_from_slice(chunk);
        if already_read.saturating_add(rows.len()) >= MAX_SYNCTEX_BYTES {
            return Err(io::Error::other("SyncTeX source map exceeds 128 MiB"));
        }
        let consumed = chunk.len();
        reader.consume(consumed);
    }
}

/// The form tags the row loop will register when it reads `rows`. It skips the whole body of a
/// duplicate or malformed definition, so nothing nested inside one counts.
fn forms_defined_in(
    rows: &[u8],
    registered: &std::collections::HashMap<u32, usize>,
    operation: Option<&Operation>,
) -> io::Result<std::collections::HashSet<u32>> {
    let mut defined = std::collections::HashSet::new();
    let mut ignored_depth = 0usize;
    for (index, row) in rows.split(|&byte| byte == b'\n').enumerate() {
        if index & 1023 == 0
            && let Some(op) = operation
        {
            op.check()?;
        }
        match row.first() {
            Some(b'<') if ignored_depth > 0 => ignored_depth += 1,
            Some(b'>') if ignored_depth > 0 => ignored_depth -= 1,
            Some(b'<') => {
                let tag = std::str::from_utf8(&row[1..])
                    .ok()
                    .and_then(parse_synctex_int_prefix)
                    .and_then(|(tag, _)| u32::try_from(tag).ok())
                    .filter(|tag| !registered.contains_key(tag));
                match tag {
                    Some(tag) if defined.insert(tag) => {}
                    _ => ignored_depth = 1,
                }
            }
            _ => {}
        }
    }
    Ok(defined)
}

#[derive(Default)]
struct SheetEntry {
    page: u32,
    root: usize,
    hboxes: Vec<usize>,
    graphics_boxes: std::cell::OnceCell<Vec<GraphicsBox>>,
}

#[derive(Default)]
pub(crate) struct ParsedSynctex {
    inputs: Vec<(u32, String)>,
    max_lines: std::collections::HashMap<u32, i64>,
    nodes: Vec<ForwardNode>,
    /// SyncTeX's 1,024 input-line hash buckets; empty unless the parse tracks friends.
    friends: Vec<Vec<usize>>,
    sheets: std::collections::BTreeMap<u32, usize>,
    sheet_list: Vec<SheetEntry>,
    unit: f64,
    x_off: f64,
    y_off: f64,
    bound: i64,
    fix_cross_file_tags: bool,
    /// Source-map provenance of `target_page`, recorded when the parse targets a single sheet.
    source_provenance: Option<Box<SourceProvenance>>,
}

/// A source-map query against a sidecar. Path resolution, the generated input's identity, and
/// which validation checks apply all depend on the request, so [`SourceProvenance`] records the
/// raw, request-independent facts of the parse and evaluates them per request.
#[derive(Clone, Copy)]
pub(crate) struct SourceMapRequest<'a> {
    pub(crate) pdf: &'a Path,
    pub(crate) point: InversePoint,
    pub(crate) generated: &'a Path,
    /// Reject sheet form transforms, require the postamble, and locate generated boxes at the
    /// point under the parser's effective transform.
    pub(crate) geometry_required: bool,
    /// Strict input resolution relative to this directory; also requires the postamble.
    pub(crate) input_cwd: Option<&'a Path>,
    pub(crate) sidecar_dir: Option<&'a Path>,
    pub(crate) operation: &'a Operation,
}

impl ParsedSynctex {
    /// The source map of the parsed sheet. The parse must have targeted `request.point.page`.
    pub(crate) fn source_map(&self, request: SourceMapRequest<'_>) -> io::Result<SourceMap> {
        let unparsed = || invalid_input("SyncTeX source map requires a parse of the clicked sheet");
        let provenance = self.source_provenance.as_deref().ok_or_else(unparsed)?;
        if provenance.page != request.point.page {
            return Err(unparsed());
        }
        provenance.source_map(request, (self.unit, self.x_off, self.y_off))
    }
}

/// An `Input:` row, unresolved, with its position relative to the clicked sheet.
struct RawInput {
    row: usize,
    /// `None` for a malformed declaration.
    decl: Option<(u32, String)>,
    /// Declared after the clicked sheet closed: a duplicate tag no longer replaces the path.
    after_page: bool,
}

/// A row of the clicked sheet with a raw `(tag, line)` link, before any parser retagging.
struct PageLink {
    row: usize,
    tag: u32,
    /// A content record (`[ ( v h k g x $ r`), which can make the page generated.
    counts: bool,
}

/// The raw provenance a source map needs, recorded by the document parser in sidecar order:
///
/// * raw `(tag, line)` links of the clicked sheet's rows, before `fix_cross_file_tags` and the
///   parser's boundary/kern retagging, and its horizontal boxes;
/// * the origin `(page, (tag, line))` of every sheet, including sheets a single-page parse skips;
/// * every `Input:` declaration with its row, so a declaration after a node or after the sheet
///   does not retroactively claim it, and duplicates keep before/after-sheet semantics.
#[derive(Default)]
pub(crate) struct SourceProvenance {
    page: u32,
    in_page: bool,
    page_complete: bool,
    active_sheet: Option<(u32, Option<(u32, u32)>)>,
    origin: Option<(u32, u32)>,
    sheet_origins: Vec<(u32, (u32, u32))>,
    inputs: Vec<RawInput>,
    links: Vec<PageLink>,
    /// `links.len()` before the current row's link.
    row_links: usize,
    stack: Vec<(Option<[f64; 4]>, usize)>,
    /// Closed horizontal boxes of the clicked sheet with positive extent, in raw SyncTeX units,
    /// with the links of their rows: a box is generated when any of them names a generated
    /// input declared before that row.
    boxes: Vec<([f64; 4], std::ops::Range<usize>)>,
    postamble_count: bool,
    /// Errors that apply to every request.
    structure_error: Option<&'static str>,
    /// Errors that apply when `geometry_required`.
    geometry_error: Option<&'static str>,
}

#[derive(Default)]
struct ResolvedInputs {
    paths: std::collections::HashMap<u32, PathBuf>,
    order: Vec<u32>,
    first_generated_row: std::collections::HashMap<u32, usize>,
}

fn raw_node_link(row: &str) -> Option<(u32, u32)> {
    let (link, _) = row.get(1..)?.split_once(':')?;
    let (tag, rest) = link.split_once(',')?;
    Some((
        tag.parse::<u32>().ok()?,
        rest.split(',').next()?.parse::<u32>().ok()?,
    ))
}

/// Whether the innermost boxes containing the point are all generated. Conflicting non-nested
/// overlaps and equal boxes with mixed ownership abstain.
pub(crate) fn generated_box_at_point(
    boxes: impl Iterator<Item = ([f64; 4], bool)>,
    x: f64,
    y: f64,
    operation: &Operation,
) -> io::Result<bool> {
    let contains =
        |a: [f64; 4], b: [f64; 4]| a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3];
    let mut nearest: Vec<([f64; 4], bool)> = Vec::new();
    for (bounds, generated) in boxes {
        operation.check()?;
        if !contains(bounds, [x, y, x, y]) {
            continue;
        }
        if let Some(equal) = nearest.iter_mut().find(|old| old.0 == bounds) {
            equal.1 &= generated;
        } else if !nearest.iter().any(|old| contains(bounds, old.0)) {
            nearest.retain(|old| !contains(old.0, bounds));
            nearest.push((bounds, generated));
        }
    }
    Ok(nearest.len() == 1 && nearest[0].1)
}

impl SourceProvenance {
    fn new(page: u32) -> Self {
        Self {
            page,
            ..Self::default()
        }
    }

    /// Records the raw provenance of one sidecar row (without its line terminator).
    fn observe(&mut self, row: &str, row_index: usize) {
        self.row_links = self.links.len();
        if self.structure_error.is_some() {
            return;
        }
        let first = row.as_bytes().first().copied();
        if let Some(input) = row.strip_prefix("Input:") {
            let decl = input
                .split_once(':')
                .and_then(|(tag, path)| Some((tag.parse::<u32>().ok()?, path.to_owned())));
            self.inputs.push(RawInput {
                row: row_index,
                decl,
                after_page: self.page_complete,
            });
        } else if let Some(page) = row.strip_prefix('{') {
            let parsed = page.trim().parse::<u32>().ok();
            self.in_page = parsed == Some(self.page);
            if self.in_page && self.page_complete {
                self.structure_error = Some("SyncTeX source map repeats the clicked sheet");
            }
            self.active_sheet = parsed.map(|page| (page, None));
        } else if let Some(close) = row.strip_prefix('}') {
            if let Some((page, Some(origin))) = self.active_sheet.take()
                && close.trim().parse::<u32>() == Ok(page)
            {
                self.sheet_origins.push((page, origin));
            }
            self.page_complete |= std::mem::take(&mut self.in_page);
        } else if self.in_page {
            if matches!(first, Some(b'<' | b'>')) && self.geometry_error.is_none() {
                self.geometry_error =
                    Some("SyncTeX fragile body ownership cannot resolve sheet form transforms");
            }
            let link = raw_node_link(row);
            // Only the first box enclosing the whole sheet proves provenance.
            if self.origin.is_none() {
                if !matches!(first, Some(b'[' | b'(')) || link.is_none() {
                    self.structure_error = Some("SyncTeX sheet has no enclosing source box");
                    return;
                }
                self.origin = link;
                if let Some((_, sheet_origin)) = &mut self.active_sheet {
                    *sheet_origin = link;
                }
            }
            if let Some((tag, _)) = link
                && !matches!(first, Some(b']' | b')'))
            {
                self.links.push(PageLink {
                    row: row_index,
                    tag,
                    counts: matches!(
                        first,
                        Some(b'[' | b'(' | b'v' | b'h' | b'k' | b'g' | b'x' | b'$' | b'r')
                    ),
                });
            }
        } else if let Some((_, sheet_origin @ None)) = &mut self.active_sheet
            && matches!(first, Some(b'[' | b'('))
        {
            *sheet_origin = raw_node_link(row);
        }
    }

    /// A box of the clicked sheet opened on the current row; `whd` is a horizontal box's size.
    fn open_box(&mut self, [x, y]: [i64; 2], whd: Option<(i64, i64, i64)>) {
        let bounds = whd
            .filter(|&(w, h, d)| w > 0 && h.saturating_add(d) > 0)
            .map(|(w, h, d)| [x, y - h, x + w, y + d].map(|v| v as f64));
        self.stack.push((bounds, self.row_links));
    }

    fn close_box(&mut self) {
        if let Some((Some(bounds), start)) = self.stack.pop() {
            self.boxes.push((bounds, start..self.links.len()));
        }
    }

    fn resolve_inputs(&self, request: SourceMapRequest<'_>) -> io::Result<ResolvedInputs> {
        let strict_cwd = request.input_cwd;
        let current_dir = (std::env::current_dir().ok())
            .filter(|_| strict_cwd.is_none())
            .map(|dir| fs::canonicalize(&dir).unwrap_or(dir));
        let pdf_dir = request.pdf.parent();
        let mut resolved = ResolvedInputs::default();
        for (index, input) in self.inputs.iter().enumerate() {
            if index & 1023 == 0 {
                request.operation.check()?;
            }
            let fail = |message: &str| Err(invalid_data(message));
            let Some((tag, raw)) = &input.decl else {
                if strict_cwd.is_some() {
                    return fail("malformed SyncTeX input declaration");
                }
                continue;
            };
            let tag = *tag;
            let resolved_path = crate::resolve_declared_input(
                raw,
                strict_cwd.ok_or(current_dir.as_deref()),
                pdf_dir,
                request.sidecar_dir,
            );
            if strict_cwd.is_some() && tag == 0 {
                return fail("invalid SyncTeX input declaration");
            }
            let path = resolved_path.map_err(invalid_data)?;
            // TeX can emit .vrb in the build cwd while the PDF is in a separate output
            // directory. Jobname is the map identity; the generated file is neither read nor
            // trusted.
            let is_generated = path.file_name() == request.generated.file_name();
            match resolved.paths.entry(tag) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if strict_cwd.is_some() && entry.get() != &path {
                        return fail("conflicting SyncTeX input tag");
                    }
                    if strict_cwd.is_none() && !input.after_page {
                        entry.insert(path);
                        if !is_generated {
                            resolved.first_generated_row.remove(&tag);
                        }
                    } else if strict_cwd.is_none() {
                        continue;
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    resolved.order.push(tag);
                    entry.insert(path);
                }
            }
            if is_generated {
                resolved.first_generated_row.entry(tag).or_insert(input.row);
            }
        }
        Ok(resolved)
    }

    /// Evaluates the recorded provenance for `request` under the sidecar's effective
    /// `(unit, x_offset, y_offset)` transform.
    fn source_map(
        &self,
        request: SourceMapRequest<'_>,
        (unit, x_offset, y_offset): (f64, f64, f64),
    ) -> io::Result<SourceMap> {
        let operation = request.operation;
        operation.check()?;
        let geometry = request.geometry_required;
        let strict_inputs = request.input_cwd.is_some();
        if let Some(error) = self
            .structure_error
            .or(self.geometry_error.filter(|_| geometry))
        {
            return Err(io::Error::other(error));
        }
        let ResolvedInputs {
            paths,
            order,
            first_generated_row,
        } = self.resolve_inputs(request)?;
        if !self.page_complete || ((geometry || strict_inputs) && !self.postamble_count) {
            return Err(io::Error::other("SyncTeX source map is incomplete"));
        }
        // A declaration only claims rows after it.
        let generated_before = |link: &PageLink| {
            first_generated_row
                .get(&link.tag)
                .is_some_and(|&row| row < link.row)
        };
        let mut generated_on_page = self
            .links
            .iter()
            .any(|link| link.counts && generated_before(link));
        if strict_inputs {
            generated_on_page |= self
                .links
                .iter()
                .any(|link| link.counts && first_generated_row.contains_key(&link.tag));
        }
        let page = self.page;
        let is_vrb = request
            .generated
            .extension()
            .is_some_and(|ext| ext == "vrb");
        let is_sidecar_tag = |tag: u32| {
            first_generated_row.contains_key(&tag)
                || paths
                    .get(&tag)
                    .is_some_and(|path| crate::is_generated_or_sidecar_input(path))
        };
        // The nearest host sheets around a sidecar sheet; a following host sheet of the same
        // file as the preceding one comes first.
        let mut adjacent_tags = Vec::new();
        if let Some((tag, _)) = self.origin
            && is_sidecar_tag(tag)
            && !is_vrb
        {
            let hosts = || (self.sheet_origins.iter()).filter(|(_, (t, _))| !is_sidecar_tag(*t));
            let prev = hosts().rfind(|(p, _)| *p < page).map(|&(_, o)| o);
            let next = hosts().find(|(p, _)| *p > page).map(|&(_, o)| o);
            adjacent_tags.extend(prev.into_iter().chain(next));
            if matches!((prev, next), (Some(p), Some(n)) if n.0 == p.0) {
                adjacent_tags.reverse();
            }
        }
        let slides = |origin| {
            (self.sheet_origins.iter())
                .filter(|&&(p, o)| p <= page && o == origin)
                .count()
        };
        let frame_slide = (self.origin)
            .and_then(|origin| u32::try_from(slides(origin)).ok())
            .filter(|&n| n > 0);
        let generated_at_point = if geometry {
            let mut prefix = Vec::with_capacity(self.links.len() + 1);
            prefix.push(0usize);
            for link in &self.links {
                prefix.push(prefix[prefix.len() - 1] + usize::from(generated_before(link)));
            }
            generated_box_at_point(
                (self.boxes.iter())
                    .map(|(bounds, rows)| (*bounds, prefix[rows.end] > prefix[rows.start])),
                (f64::from(request.point.x) - x_offset) / unit,
                (f64::from(request.point.y_from_top) - y_offset) / unit,
                operation,
            )?
        } else {
            false
        };
        let main = paths
            .get(&1)
            .cloned()
            .ok_or_else(|| io::Error::other("SyncTeX source map has no main input"))?;
        let located = |(tag, line): (u32, u32)| Some((paths.get(&tag)?.clone(), line));
        let page_source = self.origin.filter(|_| generated_on_page).and_then(located);
        let adjacent_origins = (adjacent_tags.into_iter())
            .filter(|_| generated_on_page)
            .filter_map(located)
            .collect();
        Ok(SourceMap {
            main,
            inputs: order
                .iter()
                .filter_map(|tag| paths.get(tag).cloned())
                .collect(),
            page_source,
            generated_at_point,
            frame_slide,
            generated: request.generated.to_path_buf(),
            adjacent_origins,
        })
    }
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
    let mut provenance = target_page.map(SourceProvenance::new);
    let mut inputs: Vec<(u32, String)> = Vec::new();
    let mut max_lines = std::collections::HashMap::<u32, i64>::new();
    let mut pre_magnification = 1000i64;
    let mut pre_unit = 8192i64;
    let mut pre_offset = [578i64; 2];
    let mut post_magnification = None::<f64>;
    let mut post_offset = [None::<f64>; 2];
    let mut in_post_scriptum = false;

    let mut nodes = Vec::<ForwardNode>::new();
    let mut friends = if track_friends {
        vec![Vec::new(); 1024]
    } else {
        Vec::new()
    };
    let mut sheets = std::collections::BTreeMap::<u32, usize>::new();
    let mut sheet_list = Vec::<SheetEntry>::new();
    let mut forms = std::collections::HashMap::<u32, usize>::new();
    // `(ref, parent, page, sheet, form tag, h, v)`; references inside forms resolve first.
    let mut ref_in_form = Vec::<(usize, usize, u32, Option<usize>, u32, i64, i64)>::new();
    let mut ref_in_sheet = Vec::new();

    let mut current_page = None::<u32>;
    let mut current_sheet_idx = None::<usize>;
    let mut form_stack = Vec::<usize>::new();
    let mut ignored_form_depth = 0usize;
    let mut parent = None::<usize>;
    let mut x_stack = Vec::<Vec<usize>>::new();
    let mut last_k = None::<usize>;
    let mut last_g = None::<usize>;
    let mut last_v = -1i64;

    let make_friend = |friends: &mut [Vec<usize>], nodes: &[ForwardNode], idx: usize| {
        if track_friends {
            friends[friend_bucket(nodes[idx].tag, nodes[idx].line)].push(idx);
        }
    };
    let record_max_line =
        |max_lines: &mut std::collections::HashMap<u32, i64>, tag: u32, line: i64| {
            if track_friends {
                let max = max_lines.entry(tag).or_insert(line);
                *max = (*max).max(line);
            }
        };
    // Pending `x` records of the innermost box take the link of `source`, if any.
    let flush_x = |x_stack: &mut [Vec<usize>],
                   friends: &mut [Vec<usize>],
                   nodes: &mut [ForwardNode],
                   in_form: bool,
                   source: Option<usize>| {
        let Some(pending) = x_stack.last_mut() else {
            return;
        };
        for x_idx in pending.drain(..).rev() {
            if let Some(src) = source {
                nodes[x_idx].set_link(nodes[src].link());
            }
            if !in_form {
                make_friend(friends, nodes, x_idx);
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
        let (x, y) = parse_position(pos_str, last_v)?;
        Some((tag, line, col, x, y, rem))
    }
    fn parse_position(position: &str, last_v: &mut i64) -> Option<(i64, i64)> {
        let (x_str, y_str) = position.split_once(',')?;
        let x = x_str.parse::<i64>().ok()?;
        if !y_str.starts_with('=') {
            *last_v = parse_synctex_int_prefix(y_str)?.0;
        }
        Some((x, *last_v))
    }
    fn parse_form_ref(body: &str, last_v: &mut i64) -> Option<(u32, i64, i64)> {
        let (tag, rest) = body.split_once(':')?;
        let tag = tag.parse::<u32>().ok()?;
        let (x, y) = parse_position(rest, last_v)?;
        Some((tag, x, y))
    }
    let parse_whd = |rem: &str| -> Option<(i64, i64, i64)> {
        let mut parts = rem.splitn(3, ',');
        let w = parts.next()?.parse::<i64>().ok()?;
        let h = parts.next()?.parse::<i64>().ok()?;
        let (d, _) = parse_synctex_int_prefix(parts.next()?)?;
        Some((w, h, d))
    };
    let parse_post_dim = |val: &str| {
        let val = val.trim();
        let (number, unit) = val.split_at_checked(val.len().checked_sub(2)?)?;
        let number = number.trim().parse::<f64>().ok()?;
        number
            .is_finite()
            .then_some(number * synctex_unit_scale(unit)?)
    };

    let mut row = String::new();
    let mut later_forms = None::<std::collections::HashSet<u32>>;
    let mut buffered_rows = None::<io::Cursor<Vec<u8>>>;
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
        let count = match &mut buffered_rows {
            Some(rows) => rows.read_line(&mut row),
            None => reader.read_line(&mut row),
        };
        let count = count?;
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
        if let Some(provenance) = provenance.as_mut() {
            provenance.observe(trimmed, line_count);
        }
        if trimmed.starts_with("SyncTeX Version:") {
            saw_preamble = true;
        } else if trimmed == "Postamble:" {
            if current_page.is_some() || !form_stack.is_empty() || parent.is_some() {
                return Err(invalid_data("unclosed SyncTeX container before postamble"));
            }
            saw_postamble = true;
        } else if saw_postamble && let Some(count_str) = trimmed.strip_prefix("Count:") {
            if count_str.trim().parse::<u32>().is_err() {
                return Err(invalid_input("invalid SyncTeX Count"));
            }
            saw_count = true;
        } else if let Some(input) = trimmed.strip_prefix("Input:")
            && let Some((tag_str, path)) = input.split_once(':')
            && let Ok(tag) = tag_str.parse::<u32>()
        {
            inputs.push((tag, path.trim_end_matches(' ').to_string()));
        } else if let Some(val) = trimmed.strip_prefix("Magnification:") {
            if in_post_scriptum {
                let mag = val.trim().parse::<f64>().ok();
                post_magnification = Some(
                    mag.filter(|&m| m.is_finite() && m > 0.0)
                        .ok_or_else(|| invalid_input("invalid postscript Magnification"))?,
                );
            } else {
                let mag = val.trim().parse::<i64>().ok().filter(|&m| m > 0);
                pre_magnification =
                    mag.ok_or_else(|| invalid_input("invalid SyncTeX Magnification"))?;
            }
        } else if let Some(val) = trimmed.strip_prefix("Unit:") {
            let unit = val.trim().parse::<i64>().ok().filter(|&u| u > 0);
            pre_unit = unit.ok_or_else(|| invalid_input("invalid SyncTeX Unit"))?;
        } else if let Some((axis, val)) = trimmed
            .strip_prefix("X Offset:")
            .map(|val| (0, val))
            .or_else(|| trimmed.strip_prefix("Y Offset:").map(|val| (1, val)))
        {
            if in_post_scriptum {
                let message = ["invalid postscript X Offset", "invalid postscript Y Offset"];
                let offset = parse_post_dim(val).ok_or_else(|| invalid_input(message[axis]))?;
                post_offset[axis] = Some(offset);
            } else {
                let message = ["invalid SyncTeX X Offset", "invalid SyncTeX Y Offset"];
                pre_offset[axis] = val
                    .trim()
                    .parse()
                    .map_err(|_| invalid_input(message[axis]))?;
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
            current_page = Some(page);
            if target_page.is_some_and(|want| want != page) {
                current_sheet_idx = None;
                parent = None;
            } else {
                if nodes.len() >= MAX_FORWARD_NODES {
                    return Err(io::Error::other("SyncTeX node count exceeds limit"));
                }
                let root = nodes.len();
                nodes.push(ForwardNode::new(b'{', page, None, 0, 0, 0, [0; 5]));
                sheets.insert(page, root);
                sheet_list.push(SheetEntry {
                    page,
                    root,
                    ..SheetEntry::default()
                });
                current_sheet_idx = Some(sheet_list.len() - 1);
                parent = Some(root);
            }
            x_stack.clear();
            (last_k, last_g) = (None, None);
        } else if let Some(page_str) = trimmed.strip_prefix('}')
            && form_stack.is_empty()
            && current_page.is_some()
        {
            if let Some((close_page, _)) = parse_synctex_int_prefix(page_str)
                && u32::try_from(close_page).ok() != current_page
            {
                return Err(invalid_data("mismatched SyncTeX sheet close tag"));
            }
            if let Some(p_idx) = parent
                && nodes[p_idx].kind != b'{'
            {
                return Err(invalid_data("unclosed SyncTeX box at sheet end"));
            }
            current_page = None;
            current_sheet_idx = None;
            parent = None;
            x_stack.clear();
            (last_k, last_g) = (None, None);
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
                (last_k, last_g) = (None, None);
            } else {
                ignored_form_depth = 1;
            }
        } else if trimmed.starts_with('>') && !form_stack.is_empty() {
            let form_root = form_stack.pop().unwrap();
            if parent != Some(form_root) {
                return Err(invalid_data("unclosed SyncTeX box at form end"));
            }
            parent = nodes[form_root]
                .parent
                .or_else(|| current_sheet_idx.map(|s_idx| sheet_list[s_idx].root));
            (last_k, last_g) = (None, None);
        } else if (current_page.is_some() || !form_stack.is_empty())
            && let Some(&kind) = trimmed.as_bytes().first()
        {
            let in_form = !form_stack.is_empty();
            if !in_form && current_sheet_idx.is_none() {
                match kind {
                    b'[' | b'(' | b'v' | b'h' | b'r' | b'k' | b'g' | b'$' | b'x' => {
                        let _ = parse_link_pos(&trimmed[1..], &mut last_v);
                    }
                    b'f' => _ = parse_form_ref(&trimmed[1..], &mut last_v),
                    _ => {}
                }
                continue;
            }
            if nodes.len() + 2 >= MAX_FORWARD_NODES {
                return Err(io::Error::other("SyncTeX node count exceeds limit"));
            }
            let page = current_page.filter(|_| !in_form).unwrap_or(0);
            let mut sheet_provenance = provenance
                .as_mut()
                .filter(|_| !in_form && current_sheet_idx.is_some());
            match kind {
                b'[' | b'(' => {
                    if x_stack.len() >= MAX_BOX_DEPTH {
                        return Err(io::Error::other("SyncTeX box nesting depth exceeds limit"));
                    }
                    if let Some((tag, line, col, x, y, rem)) =
                        parse_link_pos(&trimmed[1..], &mut last_v)
                        && let Some((w, h, d)) = parse_whd(rem)
                    {
                        let coords = [x, y, w, h, d];
                        scaled_geometry(&coords)?;
                        let idx = nodes.len();
                        nodes.push(ForwardNode::new(kind, page, parent, tag, line, col, coords));
                        if let Some(p_idx) = parent {
                            nodes[p_idx].children.push(idx);
                        }
                        if let Some(provenance) = sheet_provenance.as_mut() {
                            provenance.open_box([x, y], (kind == b'(').then_some((w, h, d)));
                        }
                        parent = Some(idx);
                        x_stack.push(Vec::new());
                        if kind == b'(' {
                            let bdry = nodes.len();
                            let at = [x, y, 0, 0, 0];
                            nodes.push(ForwardNode::new(b'b', page, parent, tag, line, col, at));
                            nodes[idx].children.push(bdry);
                            if !in_form {
                                make_friend(&mut friends, &nodes, bdry);
                            }
                        }
                        record_max_line(&mut max_lines, tag, line);
                        (last_k, last_g) = (None, None);
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
                        flush_x(&mut x_stack, &mut friends, &mut nodes, in_form, None);
                        x_stack.pop();
                        flush_x(&mut x_stack, &mut friends, &mut nodes, in_form, Some(p_idx));
                        if let Some(provenance) = sheet_provenance.as_mut() {
                            provenance.close_box();
                        }
                        parent = next_parent;
                        (last_k, last_g) = (None, None);
                    } else {
                        return Err(invalid_data("mismatched SyncTeX vbox close"));
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
                        let (tag, line, col) = nodes[last_c].link();
                        let end = [nodes[p_idx].h_v + nodes[p_idx].width_v, nodes[p_idx].v_v];
                        let bdry2 = nodes.len();
                        let coords = [end[0], end[1], 0, 0, 0];
                        nodes.push(ForwardNode::new(b'b', page, parent, tag, line, col, coords));
                        nodes[p_idx].children.push(bdry2);
                        let first_c = nodes[p_idx].children[0];
                        let (start_h, start_v) = (nodes[p_idx].h_v, nodes[p_idx].v_v);
                        nodes[first_c].h = start_h;
                        nodes[first_c].v = start_v;
                        if let (Some(k_idx), Some(g_idx)) = (last_k, last_g) {
                            for i in 0..nodes[p_idx].children.len().saturating_sub(1) {
                                if nodes[p_idx].children[i + 1] == k_idx {
                                    let link = nodes[nodes[p_idx].children[i]].link();
                                    nodes[k_idx].set_link(link);
                                    nodes[g_idx].set_link(link);
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
                        if let Some(provenance) = sheet_provenance.as_mut() {
                            provenance.close_box();
                        }
                        flush_x(&mut x_stack, &mut friends, &mut nodes, in_form, None);
                        x_stack.pop();
                        flush_x(&mut x_stack, &mut friends, &mut nodes, in_form, Some(p_idx));
                        if let Some(np_idx) = next_parent {
                            nodes[np_idx].contain_box(box_v);
                        }
                        parent = next_parent;
                        (last_k, last_g) = (None, None);
                    } else {
                        return Err(invalid_data("mismatched SyncTeX hbox close"));
                    }
                }
                b'v' | b'h' | b'r' | b'k' | b'g' | b'$' | b'x' => {
                    let Some((mut tag, mut line, col, x, y, rem)) =
                        parse_link_pos(&trimmed[1..], &mut last_v)
                    else {
                        continue;
                    };
                    let coords = match kind {
                        b'v' | b'h' | b'r' => parse_whd(rem).map(|(w, h, d)| [x, y, w, h, d]),
                        b'k' => parse_synctex_int_prefix(rem).map(|(w, _)| [x, y, w, 0, 0]),
                        _ => Some([x, y, 0, 0, 0]),
                    };
                    let Some(coords) = coords else {
                        continue;
                    };
                    scaled_geometry(&coords)?;
                    if fix_cross_file_tags
                        && kind == b'r'
                        && coords[2] == 0
                        && let Some(p_idx) = parent
                        && nodes[p_idx].width != 0
                        && tag != nodes[p_idx].tag
                    {
                        tag = nodes[p_idx].tag;
                        line = nodes[p_idx].line;
                    }
                    let idx = nodes.len();
                    nodes.push(ForwardNode::new(kind, page, parent, tag, line, col, coords));
                    let prev_is_bdry = parent.is_some_and(|p_idx| {
                        nodes[p_idx].children.len() == 1
                            && nodes[nodes[p_idx].children[0]].kind == b'b'
                    });
                    if let Some(p_idx) = parent {
                        nodes[p_idx].children.push(idx);
                        match kind {
                            b'h' | b'k' => {
                                let node = &nodes[idx];
                                let bounds = if kind == b'h' {
                                    node.data_box()
                                } else {
                                    node.data_xob()
                                };
                                nodes[p_idx].contain_box(bounds);
                            }
                            b'g' | b'$' | b'x' => nodes[p_idx].contain_point(x, y),
                            _ => {}
                        }
                    }
                    if kind != b'x' {
                        if !in_form && !matches!(kind, b'v' | b'h') {
                            make_friend(&mut friends, &nodes, idx);
                        }
                        flush_x(&mut x_stack, &mut friends, &mut nodes, in_form, Some(idx));
                    } else if prev_is_bdry || x_stack.last().is_some_and(|s| !s.is_empty()) {
                        if let Some(pending) = x_stack.last_mut() {
                            pending.push(idx);
                        }
                    } else if !in_form {
                        make_friend(&mut friends, &nodes, idx);
                    }
                    record_max_line(&mut max_lines, tag, line);
                    (last_k, last_g) = match kind {
                        b'k' => (Some(idx), None),
                        b'g' if last_k.is_some() => (last_k, Some(idx)),
                        _ => (None, None),
                    };
                }
                b'f' => {
                    if let Some((ftag, fx, fy)) = parse_form_ref(&trimmed[1..], &mut last_v) {
                        if let Some(p_idx) = parent {
                            scaled_geometry(&[fx, fy])?;
                            // A reference to a form the sidecar never defines (pdfTeX's clipped
                            // graphics) has no content or source identity, so it must not disturb
                            // its neighbors. A definition may follow it, which takes reading on.
                            let mut defined = forms.contains_key(&ftag);
                            if !defined {
                                if later_forms.is_none() {
                                    let rows =
                                        read_remaining_rows(&mut reader, bytes_read, operation)?;
                                    later_forms = Some(forms_defined_in(&rows, &forms, operation)?);
                                    buffered_rows = Some(io::Cursor::new(rows));
                                }
                                defined = later_forms
                                    .as_ref()
                                    .is_some_and(|later| later.contains(&ftag));
                            }
                            if !defined {
                                continue;
                            }
                            let ref_idx = nodes.len();
                            let coords = [fx, fy, 0, 0, 0];
                            nodes.push(ForwardNode::new(b'f', page, parent, 0, 0, 0, coords));
                            nodes[p_idx].children.push(ref_idx);
                            let (refs, sheet) = if in_form {
                                (&mut ref_in_form, None)
                            } else {
                                (&mut ref_in_sheet, current_sheet_idx)
                            };
                            refs.push((ref_idx, p_idx, page, sheet, ftag, fx, fy));
                        }
                        (last_k, last_g) = (None, None);
                    }
                }
                _ => {
                    (last_k, last_g) = (None, None);
                }
            }
        }
    }

    if current_page.is_some()
        || !form_stack.is_empty()
        || ignored_form_depth > 0
        || parent.is_some()
    {
        return Err(invalid_data("unclosed SyncTeX sheet, form, or box at EOF"));
    }
    if (saw_preamble || saw_postamble) && !(saw_postamble && saw_count) {
        return Err(invalid_data("truncated SyncTeX postamble"));
    }

    // With form recording disabled, TeX still emits sheet references whose definitions are
    // absent; those are dropped. Recorded ones need transforms a source map does not invent.
    if let Some(provenance) = provenance.as_mut()
        && ref_in_sheet.iter().any(|r| forms.contains_key(&r.4))
    {
        provenance.geometry_error.get_or_insert(
            "SyncTeX fragile body ownership cannot resolve recorded sheet form transforms",
        );
    }
    let form_refs = ref_in_form.iter().map(|r| (r, false));
    for (&(ref_idx, p_idx, page, sheet_idx, ftag, fx, fy), in_sheet) in
        form_refs.chain(ref_in_sheet.iter().map(|r| (r, true)))
    {
        if let Some(op) = operation {
            op.check()?;
        }
        let Some(&form_root) = forms.get(&ftag) else {
            nodes[p_idx].children.retain(|&c| c != ref_idx);
            continue;
        };
        let target = nodes[form_root]
            .children
            .first()
            .copied()
            .ok_or_else(|| invalid_data("SyncTeX form reference targets empty form"))?;
        let (dh, dv) = (fx, fy - nodes[target].height);
        let mut reg = (in_sheet && track_friends).then_some(friends.as_mut_slice());
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
    let [x_off, y_off] = [0, 1].map(|axis| {
        post_offset[axis].map_or((pre_offset[axis] as f64) * base_unit, |off| off / 65781.76)
    });
    let bound = (1_500_000.0 / mag_factor) as i64;
    if let Some(provenance) = provenance.as_mut() {
        provenance.postamble_count = saw_postamble && saw_count;
    }

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
        source_provenance: provenance.map(Box::new),
    })
}

pub(crate) fn forward_candidates_resolved(
    pdf: &Path,
    file: &Path,
    view_line: u32,
    page_hint: Option<u32>,
    build_directory: Option<&Path>,
) -> io::Result<Vec<ForwardCandidate>> {
    let (reader, sidecar_path) = crate::open_synctex_with_path_in(pdf, build_directory)?;
    let sidecar_dir = crate::canonical_sidecar_dir(&sidecar_path);
    forward_candidates_from_reader_in(
        reader,
        ForwardLookup {
            pdf,
            file,
            view_line,
            fix_readahead: true,
            page_hint,
            sidecar_dir: sidecar_dir.as_deref(),
        },
    )
}

#[derive(Clone, Copy)]
pub(crate) struct ForwardLookup<'a> {
    pub(crate) pdf: &'a Path,
    pub(crate) file: &'a Path,
    pub(crate) view_line: u32,
    pub(crate) fix_readahead: bool,
    pub(crate) page_hint: Option<u32>,
    pub(crate) sidecar_dir: Option<&'a Path>,
}

pub(crate) fn forward_candidates_from_reader_in(
    reader: impl BufRead,
    lookup: ForwardLookup<'_>,
) -> io::Result<Vec<ForwardCandidate>> {
    let view_line = lookup.view_line;
    let ParsedSynctex {
        inputs,
        max_lines,
        nodes,
        friends,
        sheets,
        unit,
        x_off,
        y_off,
        bound,
        ..
    } = parse_synctex_document_with_options(reader, lookup.fix_readahead, None, None)?;

    let no_match = || {
        io::Error::other(ForwardUnavailable(
            "synctex view returned no complete match",
        ))
    };
    let (input_tag, matching_tags) =
        crate::forward_input_tags(&inputs, lookup.file, lookup.pdf, lookup.sidecar_dir)
            .ok_or_else(no_match)?;

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
                    || nodes[cursor].height + nodes[cursor].depth > bound
                {
                    return cursor;
                }
                idx = cursor;
            }
        }
        idx
    };

    let display_query = |nodes: &[ForwardNode],
                         tag: u32,
                         query_line: u32|
     -> Vec<ForwardCandidate> {
        let max_l = max_lines.get(&tag).copied().unwrap_or(0);
        let mut line = i64::from(query_line).min(max_l);
        let mut line_offset = 1i64;
        for _ in 0..100 {
            if line <= max_l {
                for exclude_box in [true, false] {
                    let bucket_idx = friend_bucket(tag, line);
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
                        let mut parent_weights = std::collections::HashMap::<usize, usize>::new();
                        for (p_num, page_nodes) in pages {
                            parent_weights.clear();
                            let mut weighted = Vec::with_capacity(page_nodes.len());
                            for n_idx in page_nodes {
                                if let Some(p_idx) = nodes[n_idx].parent {
                                    let w = if let Some(&cached) = parent_weights.get(&p_idx) {
                                        cached
                                    } else {
                                        let count = nodes[p_idx]
                                            .children
                                            .iter()
                                            .filter(|&&c| {
                                                nodes[c].tag == tag && nodes[c].line == line
                                            })
                                            .count();
                                        let has_weight_slot = nodes[p_idx].kind == b'('
                                            && nodes[p_idx].target.is_none();
                                        parent_weights
                                            .insert(p_idx, if has_weight_slot { 0 } else { count });
                                        count
                                    };
                                    weighted.push((w, n_idx));
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
                        let hint = lookup.page_hint.unwrap_or(0);
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
                                let v = (((nodes[bv].v_v + nodes[bv].depth_v) as f64) * unit
                                    + y_off) as f32;
                                let width = ((nodes[bv].width_v as f64) * unit) as f32;
                                let height = (((nodes[bv].height_v + nodes[bv].depth_v) as f64)
                                    * unit) as f32;
                                if !forward_geometry_valid(p_num, h, v, width, height) {
                                    continue;
                                }
                                let candidate = ForwardCandidate {
                                    page: p_num,
                                    source_line: line as u32,
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
                // Alternate around the query line: +1, -2, +3, ..., skipping lines below 1.
                for _ in 0..2 {
                    line += line_offset;
                    line_offset = -(line_offset + line_offset.signum());
                    if line > 0 {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        Vec::new()
    };

    let mut candidates = display_query(&nodes, input_tag, view_line);
    if lookup.fix_readahead
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
            overflow = display_query(&nodes, input_tag, target_line)
                .into_iter()
                .filter(|candidate| candidate.page >= next_page)
                .collect();
        }
        if best_guess_forward_candidate(&overflow).is_ok() {
            candidates = overflow;
        }
    }
    if candidates.is_empty() {
        return Err(no_match());
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
    if point.page > 0
        && point.x.is_finite()
        && point.y_from_top.is_finite()
        && point.page_height_pt.is_finite()
        && point.page_height_pt > 0.0
    {
        Ok(())
    } else {
        Err(invalid_input("invalid SyncTeX inverse query point"))
    }
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
    [
        b".pdf_tex".as_slice(),
        b".pdftex_t",
        b".ps_tex",
        b".pstex_t",
    ]
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
            && (child.kind == b'g' || (child.kind == b'k' && child.width == -d.width))
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
        if p.width > 0 && p.height + p.depth > 0 {
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
            if p.width > 0 && p.height + p.depth > 0 {
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
        return Err(invalid_input(
            "SyncTeX inverse query point out of scaled-point range",
        ));
    }
    let hit_h = i64::from(raw_h as i32);
    let hit_v = i64::from(raw_v as i32);
    let nodes = &parsed.nodes;

    // A form proxy carries no data of its own.
    let data_tag = |idx: usize| nodes[idx].target.map_or(nodes[idx].tag, |_| 0);
    let data_line = |idx: usize| nodes[idx].target.map_or(nodes[idx].line, |_| 0);
    let data_col = |idx: usize| nodes[idx].target.map_or(nodes[idx].column, |_| 0);
    let data_width = |idx: usize| nodes[idx].target.map_or(nodes[idx].width, |_| 0);
    // Raw heights and depths fit `i32`, node edges and visible sizes respect `MAX_COORD`, and the
    // hit point fits `i32`, so none of the distance arithmetic below can overflow.
    let parent_hd = |idx: usize| -> (i64, i64) {
        nodes[idx]
            .parent
            .map_or((0, 0), |p| (nodes[p].height.abs(), nodes[p].depth.abs()))
    };
    let signed_gap = |hit: i64, min: i64, max: i64| -> i64 {
        if hit < min {
            min - hit
        } else if hit > max {
            max - hit
        } else {
            0
        }
    };

    let h_ordered_distance = |idx: usize| -> i64 {
        let n = &nodes[idx];
        match n.kind {
            b'[' | b'v' | b'h' => signed_gap(hit_h, n.h, n.h + n.width.abs()),
            b'(' => signed_gap(hit_h, n.h_v, n.h_v + n.width_v.abs()),
            b'k' => {
                let w = n.width;
                let (min, max) = if w < 0 {
                    (n.h, n.h - w)
                } else {
                    (n.h - w, n.h)
                };
                let med = (min + max) / 2;
                if hit_h < min {
                    min - hit_h + 1
                } else if hit_h > max {
                    max - hit_h - 1
                } else if hit_h > med {
                    max - hit_h + 1
                } else {
                    min - hit_h - 1
                }
            }
            b'r' | b'g' | b'$' | b'x' | b'b' => n.h - hit_h,
            _ => i64::from(i32::MAX),
        }
    };

    let v_ordered_distance = |idx: usize| -> i64 {
        let n = &nodes[idx];
        let (min, max) = match n.kind {
            b'[' | b'v' | b'h' => (n.v - n.height.abs(), n.v + n.depth.abs()),
            b'(' => (n.v_v - n.height_v.abs(), n.v_v + n.depth_v.abs()),
            b'r' | b'k' | b'g' | b'$' => {
                let (ph, pd) = parent_hd(idx);
                (n.v - ph, n.v + pd)
            }
            _ => return i64::from(i32::MAX),
        };
        signed_gap(hit_v, min, max)
    };

    let point_in_box =
        |idx: usize| -> bool { h_ordered_distance(idx) == 0 && v_ordered_distance(idx) == 0 };

    let dist_to_box = |min_h: i64, max_h: i64, min_v: i64, max_v: i64| -> i64 {
        signed_gap(hit_h, min_h, max_h).abs() + signed_gap(hit_v, min_v, max_v).abs()
    };

    let point_node_distance = |idx: usize| -> i64 {
        let n = &nodes[idx];
        match n.kind {
            b'[' => dist_to_box(
                n.h,
                n.h + n.width.abs(),
                n.v - n.height.abs(),
                n.v + n.depth.abs(),
            ),
            b'(' => dist_to_box(
                n.h_v,
                n.h_v + n.width_v.abs(),
                n.v_v - n.height_v.abs(),
                n.v_v + n.depth_v.abs(),
            ),
            b'v' | b'h' => {
                let min_v = n.v - n.height.abs();
                let max_v = n.v + n.depth.abs();
                let right_h = n.h + n.width.abs();
                dist_to_box(n.h, n.h, min_v, max_v).min(dist_to_box(right_h, right_h, min_v, max_v))
            }
            b'k' => {
                let (ph, _) = parent_hd(idx);
                let min_v = n.v - ph;
                let other_h = n.h - n.width;
                dist_to_box(n.h, n.h, min_v, n.v).min(dist_to_box(other_h, other_h, min_v, n.v))
            }
            b'g' | b'$' | b'x' | b'b' => {
                let (ph, _) = parent_hd(idx);
                dist_to_box(n.h, n.h, n.v - ph, n.v)
            }
            _ => i64::from(i32::MAX),
        }
    };

    let smallest_container = |node: usize, other: usize| -> usize {
        // Visible sizes are below 2^42, so their product needs `i128` rather than saturation.
        let dims = |idx: usize| -> (i128, i64) {
            if nodes[idx].kind == b'(' {
                let w = nodes[idx].width_v.abs();
                let th = nodes[idx].depth_v.abs() + nodes[idx].height_v.abs();
                (i128::from(w) * i128::from(th), th)
            } else {
                (0, 0)
            }
        };
        // Smaller area, then wider data, then shorter height; ties keep `node`.
        let key = |idx: usize| {
            let (area, th) = dims(idx);
            (area, std::cmp::Reverse(data_width(idx).abs()), th)
        };
        if key(node) <= key(other) { node } else { other }
    };

    /// The deepest box under `idx` that owns the point. With `hit_only`, the walk descends only
    /// into children containing the point and a vbox keeps the last of its equally distant
    /// non-empty children; otherwise it descends into every child and keeps the first.
    fn deepest_container(
        nodes: &[ForwardNode],
        idx: usize,
        hit_only: bool,
        point_in_box: &impl Fn(usize) -> bool,
        point_node_distance: &impl Fn(usize) -> i64,
    ) -> Option<(usize, i64)> {
        if nodes[idx].children.is_empty() {
            return None;
        }
        for &ch in &nodes[idx].children {
            if (!hit_only || point_in_box(ch))
                && let Some(deep) =
                    deepest_container(nodes, ch, hit_only, point_in_box, point_node_distance)
            {
                return Some(deep);
            }
        }
        if nodes[idx].kind == b'[' {
            let mut best = None::<(usize, i64)>;
            for &ch in &nodes[idx].children {
                if !nodes[ch].children.is_empty() {
                    let d = point_node_distance(ch);
                    if best.is_none_or(|(_, bd)| d < bd || (hit_only && d == bd)) {
                        best = Some((ch, d));
                    }
                }
            }
            if best.is_some() {
                return best;
            }
        }
        point_in_box(idx).then_some((idx, 0))
    }

    fn closest_deep_child_v2(
        nodes: &[ForwardNode],
        idx: usize,
        point_node_distance: &impl Fn(usize) -> i64,
    ) -> (Option<usize>, i64) {
        // Distances are i64; an i32 sentinel would lose to nothing.
        if nodes[idx].children.is_empty() {
            return (None, i64::MAX);
        }
        let mut best = (None::<usize>, i64::MAX);
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

    let closer_or_earlier = |cur: Option<(usize, i64)>, d: i64, ch: usize| match cur {
        None => true,
        Some((n, nd)) => {
            d < nd
                || (d == nd
                    && data_tag(n) == data_tag(ch)
                    && (data_line(n), data_col(n)) > (data_line(ch), data_col(ch)))
        }
    };
    let descend_child = |cur: (usize, i64)| {
        let mut cur = deepest_container(nodes, cur.0, false, &point_in_box, &point_node_distance)
            .unwrap_or(cur);
        if matches!(nodes[cur.0].kind, b'[' | b'(' | b'v' | b'h')
            && let (Some(cn), _) = closest_deep_child_v2(nodes, cur.0, &point_node_distance)
        {
            cur.0 = cn;
        }
        cur
    };
    let get_closest_children_in_box = |mut idx: usize| {
        'hbox: loop {
            if nodes[idx].children.is_empty() || nodes[idx].kind != b'(' {
                return (None, None);
            }
            let mut l = None::<(usize, i64)>;
            let mut r = None::<(usize, i64)>;
            for &ch in &nodes[idx].children {
                let d = h_ordered_distance(ch);
                if d > 0 {
                    if closer_or_earlier(r, d, ch) {
                        r = Some((ch, d));
                    }
                } else if d == 0 {
                    if !nodes[ch].children.is_empty() {
                        idx = ch;
                        continue 'hbox;
                    }
                    l = Some((ch, 0));
                } else {
                    let d = -d;
                    if closer_or_earlier(l, d, ch) {
                        l = Some((ch, d));
                    }
                }
            }
            return (l.map(descend_child), r.map(descend_child));
        }
    };

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
                deepest_container(nodes, best, true, &point_in_box, &point_node_distance)
                    .map_or(best, |(deep, _)| deep),
            );
            break;
        }
    }

    let (nds_l, nds_r) = if let Some(c_idx) = container {
        get_closest_children_in_box(c_idx)
    } else if let Some(&root_child) = nodes[sheet.root].children.last() {
        let (cn, cd) = closest_deep_child_v2(nodes, root_child, &point_node_distance);
        (cn.map(|n| (n, cd)), None)
    } else {
        return Ok(Vec::new());
    };

    let mut selected_nodes: Vec<usize> = match (nds_l, nds_r) {
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

    let graphics_boxes = sheet
        .graphics_boxes
        .get_or_init(|| {
            sheet_graphics_boxes(
                nodes,
                &parsed.inputs,
                parsed.fix_cross_file_tags,
                sheet.root,
            )
        })
        .as_slice();
    let point_in_data_box = |idx: usize| -> bool {
        let [min_h, max_h, min_v, max_v] = nodes[idx].data_box();
        min_h <= hit_h && hit_h <= max_h && min_v <= hit_v && hit_v <= max_v
    };
    let mut hits = graphics_boxes
        .iter()
        .filter(|g| point_in_data_box(g.box_idx));
    let unique_graphics = match (hits.next(), hits.next()) {
        (Some(graphics), None) => Some(graphics),
        _ => None,
    };
    if let Some(graphics) = unique_graphics
        && !selected_nodes.is_empty()
        && selected_nodes.iter().all(|&idx| {
            let mut ancestor = Some(idx);
            while let Some(current) = ancestor {
                if graphics_boxes.iter().any(|g| g.box_idx == current) {
                    return true;
                }
                ancestor = nodes[current].parent;
            }
            false
        })
    {
        // Unscaled inner image boxes can overlap a neighboring image's scaled box.
        selected_nodes.clear();
        selected_nodes.push(graphics.host_idx);
    }

    // The hull of a box enclosing a graphic absorbs its natural inner box, so containing
    // the point does not show that such a box was typeset independently of the graphic.
    let mut graphics_enclosures = std::collections::HashSet::new();
    for g in graphics_boxes {
        let mut ancestor = nodes[g.box_idx].parent;
        while let Some(current) = ancestor {
            if !graphics_enclosures.insert(current) {
                break;
            }
            ancestor = nodes[current].parent;
        }
    }
    // Text typeset over an image keeps its own identity, even on the image's source line.
    let overlaid_on_graphics = |selected: usize| -> bool {
        let chain = std::iter::successors(Some(selected), |&idx| nodes[idx].parent);
        !chain.clone().any(|idx| {
            graphics_boxes
                .iter()
                .any(|g| g.box_idx == idx || g.gen_root == Some(idx))
        }) && chain
            .take_while(|idx| !graphics_enclosures.contains(idx))
            .any(|idx| nodes[idx].kind == b'(' && point_in_box(idx))
    };

    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();
    for mut idx in selected_nodes {
        let overlaid = overlaid_on_graphics(idx);
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
        for g in graphics_boxes {
            if nodes[g.host_idx].tag == tag && nodes[g.host_idx].line == nodes[idx].line {
                if point_in_data_box(g.box_idx) {
                    hit_index = Some(same_anchor_count);
                    hit_same_anchor += 1;
                }
                same_anchor_count += 1;
            }
        }
        let graphics = if overlaid {
            None
        } else if hit_same_anchor == 1 && unique_graphics.is_some() {
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
