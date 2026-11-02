## Privacy & No Private Data Leaks

- Never copy text, titles, author names, institutions, equations, or verbatim excerpts from the user's PDFs, LaTeX documents, talks, or local files into code, tests, fixtures, comments, docs, or commit messages.
- For every PDF/SyncTeX navigation bug, first reproduce it with the smallest runnable synthetic example: use gibberish prose and rename all document-specific commands, environments, labels, identifiers, and formulas while preserving the triggering syntax. Prove failure before editing, then retain minimal regression coverage. Never copy private content into the reproducer or recompile a private document merely to reproduce a bug.
- Never include personal file paths, usernames, or private directory names in tracked files or commit history.
