#!/usr/bin/env python3
"""Fuzz forward and inverse SyncTeX mappings for any TeX-built PDF.

Forward probes compare pdfterm's page with raw SyncTeX results. Inverse probes
enumerate painted PDF words with PyMuPDF, then send all selected word centers
through pdfterm's PDFium + inverse resolver batch diagnostic. File/line identity
is checked even for coarse results; fragile frames use the clicked sheet's
original source-map box, never the final reused .vrb contents. Precision checks
unique eligible literal spellings, not arbitrary TeX execution ownership.
Unknown expansions and repeated occurrences are explicitly unverified.
Failures exit 1; an enabled best-guess inverse run with no verified precision
exits 2 (inconclusive). Disabled inverse probing and text-free pages are exempt.

Usage: synctex_fuzz.py PDF [--source TEX] [--lines N] [--page N ...]
       [--pages N] [--words N] [--seed N] [--allow-stale]

By default every eligible painted word on every page is probed. --pages and
--words provide seed-deterministic quick caps; --pages 0 disables inverse
probes and --words 0 means all words on each selected page. Repeat --page to
probe exact one-based pages (for example, --page 27 --page 28).
"""

import argparse
import bisect
import concurrent.futures
import functools
import gzip
import json
import math
import random
import re
import string
import subprocess
import struct
import sys
import unicodedata
from collections import defaultdict
from collections.abc import Iterator
from pathlib import Path

WordPoint = tuple[str, tuple[float, float, float, float], float]
PDFTERM_BINARY = Path(
    globals().get(
        "PDFTERM_DEFAULT_BINARY",
        Path(__file__).resolve().parents[2] / "pdfterm" / "target" / "release" / "pdfterm",
    )
)
BUILD_DIRECTORY: Path | None = None


def usable_forward_record(record: dict[str, int | float]) -> bool:
    page = record.get("Page", 0)
    if not isinstance(page, int) or page <= 0:
        return False
    if not all(key in record for key in ("h", "v", "W", "H")):
        return False
    h, v, w, height = record["h"], record["v"], record["W"], record["H"]
    return (
        all(math.isfinite(val) for val in (h, v, w, height, h + w, v - height))
        and w >= 0
        and height >= 0
        and (w > 0 or height > 0)
    )


def raw_forward_records(pdf: Path, source: Path, line: int, column: int = 1) -> list[dict]:
    result = subprocess.run(
        ["synctex", "view", "-i", f"{line}:{column}:{source}", "-o", str(pdf)],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"synctex view: {result.stderr.strip()[:200]}")
    records, record = [], {}
    for row in [*result.stdout.splitlines(), ""]:
        key, sep, value = row.strip().partition(":")
        if sep and key in {"Page", "h", "v", "W", "H"}:
            if key == "Page" and "Page" in record:
                if usable_forward_record(record):
                    records.append(record)
                record = {}
            try:
                record[key] = int(value) if key == "Page" else float(value)
            except ValueError:
                pass
        elif not row.strip() or row.strip().startswith("SyncTeX result end"):
            if usable_forward_record(record):
                records.append(record)
            record = {}
    return records


def raw_pages(pdf: Path, source: Path, line: int, column: int = 1) -> set[int]:
    return {int(record["Page"]) for record in raw_forward_records(pdf, source, line, column)}


def raw_inverse(pdf: Path, x: float, y: float, page: int) -> list[tuple[int, str]]:
    result = subprocess.run(
        ["synctex", "edit", "-o", f"{page}:{x}:{y}:{pdf}"],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"synctex edit: {result.stderr.strip()[:200]}")
    seen, records, current_input = set(), [], None
    for row in result.stdout.splitlines():
        if row.startswith("Input:"):
            current_input = row.removeprefix("Input:").strip()
        elif row.startswith("Line:") and current_input:
            file, current_input = current_input, None
            try:
                line = int(row.removeprefix("Line:").strip())
            except ValueError:
                continue
            if line > 0 and (file, line) not in seen:
                seen.add((file, line))
                records.append((line, file))
    return records


def source_lines(text: str, keepends: bool = False) -> list[str]:
    """Match Rust source.lines(): LF/CRLF only, with no trailing empty line."""
    parts = text.split("\n")
    rows = [part + "\n" for part in parts[:-1]]
    if parts[-1]:
        rows.append(parts[-1])
    if keepends:
        return rows
    return [row[:-2] if row.endswith("\r\n") else row.removesuffix("\n") for row in rows]


def source_line_text(text: str) -> str:
    escaped = False
    for index, char in enumerate(text):
        if escaped:
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == "%":
            return text[:index]
    return text


def sample_source_positions(
    source: str, rng: random.Random, count: int
) -> list[tuple[int, int]]:
    candidates = []
    for line, row in enumerate(source_lines(source), 1):
        if not row.strip() or row.lstrip().startswith("%"):
            continue
        row = source_line_text(row)
        words = [
            match.start() + 1
            for match in re.finditer(r"[A-Za-z]{6,}", row)
            if match.start() == 0 or row[match.start() - 1] != "\\"
        ]
        candidates.append((line, rng.choice(words) if words else 1))
    return sorted(rng.sample(candidates, min(count, len(candidates))))


def normalized_words(value: str) -> list[str]:
    folded = "".join(
        char
        for char in unicodedata.normalize("NFKD", value).casefold()
        if not unicodedata.combining(char)
    )
    return re.findall(r"\w+", folded)


def context_score(tokens: list[str], hint: dict) -> int | None:
    source = [normalized_words(word) for word in hint["words"]]
    if any(len(word) != 1 for word in source):
        return None
    source = [word[0] for word in source]
    selected = hint["selected"]
    best = None
    for index, word in enumerate(tokens):
        if word != source[selected]:
            continue
        score = 0
        for distance in range(1, 4):
            for direction in (-1, 1):
                neighbor = index + direction * distance
                source_neighbor = selected + direction * distance
                if (
                    0 <= neighbor < len(tokens)
                    and 0 <= source_neighbor < len(source)
                    and tokens[neighbor] == source[source_neighbor]
                ):
                    score += 4 - distance
        best = max(score, best or 0)
    return best


def pdf_visible_words(pdf: Path) -> list[list[str]]:
    result = subprocess.run(
        ["pdftotext", "-layout", str(pdf), "-"],
        capture_output=True,
        text=True,
        errors="replace",
        timeout=120,
        check=False,
    )
    if result.returncode:
        raise SystemExit(f"pdftotext: {result.stderr.strip()[:200]}")
    pages = result.stdout.split("\f")
    if pages[-1].strip() == "":
        pages.pop()
    return [normalized_words(page) for page in pages]


SIDE_EXTENSIONS = {
    ".aux", ".nav", ".out", ".toc", ".w18", ".vrb",
    ".snm", ".bbl", ".blg", ".log", ".fls", ".fdb_latexmk", ".synctex",
}
GENERATED_GRAPHICS_EXTENSIONS = {".pdf_tex", ".pdftex_t", ".ps_tex", ".pstex_t"}


def is_compiler_sidecar_or_graphics(path: Path) -> bool:
    return (
        path.suffix in SIDE_EXTENSIONS
        or path.name.endswith(".synctex.gz")
        or path.suffix.lower() in GENERATED_GRAPHICS_EXTENSIONS
    )


def is_generated_input(path: Path, pdf_stem: str) -> bool:
    return (
        path.suffix in SIDE_EXTENSIONS
        and path.stem == pdf_stem
    ) or path.suffix.lower() in GENERATED_GRAPHICS_EXTENSIONS


def source_context_hint(text: str, line: int, column: int) -> dict | None:
    rows = source_lines(text, keepends=True)
    if not (1 <= line <= len(rows) and column >= 1):
        return None
    start = sum(len(row) for row in rows[:line - 1])
    end = start + len(source_lines(rows[line - 1])[0])
    position = start + column - 1
    if position >= end:
        return None
    literals, uncertain, opaque_words, _ = lexical_words(text)
    if any(lo < end and hi > start for lo, hi in uncertain):
        return None
    tokens = sorted(
        (lo, hi, word)
        for word, spans in literals.items()
        for lo, hi in spans
        if start <= lo and hi <= end
    )
    for selected, (lo, hi, word) in enumerate(tokens):
        if lo <= position < hi and word not in opaque_words:
            return {"words": [token[2] for token in tokens], "selected": selected}
    return None


