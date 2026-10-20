use super::*;

#[test]
fn caption_review_embedded_comment_end_does_not_expose_hidden_caption() {
    let source = "\\begin{comment}\nignored \\end{comment}\n\
                  \\caption{hidden\ncaption}\n\\end{comment}";
    assert_eq!(caption_closing_line(source, 3, 10), None);
}

#[test]
fn caption_review_comment_keys_cannot_become_inverse_word_targets() {
    let source = "\\newcommand{\\Printed}{Zephyr}\n\
                  \\begin{comment}\n\
                  \\label{Zephyr}\n\
                  \\end{comment}\n\
                  \\Printed";
    assert_eq!(source_word_location(source, 5, "Zephyr", 0, 4), None);
}

#[test]
fn caption_review_unbraced_short_brackets_do_not_own_adjacent_prose() {
    let source = "\\caption[short[inner]]{first\nsecond}";
    let column = source.find("first").unwrap() as u32 + 1;
    assert_eq!(caption_closing_line(source, 1, column), None);
    assert_eq!(math::caption_lines(source, 2), None);
    let source = "\\caption[{short[inner]}]{first\nsecond}";
    let column = source.find("first").unwrap() as u32 + 1;
    assert_eq!(caption_closing_line(source, 1, column), Some(2));
    assert_eq!(math::caption_lines(source, 2), Some(0..2));
}

#[test]
fn optional_arguments_shield_braces_comments_and_control_symbols() {
    let source = "[short[inner]] trailing";
    assert_eq!(
        optional_argument_end(source, 0),
        Some(source.find(']').unwrap() + 1)
    );
    for source in [
        "[{short[inner]}]",
        "[% ignored ]\n{brace]} \\] tail]",
        "[escaped \\{ brace]",
    ] {
        assert_eq!(optional_argument_end(source, 0), Some(source.len()));
    }
    assert_eq!(optional_argument_end("[{unclosed]", 0), None);
    assert!(
        nonprinting_source_ranges("\\begin{verbatim}\n\\label{printed}\n\\end{verbatim}")
            .is_empty()
    );
    let source = "\\begin{comment}\n\\label{hidden}\n\\end{comment}";
    assert_eq!(nonprinting_source_ranges(source), vec![0..source.len()]);
}

#[test]
fn comment_closure_requires_a_complete_subsequent_input_line() {
    for fake in [
        "ignored \\end{comment}",
        "\\end{comment} ignored",
        " \\end{comment}",
    ] {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "\\begin{{comment}}{newline}{fake}{newline}\\caption{{hidden{newline}caption}}{newline}\\end{{comment}}{newline}\\caption{{visible{newline}caption}}"
            );
            assert_eq!(caption_closing_line(&source, 3, 10), None);
            assert_eq!(caption_closing_line(&source, 6, 10), Some(7));
            let unclosed = format!(
                "\\begin{{comment}}{newline}{fake}{newline}\\caption{{hidden{newline}caption}}"
            );
            assert_eq!(caption_closing_line(&unclosed, 3, 10), None);
        }
    }
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "\\begin{{comment}}\\end{{comment}}{newline}\\caption{{hidden{newline}caption}}{newline}\\end{{comment}}   {newline}\\caption{{visible{newline}caption}}"
        );
        assert_eq!(caption_closing_line(&source, 2, 10), None);
        assert_eq!(caption_closing_line(&source, 5, 10), Some(6));
    }
}

#[test]
fn forward_caption_anchor_uses_only_complete_long_argument() {
    for caption in [
        "\\caption",
        "\\caption*",
        "\\caption[short {nested}]",
        "\\caption*[short]",
    ] {
        let source = format!(
            "before\n{caption}% argument follows\n{{école first\nsecond \\emph{{literal}}\n}}\nafter"
        );
        assert_eq!(caption_closing_line(&source, 3, 2), Some(5));
        assert_eq!(caption_closing_line(&source, 4, 1), Some(5));
        for (line, column) in [(1, 1), (2, 1), (3, 13), (5, 1), (6, 1), (0, 1), (3, 0)] {
            assert_eq!(caption_closing_line(&source, line, column), None);
        }
    }
    for source in [
        "\\caption[short]{unclosed\nbody",
        "\\caption[unclosed{body}",
        "\\caption{body\n} trailing",
    ] {
        let last = source.lines().count() as u32;
        let column = source.lines().last().unwrap().chars().count() as u32;
        assert_eq!(caption_closing_line(source, last, column), None);
    }
}

#[test]
fn forward_caption_anchor_excludes_nonprinting_and_opaque_source() {
    let source = "\\newcommand{\\Hidden}{\\caption{definition}}\n\
                  \\def\\Other{\\caption{primitive}}\n\
                  \\verb|\\caption{inline}|\n\
                  \\begin{verbatim}\n\\caption{verbatim}\n\\end{verbatim}\n\
                  \\caption[short]{visible \\label{key} % comment\n\
                  \\verb|opaque| \\newcommand{\\Inner}{nested} final}\n\
                  % \\caption{commented}\n\
                  \\label{\\caption{label}}\n";
    for word in [
        "definition",
        "primitive",
        "inline",
        "verbatim}",
        "short",
        "key",
        "comment",
        "opaque",
        "nested",
        "commented",
        "label",
    ] {
        let at = source.find(word).unwrap();
        let line = source[..at].bytes().filter(|ch| *ch == b'\n').count() as u32 + 1;
        let start = source[..at].rfind('\n').map_or(0, |end| end + 1);
        let column = source[start..at].chars().count() as u32 + 1;
        assert_eq!(caption_closing_line(source, line, column), None, "{word}");
    }
    assert_eq!(caption_closing_line(source, 7, 17), Some(8));
    let source = "\\begin{frame}\n\\caption{first\nsecond}\n\\end{frame}";
    assert_eq!(caption_closing_line(source, 2, 10), Some(3));
    assert_eq!(
        frame_closing_line(source, 2).or_else(|| caption_closing_line(source, 2, 10)),
        Some(4),
    );
}

#[test]
fn explicit_inverse_guess_preserves_compiled_coordinates_and_warns_on_ambiguity() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let source = root.join("edited.tex");
    fs::write(&source, "only one current line\n").unwrap();
    let pdf = root.join("paper.pdf");
    let point = InversePoint {
        page: 1,
        x: 10.0,
        y_from_top: 20.0,
        page_height_pt: 100.0,
    };
    let output = format!(
        "Input:{}\nLine:9\nInput:{}\nLine:12\n",
        source.display(),
        source.display()
    );
    let guessed = resolve_inverse_output(
        &pdf,
        point,
        Some(("current", 0)),
        4,
        &Operation::default(),
        &output,
        QuerySources {
            best_guess: true,
            ..QuerySources::default()
        },
    )
    .unwrap();
    assert_eq!(guessed.location.line, 9);
    assert_eq!(
        (
            guessed.location.byte_column,
            guessed.location.column,
            guessed.location.column_char
        ),
        (0, 1, 1)
    );
    assert!(!guessed.location.precise);
    assert!(guessed.warning.unwrap().contains("multiple coarse"));
    assert!(
        resolve_inverse_output(
            &pdf,
            point,
            None,
            4,
            &Operation::default(),
            &output,
            QuerySources::default(),
        )
        .is_err()
    );
    fs::write(&source, "current source line\n".repeat(12)).unwrap();
    assert!(
        resolve_inverse_output(
            &pdf,
            point,
            None,
            4,
            &Operation::default(),
            &output,
            QuerySources::default(),
        )
        .err()
        .unwrap()
        .to_string()
        .contains("multiple coarse")
    );
    let operation = Operation::default();
    operation.cancel();
    assert!(
        resolve_inverse_output(
            &pdf,
            point,
            None,
            4,
            &operation,
            &output,
            QuerySources {
                best_guess: true,
                ..QuerySources::default()
            },
        )
        .is_err()
    );
}

#[test]
fn pdf_revision_rejects_rewrites_and_atomic_replacements() {
    let root = tempfile::tempdir().unwrap();
    let pdf = root.path().join("paper.pdf");
    fs::write(&pdf, b"first").unwrap();
    let first = PdfRevision::read(&pdf).unwrap();
    first.check(&pdf).unwrap();
    fs::write(&pdf, b"second").unwrap();
    assert!(first.check(&pdf).is_err());
    let second = PdfRevision::read(&pdf).unwrap();
    let replacement = root.path().join("replacement.pdf");
    fs::write(&replacement, b"second").unwrap();
    fs::rename(replacement, &pdf).unwrap();
    assert!(second.check(&pdf).is_err());
}
#[test]
fn inverse_word_matching_resolves_context_and_utf8_byte_columns() {
    let source = "A repeated word far away.\nÉmile uses \\emph{repeated} words near fields.\nRepeated noise.\n";
    let context = "Émile uses repeated words near ﬁelds.";
    assert_eq!(
        super::source_word_location(source, 2, context, context.find("repeated").unwrap(), 4),
        Some((2, source.lines().nth(1).unwrap().find("repeated").unwrap()))
    );
    assert_eq!(
        super::source_word_location(source, 3, context, context.find("ﬁelds").unwrap(), 4),
        Some((2, source.lines().nth(1).unwrap().find("fields").unwrap()))
    );
    assert_eq!(
        super::source_word_location("word word", 1, "word", 1, 4),
        None
    );
    assert_eq!(
        super::source_word_location("\\word % word", 1, "word", 1, 4),
        None
    );
    assert_eq!(
        super::source_word_location("word", 1, "missing", 1, 4),
        None
    );
    let boundary = "one\ntwo\nthree\nfour\nfive\nsix";
    assert_eq!(
        super::source_word_location(boundary, 1, "five", 1, 4),
        Some((5, 0))
    );
    assert_eq!(super::source_word_location(boundary, 1, "six", 1, 4), None);
}

#[test]
fn inverse_frame_matching_reaches_prose_without_crossing_frames() {
    let source = "\\begin{frame}\nSome samples display patterns.\n\\pause\n\
                  \\includegraphics{samples/image.pdf}\n\n\n\n\n\
                  \\only<2>{Other visible text.}\n\\end{frame}\n\
                  \\begin{frame}\nSome samples display patterns.\n\\end{frame}";
    assert_eq!(
        source_word_location(source, 10, "Some samples display patterns.", 5, 4),
        Some((2, 5))
    );
    assert_eq!(
        source_word_location(source, 13, "Some samples display patterns.", 5, 4),
        Some((12, 5))
    );
    assert_eq!(
        source_word_location(source, 10, "samples", 1, 4),
        Some((2, 5))
    );
    assert_eq!(source_word_location(source, 13, "Other", 1, 4), None);
}

