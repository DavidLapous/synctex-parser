#!/usr/bin/env python3
"""Generate synthetic LaTeX documents (Lorem Ipsum + gibberish math) and fuzz SyncTeX.

Builds minimal, self-contained TeX fixtures covering article and Beamer classes,
tabularx tables, multiline captions, multi-file \\input hierarchies, and fragile
frames using only generic Lorem Ipsum prose and synthetic math formulas, then
verifies both forward and inverse SyncTeX resolution via synctex_fuzz.py.
"""

from __future__ import annotations

import argparse
import json
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

LOREM_WORDS = [
    "lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing",
    "elit", "sed", "eiusmod", "tempor", "incididunt", "labore", "dolore",
    "magna", "aliqua", "enim", "minim", "veniam", "quis", "nostrud",
    "exercitation", "ullamco", "laboris", "nisi", "aliquip", "commodo",
    "consequat", "duis", "aute", "irure", "reprehenderit", "voluptate",
    "velit", "esse", "cillum", "fugiat", "nulla", "pariatur", "excepteur",
    "sint", "occaecat", "cupidatat", "proident", "sunt", "culpa", "officia",
    "deserunt", "mollit", "anim", "est", "laborum",
]

GIBBERISH_FORMULAS = [
    r"\alpha_k + \beta_m = \gamma_n",
    r"\sum_{j=1}^{q} \xi_j \eta_j",
    r"\Omega_{p,q} = \theta_p - \lambda_q",
    r"\Delta_r = \mu_r + \nu_r",
    r"\Phi(u, v) = \kappa u + \sigma v",
]


def sentence(rng: random.Random, length: int = 7) -> str:
    chosen = rng.sample(LOREM_WORDS, min(length, len(LOREM_WORDS)))
    return chosen[0].capitalize() + " " + " ".join(chosen[1:]) + "."


def build_article_tabularx(rng: random.Random, directory: Path) -> tuple[Path, Path]:
    sub = directory / "section_sub.tex"
    main = directory / "main.tex"
    rows = []
    for _ in range(4):
        c1 = " ".join(rng.sample(LOREM_WORDS, 3))
        c2 = f"${rng.choice(GIBBERISH_FORMULAS)}$"
        c3 = sentence(rng, 6)
        rows.append(f"{c1} & {c2} & {c3} \\\\")
    sub_body = "\n".join([
        r"\section{Lorem Ipsum Section}",
        sentence(rng, 10),
        r"\begin{table}[h]",
        r"\centering",
        r"\begin{tabularx}{\textwidth}{>{\raggedright\arraybackslash}p{2.5cm}cX}",
        r"Header One & Header Two & Header Three \\",
        r"\hline",
        *rows,
        r"\end{tabularx}",
        r"\caption[Short lorem]{",
        sentence(rng, 8),
        sentence(rng, 8),
        r"}",
        r"\end{table}",
        sentence(rng, 10),
    ]) + "\n"
    sub.write_text(sub_body, encoding="utf-8")
    main_body = "\n".join([
        r"\documentclass{article}",
        r"\usepackage{tabularx}",
        r"\usepackage{amsmath}",
        r"\title{Lorem Ipsum Synthetic Document}",
        r"\author{Synthetic Author}",
        r"\begin{document}",
        r"\maketitle",
        sentence(rng, 12),
        r"\input{section_sub.tex}",
        r"\end{document}",
    ]) + "\n"
    main.write_text(main_body, encoding="utf-8")
    return main, sub


def build_beamer_deck(rng: random.Random, directory: Path) -> tuple[Path, Path]:
    main = directory / "deck.tex"
    main_body = "\n".join([
        r"\documentclass{beamer}",
        r"\usepackage{tabularx}",
        r"\title{Lorem Ipsum Presentation}",
        r"\author{Synthetic Speaker}",
        r"\begin{document}",
        r"\begin{frame}{First Frame}",
        sentence(rng, 8),
        r"\begin{tabularx}{\textwidth}{lX}",
        f"{' '.join(rng.sample(LOREM_WORDS, 2))} & {sentence(rng, 6)} \\\\",
        f"${rng.choice(GIBBERISH_FORMULAS)}$ & {sentence(rng, 6)} \\\\",
        r"\end{tabularx}",
        r"\end{frame}",
        r"\begin{frame}[fragile]{Fragile Frame}",
        sentence(rng, 8),
        r"\begin{verbatim}",
        r"verbatim lorem ipsum token",
        r"\end{verbatim}",
        sentence(rng, 8),
        r"\end{frame}",
        r"\end{document}",
    ]) + "\n"
    main.write_text(main_body, encoding="utf-8")
    return main, main


def compile_tex(tex: Path) -> Path:
    for _ in range(2):
        proc = subprocess.run(
            ["pdflatex", "-interaction=nonstopmode", "-halt-on-error", "-synctex=1", tex.name],
            cwd=tex.parent,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        if proc.returncode != 0:
            raise RuntimeError(f"pdflatex failed on {tex.name}:\n{proc.stdout[-500:]}\n{proc.stderr[-500:]}")
    return tex.with_suffix(".pdf")


def main() -> None:
    default_binary = Path(
        globals().get(
            "PDFTERM_DEFAULT_BINARY",
            Path(__file__).resolve().parents[2] / "pdfterm" / "target" / "release" / "pdfterm",
        )
    )
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iterations", type=int, default=3, help="synthetic documents per template")
    parser.add_argument("--seed", type=int, default=20261020)
    parser.add_argument("--binary", type=Path, default=default_binary, help="pdfterm executable path")
    args = parser.parse_args()

    if shutil.which("pdflatex") is None:
        raise SystemExit("pdflatex is required in PATH to compile synthetic TeX fixtures")

    fuzz_script = Path(__file__).with_name("synctex_fuzz.py")
    rng = random.Random(args.seed)
    summaries = []

    with tempfile.TemporaryDirectory(prefix="synctex-synthetic-fuzz-") as tmp:
        root = Path(tmp)
        for i in range(args.iterations):
            for kind, builder in (("article_tabularx", build_article_tabularx), ("beamer_tabularx", build_beamer_deck)):
                case_dir = root / f"{kind}_{i}"
                case_dir.mkdir(parents=True, exist_ok=True)
                main_tex, probe_tex = builder(rng, case_dir)
                pdf = compile_tex(main_tex)
                cmd = [
                    sys.executable,
                    str(fuzz_script),
                    str(pdf),
                    "--source",
                    str(probe_tex),
                    "--best-guess",
                    "--seed",
                    str(args.seed + i),
                ]
                if args.binary is not None:
                    cmd.extend(["--binary", str(args.binary)])
                proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
                try:
                    report = json.loads(proc.stdout)
                except Exception:
                    sys.stderr.write(proc.stdout)
                    sys.stderr.write(proc.stderr)
                    raise SystemExit(f"synthetic fuzz failed on {kind}_{i}")
                if proc.returncode != 0:
                    failed_probes = [
                        p for p in report.get("results", [])
                        if not p.get("ok") and not p.get("abstained")
                    ]
                    sys.stderr.write(json.dumps(failed_probes, indent=2) + "\n")
                    sys.stderr.write(proc.stderr)
                    raise SystemExit(f"synthetic fuzz failed on {kind}_{i}")
                summaries.append({
                    "case": f"{kind}_{i}",
                    "probes": report["probes"],
                    "fails": report["fails"],
                    "checked": report["inverse_coverage"]["checked"],
                })

    print(json.dumps({"ok": True, "cases": len(summaries), "summaries": summaries}, indent=2))


if __name__ == "__main__":
    main()