def forward(
    pdf: Path,
    source: Path,
    source_text: str,
    positions: list[tuple[int, int]],
    page_count: int,
    visible_words: list[list[str]] | None = None,
    best_guess: bool = False,
    source_map: dict | None = None,
) -> Iterator[dict]:
    extra = ["--synctex-best-guess"] if best_guess else []
    if source_map is None and positions:
        source_map = read_source_map(pdf)
    source_resolved = source.resolve()
    if source_map is not None and source_resolved not in source_map["inputs"].values():
        for line, column in positions:
            yield {"direction": "forward", "line": line, "column": column, "ok": False, "error": "source is absent from compiler inputs"}
        return
    for line, column in positions:
        view_line = (
            collected_env_closing_line(source_text, line, column)
            or caption_closing_line(source_text, line, column)
            or line
        )
        try:
            expected = raw_pages(pdf, source, view_line, column)
        except (OSError, subprocess.TimeoutExpired, RuntimeError) as error:
            yield {
                "direction": "forward",
                "line": line,
                "ok": False,
                "error": str(error),
            }
            continue
        if source_map is not None:
            for p in sorted(expected):
                origin = source_map["origins"].get(p)
                if origin and source_map["inputs"].get(origin[0]) == source_resolved:
                    otag, oline = origin
                    s_max = max(
                        (l for t, l in source_map["page_lines"].get(p, set()) if t == otag and l < oline),
                        default=0,
                    )
                    if view_line > s_max:
                        for next_p in sorted(k for k in source_map["page_lines"] if k > p):
                            next_origin = source_map["origins"].get(next_p)
                            overflow = [
                                l
                                for t, l in source_map["page_lines"][next_p]
                                if t == otag and (t, l) != next_origin and l > s_max
                            ]
                            if overflow and min(overflow) <= oline:
                                expected.add(next_p)
                            else:
                                break
        try:
            result = subprocess.run(
                [
                    str(pdfterm_bin()),
                    str(pdf),
                    "--synctex-view",
                    str(source),
                    "--line",
                    str(line),
                    "--column",
                    str(column),
                    *extra,
                ],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            yield {
                "direction": "forward",
                "line": line,
                "column": column,
                "ok": False,
                "error": str(error),
            }
            continue
        if result.returncode != 0:
            yield {
                "direction": "forward",
                "line": line,
                "ok": not expected
                and (
                    "no complete match" in result.stderr
                    or "no usable compiler anchor geometry" in result.stderr
                ),
                "error": result.stderr.strip()[:200],
                "raw_pages": sorted(expected),
            }
            continue
        try:
            got = json.loads(result.stdout)
            page = got["page"]
            h, v, width, height = got["h"], got["v"], got["width"], got["height"]
            valid = (
                got["pdf"] == str(pdf)
                and type(page) is int
                and 1 <= page <= page_count
                and (not expected or page in expected)
                and all(
                    isinstance(val, (int, float))
                    and not isinstance(val, bool)
                    and math.isfinite(val)
                    for val in (h, v, width, height, h + width, v - height)
                )
                and width >= 0
                and height >= 0
                and (width > 0 or height > 0)
            )
        except (ValueError, KeyError, TypeError) as error:
            yield {
                "direction": "forward",
                "line": line,
                "ok": False,
                "error": str(error),
            }
            continue
        entry = {
            "direction": "forward",
            "line": line,
            "column": column,
            "ok": valid,
            "resolved_page": page,
            "raw_pages": sorted(expected),
            "raw_page_match": page in expected,
            "raw_unmapped": not expected,
        }
        if valid and (not expected or len(expected) > 1):
            hint = source_context_hint(source_text, line, column)
            entry["mapping_ok"] = True
            entry["visible_checked"] = False
            if visible_words is None or hint is None:
                entry.update(ok=False, abstained=True, reason="unverified_forward_source_context")
            else:
                try:
                    chosen_score = context_score(visible_words[page - 1], hint)
                    alternatives = [
                        candidate
                        for candidate in (expected or range(1, page_count + 1))
                        if candidate != page
                        and 1 <= candidate <= page_count
                        and (score := context_score(visible_words[candidate - 1], hint)) is not None
                        and (not expected or score > 0)
                    ]
                    if chosen_score is None and alternatives:
                        entry.update(
                            ok=False, visible_checked=True,
                            error="source word absent from chosen PDF page but present on another candidate page",
                            visible_alternatives=sorted(alternatives),
                        )
                    elif chosen_score is None:
                        entry.update(ok=False, abstained=True, reason="unverified_forward_source_context")
                    else:
                        entry["visible_checked"] = True
                except (IndexError, KeyError, TypeError, ValueError) as error:
                    entry["ok"] = False
                    entry["error"] = f"invalid independent source context or PDF page: {error}"
        yield entry


def read_source_map(pdf: Path) -> dict:
    companion = next((path for path in (pdf.with_suffix(".synctex.gz"), pdf.with_suffix(".synctex")) if path.is_file()), None)
    if companion is None:
        raise RuntimeError("missing SyncTeX source map")
    opener = gzip.open if companion.suffix == ".gz" else open
    build_dir = BUILD_DIRECTORY
    recorder = pdf.with_suffix(".fls")
    if build_dir is None and recorder.is_file():
        recorded = [row[4:] for row in recorder.read_text().splitlines() if row.startswith("PWD ")]
        if len(set(recorded)) != 1 or not Path(recorded[0]).is_absolute():
            raise RuntimeError("compiler recorder has no unique absolute PWD; pass --build-dir")
        build_dir = Path(recorded[0])
    inputs, origins, page_inputs, page_lines, host_origins = {}, {}, {}, {}, {}
    page = None
    box_stack: list[tuple[int, int]] = []
    with opener(companion, "rt", encoding="utf-8") as stream:
        size = 0
        for row in stream:
            size += len(row.encode("utf-8"))
            if size > 128 * 1024 * 1024:
                raise RuntimeError("SyncTeX source map exceeds 128 MiB")
            if row.startswith("Input:"):
                tag, _, name = row[len("Input:"):].rstrip("\r\n").partition(":")
                path = Path(name)
                if not path.is_absolute() and build_dir is None:
                    raise RuntimeError("relative compiler input requires .fls PWD or --build-dir")
                inputs[int(tag)] = (path if path.is_absolute() else build_dir / path).resolve()
            elif re.fullmatch(r"\{\d+\s*", row):
                page = int(row[1:])
                page_inputs[page] = set()
                page_lines[page] = set()
                box_stack.clear()
            elif page is not None and row.startswith("}"):
                if int(row[1:]) != page:
                    raise RuntimeError("SyncTeX source map has mismatched sheet bounds")
                page = None
                box_stack.clear()
            elif page is not None:
                if row[0] in "])":
                    if box_stack:
                        box_stack.pop()
                    continue
                node = re.match(r"([\[(vhkgx$r])(\d+),(\d+)(?:,\d+)?:", row)
                if not origins.get(page):
                    if not node or node[1] not in "[(":
                        raise RuntimeError("SyncTeX sheet has no enclosing source box")
                    origins[page] = (int(node[2]), int(node[3]))
                if node:
                    tag, line = int(node[2]), int(node[3])
                    page_inputs[page].add(tag)
                    page_lines[page].add((tag, line))
                    if (
                        tag in inputs
                        and inputs[tag].suffix.lower() in GENERATED_GRAPHICS_EXTENSIONS
                        and (page, tag) not in host_origins
                    ):
                        for htag, hline in reversed(box_stack):
                            if htag in inputs and not is_compiler_sidecar_or_graphics(inputs[htag]):
                                host_origins[(page, tag)] = (htag, hline)
                                break
                    if node[1] in "[(":
                        box_stack.append((tag, line))
    if page is not None or 1 not in inputs:
        raise RuntimeError("SyncTeX source map is incomplete or has no main input")
    return {
        "inputs": inputs,
        "origins": origins,
        "host_origins": host_origins,
        "page_inputs": page_inputs,
        "page_lines": page_lines,
        "build_dir": build_dir,
    }


def tex_group_end(text: str, start: int, opening: str = "{") -> int | None:
    closing = "}" if opening == "{" else "]"
    start = tex_space_end(text, start)
    if start >= len(text) or text[start] != opening:
        return None
    depth, at = 1, start + 1
    while at < len(text):
        char = text[at]
        if char == "\\":
            at += 2
            continue
        if char == "%":
            end = text.find("\n", at)
            at = len(text) if end < 0 else end
            continue
        if opening == "[" and char == "{":
            end = tex_group_end(text, at)
            if end is None:
                return None
            at = end
            continue
        depth += (char == opening) - (char == closing)
        at += 1
        if depth == 0:
            return at
    return None


def tex_space_end(text: str, at: int) -> int:
    while at < len(text):
        if text[at].isspace():
            at += 1
        elif text[at] == "%":
            end = text.find("\n", at)
            at = len(text) if end < 0 else end
        else:
            break
    return at


def overlay_end(text: str, start: int) -> int | None:
    if start >= len(text) or text[start] != "<":
        return None
    end = text.find(">", start + 1)
    newline = text.find("\n", start + 1)
    if end < 0 or (0 <= newline < end):
        return None
    body = text[start + 1 : end]
    if not body:
        return None
    has_action = ":" in body or "@" in body
    allowed = set("0123456789+-,.()|* ")
    if has_action:
        allowed.update("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ:@")
    return end + 1 if all(ch in allowed for ch in body) else None


def opaque_command_end(text: str, command: str, at: int) -> tuple[int, bool] | None:
    if command in {"verb", "lstinline"}:
        if command == "verb" and at < len(text) and text[at] == "*":
            at += 1
        elif command == "lstinline":
            at = tex_space_end(text, at)
            if (opt_end := tex_group_end(text, at, "[")) is not None:
                at = tex_space_end(text, opt_end)
            if at < len(text) and text[at] == "{":
                end = tex_group_end(text, at)
                return (len(text) if end is None else end, False)
        if at >= len(text):
            return None
        delim = text[at]
        at += 1
        end = text.find(delim, at)
        return (len(text) if end < 0 else end + 1, False)
    if command != "begin":
        return None
    body = tex_space_end(text, at)
    end = tex_group_end(text, body)
    if end is None:
        return None
    env = text[body + 1 : end - 1]
    base = env.rstrip("*")
    if base not in {"verbatim", "Verbatim", "BVerbatim", "LVerbatim", "lstlisting", "minted", "comment"}:
        return None
    if base == "comment":
        nl = text.find("\n", end)
        if nl < 0:
            return (len(text), True)
        pos = nl + 1
        for row in source_lines(text[pos:], keepends=True):
            pos += len(row)
            if row.rstrip("\r\n").rstrip(" ") == f"\\end{{{env}}}":
                return (pos, True)
        return (len(text), True)
    if base in {"Verbatim", "BVerbatim", "LVerbatim", "minted"}:
        nl = text.find("\n", end)
        if nl < 0:
            return (len(text), False)
        pos = nl + 1
        for row in source_lines(text[pos:], keepends=True):
            pos += len(row)
            idx = row.find("\\end{")
            if idx >= 0 and row[idx + 5 :].startswith(f"{env}}}"):
                return (pos, False)
        return (len(text), False)
    close = text.find(f"\\end{{{env}}}", end)
    return (len(text) if close < 0 else close + len(env) + 6, False)


REFERENCE_COMMANDS = {
    "ref", "eqref", "pageref", "autoref", "cref", "Cref", "cite",
    "citep", "citet", "parencite", "textcite", "autocite", "footcite",
}
NONPRINTING_KEYS = REFERENCE_COMMANDS | {
    "label", "nocite", "bibitem", "href", "includegraphics", "includesvg",
    "input", "include", "bibliography", "addbibresource", "textcolor", "color",
}


@functools.lru_cache(maxsize=32)
def nonprinting_ranges(text: str) -> list[tuple[int, int]]:
    definitions = {
        "newcommand", "renewcommand", "providecommand", "DeclareRobustCommand",
        "DeclareMathOperator", "pgfmathsetmacro", "pgfmathtruncatemacro",
        "NewDocumentCommand", "RenewDocumentCommand", "ProvideDocumentCommand",
        "DeclareDocumentCommand", "NewExpandableDocumentCommand",
        "RenewExpandableDocumentCommand", "ProvideExpandableDocumentCommand",
        "DeclareExpandableDocumentCommand", "newenvironment", "renewenvironment",
        "provideenvironment", "NewDocumentEnvironment", "RenewDocumentEnvironment",
        "ProvideDocumentEnvironment", "DeclareDocumentEnvironment",
        "def", "gdef", "edef", "xdef",
    }
    hidden, at = [], 0
    while at < len(text):
        if text[at] == "%":
            end = text.find("\n", at)
            hidden.append((at, len(text) if end < 0 else end))
            at = len(text) if end < 0 else end
            continue
        if text[at] == "<" and at > 0 and text[at - 1] in "}]":
            if (end := overlay_end(text, at)) is not None:
                hidden.append((at, end))
                at = end
                continue
            at += 1
            continue
        if text[at] != "\\":
            at += 1
            continue
        match = re.match(r"\\([A-Za-z@]+)", text[at:])
        if match is None:
            at += 2
            continue
        start, command, cmd_end = at, match[1], at + match.end()
        if (opaque := opaque_command_end(text, command, cmd_end)) is not None:
            at, is_hidden = opaque
            if is_hidden:
                hidden.append((start, at))
            continue
        if command == "newif":
            at = tex_space_end(text, cmd_end)
            if (end := tex_group_end(text, at)) is not None:
                at = end
            elif (name := re.match(r"\\[A-Za-z@]+|\\.", text[at:], re.DOTALL)) is not None:
                at += name.end()
            hidden.append((start, at))
            continue
        if command in {"let", "futurelet"}:
            at = tex_space_end(text, cmd_end)
            if at < len(text):
                at += (re.match(r"\\[A-Za-z@]+|\\.", text[at:], re.DOTALL).end() if text[at] == "\\" else 1)
            at = tex_space_end(text, at)
            if command == "let" and at < len(text) and text[at] == "=":
                at = tex_space_end(text, at + 1)
            if at < len(text):
                at += (re.match(r"\\[A-Za-z@]+|\\.", text[at:], re.DOTALL).end() if text[at] == "\\" else 1)
            hidden.append((start, at))
            continue
        at = cmd_end + (1 if cmd_end < len(text) and text[cmd_end] in "*+" else 0)
        if command in definitions:
            if command in {"def", "gdef", "edef", "xdef"}:
                while at < len(text) and text[at] != "{":
                    if text[at] == "\\":
                        at += 2
                    elif text[at] == "%":
                        at = tex_space_end(text, at)
                    else:
                        at += 1
            else:
                at = tex_space_end(text, at)
                if (end := tex_group_end(text, at)) is not None:
                    at = end
                elif (name := re.match(r"\\[A-Za-z@]+|\\.", text[at:], re.DOTALL)) is not None:
                    at += name.end()
                while (end := tex_group_end(text, at, "[")) is not None:
                    at = end
                if "Document" in command:
                    end = tex_group_end(text, at)
                    if end is None:
                        continue
                    at = end
            body_start = tex_space_end(text, at)
            end = tex_group_end(text, body_start)
            if end is None:
                if body_start < len(text) and text[body_start] == "{":
                    hidden.append((start, len(text)))
                    break
                at = body_start
                continue
            at = end
            if command.endswith("environment") or command.endswith("Environment"):
                at = tex_group_end(text, at) or at
            hidden.append((start, at))
        elif command in NONPRINTING_KEYS:
            while (end := tex_group_end(text, at, "[") or overlay_end(text, at)) is not None:
                at = end
            body = tex_space_end(text, at)
            end = tex_group_end(text, body)
            if end is not None:
                hidden.append((start if command in {"includegraphics", "includesvg"} else body, end))
                at = end
        elif command in {"begin", "end"}:
            body = tex_space_end(text, at)
            if (end := tex_group_end(text, body)) is not None:
                env = text[body + 1 : end - 1].strip()
                if command == "end" and env == "document":
                    hidden.append((start, len(text)))
                    break
                at = end
                if command == "begin":
                    if (over := overlay_end(text, tex_space_end(text, at))) is not None:
                        at = over
                    if env in {"tabularx", "tabular*"} and (w_end := tex_group_end(text, at)) is not None:
                        at = w_end
                    if (opt_end := tex_group_end(text, at, "[")) is not None:
                        at = opt_end
                    if env in {"tabularx", "tabular*", "tabular", "array"}:
                        at = tex_group_end(text, at) or at
                hidden.append((body, at))
        elif command in {"caption", "title", "subtitle", "author", "date", "institute", "frametitle", "framesubtitle", "note"}:
            at = tex_space_end(text, at)
            if (over := overlay_end(text, at)) is not None:
                at = tex_space_end(text, over)
            while (end := tex_group_end(text, at, "[")) is not None:
                hidden.append((at, end))
                at = tex_space_end(text, end)
            if command == "note" and (end := tex_group_end(text, at)) is not None:
                hidden.append((start, end))
                at = end
        elif command in {"phantom", "hphantom", "vphantom", "typeout", "message", "errmessage", "wlog"}:
            if (end := tex_group_end(text, at)) is not None:
                hidden.append((start, end))
                at = end
        elif command == "foreach":
            body = text.find("{", at)
            if body >= 0 and (end := tex_group_end(text, body)) is not None:
                hidden.append((start, end))
                at = end
        elif command == "endinput":
            hidden.append((start, len(text)))
            break
        elif (end := overlay_end(text, at)) is not None:
            hidden.append((cmd_end, end))
            at = end
    return hidden


def _merged_ranges(ranges) -> tuple[tuple[int, int], ...]:
    merged = []
    for lo, hi in sorted(ranges):
        if merged and lo <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], hi))
        else:
            merged.append((lo, hi))
    return tuple(merged)