#[test]
fn inverse_frame_matching_keeps_duplicate_overlays_line_only() {
    let source = "\\begin{frame}\n\\only<1>{Identical visible phrase.}\n\
                  \\only<2>{Identical visible phrase.}\n\\end{frame}";
    assert_eq!(
        source_word_location(source, 4, "Identical visible phrase.", 10, 4),
        None
    );
}

#[test]
fn inverse_title_word_survives_unrelated_opaque_math() {
    let source = "\\begin{frame}[fragile]{Let's review this section}\n\
                  Ordinary body words:\n\
                  \\begin{equation*}\n\
                  \\custom{x}\n\
                  \\end{equation*}\n\
                  \\end{frame}";
    let context = "Let’s review this section\r\nOrdinary body words:";
    assert_eq!(
        source_prose_scored_location(
            source,
            6,
            context,
            0,
            0..6,
            true,
            &nonprinting_source_ranges(source)
        )
        .map(|(row, byte, _)| (row, byte)),
        Some((1, 23))
    );
    assert_eq!(
        source_word_location(source, 6, context, 0, 4),
        Some((1, 23))
    );
}

#[test]
fn inverse_title_words_prefer_the_frame_header_when_body_repeats_them() {
    let source = "\\begin{frame}{Overview}\n\
                  An overview follows in the body.\n\
                  $\\custom{x}$\n\
                  \\end{frame}";
    assert_eq!(
        source_word_location(source, 4, "Overview\r\n0.0 0.2", 6, 4),
        Some((1, 14))
    );
    assert_eq!(
        source_word_location(source, 4, "An overview follows", 3, 4),
        Some((2, 3))
    );
    let source = "\\begin{frame}{Primary Topic}{A small example}\n\
                  A different body sentence.\n\
                  $\\custom{x}$\n\
                  \\end{frame}";
    let context = "Primary Topic\r\nA small example\r\nTheorem";
    assert_eq!(
        source_word_location(source, 4, context, context.find("A small").unwrap(), 4),
        Some((
            1,
            source.lines().next().unwrap().find("{A small").unwrap() + 1
        ))
    );
}

#[test]
fn inverse_heading_matches_tex_accent_and_pdf_unicode() {
    let source = "\\begin{frame}{A note on \\v Zora's method}\n\
                  $x$\n\
                  \\end{frame}";
    let context = "A note on Žora’s method";
    assert_eq!(
        source_word_location(source, 3, context, context.find('Ž').unwrap(), 4),
        Some((1, source.lines().next().unwrap().find("Zora").unwrap()))
    );
}

#[test]
fn inverse_literal_words_inside_boxed_math_keep_their_source_spans() {
    let source = "\\begin{frame}{Example}\n\
                  \\[\\boxed{\\textnormal{Visible sentence inside the box.}}\\]\n\
                  \\[\\begin{aligned}N\\to\\infty\\end{aligned}\\]\n\
                  \\end{frame}";
    let context = "Example\r\nVisible sentence inside the box.";
    assert_eq!(
        source_word_location(source, 4, context, context.find("Visible").unwrap(), 4),
        Some((2, source.lines().nth(1).unwrap().find("Visible").unwrap()))
    );
}

#[test]
fn inverse_prose_word_does_not_match_inside_longer_math_word() {
    let source = "\\begin{frame}{Example}\n\
                  Pick an option here.\n\
                  $\\mathrm{constant}(x)$ $\\custom{q}$\n\
                  \\end{frame}";
    let context = "Pick an option here. constant(x)";
    assert_eq!(
        source_word_location(source, 4, context, context.find("an option").unwrap(), 4),
        Some((2, 5))
    );
}

#[test]
fn inverse_caption_word_ignores_image_filename() {
    let source = "\\begin{frame}{Example}\n\
                  \\includegraphics[width=\\textwidth]{assets/sample_red_blue_boxes.jpg}\n\
                  \\caption{Sample red blue boxes.}\n\
                  \\end{frame}";
    let context = "Figure 1: Sample red blue boxes.";
    assert_eq!(
        source_word_location(source, 4, context, context.find("red").unwrap(), 4),
        Some((3, source.lines().nth(2).unwrap().find("red").unwrap()))
    );
    assert_eq!(source_word_location(source, 4, "assets", 0, 4), None);
}

#[test]
fn inverse_prose_excludes_nonprinting_keys_and_multiline_definitions() {
    let source = "\\newcommand{\\Hidden}{\nZephyr\n}\n\
                  \\Hidden\\label% keep key on the next line\n{Zephyr}\n\
                  \\cite[PrintedNote]{Zephyr}\\ref{Zephyr}\\href{Zephyr}{DisplayAmber}\n\
                  \\newcommand{\\Unused}{Zephyr} éé PrintedCopper\n";
    assert_eq!(source_word_location(source, 5, "Zephyr", 0, 10), None);
    for (word, row) in [
        ("PrintedNote", 6),
        ("DisplayAmber", 6),
        ("PrintedCopper", 7),
    ] {
        let byte = source.lines().nth(row - 1).unwrap().find(word).unwrap();
        assert_eq!(
            source_word_location(source, row as u32, word, 0, 10),
            Some((row as u32, byte))
        );
    }
    let source = "\\def\\Hidden#1{Zephyr {NestedKey}} PrintedCopper\n";
    for word in ["Zephyr", "NestedKey"] {
        assert_eq!(source_word_location(source, 1, word, 0, 4), None);
    }
    assert_eq!(
        source_word_location(source, 1, "PrintedCopper", 0, 4),
        Some((1, source.find("PrintedCopper").unwrap()))
    );
    let source = "\\newcommand{\\Hidden}{$Zephyr$}\n\\Hidden\\label{Zephyr}\n";
    assert_eq!(source_word_location(source, 2, "Zephyr", 0, 4), None);
}

#[test]
fn inverse_repeated_prose_uses_words_past_adjacent_math() {
    let source = "\\begin{frame}{Example}\n\
                  \\item the \\blue{$A_i$}s are sample elements in $\\mathcal{A}$, a list of tokens,\n\
                  \\item the \\blue{$B_i$}s are sample elements in $\\mathcal{B}$, a list of labels,\n\
                  \\end{frame}";
    let context = "the Ais are sample elements in A, a list of tokens,\r\n\
                   the Bis are sample elements in B, a list of labels,";
    for (needle, line) in [("tokens", 2), ("labels", 3)] {
        let at = context.find(needle).unwrap();
        let selected = context[..at].rfind("elements").unwrap();
        assert_eq!(
            source_word_location(source, 4, context, selected, 4),
            Some((
                line,
                source
                    .lines()
                    .nth(line as usize - 1)
                    .unwrap()
                    .find("elements")
                    .unwrap()
            ))
        );
    }
}

#[test]
fn inverse_prose_context_beats_unrelated_math_word() {
    let source = "\\begin{frame}{Example}\n\
                  The list defines a \\emph{value} $(a_1, a_2)$ on a set.\n\
                  \\begin{equation*}\\mathrm{value}(f)=\\mathrm{value}(g)\\end{equation*}\n\
                  \\end{frame}";
    let context = "The list defines a value (a1, a2) on a set.";
    assert_eq!(
        source_word_location(source, 4, context, context.find("value").unwrap(), 4),
        Some((2, source.lines().nth(1).unwrap().find("value").unwrap()))
    );
}

#[test]
fn inverse_repeated_word_uses_short_context_across_math() {
    let source = "\\begin{frame}{Example}\n\
                  It is valid on $\\mathrm{part}(f)$ as soon\n\
                  as $\\mathrm{t}(g)$ is valid.\n\
                  \\end{frame}";
    let context = "It is valid on part(f) as soon\r\nas t(g) is valid.";
    let selected = context.rfind("valid").unwrap();
    assert_eq!(
        source_word_location(source, 4, context, selected, 4),
        Some((3, source.lines().nth(2).unwrap().find("valid").unwrap()))
    );
}

#[test]
fn inverse_ordinary_word_rejects_a_different_source_word() {
    let source = "The item is valid and has a simple label.";
    let byte = source.find("valid").unwrap();
    assert_eq!(
        ordinary_word_source_start(
            source,
            "The item is active and has a simple label.",
            "The item is ".len(),
            1,
            byte,
        ),
        None
    );
    assert_eq!(
        ordinary_word_source_start(
            source,
            "The item is valid and has a simple label.",
            "The item is ".len(),
            1,
            byte + 2,
        ),
        Some(byte)
    );
}

