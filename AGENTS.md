## Privacy & No Private Data Leaks

- Never copy text, titles, author names, institutions, equations, or verbatim excerpts from the user's PDFs, LaTeX documents, talks, or local files into code, tests, fixtures, comments, docs, or commit messages.
- When reproducing a bug found while fuzzing or inspecting a real PDF/SyncTeX file, always replace the text with generic synthetic strings (e.g., `"Sample Title"`, `"Author Name"`, `"word (detail)"`) before writing tests or commits.
- Never include personal file paths, usernames, or private directory names in tracked files or commit history.