def _range_contains(ranges: tuple[tuple[int, int], ...], position: int) -> bool:
    index = bisect.bisect_right(ranges, (position, math.inf)) - 1
    return index >= 0 and position < ranges[index][1]


@functools.lru_cache(maxsize=32)
def _nonprinting_index(text: str) -> tuple[tuple[int, int], ...]:
    return _merged_ranges(nonprinting_ranges(text))


def input_path(name: str, source_map: dict) -> Path:
    path = Path(name)
    if not path.is_absolute():
        build_dir = source_map.get("build_dir")
        if build_dir is None:
            raise RuntimeError("relative raw compiler input requires .fls PWD or --build-dir")
        path = build_dir / path
    path = path.resolve()
    if path not in source_map["inputs"].values():
        raise RuntimeError(f"raw compiler input is absent from source map: {name}")
    return path


TEX_ACCENT_RE = re.compile(
    r"""\{?\\(?:['`^"~=.](?:\{\\?([ij])\}|\\([ij])(?![A-Za-z@])|\{([A-Za-z])\}|([A-Za-z]))|[uvHckrbdt]\{\\?([A-Za-z])\})\}?"""
)


def strip_tex_accents(text: str) -> str:
    return TEX_ACCENT_RE.sub(lambda m: next(g for g in m.groups() if g is not None), text)