#[test]
fn inverse_generated_verbatim_uses_clicked_sheet_original_frame() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("original.tex");
    let source = "\\begin {frame}\n[fragile]{First}\nEarlyZephyr.\nFirstCopper.\n\\end{frame}\n\
                  \\begin{frame}[fragile]{Second}\nLateNebula.\nSecondSilver.\n\\end{frame}\n";
    fs::write(&source_path, source).unwrap();
    let pdf = directory.path().join("build/deck.pdf");
    let generated = directory.path().join("build/deck.vrb");
    let contents = format!(
        "SyncTeX Version:1\nInput:1:{}\nInput:2:{}\n\
         Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n\
         {{1\n[1,5:0,0:1,1,0\nx2,2:0,0\n]\n}}1\n\
         Input:3:{}\n{{2\n[1,9:0,0:1,1,0\nx3,2:0,0\n]\n}}2\n\
         Postamble:\nCount:8\nPost scriptum:\n",
        source_path.display(),
        generated.display(),
        generated.display()
    );
    for (page, anchor) in [(1, 5), (2, 9)] {
        let map = source_map_from_reader(
            contents.as_bytes(),
            &pdf,
            InversePoint {
                page,
                x: 0.0,
                y_from_top: 0.0,
                page_height_pt: 1.0,
            },
            &generated,
            false,
            &Operation::default(),
            None,
        )
        .unwrap();
        let remapped = original_source_from_verbatim(
            &map,
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .unwrap();
        assert_eq!(
            remapped.0,
            fs::canonicalize(&source_path).unwrap().to_str().unwrap()
        );
        assert_eq!(remapped.1.as_str(), source);
        assert_eq!(remapped.2, anchor);
    }
    let missing = source_map_from_reader(
        contents.as_bytes(),
        &pdf,
        InversePoint {
            page: 1,
            x: 0.0,
            y_from_top: 0.0,
            page_height_pt: 1.0,
        },
        Path::new("/unrelated.vrb"),
        false,
        &Operation::default(),
        None,
    )
    .unwrap();
    assert!(
        original_source_from_verbatim(
            &missing,
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .is_err()
    );
}

#[test]
fn inverse_body_ownership_uses_specific_boxes_and_effective_transform() {
    let sheet = "SyncTeX Version:1\nInput:1:/source.tex\nInput:2:/deck.vrb\n\
                 Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n\
                 {2\n[1,18:0,100:100,100,0\n\
                 (1,18:0,100:100,100,0\n(1,18:0,100:100,100,0\n)\n)\n\
                 (1,18:10,20:30,10,0\nx2,2:20,20\n)\n\
                 (1,18:70,90:20,10,0\nx1,18:80,90\n)\n\
                 ]\n}2\nPostamble:\nCount:12\nPost scriptum:\n";
    for (has_post_scriptum, post, magnification, x_offset, y_offset) in [
        (true, "", 1.0, 0.0, 0.0),
        (false, "", 1.0, 0.0, 0.0),
        (
            true,
            "Magnification:2\nX Offset:10bp\nY Offset:20bp\n",
            2.0,
            10.0,
            20.0,
        ),
    ] {
        let header = if has_post_scriptum {
            sheet
        } else {
            sheet.strip_suffix("Post scriptum:\n").unwrap()
        };
        let contents = format!("{header}{post}");
        let compressed = contents
            .replacen(
                "{2\n[1,18:0,100",
                "{1\n[1,1:0,200:100,200,0\nx1,1:0,100\n]\n}1\n{2\n[1,18:0,=",
                1,
            )
            .replace("(1,18:10,20:", "$1,18:0,20\n(1,18:10,=:");
        let compressed_form = contents.replace("(1,18:10,20:", "f481:0,20\n(1,18:10,=:");
        let form_only_footer = contents.replace("x1,18:80,90", "f2:80,90");
        let form_without_generated = contents.replace("x2,2:20,20", "f2:20,20");
        for (contents, x, y, body) in [
            (&contents, 20.0, 15.0, true),
            (&contents, 80.0, 85.0, false),
            (&contents, 50.0, 50.0, false),
            (&compressed, 20.0, 15.0, true),
            (&compressed_form, 20.0, 15.0, true),
            (&form_only_footer, 80.0, 85.0, false),
            (&form_without_generated, 20.0, 15.0, false),
        ] {
            let map = source_map_from_reader(
                contents.as_bytes(),
                Path::new("/deck.pdf"),
                InversePoint {
                    page: 2,
                    x: (x * 65536.0 / 65781.76 * magnification + x_offset) as f32,
                    y_from_top: (y * 65536.0 / 65781.76 * magnification + y_offset) as f32,
                    page_height_pt: 200.0,
                },
                Path::new("/deck.vrb"),
                true,
                &Operation::default(),
                None,
            )
            .unwrap();
            assert_eq!(
                map.generated_at_point, body,
                "post={post:?}, point=({x},{y})"
            );
        }
    }
    for contents in [
        sheet.replace("Unit:65536\n", ""),
        sheet.replace("(1,18:10,20:30,10,0", "(1,18:invalid"),
        sheet.replace("Count:12\n", ""),
        sheet.replace("x2,2:20,20", "finvalid:20,20"),
        format!(
            "{}<2\n(2,2:0,20:30,10,0\n)\n>\n",
            sheet.replace("x2,2:20,20", "f2:20,20")
        ),
    ] {
        assert!(
            source_map_from_reader(
                contents.as_bytes(),
                Path::new("/deck.pdf"),
                InversePoint {
                    page: 2,
                    x: 20.0,
                    y_from_top: 15.0,
                    page_height_pt: 200.0
                },
                Path::new("/deck.vrb"),
                true,
                &Operation::default(),
                None,
            )
            .is_err()
        );
    }
    let operation = Operation::default();
    let generated = SourceBox {
        bounds: [0.0, 0.0, 20.0, 20.0],
        generated: true,
    };
    for other in [
        SourceBox {
            bounds: generated.bounds,
            generated: false,
        },
        SourceBox {
            bounds: [10.0, 0.0, 30.0, 20.0],
            generated: false,
        },
    ] {
        assert!(!generated_box_at_point(&[generated, other], 15.0, 15.0, &operation).unwrap());
    }
}

#[test]
fn inverse_document_metadata_resolves_title_and_direct_input() {
    let directory = tempfile::tempdir().unwrap();
    let main_path = directory.path().join("original.tex");
    let main = "\\input{preamble.tex}\n\
                \\title[Example Report and Results]{Example Report and Results}\n\
                \\author{Ada Example}\n\
                \\begin{document}\n\
                \\begin{frame}{Other title}\n\
                Body text.\n\
                \\end{frame}\n";
    fs::write(&main_path, main).unwrap();
    fs::write(
        directory.path().join("preamble.tex"),
        "\\institute{North Research Center\\\\\nExample School for Mathematics}\n",
    )
    .unwrap();
    let found = document_metadata_word_location(
        &main_path,
        "Example Report and Results",
        "Example ".len(),
        &mut QuerySources::default(),
        &Operation::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!((found.2, found.3), (2, 43));
    assert!(found.4 >= 6);
    let found = document_metadata_word_location(
        &main_path,
        "North Research Center\r\nExample School for Mathematics",
        "North Research Center\r\nExample School for ".len(),
        &mut QuerySources::default(),
        &Operation::default(),
    )
    .unwrap()
    .unwrap();
    assert!(found.0.ends_with("preamble.tex"));
    assert_eq!((found.2, found.3), (2, 19));
    assert!(
        document_metadata_word_location(
            &main_path,
            "Report",
            0,
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn inverse_metadata_preserves_crlf_and_unicode_byte_offsets() {
    let directory = tempfile::tempdir().unwrap();
    let main_path = directory.path().join("original.tex");
    let declaration = "\\newcommand{\\unused}{éééé}\\title{Example Report}";
    for newline in ["\n", "\r\n"] {
        let mut source = format!("\\documentclass{{beamer}}{newline}");
        for _ in 0..7 {
            source.push_str(&format!("% padding{newline}"));
        }
        source.push_str(declaration);
        fs::write(&main_path, &source).unwrap();
        let found = document_metadata_word_location(
            &main_path,
            "Example Report",
            8,
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!((found.2, found.3), (9, declaration.find("Report").unwrap()));
    }
}

#[test]
fn inverse_frame_title_beats_same_words_in_document_title() {
    let directory = tempfile::tempdir().unwrap();
    let main_path = directory.path().join("original.tex");
    let source = "\\title{Numerical Analysis and Applications in Computational Fluid Dynamics}\n\
                  \\begin{document}\n\
                  \\begin{frame}{Computational Fluid Dynamics}{Grid refinement application}\n\
                  \\begin{figure}\\includegraphics{grid.png}\\end{figure}\n\
                  \\end{frame}\n";
    fs::write(&main_path, source).unwrap();
    let title = "Computational Fluid Dynamics\nGrid refinement application\n\
                 Numerical Analysis and Applications in Computational Fluid Dynamics";
    let title_offset = title.find("Fluid").unwrap();
    let metadata_score = document_metadata_word_location(
        &main_path,
        title,
        title_offset,
        &mut QuerySources::default(),
        &Operation::default(),
    )
    .unwrap()
    .unwrap()
    .4;
    assert!(metadata_score >= 6);
    assert_eq!(
        source_word_location(source, 5, title, title_offset, 4),
        Some((3, source.lines().nth(2).unwrap().find("Fluid").unwrap()))
    );
    assert!(!metadata_beats_frame(
        source,
        5,
        title,
        title_offset,
        10.0,
        272.0,
        metadata_score
    ));
    let footer = "Computational Fluid Dynamics\nGrid refinement application\n\
                  Numerical Analysis and Applications in Computational Fluid Dynamics";
    let footer_offset = footer.rfind("Fluid").unwrap();
    let footer_score = document_metadata_word_location(
        &main_path,
        footer,
        footer_offset,
        &mut QuerySources::default(),
        &Operation::default(),
    )
    .unwrap()
    .unwrap()
    .4;
    assert!(metadata_beats_frame(
        source,
        5,
        footer,
        footer_offset,
        228.0,
        272.0,
        footer_score
    ));
}

#[test]
fn inverse_identical_running_title_uses_click_height_for_tie() {
    let directory = tempfile::tempdir().unwrap();
    let main_path = directory.path().join("original.tex");
    let source = "\\title{Computational Fluid Dynamics}\n\
                  \\begin{frame}{Computational Fluid Dynamics}\n\
                  \\end{frame}\n";
    fs::write(&main_path, source).unwrap();
    let context = "Computational Fluid Dynamics";
    let offset = context.find("Fluid").unwrap();
    let metadata_score = document_metadata_word_location(
        &main_path,
        context,
        offset,
        &mut QuerySources::default(),
        &Operation::default(),
    )
    .unwrap()
    .unwrap()
    .4;
    assert_eq!(
        metadata_score,
        source_prose_scored_location(
            source,
            3,
            context,
            offset,
            1..3,
            true,
            &nonprinting_source_ranges(source)
        )
        .unwrap()
        .2
    );
    assert!(!metadata_beats_frame(
        source,
        3,
        context,
        offset,
        10.0,
        272.0,
        metadata_score
    ));
    assert!(metadata_beats_frame(
        source,
        3,
        context,
        offset,
        228.0,
        272.0,
        metadata_score
    ));
}

#[test]
fn inverse_frame_matching_ignores_commented_and_escaped_boundaries() {
    let source = "% \\begin{frame}\n\\begin {frame}\n\
                  % \\end{frame}\n\\\\end{frame}\n\
                  Before \\% percent target. % hidden\n\n\n\n\n\\end {frame}";
    assert_eq!(
        source_word_location(source, 10, "percent target", 9, 0),
        Some((5, 18))
    );
    assert_eq!(source_word_location(source, 10, "hidden", 1, 4), None);
    let unterminated = "\\begin{frame}\ntarget\n\n\n\n\n";
    assert_eq!(source_word_location(unterminated, 6, "target", 1, 0), None);
}

#[test]
fn inverse_caption_math_uses_literal_context_not_closing_line_proximity() {
    let source = "\\caption{Selected views.\n\
        sample points at measured offset at most \\(\\alpha_k\\) from the\n\
        baseline marker.\n\n\n\n\n\
        The vertical axis is in units of \\(\\alpha_k\\), hence rescaled.\n\
        End of caption.}\n";
    let line = source.lines().count() as u32;
    for (context, row) in [
        ("offset at most 𝛼𝑘 from the baseline marker.", 2),
        ("axis is in units of 𝛼𝑘, hence rescaled.", 8),
    ] {
        assert_eq!(
            source_word_location(source, line, context, context.find('𝛼').unwrap(), 4),
            Some((
                row,
                source
                    .lines()
                    .nth(row as usize - 1)
                    .unwrap()
                    .find("\\alpha")
                    .unwrap()
            ))
        );
    }
    // The glyph alone cannot identify which repeated expression was clicked.
    assert_eq!(source_word_location(source, line, "𝛼𝑘", 0, 4), None);
}

#[test]
fn inverse_inline_math_does_not_use_hidden_comment_context() {
    let source = "\\caption{\\emph{at most} \\(\\alpha_k\\)\n\
                  % at most\n\\(\\alpha_k\\)\n}";
    assert_eq!(
        source_word_location(source, 4, "at most 𝛼𝑘", "at most ".len(), 4),
        None
    );
}

fn inverse_output(
    pdf: &Path,
    stdout: &str,
    word: Option<(&str, usize)>,
) -> io::Result<InverseResolution> {
    resolve_inverse_output(
        pdf,
        InversePoint {
            page: 1,
            x: 72.0,
            y_from_top: 120.0,
            page_height_pt: 792.0,
        },
        word,
        0,
        &Operation::default(),
        stdout,
        QuerySources::default(),
    )
}

#[test]
fn inverse_no_match_and_competing_anchors_have_distinct_typed_failures() {
    let directory = tempfile::tempdir().unwrap();
    let pdf = directory.path().join("paper.pdf");
    fs::write(directory.path().join("paper.tex"), "Copper.\nZephyr.\n").unwrap();
    let no_match = inverse_output(&pdf, "SyncTeX result begin\nSyncTeX result end\n", None)
        .err()
        .expect("empty native results are not source ambiguity");
    assert_eq!(no_match.kind(), io::ErrorKind::NotFound);
    assert!(matches!(
        no_match
            .get_ref()
            .and_then(|error| error.downcast_ref::<InverseFailure>()),
        Some(InverseFailure::NoMatch)
    ));
    let ambiguous = inverse_output(
        &pdf,
        "Input: paper.tex\nLine: 1\nInput: paper.tex\nLine: 2\n",
        None,
    )
    .err()
    .expect("competing valid anchors are not no-match");
    assert_eq!(ambiguous.kind(), io::ErrorKind::InvalidData);
    assert!(matches!(
        ambiguous
            .get_ref()
            .and_then(|error| error.downcast_ref::<InverseFailure>()),
        Some(InverseFailure::Ambiguous(_))
    ));
}

#[test]
fn inverse_raw_hits_preserve_distinct_records_without_trailing_duplicates() {
    let stdout = "Output: paper.pdf\nInput: preamble.tex\nLine: 9\nColumn: -1\n\
                  Offset: 0\nContext: ignored\nInput: paper.tex\nLine: 3\n\
                  Column: -1\nInput: preamble.tex\nLine: 9\nColumn: -1\n";
    let targets = parse_synctex_edit(stdout);
    assert_eq!(
        targets
            .iter()
            .map(|target| (target.file.as_str(), target.line))
            .collect::<Vec<_>>(),
        vec![("preamble.tex", 9), ("paper.tex", 3)]
    );
    for incomplete in [
        "Output: paper.pdf\nLine: 3\n",
        "Output: paper.pdf\nInput: paper.tex\n",
        "Input: paper.tex\nLine: 0\n",
        "",
    ] {
        assert!(parse_synctex_edit(incomplete).is_empty());
    }
}

#[test]
fn inverse_unique_supported_position_beats_unrelated_coarse_hits_in_any_order() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("paper.tex");
    let caption = "\\caption{Mechanical measurements remain stable.}";
    fs::write(&source, format!("{caption}\nUnrelated coarse heading.\n")).unwrap();
    let supported = "Input: paper.tex\nLine: 1\nColumn: -1\n";
    let alias = "Input: ./paper.tex\nLine: 1\nColumn: -1\n";
    let coarse = "Input: paper.tex\nLine: 2\nColumn: -1\n";
    for stdout in [
        format!("{supported}{coarse}{alias}"),
        format!("{alias}{coarse}{supported}"),
        format!("{coarse}{supported}{alias}"),
    ] {
        let result = inverse_output(
            &directory.path().join("paper.pdf"),
            &stdout,
            Some(("Mechanical measurements remain stable", 0)),
        )
        .unwrap();
        assert_eq!(
            result.location.file,
            source.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(result.location.line, 1);
        assert_eq!(
            result.location.byte_column,
            caption.find("Mechanical").unwrap()
        );
        assert!(result.location.precise);
    }
}

#[test]
fn inverse_closing_layout_anchor_does_not_guess_a_neighboring_prose_row() {
    let source = "\\begin{tabular}{l}\n\
                  Group alpha \\(n\\); primary sample columns. \\\\\n\
                  \\\\\n\\\\\n\\\\\n\\\\\n\
                  Group beta \\(n\\); direct step. \\\\\n\
                  Group alpha \\(n\\); secondary columns for sample tables. \\\\\n\
                  \\end{tabular}\n";
    let first_context = "Group alpha 𝑛; primary sample columns.";
    assert_eq!(source_word_location(source, 9, first_context, 0, 4), None);
    assert_eq!(
        source_word_location(source, 2, first_context, 0, 4),
        Some((2, 0))
    );
    let tabularx_source = source
        .replace("\\begin{tabular}{l}", "\\begin{tabularx}{\\linewidth}{X}")
        .replace("\\end{tabular}", "\\end{tabularx}");
    assert_eq!(
        source_word_location(&tabularx_source, 9, first_context, 0, 4),
        Some((2, 0))
    );
    let directory = tempfile::tempdir().unwrap();
    let complete = format!("{source}{}Unrelated coarse heading.\n", "\n".repeat(8));
    let coarse_line = complete.lines().count();
    fs::write(directory.path().join("paper.tex"), complete).unwrap();
    let error = resolve_inverse_output(
        &directory.path().join("paper.pdf"),
        InversePoint {
            page: 1,
            x: 10.0,
            y_from_top: 10.0,
            page_height_pt: 600.0,
        },
        Some((first_context, 0)),
        4,
        &Operation::default(),
        &format!("Input: paper.tex\nLine: 9\nInput: paper.tex\nLine: {coarse_line}\n"),
        QuerySources::default(),
    )
    .err()
    .expect("unqualified layout rows must remain competing coarse anchors");
    assert!(error.to_string().contains("ambiguous inverse search"));
}

#[test]
fn inverse_closing_layout_does_not_guess_neighboring_mathematical_atoms() {
    let source = "\\begin{tabular}{l}\n\\(s+1\\) \\\\\n\n\n\n\n\
                  \\(s+1\\) \\\\\n\\end{tabular}\n";
    assert_eq!(source_word_location(source, 8, "𝑠+1", 0, 4), None);
    assert_eq!(source_word_location(source, 2, "𝑠+1", 0, 4), Some((2, 2)));
    let equation = "\\begin{equation}\ns+1\n\\end{equation}\n";
    assert_eq!(source_word_location(equation, 3, "𝑠+1", 0, 4), Some((2, 0)));
}

#[test]
fn inverse_competing_precise_or_coarse_positions_are_ambiguous_in_any_order() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("paper.tex"),
        "Silver signal remains stable.\nSilver signal remains stable.\n",
    )
    .unwrap();
    let first = "Input: paper.tex\nLine: 1\n";
    let second = "Input: paper.tex\nLine: 2\n";
    for stdout in [format!("{first}{second}"), format!("{second}{first}")] {
        for (word, expected) in [
            (Some(("Silver signal remains stable", 0)), "precise"),
            (None, "coarse"),
        ] {
            let error = inverse_output(&directory.path().join("paper.pdf"), &stdout, word)
                .err()
                .expect("competing source locations must be ambiguous")
                .to_string();
            assert!(error.contains("ambiguous inverse search"), "{error}");
            assert!(error.contains(expected), "{error}");
        }
    }
}

#[test]
fn inverse_duplicate_hits_deduplicate_final_canonical_coarse_positions() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("paper.tex");
    fs::write(&source, "First visible line.\nSecond visible line.\n").unwrap();
    let stdout = "Input: paper.tex\nLine: 2\nColumn: -1\n\
                  Input: ./paper.tex\nLine: 2\nColumn: -1\n\
                  Input: paper.tex\nLine: 2\nColumn: -1\n";
    let result = inverse_output(&directory.path().join("paper.pdf"), stdout, None).unwrap();
    assert_eq!(
        result.location.file,
        source.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!((result.location.line, result.location.byte_column), (2, 0));
    assert!(!result.location.precise);
    assert!(result.warning.is_none());
}

#[test]
fn inverse_readable_bounds_are_required_even_without_word_refinement() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.tex"),
        "Visible line.\n".repeat(272),
    )
    .unwrap();
    let error = inverse_output(
        &directory.path().join("main.pdf"),
        "Input: main.tex\nLine: 490\n",
        None,
    )
    .err()
    .expect("a readable out-of-range source anchor must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let failure = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<InverseFailure>())
        .expect("source bounds retain a typed failure");
    match failure {
        InverseFailure::InvalidSourceAnchor {
            file,
            line,
            available,
        } => {
            assert_eq!(
                file,
                &directory.path().join("main.tex").canonicalize().unwrap()
            );
            assert_eq!((*line, *available), (490, 272));
        }
        other => panic!("unexpected inverse failure: {other}"),
    }
}

#[test]
fn inverse_unreadable_source_remains_explicitly_warned_coarse_navigation() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("main.tex");
    fs::write(&source, [0xff]).unwrap();
    for unavailable in ["invalid UTF-8", "missing"] {
        if unavailable == "missing" {
            fs::remove_file(&source).unwrap();
        }
        for word in [None, Some(("Visible line", 0))] {
            let result = inverse_output(
                &directory.path().join("main.pdf"),
                "Input: main.tex\nLine: 490\nInput: ./main.tex\nLine: 490\n",
                word,
            )
            .unwrap();
            assert_eq!(result.location.line, 490);
            assert!(!result.location.precise);
            assert!(
                result
                    .warning
                    .unwrap()
                    .contains("source refinement unavailable"),
                "{unavailable}"
            );
        }
    }
}

#[test]
fn inverse_generated_hit_remaps_before_bounds_without_reading_reused_verbatim() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.tex");
    fs::write(
        &original,
        "\\title{Shared Section Words}\n\\begin{frame}[fragile]\nLiteral body text.\n\\end{frame}\n\\begin{frame}{Plan}\n\\tableofcontents\n\\end{frame}\n",
    )
    .unwrap();
    fs::write(directory.path().join("deck.vrb"), [0xff]).unwrap();
    fs::write(
        directory.path().join("deck.synctex"),
        "Input:1:original.tex\nInput:2:deck.vrb\nInput:3:build/deck.toc\nContent:\n\
         {1\n[1,4:0,0:1,1,0\nx2,490:0,0\n]\n}1\n\
         {2\n[1,7:0,0:1,1,0\nx3,12:0,0\n]\n}2\n",
    )
    .unwrap();
    let result = inverse_output(
        &directory.path().join("deck.pdf"),
        "Input: deck.vrb\nLine: 490\n",
        None,
    )
    .unwrap();
    assert_eq!(
        result.location.file,
        original.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(result.location.line, 4);
    assert!(!result.location.precise);
    assert!(result.warning.is_none());

    let toc_result = resolve_inverse_output(
        &directory.path().join("deck.pdf"),
        InversePoint {
            page: 2,
            x: 72.0,
            y_from_top: 120.0,
            page_height_pt: 792.0,
        },
        Some(("Shared Section Words", 0)),
        0,
        &Operation::default(),
        "Input: build/deck.toc\nLine: 12\n",
        QuerySources::default(),
    )
    .unwrap();
    assert_eq!(
        toc_result.location.file,
        original.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(toc_result.location.line, 7);
    assert!(!toc_result.location.precise);
    assert!(toc_result.warning.is_none());
}

#[test]
fn forward_source_bounds_apply_when_the_original_position_has_no_word() {
    let source = "école \\hidden{} % not printed\n\n";
    for (line, column) in [(1, 6), (2, 1)] {
        let text = validate_forward_source_position(source, line, column).unwrap();
        assert!(ForwardWord::at(text, column).is_none());
    }
    assert!(validate_forward_source_position(source, 2, 2).is_err());
    assert!(validate_forward_source_position(source, 3, 1).is_err());
    assert!(validate_forward_source_position(source, 1, u32::MAX).is_err());
}

#[test]
fn forward_duplicate_regions_do_not_hide_invalid_or_incomplete_records() {
    let synctex = "\
Input:1:/tmp/main.tex
Content:
<10
(1,5:0,100:200,10,0
x1,5:0,100
)
>
{1
[1,1:0,0:0,0,0
(1,5:100,200:200,10,0
x1,5:100,200
)
(1,5:100,200:200,10,0
x1,5:100,200
)
(1,5:100,300:-50,10,0
x1,5:100,300
)
f10:50,400
]
}1
";
    let pdf = Path::new("/tmp/main.pdf");
    let file = Path::new("/tmp/main.tex");
    let candidates =
        forward_candidates_from_reader(synctex.as_bytes(), pdf, file, None, 5).unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        forward_candidates_from_reader(
            b"Input:1:/tmp/other.tex\nContent:\n" as &[u8],
            pdf,
            file,
            None,
            1
        )
        .is_err()
    );
}

#[test]
fn forward_word_uses_scalar_columns_and_rejects_commands_and_comments() {
    let line = "école \\emph{naïve} word % hidden";
    let hint = ForwardWord::at(line, 13).unwrap();
    assert_eq!(hint.words[hint.selected], "naïve");
    for column in [0, 6, 8, 24, 26, u32::MAX] {
        assert!(ForwardWord::at(line, column).is_none(), "column {column}");
    }
    let mut hint = hint;
    hint.selected = hint.words.len();
    assert!(!hint.valid());
    let decomposed = "e\u{301}cole";
    for column in 1..=decomposed.chars().count() as u32 {
        let hint = ForwardWord::at(decomposed, column).unwrap();
        assert_eq!(hint.words[hint.selected], decomposed);
        assert!(hint.valid());
    }
    let overlay = "\\only<3>{visible} \\includesvg[width=\\linewidth]{diagram}";
    assert!(ForwardWord::at(overlay, 7).is_none());
    assert!(ForwardWord::at(overlay, 50).is_none());
    let visible_hint = ForwardWord::at(overlay, 10).unwrap();
    assert_eq!(visible_hint.words, ["visible"]);
}

#[test]
fn literal_forward_word_uses_utf8_boundaries_and_unicode_context() {
    let revision = PdfRevision::read(Path::new(file!())).unwrap();
    let request = |text: &str, byte_column: usize| {
        parse_forward_request(
            &serde_json::json!({
                "pdf": "/literal.pdf", "revision": revision, "page": 1,
                "h": 72, "v": 120, "width": 0, "height": 0,
                "word": { "text": text, "byte_column": byte_column }
            })
            .to_string(),
        )
    };
    let text = "one two three four café λ e\u{301}cole 漢字 nine ten";
    let selected = text.find("e\u{301}cole").unwrap();
    for byte in [selected, selected + 1] {
        let hint = request(text, byte).unwrap().word.unwrap();
        assert_eq!(
            hint.words,
            ["four", "café", "λ", "e\u{301}cole", "漢字", "nine", "ten"]
        );
        assert_eq!(hint.selected, 3);
    }
    assert_eq!(request("λ first", 0).unwrap().word.unwrap().words[0], "λ");
    assert_eq!(
        request("100% literal", 5).unwrap().word.unwrap().words[1],
        "literal"
    );
    assert!(request("λ first", 1).is_err());
    assert!(request(text, text.len() + 1).is_err());
    for byte in [3, text.len()] {
        assert!(request(text, byte).unwrap().word.is_none());
    }
    assert!(request(&"x".repeat(129), 0).unwrap().word.is_none());
}

#[test]
fn forward_geometry_preserves_top_down_coordinates_and_rejects_overflow() {
    let revision = PdfRevision::read(Path::new(file!())).unwrap();
    let payload = format!(
        r#"{{"pdf":"/a:b.pdf","revision":{},"page":2,"h":72,"v":120,"width":250,"height":12}}"#,
        serde_json::to_string(&revision).unwrap()
    );
    let request = parse_forward_request(&payload).unwrap();
    assert_eq!(
        request.rect(),
        SearchRect {
            left: 72.0,
            top: 108.0,
            right: 322.0,
            bottom: 120.0
        }
    );
    assert_eq!(request.pdf, Path::new("/a:b.pdf"));
    let mut request = request;
    request.h = f32::MAX;
    request.width = f32::MAX;
    assert!(request.validate().is_err());
    for invalid in [
        payload.replace("\"page\":2", "\"page\":0"),
        payload.replace("\"/a:b.pdf\"", "\"relative.pdf\""),
        payload.replace("\"height\":12", "\"height\":-12"),
        payload.replace("\"h\":72", "\"h\":1e100"),
        payload.replace("\"h\":72", "\"h\":72,\"h\":73"),
        payload.replace("\"h\":72", "\"h\":72,\"unexpected\":0"),
        payload.replace("\"h\":72", r#""h":72,"best_guess":"false""#),
        payload.replace("\"h\":72", r#""h":72,"best_guess":null"#),
        payload.replace("\"h\":72", r#""h":72,"source_snapshot":"relative.json""#),
        payload.replace(
            "\"h\":72",
            r#""h":72,"source_snapshot":"/cache/\u0000.json""#,
        ),
        payload.replace("\"h\":72", r#""h":72,"warning":"" "#),
        payload.replace("\"h\":72", r#""h":72,"warning":"\u001b[31munsafe""#),
        payload.replace(
            "\"h\":72",
            &format!(
                "\"h\":72,\"warning\":\"{}\"",
                "x".repeat(crate::source_snapshot::MAX_WARNING_BYTES + 1)
            ),
        ),
        payload.replace("\"h\":72", r#""h":72,"word":{"words":[],"selected":0}"#),
        payload.replace(
            "\"h\":72",
            r#""h":72,"word":{"words":["text"],"selected":1}"#,
        ),
        payload.replace(
            "\"h\":72",
            r#""h":72,"word":{"words":["two words"],"selected":0}"#,
        ),
    ] {
        assert!(parse_forward_request(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn nonprinting_spans_and_frame_bounds_handle_control_symbols_let_and_verbatim() {
    let source = "\\let\\olddef\\def\n\\newcommand\\+{HiddenSymbol}\nVisibleProse\n";
    let visible_at = source.find("VisibleProse").unwrap();
    let hidden = nonprinting_source_ranges(source);
    assert!(hidden.iter().all(|range| !range.contains(&visible_at)));
    let hidden_at = source.find("HiddenSymbol").unwrap();
    assert!(hidden.iter().any(|range| range.contains(&hidden_at)));

    let frame_source = "\
\\begin{frame}
Slide text
\\begin{verbatim}
\\begin{frame}
\\end{frame}
\\end{verbatim}
More slide text
\\end{frame}";
    assert_eq!(frame_bounds(frame_source, 7), Some((0, 7)));
    assert_eq!(normalized_word("oﬃce"), "office");
    assert_eq!(normalized_word(" Café "), " cafe ");

    let article = "\\title{Sample Document Title for Testing}\n\
                   \\maketitle\n\
                   Keywords. Sample-document testing, synthetic examples.\n";
    assert!(!metadata_beats_frame(
        article,
        3,
        "Sample-document testing,",
        9,
        386.5,
        792.0,
        6,
    ));
    assert!(!allows_metadata_fallback(article, 3, 0, 386.5, 792.0));
    assert!(allows_metadata_fallback(article, 2, 1, 115.0, 792.0));
    assert_eq!(
        source_word_location(article, 3, "Keywords. Sample-document testing", 13, 1),
        Some((3, 10)),
    );
    let accented =
        "\\author{Jane Example\\thanks{Center of \\'Etudes\nMod\\^ele d'Éxemple}}";
    assert_eq!(
        source_word_location(
            accented,
            1,
            "Center of Études Modêle d’Éxemple",
            10,
            1,
        ),
        Some((1, 38)),
    );
    assert_eq!(
        source_word_location(
            accented,
            2,
            "Center of Études Modêle d’Éxemple",
            18,
            1,
        ),
        Some((2, 0)),
    );

    let synctex = "\
Input:1:/tmp/main.tex
Input:2:/tmp/./section.tex
{1
[2,44:0,0:0,0,0
(2,44:0,49:100,10,0
x2,44:0,49
)
(2,29:0,700:100,10,0
x2,29:0,700
)
r2,44:0,700:8,7,0
]
}1
{2
[1,10:0,0:0,0,0
(2,44:0,178:100,10,0
x2,43:0,178
g2,43:10,178
k2,44:20,178:0
)
(2,48:0,200:100,10,0
x2,48:0,200
)
]
}2
";
    let pdf = Path::new("/tmp/main.pdf");
    let section = Path::new("/tmp/section.tex");
    assert_eq!(
        forward_candidates_from_reader(synctex.as_bytes(), pdf, section, None, 29).unwrap()[0]
            .page,
        1
    );
    assert_eq!(
        forward_candidates_from_reader(synctex.as_bytes(), pdf, section, None, 44).unwrap()[0]
            .page,
        2
    );
    assert_eq!(
        forward_candidates_from_reader(synctex.as_bytes(), pdf, section, None, 45).unwrap()[0]
            .page,
        2
    );
}

#[test]
fn fuzz_forward_candidates_matches_synctex_view_and_survives_malformed_inputs() {
    fn next_u64(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }
    fn parse_cli_synctex_view(output: &str) -> Vec<ForwardCandidate> {
        let mut out = Vec::new();
        let mut page = None::<u32>;
        let mut h = None::<f32>;
        let mut v = None::<f32>;
        let mut w = None::<f32>;
        let mut height_opt = None::<f32>;
        let flush = |out: &mut Vec<ForwardCandidate>,
                     page: &mut Option<u32>,
                     h: &mut Option<f32>,
                     v: &mut Option<f32>,
                     w: &mut Option<f32>,
                     height_opt: &mut Option<f32>| {
            if let (Some(p), Some(x), Some(y), Some(width), Some(height)) =
                (page.take(), h.take(), v.take(), w.take(), height_opt.take())
                && forward_geometry_valid(p, x, y, width, height)
            {
                let cand = ForwardCandidate {
                    page: p,
                    rect: SearchRect {
                        left: x,
                        top: y - height,
                        right: x + width,
                        bottom: y,
                    },
                };
                if !out
                    .iter()
                    .any(|old| old.page == cand.page && old.rect == cand.rect)
                {
                    out.push(cand);
                }
            }
            *page = None;
            *h = None;
            *v = None;
            *w = None;
            *height_opt = None;
        };
        for line in output.lines() {
            if let Some(val) = line.strip_prefix("Page:") {
                flush(&mut out, &mut page, &mut h, &mut v, &mut w, &mut height_opt);
                page = val.trim().parse().ok();
            } else if let Some(val) = line.strip_prefix("h:") {
                h = val.trim().parse().ok();
            } else if let Some(val) = line.strip_prefix("v:") {
                v = val.trim().parse().ok();
            } else if let Some(val) = line.strip_prefix("W:") {
                w = val.trim().parse().ok();
            } else if let Some(val) = line.strip_prefix("H:") {
                height_opt = val.trim().parse().ok();
            }
        }
        flush(&mut out, &mut page, &mut h, &mut v, &mut w, &mut height_opt);
        out
    }
    fn gen_box_body(
        rng: &mut u64,
        out: &mut String,
        depth: usize,
        base_line: i64,
        max_form_tag: u32,
        in_hbox: bool,
    ) {
        let count = (next_u64(rng) % 6) + 1;
        for idx in 0..count {
            let tag = if next_u64(rng) % 4 == 0 { 2 } else { 1 };
            let line = base_line + (next_u64(rng) % 8) as i64;
            let x = ((next_u64(rng) % 400_000) as i64) + 10_000;
            let y = ((next_u64(rng) % 600_000) as i64) + 20_000;
            let w = ((next_u64(rng) % 150_000) as i64) + 1_000;
            let h = ((next_u64(rng) % 20_000) as i64) + 500;
            let d = (next_u64(rng) % 5_000) as i64;
            let y_str = if next_u64(rng) % 5 == 0 {
                "=".to_string()
            } else {
                y.to_string()
            };
            match next_u64(rng) % 10 {
                0 if depth < 3 && (in_hbox || idx + 1 < count) => {
                    out.push_str(&format!("[{tag},{line}:{x},{y}:{w},{h},{d}\n"));
                    gen_box_body(rng, out, depth + 1, base_line, max_form_tag, false);
                    out.push_str("]\n");
                }
                1 | 2 if depth < 3 && (in_hbox || idx + 1 < count) => {
                    out.push_str(&format!("({tag},{line}:{x},{y}:{w},{h},{d}\n"));
                    gen_box_body(rng, out, depth + 1, base_line, max_form_tag, true);
                    out.push_str(")\n");
                }
                3 => out.push_str(&format!("v{tag},{line}:{x},{y_str}:{w},{h},{d}\n")),
                4 => out.push_str(&format!("h{tag},{line}:{x},{y_str}:{w},{h},{d}\n")),
                5 => {
                    let kw = if next_u64(rng) % 3 == 0 { -w } else { w };
                    out.push_str(&format!("k{tag},{line}:{x},{y_str}:{kw}\n"));
                    if next_u64(rng) % 2 == 0 {
                        out.push_str(&format!("g{tag},{line}:{x},{y_str}\n"));
                    }
                }
                6 => out.push_str(&format!("g{tag},{line}:{x},{y_str}\n")),
                7 => out.push_str(&format!("r{tag},{line}:{x},{y_str}:{w},{h},{d}\n")),
                8 => out.push_str(&format!("${tag},{line}:{x},{y_str}\n")),
                9 if max_form_tag > 0 && (in_hbox || idx > 0) && next_u64(rng) % 2 == 0 => {
                    let ftag = ((next_u64(rng) as u32) % max_form_tag) + 1;
                    out.push_str(&format!("f{ftag}:{x},{y_str}\n"));
                }
                _ => out.push_str(&format!("x{tag},{line}:{x},{y_str}\n")),
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("doc.pdf");
    let main_tex = dir.path().join("main.tex");
    let sub_tex = dir.path().join("sub.tex");
    let synctex_path = dir.path().join("doc.synctex");
    fs::write(&pdf, b"%PDF-1.4\n").unwrap();
    fs::write(&main_tex, "line\n".repeat(120)).unwrap();
    fs::write(&sub_tex, "line\n".repeat(120)).unwrap();

    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    for case_idx in 0..80 {
        let mag = [1000, 1200, 800][(next_u64(&mut rng) % 3) as usize];
        let unit = [8192, 4096][(next_u64(&mut rng) % 2) as usize];
        let x_off = (next_u64(&mut rng) % 600) as i64;
        let y_off = (next_u64(&mut rng) % 600) as i64;
        let mut synctex = format!(
            "SyncTeX Version:1\nInput:1:{}\nInput:2:{}\nOutput:pdf\nMagnification:{mag}\nUnit:{unit}\nX Offset:{x_off}\nY Offset:{y_off}\nContent:\n",
            main_tex.display(),
            sub_tex.display(),
        );
        let num_forms = (next_u64(&mut rng) % 3) as u32;
        for ftag in 1..=num_forms {
            synctex.push_str(&format!("<{ftag}\n(1,5:10000,20000:50000,8000,2000\n"));
            gen_box_body(&mut rng, &mut synctex, 1, 5, ftag - 1, true);
            synctex.push_str(")\n>\n");
        }
        let pages = ((next_u64(&mut rng) % 3) + 1) as u32;
        for p in 1..=pages {
            let base_line = i64::from(p) * 20;
            synctex.push_str(&format!(
                "{{{p}\n[1,1:0,0:400000,600000,0\n(1,{base_line}:10000,30000:300000,12000,3000\n"
            ));
            gen_box_body(&mut rng, &mut synctex, 1, base_line, num_forms, true);
            synctex.push_str(&format!(")\n]\n}}{p}\n"));
        }
        synctex.push_str("Postamble:\nCount:100\n");
        if next_u64(&mut rng) % 2 == 0 {
            let x_unit = ["pt", "bp", "in", "cm", "mm", "pc", "sp", "dd", "cc", "nd", "nc"]
                [(next_u64(&mut rng) % 11) as usize];
            let y_unit = ["pt", "bp", "in", "cm", "mm", "pc", "sp", "dd", "cc", "nd", "nc"]
                [(next_u64(&mut rng) % 11) as usize];
            synctex.push_str(&format!(
                "Post scriptum:\nMagnification:1.25\nX Offset:2{x_unit}\nY Offset:3{y_unit}\n"
            ));
        }
        fs::write(&synctex_path, &synctex).unwrap();

        for (target_tex, query_line) in [
            (&main_tex, 1u32),
            (&main_tex, 5),
            (&main_tex, 20),
            (&main_tex, 23),
            (&main_tex, 27),
            (&main_tex, 41),
            (&main_tex, 45),
            (&main_tex, 62),
            (&main_tex, 95),
            (&sub_tex, 21),
            (&sub_tex, 42),
        ] {
            let cli_out = Command::new("synctex")
                .args([
                    "view",
                    "-i",
                    &format!("{query_line}:0:{}", target_tex.display()),
                    "-o",
                    &pdf.to_string_lossy(),
                ])
                .output()
                .unwrap();
            let expected = parse_cli_synctex_view(&String::from_utf8_lossy(&cli_out.stdout));
            let actual = forward_candidates_from_reader_with_options(
                synctex.as_bytes(),
                &pdf,
                target_tex,
                None,
                query_line,
                false,
            )
            .unwrap_or_default();
            let _ = forward_candidates_from_reader(
                synctex.as_bytes(),
                &pdf,
                target_tex,
                None,
                query_line,
            );
            assert_eq!(
                actual.len(),
                expected.len(),
                "case {case_idx} line {query_line} count mismatch\nactual={actual:?}\nexpected={expected:?}\nstdout={}\nstderr={}\nsynctex:\n{synctex}",
                String::from_utf8_lossy(&cli_out.stdout),
                String::from_utf8_lossy(&cli_out.stderr),
            );
            for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
                assert_eq!(
                    a.page, e.page,
                    "case {case_idx} line {query_line} idx {i} page\nactual={actual:?}\nexpected={expected:?}\nsynctex:\n{synctex}"
                );
                assert!(
                    (a.rect.left - e.rect.left).abs() < 2e-2
                        && (a.rect.top - e.rect.top).abs() < 2e-2
                        && (a.rect.right - e.rect.right).abs() < 2e-2
                        && (a.rect.bottom - e.rect.bottom).abs() < 2e-2,
                    "case {case_idx} line {query_line} idx {i} rect mismatch: actual={:?} expected={:?}\nsynctex:\n{synctex}",
                    a.rect,
                    e.rect
                );
            }
        }

        for _ in 0..4 {
            let qpage = ((next_u64(&mut rng) as u32) % pages) + 1;
            let qx = (next_u64(&mut rng) % 400) as f32;
            let qy = (next_u64(&mut rng) % 600) as f32;
            let cli_out = Command::new("synctex")
                .args([
                    "edit",
                    "-o",
                    &format!("{qpage}:{qx}:{qy}:{}", pdf.display()),
                ])
                .output()
                .unwrap();
            let expected = parse_synctex_edit(&String::from_utf8_lossy(&cli_out.stdout));
            let actual = inverse_candidates_from_reader(
                synctex.as_bytes(),
                InversePoint {
                    page: qpage,
                    x: qx,
                    y_from_top: qy,
                    page_height_pt: 792.0,
                },
            )
            .unwrap_or_default();
            assert_eq!(
                actual,
                expected,
                "case {case_idx} inverse ({qpage},{qx},{qy}) mismatch\nactual={actual:?}\nexpected={expected:?}\nstdout={}\nstderr={}\nsynctex:\n{synctex}",
                String::from_utf8_lossy(&cli_out.stdout),
                String::from_utf8_lossy(&cli_out.stderr),
            );
        }

        // Mutate the valid document 8 ways per case (640 total mutated inputs) and verify no panics
        for _ in 0..8 {
            let mut mutated = synctex.clone().into_bytes();
            let ops = (next_u64(&mut rng) % 6) + 1;
            for _ in 0..ops {
                if mutated.is_empty() {
                    break;
                }
                let idx = (next_u64(&mut rng) as usize) % mutated.len();
                match next_u64(&mut rng) % 4 {
                    0 => mutated.truncate(idx),
                    1 => {
                        let b = b"()[]{}<>fxgkhvr$0123456789,-:=\n"
                            [(next_u64(&mut rng) % 31) as usize];
                        mutated[idx] = b;
                    }
                    2 => {
                        mutated.remove(idx);
                    }
                    _ => {
                        mutated.insert(idx, (next_u64(&mut rng) & 0xFF) as u8);
                    }
                }
            }
            let qline = ((next_u64(&mut rng) % 80) + 1) as u32;
            let _ = forward_candidates_from_reader(&mutated[..], &pdf, &main_tex, None, qline);
            let _ = inverse_candidates_from_reader(
                &mutated[..],
                InversePoint {
                    page: 1,
                    x: 50.0,
                    y_from_top: 100.0,
                    page_height_pt: 792.0,
                },
            );
        }
    }

    // Adversarial / malformed inputs: cyclic forms, huge i64 values, unbalanced delimiters
    let adversarial = [
        format!(
            "Input:1:{}\nContent:\n<1\n(1,1:0,0:10,10,0\nf2:0,0\n)\n>\n<2\n(1,1:0,0:10,10,0\nf1:0,0\n)\n>\n{{1\n[1,1:0,0:10,10,0\nf1:0,0\n]\n}}1\n",
            main_tex.display()
        ),
        format!(
            "Input:1:{}\nMagnification:-9223372036854775808\nUnit:0\nContent:\n{{1\n(1,9223372036854775807:9223372036854775807,-9223372036854775808:9223372036854775807,9223372036854775807,9223372036854775807\nx1,1:-9223372036854775808,9223372036854775807\nk1,1:9223372036854775807,=:9223372036854775807\ng1,1:0,=\n)\n]\n)\n}}1\n",
            main_tex.display()
        ),
        format!(
            "Input:1:{}\nContent:\n{{1\n(\n[\n)\n]\nf999:1,2\nx1,2:3,=\n",
            main_tex.display()
        ),
    ];
    for bad in &adversarial {
        let _ = forward_candidates_from_reader(bad.as_bytes(), &pdf, &main_tex, None, 1);
        let _ = inverse_candidates_from_reader(
            bad.as_bytes(),
            InversePoint {
                page: 1,
                x: 10.0,
                y_from_top: 10.0,
                page_height_pt: 792.0,
            },
        );
    }
}

#[test]
fn tabularx_closing_line_and_inverse_word_refinement() {
    let source = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\begin{table}[t]\n",
        "\\begin{tabularx}{\\textwidth}{>{\\raggedright\\arraybackslash}p{2.3cm}p{2.2cm}X}\n",
        "Lorem & Ipsum & Dolor \\\\\n",
        "consectetur & $\\alpha_k + \\beta_m = \\gamma_n$ & adipiscing elit \\\\\n",
        "sed do & eiusmod tempor & $\\sum_{j=1}^{q} \\xi_j \\eta_j$ \\\\\n",
        "\\end{tabularx}\n",
        "\\end{table}\n",
        "\\end{document}\n",
    );
    assert_eq!(tabularx_closing_line(source, 3), None);
    for line in 4..=8 {
        assert_eq!(tabularx_closing_line(source, line), Some(8));
    }
    assert_eq!(tabularx_closing_line(source, 9), None);

    // Prose word on interior line 6 resolves when SyncTeX reports closing line 8.
    let context = "consectetur αk + βm = γn adipiscing elit";
    let offset = context.find("adipiscing").unwrap();
    assert_eq!(
        source_word_location(source, 8, context, offset, 2),
        Some((6, 48))
    );

    // Gibberish formula on interior line 6 resolves when SyncTeX reports closing line 8.
    let math_offset = context.find("βm").unwrap();
    assert_eq!(
        source_word_location(source, 8, context, math_offset, 2),
        Some((6, 26))
    );

    // Column-spec tokens on the \begin{tabularx} line are masked as nonprinting.
    for spec_token in ["tabularx", "textwidth", "3cm"] {
        assert_eq!(
            source_word_location(source, 8, spec_token, 0, 2),
            None,
            "column spec token {spec_token:?} must not match as printed prose"
        );
    }
}

#[test]
fn real_pdflatex_tabularx_forward_and_inverse() {
    if Command::new("pdflatex").arg("--version").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tex = dir.path().join("doc.tex");
    let pdf = dir.path().join("doc.pdf");
    let source = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{tabularx}\n",
        "\\begin{document}\n",
        "\\noindent\n",
        "\\begin{tabularx}{\\textwidth}{l c X}\n",
        "Lorem ipsum & dolor sit & amet consectetur \\\\\n",
        "adipiscing elit & $\\alpha_k + \\beta_m = \\gamma_n$ & sed eiusmod tempor \\\\\n",
        "\\end{tabularx}\n",
        "\\end{document}\n",
    );
    fs::write(&tex, source).unwrap();
    let status = Command::new("pdflatex")
        .args([
            "-interaction=nonstopmode",
            "-halt-on-error",
            "-synctex=1",
            "doc.tex",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    if !status.status.success() {
        return;
    }
    let forward = resolve_forward(&pdf, &tex, 7, 1).unwrap();
    assert_eq!(forward.page, 1);
    assert!(forward.width > 0.0 || forward.height > 0.0);

    let point = InversePoint {
        page: 1,
        x: forward.h + forward.width * 0.5,
        y_from_top: forward.v - forward.height * 0.5,
        page_height_pt: 792.0,
    };
    let context = "adipiscing elit αk + βm = γn sed eiusmod tempor";
    let offset = context.find("eiusmod").unwrap();
    let inverse = resolve_inverse(
        &pdf,
        point,
        Some((context, offset)),
        2,
        &Operation::default(),
        None,
        true,
    )
    .unwrap();
    assert!(inverse.location.precise);
    assert_eq!(inverse.location.line, 7);
}

#[test]
fn parser_enforces_box_depth_and_selected_input_tag() {
    let dir = tempfile::tempdir().unwrap();
    let _pdf = dir.path().join("doc.pdf");
    let main_tex = dir.path().join("main.tex");
    let sub_tex = dir.path().join("sub.tex");

    // Box nesting beyond MAX_BOX_DEPTH (512) must fail cleanly without stack overflow.
    let mut deep = format!("Input:1:{}\nContent:\n{{1\n", main_tex.display());
    for _ in 0..=MAX_BOX_DEPTH + 2 {
        deep.push_str("(1,1:0,0:10,10,0\n");
    }
    let Err(err) = parse_synctex_document(deep.as_bytes()) else {
        panic!("expected depth limit error");
    };
    assert!(err.to_string().contains("nesting depth exceeds limit"));

    // Multi-input sheet where root vbox uses Input:1 and matched child uses Input:2.
    let synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nInput:2:{}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,10:0,0:30000000,40000000,0\n(2,42:655360,6553600:6553600,655360,0\nx2,42:1310720,6553600\n)\n]\n}}1\n",
        main_tex.display(),
        sub_tex.display(),
    );
    let candidates = inverse_candidates_from_reader(
        synctex.as_bytes(),
        InversePoint {
            page: 1,
            x: 20.0,
            y_from_top: 98.0,
            page_height_pt: 792.0,
        },
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].file, sub_tex.to_string_lossy());
    assert_eq!(candidates[0].line, 42);

    // Fragile-frame trailing paragraph hbox opened with (1,19 (deck.tex) and flushed by g2,6 (deck.vrb)
    // must propagate tag 2 (deck.vrb) to both the initial x node and the hbox left boundary.
    let vrb_tex = dir.path().join("doc.vrb");
    let fragile_synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nInput:2:{}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{2\n[1,19:0,0:30000000,40000000,0\n(1,19:655360,6553600:6553600,655360,0\nx1,19:2621440,6553600\ng2,6:2800000,6553600\nx2,6:4000000,6553600\n)\n]\n}}2\nPostamble:\nCount:8\n",
        main_tex.display(),
        vrb_tex.display(),
    );
    let fragile_candidates = inverse_candidates_from_reader_with_options(
        fragile_synctex.as_bytes(),
        InversePoint {
            page: 2,
            x: 20.0,
            y_from_top: 98.0,
            page_height_pt: 792.0,
        },
        true,
    )
    .unwrap();
    assert_eq!(fragile_candidates.len(), 1);
    assert_eq!(fragile_candidates[0].file, vrb_tex.to_string_lossy());
    assert_eq!(fragile_candidates[0].line, 6);

    // Included tabularx parbox cell (tag 2, line 12) ending with a cross-file \@arstrutbox rule
    // (r1,10 from main.tex) must keep tag 2, line 12 on its trailing word x2,12.
    let strut_synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nInput:2:{}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,15:0,0:30000000,40000000,0\n(2,12:655360,6553600:6553600,655360,0\nx2,12:1310720,6553600\nr1,10:2621440,6553600:0,0,262144\nk2,12:2621440,6553600:0\ng2,12:2621440,6553600\n)\n]\n}}1\nPostamble:\nCount:9\n",
        main_tex.display(),
        sub_tex.display(),
    );
    let strut_candidates = inverse_candidates_from_reader_with_options(
        strut_synctex.as_bytes(),
        InversePoint {
            page: 1,
            x: 20.0,
            y_from_top: 98.0,
            page_height_pt: 792.0,
        },
        true,
    )
    .unwrap();
    assert_eq!(strut_candidates.len(), 1);
    assert_eq!(strut_candidates[0].file, sub_tex.to_string_lossy());
    assert_eq!(strut_candidates[0].line, 12);
}

#[test]
fn synctex_cache_reuses_parsed_pages_and_invalidates_on_companion_change() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &tex,
        "\\documentclass{article}\n\\begin{document}\nfirst word\nsecond word\nthird word\n\\end{document}\n",
    )
    .unwrap();
    let write_synctex = |line_p1: u32, line_p2: u32| {
        let content = format!(
            "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,{line_p1}:0,0:30000000,40000000,0\n(1,{line_p1}:655360,6553600:6553600,655360,0\nx1,{line_p1}:1310720,6553600\n)\n]\n}}1\n{{2\n[1,{line_p2}:0,0:30000000,40000000,0\n(1,{line_p2}:655360,6553600:6553600,655360,0\nx1,{line_p2}:1310720,6553600\n)\n]\n}}2\nPostamble:\nCount:8\n",
            tex.display()
        );
        fs::write(pdf.with_extension("synctex"), content).unwrap();
    };
    write_synctex(3, 4);
    let mut cache = SyncTeXCache::default();
    let op = Operation::default();
    let pt1 = InversePoint {
        page: 1,
        x: 20.0,
        y_from_top: 98.0,
        page_height_pt: 792.0,
    };
    let pt2 = InversePoint { page: 2, ..pt1 };
    let r1 = resolve_inverse_with_cache(
        &pdf,
        pt1,
        Some(("first word", 0)),
        2,
        &op,
        None,
        false,
        Some(&mut cache),
    )
    .unwrap();
    assert_eq!(r1.location.line, 3);
    assert!(r1.location.precise);

    // Warm hit on page 1 and lazy parse of page 2 from cached raw bytes
    let r1_again = resolve_inverse_with_cache(
        &pdf,
        pt1,
        Some(("first word", 6)),
        2,
        &op,
        None,
        false,
        Some(&mut cache),
    )
    .unwrap();
    assert_eq!(r1_again.location.line, 3);
    assert_eq!(r1_again.location.column_char, 7);

    let r2 = resolve_inverse_with_cache(
        &pdf,
        pt2,
        Some(("second word", 0)),
        2,
        &op,
        None,
        false,
        Some(&mut cache),
    )
    .unwrap();
    assert_eq!(r2.location.line, 4);
    assert!(r2.location.precise);

    // Updating the companion file invalidates the cache automatically
    write_synctex(5, 4);
    let r1_updated = resolve_inverse_with_cache(
        &pdf,
        pt1,
        Some(("third word", 0)),
        2,
        &op,
        None,
        false,
        Some(&mut cache),
    )
    .unwrap();
    assert_eq!(r1_updated.location.line, 5);
    assert!(r1_updated.location.precise);
}

#[test]
fn gzip_magic_columns_and_forward_page_hint_work_across_apis() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &tex,
        "\\documentclass{article}\n\\begin{document}\nalpha beta gamma\n\\end{document}\n",
    )
    .unwrap();

    let synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3,7:0,0:30000000,40000000,0\n(1,3,7:655360,6553600:6553600,655360,0\nx1,3,7:1310720,6553600\n)\n]\n}}1\n{{2\n[1,3,12:0,0:30000000,40000000,0\n(1,3,12:655360,13107200:6553600,655360,0\nx1,3,12:1310720,13107200\n)\n]\n}}2\nPostamble:\nCount:8\n",
        tex.display()
    );

    // 1. Gzip-compressed content stored in a `.synctex` file (without `.gz` suffix)
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(synctex.as_bytes()).unwrap();
    let gz_bytes = encoder.finish().unwrap();
    fs::write(pdf.with_extension("synctex"), &gz_bytes).unwrap();

    let info = crate::source_snapshot::SourceInfo::read(&pdf).unwrap();
    assert_eq!(info.main.as_deref(), Some(tex.as_path()));

    // 2. Compiler-recorded columns preserved in raw candidates, `parse_synctex_edit`, and `resolve_inverse`
    let pt1 = InversePoint {
        page: 1,
        x: 20.0,
        y_from_top: 98.0,
        page_height_pt: 792.0,
    };
    let raw = inverse_candidates(&pdf, pt1).unwrap();
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].line, 3);
    assert_eq!(raw[0].byte_column, 6);
    assert_eq!(raw[0].column, 7);
    assert_eq!(raw[0].column_char, 7);
    assert!(!raw[0].precise);

    let coarse = resolve_inverse(&pdf, pt1, None, 2, &Operation::default(), None, false).unwrap();
    assert_eq!(coarse.location.line, 3);
    assert_eq!(coarse.location.byte_column, 6);
    assert_eq!(coarse.location.column, 7);
    assert_eq!(coarse.location.column_char, 7);
    assert!(!coarse.location.precise);

    let parsed_edit = parse_synctex_edit(&format!(
        "SyncTeX result begin\nInput:{}\nLine:3\nColumn:7\nSyncTeX result end\n",
        tex.display()
    ));
    assert_eq!(parsed_edit.len(), 1);
    assert_eq!(parsed_edit[0].column, 7);
    assert_eq!(parsed_edit[0].byte_column, 6);

    // 3. Forward `page_hint` selects page 2 when hinted, and page 1 by default
    let default_forward = resolve_forward(&pdf, &tex, 3, 1).unwrap();
    assert_eq!(default_forward.page, 1);

    let hinted_forward = resolve_forward_with_selector(
        &pdf,
        &tex,
        3,
        1,
        ForwardSource {
            page_hint: Some(2),
            ..ForwardSource::default()
        },
        |_, _, _, anchor| Ok(anchor),
    )
    .unwrap();
    assert_eq!(hinted_forward.page, 2);

    // Plain-text content stored in `.synctex.gz` also decodes cleanly
    fs::remove_file(pdf.with_extension("synctex")).unwrap();
    fs::write(pdf.with_extension("synctex.gz"), synctex.as_bytes()).unwrap();
    let plain_gz_forward = resolve_forward_with_selector(
        &pdf,
        &tex,
        3,
        1,
        ForwardSource {
            page_hint: Some(2),
            ..ForwardSource::default()
        },
        |_, _, _, anchor| Ok(anchor),
    )
    .unwrap();
    assert_eq!(plain_gz_forward.page, 2);
}

