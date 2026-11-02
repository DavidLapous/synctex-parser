#!/usr/bin/env python3
"""Behavioral oracle regressions; run: python -m unittest discover -s scripts."""

import contextlib
import io
import json
import os
import random
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import pymupdf
import fuzz_synthetic as synthetic
import synctex_fuzz as oracle

_DEFAULT_PDF_WORD = object()


class OracleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.source = self.root / "main.tex"
        self.pdf = self.root / "main.pdf"
        self.map = self.pdf.with_suffix(".synctex")
        self.pdf.touch()
        self.map.touch()

    def mapping(self, anchor=2, inputs=None, page_lines=None):
        return {"inputs": inputs or {1: self.source}, "origins": {1: (1, anchor)},
                "host_origins": {}, "page_inputs": {1: {1}},
                "page_lines": {1: page_lines or {(1, anchor)}}}

    def probe(self, text, word, offset, anchor=2, precise=True, pdf_word=_DEFAULT_PDF_WORD, mapping=None,
              box=(40, 190, 60, 210), columns=None, **echo):
        self.source.write_text(text)
        prefix = text[:offset]
        line = prefix.count("\n") + 1
        row_prefix = prefix.rsplit("\n", 1)[-1]
        location = {"file": str(self.source), "line": line, "precise": precise,
                    "byte_column": len(row_prefix.encode()), "column_char": len(row_prefix) + 1,
                    "column": len(row_prefix.encode("utf-16-le")) // 2 + 1}
        location.update(columns or {})
        batch = {"ok": True, "location": location, "warning": None,
                 "pdf_word": word if pdf_word is _DEFAULT_PDF_WORD else pdf_word,
                 "page": 1, "x": (box[0] + box[2]) / 2, "y": (box[1] + box[3]) / 2, **echo}
        result = subprocess.CompletedProcess([], 0, json.dumps(batch) + "\n", "")
        with patch.object(oracle, "read_source_map", return_value=mapping or self.mapping(anchor)), \
             patch.object(oracle, "pdfterm_bin", return_value=Path("unused")), \
             patch.object(oracle, "raw_inverse", return_value=[(anchor, str(self.source))]), \
             patch.object(oracle.subprocess, "run", return_value=result):
            return list(oracle.inverse(self.pdf, self.source, {1: [(word, box, 800)]}, True))[0]

    def test_ordinary_printed_title_caption_and_prose_are_checked(self):
        for text, word in [
            ("\\begin{frame}{Quartztitle}\nPlain body.\n\\end{frame}\n", "Quartztitle"),
            ("\\begin{frame}\nQuartzprose\n\\end{frame}\n", "Quartzprose"),
            ("\\caption[unused]{Quartzlong caption}\n", "Quartzlong"),
        ]:
            with self.subTest(word=word):
                result = self.probe(text, word, text.index(word), anchor=1 if "caption" in text else 3)
                self.assertTrue(result.get("precision_checked"), result)

    def test_known_nonprinting_positions_are_failures(self):
        for text, word in [
            ("\\begin{frame}\nVisible % Quartzcomment\n\\end{frame}\n", "Quartzcomment"),
            ("\\begin{frame}[Quartzoption]\nVisible\n\\end{frame}\n", "Quartzoption"),
            ("\\begin{frame}\nVisible\n\\end{frame}\n", "frame"),
            ("\\caption[Quartzshort]{Printed caption}\n", "Quartzshort"),
            ("\\includegraphics{Quartzimage.pdf}\n", "Quartzimage"),
        ]:
            with self.subTest(word=word):
                result = self.probe(text, word, text.index(word), anchor=1)
                self.assertFalse(result["ok"], result)
                self.assertIn("nonprinting", result.get("error", ""), result)

    def test_known_nonprinting_keys_fail_even_inside_unknown_wrappers(self):
        for invocation in [
            "\\label{Quartzhidden}", "\\cite{Quartzhidden}", "\\ref{Quartzhidden}",
            "\\href{Quartzhidden}{Quartzlink}", "\\bibitem{Quartzhidden}",
            "\\unknownwrapper{\\label{Quartzhidden}}",
        ]:
            with self.subTest(invocation=invocation):
                text = "\\begin{frame}\nQuartzbody\n" + invocation + "\n\\end{frame}\n"
                bad = self.probe(text, "Quartzhidden", text.index("Quartzhidden"), anchor=4)
                self.assertIn("nonprinting", bad.get("error", ""), bad)
                self.assertFalse(bad.get("abstained"), bad)
                if "unknownwrapper" not in invocation:
                    good = self.probe(text, "Quartzbody", text.index("Quartzbody"), anchor=4)
                    self.assertTrue(good.get("precision_checked"), good)

    def test_graphics_cannot_bypass_pdf_word_mismatch(self):
        text = "\\includegraphics{Quartzimage.pdf}\n"
        result = self.probe(text, "Quartzprobe", text.index("Quartzimage"), anchor=1, pdf_word="Other")
        self.assertFalse(result["ok"], result)
        self.assertNotEqual(result.get("match"), "graphics_argument")

    def test_note_arguments_and_unknown_wrappers_are_unverified(self):
        for invocation in ["\\note{Quartzopaque}", "\\unknownwrapper{Quartzopaque}"]:
            text = "\\begin{frame}\n" + invocation + "\n\\end{frame}\n"
            result = self.probe(text, "Quartzopaque", text.index("Quartzopaque"), anchor=3)
            self.assertFalse(result["ok"], result)
            self.assertEqual(result.get("reason"), "unverified_execution_ownership", result)
        text = "\\begin{frame}\n\\note{Quartzbody}\nQuartzbody\n\\end{frame}\n"
        result = self.probe(text, "Quartzbody", text.rindex("Quartzbody"), anchor=4)
        self.assertFalse(result["ok"], result)
        self.assertEqual(result.get("reason"), "unverified_execution_ownership", result)
        text = "\\renewcommand{\\textbf}[1]{\\note{#1}}\n\\begin{frame}\n\\textbf{Quartzforwarded}\n\\end{frame}\n"
        result = self.probe(text, "Quartzforwarded", text.index("Quartzforwarded"), anchor=4)
        self.assertFalse(result["ok"], result)
        self.assertEqual(result.get("reason"), "unverified_execution_ownership", result)

    def test_at_prefixed_control_words_cannot_prove_discarded_argument_ownership(self):
        text = "\\title{Quartz{}hidden}\n\\makeatletter\n\\begin{frame}\n\\maketitle\n\\@gobble{Quartzhidden}\n\\end{frame}\n"
        result = self.probe(text, "Quartzhidden", text.rindex("Quartzhidden"), anchor=6)
        self.assertEqual(result.get("reason"), "unverified_execution_ownership", result)
        self.assertFalse(result.get("precision_checked"), result)

    def test_loop_expansion_is_not_a_missing_literal_failure(self):
        text = "\\begin{frame}\n\\foreach \\w in {Quartzloop}{\\node {\\w};}\n\\end{frame}\n"
        result = self.probe(text, "Quartzloop", text.index("\\w};"), anchor=3)
        self.assertTrue(result.get("abstained"), result)
        self.assertNotIn("error", result)

    def test_wrong_plain_word_or_line_is_a_failure_not_an_abstention(self):
        for other in ["Quartzright", "\\textbf{Quartzright}", "$\\alpha$"]:
            with self.subTest(other=other):
                text = "\\begin{frame}\nQuartzleft\n" + other + "\n\\end{frame}\n"
                good = self.probe(text, "Quartzleft", text.index("Quartzleft"), anchor=4)
                self.assertTrue(good.get("precision_checked"), good)
                wrong = text.index("\\alpha" if "alpha" in other else "Quartzright")
                bad = self.probe(text, "Quartzleft", wrong, anchor=4)
                self.assertIn("does not select", bad.get("error", ""), bad)
                self.assertFalse(bad.get("abstained"), bad)
        text = "\\begin{frame}\nQuartzright\n\\end{frame}\n"
        missing = self.probe(text, "Quartzabsent", text.index("Quartzright"), anchor=3)
        self.assertIn("does not select", missing.get("error", ""), missing)
        self.assertFalse(missing.get("abstained"), missing)

    def test_wrong_plain_column_cannot_hide_behind_producer_word_or_warning(self):
        text = "\\begin{frame}\nQuartzleft Quartzright\n\\end{frame}\n"
        for answer, reason in [
            ({"warning": "generic resolver warning"}, "resolver_warning"),
            ({"pdf_word": None}, "pdfium_selected_no_word"),
            ({"pdf_word": "Quartzright"}, "pdfium_word_mismatch"),
        ]:
            with self.subTest(answer=answer):
                good = self.probe(text, "Quartzleft", text.index("Quartzleft"), anchor=3, **answer)
                self.assertEqual(good.get("reason"), reason, good)
                bad = self.probe(text, "Quartzleft", text.index("Quartzright"), anchor=3, **answer)
                self.assertIn("does not select", bad.get("error", ""), bad)
                self.assertFalse(bad.get("abstained"), bad)

    def test_source_lines_ignore_non_lf_unicode_and_control_separators(self):
        for separator in ["\f", "\v", "\x85", "\u2028", "\u2029"]:
            for newline in ["\n", "\r\n"]:
                with self.subTest(separator=repr(separator), newline=repr(newline)):
                    text = newline.join(["\\begin{frame}", "Quartzprefix" + separator + "Quartzbody", "\\end{frame}", ""])
                    sampled = oracle.sample_source_positions(text, random.Random(0), 10)
                    self.assertEqual([line for line, _ in sampled], [1, 2, 3])
                    result = self.probe(text, "Quartzbody", text.index("Quartzbody"), anchor=3)
                    self.assertTrue(result.get("precision_checked"), result)

    def test_tex_linebreak_does_not_consume_following_literal_as_a_macro(self):
        text = "\\begin{frame}\nFirst \\\\Quartzlinebreak\n\\end{frame}\n"
        good = self.probe(text, "Quartzlinebreak", text.index("Quartzlinebreak"), anchor=3)
        self.assertTrue(good.get("precision_checked"), good)
        repeated = text.replace("\\end{frame}", "Quartzlinebreak\n\\end{frame}")
        ambiguous = self.probe(repeated, "Quartzlinebreak", repeated.rindex("Quartzlinebreak"), anchor=4)
        self.assertEqual(ambiguous.get("reason"), "ambiguous_repeated_occurrence", ambiguous)
        for opaque in [
            "\\begin{frame}\n" + "\\" * 3 + "unknownwrapper{Quartzbody}\n\\end{frame}\n",
            "\\renewcommand{\\\\}{\\relax}\n\\begin{frame}\n\\\\Quartzbody\n\\end{frame}\n",
        ]:
            with self.subTest(opaque=opaque):
                result = self.probe(opaque, "Quartzbody", opaque.index("Quartzbody"), anchor=opaque.count("\n"))
                self.assertEqual(result.get("reason"), "unverified_execution_ownership", result)

    def test_actual_math_expansion_remains_unverified(self):
        text = "\\begin{frame}\n$\\alpha$\n\\end{frame}\n"
        result = self.probe(text, "α", text.index("\\alpha"), anchor=3)
        self.assertEqual(result.get("reason"), "precision_not_literal_math_or_expansion", result)
        self.assertTrue(result.get("abstained"), result)
        self.assertNotIn("error", result)

    def test_distant_at_prefixed_math_expansion_remains_unverified(self):
        for command in ["\\kappa", "\\@kappa", "\\α"]:
            with self.subTest(command=command):
                text = "\\makeatletter\n\\let\\@kappa\\kappa\n\\def\\α{\\kappa}\n$" + command + "\n" + "% padding\n" * 6 + "u$\n"
                result = self.probe(text, "κu", text.index("u$"), anchor=text.count("\n"))
                self.assertEqual(result.get("reason"), "precision_not_literal_math_or_expansion", result)
                self.assertTrue(result.get("abstained"), result)
                self.assertNotIn("error", result)

    def test_reference_expansion_is_unverified_but_its_key_is_not(self):
        text = "\\begin{frame}\n\\ref{Quartzkey}\n\\end{frame}\n"
        expanded = self.probe(text, "42", text.index("\\ref"), anchor=3)
        self.assertTrue(expanded.get("abstained"), expanded)
        self.assertNotIn("error", expanded)
        key = self.probe(text, "Quartzkey", text.index("Quartzkey"), anchor=3)
        self.assertIn("nonprinting", key.get("error", ""), key)

    def test_accent_spelling_in_opaque_note_cannot_qualify_body_precision(self):
        text = "\\note{Mod\\^ele}\n\\begin{frame}\nModêle\n\\end{frame}\n"
        result = self.probe(text, "Modêle", text.index("Modêle"), anchor=4)
        self.assertEqual(result.get("reason"), "unverified_execution_ownership", result)
        self.assertFalse(result.get("precision_checked"), result)

    def test_tex_accent_literals_preserve_original_unicode_columns(self):
        for source_word, painted in [
            ("Mod\\^ele", "Modêle"), ("Mod{\\^e}le", "Modêle"),
            ("\\c{c}edille", "çedille"), ("{\\c{c}}edille", "çedille"),
        ]:
            with self.subTest(source_word=source_word):
                text = "\\begin{frame}\nPréface 😀 " + source_word + " Quartzother\n\\end{frame}\n"
                offset = text.index(source_word)
                good = self.probe(text, painted, offset, anchor=3)
                self.assertTrue(good.get("precision_checked"), good)
                for wrong in [offset + 1, text.index("Quartzother")]:
                    bad = self.probe(text, painted, wrong, anchor=3)
                    self.assertIn("does not select", bad.get("error", ""), bad)
                    self.assertFalse(bad.get("abstained"), bad)
                invalid_byte = len("Préface ".encode()) + 1
                bad = self.probe(text, painted, offset, anchor=3, columns={"byte_column": invalid_byte})
                self.assertFalse(bad.get("precision_checked"), bad)
                self.assertIn("error", bad)

    def test_out_of_file_compiler_anchors_still_fail(self):
        text = "\\begin{frame}\nQuartzbody\n\\end{frame}\n"
        for precise in (False, True):
            result = self.probe(text, "Quartzbody", text.index("Quartzbody"), anchor=4, precise=precise)
            self.assertIn("outside its source file", result.get("error", ""), result)

    def test_repeated_word_is_not_occurrence_proof(self):
        text = "\\begin{frame}\nQuartzrepeat first.\nQuartzrepeat second.\n\\end{frame}\n"
        result = self.probe(text, "Quartzrepeat", text.rindex("Quartzrepeat"), anchor=4)
        self.assertFalse(result["ok"], result)
        self.assertEqual(result.get("reason"), "ambiguous_repeated_occurrence", result)

    def test_unrelated_table_cannot_choose_expected_scope_or_raw_anchor(self):
        text = "\\begin{tabularx}{10cm}{X}\nQuartzfirst\n\\end{tabularx}\n" + "\n" * 5 + "\\begin{tabularx}{10cm}{X}\nQuartzsecond\n\\end{tabularx}\n"
        mapping = self.mapping(3, page_lines={(1, 3), (1, 11)})
        result = self.probe(text, "Quartzsecond", text.index("Quartzsecond"), anchor=3, mapping=mapping)
        self.assertFalse(result["ok"], result)
        self.assertIn("scope", result.get("error", ""), result)
        self.assertEqual(result["raw_anchor"]["line"], 3)

    def test_body_cannot_escape_to_document_metadata(self):
        text = "\\title{Quartzbody}\n" + "\n" * 8 + "\\begin{frame}\nQuartzbody\n\\end{frame}\n"
        result = self.probe(text, "Quartzbody", text.index("Quartzbody"), anchor=12)
        self.assertFalse(result["ok"], result)
        self.assertIn("scope", result.get("error", ""), result)

    def test_valid_coarse_anchor_remains_checked_and_changed_anchor_fails(self):
        text = "\\begin{frame}\nQuartzbody\n\\end{frame}\n"
        good = self.probe(text, "Quartzbody", text.index("\\end"), anchor=3, precise=False)
        self.assertTrue(good.get("mapping_ok"), good)
        self.assertEqual(good.get("reason"), "coarse_source_refinement")
        bad = self.probe(text, "Quartzbody", text.index("Quartzbody"), anchor=3, precise=False)
        self.assertIn("anchor", bad.get("error", ""), bad)

    def test_echoed_batch_identity_is_enforced(self):
        text = "Quartzpositive\n"
        for echo in [
            {"x": 51.0}, {"page": True}, {"page": 1.0}, {"x": True}, {"y": "200"},
            {"x": float("nan")}, {"y": float("inf")}, {"x": 1e100},
            {"input": '{"page":1,"x":8,"y":200}'},
            {"input": '{"page":true,"x":50,"y":200}'},
            {"input": '{"page":1,"x":true,"y":200}'},
        ]:
            with self.subTest(echo=echo):
                result = self.probe(text, "Quartzpositive", 0, anchor=1, **echo)
                self.assertFalse(result["ok"], result)
                self.assertIn("point", result.get("error", ""), result)

    def test_native_f32_point_echoes_accept_rounding_but_reject_adjacent_values(self):
        text = "Quartzpositive\n"
        for box, x in [
            ((90.0, 190, 110.00000762939453, 210), 100.0),
            ((0.0, 190, 0.20000000298023224, 210), 0.1),
        ]:
            with self.subTest(box=box, x=x):
                for echo in [{"x": x}, {"x": x, "input": json.dumps({"page": 1, "x": x, "y": 200})}]:
                    good = self.probe(text, "Quartzpositive", 0, anchor=1, box=box, **echo)
                    self.assertTrue(good.get("precision_checked"), good)
        for echo in [
            {"x": 100.00000762939453},
            {"input": '{"page":1,"x":100.00000762939453,"y":200}'},
        ]:
            bad = self.probe(text, "Quartzpositive", 0, anchor=1,
                             box=(90.0, 190, 110.00000762939453, 210), **echo)
            self.assertIn("point", bad.get("error", ""), bad)
            self.assertFalse(bad.get("precision_checked"), bad)

    def test_unsafe_same_line_file_substitution_is_not_authority(self):
        self.source.write_text("Quartzpositive\n")
        other = self.root / "other.tex"
        other.write_text("Quartzpositive\n")
        result = self.probe("Quartzpositive\n", "Quartzpositive", 0, anchor=1,
                            mapping=self.mapping(1, {1: self.source, 2: other}, {(2, 1)}))
        self.assertEqual(result["expected_anchor"]["file"], str(self.source), result)
        self.assertTrue(result["precision_checked"], result)

    def test_generated_adjacent_host_mentions_are_not_execution_authority(self):
        self.source.write_text("\\end{document}\n\\bibliography{unused}\n")
        generated = self.pdf.with_suffix(".bbl")
        generated.write_text("Generated\n")
        mapping = self.mapping(1, {1: self.source, 2: generated})
        mapping.update(origins={1: (2, 1)}, page_inputs={1: {2}}, page_lines={1: {(2, 1)}},
                       adjacent_origins={1: [(1, 2)]})
        location = {"file": str(self.source), "line": 2, "precise": False}
        batch = subprocess.CompletedProcess([], 0, json.dumps({"ok": True, "location": location}), "")
        with patch.object(oracle, "read_source_map", return_value=mapping), \
             patch.object(oracle, "pdfterm_bin", return_value=Path("unused")), \
             patch.object(oracle, "raw_inverse", return_value=[(1, str(generated))]), \
             patch.object(oracle.subprocess, "run", return_value=batch):
            result = list(oracle.inverse(self.pdf, self.source, {1: [("Generated", (40, 190, 60, 210), 800)]}, True))[0]
        self.assertFalse(result["ok"], result)
        self.assertIn("anchor", result.get("error", ""), result)

    def test_caption_scope_and_cursor_visibility_stay_distinct(self):
        cases = [
            (
                "\\caption[Quartzshort]{Quartzfirst\nQuartzsecond}\n",
                2, (0, 1),
                [("Quartzfirst", 2), ("Quartzsecond", 2), ("Quartzshort", None)],
            ),
            (
                "\\caption{Quartzfirst % Quartzcomment\n"
                "\\label{Quartzkey}\\verb|Quartzopaque| Quartzlast}\n",
                2, (0, 1),
                [("Quartzlast", 2), ("Quartzcomment", None),
                 ("Quartzkey", None), ("Quartzopaque", None)],
            ),
            (
                "% \\caption{Quartzcomment}\n\\verb|\\caption{Quartzopaque}|\n"
                "\\newcommand{\\stash}{\\caption{Quartzstored}}\n"
                "\\caption*{Quartzbody} Quartzoutside\n",
                4, (3, 3),
                [("Quartzbody", 4), ("Quartzoutside", None),
                 ("Quartzcomment", None), ("Quartzopaque", None), ("Quartzstored", None)],
            ),
        ]
        for text, anchor, bounds, positions in cases:
            with self.subTest(text=text):
                self.assertEqual(oracle.caption_bounds(text, anchor), bounds)
                for token, expected in positions:
                    at = text.index(token)
                    prefix = text[:at]
                    line = prefix.count("\n") + 1
                    column = len(prefix.rsplit("\n", 1)[-1]) + 1
                    self.assertEqual(oracle.caption_closing_line(text, line, column), expected, token)

    def test_pre_table_source_column_does_not_jump_to_closing_line(self):
        text = "Quartzprefix \\begin{tabularx}{10cm}{X}\nCell\n\\end{tabularx}\n"
        self.assertIsNone(oracle.collected_env_closing_line(text, 1, 1))
        self.assertEqual(oracle.collected_env_closing_line(text, 1, text.index("\\begin") + 1), 3)

    def test_relative_compiler_paths_use_recorded_or_explicit_build_context(self):
        self.source.write_text("Quartzpositive\n")
        self.map.write_text("Input:1:./main.tex\n")
        with patch.object(oracle, "BUILD_DIRECTORY", None):
            with self.assertRaisesRegex(RuntimeError, "build-dir"):
                oracle.read_source_map(self.pdf)
            self.pdf.with_suffix(".fls").write_text(f"PWD {self.root}\n")
            self.assertEqual(oracle.read_source_map(self.pdf)["inputs"][1], self.source)
        self.pdf.with_suffix(".fls").unlink()
        with patch.object(oracle, "BUILD_DIRECTORY", self.root):
            self.assertEqual(oracle.read_source_map(self.pdf)["inputs"][1], self.source)

    def test_included_source_freshness_and_source_membership(self):
        self.source.write_text("Quartzpositive\n")
        child = self.root / "child.tex"
        child.write_text("Quartzchild\n")
        os.utime(self.source, ns=(100, 100))
        os.utime(self.pdf, ns=(200, 200))
        os.utime(self.map, ns=(200, 200))
        os.utime(child, ns=(300, 300))
        mapping = self.mapping(inputs={1: self.source, 2: child})
        with self.assertRaisesRegex(RuntimeError, "child.tex"):
            oracle.validate_sources(self.pdf, self.map, mapping, self.source, False)
        oracle.validate_sources(self.pdf, self.map, mapping, self.source, True)
        with self.assertRaisesRegex(RuntimeError, "absent"):
            oracle.validate_sources(self.pdf, self.map, self.mapping(), child, True)
        self.assertFalse(list(oracle.forward(self.pdf, child, "Quartzchild", [(1, 1)], 1, source_map=self.mapping()))[0]["ok"])

    def test_pdfinfo_timeout_is_an_explicit_failure(self):
        def timed_out(*args, **kwargs):
            self.assertEqual(kwargs["timeout"], 30)
            raise subprocess.TimeoutExpired(args[0], 30)
        with patch.object(oracle.subprocess, "run", side_effect=timed_out):
            with self.assertRaises(subprocess.TimeoutExpired):
                oracle.pdf_page_count(self.pdf)

    def test_real_cropped_rotated_fill_stroke_words_keep_compiler_coordinates(self):
        for rotation in (0, 90, 180, 270):
            for paint_mode in (0, 2):
                with self.subTest(rotation=rotation, paint_mode=paint_mode):
                    with pymupdf.open() as doc:
                        page = doc.new_page(width=600, height=800)
                        page.insert_text((130, 150), "Quartzpaint", fontsize=14, render_mode=paint_mode)
                        page.set_mediabox(pymupdf.Rect(20, 30, 620, 830))
                        page.set_cropbox(pymupdf.Rect(70, 100, 570, 700))
                        page.set_rotation(rotation)
                        pdf = self.root / f"paint-{rotation}-{paint_mode}.pdf"
                        doc.save(pdf)
                    points, coverage = oracle.painted_word_points(pdf, 1, None, 0, random.Random(1), None)
                    self.assertEqual(coverage["selected"], 1, coverage)
                    word, box, height = points[1][0]
                    self.assertEqual(word, "Quartzpaint")
                    self.assertAlmostEqual(box[0], 130, places=3)
                    self.assertLess(box[1], 150)
                    self.assertGreater(box[3], 150)
                    self.assertEqual(height, 800)

    def test_nonpositive_iterations_fail_before_compilation(self):
        for count in ("0", "-1"):
            with patch.object(sys, "argv", ["fuzz_synthetic.py", "--iterations", count]), \
                 contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    synthetic.main()
                self.assertEqual(error.exception.code, 2)

    def test_zero_precision_is_inconclusive_and_positive_controls_fail(self):
        self.source.write_text("Quartzpositive\n")
        points = {1: [("Quartzpositive", (40, 190, 60, 210), 800)]}
        coverage = {"selected": 1, "page_cap_unselected": 0, "word_cap_unselected": 0}
        coarse = {"direction": "inverse", "word": "Quartzpositive", "ok": False,
                  "mapping_ok": True, "abstained": True, "reason": "coarse_source_refinement",
                  "location": {"precise": False}}
        for extra, code in [([], 2), (["--require-precision-word", "Quartzpositive"], 1), (["--pages", "0"], 0)]:
            output = io.StringIO()
            with patch.object(sys, "argv", ["synctex_fuzz.py", str(self.pdf), "--lines", "0", "--best-guess", "--allow-stale", *extra]), \
                 patch.object(oracle, "read_source_map", return_value=self.mapping(1)), \
                 patch.object(oracle, "pdf_page_count", return_value=1), \
                 patch.object(oracle, "painted_word_points", return_value=({}, {**coverage, "selected": 0}) if "--pages" in extra else (points, dict(coverage))), \
                 patch.object(oracle, "inverse", return_value=iter([] if "--pages" in extra else [dict(coarse)])), \
                 contextlib.redirect_stdout(output):
                with self.assertRaises(SystemExit) as error:
                    oracle.main()
            self.assertEqual(error.exception.code, code, output.getvalue())
            report = json.loads(output.getvalue())
            self.assertEqual(report["inverse_coverage"]["checked"], 0)
            self.assertEqual(report["inverse_coverage"]["coarse_valid"], 0 if "--pages" in extra else 1)
            self.assertFalse(report["revision_verified"])

    def test_synthetic_deadline_and_lost_positive_control_are_failures(self):
        for timeout in (True, False):
            with self.subTest(timeout=timeout):
                def run(command, **kwargs):
                    self.assertEqual(kwargs["timeout"], 600)
                    self.assertIn("--build-dir", command)
                    self.assertIn("--require-precision-word", command)
                    if timeout:
                        raise subprocess.TimeoutExpired(command, 600)
                    return subprocess.CompletedProcess(command, 0, json.dumps({
                        "probes": 1, "fails": 0, "inverse_coverage": {"checked": 0}, "results": [],
                    }), "")
                with patch.object(sys, "argv", ["fuzz_synthetic.py", "--iterations", "1"]), \
                     patch.object(synthetic.shutil, "which", return_value="pdflatex"), \
                     patch.object(synthetic, "compile_tex", side_effect=lambda source: source.with_suffix(".pdf")), \
                     patch.object(synthetic, "run_oracle", side_effect=run):
                    with self.assertRaises(SystemExit) as error:
                        synthetic.main()
                self.assertIn("600 seconds" if timeout else "precision control", str(error.exception))

    def test_oracle_deadline_and_malformed_output_remove_owned_descendants(self):
        real_popen = subprocess.Popen
        for count in (1, 4):
            for stream in (None, "stdout", "stderr"):
                with self.subTest(descendants=count, malformed_stream=stream):
                    pidfile = self.root / f"children-{count}-{stream}.json"
                    processes = []
                    cleaned = False
                    command = [
                        sys.executable, "-c",
                        "import json,os,pathlib,subprocess,sys\n"
                        "stream=sys.argv[3]; output=subprocess.DEVNULL if stream!='None' else None\n"
                        "children=[subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'], "
                        "stdout=output,stderr=output) for _ in range(int(sys.argv[2]))]\n"
                        "ready=pathlib.Path(sys.argv[1]); staging=ready.with_suffix('.ready')\n"
                        "staging.write_text(json.dumps([os.getpid()]+[p.pid for p in children])); staging.replace(ready)\n"
                        "if stream!='None': os.write(1 if stream=='stdout' else 2,b'\\xff')\n"
                        "else:\n"
                        "    for child in children: child.wait()\n",
                        str(pidfile), str(count), str(stream),
                    ]

                    def launch(*args, **kwargs):
                        process = real_popen(*args, **kwargs)
                        processes.append(process)
                        return process

                    try:
                        expected = UnicodeDecodeError if stream else subprocess.TimeoutExpired
                        with patch.object(synthetic.subprocess, "Popen", side_effect=launch):
                            with self.assertRaises(expected):
                                synthetic.run_oracle(command, timeout=0.75)
                        self.assertTrue(pidfile.exists(), "generic descendants did not start")
                        pids = json.loads(pidfile.read_text())
                        self.assertEqual(len(pids), count + 1)
                        until = time.monotonic() + 3
                        alive = set(pids)
                        while alive and time.monotonic() < until:
                            for pid in tuple(alive):
                                try:
                                    os.kill(pid, 0)
                                except ProcessLookupError:
                                    alive.remove(pid)
                            if alive:
                                time.sleep(0.01)
                        self.assertFalse(alive, f"owned processes survived supervision: {sorted(alive)}")
                        self.assertIsNotNone(processes[0].returncode, "direct child was not reaped")
                        cleaned = True
                    finally:
                        for process in processes:
                            if process.returncode is None:
                                try:
                                    os.killpg(process.pid, signal.SIGKILL)
                                except ProcessLookupError:
                                    pass
                                process.wait()
                        if not cleaned and pidfile.exists():
                            for pid in json.loads(pidfile.read_text())[1:]:
                                try:
                                    if os.getpgid(pid) == processes[0].pid:
                                        os.kill(pid, signal.SIGKILL)
                                except ProcessLookupError:
                                    pass

    def test_sigint_reaps_exited_leader_and_kills_pipe_holding_descendant(self):
        pidfile = self.root / "interrupted-pids.json"
        ready = self.root / "leader-exited"
        descendant = (
            "import os,pathlib,sys,time\n"
            "while os.getppid()==int(sys.argv[1]): time.sleep(0.01)\n"
            "pathlib.Path(sys.argv[2]).touch()\n"
            "time.sleep(30)\n"
        )
        leader = (
            "import json,os,pathlib,subprocess,sys\n"
            f"child=subprocess.Popen({[sys.executable, '-c', descendant]!r}+"
            "[str(os.getpid()),sys.argv[2]])\n"
            "pathlib.Path(sys.argv[1]).write_text(json.dumps([os.getpid(),child.pid]))\n"
            "os._exit(0)\n"
        )
        command = [sys.executable, "-c", leader, str(pidfile), str(ready)]
        supervisor_code = (
            "import json,os,sys\n"
            f"sys.path.insert(0,{str(Path(synthetic.__file__).parent)!r})\n"
            "import fuzz_synthetic as synthetic\n"
            "try:\n"
            f"    synthetic.run_oracle({command!r},timeout=10)\n"
            "except KeyboardInterrupt:\n"
            f"    leader=json.loads(open({str(pidfile)!r}).read())[0]\n"
            "    try: os.waitpid(leader,os.WNOHANG)\n"
            "    except ChildProcessError: print('leader reaped',flush=True); sys.exit(71)\n"
            "    sys.exit(72)\n"
        )
        supervisor = subprocess.Popen(
            [sys.executable, "-c", supervisor_code], stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, start_new_session=True,
        )
        pids = []
        try:
            until = time.monotonic() + 5
            while not ready.exists() and time.monotonic() < until:
                time.sleep(0.01)
            self.assertTrue(ready.exists(), "leader did not exit with an inherited pipe open")
            pids = json.loads(pidfile.read_text())
            os.kill(supervisor.pid, signal.SIGINT)
            stdout, stderr = supervisor.communicate(timeout=5)
            self.assertEqual(supervisor.returncode, 71, stderr)
            self.assertEqual(stdout, "leader reaped\n")
            until = time.monotonic() + 3
            alive = set(pids)
            while alive and time.monotonic() < until:
                for pid in tuple(alive):
                    try:
                        os.kill(pid, 0)
                    except ProcessLookupError:
                        alive.remove(pid)
                if alive:
                    time.sleep(0.01)
            self.assertFalse(alive, f"owned processes survived SIGINT: {sorted(alive)}")
        finally:
            if not pids and pidfile.exists():
                pids = json.loads(pidfile.read_text())
            if pids:
                try:
                    if os.getpgid(pids[1]) == pids[0]:
                        os.killpg(pids[0], signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if supervisor.returncode is None:
                os.killpg(supervisor.pid, signal.SIGKILL)
            supervisor.communicate(timeout=5)

    def test_oracle_normal_exit_preserves_status_and_output_without_group_signals(self):
        for status in (0, 7):
            with self.subTest(status=status), patch.object(synthetic.os, "killpg") as kill_group:
                command = [sys.executable, "-c",
                           "import sys; print('generic stdout'); print('generic stderr', file=sys.stderr); "
                           f"sys.exit({status})"]
                result = synthetic.run_oracle(command, timeout=2)
                self.assertEqual(result.returncode, status)
                self.assertEqual(result.stdout, "generic stdout\n")
                self.assertEqual(result.stderr, "generic stderr\n")
                kill_group.assert_not_called()

    def test_collected_source_requires_its_point_containing_compiler_box(self):
        text = "Mainprose\n\\end{document}\n"
        table = self.root / "included.tex"
        other = self.root / "other.tex"
        table_text = "\\begin{tabularx}{10cm}{X}\nQuartzcell\n\\end{tabularx}\n"
        table.write_text(table_text)
        other.write_text(table_text)
        mapping = self.mapping(2, {1: self.source, 2: table, 3: other}, {(1, 2), (2, 3), (3, 3)})
        def records(pdf, path, line):
            self.assertEqual(line, 3)
            return [{"Page": 1, "h": 40 if path == table else 400, "v": 210, "W": 20, "H": 20}]
        columns = {"file": str(table), "line": 2, "byte_column": 0, "column": 1, "column_char": 1}
        with patch.object(oracle, "raw_forward_records", side_effect=records):
            good = self.probe(text, "Quartzcell", 0, anchor=2, mapping=mapping, columns=columns)
            bad = self.probe(text, "Quartzcell", 0, anchor=2, mapping=mapping,
                             columns={**columns, "file": str(other)})
        self.assertTrue(good.get("mapping_ok"), good)
        self.assertEqual(good.get("reason"), "ambiguous_compiler_ownership", good)
        self.assertFalse(good.get("precision_checked"), good)
        self.assertEqual(good["raw_anchor"], {"line": 2, "file": str(self.source)})
        self.assertEqual(good["point_anchors"], [{"line": 3, "file": str(table)}])
        self.assertIn("source file", bad.get("error", ""), bad)
        self.assertFalse(bad.get("abstained"), bad)

    def test_composite_math_requires_local_expansion_evidence(self):
        text = "\\begin{frame}\n$\\kappa u + \\sigma v$\n\\end{frame}\n"
        result = self.probe(text, "κu", text.index(" u") + 1, anchor=3)
        self.assertEqual(result.get("reason"), "precision_not_literal_math_or_expansion", result)
        self.assertNotIn("error", result)
        for row in ["Quartzwrong", "$Quartzwrong$"]:
            text = "\\begin{frame}\n" + row + "\n$\\kappa u$\n\\end{frame}\n"
            wrong = self.probe(text, "Quartzabsent", text.index("Quartzwrong"), anchor=4)
            self.assertIn("does not select", wrong.get("error", ""), wrong)
            self.assertFalse(wrong.get("abstained"), wrong)

    def test_math_context_respects_hidden_delimiters_and_inline_boundaries(self):
        for text, word, expected in [
            ("Before $\\alpha Quartzmath$ After", "Quartzmath", True),
            ("Before $Quartzliteral$ After", "Quartzliteral", False),
            ("$\\note{\\alpha} Quartzliteral$", "Quartzliteral", False),
            ("% $\\alpha$\nQuartzoutside", "Quartzoutside", False),
            ("\\note{$\\alpha$} Quartzoutside", "Quartzoutside", False),
            ("\\begin{equation}\nQuartzdisplay\n\\end{equation}\nOutside", "Quartzdisplay", True),
            ("\\begin{equation}\n$Quartzliteral$\n\\end{equation}\n", "Quartzliteral", False),
            ("\\begin{equation}\nQuartzdisplay\n\\end{equation}\nOutside", "Outside", False),
            ("$\\alpha Quartzmath$ Quartzoutside", "Quartzoutside", False),
        ]:
            with self.subTest(text=text, word=word):
                self.assertEqual(oracle.math_expansion_at(text, text.index(word)), expected)
        text = "$\\alpha Quartzmath$"
        self.assertTrue(oracle.math_expansion_at(text, text.index("\\alpha")))
        self.assertFalse(oracle.math_expansion_at(text, 0))
        self.assertFalse(oracle.math_expansion_at(text, len(text) - 1))

    def test_environment_bounds_preserve_first_same_line_match_and_nested_rejection(self):
        text = "\\begin{equation}\nFirst\n\\end{equation}\\begin{equation}\nSecond\n\\end{equation}\n"
        self.assertEqual(oracle.environment_bounds(text, 3, "equation"), (0, 2))
        self.assertEqual(oracle.environment_bounds(text, 4, "equation"), (2, 4))
        nested = text + "\\begin{equation}\n\\begin{equation}\nNested\n\\end{equation}\n"
        self.assertEqual(oracle.environment_bounds(nested, 2, "equation"), (0, 2))
        self.assertIsNone(oracle.environment_bounds(nested, 8, "equation"))

    def test_lexical_words_keep_visible_hidden_span_boundaries(self):
        text = "\\note[Quartzoption]{Quartznote}Quartzvisible % Quartzcomment\nQuartzafter\\label{Quartzkey}Quartzlast"
        words, uncertain, opaque_words, hidden = oracle.lexical_words(text)
        self.assertEqual(set(words), {"quartzvisible", "quartzafter", "quartzlast"})
        self.assertIn("quartznote", opaque_words)
        for word, spans in words.items():
            self.assertEqual(len(spans), 1)
            lo, hi = spans[0]
            self.assertEqual(text[lo:hi].lower(), word)
        self.assertTrue(any(lo <= text.index("Quartzkey") < hi for lo, hi in hidden))

    def test_absolute_compiler_paths_need_no_build_context(self):
        self.source.write_text("Quartzpositive\n")
        self.map.write_text(f"Input:1:{self.source}\n")
        with patch.object(oracle, "BUILD_DIRECTORY", None):
            self.assertEqual(oracle.read_source_map(self.pdf)["inputs"][1], self.source)

    def test_forward_contract_checks_page_not_arbitrary_raw_box_union(self):
        self.source.write_text("Quartzpositive\n")
        output = {"pdf": str(self.pdf), "page": 1, "h": 20, "v": 80, "width": 90, "height": 10}
        with patch.object(oracle, "raw_pages", return_value={1}), \
             patch.object(oracle, "pdfterm_bin", return_value=Path("unused")), \
             patch.object(oracle.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(output), "")):
            result = list(oracle.forward(self.pdf, self.source, "Quartzpositive\n", [(1, 1)], 1, source_map=self.mapping(1)))[0]
        self.assertTrue(result["ok"], result)

    def test_forward_page_hint_is_derived_from_sampled_source_not_the_answer(self):
        text = "Quartzpositive neighborcontext\n"
        visible = [["quartzpositive", "neighborcontext"], ["quartzother"]]
        for expected in [set(), {1, 2}]:
            for hint in [None, {"words": ["Quartzother"], "selected": 0}, {"words": [], "selected": 99}]:
                with self.subTest(expected=expected, hint=hint):
                    output = {"pdf": str(self.pdf), "page": 2, "h": 20, "v": 80, "width": 90, "height": 10}
                    if hint is not None:
                        output["word"] = hint
                    with patch.object(oracle, "raw_pages", return_value=expected), \
                         patch.object(oracle, "pdfterm_bin", return_value=Path("unused")), \
                         patch.object(oracle.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(output), "")):
                        bad = list(oracle.forward(self.pdf, self.source, text, [(1, 1)], 2, visible, source_map=self.mapping(1)))[0]
                    self.assertFalse(bad["ok"], bad)
                    self.assertFalse(bad.get("abstained"), bad)
                    self.assertEqual(bad.get("visible_alternatives"), [1], bad)
                    output["page"] = 1
                    with patch.object(oracle, "raw_pages", return_value=expected), \
                         patch.object(oracle, "pdfterm_bin", return_value=Path("unused")), \
                         patch.object(oracle.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(output), "")):
                        good = list(oracle.forward(self.pdf, self.source, text, [(1, 1)], 2, visible, source_map=self.mapping(1)))[0]
                    self.assertTrue(good["ok"], good)
                    self.assertTrue(good.get("visible_checked"), good)

    def test_unmapped_forward_without_independent_visible_context_abstains(self):
        for text, column, visible in [
            ("Quartzpositive\n", 1, None),
            ("Quartzpositive\n", 1, [["unrelated"]]),
            ("\\unknownwrapper{Quartzpositive}\n", 17, [["quartzpositive"]]),
        ]:
            with self.subTest(text=text, visible=visible):
                output = {"pdf": str(self.pdf), "page": 1, "h": 20, "v": 80, "width": 90, "height": 10}
                with patch.object(oracle, "raw_pages", return_value=set()), \
                     patch.object(oracle, "pdfterm_bin", return_value=Path("unused")), \
                     patch.object(oracle.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(output), "")):
                    result = list(oracle.forward(self.pdf, self.source, text, [(1, column)], 1, visible, source_map=self.mapping(1)))[0]
                self.assertFalse(result["ok"], result)
                self.assertTrue(result.get("mapping_ok"), result)
                self.assertEqual(result.get("reason"), "unverified_forward_source_context", result)
                self.assertTrue(result.get("abstained"), result)
                self.assertNotIn("error", result)