def line_has_word(line: str, word: str) -> bool:
    text = source_line_text(line)
    hidden = _nonprinting_index(text)
    text = "".join(" " if _range_contains(hidden, at) else char for at, char in enumerate(text))
    text = strip_tex_accents(text).replace("$", "")
    text = re.sub(r"\\\\|\\[A-Za-z@]+\*?|_+", " ", text)
    needle = normalized_words(word)
    return len(needle) == 1 and needle[0] in normalized_words(text)


def metadata_spans(text: str) -> list[tuple[int, int]]:
    spans, start = [], 0
    for raw_row in source_lines(text, keepends=True):
        visible = source_line_text(raw_row.rstrip("\r\n"))
        for match in re.finditer(r"\\(?:title|subtitle|author|date|institute)(?![A-Za-z@])\*?", visible):
            at = start + match.end()
            while (opt_end := tex_group_end(text, at, "[")) is not None:
                at = opt_end
            body = tex_space_end(text, at)
            if (end := tex_group_end(text, body)) is not None:
                spans.append((body + 1, end))
        start += len(raw_row)
    return spans


@functools.lru_cache(maxsize=32)
def _ignored_env_ranges(text: str) -> tuple[tuple[int, int], ...]:
    ignored, scan = [], 0
    while scan < len(text):
        if text[scan] == "%":
            end = text.find("\n", scan)
            scan = len(text) if end < 0 else end
        elif text[scan] == "\\":
            match = re.match(r"\\([A-Za-z@]+|.)", text[scan:], re.DOTALL)
            if match is None:
                scan += 1
                continue
            end = scan + match.end()
            if match[1].isalpha() or "@" in match[1]:
                if (opaque := opaque_command_end(text, match[1], end)) is not None:
                    ignored.append((scan, opaque[0]))
                    scan = opaque[0]
                    continue
            scan = end
        else:
            scan += 1
    ignored.extend(nonprinting_ranges(text))
    return _merged_ranges(ignored)


@functools.lru_cache(maxsize=32)
def _environment_ranges(text: str, environment: str) -> tuple[tuple[int, int], ...]:
    if f"{{{environment}}}" not in text:
        return ()
    ignored = _ignored_env_ranges(text)
    ranges, start, line_offset = [], None, 0
    begin_re = re.compile(rf"begin\s*\{{{re.escape(environment)}\}}")
    end_re = re.compile(rf"end\s*\{{{re.escape(environment)}\}}")
    for index, raw_row in enumerate(source_lines(text, keepends=True)):
        visible = source_line_text(raw_row.rstrip("\r\n"))
        cursor = 0
        while (rel := visible.find("\\", cursor)) >= 0:
            slash = rel
            abs_pos = line_offset + slash
            cursor = slash + 1
            if visible.startswith("\\", cursor):
                cursor += 1
                continue
            if _range_contains(ignored, abs_pos):
                continue
            rest = visible[cursor:]
            if (m := begin_re.match(rest)) is not None:
                cursor += m.end()
                if start is not None:
                    return tuple(ranges)
                start = index
            elif (m := end_re.match(rest)) is not None:
                cursor += m.end()
                if start is not None:
                    ranges.append((start, index + 1))
                    start = None
                elif environment == "frame":
                    ranges.append((index, index + 1))
        line_offset += len(raw_row)
    return tuple(ranges)


def environment_bounds(text: str, line_number: int, environment: str) -> tuple[int, int] | None:
    anchor = line_number - 1
    ranges = _environment_ranges(text, environment)
    index = bisect.bisect_right(ranges, (anchor, math.inf)) - 1
    while index > 0 and anchor < ranges[index - 1][1]:
        index -= 1
    if index >= 0 and anchor < ranges[index][1]:
        return ranges[index][0], ranges[index][1] - 1
    return None


def frame_bounds(text: str, line_number: int) -> tuple[int, int] | None:
    return environment_bounds(text, line_number, "frame")


DISPLAY_MATH_ENVS = (
    "equation",
    "equation*",
    "align",
    "align*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "flalign",
    "flalign*",
    "alignat",
    "alignat*",
    "displaymath",
)


def collected_env_bounds(text: str, line_number: int) -> tuple[int, int] | None:
    if bounds := (frame_bounds(text, line_number) or environment_bounds(text, line_number, "tabularx")):
        return bounds
    for env in DISPLAY_MATH_ENVS:
        if bounds := environment_bounds(text, line_number, env):
            return bounds
    return None


def collected_env_closing_line(text: str, line: int, column: int = 1) -> int | None:
    for env in ("frame", "tabularx", *DISPLAY_MATH_ENVS):
        bounds = environment_bounds(text, line, env)
        if bounds is None:
            continue
        if line - 1 == bounds[0]:
            rows = source_lines(text, keepends=True)
            line_start = sum(len(r) for r in rows[: bounds[0]])
            visible = source_line_text(rows[bounds[0]].rstrip("\r\n"))
            ignored = _ignored_env_ranges(text)
            begin_col = None
            for m in re.finditer(rf"(?<!\\)(?:\\\\)*\\begin\s*\{{{re.escape(env)}\}}", visible):
                slash = m.end() - len(m.group(0).lstrip("\\")) - 1
                if not _range_contains(ignored, line_start + slash):
                    begin_col = slash
                    break
            if begin_col is not None and visible[:begin_col].strip() and max(0, column - 1) < begin_col:
                continue
        return bounds[1] + 1
    return None


def caption_argument(text: str, at: int) -> tuple[int, int, int] | None:
    if at < len(text) and text[at] == "*":
        at += 1
    at = tex_space_end(text, at)
    if at < len(text) and text[at] == "[":
        at = tex_group_end(text, at, "[")
        if at is None:
            return None
    body = tex_space_end(text, at)
    end = tex_group_end(text, body)
    return (body + 1, end - 1, end) if end is not None else None


def _caption_arguments(text: str, position: int | None = None) -> Iterator[tuple[int, int, int]]:
    if "\\caption" not in text:
        return
    hidden = nonprinting_ranges(text)
    h_idx, at, candidate = 0, 0, None
    limit = len(text) - 1 if position is None else position
    while at <= limit:
        while h_idx < len(hidden) and hidden[h_idx][1] <= at:
            h_idx += 1
        if h_idx < len(hidden) and hidden[h_idx][0] <= at:
            at = hidden[h_idx][1]
            if position is not None and position < at:
                return
            continue
        if text[at] == "%":
            end = text.find("\n", at)
            at = len(text) if end < 0 else end
            if position is not None and position < at:
                return
        elif text[at] == "\\":
            match = re.match(r"\\([A-Za-z]+|.)", text[at:], re.DOTALL)
            if match is None:
                return
            name, at = match[1], at + match.end()
            if (opaque := opaque_command_end(text, name, at)) is not None:
                at = opaque[0]
                if position is not None and position < at:
                    return
            elif name == "caption" and (arg := caption_argument(text, at)) is not None:
                body_start, body_stop, arg_end = arg
                if position is None:
                    yield arg
                    at = arg_end
                elif body_start <= position < body_stop:
                    # A cursor inside the body still needs its lexical visibility checked.
                    candidate, at = arg, body_start
                else:
                    at = arg_end
        else:
            at += 1
    if candidate is not None:
        yield candidate


