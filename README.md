# synctex-parser

Pure-Rust SyncTeX parser and forward/inverse navigation engine with zero runtime dependency on the `synctex` CLI or a PDF rendering library.

## Core Specifications

### 1. Companion Discovery & Caching
- **Sidecar lookup**: Searches beside the PDF first, then in an optional build directory (`set_build_directory` / `SYNCTEX_BUILD_DIRECTORY`), checking `.synctex`, `.synctex.gz`, and `.pdf.synctex(.gz)`.
- **Content-based decompression**: Detects gzip streams via magic bytes (`0x1f 0x8b`) regardless of file extension.
- **Session caching (`SyncTeXCache`)**: Reuses decompressed companion bytes, parsed inverse sheet trees, and structural maps across queries while automatically invalidating on companion `DocumentRevision` changes (device, inode, size, mtime, ctime).

### 2. Coordinate Systems
- **PDF geometry (`SearchRect`, `InversePoint`)**: Top-down PDF points (`1 pt = 1/72 in`, TeX `sp` scaled by unit/magnification plus postscript offsets).
- **Source locations (`SourceLocation`)**:
  - `line`: 1-based line number.
  - `byte_column`: 0-based UTF-8 byte offset within the line.
  - `column`: 1-based UTF-16 code-unit column (LSP / VS Code).
  - `column_char`: 1-based Unicode scalar (`char`) column.
  - `precise`: `true` when refined to a unique token/column; `false` for line-level anchors.

### 3. Forward Resolution (`resolve_forward*` / `forward_candidates*`)
- **Raw candidates**: Streams `.synctex(.gz)` records to emit merged horizontal bounding boxes per page, with optional 1-based `page_hint` tie-breaking.
- **Collected-body read-ahead**: Maps cursor lines inside collected TeX bodies (`\begin{frame}...\end{frame}`, `\begin{tabularx}...\end{tabularx}`, and multi-line `\caption{...}`) to their closing emission line when TeX defers box anchors.
- **Overflow-sheet selection & token extraction**: Supports caller callbacks (`resolve_forward_with_selector`) to pick among multi-page frame/environment sheets and extracts source `ForwardWord` tokens (`source_word_at`) for caller-side PDF word matching.

### 4. Inverse Resolution (`resolve_inverse*` / `inverse_candidates*`)
- **Hit testing**: Selects the smallest containing or nearest SyncTeX boxes on the clicked page, preserving distinct compiler columns on the same line.
- **Fragile Beamer & metadata remapping**: Remaps `.vrb` hits back to the originating `frame` in the `.tex` source and resolves preamble metadata (`\title`, `\subtitle`, `\author`, `\institute`, `\date`, `\frametitle`, `\framesubtitle`).
- **Prose & math refinement**: Uses surrounding PDF words (`InverseContextWord`) to disambiguate repeated prose tokens (including TeX accents and OT1 spacing diacritics) and inline/display math spans while ignoring comments and non-printing definitions.
- **Strict vs. `best_guess`**: Strict mode rejects ambiguous anchors or unverified source history with typed errors (`InverseFailure` / `inverse_failure`); `best_guess` returns the best candidate alongside an explicit warning.

### 5. Source Snapshots & Safety Guarantees
- **Revision-bound snapshots (`source_snapshot`)**: Validates a private (`0700`) compiled-source cache against the exact `PdfRevision` and lowercase SHA-256 file digests, resolving relative compiler inputs against the captured working/build directories.
- **Bounded & non-blocking I/O**: Opens companions with `O_NONBLOCK` regular-file verification (rejecting FIFOs/devices), enforces a 128 MiB decoded stream limit, caps box/form nesting at 512 levels, rejects `i64::MIN` geometry overflow, and checks `Operation` cancellation/deadlines throughout parsing.

## Comparison vs. Baseline SyncTeX (`libsynctex`)

| Area | Baseline `synctex` (`libsynctex` CLI) | `synctex-parser` |
| --- | --- | --- |
| **Runtime & caching** | External C binary; re-reads and decompresses `.synctex.gz` on every call | Pure-Rust in-process library; `SyncTeXCache` reuses decompressed bytes and parsed page trees per `DocumentRevision` |
| **Granularity & units** | Line-level anchors (raw or missing column offsets) | Token- and math-refined positions with validated UTF-8 (`byte_column`), UTF-16 (`column`), and scalar (`column_char`) columns |
| **Collected bodies (`frame`, `tabularx`, `\caption`)** | Emits anchors only on the closing line (`\end{frame}`, `\end{tabularx}`, closing `}`) | Read-ahead resolves interior lines on forward search and remaps closing-line hits back to the interior source token on inverse search |
| **Beamer `.vrb` & metadata** | Inverse clicks open generated `.vrb` files or land on `\maketitle` / `\end{frame}` | Remaps `.vrb` hits back to the originating `.tex` frame and resolves `\title`, `\author`, `\frametitle`, etc. |
| **Source drift & ambiguity** | Silently picks the first candidate; assumes disk files match the PDF | Verifies revision-bound `SourceSnapshot` SHA-256 digests and distinguishes strict errors from warned `best_guess` results |
| **Malformed input & I/O safety** | Can block on FIFOs, ignore bad postambles, or overflow on extreme integers | `O_NONBLOCK` regular-file check, 128 MiB stream cap, 512 box-depth cap, `i64::MIN` rejection, and `Operation` deadlines |
