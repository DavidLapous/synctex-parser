# synctex-parser

Pure-Rust SyncTeX parser and forward/inverse navigation engine for Unix (`macOS` / `Linux`), with zero runtime dependency on the `synctex` CLI or a PDF rendering library.

Diff with original [synctex](https://github.com/jlaurens/synctex).
 - Only handles the reading of synctex data for forward/backward synctex.
 - Heuristics to have word-precise backward, i.e., better column support for synctex.
 - Can dig into frames, equations, tabular, title, captions, etc. and forward back fragile beamer slides. This is based on heuristics, and have to be implemented per functions. open an issue / pr if something is missing there. 
 - Session cache support, and awarness on the pdf hash for best guesses.

## Quick Start

Add `synctex-parser` to your crate (`cargo add synctex-parser`) or `Cargo.toml`:

```toml
[dependencies]
synctex-parser = "0.1"
```

```rust,no_run
use std::path::Path;
use synctex_parser::{InversePoint, Operation, SyncTeXCache, resolve_forward, resolve_inverse_with_cache};

fn main() -> std::io::Result<()> {
    let pdf = Path::new("/abs/path/paper.pdf");
    let tex = Path::new("/abs/path/paper.tex");

    // Forward search: 1-based line and 1-based Unicode scalar column -> PDF box
    let forward = resolve_forward(pdf, tex, 42, 10)?;
    println!("page {} @ ({}, {})", forward.page, forward.h, forward.v);

    // Inverse search: PDF click -> 1-based line and UTF-8 / UTF-16 / char columns
    let mut cache = SyncTeXCache::default();
    let point = InversePoint { page: 1, x: 140.0, y_from_top: 220.0, page_height_pt: 792.0 };
    let op = Operation::default();
    let hit = resolve_inverse_with_cache(
        pdf,
        point,
        Some(("theorem", 2)), // optional clicked PDF word + 0-based UTF-8 byte offset
        12,
        &op,
        None,
        false, // strict mode (set true for additional best_guess fallback)
        Some(&mut cache),
    )?;
    println!("{}:{}:{}", hit.location.file, hit.location.line, hit.location.column);
    Ok(())
}
```

## Navigation guarantees

`SourceLocation.precise` means an accepted native-compatible column hint, not
proof that a particular TeX execution printed that occurrence. Strict and
Best Guess refinements use the same final native compatibility guard. Best Guess
also enables lexical fallback and warned coarse anchors; a precise compatible
fallback does not necessarily carry a separate guess warning.
Document metadata declared outside that native support does not override a
compiler anchor, including an anchor on `\maketitle`.
Unresolved or truncated TeX imports do not globally invalidate column hints.
Known macro definitions can remain available after an unresolved input; this
does not establish the actual TeX expansion. Required source-cache errors remain
errors.

A click in a uniquely supported compiler graphics region may resolve to the
`\includegraphics` filename, including when the PDF contains selectable vector
text. That filename is an image-invocation anchor, not a claim that its bytes
were printed. Ordinary lexical matches inside nonprinting filename arguments
do not establish graphics ownership.
Undefined SyncTeX form references do not erase their enclosing source anchors;
defined forms, including definitions after their references, retain native
mapping. Overlapping or ambiguous graphics regions may still keep coarse
compiler anchors; this is not a guarantee of exact navigation for every image.

Run-in `\paragraph` and `\subparagraph` headings (including starred forms) may
refine to their title columns when the native anchor names the first body line.
Comments and literal `\label`/`\index` prefixes do not move that body boundary;
quoted or verbatim section commands do not establish heading support.
Multiline standard text-style arguments (`\textbf`, `\textit`, `\emph`, and
related styles) and math-font arguments (`\mathbf`, `\mathrm`, `\mathbb`, and
related fonts) share the collected-argument handling used by captions. Words
retain their original line and column when the native anchor names the closing
line. Nested collectors retain the enclosing compiler boundary; single-line
formatting and nonclosing native anchors keep their ordinary matching behavior.
Quoted commands, opaque definitions and math atom-class primitives do not
establish collected formatting support. Custom macro signatures are not inferred.
The first literal word on a continued physical source line may retain its own
column when native SyncTeX names the immediately preceding line. This narrow
read-ahead alias excludes blank/comment-only lines, previous rows containing
braces, and commands outside closed math regions on that row. Grouped or
non-math-command continuations, including a row following an unbraced section
title, remain conservative without additional scope proof; later words and
existing collected scopes keep their compiler-line rules.

Beamer frame overlay and option metadata are excluded from literal matching.
Printed titles, subtitles and body text after a paragraph boundary remain
eligible; printed optional headings in other environments are not frame options.

Direct reference commands (`\ref`, `\eqref`, `\pageref`, `\autoref`, `\cref`,
`\Cref`, including supported starred forms) can resolve generated reference
text to the first non-space/comment label byte inside the referring `{...}`.
Local source/PDF flanks must identify a unique invocation across competing body
and footnote output and pass the native compiler-line guard; label lists anchor
at their first key. Repeated indistinguishable flanks remain coarse rather than
being paired by occurrence count. Unsupported or ambiguous output remains
coarse, and raw keys stay excluded from ordinary literal matching. This does
not follow the hyperlink to the label declaration or interpret arbitrary TeX
macros.
At a same-line page break or Beamer frame boundary, a generated span touching
the local PDF excerpt's edge remains coarse: running furniture cannot safely be
excluded, even when source-side text is hidden or glued to the reference output.
References with distinct visible flanks on both sides remain eligible. Known
page-break command overrides are respected; dynamic TeX conditionals are not
evaluated.
Automatic page-break/footer ownership and hidden argument openings outside the
bounded source window are not execution-certified by native-compatible columns
or matching saved-source snapshots.

Fixed zero-argument prose macros can resolve to the invocation's backslash byte
in the compiled body file, including definitions imported from another file and
calls inside transparent text formatting. Expansion is limited to 64 KiB of known
literal text, groups, transparent formatting and nested zero-argument macros;
it does not interpret dynamic TeX or macros that consume parameters. Nearby word
corroboration is required; equal-evidence macro/literal ties remain coarse.
Nonprinting phantom, spacing (`\hspace`, `\vspace`) and `\index` arguments are
excluded unless a known user definition overrides the built-in.
Footnote references require a matched word inside the footnote; a mark glued to
the only available left anchor does not justify stripping digits or borrowing
body text to manufacture a match.

Forward candidates expose the probed `source_line`. Forward JSON requests use
`inverse_search` for the editor's revision-bound source-map service;
`source_snapshot` is not a request field, and unknown fields are rejected.

## Replay workflow

During iteration, run focused regressions and a real-input preflight, then
finish independent review **before** freezing the consumer for a full replay:

```sh
python3 -m unittest discover -s scripts
python3 scripts/synctex_fuzz.py document.pdf --source main.tex --preflight
# REVIEWED_SHA is the SHA-256 of the binary that actually received review.
python3 scripts/synctex_fuzz.py document.pdf --source main.tex \
  --full --reviewed-binary-sha256 "$REVIEWED_SHA" --capture target/replay-final
# After an oracle-only change, keep the producer and all inputs unchanged:
python3 scripts/synctex_fuzz.py document.pdf --source main.tex \
  --full --reviewed-binary-sha256 "$REVIEWED_SHA" --replay target/replay-final
```

`--preflight` scans declarations in the actual recorded compiler inputs before
probing up to eight source lines and eight painted words on one selected page.
`--full` automatically exercises a bounded preflight before the main queries,
rejects sampling caps, probes every eligible position in the **selected source**
and every eligible painted PDF word, and requires an explicit reviewed-binary
hash. That hash is a review attestation, not an automated reviewer. Repeat with
separate capture directories for each authored source and Strict/Best Guess mode;
the script does not infer which compiler inputs are authored corpus members.

Captures pin the binary, PDF, sidecar, recorder, recorded inputs, tool binaries,
sampling parameters, file revisions and (when inverse probing is enabled) the
PyMuPDF/MuPDF versions. Exact argv, stdin, working directory and decoding bind
every raw result. Inputs and both loaded oracle modules (`synctex_fuzz.py` and
`replay_capture.py`) are checked again before completion; their module-load
hashes are reported but deliberately excluded from replay input identity.
A completed capture certifies execution, **not** passing classification:
nonzero diagnostic results, failures and abstentions remain. The JSON
`capture_complete` flag distinguishes a complete capture/replay from operational
failure; `stopped_early` includes transport failures even without a batch exception.
Oracle-only replay never launches an **external producer process**; Python
reruns PyMuPDF word extraction and classification. Changed inputs, absent or
unused requests, corruption and incomplete captures fail explicitly rather than
rerunning silently. Replay must consume exactly the same set of unique producer
requests, independent of order. A classifier change needing more **or fewer**
queries requires a fresh reviewed capture.

Capture directories are private (owner-only) and contain unredacted source paths
and outputs. Keep them under ignored `target/`; never commit or share them.
Interrupted runs retain completed command files for diagnosis but cannot be
replayed as complete. Forward requests still use the existing one-shot CLI:
profiling has not established a native startup bottleneck that justifies a new
production batch API.

## Manual theorem-title reproducer

`scripts/fixtures/theorem-title/` isolates a local theorem definition, optional
heading math and nested inputs. From the repository root, with `pdflatex`, the
Python oracle dependencies and a built sibling `pdfterm`:

```sh
mkdir -p target/theorem-title
TEXINPUTS="$PWD/scripts/fixtures/theorem-title/:" pdflatex \
  -interaction=nonstopmode -halt-on-error -synctex=1 -recorder \
  -output-directory=target/theorem-title scripts/fixtures/theorem-title/main.tex
python3 scripts/synctex_fuzz.py target/theorem-title/main.pdf \
  --source scripts/fixtures/theorem-title/snazzle.tex \
  --binary ../pdfterm/target/release/pdfterm --seed 20261020 --lines 20 \
  --require-precision-word Frumple --require-precision-word wazzit
```

The optional heading is printable because a recorded compiler input declares
`zibble` with `\newtheorem`; unknown environment options remain nonprinting
oracle controls. The heading and body literals above must retain verified
precision. Forward anchors on the first two source lines are supported;
the closing line has no native match and may be rejected. Forward probes check
native page compatibility, not title columns: declared-theorem context is used
only by the inverse oracle, so an otherwise ambiguous forward theorem title
without independent visible context remains unverified.

Declaration discovery scans ASCII TeX syntax through a lossless byte-preserving
view of recorded inputs; non-UTF-8 package bytes are neither discarded nor
replaced. Queried source text still requires UTF-8.

The full oracle is still a diagnostic, not a guaranteed passing regression.
It must reject a precise result on the environment identifier when clicking
the generated theorem label. The joined PDF math word `Qkz` also does not
independently establish which literal in `$Q_kz$` a column should select;
this composite-script case remains a reported precision mismatch rather than
being exempted from the oracle.