def caption_bounds(text: str, line_number: int) -> tuple[int, int] | None:
    target = line_number - 1
    if target < 0:
        return None
    for body_start, _, arg_end in _caption_arguments(text):
        first = text.count("\n", 0, body_start)
        last = first + text.count("\n", body_start, arg_end)
        if first <= target <= last:
            return first, last
        if first > target:
            break
    return None


def caption_closing_line(text: str, line: int, column: int) -> int | None:
    rows = source_lines(text, keepends=True)
    if not (1 <= line <= len(rows) and column >= 1):
        return None
    row = rows[line - 1].rstrip("\r\n")
    if column - 1 >= len(row):
        return None
    pos = sum(len(r) for r in rows[: line - 1]) + (column - 1)
    for _, body_stop, _ in _caption_arguments(text, pos):
        return text.count("\n", 0, body_stop) + 1
    return None


def fragile_frame(text: str, starts: list[int], lines: list[str], bounds: tuple[int, int]) -> bool:
    row = source_line_text(lines[bounds[0]])
    row_start = starts[bounds[0]]
    at = None
    for match in re.finditer(r"(?<!\\)(?:\\\\)*\\begin", row):
        end = tex_group_end(text, row_start + match.end())
        if end is not None and text[tex_space_end(text, row_start + match.end()) + 1 : end - 1] == "frame":
            at = end
            break
    if at is None:
        return False
    at = tex_space_end(text, at)
    if at < len(text) and text[at] == "<":
        close = text.find(">", at)
        if close < 0:
            return False
        at = tex_space_end(text, close + 1)
    end = tex_group_end(text, at, "[")
    if end is None:
        return False
    options = text[tex_space_end(text, at) + 1 : end - 1]
    return any(
        (opt := source_line_text(line).strip()) == "fragile" or opt.startswith("fragile=")
        for part in options.split(",")
        for line in source_lines(part)
    )


MATH_COMMANDS = {
    "alpha", "beta", "gamma", "sum", "xi", "eta", "Omega", "theta", "lambda",
    "Delta", "mu", "nu", "Phi", "kappa", "sigma",
}


def same_point_value(key: str, value, expected) -> bool:
    if key == "page":
        return type(value) is int and value == expected
    if type(value) not in (int, float):
        return False
    try:
        if not math.isfinite(value) or not math.isfinite(expected):
            return False
        return struct.pack("!f", value) == struct.pack("!f", expected)
    except (OverflowError, struct.error):
        return False


@functools.lru_cache(maxsize=32)
def _math_ranges(text: str):
    hidden = _nonprinting_index(text)
    inline, opening, expansion = [], None, False
    closers = {"$": "$", "$$": "$$", "\\(": "\\)", "\\[": "\\]"}
    for token in re.finditer(r"\\(?:[A-Za-z@]+|.)|\$\$?", text, re.DOTALL):
        if _range_contains(hidden, token.start()):
            continue
        if opening is None:
            if token[0] in closers:
                opening, expansion = token, False
        elif token[0] == closers[opening[0]]:
            inline.append((opening.end(), token.start(), expansion))
            opening = None
        elif token[0].startswith("\\") and (token[0][1:].isalpha() or "@" in token[0]):
            expansion = True
    display = _merged_ranges(
        (lo + 1, hi + 1) for env in DISPLAY_MATH_ENVS for lo, hi in _environment_ranges(text, env)
    )
    return tuple(inline), display


def math_expansion_at(text: str, position: int) -> bool:
    """Recognize conventional math context, not the producer's symbol decoding."""
    inline, display = _math_ranges(text)
    index = bisect.bisect_right(inline, (position, math.inf)) - 1
    if index >= 0 and position < inline[index][1]:
        return inline[index][2]
    return _range_contains(display, text.count("\n", 0, position) + 1)


@functools.lru_cache(maxsize=32)
def lexical_words(text: str) -> tuple[dict[str, list[tuple[int, int]]], list[tuple[int, int]], set[str], list[tuple[int, int]]]:
    """Index literal spellings; unknown macro arguments remain unverified."""
    hidden = nonprinting_ranges(text)
    hidden_index = _nonprinting_index(text)
    uncertain = []
    opaque = [
        (lo, hi) for lo, hi in hidden
        if re.match(r"\\(?:note|(?:re)?newcommand|providecommand|DeclareRobustCommand|DeclareMathOperator|pgfmath(?:trunc)?setmacro|[gex]?def|(?:re)?newenvironment|provideenvironment|(?:New|Renew|Provide|Declare)(?:Expandable)?Document(?:Command|Environment))\b", text[lo:])
    ]
    defined = set()
    for lo, _ in hidden:
        definition = re.match(r"\\(?:(?:re)?newcommand|providecommand|DeclareRobustCommand|[gex]?def|let|futurelet|(?:New|Renew|Provide|Declare)(?:Expandable)?DocumentCommand)\*?", text[lo:])
        if definition:
            at = tex_space_end(text, lo + definition.end())
            at = tex_space_end(text, at + 1) if at < len(text) and text[at] == "{" else at
            name = re.match(r"\\([A-Za-z@]+|.)", text[at:], re.DOTALL)
            if name:
                defined.add(name[1])
    # Conventional syntax only; local redefinitions and unknown calls stay opaque.
    prose = {
        "begin", "end", "caption", "title", "subtitle", "author", "date", "institute",
        "frametitle", "framesubtitle", "section", "subsection", "subsubsection",
        "textbf", "textit", "texttt", "textrm", "textsf", "textnormal", "emph",
        "underline", "footnote", "footnotetext", "href", "textcolor", "item",
        "centering", "hline", "par", "relax", "noindent", "maketitle", "titlepage",
    } | MATH_COMMANDS | NONPRINTING_KEYS
    for command in re.finditer(r"\\([A-Za-z@]+|.)\*?", text, re.DOTALL):
        if command[1][0] not in string.ascii_letters and command[1][0] != "@" and command[1] not in defined:
            continue
        if _range_contains(hidden_index, command.start()):
            continue
        if (command[1] in prose or TEX_ACCENT_RE.match(text, command.start())) and command[1] not in defined:
            continue
        uncertain.append((command.start(), command.end()))
        at = tex_space_end(text, command.end())
        while (end := tex_group_end(text, at, "[") or tex_group_end(text, at)) is not None:
            uncertain.append((at, end))
            at = tex_space_end(text, end)
    words = defaultdict(list)
    for token in re.finditer(rf"(?P<word>(?:[^\W_]|{TEX_ACCENT_RE.pattern})+)|\\(?:[A-Za-z@]+|.)\*?", text, re.DOTALL):
        if token["word"] is None:
            continue
        if _range_contains(hidden_index, token.start()):
            continue
        normalized = normalized_words(strip_tex_accents(token[0]))
        if len(normalized) == 1:
            words[normalized[0]].append(token.span())
    opaque_words = {
        word for lo, hi in opaque + uncertain for word in normalized_words(strip_tex_accents(text[lo:hi]))
    }
    known_nonprinting = [span for span in hidden if span not in opaque]
    return dict(words), uncertain + opaque, opaque_words, known_nonprinting


