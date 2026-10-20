# synctex-parser

Pure-Rust SyncTeX parser and forward/inverse query engine (`view` and `edit`), including heuristics for:

- `\shipout` read-ahead line resolution (when a line sits inside a collected box or environment body whose anchor is emitted on a closing line)
- Beamer `frame` and fragile `.vrb` remapping
- Caption and preamble metadata (`\title`, `\subtitle`, `\author`, `\institute`, `\date`, `\frametitle`, `\framesubtitle`) fallback
- Conservative prose and inline/display math lexical refinement
- Revision-bound compiled source snapshots (`source_snapshot`)

Requires zero runtime dependency on the external `synctex` CLI.
