# synctex-parser

Pure-Rust SyncTeX parser and forward/inverse navigation engine for Unix (`macOS` / `Linux`), with zero runtime dependency on the `synctex` CLI or a PDF rendering library.

Diff with original [synctex](https://github.com/jlaurens/synctex).
 - Only handles the reading of synctex data for forward/backward synctex.
 - Heuristics to have word-precise backward, i.e., better column support for synctex.
 - Can dig into frames, equations, tabular, title, captions, etc. and forward back fragile beamer slides. This is based on heuristics, and have to be implemented per functions. open an issue / pr if something is missing there. 
 - Session cache support and optional best-guess fallback.

## Quick Start

Add `synctex-parser` to your crate (`cargo add synctex-parser`) or `Cargo.toml`:

```toml
[dependencies]
synctex-parser = "0.1.2"
```

```rust,no_run
use std::path::Path;
use synctex_parser::{
    ForwardSource, InversePoint, Operation, SyncTeXCache, resolve_forward_with_selector,
    resolve_inverse_with_cache,
};

fn main() -> std::io::Result<()> {
    let pdf = Path::new("/abs/path/paper.pdf");
    let tex = Path::new("/abs/path/paper.tex");

    // Forward search: 1-based line and 1-based Unicode scalar column -> PDF box
    // The selector may refine the compiler anchor using PDF text; this one keeps it.
    let forward = resolve_forward_with_selector(
        pdf,
        tex,
        42,
        10,
        ForwardSource::default(),
        |_, _, _, anchor| Ok(anchor),
    )?;
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
        false, // strict mode (set true for additional best_guess fallback)
        Some(&mut cache),
    )?;
    println!("{}:{}:{}", hit.location.file, hit.location.line, hit.location.column);
    Ok(())
}
```