#[test]
fn separate_build_directory_sidecar_lookup_works_for_forward_inverse_and_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let out_dir = root.join("out");
    let build_dir = root.join("build");
    fs::create_dir_all(&out_dir).unwrap();
    fs::create_dir_all(&build_dir).unwrap();

    let pdf = out_dir.join("separate_build_dir_doc.pdf");
    let tex = build_dir.join("main.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &tex,
        "\\documentclass{article}\n\\begin{document}\nseparate build word\n\\end{document}\n",
    )
    .unwrap();

    // Relative Input path inside the separate build directory
    let synctex = "SyncTeX Version:1\nInput:1:./main.tex\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{1\n[1,3,5:0,0:30000000,40000000,0\n(1,3,5:655360,6553600:6553600,655360,0\nx1,3,5:1310720,6553600\n)\n]\n}1\nPostamble:\nCount:4\n";
    let sidecar = build_dir.join("separate_build_dir_doc.synctex");
    fs::write(&sidecar, synctex).unwrap();

    assert_eq!(locate_synctex(&pdf), None);
    assert_eq!(
        locate_synctex_in(&pdf, Some(&build_dir)).as_deref(),
        Some(sidecar.as_path())
    );
    // Relative build_directory resolved against pdf.parent()
    assert_eq!(
        locate_synctex_in(&pdf, Some(Path::new("../build")))
            .and_then(|p| fs::canonicalize(p).ok())
            .as_deref(),
        Some(sidecar.as_path())
    );

    let info = crate::source_snapshot::SourceInfo::read_in(&pdf, Some(&build_dir)).unwrap();
    assert_eq!(info.main.as_deref(), Some(tex.as_path()));
    assert_eq!(info.inputs, vec![tex.clone()]);

    let forward = resolve_forward_with_selector(
        &pdf,
        &tex,
        3,
        1,
        ForwardSource {
            build_directory: Some(&build_dir),
            ..ForwardSource::default()
        },
        |_, _, _, anchor| Ok(anchor),
    )
    .unwrap();
    assert_eq!(forward.page, 1);

    let pt = InversePoint {
        page: 1,
        x: 20.0,
        y_from_top: 98.0,
        page_height_pt: 792.0,
    };
    let mut cache = SyncTeXCache::default();
    let inverse = resolve_inverse_with_cache_in(
        &pdf,
        pt,
        Some(("separate build word", 9)),
        2,
        &Operation::default(),
        None,
        false,
        Some(&mut cache),
        Some(&build_dir),
    )
    .unwrap();
    assert_eq!(Path::new(&inverse.location.file), tex.as_path());
    assert_eq!(inverse.location.line, 3);
    assert_eq!(inverse.location.column_char, 10);
    assert!(inverse.location.precise);

    // Configured default build directory via set_build_directory
    set_build_directory(Some(build_dir.clone()));
    let forward_global = resolve_forward(&pdf, &tex, 3, 1).unwrap();
    assert_eq!(forward_global.page, 1);
    let inverse_global = resolve_inverse(
        &pdf,
        pt,
        None,
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    set_build_directory(None);
    assert_eq!(Path::new(&inverse_global.location.file), tex.as_path());
    assert_eq!(inverse_global.location.line, 3);
    assert_eq!(inverse_global.location.column, 5);
}