def inverse(
    pdf: Path,
    source: Path,
    word_points: dict[int, list[WordPoint]],
    best_guess: bool = False,
) -> Iterator[dict]:
    points = []
    for page, words in word_points.items():
        for word, box, page_height in words:
            # Submit exact f32 values: decimal midpoints can round differently in Rust's JSON parser.
            x = struct.unpack("!f", struct.pack("!f", (box[0] + box[2]) / 2))[0]
            y = struct.unpack("!f", struct.pack("!f", (box[1] + box[3]) / 2))[0]
            points.append({"page": page, "x": x, "y": y, "word": word, "box": box, "page_height": page_height})
    if not points:
        return
    extra = ["--synctex-best-guess"] if best_guess else []
    result = subprocess.run(
        [str(pdfterm_bin()), str(pdf), "--synctex-edit-batch", *extra],
        input="".join(json.dumps({key: point[key] for key in ("page", "x", "y")}) + "\n" for point in points),
        capture_output=True,
        text=True,
        timeout=max(30, len(points) * 2),
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"pdfterm --synctex-edit-batch exited {result.returncode}: {result.stderr.strip()[:500]}")
    rows = []
    for line in result.stdout.splitlines():
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as error:
            raise RuntimeError(f"invalid batch JSON: {error}: {line[:200]}") from error
        if not isinstance(rows[-1], dict) or type(rows[-1].get("ok")) is not bool:
            raise RuntimeError("batch response must be an object with a boolean ok field")
    if len(rows) != len(points):
        raise RuntimeError(f"batch returned {len(rows)} results for {len(points)} points")
    source_map = read_source_map(pdf)
    known_inputs = set(source_map["inputs"].values())
    sources = {}

    def source_info(path: Path) -> tuple[str, list[str], list[tuple[int, int]], list[int], list[tuple[int, int]]]:
        if path not in sources:
            with path.open(newline="") as stream:
                text = stream.read()
            starts, at = [], 0
            for row in source_lines(text, keepends=True):
                starts.append(at)
                at += len(row)
            sources[path] = (
                text,
                source_lines(text),
                nonprinting_ranges(text),
                starts,
                metadata_spans(text),
            )
        return sources[path]

    @functools.lru_cache(maxsize=32)
    def candidate_anchor(page: int, raw: tuple[int, str]):
        raw_file = input_path(raw[1], source_map)
        expected_file, expected_anchor = raw_file, raw[0]
        bounds = None
        remapped = False
        generated = is_generated_input(raw_file, pdf.stem)
        if generated:
            raw_tag = next(
                (
                    tag
                    for tag in source_map["page_inputs"].get(page, set())
                    if source_map["inputs"].get(tag) == raw_file
                ),
                None,
            )
            is_pdf_tex = raw_file.suffix.lower() in GENERATED_GRAPHICS_EXTENSIONS
            origin = (
                source_map["host_origins"].get((page, raw_tag))
                if is_pdf_tex
                else source_map["origins"].get(page)
            )
            generated_on_page = raw_tag is not None
            # Adjacent source mentions are not compiler execution ownership.
            if origin and generated_on_page and origin[0] in source_map["inputs"]:
                original = source_map["inputs"][origin[0]]
                if not is_compiler_sidecar_or_graphics(original):
                    original_text, original_lines, _, original_starts, *_ = source_info(original)
                    original_bounds = frame_bounds(original_text, origin[1])
                    if raw_file.suffix != ".vrb":
                        if 1 <= origin[1] <= len(original_lines):
                            expected_file, expected_anchor = original, origin[1]
                            bounds, remapped = original_bounds, (original != raw_file)
                    elif (
                        original_bounds
                        and origin[1] == original_bounds[1] + 1
                        and fragile_frame(original_text, original_starts, original_lines, original_bounds)
                    ):
                        expected_file, expected_anchor = original, origin[1]
                        bounds, remapped = original_bounds, True
        else:
            raw_text = source_info(expected_file)[0]
            bounds = (
                caption_bounds(raw_text, raw[0])
                or collected_env_bounds(raw_text, raw[0])
                or environment_bounds(raw_text, raw[0], "tikzpicture")
            )
        return raw, expected_file, expected_anchor, bounds, remapped, generated

    @functools.lru_cache(maxsize=32)
    def collected_boxes(page: int) -> tuple:
        boxes = []
        queried = set()
        for tag, line in sorted(source_map["page_lines"].get(page, set())):
            path = source_map["inputs"].get(tag)
            if path is None:
                raise RuntimeError(f"SyncTeX page {page} references unknown input tag {tag}")
            if is_compiler_sidecar_or_graphics(path) or (path, line) in queried:
                continue
            text, lines, *_ = source_info(path)
            if not 1 <= line <= len(lines):
                continue
            if "\\end" not in source_line_text(lines[line - 1]):
                continue
            collected = collected_env_bounds(text, line)
            if collected is None or line != collected[1] + 1:
                continue
            queried.add((path, line))
            for record in raw_forward_records(pdf, path, line):
                if record["Page"] == page:
                    boxes.append((line, path, record["h"], record["v"], record["W"], record["H"]))
        return tuple(boxes)


    def safe_raw_inverse(point: dict) -> tuple[list[tuple[int, str]] | None, Exception | None]:
        try:
            return raw_inverse(pdf, point["x"], point["y"], point["page"]), None
        except (OSError, subprocess.TimeoutExpired, RuntimeError) as error:
            return None, error

    with concurrent.futures.ThreadPoolExecutor() as pool:
        raw_results = list(pool.map(safe_raw_inverse, points))

    for point, batch, (raw_records, raw_error) in zip(points, rows, raw_results):
        page, word = point["page"], point["word"]
        entry = {
            "direction": "inverse",
            "page": page,
            "word": word,
            "word_box": point["box"],
            "page_height": point["page_height"],
            "raw_anchor": None,
            "location": batch.get("location"),
            "warning": batch.get("warning"),
            "ok": False,
        }
        if any(key in batch and not same_point_value(key, batch[key], point[key]) for key in ("page", "x", "y")):
            entry["error"] = "batch response point identity differs from the submitted point"
            yield entry
            continue
        if "input" in batch:
            try:
                echoed = json.loads(batch["input"])
            except (TypeError, ValueError):
                echoed = None
            if not isinstance(echoed, dict) or set(echoed) != {"page", "x", "y"} or any(
                not same_point_value(key, echoed[key], point[key]) for key in ("page", "x", "y")
            ):
                entry["error"] = "batch error input differs from the submitted point"
                yield entry
                continue
        # Warnings/coarse results do not exempt file, line or remap identity.
        if raw_error is not None:
            entry["error"] = f"raw SyncTeX edit failed: {raw_error}"
            yield entry
            continue
        if not raw_records:
            error = batch.get("error", "")
            if not batch.get("ok") and "synctex edit returned no match" in error:
                entry.update(abstained=True, reason="raw_synctex_unmapped", error=error)
            else:
                entry["error"] = "raw SyncTeX edit returned no match"
            yield entry
            continue
        candidates = [candidate_anchor(page, item) for item in raw_records]
        point_anchors = sorted({
            (line, str(path)) for line, path, h, v, width, height in collected_boxes(page)
            if h <= point["x"] <= h + width and v - height <= point["y"] <= v
        })
        entry["point_anchors"] = [{"line": line, "file": path} for line, path in point_anchors]
        candidates.extend(candidate_anchor(page, item) for item in point_anchors if item not in raw_records)
        raw, expected_file, expected_anchor, bounds, remapped, generated = candidates[0]
        entry["raw_anchor"] = {"line": raw[0], "file": raw[1]}
        entry["raw_anchors"] = [{"line": item[0], "file": item[1]} for item in raw_records]
        entry["expected_anchors"] = [
            {"file": str(cand[1]), "line": cand[2], "remapped": cand[4]} for cand in candidates
        ]
        if not batch.get("ok"):
            error = batch.get("error", "batch returned no result")
            if "no text near point" in error or "hit-test failed" in error:
                entry.update(abstained=True, reason="pdfium_hit_unsupported", error=error)
            elif not best_guess and "ambiguous inverse search:" in error and len(candidates) > 1:
                entry.update(abstained=True, reason="ambiguous_inverse_anchors", error=error)
            else:
                entry["error"] = error
            yield entry
            continue
        location = batch.get("location")
        entry["expected_anchor"] = {"file": str(expected_file), "line": expected_anchor, "remapped": remapped}
        if not (generated and not remapped) and not 1 <= expected_anchor <= len(source_info(expected_file)[1]):
            entry["error"] = "page-proven source anchor is outside its source file"
            yield entry
            continue
        if not isinstance(location, dict):
            entry["error"] = "successful batch has no source location"
            yield entry
            continue
        if not isinstance(location.get("file"), str) or type(location.get("line")) is not int or type(location.get("precise")) is not bool:
            entry["error"] = "batch source location has invalid file, line or precision fields"
            yield entry
            continue
        resolved_file = Path(location["file"]).resolve()
        resolved_line = location["line"]
        precise = location["precise"]
        warning = batch.get("warning") or ""
        if best_guess:
            for baseline in (
                "best-guess navigation: assuming saved source is unchanged since compilation",
                "best-guess navigation: no literal source position was qualified; using the compiler line anchor",
            ):
                warning = warning.replace(baseline, "")
            warning = warning.strip("; ")
        if precise is False:
            raw_path = input_path(raw[1], source_map)
            unremapped_pdf_tex_ok = (
                raw_path.suffix.lower() in GENERATED_GRAPHICS_EXTENSIONS
                and resolved_file == raw_path
                and resolved_line == raw[0]
            )
            if not any(resolved_file == cand[1] and resolved_line == cand[2] for cand in candidates) and not unremapped_pdf_tex_ok:
                entry["error"] = "coarse inverse result changed the source file or page-proven anchor"
                yield entry
                continue
            entry["mapping_ok"] = True
            entry.update(abstained=True, reason="resolver_warning" if warning else "coarse_source_refinement")
            yield entry
            continue
        if generated and not remapped:
            entry["error"] = "precise fragile result has no original frame provenance"
            yield entry
            continue
        if resolved_file not in known_inputs:
            entry["error"] = "inverse result selected a file absent from the source map"
            yield entry
            continue
        resolved_text, resolved_lines, hidden, starts, meta_spans = source_info(resolved_file)
        if not 1 <= resolved_line <= len(resolved_lines):
            entry["error"] = "SyncTeX source line is invalid"
            yield entry
            continue
        resolved_row = resolved_lines[resolved_line - 1]
        try:
            if any(type(location.get(key)) is not int for key in ("byte_column", "column", "column_char")):
                raise ValueError("source columns must be integers")
            byte = location["byte_column"]
            if byte < 0 or byte > len(resolved_row.encode("utf-8")):
                raise ValueError("source byte column is outside its line")
            prefix = resolved_row.encode("utf-8")[:byte].decode("utf-8")
            if location["column"] != len(prefix.encode("utf-16-le")) // 2 + 1 or location["column_char"] != len(prefix) + 1:
                raise ValueError("source Unicode columns disagree")
        except (UnicodeError, ValueError) as error:
            entry["error"] = str(error)
            yield entry
            continue
        absolute_column = starts[resolved_line - 1] + len(prefix)
        literals, uncertain, opaque_words, known_nonprinting = lexical_words(resolved_text)
        if any(start <= absolute_column < end for start, end in known_nonprinting):
            entry["error"] = "precise inverse result selected a known nonprinting argument or definition"
            yield entry
            continue
        pdf_word = batch.get("pdf_word")
        entry["pdfium_word"] = pdf_word
        expected_lines = source_info(expected_file)[1]
        # Metadata must have its own compiler context, not just a frame or margin.
        metadata = (
            resolved_file == expected_file == source_map["inputs"][1]
            and any(lo <= absolute_column < hi for lo, hi in meta_spans)
            and (
                any(lo <= starts[expected_anchor - 1] < hi for lo, hi in meta_spans)
                or any(
                    re.search(r"\\(?:maketitle|titlepage)\b", source_line_text(row))
                    for row in expected_lines[max(0, expected_anchor - 5):expected_anchor + 4]
                )
            )
        )
        if resolved_file not in {cand[1] for cand in candidates}:
            entry["error"] = "inverse result changed the page-proven source file"
            yield entry
            continue
        in_scope = any(
            resolved_file == cand[1] and (
                cand[3][0] + 1 <= resolved_line <= cand[3][1] + 1
                if cand[3] else abs(resolved_line - cand[2]) <= 4
            )
            for cand in candidates
        )
        if not metadata and not in_scope:
            entry["error"] = "inverse result escaped the page-proven source scope"
            yield entry
            continue
        entry["mapping_ok"] = True
        scopes = {(cand[1], cand[3] or (cand[2], cand[2])) for cand in candidates}
        if len(scopes) > 1:
            entry.update(abstained=True, reason="ambiguous_compiler_ownership")
        else:
            needle = normalized_words(word)
            occurrences = literals.get(needle[0], []) if len(needle) == 1 else []
            opaque_word = len(needle) == 1 and needle[0] in opaque_words
            scope_first, scope_last = bounds or (max(0, expected_anchor - 5), min(len(resolved_lines) - 1, expected_anchor + 3))
            scope_lo = starts[scope_first]
            scope_hi = starts[scope_last] + len(resolved_lines[scope_last])
            unknown_scope = any(scope_lo <= lo < scope_hi for lo, _ in uncertain)
            if unknown_scope or any(lo <= absolute_column < hi for lo, hi in uncertain) or opaque_word:
                entry.update(abstained=True, reason="unverified_execution_ownership")
            elif len(occurrences) > 1:
                entry.update(abstained=True, reason="ambiguous_repeated_occurrence")
            elif len(occurrences) == 1 and occurrences[0][0] == absolute_column:
                if warning:
                    entry.update(abstained=True, reason="resolver_warning")
                elif not pdf_word:
                    entry.update(abstained=True, reason="pdfium_selected_no_word")
                elif normalized_words(pdf_word) != needle:
                    entry.update(abstained=True, reason="pdfium_word_mismatch")
                else:
                    entry.update(
                        ok=True, precision_checked=True,
                        match="document_metadata" if metadata else "literal_frame_word" if bounds else "literal_source",
                        ownership="unique_lexical_literal_not_execution_trace",
                    )
            elif not occurrences and (
                math_expansion_at(resolved_text, absolute_column)
                or (
                    (command := re.match(r"\\([A-Za-z@]+)", resolved_row[len(prefix):]))
                    and command[1] in MATH_COMMANDS | REFERENCE_COMMANDS
                )
            ):
                # Actual math/reference expansion is evidence; a wrong plain word is not.
                entry.update(abstained=True, reason="precision_not_literal_math_or_expansion")
            else:
                entry["error"] = "precise inverse column does not select the PDF word"
        yield entry


