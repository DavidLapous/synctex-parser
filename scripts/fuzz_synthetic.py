#!/usr/bin/env python3
"""Generate synthetic LaTeX documents (Lorem Ipsum + gibberish math) and fuzz SyncTeX.

Builds minimal, self-contained TeX fixtures covering article and Beamer classes,
tabularx tables, multiline captions, multi-file \\input hierarchies, and fragile
frames using only generic Lorem Ipsum prose and synthetic math formulas, then
verifies both forward and inverse SyncTeX resolution via synctex_fuzz.py.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import random
import shutil
import signal
import subprocess
import sys
import tempfile
import time
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
PRECISION_WORDS = {
    "article_tabularx": "Quartzpositivearticle",
    "beamer_tabularx": "Quartzpositivebeamer",
}


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
        r"\par\noindent " + PRECISION_WORDS["article_tabularx"] + r"\par",
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
        r"\par\noindent " + PRECISION_WORDS["beamer_tabularx"] + r"\par",
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
        proc = run_oracle(
            ["pdflatex", "-interaction=nonstopmode", "-halt-on-error", "-synctex=1", "-recorder", tex.name],
            timeout=60,
            cwd=tex.parent,
        )
        if proc.returncode != 0:
            raise RuntimeError(f"pdflatex failed on {tex.name}:\n{proc.stdout[-500:]}\n{proc.stderr[-500:]}")
    return tex.with_suffix(".pdf")


def run_oracle(
    command: list[str], timeout: float = 600, cwd: Path | None = None,
) -> subprocess.CompletedProcess:
    if os.name != "posix":
        raise RuntimeError("synthetic fuzz requires POSIX process-group supervision")
    process = None
    group_signaled = False
    previous_handler = signal.getsignal(signal.SIGINT)
    if previous_handler is None:
        raise RuntimeError("cannot supervise SIGINT with an unknown native signal handler")
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, [])

    def kill_group():
        nonlocal group_signaled
        if process is not None and process.returncode is None and not group_signaled:
            try:
                signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT})
                if not group_signaled:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    group_signaled = True
            finally:
                signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)

    def interrupt(signum, frame):
        # Owned leaders are only reaped with SIGINT blocked through status publication.
        kill_group()
        if callable(previous_handler):
            previous_handler(signum, frame)
        else:
            raise KeyboardInterrupt

    try:
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT})
        if previous_handler != signal.SIG_IGN:
            signal.signal(signal.SIGINT, interrupt)
        # Keep SIGINT pending until ownership is recorded; exec restores the child's mask.
        launch = [
            sys.executable, "-I", "-c",
            "import json,os,signal,sys; "
            "signal.pthread_sigmask(signal.SIG_SETMASK,json.loads(sys.argv[1])); "
            "os.execvp(sys.argv[2],sys.argv[2:])",
            json.dumps(list(previous_mask)), *command,
        ]
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as readers, subprocess.Popen(
            launch, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            start_new_session=True, cwd=cwd,
        ) as process:
            try:
                deadline = time.monotonic() + timeout
                # Readers keep SIGINT blocked and decode before wait() releases the group ID.
                output = [readers.submit(pipe.read) for pipe in (process.stdout, process.stderr)]
                signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
                try:
                    for result in concurrent.futures.as_completed(
                        output, timeout=max(0, deadline - time.monotonic()),
                    ):
                        result.result()
                except concurrent.futures.TimeoutError as error:
                    raise subprocess.TimeoutExpired(command, timeout) from error
                while True:
                    try:
                        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT})
                        status = process.poll()
                    finally:
                        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
                    if status is not None:
                        break
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise subprocess.TimeoutExpired(command, timeout)
                    time.sleep(min(0.01, remaining))
            except BaseException:
                kill_group()
                process.wait()
                raise
            return subprocess.CompletedProcess(command, process.returncode, *(result.result() for result in output))
    finally:
        try:
            signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT})
        finally:
            try:
                if previous_handler != signal.SIG_IGN:
                    signal.signal(signal.SIGINT, previous_handler)
            finally:
                signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)


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
    if args.iterations <= 0:
        parser.error("--iterations must be positive")

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
                    "--build-dir",
                    str(main_tex.parent),
                    "--require-precision-word",
                    PRECISION_WORDS[kind],
                    "--best-guess",
                    "--seed",
                    str(args.seed + i),
                ]
                if args.binary is not None:
                    cmd.extend(["--binary", str(args.binary)])
                try:
                    proc = run_oracle(cmd, timeout=600)
                except subprocess.TimeoutExpired as error:
                    raise SystemExit(f"synthetic fuzz exceeded 600 seconds on {kind}_{i}") from error
                try:
                    report = json.loads(proc.stdout)
                except json.JSONDecodeError:
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
                if not any(
                    probe.get("precision_checked") and probe.get("ok")
                    and probe.get("word", "").casefold() == PRECISION_WORDS[kind].casefold()
                    for probe in report.get("results", [])
                ):
                    raise SystemExit(f"synthetic fuzz lost printed precision control {PRECISION_WORDS[kind]} on {kind}_{i}")
                summaries.append({
                    "case": f"{kind}_{i}",
                    "probes": report["probes"],
                    "fails": report["fails"],
                    "checked": report["inverse_coverage"]["checked"],
                    "precision_control": PRECISION_WORDS[kind],
                })

    print(json.dumps({"ok": True, "cases": len(summaries), "summaries": summaries}, indent=2))


if __name__ == "__main__":
    main()