@unittest.skipUnless(os.name == "posix", "requires owned POSIX sessions and masks")
class SupervisionRegressions(unittest.TestCase):
    def isolated(self, source, *arguments):
        env = dict(os.environ)
        env["PYTHONPATH"] = str(Path(synthetic.__file__).parent)
        return subprocess.Popen(
            [sys.executable, "-c", textwrap.dedent(source), *map(str, arguments)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            start_new_session=True, env=env,
        )

    def assert_owned_processes_gone(self, pids):
        alive = set(pids)
        until = time.monotonic() + 3
        while alive and time.monotonic() < until:
            for pid in tuple(alive):
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    alive.remove(pid)
            if alive:
                time.sleep(0.01)
        self.assertFalse(alive, f"owned processes survived: {sorted(alive)}")

    def test_launcher_does_not_import_cwd_or_pythonpath_shadow_modules(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cwd, pythonpath = root / "cwd", root / "pythonpath"
            cwd.mkdir()
            pythonpath.mkdir()
            for directory in (cwd, pythonpath):
                with self.subTest(directory=directory.name):
                    marker = root / "shadow-executed"
                    module = directory / "json.py"
                    module.write_text(
                        f"open({str(marker)!r}, 'w').write('shadow executed')\n"
                        "raise RuntimeError('shadow json imported')\n", encoding="utf-8",
                    )
                    command = [
                        sys.executable, "-c",
                        "import os,sys; print(os.getcwd()); print(os.environ['PYTHONPATH']); "
                        "print(repr(sys.argv[1:]))",
                        "two words", "quartz-α",
                    ]
                    with patch.dict(os.environ, {"PYTHONPATH": str(pythonpath)}):
                        result = synthetic.run_oracle(command, timeout=2, cwd=cwd)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.args, command)
                    self.assertEqual(result.stdout.splitlines(), [
                        str(cwd.resolve()), str(pythonpath), repr(command[3:]),
                    ])
                    self.assertFalse(marker.exists(), "launcher executed an untrusted shadow module")
                    module.unlink()

    def test_sigint_at_real_reap_boundary_never_signals_released_group(self):
        runner = self.isolated('''
            import os,signal,subprocess,sys,threading
            from unittest.mock import patch
            import fuzz_synthetic as synthetic
            original_status=subprocess.Popen._handle_exitstatus
            original_killpg=os.killpg
            original_mask=signal.pthread_sigmask(signal.SIG_BLOCK,[])
            original_handler=signal.getsignal(signal.SIGINT)
            original_threads=set(threading.enumerate())
            released=False
            def status(process, value):
                global released
                try: os.waitpid(process.pid,os.WNOHANG)
                except ChildProcessError: released=True
                else: raise AssertionError('status hook did not follow a real reap')
                signal.raise_signal(signal.SIGINT)
                return original_status(process,value)
            def killpg(pid, signum):
                assert not released, 'signaled a released process group'
                return original_killpg(pid,signum)
            with patch.object(subprocess.Popen,'_handle_exitstatus',status), patch.object(os,'killpg',killpg):
                try: synthetic.run_oracle([sys.executable,'-c','print("completed")'],timeout=2)
                except KeyboardInterrupt: pass
                else: raise AssertionError('SIGINT was not delivered')
            assert released
            assert signal.getsignal(signal.SIGINT)==original_handler
            assert signal.pthread_sigmask(signal.SIG_BLOCK,[])==original_mask
            assert set(threading.enumerate())==original_threads
        ''')
        try:
            stdout, stderr = runner.communicate(timeout=8)
            self.assertEqual(runner.returncode, 0, stderr)
        finally:
            if runner.poll() is None:
                os.killpg(runner.pid, signal.SIGKILL)
            runner.communicate(timeout=5)

    def test_closed_output_wait_remains_deadline_bounded_and_interruptible(self):
        for interrupted in (False, True):
            with self.subTest(interrupted=interrupted):
                runner = self.isolated('''
                    import json,os,signal,subprocess,sys,tempfile,time
                    from pathlib import Path
                    import fuzz_synthetic as synthetic
                    interrupted=sys.argv[1]=='True'
                    with tempfile.TemporaryDirectory() as tmp:
                        ready=Path(tmp)/'ready'
                        command=[sys.executable,'-c',
                            'import os,pathlib,signal,sys,time; os.close(1); os.close(2); '
                            'pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); '
                            'time.sleep(0.1); '
                            +('os.kill(os.getppid(),signal.SIGINT); ' if interrupted else '')
                            +'time.sleep(30)',str(ready)]
                        started=time.monotonic()
                        try: synthetic.run_oracle(command,timeout=5 if interrupted else 0.75)
                        except (KeyboardInterrupt if interrupted else subprocess.TimeoutExpired): pass
                        else: raise AssertionError('closed-output child escaped supervision')
                        assert time.monotonic()-started<3, 'wait was not responsive'
                        pid=int(ready.read_text())
                        try: os.kill(pid,0)
                        except ProcessLookupError: pass
                        else: raise AssertionError('owned child survived')
                ''', interrupted)
                try:
                    stdout, stderr = runner.communicate(timeout=8)
                    self.assertEqual(runner.returncode, 0, stderr)
                finally:
                    if runner.poll() is None:
                        os.killpg(runner.pid, signal.SIGKILL)
                    runner.communicate(timeout=5)

    def test_compile_deadline_and_sigint_kill_compiler_descendants(self):
        for interrupted in (False, True):
            with self.subTest(interrupted=interrupted), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                compiler = root / "pdflatex"
                compiler.write_text(
                    f"#!{sys.executable}\n"
                    "import json,os,pathlib,subprocess,sys\n"
                    "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])\n"
                    "ready=pathlib.Path('compiler-pids.json'); staging=ready.with_suffix('.ready')\n"
                    "staging.write_text(json.dumps([os.getpid(),child.pid])); staging.replace(ready)\n"
                    "child.wait()\n", encoding="utf-8",
                )
                compiler.chmod(0o755)
                tex = root / "generic.tex"
                tex.write_text("generic fixture", encoding="utf-8")
                pidfile = root / "compiler-pids.json"
                runner = self.isolated('''
                    import os,signal,subprocess,sys
                    from pathlib import Path
                    import fuzz_synthetic as synthetic
                    root=Path(sys.argv[1]); interrupted=sys.argv[2]=='True'
                    os.environ['PATH']=str(root)+os.pathsep+os.environ['PATH']
                    original=synthetic.run_oracle
                    if not interrupted:
                        def bounded(command, timeout=600, cwd=None):
                            assert timeout==60, timeout
                            return original(command, timeout=0.75, cwd=cwd)
                        synthetic.run_oracle=bounded
                    handler=signal.getsignal(signal.SIGINT)
                    mask=signal.pthread_sigmask(signal.SIG_BLOCK, [])
                    try:
                        synthetic.compile_tex(root/'generic.tex')
                    except (KeyboardInterrupt if interrupted else subprocess.TimeoutExpired):
                        assert signal.getsignal(signal.SIGINT)==handler
                        assert signal.pthread_sigmask(signal.SIG_BLOCK, [])==mask
                        print('compiler tree stopped',flush=True)
                    else:
                        raise AssertionError('compiler did not fail')
                ''', root, interrupted)
                pids = []
                try:
                    until = time.monotonic() + 5
                    while not pidfile.exists() and time.monotonic() < until:
                        time.sleep(0.01)
                    self.assertTrue(pidfile.exists(), "fake compiler did not launch its child")
                    pids = json.loads(pidfile.read_text())
                    if interrupted:
                        os.kill(runner.pid, signal.SIGINT)
                    stdout, stderr = runner.communicate(timeout=8)
                    self.assertEqual(runner.returncode, 0, stderr)
                    self.assertEqual(stdout, "compiler tree stopped\n")
                    self.assert_owned_processes_gone(pids)
                finally:
                    try:
                        if runner.returncode is None:
                            # The runner must stop its separately owned compiler session first.
                            try:
                                os.kill(runner.pid, signal.SIGINT)
                            except ProcessLookupError:
                                pass
                            runner.communicate(timeout=3)
                    finally:
                        if not pids and pidfile.exists():
                            pids = json.loads(pidfile.read_text())
                        if pids:
                            try:
                                group = os.getpgid(pids[1])
                                if group in (pids[0], runner.pid):
                                    os.killpg(group, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        if runner.returncode is None:
                            os.killpg(runner.pid, signal.SIGKILL)
                        runner.communicate(timeout=5)

    def test_mask_and_handler_restored_after_partial_signal_operations(self):
        # Fault entry or restoration after a real reap, never a mask-call ordinal.
        for phase in ('entry_interrupt', 'cleanup_interrupt', 'cleanup_block_error', 'handler_restore_error'):
            with self.subTest(phase=phase):
                runner = self.isolated('''
                    import os,signal,sys
                    from unittest.mock import patch
                    import fuzz_synthetic as synthetic
                    phase=sys.argv[1]
                    def prior(signum, frame): raise KeyboardInterrupt('prior handler')
                    signal.signal(signal.SIGINT, prior)
                    signal.signal(signal.SIGUSR1, lambda signum, frame: (_ for _ in ()).throw(KeyboardInterrupt('after mask change')))
                    signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR2})
                    real_mask=signal.pthread_sigmask; real_signal=signal.signal
                    original_mask=real_mask(signal.SIG_BLOCK, [])
                    real_popen=synthetic.subprocess.Popen
                    processes=[]
                    fired=False
                    def launch(*args, **kwargs):
                        process=real_popen(*args, **kwargs)
                        processes.append(process)
                        return process
                    def mask(how, values):
                        global fired
                        result=real_mask(how, values)
                        boundary=(not processes if phase=='entry_interrupt' else
                                  bool(processes) and processes[0].returncode is not None)
                        if not fired and boundary and how==signal.SIG_BLOCK and signal.SIGINT in values:
                            fired=True
                            if phase.endswith('_interrupt'): signal.raise_signal(signal.SIGUSR1)
                            if phase=='cleanup_block_error': raise OSError('cleanup block failed')
                        return result
                    def handler(signum, action):
                        result=real_signal(signum, action)
                        if phase=='handler_restore_error' and action is prior:
                            raise OSError('handler restore failed')
                        return result
                    try:
                        with patch.object(signal,'pthread_sigmask',side_effect=mask), patch.object(signal,'signal',side_effect=handler), patch.object(synthetic.subprocess,'Popen',side_effect=launch):
                            synthetic.run_oracle([sys.executable,'-c','print("child completed")'],timeout=2)
                    except (KeyboardInterrupt if phase.endswith('_interrupt') else OSError) as error:
                        expected=('after mask change' if phase.endswith('_interrupt') else
                                  'cleanup block failed' if phase=='cleanup_block_error' else 'handler restore failed')
                        assert str(error)==expected, repr(error)
                    else: raise AssertionError('signal operation failure was hidden')
                    assert signal.getsignal(signal.SIGINT) is prior
                    assert real_mask(signal.SIG_BLOCK,[])==original_mask
                ''', phase)
                try:
                    stdout, stderr = runner.communicate(timeout=8)
                    self.assertEqual(runner.returncode, 0, stderr)
                finally:
                    if runner.poll() is None:
                        os.killpg(runner.pid, signal.SIGKILL)
                    runner.communicate(timeout=5)

    def test_original_handlers_and_child_mask_are_preserved(self):
        runner = self.isolated('''
            import json,signal,sys
            import fuzz_synthetic as synthetic
            def prior(signum, frame): raise KeyboardInterrupt('prior handler')
            for handler in (signal.SIG_IGN,signal.SIG_DFL,prior):
                for blocked in (False,True):
                    signal.signal(signal.SIGINT,handler)
                    mask={signal.SIGUSR2}|({signal.SIGINT} if blocked else set())
                    signal.pthread_sigmask(signal.SIG_SETMASK,mask)
                    command=[sys.executable,'-c',
                        'import json,signal; print(json.dumps([sorted(signal.pthread_sigmask(signal.SIG_BLOCK,[])),signal.getsignal(signal.SIGINT)==signal.SIG_IGN]))']
                    result=synthetic.run_oracle(command,timeout=2)
                    assert result.returncode==0, result.stderr
                    assert json.loads(result.stdout)==[sorted(mask),handler==signal.SIG_IGN], result.stdout
                    assert signal.getsignal(signal.SIGINT)==handler
                    assert signal.pthread_sigmask(signal.SIG_BLOCK,[])==mask
            signal.signal(signal.SIGINT,prior)
            signal.pthread_sigmask(signal.SIG_SETMASK,{signal.SIGUSR2})
            try:
                synthetic.run_oracle([sys.executable,'-c',
                    'import os,signal,time; os.kill(os.getppid(),signal.SIGINT); time.sleep(30)'],timeout=2)
            except KeyboardInterrupt as error:
                assert str(error)=='prior handler', error
            else: raise AssertionError('callable SIGINT handler was not delegated')
            assert signal.getsignal(signal.SIGINT) is prior
            assert signal.pthread_sigmask(signal.SIG_BLOCK,[])=={signal.SIGUSR2}
        ''')
        try:
            stdout, stderr = runner.communicate(timeout=20)
            self.assertEqual(runner.returncode, 0, stderr)
        finally:
            if runner.poll() is None:
                os.killpg(runner.pid, signal.SIGKILL)
            runner.communicate(timeout=5)

    def test_unknown_native_handler_rejected_without_mutating_signal_state(self):
        # getsignal documents None for an unknown C handler; Python cannot install one.
        runner = self.isolated('''
            import signal,sys,tempfile
            from pathlib import Path
            from unittest.mock import patch
            import fuzz_synthetic as synthetic
            handler=signal.getsignal(signal.SIGINT)
            signal.pthread_sigmask(signal.SIG_BLOCK,{signal.SIGUSR2})
            mask=signal.pthread_sigmask(signal.SIG_BLOCK,[])
            with tempfile.TemporaryDirectory() as tmp:
                sentinel=Path(tmp)/'launched'
                with patch.object(signal,'getsignal',return_value=None):
                    try: synthetic.run_oracle([sys.executable,'-c',f'open({str(sentinel)!r},"w").close()'],timeout=2)
                    except RuntimeError as error: assert 'unknown native signal handler' in str(error)
                    else: raise AssertionError('unknown handler was silently accepted')
                assert not sentinel.exists()
            assert signal.getsignal(signal.SIGINT)==handler
            assert signal.pthread_sigmask(signal.SIG_BLOCK,[])==mask
        ''')
        try:
            stdout, stderr = runner.communicate(timeout=8)
            self.assertEqual(runner.returncode, 0, stderr)
        finally:
            if runner.poll() is None:
                os.killpg(runner.pid, signal.SIGKILL)
            runner.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
