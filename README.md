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

Forward candidates expose the probed `source_line`. Forward JSON requests use
`inverse_search` for the editor's revision-bound source-map service;
`source_snapshot` is not a request field, and unknown fields are rejected.

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
  --binary ../pdfterm/target/release/pdfterm --seed 20261020 --lines 20
```

The full oracle currently reports failures on this case. It is a diagnostic
reproducer, not a passing regression test or a positive precision control.