def compiler_word_box(page, rect) -> tuple[float, float, float, float]:
    """PyMuPDF words are unrotated crop coordinates, not page.rect coordinates."""
    # PyMuPDF's transformation_matrix drops crop translation on rotated pages.
    crop, media = page.cropbox, page.mediabox
    return rect.x0 + crop.x0, rect.y0 + crop.y0 - media.y0, rect.x1 + crop.x0, rect.y1 + crop.y0 - media.y0


def painted_word_points(
    pdf: Path, page_count: int, page_limit: int | None, word_limit: int,
    rng: random.Random, specific_pages: list[int] | None,
) -> tuple[dict[int, list[WordPoint]], dict]:
    """Return on-page words with painted glyphs (not an occlusion proof)."""
    if page_limit == 0 and specific_pages is None:
        return {}, {
            "eligible_pages": None,
            "pages_without_eligible_words": None,
            "selected_pages": [],
            "total": None,
            "selected": 0,
            "raw_extracted_words": None,
            "eligibility_filtered_words": None,
            "page_cap_unselected": None,
            "word_cap_unselected": 0,
            "eligible_words_by_selected_page": {},
            "selected_pages_without_eligible_words": [],
        }
    try:
        import pymupdf
    except ImportError as error:
        raise SystemExit("inverse word probes require PyMuPDF in the Python environment") from error
    selected = {}
    eligible_by_page = {}
    with pymupdf.open(pdf) as document:
        if document.page_count != page_count:
            raise SystemExit("PyMuPDF page count differs from pdfinfo")
        raw_extracted = 0
        for page_number, page in enumerate(document, 1):
            words = []
            cells = defaultdict(list)
            seen_glyphs = set()
            order = 0
            for span in page.get_texttrace():
                if span["type"] not in (0, 1) or span["opacity"] <= 0:
                    continue
                for codepoint, _, _, bounds in span["chars"]:
                    key = (codepoint, *bounds)
                    if not 0 < codepoint <= 0x10FFFF or key in seen_glyphs:
                        continue
                    seen_glyphs.add(key)
                    x, y = (bounds[0] + bounds[2]) / 2, (bounds[1] + bounds[3]) / 2
                    cells[(math.floor(x / 32), math.floor(y / 32))].append((order, chr(codepoint), x, y))
                    order += 1
            text_words = page.get_text("words")
            raw_extracted += len(text_words)
            for x0, y0, x1, y1, word, *_ in text_words:
                rect = pymupdf.Rect(x0, y0, x1, y1)
                if rect.width <= 0 or rect.height <= 0:
                    continue
                rotated = rect * page.rotation_matrix
                if not page.rect.contains(rotated.tl + (rotated.br - rotated.tl) / 2):
                    continue
                glyphs = [
                    glyph
                    for cx in range(math.floor(x0 / 32), math.floor(x1 / 32) + 1)
                    for cy in range(math.floor(y0 / 32), math.floor(y1 / 32) + 1)
                    for glyph in cells.get((cx, cy), ())
                    if x0 <= glyph[2] <= x1 and y0 <= glyph[3] <= y1
                ]
                painted_word = "".join(glyph[1] for glyph in sorted(glyphs))
                if normalized_words(word) and normalized_words(word) == normalized_words(painted_word):
                    words.append((word, compiler_word_box(page, rect), page.mediabox.height))
            if words:
                eligible_by_page[page_number] = words
    candidates = sorted(eligible_by_page)
    if specific_pages is not None:
        chosen = sorted(specific_pages)
    else:
        chosen = candidates if page_limit is None else sorted(rng.sample(candidates, min(page_limit, len(candidates))))
    for page in chosen:
        words = eligible_by_page.get(page, [])
        selected[page] = rng.sample(words, min(word_limit, len(words))) if word_limit else words
    eligible_total = sum(map(len, eligible_by_page.values()))
    coverage = {
        "eligible_pages": len(eligible_by_page),
        "pages_without_eligible_words": page_count - len(eligible_by_page),
        "selected_pages": chosen,
        "total": eligible_total,
        "raw_extracted_words": raw_extracted,
        "eligibility_filtered_words": raw_extracted - eligible_total,
        "selected": sum(map(len, selected.values())),
        "page_cap_unselected": sum(len(eligible_by_page[page]) for page in set(candidates) - set(chosen)),
        "word_cap_unselected": sum(len(eligible_by_page.get(page, [])) - len(selected[page]) for page in chosen),
        "eligible_words_by_selected_page": {page: len(eligible_by_page.get(page, [])) for page in chosen},
        "selected_pages_without_eligible_words": [page for page in chosen if not eligible_by_page.get(page)],
    }
    return selected, coverage


def pdf_page_count(pdf: Path) -> int:
    result = subprocess.run(
        ["pdfinfo", str(pdf)], capture_output=True, text=True, errors="replace", timeout=30, check=False
    )
    if result.returncode:
        raise SystemExit(f"pdfinfo: {result.stderr.strip()[:200]}")
    match = re.search(r"^Pages:\s+(\d+)$", result.stdout, re.MULTILINE)
    if not match:
        raise SystemExit("pdfinfo returned no page count")
    return int(match.group(1))


def pdfterm_bin() -> Path:
    if PDFTERM_BINARY.is_file():
        return PDFTERM_BINARY
    raise SystemExit(f"missing {PDFTERM_BINARY}; build pdfterm or pass --binary PATH")


def validate_sources(pdf: Path, companion: Path, source_map: dict, source: Path, allow_stale: bool) -> None:
    if source not in source_map["inputs"].values():
        raise RuntimeError("--source is absent from the compiler inputs")
    inputs = {
        path for path in source_map["inputs"].values()
        if not is_compiler_sidecar_or_graphics(path)
    } | {source}
    built = min(pdf.stat().st_mtime_ns, companion.stat().st_mtime_ns)
    for path in inputs:
        if not path.is_file():
            raise RuntimeError(f"missing compiler source: {path}")
        if not allow_stale and path.stat().st_mtime_ns > built:
            raise RuntimeError(f"PDF/SyncTeX pair predates compiler source {path}; rebuild or pass --allow-stale")


def main() -> None:
    global PDFTERM_BINARY, BUILD_DIRECTORY
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pdf", type=Path)
    parser.add_argument("--binary", type=Path, default=PDFTERM_BINARY, help="pdfterm executable to probe")
    parser.add_argument(
        "--source",
        type=Path,
        help="source file to sample (default: main input recorded in SyncTeX)",
    )
    parser.add_argument("--build-dir", type=Path, help="actual compiler working directory (otherwise recorded .fls PWD)")
    parser.add_argument("--require-precision-word", action="append", default=[], help="known printed positive control; require a successful literal precision check (repeatable)")
    parser.add_argument("--page", type=int, action="append", help="probe this one-based page; repeatable")
    parser.add_argument("--lines", type=int, default=100, help="source lines to sample")
    parser.add_argument("--pages", type=int, help="seed-selected inverse page cap; default probes all pages")
    parser.add_argument("--words", type=int, default=0, help="max words per selected page; 0 means all")
    parser.add_argument("--seed", type=int, default=20260922)
    parser.add_argument("--allow-stale", action="store_true", help="accept an older PDF/SyncTeX pair")
    parser.add_argument("--best-guess", action="store_true", help="pass --synctex-best-guess to pdfterm")
    args = parser.parse_args()
    PDFTERM_BINARY = args.binary.resolve()
    BUILD_DIRECTORY = args.build_dir.resolve() if args.build_dir else None
    if (
        min(args.lines, args.words) < 0
        or (args.pages is not None and args.pages < 0)
        or (args.page is not None and (args.pages is not None or min(args.page) < 1 or len(set(args.page)) != len(args.page)))
    ):
        parser.error("counts must be nonnegative; --page values must be unique and cannot combine with --pages")

    pdf = args.pdf.resolve()
    companion = next(
        (
            path
            for path in (pdf.with_suffix(".synctex.gz"), pdf.with_suffix(".synctex"))
            if path.exists()
        ),
        None,
    )
    if not pdf.is_file() or companion is None:
        raise SystemExit("missing PDF or matching SyncTeX sidecar")
    try:
        source_map = read_source_map(pdf)
        source_file = (args.source or source_map["inputs"][1]).resolve()
        validate_sources(pdf, companion, source_map, source_file, args.allow_stale)
        with source_file.open(newline="") as stream:
            source = stream.read()
    except (OSError, RuntimeError, UnicodeError) as error:
        raise SystemExit(str(error)) from error

    rng = random.Random(args.seed)
    forward_positions = sample_source_positions(source, rng, args.lines)
    page_count = pdf_page_count(pdf)
    if args.page and max(args.page) > page_count:
        parser.error(f"--page must not exceed the PDF's {page_count} pages")
    word_points, inverse_coverage = painted_word_points(
        pdf, page_count, args.pages, args.words, rng, args.page
    )
    visible_words = pdf_visible_words(pdf) if forward_positions else []
    if visible_words and len(visible_words) != page_count:
        raise SystemExit("pdftotext page count differs from pdfinfo")

    results = list(forward(pdf, source_file, source, forward_positions, page_count, visible_words, args.best_guess, source_map))
    batch_error = None
    try:
        results.extend(inverse(pdf, source_file, word_points, args.best_guess))
    except (OSError, subprocess.TimeoutExpired, RuntimeError) as error:
        batch_error = str(error)
    failures = sum(not probe["ok"] and not probe.get("abstained") for probe in results) + bool(batch_error)
    inverse_results = [probe for probe in results if probe.get("direction") == "inverse"]
    reasons = {}
    for probe in inverse_results:
        if probe.get("abstained"):
            reason = probe["reason"]
            reasons[reason] = reasons.get(reason, 0) + 1
    if inverse_coverage["page_cap_unselected"]:
        reasons["page_cap_unselected"] = inverse_coverage["page_cap_unselected"]
    if inverse_coverage["word_cap_unselected"]:
        reasons["word_cap_unselected"] = inverse_coverage["word_cap_unselected"]
    abstained = sum(bool(probe.get("abstained")) for probe in inverse_results)
    inverse_coverage.update({
        "probed": len(inverse_results),
        "checked": sum(bool(probe.get("precision_checked")) for probe in inverse_results),
        "abstained": abstained,
        "mapping_checked": sum(bool(probe.get("mapping_ok")) for probe in inverse_results),
        "reasons": reasons,
        "failed": sum(not probe["ok"] and not probe.get("abstained") for probe in inverse_results) + bool(batch_error),
        "document_metadata_matches": sum(probe.get("match") == "document_metadata" for probe in inverse_results),
        "coarse_valid": sum(bool(probe.get("mapping_ok")) and probe.get("reason") in {"coarse_source_refinement", "resolver_warning"} and not (probe.get("location") or {}).get("precise") for probe in inverse_results),
        "unsupported_or_ambiguous": sum(bool(probe.get("abstained")) and bool((probe.get("location") or {}).get("precise", True)) for probe in inverse_results),
    })
    missing_controls = [
        word for word in args.require_precision_word
        if not any(probe.get("precision_checked") and normalized_words(probe["word"]) == normalized_words(word) for probe in inverse_results)
    ]
    failures += len(missing_controls)
    inconclusive = bool(args.best_guess and inverse_coverage["selected"] and not inverse_coverage["checked"] and not failures)
    print(json.dumps({
        "pdf": str(pdf),
        "source": str(source_file),
        "revision_verified": False,
        "freshness_check": "disabled" if args.allow_stale else "all_recorded_sources_mtime_only",
        "precision_status": "verified_controls" if inverse_coverage["checked"] else "unverified" if inverse_coverage["selected"] else "not_requested_or_no_eligible_text",
        "missing_precision_controls": missing_controls,
        "probes": len(results),
        "fails": failures,
        "stopped_early": bool(batch_error),
        "batch_error": batch_error,
        "inverse_coverage": inverse_coverage,
        "visible_checked": sum(bool(probe.get("visible_checked")) for probe in results),
        "results": results,
    }, indent=2))
    sys.exit(1 if failures else 2 if inconclusive else 0)


if __name__ == "__main__":
    main()
