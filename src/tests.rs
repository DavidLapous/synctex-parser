use super::*;

#[test]
fn inverse_validation_precedes_io_and_respects_cancellation() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let missing = directory.path().join("missing.pdf");
    let snapshot = directory.path().join("missing.json");
    let valid = InversePoint {
        page: 1,
        x: -1.0,
        y_from_top: -0.0,
        page_height_pt: f32::MIN_POSITIVE,
    };
    let resolve = |point, operation: &Operation| {
        [
            resolve_inverse_in(
                &missing,
                point,
                None,
                1,
                operation,
                Some(&snapshot),
                false,
                None,
            ),
            resolve_inverse_with_cache_in(
                &missing, point, None, 1, operation, None, false, None, None,
            ),
        ]
    };
    for point in [
        InversePoint { page: 0, ..valid },
        InversePoint {
            x: f32::NAN,
            ..valid
        },
        InversePoint {
            y_from_top: f32::INFINITY,
            ..valid
        },
        InversePoint {
            page_height_pt: f32::NEG_INFINITY,
            ..valid
        },
        InversePoint {
            page_height_pt: -0.0,
            ..valid
        },
    ] {
        for result in resolve(point, &Operation::default()) {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }
    for result in resolve(valid, &Operation::default()) {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }
    let cancelled = Operation::default();
    cancelled.cancel();
    for result in resolve(InversePoint { page: 0, ..valid }, &cancelled) {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }
    for result in resolve(valid, &Operation::new(Duration::ZERO)) {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}

#[test]
fn scaled_graphics_native_selection_keeps_overlay_prose() {
    let graphics = [(3, 50), (4, 100)]
        .map(|(line, x)| {
            format!(
                concat!(
                    "(1,{line}:{x},100:30,30,0\n",
                    "(1,{line}:{x},100:30,30,0\n",
                    "(1,{line}:{x},100:0,30,0\n",
                    "(1,{line}:{x},100:0,60,0\n",
                    "(1,{line}:{x},100:200,60,0\n",
                    "k1,{line}:{right},100:0\n",
                    ")\n",
                    "k1,{line}:{x},100:-200\n",
                    ")\n)\n)\n)\n",
                ),
                line = line,
                x = x,
                right = x + 200,
            )
        })
        .join("");
    for overlay in [false, true] {
        let prose = if overlay {
            "(1,5:100,90:30,10,0\ng1,5:100,90\nx1,5:130,90\n)\n"
        } else {
            ""
        };
        let synctex = format!(
            concat!(
                "SyncTeX Version:1\nInput:1:doc.tex\nOutput:pdf\n",
                "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
                "{{1\n[1,6:0,0:400,792,0\n(1,6:10,100:300,60,0\n",
                "{graphics}{prose})\n]\n}}1\nPostamble:\nCount:40\n",
            ),
            graphics = graphics,
            prose = prose,
        );
        let candidates = inverse_candidates_from_reader(
            synctex.as_bytes(),
            InversePoint {
                page: 1,
                x: 115.0,
                y_from_top: 85.0,
                page_height_pt: 792.0,
            },
        )
        .unwrap();
        assert_eq!(candidates.len(), 1, "overlay={overlay}: {candidates:?}");
        assert_eq!(candidates[0].file, "doc.tex");
        assert_eq!(candidates[0].line, if overlay { 5 } else { 4 });
    }
}

#[test]
fn undefined_form_reference_keeps_clipped_graphics_host_identity() {
    // A clipped `\includegraphics` leaf holds only a form reference with no `<`
    // definition, and each natural box overlaps its neighboring image.
    let host = |line: u32, x: u32, forms: &[u32]| {
        let leaf: String = forms.iter().map(|tag| format!("f{tag}:{x},100\n")).collect();
        format!(
            concat!(
                "(1,{line}:{x},100:30,30,0\n",
                "(1,{line}:{x},100:30,30,0\n",
                "(1,{line}:{x},100:0,30,0\n",
                "(1,{line}:{x},100:0,60,0\n",
                "(1,{line}:{x},100:200,60,0\n",
                "{leaf}",
                ")\n",
                "k1,{line}:{x},100:-200\n",
                ")\n)\n)\n)\n",
            ),
            line = line,
            x = x,
            leaf = leaf,
        )
    };
    let document = |forms: &[&[u32]; 2]| {
        format!(
            concat!(
                "SyncTeX Version:1\nInput:1:/tmp/doc.tex\nOutput:pdf\n",
                "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
                "{{1\n[1,6:0,0:400,792,0\n(1,6:10,100:300,60,0\n",
                "{left}{right})\n",
                "(1,5:10,150:300,10,0\ng1,5:10,150\nx1,5:200,150\n)\n",
                "]\n}}1\nPostamble:\nCount:40\n",
            ),
            left = host(3, 50, forms[0]),
            right = host(4, 100, forms[1]),
        )
    };
    let unclipped = document(&[&[], &[]]);
    let clipped = document(&[&[7], &[8]]);
    let repeated = document(&[&[7, 9], &[8]]);
    for (name, synctex) in [
        ("unclipped", &unclipped),
        ("clipped", &clipped),
        ("repeated", &repeated),
    ] {
        for fix_cross_file_tags in [false, true] {
            for (x, y, line) in [(60.0, 85.0, 3), (115.0, 85.0, 4), (40.0, 145.0, 5)] {
                let candidates = inverse_candidates_from_reader_with_options(
                    synctex.as_bytes(),
                    InversePoint {
                        page: 1,
                        x,
                        y_from_top: y,
                        page_height_pt: 792.0,
                    },
                    fix_cross_file_tags,
                )
                .unwrap();
                assert_eq!(candidates.len(), 1, "{name} ({x},{y}): {candidates:?}");
                assert_eq!(candidates[0].line, line, "{name} ({x},{y})");
            }
        }
    }

    // Forward geometry ignores the undefined placeholder too.
    let forward = |synctex: &str, line: u32| {
        forward_candidates_from_reader(
            synctex.as_bytes(),
            Path::new("/tmp/doc.pdf"),
            Path::new("/tmp/doc.tex"),
            None,
            line,
        )
        .unwrap()
        .into_iter()
        .map(|candidate| (candidate.page, candidate.source_line, candidate.rect))
        .collect::<Vec<_>>()
    };
    for line in [3, 4] {
        let expected = forward(&unclipped, line);
        assert!(!expected.is_empty());
        assert_eq!(forward(&clipped, line), expected);
    }

    // A defined form still owns its recorded content.
    let defined = format!(
        concat!(
            "SyncTeX Version:1\nInput:1:/tmp/doc.tex\nOutput:pdf\n",
            "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
            "<7\n(1,9:0,60:200,60,0\nx1,9:200,60\n)\n>\n",
            "{{1\n[1,6:0,0:400,792,0\n{host}]\n}}1\nPostamble:\nCount:40\n",
        ),
        host = host(3, 50, &[7]),
    );
    let candidates = inverse_candidates_from_reader(
        defined.as_bytes(),
        InversePoint {
            page: 1,
            x: 60.0,
            y_from_top: 85.0,
            page_height_pt: 792.0,
        },
    )
    .unwrap();
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    assert_eq!(candidates[0].line, 9);

    // The clipped image dispatches to its own filename, ignoring embedded vector text.
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    let source = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\includegraphics[clip,width=30pt]{figs/left.pdf}\n",
        "\\includegraphics[clip,width=30pt]{figs/right.pdf}\n",
        "Trailing prose.\n",
        "\\end{document}\n",
    );
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(&tex, source).unwrap();
    fs::write(
        pdf.with_extension("synctex"),
        clipped.replace("/tmp/doc.tex", &tex.to_string_lossy()),
    )
    .unwrap();
    for (x, line, filename) in [(60.0, 3, "figs/left.pdf"), (115.0, 4, "figs/right.pdf")] {
        let hit = resolve_inverse(
            &pdf,
            InversePoint {
                page: 1,
                x,
                y_from_top: 85.0,
                page_height_pt: 792.0,
            },
            Some(("Blorf", 0)),
            2,
            &Operation::default(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(hit.location.line, line);
        assert_eq!(
            hit.location.byte_column,
            source
                .lines()
                .nth(line as usize - 1)
                .unwrap()
                .find(filename)
                .unwrap()
        );
        assert!(hit.location.precise);
    }
}

#[test]
fn only_undefined_form_references_leave_their_host_identity_intact() {
    let document = |before: &str, leaf: &str, after: &str| {
        format!(
            concat!(
                "SyncTeX Version:1\nInput:1:/tmp/doc.tex\nOutput:pdf\n",
                "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
                "{before}{{1\n[1,1:0,0:400,792,0\n(1,7:100,100:200,60,0\n{leaf})\n]\n}}1\n",
                "{after}Postamble:\nCount:10\n",
            ),
            before = before,
            leaf = leaf,
            after = after,
        )
    };
    let forward = |synctex: &str, line: u32, fix_readahead: bool| {
        forward_candidates_from_reader_with_options(
            synctex.as_bytes(),
            Path::new("/tmp/doc.pdf"),
            Path::new("/tmp/doc.tex"),
            None,
            line,
            fix_readahead,
        )
        .unwrap_or_default()
        .into_iter()
        .map(|candidate| (candidate.page, candidate.source_line, candidate.rect))
        .collect::<Vec<_>>()
    };
    let definition = "<1\n(1,5:0,60:200,60,0\nx1,5:200,60\n)\n>\n";
    for fix_readahead in [false, true] {
        // Native gives a defined reference's host a zero leading boundary, so nothing is
        // recorded at the host's own line, wherever the definition sits in the sidecar.
        for (before, leaf, after) in [
            (definition, "f1:100,100\n", ""),
            ("", "f1:100,100\n", definition),
            (definition, "f9:100,100\nf1:100,100\n", ""),
        ] {
            let synctex = document(before, leaf, after);
            assert!(forward(&synctex, 7, fix_readahead).is_empty(), "{leaf}");
            assert!(!forward(&synctex, 5, fix_readahead).is_empty(), "{leaf}");
        }

        // An undefined reference leaves the same geometry as a sidecar that never had it.
        for (with_reference, without) in [
            ("f9:100,100\n", ""),
            ("f9:100,100\nx1,7:150,100\n", "x1,7:150,100\n"),
        ] {
            let expected = forward(&document("", without, ""), 7, fix_readahead);
            assert!(!expected.is_empty(), "{with_reference}");
            assert_eq!(
                forward(&document("", with_reference, ""), 7, fix_readahead),
                expected,
                "{with_reference}"
            );
        }
    }
}

#[test]
fn undefined_form_references_parse_as_if_never_recorded() {
    let document = |content: &str, after: &str| {
        format!(
            concat!(
                "SyncTeX Version:1\nInput:1:/tmp/doc.tex\nOutput:pdf\n",
                "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
                "{{1\n[1,1:0,0:400,792,0\n{content}]\n}}1\n{after}Postamble:\nCount:20\n",
            ),
            content = content,
            after = after,
        )
    };
    let forward = |synctex: &str, line: u32, fix_readahead: bool| {
        forward_candidates_from_reader_with_options(
            synctex.as_bytes(),
            Path::new("/tmp/doc.pdf"),
            Path::new("/tmp/doc.tex"),
            None,
            line,
            fix_readahead,
        )
        .map(|candidates| {
            candidates
                .into_iter()
                .map(|candidate| (candidate.page, candidate.source_line, candidate.rect))
                .collect::<Vec<_>>()
        })
        .map_err(|error| error.to_string())
    };
    let inverse = |synctex: &str, x: f32, y_from_top: f32, fix_cross_file_tags: bool| {
        inverse_candidates_from_reader_with_options(
            synctex.as_bytes(),
            InversePoint {
                page: 1,
                x,
                y_from_top,
                page_height_pt: 792.0,
            },
            fix_cross_file_tags,
        )
        .map_err(|error| error.to_string())
    };
    let hbox = |body: &str| format!("(1,7:100,100:200,60,0\n{body})\n");
    let ancestor = concat!(
        "(1,50:10,100:300,60,0\n(1,7:100,100:30,10,0\nf9:100,100\nx1,7:130,100\n)\n",
        "k1,100:140,100:10\ng1,100:150,100\n)\n",
    );
    let definition = "<1\n(1,5:0,60:200,60,0\nx1,5:200,60\n)\n>\n";
    let reference = "f9:100,100\n";
    for (with_reference, after) in [
        // The trailing kern and glue take their identity from what precedes them at close.
        (hbox("f9:100,100\nk1,7:150,100:10\ng1,7:160,100\n"), ""),
        // A kern and glue separated only by the reference still pair up.
        (hbox("k1,7:150,100:10\nf9:100,100\ng1,7:160,100\n"), ""),
        // A glyph run directly after the leading boundary takes the next record's identity.
        (hbox("f9:100,100\nx1,7:150,100\nk1,8:160,100:10\n"), ""),
        // A vbox holding only the reference is empty, so it registers itself.
        ("[1,7:100,100:30,10,0\nf9:100,100\n]\n".to_owned(), ""),
        // The ancestor's mean uses the trailing records' identity before their recoloring.
        (ancestor.to_owned(), ""),
        // A defined reference, wherever its definition sits, stays alongside the undefined one.
        (
            hbox("f9:100,100\nf1:100,100\nk1,7:150,100:10\ng1,7:160,100\n"),
            definition,
        ),
    ] {
        let with = document(&with_reference, after);
        let without = document(&with_reference.replace(reference, ""), after);
        for mode in [false, true] {
            for line in [1, 5, 7, 8, 50, 100] {
                assert_eq!(
                    forward(&with, line, mode),
                    forward(&without, line, mode),
                    "{with_reference} mode={mode} line={line}"
                );
            }
            for (x, y) in [(100.0, 85.0), (120.0, 95.0), (150.0, 85.0), (250.0, 85.0)] {
                assert_eq!(
                    inverse(&with, x, y, mode),
                    inverse(&without, x, y, mode),
                    "{with_reference} mode={mode} ({x},{y})"
                );
            }
        }
    }

    // The measured failures, stated directly rather than only relative to the deleted form.
    let trailing = document(&hbox("f9:100,100\nk1,7:150,100:10\ng1,7:160,100\n"), "");
    for mode in [false, true] {
        assert!(!forward(&trailing, 7, mode).unwrap().is_empty(), "mode={mode}");
    }
    assert_eq!(forward(&document(ancestor, ""), 7, true).unwrap().len(), 2);

    // Only the identity is dropped: a following `=` still means the reference's recorded position.
    assert_eq!(
        forward(&document(&hbox("f9:100,77\nx1,7:150,=\n"), ""), 7, false),
        forward(&document(&hbox("x1,7:150,77\n"), ""), 7, false),
    );
}

#[test]
fn same_line_text_over_graphics_keeps_its_word() {
    // Natural inner boxes overlap the next image, as in a scaled `\includegraphics`.
    let graphic = |line: u32, x: u32, leaf: String| {
        format!(
            concat!(
                "(1,{line}:{x},100:30,30,0\n(1,{line}:{x},100:30,30,0\n",
                "(1,{line}:{x},100:0,30,0\n(1,{line}:{x},100:0,60,0\n",
                "(1,{line}:{x},100:200,60,0\n{leaf})\n",
                "k1,{line}:{x},100:-200\n)\n)\n)\n)\n",
            ),
            line = line,
            x = x,
            leaf = leaf,
        )
    };
    let plain = |line: u32, x: u32| graphic(line, x, format!("k1,{line}:{},100:0\n", x + 200));
    let clipped = |line: u32, x: u32| graphic(line, x, format!("f{line}:{x},100\n"));
    let overlay = |line: u32, x: u32| {
        format!(
            "(1,{line}:{x},90:30,10,0\ng1,{line}:{x},90\nx1,{line}:{},90\n)\n",
            x + 30
        )
    };
    let separate = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\includegraphics[width=30pt]{figs/left.pdf}\n",
        "\\includegraphics[width=30pt]{figs/right.pdf}\\llap{Overlay text}\n",
        "\\end{document}\n",
    );
    let same_line = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\includegraphics[width=30pt]{figs/a.pdf}\\includegraphics[width=30pt]{figs/b.pdf}\\llap{Overlay text}\n",
        "\\end{document}\n",
    );
    let word = Some("Overlay text");
    let vector = Some("Blorf");
    for (source, body, clicks) in [
        (
            separate,
            [plain(3, 50), plain(4, 100), overlay(4, 100)].concat(),
            [
                (115.0, 85.0, word, 4, "Overlay text"),
                (115.0, 75.0, vector, 4, "figs/right.pdf"),
                (60.0, 85.0, vector, 3, "figs/left.pdf"),
            ],
        ),
        (
            separate,
            [clipped(3, 50), clipped(4, 100), overlay(4, 100)].concat(),
            [
                (115.0, 85.0, word, 4, "Overlay text"),
                (115.0, 75.0, vector, 4, "figs/right.pdf"),
                (60.0, 85.0, vector, 3, "figs/left.pdf"),
            ],
        ),
        (
            same_line,
            [plain(3, 50), plain(3, 100), overlay(3, 100)].concat(),
            [
                (115.0, 85.0, word, 3, "Overlay text"),
                (115.0, 75.0, None, 3, "figs/b.pdf"),
                (60.0, 85.0, None, 3, "figs/a.pdf"),
            ],
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let pdf = root.join("doc.pdf");
        let tex = root.join("doc.tex");
        fs::write(&pdf, b"pdf-v1").unwrap();
        fs::write(&tex, source).unwrap();
        fs::write(
            pdf.with_extension("synctex"),
            format!(
                concat!(
                    "SyncTeX Version:1\nInput:1:{tex}\nOutput:pdf\n",
                    "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
                    "{{1\n[1,2:0,0:400,792,0\n(1,2:10,100:300,60,0\n{body})\n]\n}}1\n",
                    "Postamble:\nCount:40\n",
                ),
                tex = tex.display(),
                body = body,
            ),
        )
        .unwrap();
        for (x, y, context, line, needle) in clicks {
            let hit = resolve_inverse(
                &pdf,
                InversePoint {
                    page: 1,
                    x,
                    y_from_top: y,
                    page_height_pt: 792.0,
                },
                context.map(|context| (context, 0)),
                2,
                &Operation::default(),
                None,
                false,
            )
            .unwrap();
            assert_eq!(hit.location.line, line, "({x},{y}) {needle}");
            assert_eq!(
                hit.location.byte_column,
                source
                    .lines()
                    .nth(line as usize - 1)
                    .unwrap()
                    .find(needle)
                    .unwrap(),
                "({x},{y}) {needle}"
            );
            assert!(hit.location.precise, "({x},{y}) {needle}");
        }
    }
}

#[test]
fn printing_overrides_of_reference_wrappers_do_not_own_their_text() {
    let source_with = |preamble: &str, body: &str| {
        format!("\\documentclass{{article}}\n{preamble}\n\\begin{{document}}\n{body}\n\\end{{document}}\n")
    };
    let label_byte = |source: &str| source.lines().nth(3).unwrap().find("sec:a").unwrap();
    for (preamble, body, context) in [
        (
            "\\renewcommand{\\emph}[1]{Zed #1}",
            "See \\emph{\\cref{sec:a}} then.",
            "See Zed section 1 then.",
        ),
        (
            "\\renewcommand{\\clearpage}{\\textsc{Zed}}",
            "See \\cref{sec:a}\\clearpage then.",
            "See section 1 Zed then.",
        ),
    ] {
        let source = source_with(preamble, body);
        let label = label_byte(&source);
        for word in ["Zed", "section", "1"] {
            let offset = context.find(word).unwrap();
            assert_ne!(
                source_word_location(&source, 4, context, offset, 2),
                Some((4, label)),
                "{body} {word}"
            );
            let guess = source_word_guess_with_sources(
                &source,
                4,
                context,
                offset,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            );
            assert!(
                !matches!(guess, Ok(Some(location)) if location.line == 4 && location.byte == label),
                "{body} {word}"
            );
        }
    }

    // Neither the unmodified wrapper nor an unrelated override costs the label its precision.
    for preamble in ["", "\\renewcommand{\\textbf}[1]{Zed #1}"] {
        let source = source_with(preamble, "See \\emph{\\cref{sec:a}} then.");
        let label = label_byte(&source);
        let context = "See section 1 then.";
        for word in ["section", "1"] {
            let offset = context.find(word).unwrap();
            assert_eq!(
                source_word_location(&source, 4, context, offset, 2),
                Some((4, label)),
                "{preamble} {word}"
            );
        }
    }
}

#[test]
fn frame_options_do_not_own_printed_words_but_titles_keep_theirs() {
    let multiline = "\\begin{frame} <1->[<+->]\n[label=syntheticish-theorem,\n  t]{Theorem statement}";
    let commented = "\\begin{frame}% keep header\n[label=syntheticish-theorem]{Theorem statement}";
    for (header, anchor, line, context, needle, owned) in [
        (
            "\\begin{frame}[label=syntheticish-theorem]\n\\begin{theorem}\nBounded claim holds here.\n\\end{theorem}",
            3,
            3,
            "Theorem 1. Bounded claim holds here.",
            "[label=syntheticish-theorem]",
            false,
        ),
        (
            multiline,
            4,
            4,
            "syntheticish theorem",
            "[label=syntheticish-theorem,",
            false,
        ),
        (multiline, 5, 5, "Theorem statement", "Theorem statement", true),
        // A comment newline continues the header, so the option after it is still metadata.
        (
            commented,
            4,
            4,
            "syntheticish theorem",
            "[label=syntheticish-theorem]",
            false,
        ),
        (commented, 4, 4, "Theorem statement", "Theorem statement", true),
        // A blank line ends the header: this bracket is printed body text, not an option.
        (
            "\\begin{frame}\n\n[Bracketword ordinary body.]",
            6,
            5,
            "Bracketword ordinary body.",
            "Bracketword ordinary body.",
            true,
        ),
        (
            "\\begin{frame}[label=é-théorème]{Théorème énoncé}",
            3,
            3,
            "Théorème énoncé",
            "Théorème énoncé",
            true,
        ),
        (
            "\\begin{frame}[label=topic-theorem]{Primary Topic}{Theorem subtitle}",
            3,
            3,
            "Theorem subtitle",
            "Theorem subtitle",
            true,
        ),
        // Only frame options are configuration: other environments can print theirs.
        (
            "\\begin{frame}\n\\begin{theorem}[Printed Name]\nBounded claim holds here.\n\\end{theorem}",
            4,
            4,
            "Printed Name",
            "Printed Name",
            true,
        ),
    ] {
        let source = format!(
            "\\documentclass{{beamer}}\n\\begin{{document}}\n{header}\n\\end{{frame}}\n\\end{{document}}\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let pdf = root.join("doc.pdf");
        let tex = root.join("doc.tex");
        fs::write(&pdf, b"pdf-v1").unwrap();
        fs::write(&tex, &source).unwrap();
        fs::write(
            pdf.with_extension("synctex"),
            format!(
                concat!(
                    "SyncTeX Version:1\nInput:1:{tex}\nOutput:pdf\n",
                    "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
                    "{{1\n[1,2:0,0:400,792,0\n(1,{anchor}:10,100:300,10,0\n",
                    "g1,{anchor}:10,100\nx1,{anchor}:310,100\n)\n]\n}}1\n",
                    "Postamble:\nCount:10\n",
                ),
                tex = tex.display(),
                anchor = anchor,
            ),
        )
        .unwrap();
        let hit = resolve_inverse(
            &pdf,
            InversePoint {
                page: 1,
                x: 50.0,
                y_from_top: 95.0,
                page_height_pt: 792.0,
            },
            Some((context, 0)),
            2,
            &Operation::default(),
            None,
            false,
        )
        .unwrap();
        let start = source
            .lines()
            .nth(line as usize - 1)
            .unwrap()
            .find(needle)
            .unwrap();
        let span = start..start + needle.len();
        if owned {
            assert_eq!(hit.location.line, line, "{header}");
            assert_eq!(hit.location.byte_column, start, "{header}");
            assert!(hit.location.precise, "{header}");
        } else {
            assert!(
                !(hit.location.line == line
                    && hit.location.precise
                    && span.contains(&hit.location.byte_column)),
                "{header}: {:?}",
                hit.location
            );
            let guess = source_word_guess_with_sources(
                &source,
                anchor,
                context,
                0,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            );
            assert!(
                !matches!(guess, Ok(Some(location)) if location.line == line && span.contains(&location.byte)),
                "{header}"
            );
        }
    }
}

#[test]
fn compatible_columns_do_not_require_macro_execution_classification() {
    let directory = tempfile::tempdir().unwrap();
    let pdf = directory.path().join("gnimble.pdf");
    let file = directory.path().join("gnimble.tex");
    for newline in ["\n", "\r\n"] {
        let source = [
            r"\documentclass{beamer}",
            r"\newcommand{\zoggle}[1]{\ifnum#1>0 \expandafter\def\csname bimble#1\endcsname{}\expandafter\zoggle\expandafter{\number\numexpr#1-1\relax}\fi}",
            r"\zoggle{2}",
            r"\begin{document}",
            r"\begin{frame}{Zibble}",
            "Frumple snazzling zoggle.",
            r"\end{frame}",
            r"\end{document}",
        ].join(newline);
        fs::write(&file, source).unwrap();
        let result = inverse_output(
            &pdf,
            "Input: gnimble.tex\nLine: 7\n",
            Some(("Frumple snazzling zoggle.", 8)),
        )
        .unwrap();
        assert_eq!(result.location.line, 6);
        assert_eq!(result.location.byte_column, 8);
        assert!(result.location.precise);
    }
}

#[test]
fn incompatible_neighboring_word_keeps_the_native_line() {
    let directory = tempfile::tempdir().unwrap();
    let pdf = directory.path().join("gnimble.pdf");
    fs::write(
        directory.path().join("gnimble.tex"),
        "Frumple snazzling zoggle.\nBrindle wuzzling troggle.\n",
    )
    .unwrap();
    let result = resolve_inverse_output(
        &pdf,
        InversePoint {
            page: 1,
            x: 72.0,
            y_from_top: 120.0,
            page_height_pt: 792.0,
        },
        Some(("Frumple snazzling zoggle.", 8)),
        3,
        &Operation::default(),
        "Input: gnimble.tex\nLine: 2\n",
        QuerySources::default(),
    )
    .unwrap();
    assert_eq!(result.location.line, 2);
    assert_eq!(result.location.byte_column, 0);
    assert!(!result.location.precise);
}

#[test]
fn inverse_title_refinement_stays_native_supported() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("paper.tex");
    let pdf = directory.path().join("paper.pdf");
    let context = "Computational Fluid Dynamics";
    for (preamble, body, anchor, expected) in [
        (
            "\\title{Computational Fluid Dynamics}",
            "\\begin{frame}{Computational Fluid Dynamics}\n\\end{frame}",
            4,
            (3, 28, true),
        ),
        (
            "\\title{Computational Fluid Dynamics}",
            "\\maketitle",
            3,
            (3, 0, false),
        ),
        (
            "\\documentclass{beamer}",
            "\\title[Computational Fluid Dynamics]{Computational Fluid Dynamics}\\maketitle",
            3,
            (3, 51, true),
        ),
        (
            "\\documentclass{beamer}",
            "\\begin{frame}\n\\title[Computational Fluid Dynamics]{Computational Fluid Dynamics}\\maketitle\n\\end{frame}",
            5,
            (4, 51, true),
        ),
    ] {
        fs::write(
            &source,
            format!("{preamble}\n\\begin{{document}}\n{body}\n\\end{{document}}\n"),
        )
        .unwrap();
        fs::write(
            pdf.with_extension("synctex"),
            format!(
                "SyncTeX Version:1\nInput:1:{}\n\
                 Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n\
                 {{1\n[1,{anchor}:0,0:100,100,0\n(1,{anchor}:0,0:100,100,0\n)\n]\n}}1\n\
                 Postamble:\nCount:4\n",
                source.display()
            ),
        )
        .unwrap();
        for y_from_top in [10.0, 228.0] {
            let result = resolve_inverse_output(
                &pdf,
                InversePoint {
                    page: 1,
                    x: 72.0,
                    y_from_top,
                    page_height_pt: 272.0,
                },
                Some((context, context.find("Fluid").unwrap())),
                4,
                &Operation::default(),
                &format!("Input: paper.tex\nLine: {anchor}\n"),
                QuerySources::default(),
            )
            .unwrap();
            assert_eq!(
                (
                    result.location.line,
                    result.location.byte_column,
                    result.location.precise,
                ),
                expected,
                "body={body}, y_from_top={y_from_top}",
            );
        }
    }
}

#[test]
fn caption_review_embedded_comment_end_does_not_expose_hidden_caption() {
    let source = "\\begin{comment}\nignored \\end{comment}\n\
                  \\caption{hidden\ncaption}\n\\end{comment}";
    assert_eq!(collected_text_closing_line(source, 3, 10), None);
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
    assert_eq!(collected_text_closing_line(source, 1, column), None);
    assert_eq!(math::collected_text_lines(source, 2), None);
    let source = "\\caption[{short[inner]}]{first\nsecond}";
    let column = source.find("first").unwrap() as u32 + 1;
    assert_eq!(collected_text_closing_line(source, 1, column), Some(2));
    assert_eq!(math::collected_text_lines(source, 2), Some(0..2));
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
fn inverse_line_leading_word_retains_native_readahead_anchor() {
    let source = "Hauling a kettle slowly past a lopsided\n\
                  turnip barrow with inline $V$ beside damp gravel nudges an\n\
                  brambling porridges teakettle:";
    assert_eq!(
        tex::source_word_location(source, 2, "brambling", 0, 12),
        Some((3, 0)),
    );
    assert_eq!(source_word_compiler_line(source, 3, 1), 2);
    assert_eq!(source_word_compiler_line(source, 2, 1), 1);
    assert_eq!(source_word_compiler_line(source, 3, 11), 3);
    assert_eq!(
        tex::line_start_word_compiler_line("Prior prose\n  Café next", 2, 3),
        Some(1)
    );
    assert_eq!(
        tex::line_start_word_compiler_line("Pewter $R_\\star$ nudges an\nbrambling teapot", 2, 1),
        Some(1)
    );
    for (source, line, column) in [
        ("brambling porridges", 1, 1),
        ("Prior prose\n\nbrambling porridges", 3, 1),
        ("% comment\nbrambling porridges", 2, 1),
        ("Prior prose\\par\nbrambling porridges", 2, 1),
        ("\\section{Copper}\n1 Wazzit.", 2, 1),
        ("\\section{Copper\nspindles}\n1 Wazzit.", 3, 1),
        ("\\section{Copper\nspindles}\\relax\n1 Wazzit.", 3, 1),
        ("\\section{Copper\n\\{spindles}\n1 Wazzit.", 3, 1),
        ("\\section\n{Copper}\n1 Wazzit.", 3, 1),
        ("\\section\nCopper\n1 Wazzit.", 3, 1),
        ("\\section\n% comment\nCopper\n1 Wazzit.", 4, 1),
        ("\\section\n[Short]\nC\n1 Wazzit.", 4, 1),
        (
            "\\newcommand{\\Unused}{\\verb}\n\\section\nC\n1 Wazzit.",
            4,
            1,
        ),
        ("Prior \\xref{spindle}\nbrambling porridges", 2, 1),
        ("\\tableofcontents\nContents Wazzit.", 2, 1),
        ("Prior {prose\nbrambling porridges", 2, 1),
        ("\\paragraph{Copper}\nWazzit body.", 2, 1),
        ("\\item\n1 Wazzit.", 2, 1),
        ("\\end{figure}\nWazzit body.", 2, 1),
        ("Prior prose\nbrambling porridges", 2, 0),
        ("Prior prose\nbrambling porridges", 2, 11),
        ("Prior prose\n\\command brambling", 2, 10),
    ] {
        assert_eq!(
            tex::line_start_word_compiler_line(source, line, column),
            None
        );
    }
}

#[test]
fn multiline_formatting_uses_collected_scope_without_losing_outer_boundaries() {
    for wrapper in [
        "textbf",
        "textit",
        "emph",
        "textnormal",
        "texttt",
        "textsf",
        "mathbf",
        "mathrm",
        "mathsf",
        "mathbb",
    ] {
        let source = format!("\\{wrapper}{{Café Snazzles\nWazzit}}");
        let byte = source.find("Snazzles").unwrap();
        let column = source[..byte].chars().count() as u32 + 1;
        assert_eq!(source_word_compiler_line(&source, 1, column), 2);
        assert_eq!(
            tex::source_word_location(&source, 2, "Snazzles", 0, 0),
            Some((1, byte)),
        );
    }
    for (source, word, expected) in [
        (
            "\\textbf{Outer \\textit{Snazzles\nWazzit}\nTail}",
            "Snazzles",
            3,
        ),
        (
            "\\begin{align}\n\\text{Snazzles\nWazzit}\n\\end{align}",
            "Snazzles",
            4,
        ),
        ("\\textbf{$A\nB$\nSnazzles}", "A", 3),
        (
            "\\subparagraph*{\\textbf{Snazzles\nWazzit}}\nBody.",
            "Snazzles",
            3,
        ),
    ] {
        let at = source.find(word).unwrap();
        let prefix = &source[..at];
        let line = prefix.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
        let start = prefix.rfind('\n').map_or(0, |at| at + 1);
        let column = source[start..at].chars().count() as u32 + 1;
        assert_eq!(
            source_word_compiler_line(source, line, column),
            expected,
            "{source}"
        );
    }
    let source = "\\begin{frame}\nSnazzles only.\n\\textbf{Copper\nNickel}\\end{frame}";
    assert_eq!(
        tex::source_word_location(source, 4, "Snazzles", 0, 0),
        Some((2, 0)),
    );
    let adjoining = "\\textbf{Snazzles frobnitz\nquux}\\begin{tabularx}{\\linewidth}{X}\nCopper\\\\\n\\end{tabularx}";
    assert_eq!(
        tex::source_word_location(adjoining, 2, "frobnitz", 0, 0),
        Some((1, adjoining.find("frobnitz").unwrap())),
    );
    assert_eq!(
        tex::source_word_location("\\textbf{Snazzles\nSnazzles}", 1, "Snazzles", 0, 4),
        Some((1, 8)),
    );
    let quoted = "\\csname string\\endcsname Zephyr";
    assert_eq!(
        tex::source_word_location(quoted, 1, "Zephyr", 0, 0),
        Some((1, quoted.find("Zephyr").unwrap())),
    );
    for source in [
        "\\string\\textbf{Snazzles\nWazzit}",
        "\\noexpand\\textbf{Snazzles\nWazzit}",
        "\\csname string\\endcsname\\textbf{Snazzles\nWazzit}",
        "\\csname noexpand\\endcsname\\textbf{Snazzles\nWazzit}",
        "\\mathord{Snazzles\nWazzit}",
        "\\mathrel{Snazzles\nWazzit}",
        "\\mathop{Snazzles\nWazzit}",
        "\\verb|\\textbf{Snazzles\nWazzit}|",
        "\\newcommand{\\hidden}{\\textbf{Snazzles\nWazzit}}",
        "% \\textbf{Snazzles\nWazzit}",
        "\\textbf{Snazzles",
    ] {
        let byte = source.find("Snazzles").unwrap();
        assert_eq!(
            math::collected_text_argument_at(source, byte),
            None,
            "{source}"
        );
        assert_eq!(math::collected_text_lines(source, 1), None, "{source}");
    }
    assert_eq!(math::collected_text_lines("\\textbf{Snazzles}", 1), None);
}

#[test]
fn environment_identifiers_are_not_literal_output_but_titles_remain_visible() {
    let source = "\\begin{zibble}[Frumple snazzling]\nBlorple wazzit.\n\\end{zibble}";
    for (word, expected) in [
        ("zibble", None),
        ("Frumple", Some((1, source.find("Frumple").unwrap()))),
        ("snazzling", Some((1, source.find("snazzling").unwrap()))),
        ("wazzit", Some((2, 8))),
    ] {
        assert_eq!(
            tex::source_word_location_with_sources(
                source,
                1,
                word,
                0,
                4,
                &[],
                &Operation::default(),
            )
            .map(WordOutcome::location),
            Ok(expected),
            "{word}",
        );
    }
}

#[test]
fn run_in_heading_aliases_skip_labels_but_not_literal_commands() {
    for prefix in [
        "",
        "\\label{key}",
        "\\label{key}\n\\index{entry}",
        "% comment",
    ] {
        let source = format!("\\subparagraph*{{Café Snazzles}}{prefix}\nBody text.");
        let column = source[..source.find("Snazzles").unwrap()].chars().count() as u32 + 1;
        assert_eq!(
            tex::run_in_heading_compiler_line(&source, 1, column),
            Some(source.lines().count() as u32),
            "{source}",
        );
    }
    assert_eq!(
        tex::run_in_heading_compiler_line(
            "\\paragraph X\nFirst body.\n\n\\paragraph{Snazzles}\nSecond body.",
            4,
            12,
        ),
        Some(5),
    );
    for source in [
        "\\verb|\\subparagraph*{Snazzles}|\nBody text.",
        "\\string\\subparagraph*{Snazzles}\nBody text.",
        "\\begin{verbatim}\n\\subparagraph*{Snazzles}\nBody text.\n\\end{verbatim}",
        "% \\subparagraph*{Snazzles}\nBody text.",
        "\\subparagraph*{Snazzles}\n\\end{document}",
    ] {
        let at = source.find("Snazzles").unwrap();
        let line = source[..at].bytes().filter(|&b| b == b'\n').count() as u32 + 1;
        let row_start = source[..at].rfind('\n').map_or(0, |at| at + 1);
        let column = source[row_start..at].chars().count() as u32 + 1;
        assert_eq!(
            tex::run_in_heading_compiler_line(source, line, column),
            None,
            "{source}"
        );
    }
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
            assert_eq!(collected_text_closing_line(&source, 3, 10), None);
            assert_eq!(collected_text_closing_line(&source, 6, 10), Some(7));
            let unclosed = format!(
                "\\begin{{comment}}{newline}{fake}{newline}\\caption{{hidden{newline}caption}}"
            );
            assert_eq!(collected_text_closing_line(&unclosed, 3, 10), None);
        }
    }
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "\\begin{{comment}}\\end{{comment}}{newline}\\caption{{hidden{newline}caption}}{newline}\\end{{comment}}   {newline}\\caption{{visible{newline}caption}}"
        );
        assert_eq!(collected_text_closing_line(&source, 2, 10), None);
        assert_eq!(collected_text_closing_line(&source, 5, 10), Some(6));
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
        assert_eq!(collected_text_closing_line(&source, 3, 2), Some(5));
        assert_eq!(collected_text_closing_line(&source, 4, 1), Some(5));
        for (line, column) in [(1, 1), (2, 1), (3, 13), (5, 1), (6, 1), (0, 1), (3, 0)] {
            assert_eq!(collected_text_closing_line(&source, line, column), None);
        }
    }
    for source in [
        "\\caption[short]{unclosed\nbody",
        "\\caption[unclosed{body}",
        "\\caption{body\n} trailing",
    ] {
        let last = source.lines().count() as u32;
        let column = source.lines().last().unwrap().chars().count() as u32;
        assert_eq!(collected_text_closing_line(source, last, column), None);
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
        assert_eq!(
            collected_text_closing_line(source, line, column),
            None,
            "{word}"
        );
    }
    assert_eq!(collected_text_closing_line(source, 7, 17), Some(8));
    let source = "\\begin{frame}\n\\caption{first\nsecond}\n\\end{frame}";
    assert_eq!(collected_text_closing_line(source, 2, 10), Some(3));
    assert_eq!(
        frame_closing_line(source, 2).or_else(|| collected_text_closing_line(source, 2, 10)),
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
fn inverse_reference_commands_select_first_label_byte_inside_braces() {
    let source = "\\section{Alpha}\\label{sec:alpha}\n\
                  We check \\cref{sec:alpha} and \\Cref{thm:beta} before \\eqref{eq:gamma} \
                  on page \\pageref{sec:alpha} via \\autoref{thm:beta} and \\ref{sec:alpha} \
                  near \\cref{fig:square}.\n";
    let context = "We check section 1 and Theorem 1 before (1) on page 1 via Theorem 1 and 1 near fig. 1.";
    let row = source.lines().nth(1).unwrap();
    let expected = [
        ("section", "\\cref{", "sec:alpha"),
        ("1 and Theorem", "\\cref{", "sec:alpha"),
        ("Theorem 1 before", "\\Cref{", "thm:beta"),
        ("1 before", "\\Cref{", "thm:beta"),
        ("(1)", "\\eqref{", "eq:gamma"),
        ("1 via", "\\pageref{", "sec:alpha"),
        ("Theorem 1 and", "\\autoref{", "thm:beta"),
        ("1 and 1 near", "\\autoref{", "thm:beta"),
        ("1 near", "\\ref{", "sec:alpha"),
        ("fig. 1.", "\\cref{", "fig:square"),
        ("1.", "\\cref{", "fig:square"),
    ];
    for (needle, prefix, label) in expected {
        let offset = context.find(needle).unwrap();
        let target_byte = row.find(&format!("{prefix}{label}")).unwrap() + prefix.len();
        assert_eq!(
            source_word_location(source, 2, context, offset, 2),
            Some((2, target_byte)),
            "strict failed for {needle}"
        );
        assert_eq!(
            source_word_guess_with_sources(
                source,
                2,
                context,
                offset,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 2,
                byte: target_byte,
            })),
            "guess failed for {needle}"
        );
    }

    // Multiline, leading spaces/comments, multi-label lists, and non-ASCII UTF-8 prefix columns.
    let multiline = "Café check \\cref{%\n  sec:first,sec:second\n} done.\n";
    let multi_ctx = "Café check sections 1 and 2 done.";
    let first_byte = multiline.lines().nth(1).unwrap().find("sec:first").unwrap();
    for token in ["sections", "1", "and", "2"] {
        let offset = multi_ctx.find(token).unwrap();
        assert_eq!(
            source_word_location(multiline, 3, multi_ctx, offset, 3),
            Some((2, first_byte)),
            "multiline token {token}"
        );
        assert_eq!(
            source_word_guess_with_sources(
                multiline,
                3,
                multi_ctx,
                offset,
                3,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 2,
                byte: first_byte,
            })),
            "multiline guess token {token}"
        );
    }
    let col_char = multiline.lines().nth(1).unwrap()[..first_byte].chars().count() as u32 + 1;
    assert_eq!(
        tex::reference_argument_closing_line(multiline, 2, col_char),
        Some(3)
    );

    let custom_format = "\\crefformat{section}{#2#1#3 zone}\nBefore \\cref{sec:alpha} after.\n";
    let custom_ctx = "Before 1 zone after.";
    let custom_byte = custom_format.lines().nth(1).unwrap().find("sec:alpha").unwrap();
    for token in ["1", "zone"] {
        let offset = custom_ctx.find(token).unwrap();
        assert_eq!(
            source_word_location(custom_format, 2, custom_ctx, offset, 1),
            Some((2, custom_byte)),
            "custom format token {token}"
        );
        assert_eq!(
            source_word_guess_with_sources(
                custom_format,
                2,
                custom_ctx,
                offset,
                1,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 2,
                byte: custom_byte,
            })),
            "custom format guess token {token}"
        );
    }

    // Non-reference generated constructs (\cite, \item, \caption, theorem headers) must not steal reference labels.
    let cite_src = "Note \\cite{bib:one} before \\cref{sec:alpha} done.\n";
    let cite_ctx = "Note [7] before section 1 done.";
    let cite_ref_byte = cite_src.find("sec:alpha").unwrap();
    assert_ne!(
        source_word_location(cite_src, 1, cite_ctx, cite_ctx.find('7').unwrap(), 1),
        Some((1, cite_ref_byte))
    );
    assert_eq!(
        source_word_location(cite_src, 1, cite_ctx, cite_ctx.find("section").unwrap(), 1),
        Some((1, cite_ref_byte))
    );

    let item_src = "\\item Item before \\cref{sec:alpha} done.\n";
    let item_ctx = "1. Item before section 1 done.";
    let item_ref_byte = item_src.find("sec:alpha").unwrap();
    assert_ne!(
        source_word_location(item_src, 1, item_ctx, 0, 1),
        Some((1, item_ref_byte))
    );
    assert_eq!(
        source_word_location(item_src, 1, item_ctx, item_ctx.rfind('1').unwrap(), 1),
        Some((1, item_ref_byte))
    );

    let cap_src = "\\caption{Caption before \\cref{sec:alpha} done.}\n";
    let cap_ctx = "Figure 1: Caption before section 1 done.";
    let cap_ref_byte = cap_src.find("sec:alpha").unwrap();
    assert_ne!(
        source_word_location(cap_src, 1, cap_ctx, cap_ctx.find("Figure").unwrap(), 1),
        Some((1, cap_ref_byte))
    );
    assert_ne!(
        source_word_location(cap_src, 1, cap_ctx, cap_ctx.find("1:").unwrap(), 1),
        Some((1, cap_ref_byte))
    );
    assert_eq!(
        source_word_location(cap_src, 1, cap_ctx, cap_ctx.find("section").unwrap(), 1),
        Some((1, cap_ref_byte))
    );

    let thm_src = "\\newtheorem{thm}{Theorem}\n\\begin{thm}Claim before \\cref{sec:alpha} done.\\end{thm}\n";
    let thm_ctx = "Theorem 1. Claim before section 1 done.";
    let thm_ref_byte = thm_src.lines().nth(1).unwrap().find("sec:alpha").unwrap();
    assert_ne!(
        source_word_location(thm_src, 2, thm_ctx, thm_ctx.find("Theorem").unwrap(), 1),
        Some((2, thm_ref_byte))
    );
    assert_ne!(
        source_word_location(thm_src, 2, thm_ctx, thm_ctx.find("1.").unwrap(), 1),
        Some((2, thm_ref_byte))
    );
    assert_eq!(
        source_word_location(thm_src, 2, thm_ctx, thm_ctx.find("section").unwrap(), 1),
        Some((2, thm_ref_byte))
    );

    // Same-line literal word ("section") and generated reference ("section 1") each resolve to their own owner.
    let lit_src = "The word section precedes \\cref{sec:alpha} here.\n";
    let lit_ctx = "The word section precedes section 1 here.";
    let lit_word_byte = lit_src.find("section").unwrap();
    let lit_ref_byte = lit_src.find("sec:alpha").unwrap();
    assert_eq!(
        source_word_location(lit_src, 1, lit_ctx, lit_ctx.find("section").unwrap(), 1),
        Some((1, lit_word_byte))
    );
    assert_eq!(
        source_word_location(lit_src, 1, lit_ctx, lit_ctx.rfind("section").unwrap(), 1),
        Some((1, lit_ref_byte))
    );
    assert_eq!(
        source_word_location(lit_src, 1, lit_ctx, lit_ctx.find('1').unwrap(), 1),
        Some((1, lit_ref_byte))
    );

    // Paragraph-end reference when PDF context continues into the next paragraph and search_radius == 1.
    let para_src = "Paragraph ends at \\cref{sec:alpha}.\n\nNext paragraph starts here.\n";
    let para_ctx = "Paragraph ends at section 1. Next paragraph starts here.";
    let para_ref_byte = para_src.lines().next().unwrap().find("sec:alpha").unwrap();
    assert_eq!(
        source_word_location(para_src, 1, para_ctx, para_ctx.find("section").unwrap(), 1),
        Some((1, para_ref_byte))
    );
    assert_eq!(
        source_word_location(para_src, 1, para_ctx, para_ctx.find('1').unwrap(), 1),
        Some((1, para_ref_byte))
    );

    // Comment-only line between source lines does not create a false paragraph boundary, and off-line reference is not invisible.
    let twoline_src = "Lead \\cref{sec:first} middle\n% comment line\nand \\cref{sec:second} tail.\n";
    let twoline_ctx = "Lead section 1 middle and section 2 tail.";
    let second_ref_byte = twoline_src.lines().nth(2).unwrap().find("sec:second").unwrap();
    assert_eq!(
        source_word_location(twoline_src, 3, twoline_ctx, twoline_ctx.rfind("section").unwrap(), 1),
        Some((3, second_ref_byte))
    );
    assert_eq!(
        source_word_location(twoline_src, 3, twoline_ctx, twoline_ctx.find('2').unwrap(), 1),
        Some((3, second_ref_byte))
    );

    // Reference-first theft controls, invalid key before valid reference, and same-word references on adjacent lines.
    let ref_before_cite = "Note \\cref{sec:alpha} before \\cite{bib:one} done.\n";
    let ref_before_cite_ctx = "Note section 1 before [7] done.";
    let ref_before_cite_byte = ref_before_cite.find("sec:alpha").unwrap();
    assert_eq!(
        source_word_location(ref_before_cite, 1, ref_before_cite_ctx, ref_before_cite_ctx.find('7').unwrap(), 1),
        None
    );
    assert_eq!(
        source_word_guess_with_sources(
            ref_before_cite,
            1,
            ref_before_cite_ctx,
            ref_before_cite_ctx.find('7').unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None)
    );
    assert_eq!(
        source_word_location(ref_before_cite, 1, ref_before_cite_ctx, ref_before_cite_ctx.find("section").unwrap(), 1),
        Some((1, ref_before_cite_byte))
    );

    let item_ref_first = "\\item \\cref{sec:alpha} item text done.\n";
    let item_ref_first_ctx = "1. section 1 item text done.";
    let item_ref_first_byte = item_ref_first.find("sec:alpha").unwrap();
    assert_ne!(
        source_word_location(item_ref_first, 1, item_ref_first_ctx, 0, 1),
        Some((1, item_ref_first_byte))
    );

    let invalid_then_valid = "Lead \\cref{\\macrokey} middle \\cref{sec:second} tail.\n";
    let invalid_then_valid_ctx = "Lead section 1 middle section 2 tail.";
    let valid_second_byte = invalid_then_valid.find("sec:second").unwrap();
    assert_eq!(
        source_word_location(invalid_then_valid, 1, invalid_then_valid_ctx, invalid_then_valid_ctx.find("section").unwrap(), 1),
        None
    );
    assert_eq!(
        source_word_guess_with_sources(
            invalid_then_valid,
            1,
            invalid_then_valid_ctx,
            invalid_then_valid_ctx.find("section").unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None)
    );
    assert_eq!(
        source_word_location(invalid_then_valid, 1, invalid_then_valid_ctx, invalid_then_valid_ctx.rfind("section").unwrap(), 1),
        Some((1, valid_second_byte))
    );

    let adj_lines_src = "First \\cref{sec:one} end.\nSecond \\cref{sec:two} done.\n";
    let adj_lines_ctx = "First section 1 end. Second section 2 done.";
    let adj_one_byte = adj_lines_src.lines().next().unwrap().find("sec:one").unwrap();
    let adj_two_byte = adj_lines_src.lines().nth(1).unwrap().find("sec:two").unwrap();
    assert_eq!(
        source_word_location(adj_lines_src, 1, adj_lines_ctx, adj_lines_ctx.find("section").unwrap(), 1),
        Some((1, adj_one_byte))
    );
    assert_eq!(
        source_word_location(adj_lines_src, 2, adj_lines_ctx, adj_lines_ctx.rfind("section").unwrap(), 1),
        Some((2, adj_two_byte))
    );

    // Repeated-neighbor occurrence alignment: citations, filtered references, math, and two-reference segments
    // with identical surrounding flanks must not be stolen by a reference.
    let rep_cite_src = "By \\cref{sec:a} then \\\\ By \\cite{key} then.\n";
    let rep_cite_ctx = "By section 1 then By [7] then.";
    for offset in [rep_cite_ctx.find('7').unwrap()] {
        assert_eq!(source_word_location(rep_cite_src, 1, rep_cite_ctx, offset, 1), None);
        assert_eq!(
            source_word_guess_with_sources(
                rep_cite_src,
                1,
                rep_cite_ctx,
                offset,
                1,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(None)
        );
    }
    assert_eq!(
        source_word_location(rep_cite_src, 1, "By [7] then.", 4, 1),
        None
    );

    let frame_cite_src = "\\begin{frame}\nBy \\cref{sec:a} then.\n\nBy \\cite{key} then.\n\\end{frame}\n";
    let frame_cite_ctx = "By section 1 then. By [7] then.";
    assert_eq!(
        source_word_location(frame_cite_src, 5, frame_cite_ctx, frame_cite_ctx.find('7').unwrap(), 4),
        None
    );
    assert_eq!(
        source_word_guess_with_sources(
            frame_cite_src,
            5,
            frame_cite_ctx,
            frame_cite_ctx.find('7').unwrap(),
            4,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None)
    );

    let tabx_src = "\\begin{tabularx}{\\linewidth}{lll}\nYes & \\cref{sec:a} & ok \\\\\nYes & \\cite{key} & ok \\\\\n\\end{tabularx}\n";
    let tabx_ctx = "Yes section 1 ok Yes [7] ok";
    assert_eq!(
        source_word_location(tabx_src, 4, tabx_ctx, tabx_ctx.find('7').unwrap(), 4),
        None
    );
    assert_eq!(
        source_word_guess_with_sources(
            tabx_src,
            4,
            tabx_ctx,
            tabx_ctx.find('7').unwrap(),
            4,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None)
    );

    let rep_filtered_src = "By \\cref{sec:a} then \\\\ By \\cref{\\keymacro} then.\n";
    let rep_filtered_ctx = "By section 1 then By section 2 then.";
    assert_eq!(
        source_word_location(rep_filtered_src, 1, rep_filtered_ctx, rep_filtered_ctx.rfind("section").unwrap(), 1),
        None
    );
    assert_eq!(
        source_word_guess_with_sources(
            rep_filtered_src,
            1,
            rep_filtered_ctx,
            rep_filtered_ctx.rfind("section").unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None)
    );

    let rep_math_src = "By \\cref{sec:a} then \\\\ By $\\alpha$ then.\n";
    let rep_math_ctx = "By section 1 then By α then.";
    let rep_math_byte = rep_math_src.find("\\alpha").unwrap();
    assert_eq!(
        source_word_location(rep_math_src, 1, rep_math_ctx, rep_math_ctx.find('α').unwrap(), 1),
        Some((1, rep_math_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            rep_math_src,
            1,
            rep_math_ctx,
            rep_math_ctx.find('α').unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 1,
            byte: rep_math_byte,
        }))
    );

    let frame_and_src = "\\begin{frame}\nBy \\cref{sec:a} then\n\nBy \\cref{sec:b} and \\cref{sec:c} then\n\\end{frame}\n";
    let frame_and_ctx = "By section 1 then By section 2 and section 3 then";
    let frame_and_byte = frame_and_src.lines().nth(3).unwrap().find("and").unwrap();
    assert_eq!(
        source_word_location(frame_and_src, 5, frame_and_ctx, frame_and_ctx.find("and").unwrap(), 4),
        Some((4, frame_and_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            frame_and_src,
            5,
            frame_and_ctx,
            frame_and_ctx.find("and").unwrap(),
            4,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 4,
            byte: frame_and_byte,
        }))
    );

    let para_start_rep_src = "Use the \\cref{sec:a} as the base. Use the \\cite{key} as the base.\n";
    let para_start_rep_ctx = "Use the section 1 as the base. Use the [7] as the base.";
    assert_eq!(
        source_word_location(para_start_rep_src, 1, para_start_rep_ctx, para_start_rep_ctx.find('7').unwrap(), 1),
        None
    );
    assert!(matches!(
        source_word_guess_with_sources(
            para_start_rep_src,
            1,
            para_start_rep_ctx,
            para_start_rep_ctx.find('7').unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None) | Err(())
    ));

    // Zero-word source slots paired with mixed generated+literal segments must not shift ordinals or steal literals.
    let ord_shift_src = "By \\rule{1em}{1pt} then \\\\ By \\cref{sec:a} then \\\\ By \\cite{key} too then.\n";
    let ord_shift_ctx = "By [7] too then.";
    let ord_too_byte = ord_shift_src.find("too").unwrap();
    assert_eq!(
        source_word_location(ord_shift_src, 1, ord_shift_ctx, ord_shift_ctx.find('7').unwrap(), 1),
        None
    );
    assert!(matches!(
        source_word_guess_with_sources(
            ord_shift_src,
            1,
            ord_shift_ctx,
            ord_shift_ctx.find('7').unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None) | Err(())
    ));
    assert_eq!(
        source_word_location(ord_shift_src, 1, ord_shift_ctx, ord_shift_ctx.find("too").unwrap(), 1),
        Some((1, ord_too_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            ord_shift_src,
            1,
            ord_shift_ctx,
            ord_shift_ctx.find("too").unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 1,
            byte: ord_too_byte,
        }))
    );

    let tabx_shift_src = "\\begin{tabularx}{\\linewidth}{lll}\nYes & $-$ & ok \\\\\nYes & \\cref{sec:a} & ok \\\\\nYes & \\cite{key} table & ok \\\\\n\\end{tabularx}\n";
    let tabx_shift_ctx = "Yes − ok Yes section 1 ok Yes [7] table ok";
    let tabx_table_byte = tabx_shift_src.lines().nth(3).unwrap().find("table").unwrap();
    assert_eq!(
        source_word_location(tabx_shift_src, 5, tabx_shift_ctx, tabx_shift_ctx.find('7').unwrap(), 5),
        None
    );
    assert!(matches!(
        source_word_guess_with_sources(
            tabx_shift_src,
            5,
            tabx_shift_ctx,
            tabx_shift_ctx.find('7').unwrap(),
            5,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None) | Err(())
    ));
    assert_eq!(
        source_word_location(tabx_shift_src, 5, tabx_shift_ctx, tabx_shift_ctx.find("table").unwrap(), 5),
        Some((4, tabx_table_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            tabx_shift_src,
            5,
            tabx_shift_ctx,
            tabx_shift_ctx.find("table").unwrap(),
            5,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 4,
            byte: tabx_table_byte,
        }))
    );

    // Main-stream and footnote-stream slots with identical surrounding flanks must compete.
    let fn_cite_src = "See \\cite{key} for details.\\footnote{See \\cref{sec:a} for details.}\n";
    let fn_cite_ctx = "See [7] for details.1";
    assert_eq!(
        source_word_location(fn_cite_src, 1, fn_cite_ctx, fn_cite_ctx.find('7').unwrap(), 1),
        None
    );
    assert!(matches!(
        source_word_guess_with_sources(
            fn_cite_src,
            1,
            fn_cite_ctx,
            fn_cite_ctx.find('7').unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None) | Err(())
    ));

    let fn_math_src = "See $\\alpha$ for details.\\footnote{See \\cref{sec:a} for details.}\n";
    let fn_math_ctx = "See α for details.1";
    let fn_alpha_byte = fn_math_src.find("\\alpha").unwrap();
    assert_eq!(
        source_word_location(fn_math_src, 1, fn_math_ctx, fn_math_ctx.find('α').unwrap(), 1),
        Some((1, fn_alpha_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            fn_math_src,
            1,
            fn_math_ctx,
            fn_math_ctx.find('α').unwrap(),
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 1,
            byte: fn_alpha_byte,
        }))
    );

    // Non-printing spacing/index arguments consume their mandatory group without inserting words.
    let hspace_src = "Before \\hspace{1em} \\cref{sec:a} after.\n";
    let hspace_ctx = "Before section 1 after.";
    let hspace_byte = hspace_src.find("sec:a").unwrap();
    for needle in ["section", "1"] {
        let offset = hspace_ctx.find(needle).unwrap();
        assert_eq!(
            source_word_location(hspace_src, 1, hspace_ctx, offset, 1),
            Some((1, hspace_byte))
        );
        assert_eq!(
            source_word_guess_with_sources(
                hspace_src,
                1,
                hspace_ctx,
                offset,
                1,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 1,
                byte: hspace_byte,
            }))
        );
    }

    // Same-line page breaks must not let context-edge spans claim page-furniture numbers.
    for (footer_src, footer_ctx, num) in [
        ("Alpha \\cref{sec:a}\\clearpage\n", "Alpha section 1 5", '5'),
        ("Beta \\cref{sec:a}\\par\\clearpage\n", "Beta section 1 6", '6'),
        ("\\cref{sec:a} done.\\clearpage\n", "7 section 1 done.", '7'),
    ] {
        let off = footer_ctx.find(num).unwrap();
        assert_eq!(source_word_location(footer_src, 1, footer_ctx, off, 1), None);
        assert!(matches!(
            source_word_guess_with_sources(
                footer_src,
                1,
                footer_ctx,
                off,
                1,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(None) | Err(())
        ));
    }
    let nextline_pb_src = "Gamma \\cref{sec:a}\n\\clearpage\n";
    let nextline_pb_ctx = "Gamma section 1";
    let nextline_pb_byte = nextline_pb_src.find("sec:a").unwrap();
    for needle in ["section", "1"] {
        let off = nextline_pb_ctx.find(needle).unwrap();
        assert_eq!(
            source_word_location(nextline_pb_src, 1, nextline_pb_ctx, off, 1),
            Some((1, nextline_pb_byte))
        );
    }

    // Positive footnote reference with unjoined inner flank word refines in both modes,
    // even when a neighboring line in the local window has a shorter-flank footnote or math clause.
    let fn_pos_src = "Alpha.\\footnote{Note see \\cref{sec:a} for details.}\\clearpage\n\
                      Beta.\\footnote{See \\cref{sec:a} for details.}\\clearpage\n";
    let fn_pos_ctx = "Alpha. 1 1Note see section 1 for details.";
    let fn_pos_byte = fn_pos_src.find("sec:a").unwrap();
    for needle in ["section", "1"] {
        let off = fn_pos_ctx.rfind(needle).unwrap();
        assert_eq!(
            source_word_location(fn_pos_src, 1, fn_pos_ctx, off, 1),
            Some((1, fn_pos_byte))
        );
        assert_eq!(
            source_word_guess_with_sources(
                fn_pos_src,
                1,
                fn_pos_ctx,
                off,
                1,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 1,
                byte: fn_pos_byte,
            }))
        );
    }
    let fn_body_math_src =
        "Other see $88$ for details.\\footnote{Note see \\cref{sec:a} for details.}\n";
    let fn_body_math_ctx = "Other see 88 for details.";
    let fn_body_math_off = fn_body_math_ctx.find("88").unwrap();
    let fn_body_math_byte = fn_body_math_src.find("88").unwrap();
    let fn_body_ref_byte = fn_body_math_src.find("sec:a").unwrap();
    assert_ne!(fn_body_math_byte, fn_body_ref_byte);
    assert_eq!(
        source_word_location(fn_body_math_src, 1, fn_body_math_ctx, fn_body_math_off, 2),
        Some((1, fn_body_math_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            fn_body_math_src,
            1,
            fn_body_math_ctx,
            fn_body_math_off,
            2,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 1,
            byte: fn_body_math_byte,
        }))
    );

    // Shorter-left main-body competitors (paragraph-start and \item-start citations/math) must count against footnote-head references.
    for (short_cite_src, short_cite_line) in [
        (
            "Alpha.\n\nSee \\cite{key} for details.\\footnote{Note see \\cref{sec:a} for details.}\n",
            3u32,
        ),
        (
            "\\begin{itemize}\n\\item See \\cite{key} for details.\\footnote{Note see \\cref{sec:a} for details.}\n\\end{itemize}\n",
            2u32,
        ),
    ] {
        let short_cite_ctx = "See [7] for details.";
        let short_cite_off = short_cite_ctx.find('7').unwrap();
        assert_eq!(
            source_word_location(
                short_cite_src,
                short_cite_line,
                short_cite_ctx,
                short_cite_off,
                3
            ),
            None
        );
        assert!(matches!(
            source_word_guess_with_sources(
                short_cite_src,
                short_cite_line,
                short_cite_ctx,
                short_cite_off,
                3,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(None) | Err(())
        ));
    }
    let short_math_src =
        "Alpha.\n\nSee $88$ for details.\\footnote{Note see \\cref{sec:a} for details.}\n";
    let short_math_ctx = "See 88 for details.";
    let short_math_off = short_math_ctx.find("88").unwrap();
    let short_math_byte = short_math_src.lines().nth(2).unwrap().find("88").unwrap();
    assert_eq!(
        source_word_location(short_math_src, 3, short_math_ctx, short_math_off, 3),
        Some((3, short_math_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            short_math_src,
            3,
            short_math_ctx,
            short_math_off,
            3,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 3,
            byte: short_math_byte,
        }))
    );
    let short_alpha_src =
        "Alpha.\n\nSee $\\alpha$ for details.\\footnote{Note see \\cref{sec:a} for details.}\n";
    let short_alpha_ctx = "See α for details.";
    let short_alpha_off = short_alpha_ctx.find('α').unwrap();
    let short_alpha_byte = short_alpha_src
        .lines()
        .nth(2)
        .unwrap()
        .find("\\alpha")
        .unwrap();
    assert_eq!(
        source_word_location(short_alpha_src, 3, short_alpha_ctx, short_alpha_off, 3),
        Some((3, short_alpha_byte))
    );
    assert_eq!(
        source_word_guess_with_sources(
            short_alpha_src,
            3,
            short_alpha_ctx,
            short_alpha_off,
            3,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 3,
            byte: short_alpha_byte,
        }))
    );

    // Same-line \clearpage in main_stream prevents an ending footnote reference from absorbing trailing page footer numbers,
    // while bare-EOF and next-source-line pagebreaks keep generated footnote reference tokens accurate.
    let fn_pb_src = "Alpha.\\footnote{One two three see \\cref{sec:a}.}\\clearpage\n";
    let fn_pb_footer_ctx = "Alpha. 1 1One two three see section 1. 5";
    let fn_pb_footer_off = fn_pb_footer_ctx.rfind('5').unwrap();
    assert_eq!(
        source_word_location(fn_pb_src, 1, fn_pb_footer_ctx, fn_pb_footer_off, 1),
        None
    );
    assert!(matches!(
        source_word_guess_with_sources(
            fn_pb_src,
            1,
            fn_pb_footer_ctx,
            fn_pb_footer_off,
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None) | Err(())
    ));
    for fn_eof_src in [
        "Alpha.\\footnote{One two three see \\cref{sec:a}.}\n",
        "Alpha.\\footnote{One two three see \\cref{sec:a}.}\n\\clearpage\n",
    ] {
        let fn_eof_ctx = "Alpha. 1 1One two three see section 1.";
        let fn_eof_byte = fn_eof_src.find("sec:a").unwrap();
        for needle in ["section", "1."] {
            let off = fn_eof_ctx.rfind(needle).unwrap();
            assert_eq!(
                source_word_location(fn_eof_src, 1, fn_eof_ctx, off, 2),
                Some((1, fn_eof_byte))
            );
            assert_eq!(
                source_word_guess_with_sources(
                    fn_eof_src,
                    1,
                    fn_eof_ctx,
                    off,
                    2,
                    &[],
                    &Operation::new(Duration::from_secs(10)),
                ),
                Ok(Some(UnverifiedWordLocation {
                    line: 1,
                    byte: fn_eof_byte,
                }))
            );
        }
    }

    let fn_glued_src = "Beta.\\footnote{See \\cref{sec:a} for details.}\\clearpage\n";
    let fn_glued_ctx = "Beta. 2 2See section 1 for details.";
    let beta_off = fn_glued_ctx.find("Beta").unwrap();
    assert_eq!(
        source_word_location(fn_glued_src, 1, fn_glued_ctx, beta_off, 1),
        Some((1, 0))
    );
    assert_eq!(
        source_word_guess_with_sources(
            fn_glued_src,
            1,
            fn_glued_ctx,
            beta_off,
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation { line: 1, byte: 0 }))
    );
    for needle in ["section", "1"] {
        let off = fn_glued_ctx.rfind(needle).unwrap();
        assert_eq!(
            source_word_location(fn_glued_src, 1, fn_glued_ctx, off, 1),
            None
        );
        assert!(matches!(
            source_word_guess_with_sources(
                fn_glued_src,
                1,
                fn_glued_ctx,
                off,
                1,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(None) | Err(())
        ));
    }

    // Printed optional arguments (\item[...], \begin{thm}[...]) and internal-boundary generated segments compete as slots.
    for opt_comp_src in [
        "By \\cref{sec:a} then.\n\\begin{itemize}\n\\item[By \\cite{key} then] Item.\n\\end{itemize}\n",
        "By \\cref{sec:a} then.\n\\begin{thm}[By \\cite{key} then]\nBody.\n\\end{thm}\n",
        "By \\cref{sec:a} then.\n\nBy \\cite{a} \\\\ \\cite{b} then.\n",
        "By \\cref{sec:a} then.\n\nBy \\cite{key} \\\\ more then.\n",
    ] {
        let opt_ctx = "By [7] then";
        let off = opt_ctx.find('7').unwrap();
        assert_eq!(source_word_location(opt_comp_src, 1, opt_ctx, off, 4), None);
        assert!(matches!(
            source_word_guess_with_sources(
                opt_comp_src,
                1,
                opt_ctx,
                off,
                4,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(None) | Err(())
        ));
    }

    // Global crefformat scanner consumes only its fixed 2 mandatory groups, not following braced prose.
    let crefformat_prose_src = "\\crefformat{section}{#2#1#3 zone}\n{\\itshape Printed} word.\n";
    let crefformat_prose_ctx = "Printed word.";
    let printed_byte = crefformat_prose_src.lines().nth(1).unwrap().find("Printed").unwrap();
    assert_eq!(
        source_word_location(crefformat_prose_src, 2, crefformat_prose_ctx, 0, 1),
        Some((2, printed_byte))
    );

    // Malformed, empty, comment-only, macro-inside-key, overridden, and ambiguous cases abstain.
    for bad in [
        "See \\cref{} done.\n",
        "See \\cref{ % only comment\n} done.\n",
        "See \\cref{\\macrokey} done.\n",
        "See \\cref{sec:a,,sec:b} done.\n",
        "\\newcommand{\\cref}[1]{section 1}\nSee \\cref{sec:alpha} done.\n",
        "See \\cref{sec:alpha} \\cref{sec:beta} done.\n",
    ] {
        let line = bad.lines().count() as u32;
        assert_eq!(
            source_word_location(bad, line, "See section 1 done.", 4, 2),
            None,
            "expected strict abstention for {bad:?}"
        );
    }
    let ambiguous_frame = "\\begin{frame}\nSee \\cref{sec:alpha} done.\nSee \\cref{sec:beta} done.\n\\end{frame}\n";
    assert_eq!(
        source_word_location(ambiguous_frame, 4, "See section 1 done.", 4, 4),
        None
    );
    assert_eq!(
        source_word_guess_with_sources(
            ambiguous_frame,
            4,
            "See section 1 done.",
            4,
            4,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Err(())
    );
    let tied_frame_with_literal =
        "\\begin{frame}\nSee \\cref{sec:alpha} done.\nSee \\cref{sec:beta} done.\nLiteral token here.\n\\end{frame}\n";
    let tied_frame_ctx = "See section 1 done. Literal token here.";
    assert_eq!(
        source_word_guess_with_sources(
            tied_frame_with_literal,
            5,
            tied_frame_ctx,
            tied_frame_ctx.find("Literal").unwrap(),
            4,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation { line: 4, byte: 0 }))
    );
}

#[test]
fn inverse_note_arguments_do_not_claim_slide_words() {
    for source in [
        "\\begin{frame}\nAmber\n\\note[item]<2>{Zephyr}\n\\end{frame}",
        "\\newcommand{\\cue}[2]{\\note{\\textbf{#1 min.} #2}}\n\
         \\begin{frame}\nAmber\n\\cue{1}{Zephyr}\n\\end{frame}",
        "\\newcommand{\\inner}[1]{\\note{#1}}\n\
         \\newcommand{\\outer}[2]{#1\\inner{#2}}\n\
         \\begin{frame}\n\\outer{Amber}{Zephyr}\n\\end{frame}",
        "\\newcommand{\\inner}[1]{\\note{#1#1}}\n\
         \\newcommand{\\outer}[2]{#1\\inner{#2}\\inner{#2}}\n\
         \\begin{frame}\n\\outer{Amber}{Zephyr}\n\\end{frame}",
    ] {
        let anchor = source.lines().count() as u32;
        assert_eq!(
            source_word_location(source, anchor, "Zephyr", 0, 10),
            None,
            "{source}"
        );
        let (row, byte) = source
            .lines()
            .enumerate()
            .find_map(|(row, text)| text.find("Amber").map(|byte| (row as u32 + 1, byte)))
            .unwrap();
        assert_eq!(
            source_word_location(source, anchor, "Amber", 0, 10),
            Some((row, byte)),
            "{source}"
        );
    }
    let source = "\\newcommand{\\both}[1]{#1\\note{#1}}\n\
                  \\begin{frame}\n\\both{Amber}\n\\end{frame}";
    assert_eq!(
        source_word_location(source, 4, "Amber", 0, 10),
        Some((3, "\\both{".len()))
    );
    for source in [
        "\\newcommand{\\dropone}[1]{}\n\
         \\newcommand{\\quiet}[1]{\\note{#1}}\n\
         \\newcommand{\\wrap}[1]{\\dropone\\quiet{#1}}\n\
         \\begin{frame}\n\\wrap{Amber}\\\\\nAmber\n\\end{frame}",
        "\\newcommand{\\wrap}[1]{\\def\\saved{#1}\\saved}\n\
         \\begin{frame}\n\\wrap{Amber}\\\\\nAmber\n\\end{frame}",
        "\\renewcommand{\\note}[1]{\\textbf{#1}}\n\
         \\begin{frame}\n\\note{Amber}\\\\\nAmber\n\\end{frame}",
    ] {
        assert_eq!(
            source_word_location(source, source.lines().count() as u32, "Amber", 0, 10),
            None,
            "{source}"
        );
    }
    let source = "\\begin{frame}{Visible context}\nAmber note text.\n\
                  \\note{Amber note text.}\n\\note{Amber note text.}\n\\end{frame}";
    let context = "Visible context Amber note text.";
    for word in ["Amber", "note", "text"] {
        assert_eq!(
            source_word_location(source, 5, context, context.rfind(word).unwrap(), 10),
            None,
            "{word}"
        );
    }
    let source = "\\newcommand{\\id}[1]{#1}\n$\\id x + y$\n";
    assert_eq!(
        source_word_location(source, 2, "x + y", 0, 4),
        Some((2, "$\\id ".len()))
    );
}

#[test]
fn inverse_opaque_note_keeps_compiler_anchor_instead_of_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("paper.tex");
    let pdf = directory.path().join("paper.pdf");
    for notes in [
        "",
        "\\drop{later}",
        "\\drop{\\drop{later}}",
        "\\drop{\\note{Amber note text.}}",
        "\\drop{\\note{\\drop{Amber note text.}}}",
        "\\dropwrap{Amber note text.}",
        "\\dropnote{Amber note text.}",
        "\\note{Amber note text.}",
        "\\note{\\drop{later} Amber note text.}",
        "\\note{Amber note text.}\n\\note{Amber note text.}",
        "\\note{Amber note text. Amber note text.}",
    ] {
        let text = format!(
            "\\documentclass{{beamer}}\n\\newcommand{{\\drop}}[1]{{}}\n\
             \\newcommand{{\\dropwrap}}[1]{{\\drop{{#1}}}}\n\
             \\newcommand{{\\dropnote}}[1]{{\\drop{{\\note{{#1}}}}}}\n\
             \\title{{Amber note text}}\n\\begin{{document}}\n\
             \\begin{{frame}}{{Other title}}\n{notes}\n\\end{{frame}}\n\\end{{document}}\n"
        );
        let anchor = text
            .lines()
            .position(|line| line == "\\end{frame}")
            .unwrap() as u32
            + 1;
        fs::write(&source, &text).unwrap();
        fs::write(
            pdf.with_extension("synctex"),
            format!(
                "SyncTeX Version:1\nInput:1:{}\n\
                 Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n\
                 {{1\n[1,{anchor}:0,0:100,100,0\n(1,{anchor}:0,0:100,100,0\n)\n]\n}}1\n\
                 Postamble:\nCount:4\n",
                source.display()
            ),
        )
        .unwrap();
        let resolved = inverse_output(
            &pdf,
            &format!("Input: paper.tex\nLine: {anchor}\n"),
            Some(("Amber note text.", 0)),
        )
        .unwrap();
        assert_eq!(
            (
                resolved.location.line,
                resolved.location.byte_column,
                resolved.location.precise
            ),
            (anchor, 0, false),
            "{notes}"
        );
    }
}

#[test]
fn inverse_quoted_paragraph_tokens_preserve_following_declarations() {
    for quote in [
        "\\string",
        "\\meaning",
        "\\noexpand",
        "\\csname string\\endcsname",
        "\\csname meaning\\endcsname",
        "\\csname noexpand\\endcsname",
    ] {
        for gap in [
            "\n\n",
            "\n \t\n",
            "\n%comment\n\n",
            "%comment\n\n",
            "%comment\n%comment\n\n",
        ] {
            for newline in ["\n", "\r\n"] {
                let source = format!(
                    "\\begin{{frame}}\nNimbus\n{quote}{gap}\
                     \\newcommand{{\\cue}}[1]{{\\note{{#1}}}}\n\
                     \\char90 ephyr \\cue{{Zephyr}} Copper\n\\end{{frame}}\n"
                )
                .replace('\n', newline);
                let line = source
                    .lines()
                    .position(|text| text.starts_with("\\char90"))
                    .unwrap() as u32
                    + 1;
                assert_eq!(
                    source_word_location(&source, line, "Zephyr Copper", 0, 10),
                    None,
                    "{source}",
                );
                assert_eq!(
                    source_word_location(&source, line, "Nimbus", 0, 10),
                    Some((2, 0)),
                    "{source}",
                );
            }
        }
    }
}

#[test]
fn inverse_control_tokens_keep_csname_name_whitespace() {
    for leading in ["", " \t", "\n", "\u{a0}"] {
        for trailing in [" ", "\n", "\r\n"] {
            let source = format!(
                "\\documentclass{{article}}\n\
                 \\expandafter\\def\\csname {leading}hbox{trailing}\\endcsname#1{{}}\n\
                 \\begin{{document}}\nAmber Zephyr \\hbox{{Zephyr}} Copper\n\
                 \\end{{document}}\n"
            );
            let line = source
                .lines()
                .position(|text| text.starts_with("Amber"))
                .unwrap() as u32
                + 1;
            let location = source_word_location(&source, line, "Amber Zephyr Zephyr Copper", 13, 2);
            assert!(
                location.is_none() || location == Some((line, 19)),
                "{source}: {location:?}",
            );
        }
    }
}

#[test]
fn inverse_reviewed_conditionals_do_not_execute_stored_definitions() {
    for definition in [
        "\\newcommand{\\ReviewStart}{\\iffalse}\n",
        "\\def\\ReviewStart{\\iffalse}\n",
    ] {
        let source = format!(
            "\\documentclass{{beamer}}\n\
             \\newcommand{{\\ReviewDiscard}}[1]{{}}\n{definition}\
             \\renewcommand{{\\ReviewDiscard}}[1]{{#1}}\n\
             \\newcommand{{\\ReviewEnd}}{{\\fi}}\n\
             \\begin{{document}}\n\\begin{{frame}}\n\
             \\ReviewDiscard{{Amber}}\n\\par\nNimbus\n\\par\nAmber\n\
             \\end{{frame}}\n\\end{{document}}\n"
        );
        let line = source
            .lines()
            .position(|text| text.starts_with("\\ReviewDiscard{Amber}"))
            .unwrap() as u32
            + 1;
        let byte = source
            .lines()
            .nth(line as usize - 1)
            .unwrap()
            .find("Amber")
            .unwrap();
        let location = source_word_location(&source, line, "Amber", 0, 10);
        assert!(
            location.is_none() || location == Some((line, byte)),
            "{source}: {location:?}",
        );
    }
}

#[test]
fn inverse_reviewed_conditionals_do_not_mask_resumed_host_tokens() {
    let host = Path::new("/synthetic/chapter.tex");
    let generated = Path::new("/synthetic/paper.bbl");
    for terminal in ["\\fi", "\\else"] {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "\\newif\\ifReviewHidden\n{{\n\\iffalse\n\
                 \\def\\ReviewUnused{{{terminal}\\ifReviewHidden}}\n\
                 \\input{{paper.bbl}}\n\\fi\n{closing}}}\n",
                closing = if terminal == "\\else" { "\\fi\n" } else { "" },
            )
            .replace('\n', newline);
            assert!(
                !crate::tex::host_owns_generated_sidecar(&source, host, generated, None),
                "{source}",
            );
        }
    }
}

#[test]
fn inverse_reviewed_conditionals_reject_newly_stored_host_inputs() {
    for source in [
        concat!(
            "{\n\\iffalse\n\\def\\ReviewUnused{\\fi\n",
            "\\def\\ReviewStored{\\input{paper.bbl}}\n}\n",
        ),
        concat!(
            "\\newif\\ifReviewNested\n\\iffalse\n",
            "\\label[\\fi\\let\\ReviewUnused]{\\iffalse}\n\\newif\\ifReviewNested\n",
            "\\fi\n\\input{paper.bbl}\n\\fi\n}\n",
        ),
    ] {
        assert!(
            !crate::tex::host_owns_generated_sidecar(
                source,
                Path::new("/synthetic/chapter.tex"),
                Path::new("/synthetic/paper.bbl"),
                None,
            ),
            "{source}"
        );
    }
}

#[test]
fn inverse_reviewed_conditionals_reject_deferred_caption_masks() {
    for source in [
        concat!(
            "\\newif\\ifReviewHidden\n",
            "{\\iffalse\\captionbox{\\fi}[\\ifReviewHidden]{Unused}\n",
            "\\input{paper.bbl}\\fi\n",
        ),
        concat!(
            "\\newif\\ifReviewHidden\n",
            "{\\iffalse\\captionbox{\\fi\\let\\ReviewEat}[\\ifReviewHidden]{Unused}\n",
            "\\input{paper.bbl}\\fi}\n",
        ),
    ] {
        assert!(
            !crate::tex::host_owns_generated_sidecar(
                source,
                Path::new("/synthetic/chapter.tex"),
                Path::new("/synthetic/paper.bbl"),
                None,
            ),
            "{source}",
        );
    }
}

#[test]
fn inverse_reviewed_conditionals_do_not_infer_control_sequence_meaning() {
    for preamble in [
        concat!(
            "\\newcommand{\\ifReviewWord}{}\n",
            "\\iffalse\n\\ifReviewWord\n\\fi\n",
            "\\renewcommand{\\ReviewPaint}[1]{#1}\n\\let\\ReviewEat\\fi\n",
        ),
        concat!(
            "\\let\\ReviewSavedFalse\\iffalse\n\\def\\iffalse{}\n\\iffalse\n",
            "\\renewcommand{\\ReviewPaint}[1]{#1}\n\\let\\ReviewEat\\fi\n",
            "\\let\\iffalse\\ReviewSavedFalse\n",
        ),
    ] {
        let source = format!(
            "\\documentclass{{article}}\n\\newcommand{{\\ReviewPaint}}[1]{{}}\n\
             {preamble}\\begin{{document}}\n\
             Amber Zephyr \\ReviewPaint{{Zephyr}} Copper\n\\end{{document}}\n",
        );
        let line = source
            .lines()
            .position(|row| row.starts_with("Amber"))
            .unwrap() as u32
            + 1;
        let byte = source
            .lines()
            .nth(line as usize - 1)
            .unwrap()
            .rfind("Zephyr")
            .unwrap();
        let location = source_word_location(&source, line, "Amber Zephyr Zephyr Copper", 13, 10);
        assert!(
            location.is_none() || location == Some((line, byte)),
            "{source}: {location:?}",
        );
    }
}

#[test]
fn inverse_raw_control_tokens_preserve_imported_note_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let main_path = directory.path().join("main.tex");
    fs::write(
        directory.path().join("notemacros.tex"),
        "\\newcommand{\\cue}[1]{\\note{#1}}\n",
    )
    .unwrap();
    for body in [
        "\\csname",
        "\\csname\\string",
        "\\csname\\meaning",
        "\\csname\\noexpand",
    ] {
        for suffix in ["", "\\newcommand{\\storedend}{\\endcsname}\n"] {
            let source = format!(
                "\\let\\ecs\\endcsname\n\\newcommand{{\\bcs}}{{{body}}}\n\
                 \\input{{notemacros}}\n\\begin{{frame}}\nNimbus\n\
                 \\char90 ephyr \\cue{{Zephyr}}\n\\end{{frame}}\n{suffix}"
            );
            fs::write(&main_path, &source).unwrap();
            let spliced = macro_preamble_source(
                &main_path,
                None,
                None,
                &mut QuerySources::default(),
                &Operation::default(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                crate::tex::source_word_location_with_sources(
                    &source,
                    7,
                    "Zephyr",
                    0,
                    10,
                    &[spliced.source.as_str()],
                    &Operation::default()
                )
                .map(WordOutcome::location),
                Err(()),
                "{source}",
            );
            assert_eq!(
                crate::tex::source_word_location_with_sources(
                    &source,
                    7,
                    "Nimbus",
                    0,
                    10,
                    &[spliced.source.as_str()],
                    &Operation::default()
                )
                .map(WordOutcome::location),
                Ok(Some((5, 0))),
                "{source}",
            );
        }
    }
}

#[test]
fn inverse_raw_control_tokens_preserve_unexpandable_primitives() {
    for definition in [
        "\\noexpand\\def\\cue#1{\\note{#1}}",
        "\\noexpand\\gdef\\cue#1{\\note{#1}}",
        "\\noexpand\\let\\cue\\note",
        "\\let\\cue\\note\\noexpand\\begingroup\\renewcommand{\\note}[1]{#1}\\noexpand\\endgroup",
        "\\let\\cue\\note\\noexpand\\bgroup\\renewcommand{\\note}[1]{#1}\\noexpand\\egroup",
    ] {
        let source =
            format!("{definition}\n\\begin{{frame}}\nNimbus\n\\cue{{Zephyr}}\n\\end{{frame}}");
        assert_eq!(
            crate::tex::source_word_location_with_sources(
                &source,
                5,
                "Zephyr",
                0,
                10,
                &[],
                &Operation::default()
            )
            .map(WordOutcome::location),
            Err(()),
            "{source}",
        );
        assert_eq!(
            source_word_location(&source, 5, "Nimbus", 0, 10),
            Some((3, 0)),
            "{source}",
        );
    }
}

#[test]
fn inverse_raw_control_tokens_in_unused_bodies_preserve_printing_scope() {
    let directory = tempfile::tempdir().unwrap();
    let main_path = directory.path().join("main.tex");
    fs::write(
        directory.path().join("printing.tex"),
        "\\renewcommand{\\note}[1]{#1}\n",
    )
    .unwrap();
    for declaration in [
        "\\newcommand{\\unused}{\\string}",
        "\\def\\unused{\\meaning}",
        "\\NewDocumentCommand{\\unused}{}{\\string}",
    ] {
        let source = format!(
            "{declaration}\n\\input{{printing}}\n\\begin{{frame}}\n\
             \\note{{Copper}}\n\\end{{frame}}\n"
        );
        fs::write(&main_path, &source).unwrap();
        let spliced = macro_preamble_source(
            &main_path,
            None,
            None,
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            crate::tex::source_word_location_with_sources(
                &source,
                5,
                "Copper",
                0,
                10,
                &[spliced.source.as_str()],
                &Operation::default()
            )
            .map(WordOutcome::location),
            Ok(Some((4, 6))),
            "{source}",
        );
    }
}

#[test]
fn inverse_opaque_math_vetoes_better_context_without_hiding_other_atoms() {
    let source = "\\begin{frame}\n$\\alpha+\\beta=\\gamma$\n\
                  \\note{$\\alpha+\\beta$}\n$\\delta+1$\n\\end{frame}";
    for offset in [0, "α+".len()] {
        assert_eq!(source_word_location(source, 5, "α+β=γ", offset, 10), None);
    }
    assert_eq!(source_word_location(source, 5, "δ+1", 0, 10), Some((4, 1)));
    let source = "\\renewcommand{\\note}[1]{#1}\n\
                  \\begin{frame}\n$\\alpha+\\beta=\\gamma$\n\
                  \\note{$\\alpha+\\beta$}\n\\end{frame}";
    assert_eq!(
        source_word_location(source, 5, "α+β=γ", 0, 10),
        Some((3, 1))
    );
}

#[test]
fn inverse_warned_guess_never_masks_body_or_unscoped_import_output() {
    use crate::tex::source_word_guess_with_sources;
    let package = "\\csname\\string x\\endcsname";
    for name in ["documentclass", "usepackage", "RequirePackage"] {
        for begin in ["\\begin{document}", "\\begin % comment\n{document}"] {
            let source = format!(
                "\\documentclass{{article}}\n\\usepackage{{algorithmicx}}\n{begin}\n\
                 \\{name}{{amber}}\nQuartz amber zephyr.\n\\end{{document}}"
            );
            let anchor = source.lines().count() as u32 - 2;
            assert_eq!(
                source_word_guess_with_sources(
                    &source,
                    anchor,
                    "amber",
                    0,
                    10,
                    &[package],
                    &Operation::default()
                ),
                Err(()),
                "{source}",
            );
        }
        for stored in ["", "\\newcommand{\\unused}{\\begin{document}}\n"] {
            let source = format!("{stored}\\{name}{{mystery}}\ncafé Amber");
            let anchor = source.lines().count() as u32;
            assert_eq!(
                source_word_guess_with_sources(
                    &source,
                    anchor,
                    "Amber",
                    0,
                    10,
                    &[package],
                    &Operation::default()
                ),
                Err(()),
                "{source}",
            );
        }
    }
    for (source, anchor) in [
        ("\\usepackage[\\begin{document}]{mystery}\ncafé Amber", 2),
        (
            "\\documentclass{article}\n{\\begin{document}}\n\\usepackage{Amber}\n\
             \\begin{document}\nQuartz Amber zephyr.\n\\end{document}",
            3,
        ),
        (
            "\\documentclass{article}\n\
             \\def\\pgfmathsetmacro#1#2{\\begin{document}}\n\
             \\pgfmathsetmacro{\\relax}{x}\n\\usepackage{Amber}\n\
             \\begin{document}\nQuartz Amber zephyr.\n\\end{document}",
            4,
        ),
        (
            "\\documentclass{article}\n\
             \\newenvironment{pgfmathsetmacro}{\\begin{document}}{}\n\
             \\pgfmathsetmacro{\\relax}{x}\n\\usepackage{Amber}\n\
             \\begin{document}\nQuartz Amber zephyr.\n\\end{document}",
            4,
        ),
        (
            "\\documentclass{article}\nUnexpected literal text.\n\\usepackage{Amber}\n\
             \\begin{document}\nQuartz Amber zephyr.\n\\end{document}",
            3,
        ),
    ] {
        assert_eq!(
            source_word_guess_with_sources(
                source,
                anchor,
                "Amber",
                0,
                4,
                &[package],
                &Operation::default()
            ),
            Err(()),
            "{source}",
        );
    }
}

#[test]
fn inverse_warned_guess_keeps_unresolved_local_import_targets_opaque() {
    use crate::tex::source_word_guess_with_sources;
    let package = "\\csname\\string x\\endcsname";
    for definition in [
        "\\expandafter\\def\\csname use\\pk\\endcsname#1{\\AtBeginDocument{Quartz }}",
        "\\expandafter\\let\\csname use\\pk\\endcsname\\relax",
    ] {
        let source = format!(
            "\\documentclass{{article}}\n\\usepackage{{algorithmicx}}\n\
             \\def\\pk{{package}}\n{definition}\n\\usepackage{{foo}}\n\
             \\begin{{document}}\nQuartz amber zephyr.\n\\end{{document}}"
        );
        assert_eq!(
            source_word_guess_with_sources(
                &source,
                7,
                "Quartz amber zephyr.",
                0,
                4,
                &[package],
                &Operation::default()
            ),
            Err(()),
            "{source}",
        );
    }
}

#[test]
fn inverse_warned_guess_preserves_literal_argument_owners() {
    use crate::tex::{UnverifiedWordLocation, source_word_guess_with_sources};
    let package = "\\csname\\string x\\endcsname";
    for definition in [
        "\\newcommand{\\styled}[1]{\\textbf{#1}}",
        "\\newcommand{\\styled}[1]{{\\large\\textbf{#1}.}}",
        "\\newcommand{\\w}[1]{#1}\\newcommand{\\styled}[1]{\\w{\\textit{#1}}}",
    ] {
        let source = format!(
            "{definition}\n\\begin{{frame}}\nQuartz \\styled{{Amber}} zephyr.\n\\end{{frame}}"
        );
        assert_eq!(
            source_word_guess_with_sources(
                &source,
                4,
                "Quartz Amber zephyr.",
                7,
                10,
                &[package],
                &Operation::default()
            ),
            Ok(Some(UnverifiedWordLocation { line: 3, byte: 15 })),
            "{source}",
        );
    }
}

#[test]
fn inverse_warned_guess_bounds_repeated_nonmutating_helper_graphs() {
    use crate::tex::{UnverifiedWordLocation, source_word_guess_with_sources};
    let mut source = "\\def\\unused{\\let}\n\\newcommand{\\marker}{\\downarrow}\n\
                      \\newcommand{\\helperA}{Silver}\n"
        .to_owned();
    for level in b'B'..=b'O' {
        let previous = format!("\\helper{} ", char::from(level - 1)).repeat(10);
        source.push_str(&format!(
            "\\newcommand{{\\helper{}}}{{{previous}}}\n",
            char::from(level)
        ));
    }
    source.push_str(
        "\\iffalse\\helperO\\fi\n\\begin{frame}\n$\\marker$\n\
                     Quartz Amber zephyr.\n\\end{frame}",
    );
    let closing = source.lines().count() as u32;
    assert_eq!(
        source_word_guess_with_sources(
            &source,
            closing,
            "Quartz Amber zephyr.",
            7,
            10,
            &["\\csname\\string x\\endcsname"],
            &Operation::default()
        ),
        Ok(Some(UnverifiedWordLocation {
            line: closing - 1,
            byte: 7
        })),
    );
}

#[test]
fn inverse_warned_guess_uses_context_for_repeated_prose_and_math() {
    use crate::tex::{UnverifiedWordLocation, source_word_guess_with_sources};
    let package = "\\csname\\string x\\endcsname";
    let source = "A repeated word far away.\n\
                  Émile uses \\emph{repeated} words near fields.\nRepeated noise.\n";
    let context = "Émile uses repeated words near ﬁelds.";
    assert_eq!(
        source_word_guess_with_sources(
            source,
            3,
            context,
            context.find("repeated").unwrap(),
            4,
            &[package],
            &Operation::default()
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 2,
            byte: "Émile uses \\emph{".len(),
        })),
    );
    let source = "\\begin{frame}\n$\\alpha+1$\n$\\alpha+2$\n\\end{frame}";
    for package in [
        package,
        "\\expandafter\\let\\csname external\\the\\count0\\endcsname\\relax",
    ] {
        assert_eq!(
            source_word_guess_with_sources(
                source,
                4,
                "α+1",
                0,
                10,
                &[package],
                &Operation::default()
            ),
            Ok(Some(UnverifiedWordLocation { line: 2, byte: 1 })),
        );
    }
}

#[test]
fn inverse_warned_guess_does_not_break_context_ties_by_compiler_distance() {
    use crate::tex::source_word_guess_with_sources;
    let package = "\\csname\\string x\\endcsname";
    for (source, line, context, offset) in [
        ("Amber\nAmber", 1, "Amber", 0),
        (
            "same Amber value.\nsame Amber value.",
            1,
            "same Amber value.",
            5,
        ),
        (
            "\\begin{frame}{Amber}\nAmber\n\\end{frame}",
            3,
            "Else Amber nearby",
            5,
        ),
        ("\\begin{frame}\n$x$\nx\n\\end{frame}", 4, "x", 0),
        (
            "\\begin{frame}\n$\\alpha$\n$\\alpha$\n\\end{frame}",
            2,
            "α",
            0,
        ),
    ] {
        assert!(
            matches!(
                source_word_guess_with_sources(
                    source,
                    line,
                    context,
                    offset,
                    10,
                    &[package],
                    &Operation::default()
                ),
                Ok(None) | Err(()),
            ),
            "{source}",
        );
    }
}

#[test]
fn inverse_warned_guess_preserves_comment_quote_and_nonprinting_masks() {
    use crate::tex::{UnverifiedWordLocation, source_word_guess_with_sources};
    let package = "\\csname\\string x\\endcsname";
    for hidden in ["% Amber", "\\label{Amber}", "\\newcommand{\\unused}{Amber}"] {
        let source = format!("\\begin{{frame}}\n{hidden}\ncafé Amber\n\\end{{frame}}");
        assert_eq!(
            source_word_guess_with_sources(
                &source,
                4,
                "Amber",
                0,
                10,
                &[package],
                &Operation::default()
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 3,
                byte: "café ".len()
            })),
            "{source}",
        );
    }
    for printed in [
        "\\detokenize{Amber}",
        "\\verb|Amber|",
        "\\lstinline|Amber|",
        "\\begin{verbatim}\nAmber\n\\end{verbatim}",
        "\\begin{Verbatim}\nAmber\n\\end{Verbatim}",
        "\\begin{lstlisting}\nAmber\n\\end{lstlisting}",
        "\\begin{minted}{tex}\nAmber\n\\end{minted}",
    ] {
        let source = format!(
            "\\newcommand{{\\quoted}}{{Amber}}\n\\begin{{frame}}\n{printed}\ncafé Amber\n\\end{{frame}}",
        );
        assert_eq!(
            source_word_guess_with_sources(
                &source,
                source.lines().count() as u32,
                "Amber",
                0,
                10,
                &[package],
                &Operation::default()
            ),
            Err(()),
            "{source}",
        );
    }
    let source = "\\title{Amber}\n\\begin{frame}\nCopper\n\\end{frame}";
    assert_eq!(
        source_word_guess_with_sources(
            source,
            4,
            "Amber",
            0,
            10,
            &[package],
            &Operation::default()
        ),
        Ok(None),
    );
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
        (title, title_offset),
        10.0,
        272.0,
        metadata_score,
        &Operation::default()
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
        (footer, footer_offset),
        228.0,
        272.0,
        footer_score,
        &Operation::default()
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
            &nonprinting_source_ranges(source),
            None,
            &Operation::default()
        )
        .unwrap()
        .2
    );
    assert!(!metadata_beats_frame(
        source,
        3,
        (context, offset),
        10.0,
        272.0,
        metadata_score,
        &Operation::default()
    ));
    assert!(metadata_beats_frame(
        source,
        3,
        (context, offset),
        228.0,
        272.0,
        metadata_score,
        &Operation::default()
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
        .expect_err("empty native results are not source ambiguity");
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
    .expect_err("competing valid anchors are not no-match");
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
    .expect_err("unqualified layout rows must remain competing coarse anchors");
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
                .expect_err("competing source locations must be ambiguous")
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
    .expect_err("a readable out-of-range source anchor must be rejected");
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
        payload.replace(
            "\"h\":72",
            r#""h":72,"source_snapshot":"/cache/manifest.json""#,
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
        ("Sample-document testing,", 9),
        386.5,
        792.0,
        6,
        &Operation::default()
    ));
    assert!(!allows_metadata_fallback(article, 3, 0, 386.5, 792.0));
    assert!(allows_metadata_fallback(article, 2, 1, 115.0, 792.0));
    assert_eq!(
        source_word_location(article, 3, "Keywords. Sample-document testing", 13, 1),
        Some((3, 10)),
    );
    let accented = "\\author{Jane Example\\thanks{Center of \\'Etudes\nMod\\^ele d'Éxemple}}";
    assert_eq!(
        source_word_location(accented, 1, "Center of Études Modêle d’Éxemple", 10, 1,),
        Some((1, 38)),
    );
    assert_eq!(
        source_word_location(accented, 2, "Center of Études Modêle d’Éxemple", 18, 1,),
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
        forward_candidates_from_reader(synctex.as_bytes(), pdf, section, None, 29).unwrap()[0].page,
        1
    );
    assert_eq!(
        forward_candidates_from_reader(synctex.as_bytes(), pdf, section, None, 44).unwrap()[0].page,
        2
    );
    assert_eq!(
        forward_candidates_from_reader(synctex.as_bytes(), pdf, section, None, 45).unwrap()[0].page,
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
                    source_line: 0,
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
            let tag = if next_u64(rng).is_multiple_of(4) {
                2
            } else {
                1
            };
            let line = base_line + (next_u64(rng) % 8) as i64;
            let x = ((next_u64(rng) % 400_000) as i64) + 10_000;
            let y = ((next_u64(rng) % 600_000) as i64) + 20_000;
            let w = ((next_u64(rng) % 150_000) as i64) + 1_000;
            let h = ((next_u64(rng) % 20_000) as i64) + 500;
            let d = (next_u64(rng) % 5_000) as i64;
            let y_str = if next_u64(rng).is_multiple_of(5) {
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
                    let kw = if next_u64(rng).is_multiple_of(3) {
                        -w
                    } else {
                        w
                    };
                    out.push_str(&format!("k{tag},{line}:{x},{y_str}:{kw}\n"));
                    if next_u64(rng).is_multiple_of(2) {
                        out.push_str(&format!("g{tag},{line}:{x},{y_str}\n"));
                    }
                }
                6 => out.push_str(&format!("g{tag},{line}:{x},{y_str}\n")),
                7 => out.push_str(&format!("r{tag},{line}:{x},{y_str}:{w},{h},{d}\n")),
                8 => out.push_str(&format!("${tag},{line}:{x},{y_str}\n")),
                9 if max_form_tag > 0
                    && (in_hbox || idx > 0)
                    && next_u64(rng).is_multiple_of(2) =>
                {
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
        if next_u64(&mut rng).is_multiple_of(2) {
            let x_unit = [
                "pt", "bp", "in", "cm", "mm", "pc", "sp", "dd", "cc", "nd", "nc",
            ][(next_u64(&mut rng) % 11) as usize];
            let y_unit = [
                "pt", "bp", "in", "cm", "mm", "pc", "sp", "dd", "cc", "nd", "nc",
            ][(next_u64(&mut rng) % 11) as usize];
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
                        let b =
                            b"()[]{}<>fxgkhvr$0123456789,-:=\n"[(next_u64(&mut rng) % 31) as usize];
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
        "SyncTeX Version:1\nInput:1:{}\nInput:2:{}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,10:0,0:30000000,40000000,0\n(2,42:655360,6553600:6553600,655360,0\nx2,42:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\n",
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
        x: 19.0,
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
    let inverse_global =
        resolve_inverse(&pdf, pt, None, 2, &Operation::default(), None, false).unwrap();
    set_build_directory(None);
    assert_eq!(Path::new(&inverse_global.location.file), tex.as_path());
    assert_eq!(inverse_global.location.line, 3);
    assert_eq!(inverse_global.location.column, 5);
}

#[test]
fn snapshot_preserves_fallback_source_identity_and_required_cache_validation() {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let client_dir = root.join("client");
    let build_dir = root.join("build");
    let cache_dir = root.join("cache");
    fs::create_dir_all(&client_dir).unwrap();
    fs::create_dir_all(&build_dir).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let pdf = build_dir.join("main.pdf");
    let tex = build_dir.join("main.tex");
    let source_text =
        "\\documentclass{article}\n\\begin{document}\nsnapshot fallback line\n\\end{document}\n";
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(&tex, source_text).unwrap();

    let synctex = "SyncTeX Version:1\nInput:1:./main.tex\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{1\n[1,3,10:0,0:30000000,40000000,0\n(1,3,10:655360,6553600:6553600,655360,0\nx1,3,10:1310720,6553600\n)\n]\n}1\nPostamble:\nCount:4\n";
    fs::write(build_dir.join("main.synctex"), synctex).unwrap();

    fs::write(cache_dir.join("1.txt"), source_text).unwrap();
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(source_text.as_bytes()).iter() {
        write!(digest, "{byte:02x}").unwrap();
    }
    let manifest_path = cache_dir.join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&serde_json::json!({
            "pdf": pdf,
            "revision": DocumentRevision::read(&pdf).unwrap(),
            "cwd": client_dir,
            "main": tex,
            "files": BTreeMap::from([(tex.clone(), "1.txt")]),
            "digests": BTreeMap::from([(tex.clone(), digest)]),
            "warnings": {},
        }))
        .unwrap(),
    )
    .unwrap();

    // Forward navigation with snapshot resolves ./main.tex to captured build/main.tex
    let forward = resolve_forward_with_selector(
        &pdf,
        &tex,
        3,
        1,
        ForwardSource {
            snapshot: Some(&manifest_path),
            ..ForwardSource::default()
        },
        |_, _, _, anchor| Ok(anchor),
    )
    .unwrap();
    assert_eq!(forward.page, 1);

    // Inverse navigation with snapshot returns captured build/main.tex (not client/main.tex)
    let pt = InversePoint {
        page: 1,
        x: 19.0,
        y_from_top: 98.0,
        page_height_pt: 792.0,
    };
    let inverse = resolve_inverse(
        &pdf,
        pt,
        Some(("snapshot fallback line", 9)),
        2,
        &Operation::default(),
        Some(&manifest_path),
        false,
    )
    .unwrap();
    assert_eq!(Path::new(&inverse.location.file), tex.as_path());
    assert_eq!(inverse.location.line, 3);
    assert!(inverse.location.precise);

    // Deleting the required cached source file must fail with a required-cache error
    fs::remove_file(cache_dir.join("1.txt")).unwrap();
    let err = resolve_inverse(
        &pdf,
        pt,
        None,
        2,
        &Operation::default(),
        Some(&manifest_path),
        false,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("required compiled source cache"),
        "unexpected error: {err}"
    );
}

#[test]
fn same_line_distinct_columns_and_unicode_column_units_are_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    // Line 3 starts with two 2-byte UTF-8 characters ('é' = 2 bytes each):
    // byte 0..2 = 'é' (char 1), byte 2..4 = 'é' (char 2), byte 4 = ' ', byte 5..10 = "alpha", byte 11..16 = "omega"
    fs::write(
        &tex,
        "\\documentclass{article}\n\\begin{document}\néé alpha omega\n\\end{document}\n",
    )
    .unwrap();

    // 1. Unicode coordinate units for compiler column 3 (byte offset 2, which is after 1 char 'é'):
    // byte_column must be 2, and UTF-16 `column` / scalar `column_char` must be 2 (not 3).
    let unicode_synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3,3:0,0:30000000,40000000,0\n(1,3,3:655360,6553600:6553600,655360,0\nx1,3,3:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\n",
        tex.display()
    );
    fs::write(pdf.with_extension("synctex"), &unicode_synctex).unwrap();

    let pt = InversePoint {
        page: 1,
        x: 19.0,
        y_from_top: 98.0,
        page_height_pt: 792.0,
    };
    let raw_unicode = inverse_candidates(&pdf, pt).unwrap();
    assert_eq!(raw_unicode.len(), 1);
    assert_eq!(
        (
            raw_unicode[0].byte_column,
            raw_unicode[0].column,
            raw_unicode[0].column_char
        ),
        (2, 2, 2)
    );

    let edit_unicode = parse_synctex_edit(&format!(
        "SyncTeX result begin\nInput:{}\nLine:3\nColumn:3\nSyncTeX result end\n",
        tex.display()
    ));
    assert_eq!(edit_unicode.len(), 1);
    assert_eq!(
        (
            edit_unicode[0].byte_column,
            edit_unicode[0].column,
            edit_unicode[0].column_char
        ),
        (2, 2, 2)
    );

    // 2. Two distinct compiler columns (12 and 7) on the same line straddling the click point:
    // Both raw APIs must retain both candidates, and strict resolve_inverse must report ambiguity.
    let two_col_synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:30000000,40000000,0\n(1,3:655360,6553600:6553600,655360,0\ng1,3,12:983040,6553600\ng1,3,7:1966080,6553600\n)\n]\n}}1\nPostamble:\nCount:5\n",
        tex.display()
    );
    fs::write(pdf.with_extension("synctex"), &two_col_synctex).unwrap();

    let raw_two = inverse_candidates(&pdf, pt).unwrap();
    assert_eq!(raw_two.len(), 2);
    assert_eq!(
        (
            raw_two[0].byte_column,
            raw_two[0].column,
            raw_two[0].column_char
        ),
        (11, 10, 10)
    );
    assert_eq!(
        (
            raw_two[1].byte_column,
            raw_two[1].column,
            raw_two[1].column_char
        ),
        (6, 5, 5)
    );

    let edit_two = parse_synctex_edit(&format!(
        "SyncTeX result begin\nInput:{}\nLine:3\nColumn:12\nInput:{}\nLine:3\nColumn:7\nSyncTeX result end\n",
        tex.display(),
        tex.display()
    ));
    assert_eq!(edit_two.len(), 2);
    assert_eq!(
        (
            edit_two[0].byte_column,
            edit_two[0].column,
            edit_two[0].column_char
        ),
        (11, 10, 10)
    );
    assert_eq!(
        (
            edit_two[1].byte_column,
            edit_two[1].column,
            edit_two[1].column_char
        ),
        (6, 5, 5)
    );

    let strict_err =
        resolve_inverse(&pdf, pt, None, 2, &Operation::default(), None, false).unwrap_err();
    assert!(
        strict_err
            .to_string()
            .contains("multiple coarse source anchors"),
        "expected ambiguity error, got: {strict_err}"
    );
    let guessed = resolve_inverse(&pdf, pt, None, 2, &Operation::default(), None, true).unwrap();
    assert_eq!(
        (
            guessed.location.byte_column,
            guessed.location.column,
            guessed.location.column_char
        ),
        (11, 10, 10)
    );
    assert!(
        guessed
            .warning
            .unwrap()
            .contains("multiple coarse source anchors")
    );
}

#[test]
fn snapshot_normalize_does_not_substitute_unrelated_absolute_file_in_cwd() {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let build_dir = root.join("build");
    let client_dir = root.join("client");
    let cache_dir = root.join("cache");
    fs::create_dir_all(&build_dir).unwrap();
    fs::create_dir_all(&client_dir).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let pdf = build_dir.join("doc.pdf");
    let build_tex = build_dir.join("main.tex");
    let client_tex = client_dir.join("main.tex");
    let source_text = "\\documentclass{article}\n\\begin{document}\nalpha beta\n\\end{document}\n";
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(&build_tex, source_text).unwrap();

    let synctex = "SyncTeX Version:1\nInput:1:./main.tex\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{1\n[1,3:0,0:30000000,40000000,0\n(1,3:655360,6553600:6553600,655360,0\nx1,3:1310720,6553600\n)\n]\n}1\nPostamble:\nCount:4\n";
    fs::write(pdf.with_extension("synctex"), synctex).unwrap();

    fs::write(cache_dir.join("1.txt"), source_text).unwrap();
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(source_text.as_bytes()).iter() {
        write!(digest, "{byte:02x}").unwrap();
    }
    let manifest_path = cache_dir.join("manifest.json");
    let manifest = serde_json::json!({
        "pdf": pdf,
        "revision": DocumentRevision::read(&pdf).unwrap(),
        "cwd": client_dir,
        "main": build_tex,
        "files": BTreeMap::from([(build_tex.clone(), "1.txt")]),
        "digests": BTreeMap::from([(build_tex.clone(), digest)]),
        "warnings": {},
    });
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    // Now create an unrelated file at client/main.tex and attempt forward search for it.
    fs::write(
        &client_tex,
        "\\documentclass{article}\n\\begin{document}\ngamma delta\n\\end{document}\n",
    )
    .unwrap();

    let strict_err = resolve_forward_with_selector(
        &pdf,
        &client_tex,
        3,
        1,
        ForwardSource {
            snapshot: Some(&manifest_path),
            ..ForwardSource::default()
        },
        |_, _, _, anchor| Ok(anchor),
    )
    .unwrap_err();
    assert!(
        strict_err.to_string().contains("not captured in snapshot"),
        "expected uncaptured-input error for unrelated client/main.tex, got: {strict_err}"
    );

    let guess_err = resolve_forward_with_selector(
        &pdf,
        &client_tex,
        3,
        1,
        ForwardSource {
            snapshot: Some(&manifest_path),
            best_guess: true,
            ..ForwardSource::default()
        },
        |_, _, _, anchor| Ok(anchor),
    )
    .unwrap_err();
    assert!(
        is_forward_unavailable(&guess_err),
        "expected forward unavailable for unrelated client/main.tex, got: {guess_err}"
    );
}

#[test]
fn malformed_postscript_offset_and_min_integer_geometry_do_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &tex,
        "\\documentclass{article}\n\\begin{document}\nhello\n\\end{document}\n",
    )
    .unwrap();

    // Item 4: multibyte UTF-8 and trailing garbage in postscript offset unit suffix (e.g. "1pα", "1ptα") must return Err,
    // while scientific notation ("0e0sp", "1e0sp", "0E0sp") is accepted.
    for bad in ["1pα", "1ptα", "1ptx", "0"] {
        let bad_offset = format!(
            "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:30000000,40000000,0\n]\n}}1\nPostamble:\nCount:2\nPost scriptum:\nX Offset:{bad}\n",
            tex.display()
        );
        fs::write(pdf.with_extension("synctex"), &bad_offset).unwrap();
        let pt = InversePoint {
            page: 1,
            x: 10.0,
            y_from_top: 10.0,
            page_height_pt: 792.0,
        };
        assert!(
            inverse_candidates(&pdf, pt).is_err(),
            "expected error for {bad}"
        );
        assert!(
            resolve_forward(&pdf, &tex, 3, 1).is_err(),
            "expected error for {bad}"
        );
    }
    for ok_dim in ["0sp", "0e0sp", "1e0sp", "0E0sp"] {
        let ok_offset = format!(
            "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:30000000,40000000,0\n(1,3:655360,6553600:6553600,655360,0\nx1,3:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\nPost scriptum:\nX Offset:{ok_dim}\nY Offset:0sp\n",
            tex.display()
        );
        fs::write(pdf.with_extension("synctex"), &ok_offset).unwrap();
        assert!(
            resolve_forward(&pdf, &tex, 3, 1).is_ok(),
            "expected ok for {ok_dim}"
        );
    }

    // Item 5: i64::MIN width in hbox geometry must be rejected with InvalidInput.
    let pt = InversePoint {
        page: 1,
        x: 10.0,
        y_from_top: 10.0,
        page_height_pt: 792.0,
    };
    let min_width = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:30000000,40000000,0\n(1,3:655360,6553600:{},655360,0\nx1,3:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\n",
        tex.display(),
        i64::MIN
    );
    fs::write(pdf.with_extension("synctex"), &min_width).unwrap();
    let min_err = inverse_candidates(&pdf, pt).unwrap_err();
    assert_eq!(min_err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn warm_synctex_cache_respects_snapshot_authority_and_spacing_diacritics() {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::os::unix::fs::PermissionsExt;

    // 1. Spacing diacritics (e.g. OT1 acute '\u{00b4}') stay attached to words and match TeX accents.
    let tokenized = words("alpha caf\u{00b4}e omega");
    assert_eq!(
        tokenized,
        vec![(0, "alpha"), (6, "caf\u{00b4}e"), (13, "omega")]
    );
    assert_eq!(normalized_word("caf\u{00b4}e"), "cafe");

    // 2. Warm SyncTeXCache must not reuse a non-snapshot structural map when a snapshot is supplied.
    let cwd = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let build = root.join("build");
    let storage = root.join("snapshot");
    for d in [&build, &storage] {
        fs::create_dir_all(d).unwrap();
    }
    fs::set_permissions(&storage, fs::Permissions::from_mode(0o700)).unwrap();

    let pdf = build.join("deck.pdf");
    let tex = build.join("main.tex");
    let compiled = "\\documentclass{beamer}\n\\begin{document}\n\\begin{frame}[fragile]\nSynthetic body words.\n\\end{frame}\n\\end{document}\n";
    fs::write(&pdf, b"pdf-v1").unwrap();
    let map = "SyncTeX Version:1\nInput:1:./main.tex\nInput:2:./deck.vrb\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{1\n[1,5:0,0:30000000,40000000,0\n(2,2:655360,6553600:6553600,655360,0\nx2,2:1310720,6553600\n)\n]\n}1\nPostamble:\nCount:4\n";
    fs::write(pdf.with_extension("synctex"), map).unwrap();

    let cached = storage.join("1.txt");
    fs::write(&cached, compiled).unwrap();
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(compiled.as_bytes()).iter() {
        write!(digest, "{byte:02x}").unwrap();
    }
    let manifest_path = storage.join("manifest.json");
    let manifest = serde_json::json!({
        "pdf": pdf,
        "revision": DocumentRevision::read(&pdf).unwrap(),
        "cwd": cwd,
        "main": tex,
        "files": BTreeMap::from([(tex.clone(), "1.txt")]),
        "digests": BTreeMap::from([(tex.clone(), digest)]),
        "warnings": {},
    });
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let snapshot = SourceSnapshot::load(&manifest_path, &pdf).unwrap();

    let pt = InversePoint {
        page: 1,
        x: 19.0,
        y_from_top: 98.0,
        page_height_pt: 792.0,
    };
    let mut cache = SyncTeXCache::default();
    let _ = resolve_inverse_with_cache(
        &pdf,
        pt,
        None,
        2,
        &Operation::default(),
        None,
        true,
        Some(&mut cache),
    )
    .unwrap();
    let warm = resolve_inverse_with_cache(
        &pdf,
        pt,
        None,
        2,
        &Operation::default(),
        Some(&snapshot),
        true,
        Some(&mut cache),
    )
    .unwrap();
    assert_eq!(Path::new(&warm.location.file), tex.as_path());
    assert_eq!(warm.location.line, 5);

    fs::remove_file(&cached).unwrap();
    let missing_err = resolve_inverse_with_cache(
        &pdf,
        pt,
        None,
        2,
        &Operation::default(),
        Some(&snapshot),
        true,
        Some(&mut cache),
    )
    .unwrap_err();
    assert!(
        missing_err
            .to_string()
            .contains("required compiled source cache"),
        "expected required cache error, got: {missing_err}"
    );
}

#[cfg(unix)]
#[test]
fn fifo_synctex_sidecar_is_rejected_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    fs::write(&pdf, b"pdf-v1").unwrap();
    let fifo = pdf.with_extension("synctex");
    let c_fifo = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);

    let pt = InversePoint {
        page: 1,
        x: 10.0,
        y_from_top: 10.0,
        page_height_pt: 792.0,
    };
    let err = inverse_candidates(&pdf, pt).unwrap_err();
    assert!(
        err.to_string().contains("regular file"),
        "expected regular-file error for FIFO sidecar, got: {err}"
    );
}

#[test]
fn macro_printed_text_does_not_jump_to_adjacent_literal_line_with_precise_true() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    let source = concat!(
        "\\documentclass{article}\n",
        "\\newcommand{\\Other}{Copper needle Nickel}\n",
        "\\begin{document}\n",
        "\\Other\\par\n",
        "Copper needle Nickel\\par\n",
        "\\end{document}\n",
    );
    fs::write(&tex, source).unwrap();

    // Anchor on line 4 (`\Other\par`) must not jump to literal `Copper needle Nickel` on line 5.
    assert_eq!(
        source_word_location(source, 4, "Copper needle Nickel", 7, 2),
        None
    );

    let synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,4:0,0:30000000,40000000,0\n(1,4:655360,6553600:6553600,655360,0\nx1,4:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\n",
        tex.display()
    );
    fs::write(pdf.with_extension("synctex"), synctex).unwrap();

    let pt = InversePoint {
        page: 1,
        x: 19.0,
        y_from_top: 98.0,
        page_height_pt: 792.0,
    };
    let resolved = resolve_inverse(
        &pdf,
        pt,
        Some(("Copper needle Nickel", 7)),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(resolved.location.line, 4);
    assert!(!resolved.location.precise);
    assert!(resolved.warning.is_none());
}

#[test]
fn parser_rejects_truncated_postamble_invalid_headers_unresolved_forms_and_bad_points() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &tex,
        "\\documentclass{article}\n\\begin{document}\nword\n\\end{document}\n",
    )
    .unwrap();

    let pt = InversePoint {
        page: 1,
        x: 10.0,
        y_from_top: 10.0,
        page_height_pt: 792.0,
    };

    // 1. Truncated postamble (SyncTeX Version:1 without Postamble:/Count:)
    let truncated = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:100,100,0\n]\n}}1\n",
        tex.display()
    );
    assert!(inverse_candidates_from_reader(truncated.as_bytes(), pt).is_err());

    // 2. Unclosed box / sheet
    let unclosed = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:100,100,0\nPostamble:\nCount:2\n",
        tex.display()
    );
    assert!(inverse_candidates_from_reader(unclosed.as_bytes(), pt).is_err());

    // 3. Non-positive or malformed Unit / Magnification
    for bad_header in [
        "Unit:0",
        "Unit:-1",
        "Unit:abc",
        "Magnification:0",
        "Magnification:-5",
    ] {
        let doc = format!(
            "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\n{bad_header}\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:100,100,0\n]\n}}1\nPostamble:\nCount:2\n",
            tex.display()
        );
        assert!(
            inverse_candidates_from_reader(doc.as_bytes(), pt).is_err(),
            "expected error for {bad_header}"
        );
    }

    // 4. Cyclic or empty form reference (`f`) must return Err, not silently drop
    let empty_form = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n<1\n>\n{{1\n[1,3:0,0:100,100,0\nf1:0,0\n]\n}}1\nPostamble:\nCount:4\n",
        tex.display()
    );
    assert!(inverse_candidates_from_reader(empty_form.as_bytes(), pt).is_err());

    let cyclic_form = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n<1\n(1,3:0,0:100,100,0\nf1:0,0\n)\n>\n{{1\n[1,3:0,0:100,100,0\nf1:0,0\n]\n}}1\nPostamble:\nCount:5\n",
        tex.display()
    );
    assert!(inverse_candidates_from_reader(cyclic_form.as_bytes(), pt).is_err());

    // 5. Invalid InversePoint rejected by raw inverse APIs
    let valid_doc = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:30000000,40000000,0\n(1,3:655360,6553600:6553600,655360,0\nx1,3:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\n",
        tex.display()
    );
    for bad_pt in [
        InversePoint {
            page: 0,
            x: 10.0,
            y_from_top: 10.0,
            page_height_pt: 792.0,
        },
        InversePoint {
            page: 1,
            x: f32::NAN,
            y_from_top: 10.0,
            page_height_pt: 792.0,
        },
        InversePoint {
            page: 1,
            x: 10.0,
            y_from_top: 10.0,
            page_height_pt: 0.0,
        },
    ] {
        let err = inverse_candidates_from_reader(valid_doc.as_bytes(), bad_pt).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    // 6. Public InverseFailure downcasting via inverse_failure()
    fs::write(pdf.with_extension("synctex"), &valid_doc).unwrap();
    let miss_err = resolve_inverse(
        &pdf,
        InversePoint {
            page: 9,
            x: 10.0,
            y_from_top: 10.0,
            page_height_pt: 792.0,
        },
        None,
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap_err();
    assert_eq!(inverse_failure(&miss_err), Some(&InverseFailure::NoMatch));

    // 7. `.pdf.synctex` companion discovery and PDF-adjacent precedence over build_directory
    fs::remove_file(pdf.with_extension("synctex")).unwrap();
    let pdf_synctex = root.join("doc.pdf.synctex");
    fs::write(&pdf_synctex, &valid_doc).unwrap();
    assert_eq!(locate_synctex(&pdf).as_deref(), Some(pdf_synctex.as_path()));
}

#[test]
fn protruding_child_box_expands_both_sides_of_parent_visible_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(&tex, "line1\nline2\n").unwrap();

    // Parent hbox (line 1) spans x=[655360, 1310720] (~10pt..20pt), y=[6553600-327680, 6553600] (~95pt..100pt).
    // Child hbox (line 1) protrudes on both left and right: x=[327680, 3276800] (~5pt..50pt)
    // and both top and bottom: y=[6553600-655360, 6553600+655360] (~90pt..110pt).
    // A second unrelated hbox (line 2) sits far away at x=13107200 (~200pt).
    let synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,1:0,0:30000000,40000000,0\n(1,1:655360,6553600:655360,327680,0\n(1,1:327680,6553600:2949120,655360,655360\nx1,1:327680,6553600\n)\n)\n(1,2:13107200,6553600:655360,327680,0\nx1,2:13107200,6553600\n)\n]\n}}1\nPostamble:\nCount:7\n",
        tex.display()
    );
    let hits = inverse_candidates_from_reader_with_options(
        synctex.as_bytes(),
        InversePoint {
            page: 1,
            x: 45.0,
            y_from_top: 108.0,
            page_height_pt: 792.0,
        },
        true,
    )
    .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].line, 1);
}

#[test]
fn inverse_frame_math_macros_and_color_wrappers_refine_inside_frame() {
    let source = concat!(
        "\\DeclareMathOperator{\\SampleOp}{SampleOp}\n",
        "\\newcommand{\\samplepair}[2]{\\langle #1, #2 \\rangle}\n",
        "\\newcommand{\\red}[1]{\\textcolor{red}{#1}}\n",
        "\\begin{frame}{Sample Title}\n",
        "The \\red{highlighted} color is red.\n",
        "$\\SampleOp(x) + \\samplepair{alpha}{beta}$\n",
        "\\end{frame}\n",
    );
    let closing_line = 7;

    let color_context = "The highlighted color is red.";
    let red_offset = color_context.rfind("red").unwrap();
    assert_eq!(
        source_word_location(source, closing_line, color_context, red_offset, 2),
        Some((5, "The \\red{highlighted} color is ".len()))
    );

    let math_context = "SampleOp(x) + \u{27e8}alpha, beta\u{27e9}";
    let op_offset = math_context.find("SampleOp").unwrap();
    assert_eq!(
        source_word_location(source, closing_line, math_context, op_offset, 2),
        Some((6, 1))
    );
    let arg_offset = math_context.find("alpha").unwrap();
    assert_eq!(
        source_word_location(source, closing_line, math_context, arg_offset, 2),
        Some((6, "$\\SampleOp(x) + \\samplepair{".len()))
    );

    let coloneqq_source = concat!(
        "\\begin{frame}{Sample Title}\n",
        "$a \\Coloneqq b$\n",
        "$a \\coloneqq b$\n",
        "\\end{frame}\n",
    );
    assert_eq!(
        source_word_location(coloneqq_source, 4, "a ::= b", 0, 2),
        Some((2, 1))
    );
    assert_eq!(
        source_word_location(coloneqq_source, 4, "a := b", 0, 2),
        Some((3, 1))
    );

    let nested_source = concat!(
        "\\newcommand{\\id}[1]{#1}\n",
        "\\newcommand{\\wrap}[1]{\\id{#1}}\n",
        "\\begin{frame}{Sample Title}\n",
        "$\\wrap{alpha} + beta$\n",
        "\\end{frame}\n",
    );
    assert_eq!(
        source_word_location(nested_source, 5, "alpha + beta", 0, 2),
        Some((4, "$\\wrap{".len()))
    );

    let gap_source = concat!(
        "\\begin{frame}{Sample Title}\n",
        "$A \\xrightarrow[sub]{sup} B + \\custom{alpha} + \\sum_{i=1}^n x_k$\n",
        "\\end{frame}\n",
    );
    let gap_context = "A sup sub \u{2192} B + alpha + \u{2211} n i=1 xk";
    for (word, needle) in [
        ("sup", "sup"),
        ("sub", "sub"),
        ("alpha", "alpha"),
        ("xk", "x_k"),
    ] {
        let offset = gap_context.find(word).unwrap();
        assert_eq!(
            source_word_location(gap_source, 3, gap_context, offset, 2),
            Some((2, gap_source.lines().nth(1).unwrap().find(needle).unwrap())),
            "expected {word:?} to resolve to {needle:?}"
        );
    }

    // Negative cases: unsafe or ambiguous macro/math forms must not guess.
    for (bad_source, bad_line, bad_context, bad_word) in [
        // \def overriding \newcommand in preamble must invalidate the macro.
        (
            concat!(
                "\\newcommand{\\foo}{alpha}\n",
                "\\def\\foo{beta}\n",
                "\\begin{frame}{Sample Title}\n",
                "$\\foo$\n",
                "\\end{frame}\n",
            ),
            5,
            "alpha",
            "alpha",
        ),
        // \let overriding \newcommand in preamble must invalidate the macro.
        (
            concat!(
                "\\newcommand{\\foo}{alpha}\n",
                "\\let\\foo\\relax\n",
                "\\begin{frame}{Sample Title}\n",
                "$\\foo$\n",
                "\\end{frame}\n",
            ),
            5,
            "alpha",
            "alpha",
        ),
        // Body-scoped \renewcommand must invalidate the macro.
        (
            concat!(
                "\\newcommand{\\foo}{alpha}\n",
                "\\begin{document}\n",
                "\\renewcommand{\\foo}{beta}\n",
                "\\begin{frame}{Sample Title}\n",
                "$\\foo$\n",
                "\\end{frame}\n",
                "\\end{document}\n",
            ),
            6,
            "alpha",
            "alpha",
        ),
        // \providecommand must not override unknown prior definition.
        (
            concat!(
                "\\providecommand{\\foo}{alpha}\n",
                "\\begin{frame}{Sample Title}\n",
                "$\\foo$\n",
                "\\end{frame}\n",
            ),
            4,
            "alpha",
            "alpha",
        ),
    ] {
        let offset = bad_context.find(bad_word).unwrap();
        assert_eq!(
            source_word_location(bad_source, bad_line, bad_context, offset, 2),
            None,
            "expected None for {bad_word:?} in {bad_context:?}"
        );
    }

    // Unmodeled document-defined macro (\def\hide#1{}) consumes its {...} arguments
    // into the '\0' gap so hidden tokens do not steal matches from later visible math.
    let hide_source = concat!(
        "\\def\\hide#1{}\n",
        "\\begin{document}\n",
        "$\\hide{x a} + x b$\n",
        "\\end{document}\n",
    );
    let hide_line = hide_source.lines().nth(2).unwrap();
    assert_eq!(
        source_word_location(hide_source, 3, "+ x b", 2, 2),
        Some((3, hide_line.rfind('x').unwrap()))
    );
}

#[test]
fn inverse_includegraphics_refines_to_graphics_path_and_abstains_safely() {
    // 1. Unit tests for source_graphics_location across single-line, multi-line, frame, and negative cases.
    let single = "\\documentclass{article}\n\\begin{document}\n\\includegraphics[width=0.5\\textwidth]{figs/sample.pdf}\n\\end{document}\n";
    assert_eq!(
        source_graphics_location(single, 3, 0, 1, false),
        Some((
            3,
            single
                .lines()
                .nth(2)
                .unwrap()
                .find("figs/sample.pdf")
                .unwrap()
        ))
    );

    let multiline = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "% \\includegraphics{figs/commented.pdf}\n",
        "\\newcommand{\\myfig}{\\includegraphics{figs/in_macro.pdf}}\n",
        "\\includegraphics[\n",
        "  width=0.8\\textwidth\n",
        "]{\n",
        "  % comment before path\n",
        "  figs/multiline.pdf\n",
        "}\n",
        "\\includesvg[width=2cm]{figs/vector.svg}\n",
        "\\end{document}\n",
    );
    for anchor_line in 5..=10 {
        assert_eq!(
            source_graphics_location(multiline, anchor_line, 0, 1, false),
            Some((9, 2)),
            "expected multiline includegraphics to resolve on anchor line {anchor_line}"
        );
    }
    assert_eq!(
        source_graphics_location(multiline, 11, 0, 1, false),
        Some((
            11,
            multiline
                .lines()
                .nth(10)
                .unwrap()
                .find("figs/vector.svg")
                .unwrap()
        ))
    );

    let frame_source = concat!(
        "\\begin{frame}{Sample Title}\n",
        "\\includegraphics[width=3cm]{figs/left.pdf}\n",
        "\\includegraphics[width=3cm]{figs/right.pdf}\n",
        "\\end{frame}\n",
    );
    assert_eq!(
        source_graphics_location(frame_source, 4, 0, 2, true),
        Some((
            2,
            frame_source
                .lines()
                .nth(1)
                .unwrap()
                .find("figs/left.pdf")
                .unwrap()
        ))
    );
    assert_eq!(
        source_graphics_location(frame_source, 4, 1, 2, true),
        Some((
            3,
            frame_source
                .lines()
                .nth(2)
                .unwrap()
                .find("figs/right.pdf")
                .unwrap()
        ))
    );
    // Without scope_proven, line 4 (\end{frame}) must not widen to the whole frame.
    assert_eq!(source_graphics_location(frame_source, 4, 0, 2, false), None);
    // Count mismatch (e.g. conditional/overlay macro) must abstain.
    assert_eq!(source_graphics_location(frame_source, 4, 0, 1, true), None);
    // Empty/whitespace-only graphics argument must abstain.
    let empty_arg = "\\includegraphics{  % nothing\n}\n";
    assert_eq!(source_graphics_location(empty_arg, 1, 0, 1, false), None);
    // Equal lexical and rendered counts under conditionals/duplicating macros must abstain.
    let conditional_dup = concat!(
        "\\newcommand{\\twice}[1]{#1#1}\n",
        "\\begin{document}\n",
        "\\iffalse\\includegraphics[width=30pt]{figs/hidden.pdf}\\fi\\twice{\\includegraphics[width=30pt]{figs/dup.pdf}}\n",
        "\\end{document}\n",
    );
    assert_eq!(
        source_graphics_location(conditional_dup, 3, 0, 2, false),
        None
    );

    // 2. End-to-end resolve_inverse test with two \includegraphics on line 3, prose on line 3,
    // and a non-graphics \scalebox{2}{ScaledText} on line 4.
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    let tex_source = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "ProseLabel \\includegraphics[width=20pt]{figs/first.pdf} \\includegraphics[width=20pt]{figs/second.pdf}\n",
        "\\scalebox{2}{ScaledText} \\includegraphics[width=20pt]{figs/third.pdf}\n",
        "\\end{document}\n",
    );
    fs::write(&tex, tex_source).unwrap();

    // Unit = 65536 (1pt = 1 unit in synctex coords when Unit=65536, Magnification=1000).
    // Line 3 hbox at y=100pt, height=30pt:
    // - Prose node at x=10..40
    // - First \includegraphics outer box at x=50..80, y=70..100 (h=30, d=0) with \Gscale@box inner pair
    // - Second \includegraphics outer box at x=100..130, y=70..100 (h=30, d=0) with \Gscale@box inner pair
    // Line 4 hbox at y=200pt, height=30pt:
    // - Non-graphics \scalebox{2}{ScaledText} at x=10..40 (only 1 wrapper above b_idx, plus g node inside d_idx)
    // - Third \includegraphics outer box at x=50..80, y=170..200
    let synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{tex}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:65536\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[1,5:0,0:400,792,0\n",
            "(1,3:10,100:130,30,0\n",
            "g1,3:10,100\n",
            "x1,3:40,100\n",
            "(1,3:50,100:30,30,0\n",
            "(1,3:50,100:30,30,0\n",
            "(1,3:50,100:0,30,0\n",
            "(1,3:50,100:0,15,0\n",
            "(1,3:50,100:15,15,0\n",
            "k1,3:65,100:0\n",
            ")\n",
            "k1,3:50,100:-15\n",
            ")\n",
            ")\n",
            ")\n",
            ")\n",
            "(1,3:100,100:30,30,0\n",
            "(1,3:100,100:30,30,0\n",
            "(1,3:100,100:0,30,0\n",
            "(1,3:100,100:0,15,0\n",
            "(1,3:100,100:15,15,0\n",
            "k1,3:115,100:0\n",
            ")\n",
            "k1,3:100,100:-15\n",
            ")\n",
            ")\n",
            ")\n",
            ")\n",
            ")\n",
            "(1,4:10,200:80,30,0\n",
            "(1,4:10,200:30,30,0\n",
            "(1,4:10,200:0,30,0\n",
            "(1,4:10,200:0,15,0\n",
            "(1,4:10,200:15,15,0\n",
            "x1,4:25,200\n",
            ")\n",
            "k1,4:10,200:-15\n",
            ")\n",
            ")\n",
            ")\n",
            "(1,4:50,200:30,30,0\n",
            "(1,4:50,200:30,30,0\n",
            "(1,4:50,200:0,30,0\n",
            "(1,4:50,200:0,15,0\n",
            "(1,4:50,200:15,15,0\n",
            "k1,4:65,200:0\n",
            ")\n",
            "k1,4:50,200:-15\n",
            ")\n",
            ")\n",
            ")\n",
            ")\n",
            ")\n",
            "]\n",
            "}}1\n",
            "Postamble:\n",
            "Count:37\n",
        ),
        tex = tex.display()
    );
    fs::write(pdf.with_extension("synctex"), &synctex).unwrap();

    // Clicking inside first figure (x=60, y=85) resolves to figs/first.pdf even when vector text is passed.
    let hit_first = resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 60.0,
            y_from_top: 85.0,
            page_height_pt: 792.0,
        },
        Some(("ProseLabel", 0)),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    let line3 = tex_source.lines().nth(2).unwrap();
    assert_eq!(hit_first.location.line, 3);
    assert_eq!(
        hit_first.location.byte_column,
        line3.find("figs/first.pdf").unwrap()
    );
    assert!(hit_first.location.precise);

    // Clicking inside second figure (x=115, y=85) resolves to figs/second.pdf.
    let hit_second = resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 115.0,
            y_from_top: 85.0,
            page_height_pt: 792.0,
        },
        None,
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(hit_second.location.line, 3);
    assert_eq!(
        hit_second.location.byte_column,
        line3.find("figs/second.pdf").unwrap()
    );
    assert!(hit_second.location.precise);

    // Clicking prose outside both figures (x=20, y=95) resolves to ProseLabel.
    let hit_prose = resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 20.0,
            y_from_top: 95.0,
            page_height_pt: 792.0,
        },
        Some(("ProseLabel", 0)),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(hit_prose.location.line, 3);
    assert_eq!(hit_prose.location.byte_column, 0);
    assert!(hit_prose.location.precise);

    // Clicking inside \scalebox{2}{ScaledText} (x=20, y=185) is NOT treated as graphics,
    // so it refines via word matching to "ScaledText" on line 4.
    let line4 = tex_source.lines().nth(3).unwrap();
    let hit_scalebox = resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 20.0,
            y_from_top: 185.0,
            page_height_pt: 792.0,
        },
        Some(("ScaledText", 0)),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(hit_scalebox.location.line, 4);
    assert_eq!(
        hit_scalebox.location.byte_column,
        line4.find("ScaledText").unwrap()
    );
    assert!(hit_scalebox.location.precise);

    fs::write(
        root.join("wobble.sty"),
        "\\def\\WobbleName{wobble@state}\n\\expandafter\\def\\csname\\WobbleName\\endcsname{}\n",
    )
    .unwrap();
    fs::write(
        pdf.with_extension("synctex"),
        synctex.replace(
            "Output:pdf",
            &format!("Input:2:{}\nOutput:pdf", root.join("wobble.sty").display()),
        ),
    )
    .unwrap();
    for command in ["includegraphics", "includesvg"] {
        let uncertain = tex_source
            .replace(
                "\\documentclass{article}",
                "\\documentclass{article}\\usepackage{wobble}",
            )
            .replace("includegraphics", command);
        fs::write(&tex, &uncertain).unwrap();
        for (x, filename, hint) in [
            (60., "figs/first.pdf", Some(("Blorf", 0))),
            (115., "figs/second.pdf", None),
        ] {
            let result = resolve_inverse(
                &pdf,
                InversePoint {
                    page: 1,
                    x,
                    y_from_top: 85.,
                    page_height_pt: 792.,
                },
                hint,
                2,
                &Operation::default(),
                None,
                true,
            )
            .unwrap();
            assert_eq!(
                (result.location.line, result.location.byte_column),
                (3, uncertain.lines().nth(2).unwrap().find(filename).unwrap()),
                "{command} {filename}"
            );
            assert!(result.location.precise);
            assert!(result.warning.is_some());
            for setup in [
                format!("\\renewcommand{{\\{command}}}[2][]{{}}"),
                format!("\\newcommand{{\\WobblePatch}}{{\\def\\{command}{{}}}}\\WobblePatch"),
            ] {
                fs::write(
                    &tex,
                    uncertain.replace("\\begin{document}", &format!("{setup}\\begin{{document}}")),
                )
                .unwrap();
                let veto = resolve_inverse(
                    &pdf,
                    InversePoint {
                        page: 1,
                        x,
                        y_from_top: 85.,
                        page_height_pt: 792.,
                    },
                    hint,
                    2,
                    &Operation::default(),
                    None,
                    true,
                )
                .unwrap();
                assert_eq!(
                    (veto.location.line, veto.location.byte_column),
                    (3, 0),
                    "{setup}"
                );
            }
            fs::write(&tex, &uncertain).unwrap();
        }
        fs::write(
            root.join("wobble.sty"),
            format!(
                "\\def\\WobbleName{{wobble@state}}\n\
                     \\expandafter\\def\\csname\\WobbleName\\endcsname{{}}\n\
                     \\newcommand{{\\WobbleBox}}[1]{{\\{command}{{blorf.pdf}}}}\n"
            ),
        )
        .unwrap();
        let wrapped = uncertain
            .replace("ProseLabel ", "\\WobbleBox{ProseLabel ")
            .replace("figs/second.pdf}\n", "figs/second.pdf}}\n");
        fs::write(&tex, wrapped).unwrap();
        let veto = resolve_inverse(
            &pdf,
            InversePoint {
                page: 1,
                x: 60.,
                y_from_top: 85.,
                page_height_pt: 792.,
            },
            None,
            2,
            &Operation::default(),
            None,
            true,
        )
        .unwrap();
        assert_eq!((veto.location.line, veto.location.byte_column), (3, 0));
        fs::write(
            root.join("wobble.sty"),
            "\\def\\WobbleName{wobble@state}\n\\expandafter\\def\\csname\\WobbleName\\endcsname{}\n",
        ).unwrap();
    }

    // 3. Real pdflatex integration test with \includegraphics[...]{example-image-a} and {example-image-b}.
    let real_dir = tempfile::tempdir().unwrap();
    let real_root = fs::canonicalize(real_dir.path()).unwrap();
    let real_pdf = real_root.join("real_fig.pdf");
    let real_tex = real_root.join("real_fig.tex");
    let real_source = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{graphicx}\n",
        "\\begin{document}\n",
        "\\noindent\n",
        "\\includegraphics[width=80pt]{example-image-a}\\hspace{20pt}\\includegraphics{example-image-b}\n",
        "\\end{document}\n",
    );
    fs::write(&real_tex, real_source).unwrap();
    if let Ok(output) = Command::new("pdflatex")
        .args([
            "-interaction=nonstopmode",
            "-halt-on-error",
            "-synctex=1",
            "real_fig.tex",
        ])
        .current_dir(&real_root)
        .output()
        && output.status.success()
    {
        let fwd = resolve_forward(&real_pdf, &real_tex, 5, 1).unwrap();
        let real_line5 = real_source.lines().nth(4).unwrap();
        let inv_a = resolve_inverse(
            &real_pdf,
            InversePoint {
                page: 1,
                x: fwd.h + 40.0,
                y_from_top: fwd.v - 30.0,
                page_height_pt: 792.0,
            },
            Some(("A", 0)),
            2,
            &Operation::default(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(inv_a.location.line, 5);
        assert_eq!(
            inv_a.location.byte_column,
            real_line5.find("example-image-a").unwrap()
        );
        assert!(inv_a.location.precise);

        let inv_b = resolve_inverse(
            &real_pdf,
            InversePoint {
                page: 1,
                x: fwd.h + 140.0,
                y_from_top: fwd.v - 30.0,
                page_height_pt: 792.0,
            },
            Some(("B", 0)),
            2,
            &Operation::default(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(inv_b.location.line, 5);
        assert_eq!(
            inv_b.location.byte_column,
            real_line5.find("example-image-b").unwrap()
        );
        assert!(inv_b.location.precise);
    }

    // 4. \includesvg .pdf_tex generated wrapper maps back to the host .tex argument.
    let svg_dir = tempfile::tempdir().unwrap();
    let svg_root = fs::canonicalize(svg_dir.path()).unwrap();
    let svg_pdf = svg_root.join("svg_fig.pdf");
    let svg_tex = svg_root.join("svg_fig.tex");
    let svg_sub = svg_root.join("svg-inkscape");
    fs::create_dir_all(&svg_sub).unwrap();
    let svg_pdf_tex = svg_sub.join("sample_svg-tex.pdf_tex");
    let svg_source = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{svg}\n",
        "\\begin{document}\n",
        "\\includesvg[width=40pt]{figures/diagram_svg}\n",
        "\\end{document}\n",
    );
    fs::write(&svg_tex, svg_source).unwrap();
    fs::write(
        &svg_pdf_tex,
        "\\begin{picture}(1,1)\\put(0,0){\\includegraphics[width=\\unitlength,page=1]{sample_svg-tex.pdf}}\\end{picture}\n",
    )
    .unwrap();
    fs::write(&svg_pdf, b"pdf-v1").unwrap();
    let svg_synctex = concat!(
        "SyncTeX Version:1\n",
        "Input:1:./svg_fig.tex\n",
        "Input:2:./svg-inkscape/sample_svg-tex.pdf_tex\n",
        "Output:pdf\n",
        "Magnification:1000\n",
        "Unit:65536\n",
        "X Offset:0\n",
        "Y Offset:0\n",
        "Content:\n",
        "{1\n",
        "[1,5:0,0:600,700,0\n",
        "(1,4:20,100:50,40,0\n",
        "(2,57:20,100:50,40,0\n",
        "(2,57:20,100:50,40,0\n",
        "g2,54:20,100\n",
        "(2,57:20,100:0,40,0\n",
        "(2,56:20,100:0,40,0\n",
        "(2,56:20,100:50,40,0\n",
        "(2,56:20,100:50,40,0\n",
        "(2,56:20,100:50,40,0\n",
        "(2,56:20,100:0,40,0\n",
        "(2,56:20,100:0,30,0\n",
        "(2,56:20,100:30,30,0\n",
        ")\n",
        "k2,56:20,100:-30\n",
        ")\n",
        ")\n",
        "g2,56:70,100\n",
        ")\n",
        ")\n",
        ")\n",
        "k2,56:20,100:-50\n",
        ")\n",
        ")\n",
        "k2,57:70,100:50\n",
        ")\n",
        ")\n",
        ")\n",
        "]\n",
        "}1\n",
        "Postamble:\n",
        "Count:32\n",
    );
    fs::write(svg_root.join("svg_fig.synctex"), svg_synctex).unwrap();

    let hit_svg = resolve_inverse(
        &svg_pdf,
        InversePoint {
            page: 1,
            x: 35.0,
            y_from_top: 80.0,
            page_height_pt: 792.0,
        },
        Some(("label inside svg", 0)),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    let svg_line4 = svg_source.lines().nth(3).unwrap();
    assert_eq!(hit_svg.location.file, svg_tex.to_string_lossy());
    assert_eq!(hit_svg.location.line, 4);
    assert_eq!(
        hit_svg.location.byte_column,
        svg_line4.find("figures/diagram_svg").unwrap()
    );
    assert!(hit_svg.location.precise);

    // Non-ASCII filenames must not panic in generated-wrapper suffix checks.
    assert!(!crate::parser::is_generated_graphics_tex("/tmp/ééé.tex"));
    assert!(crate::parser::is_generated_graphics_tex("/tmp/ééé.pdf_tex"));
    assert!(crate::parser::is_generated_graphics_tex(
        "/tmp/ééé.pdftex_t"
    ));
    assert!(crate::parser::is_generated_graphics_tex("/tmp/ééé.ps_tex"));
    assert!(crate::parser::is_generated_graphics_tex("/tmp/ééé.pstex_t"));

    // A paragraph line box containing prose alongside a raw .pdf_tex input without
    // a dedicated host wrapper must not turn surrounding prose into a graphics hit.
    let mixed_dir = tempfile::tempdir().unwrap();
    let mixed_root = fs::canonicalize(mixed_dir.path()).unwrap();
    let mixed_pdf = mixed_root.join("mixed.pdf");
    let mixed_tex = mixed_root.join("mixed.tex");
    let mixed_wrapper = mixed_root.join("diagram_svg-tex.pdf_tex");
    let mixed_source = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{svg}\n",
        "\\begin{document}\n",
        "ProseLabel \\includesvg{figures/diagram_svg}\n",
        "\\end{document}\n",
    );
    fs::write(&mixed_pdf, b"%PDF-1.4\n").unwrap();
    fs::write(&mixed_tex, mixed_source).unwrap();
    fs::write(&mixed_wrapper, "% generated by inkscape\n").unwrap();
    let mixed_synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{}\n",
            "Input:2:{}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[1,1:0,51904512:30000000,47362867,0\n",
            "(1,4:1966080,6553600:19660800,3276800,0\n",
            "x1,4:1966080,6553600\n",
            "(2,57:7864320,6553600:6553600,3276800,0\n",
            "(2,56:7864320,6553600:6553600,3276800,0\n",
            "(2,56:7864320,6553600:6553600,3276800,0\n",
            "(2,56:7864320,6553600:0,3276800,0\n",
            "(2,56:7864320,6553600:0,3276800,0\n",
            "(2,56:7864320,6553600:6553600,3276800,0\n",
            ")\n",
            "g2,56:7864320,6553600\n",
            ")\n",
            ")\n",
            ")\n",
            ")\n",
            "(2,57:9830400,6553600:0,3276800,0\n",
            "x2,57:9830400,6553600\n",
            ")\n",
            ")\n",
            ")\n",
            "]\n",
            "}}1\n",
            "Postamble:\n",
            "Count:30\n",
        ),
        mixed_tex.display(),
        mixed_wrapper.display(),
    );
    fs::write(mixed_root.join("mixed.synctex"), mixed_synctex).unwrap();
    let hit_prose = resolve_inverse(
        &mixed_pdf,
        InversePoint {
            page: 1,
            x: 35.0,
            y_from_top: 80.0,
            page_height_pt: 792.0,
        },
        Some(("ProseLabel", 0)),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(hit_prose.location.file, mixed_tex.to_string_lossy());
    assert_eq!(hit_prose.location.line, 4);
    assert_eq!(hit_prose.location.byte_column, 0);
    assert!(hit_prose.location.precise);

    let hit_mixed_svg = resolve_inverse(
        &mixed_pdf,
        InversePoint {
            page: 1,
            x: 150.0,
            y_from_top: 80.0,
            page_height_pt: 792.0,
        },
        Some(("label inside svg", 0)),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    let mixed_line4 = mixed_source.lines().nth(3).unwrap();
    assert_eq!(hit_mixed_svg.location.file, mixed_tex.to_string_lossy());
    assert_eq!(hit_mixed_svg.location.line, 4);
    assert_eq!(
        hit_mixed_svg.location.byte_column,
        mixed_line4.find("figures/diagram_svg").unwrap()
    );
    assert!(hit_mixed_svg.location.precise);
}

#[test]
fn math_kern_and_depth_and_prose_collision_guards() {
    // 1. Unbraced \mkern / \kern / \mskip must not swallow a following braced group.
    let source = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "$x\\mkern-3mu{y} + \\kern 2pt{z} + \\mskip 4mu plus 2mu minus 1mu{w}$\n",
        "\\end{document}\n",
    );
    let line3 = source.lines().nth(2).unwrap();
    assert_eq!(
        source_word_location(source, 3, "xy+z+w", 1, 0),
        Some((3, line3.find('y').unwrap()))
    );
    assert_eq!(
        source_word_location(source, 3, "xy+z+w", 3, 0),
        Some((3, line3.find('z').unwrap()))
    );
    assert_eq!(
        source_word_location(source, 3, "xy+z+w", 5, 0),
        Some((3, line3.find('w').unwrap()))
    );

    // 1b. Non-literal \kern\dimexpr must abstain rather than leak tokens and bias a duplicate formula.
    let dimexpr_source = concat!(
        "\\documentclass{beamer}\n",
        "\\begin{document}\n",
        "\\begin{frame}\n",
        "$x\\kern\\dimexpr1pt+2pt\\relax+y$\n",
        "$x+y$\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    assert_eq!(source_word_location(dimexpr_source, 4, "x+y", 2, 2), None);

    // 2. Deeply nested paired scripts terminate cleanly via the recursion depth cap.
    let mut nested = String::from("$a");
    for _ in 0..100 {
        nested.push_str("_1^{2");
    }
    for _ in 0..100 {
        nested.push('}');
    }
    nested.push('$');
    let deep_source =
        format!("\\documentclass{{article}}\n\\begin{{document}}\n{nested}\n\\end{{document}}\n");
    let _ = source_word_location(&deep_source, 3, "a12", 0, 0);

    // 2b. Deeply nested braces after \kern do not recurse in skip_tex_dimen.
    let nested_kern = format!(
        "\\documentclass{{article}}\n\\begin{{document}}\n$x\\kern{}1pt{}+y$\n\\end{{document}}\n",
        "{".repeat(8192),
        "}".repeat(8192)
    );
    assert_eq!(source_word_location(&nested_kern, 3, "x+y", 2, 0), None);

    // 3. Shared token between prose (score 0) and math must abstain rather than pick math.
    let collision_source = concat!(
        "\\documentclass{beamer}\n",
        "\\newcommand{\\samplex}{x}\n",
        "\\begin{document}\n",
        "\\begin{frame}\n",
        "+x\\\\\n",
        "$a+\\samplex$\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    assert_eq!(source_word_location(collision_source, 7, "+x", 1, 2), None);
}

#[test]
fn math_environments_and_captions_forward_and_inverse_sync() {
    // 1. Multi-line align / gather / multline / alignat / split / pmatrix / dcases inverse sync
    //    when SyncTeX reports the closing \end{...} or \] line.
    let math_source = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{amsmath,mathtools}\n",
        "\\begin{document}\n",
        "\\begin{align}\n",
        "  \\alpha_1 + \\beta_1 &= \\gamma_1 \\\\\n",
        "  \\delta_2 + \\epsilon_2 &= \\zeta_2\n",
        "\\end{align}\n",
        "\\begin{gather*}\n",
        "  \\eta_1 + \\theta_1 \\\\\n",
        "  \\iota_2 + \\kappa_2\n",
        "\\end{gather*}\n",
        "\\begin{multline}\n",
        "  \\lambda_1 + \\mu_1 \\\\\n",
        "  = \\nu_2 + \\xi_2\n",
        "\\end{multline}\n",
        "\\begin{equation}\n",
        "  \\begin{split}\n",
        "    \\pi_1 &= \\rho_1 \\\\\n",
        "    \\sigma_2 &= \\tau_2\n",
        "  \\end{split}\n",
        "\\end{equation}\n",
        "\\[\n",
        "  \\begin{aligned}[t]\n",
        "    \\upsilon_1 &= \\phi_1 \\\\\n",
        "    \\chi_2 &= \\psi_2\n",
        "  \\end{aligned}\n",
        "\\]\n",
        "\\begin{equation*}\n",
        "  M = \\begin{pmatrix*}[r]\n",
        "    a_{11} & a_{12} \\\\\n",
        "    a_{21} & a_{22}\n",
        "  \\end{pmatrix*} + \\begin{dcases}\n",
        "    b_1 & x > 0 \\\\\n",
        "    b_2 & x \\le 0\n",
        "  \\end{dcases}\n",
        "\\end{equation*}\n",
        "\\end{document}\n",
    );

    // align closing line is 7; line 5 has \alpha_1, line 6 has \zeta_2
    let align_ctx = "\u{03b1}1 + \u{03b2}1 = \u{03b3}1";
    assert_eq!(
        source_word_location(math_source, 7, align_ctx, 0, 0),
        Some((
            5,
            math_source
                .lines()
                .nth(4)
                .unwrap()
                .find("\\alpha_1")
                .unwrap()
        ))
    );
    let align_ctx2 = "\u{03b4}2 + \u{03b5}2 = \u{03b6}2";
    assert_eq!(
        source_word_location(
            math_source,
            7,
            align_ctx2,
            align_ctx2.find("\u{03b6}2").unwrap(),
            0
        ),
        Some((
            6,
            math_source
                .lines()
                .nth(5)
                .unwrap()
                .find("\\zeta_2")
                .unwrap()
        ))
    );

    // gather* closing line is 11; line 9 has \eta_1, line 10 has \kappa_2
    let gather_ctx = "\u{03b7}1 + \u{03b8}1";
    assert_eq!(
        source_word_location(math_source, 11, gather_ctx, 0, 0),
        Some((
            9,
            math_source.lines().nth(8).unwrap().find("\\eta_1").unwrap()
        ))
    );

    // multline closing line is 15; line 13 has \lambda_1, line 14 has \xi_2
    let multline_ctx = "= \u{03bd}2 + \u{03be}2";
    assert_eq!(
        source_word_location(
            math_source,
            15,
            multline_ctx,
            multline_ctx.find("\u{03be}2").unwrap(),
            0
        ),
        Some((
            14,
            math_source.lines().nth(13).unwrap().find("\\xi_2").unwrap()
        ))
    );

    // equation + split: SyncTeX reports line 20 (\end{split}) or 21 (\end{equation})
    let split_ctx = "\u{03c0}1 = \u{03c1}1";
    for closing in [20, 21] {
        assert_eq!(
            source_word_location(math_source, closing, split_ctx, 0, 0),
            Some((
                18,
                math_source.lines().nth(17).unwrap().find("\\pi_1").unwrap()
            ))
        );
    }

    // \[ + aligned[t]: SyncTeX reports line 26 (\end{aligned}) or 27 (\])
    let aligned_ctx = "\u{03c7}2 = \u{03c8}2";
    for closing in [26, 27] {
        assert_eq!(
            source_word_location(
                math_source,
                closing,
                aligned_ctx,
                aligned_ctx.find("\u{03c8}2").unwrap(),
                0
            ),
            Some((
                25,
                math_source
                    .lines()
                    .nth(24)
                    .unwrap()
                    .find("\\psi_2")
                    .unwrap()
            ))
        );
    }

    // equation* + pmatrix*[r] + dcases: SyncTeX reports line 32 (\end{pmatrix*}), 35 (\end{dcases}), or 36 (\end{equation*})
    let matrix_ctx = "a21 a22";
    for closing in [32, 36] {
        assert_eq!(
            source_word_location(
                math_source,
                closing,
                matrix_ctx,
                matrix_ctx.find("a22").unwrap(),
                0
            ),
            Some((
                31,
                math_source.lines().nth(30).unwrap().find("a_{22}").unwrap()
            ))
        );
    }
    let dcases_ctx = "b2 x \u{2264} 0";
    for closing in [35, 36] {
        assert_eq!(
            source_word_location(math_source, closing, dcases_ctx, 0, 0),
            Some((
                34,
                math_source.lines().nth(33).unwrap().find("b_2").unwrap()
            ))
        );
    }

    // 2. Caption and subcaption variants: prose refinement, nonprinting argument masking,
    //    and graphics refinement across multi-line \caption, \subcaption, \captionof,
    //    \subcaptionbox, \captionbox, and \subfloat.
    let caption_source = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\begin{figure}\n",
        "  \\caption[AlphaWord beta]{\n",
        "    AlphaWord gamma\n",
        "    delta $\\omega_1 + \\omega_2$\n",
        "  }\n",
        "  \\captionof{figure}[GammaWord delta]{\n",
        "    figure epsilon\n",
        "    GammaWord zeta\n",
        "  }\n",
        "  \\subcaptionbox[EtaWord theta]{\n",
        "    EtaWord iota\\label{sub:first}\n",
        "  }[0.45\\textwidth][c]{\n",
        "    \\includegraphics[width=2cm]{figs/sub_one.pdf}\n",
        "  }\n",
        "  \\subfloat[KappaWord lambda][\n",
        "    KappaWord mu\n",
        "  ]{\n",
        "    \\includegraphics[width=2cm]{figs/sub_two.pdf}\n",
        "  }\n",
        "\\end{figure}\n",
        "\\end{document}\n",
    );

    // \caption[AlphaWord beta]{...} closes on line 7:
    // "AlphaWord" in the visible caption must resolve to line 5, not the [short] argument on line 4.
    let cap_ctx = "Figure 1: AlphaWord gamma delta \u{03c9}1 + \u{03c9}2";
    assert_eq!(
        source_word_location(
            caption_source,
            7,
            cap_ctx,
            cap_ctx.find("AlphaWord").unwrap(),
            0
        ),
        Some((
            5,
            caption_source
                .lines()
                .nth(4)
                .unwrap()
                .find("AlphaWord")
                .unwrap()
        ))
    );
    assert_eq!(
        source_word_location(
            caption_source,
            7,
            cap_ctx,
            cap_ctx.find("\u{03c9}2").unwrap(),
            0
        ),
        Some((
            6,
            caption_source
                .lines()
                .nth(5)
                .unwrap()
                .find("\\omega_2")
                .unwrap()
        ))
    );

    // \captionof{figure}[GammaWord delta]{...} closes on line 11:
    // Both {figure} and [GammaWord delta] on line 8 are masked; "figure" resolves to line 9
    // and "GammaWord" resolves to line 10.
    let capof_ctx = "Figure 2: figure epsilon GammaWord zeta";
    assert_eq!(
        source_word_location(
            caption_source,
            11,
            capof_ctx,
            capof_ctx.find("figure").unwrap(),
            0
        ),
        Some((
            9,
            caption_source
                .lines()
                .nth(8)
                .unwrap()
                .find("figure")
                .unwrap()
        ))
    );
    assert_eq!(
        source_word_location(
            caption_source,
            11,
            capof_ctx,
            capof_ctx.find("GammaWord").unwrap(),
            0
        ),
        Some((
            10,
            caption_source
                .lines()
                .nth(9)
                .unwrap()
                .find("GammaWord")
                .unwrap()
        ))
    );

    // \subcaptionbox closes on line 16:
    // [EtaWord theta] on line 12 and [0.45\textwidth][c] on line 14 are masked;
    // "EtaWord" resolves to line 13, and graphics refinement on closing line 16 resolves to line 15.
    let subbox_ctx = "(a) EtaWord iota";
    assert_eq!(
        source_word_location(
            caption_source,
            16,
            subbox_ctx,
            subbox_ctx.find("EtaWord").unwrap(),
            0
        ),
        Some((
            13,
            caption_source
                .lines()
                .nth(12)
                .unwrap()
                .find("EtaWord")
                .unwrap()
        ))
    );
    assert_eq!(
        source_graphics_location(caption_source, 16, 0, 1, true),
        Some((
            15,
            caption_source
                .lines()
                .nth(14)
                .unwrap()
                .find("figs/sub_one.pdf")
                .unwrap()
        ))
    );

    // \subfloat[KappaWord lambda][KappaWord mu]{...} closes on line 21:
    // First bracket [KappaWord lambda] on line 17 is masked list-entry; second bracket on line 18 is visible heading;
    // graphics refinement on closing line 21 resolves to line 20.
    let subfloat_ctx = "(b) KappaWord mu";
    assert_eq!(
        source_word_location(
            caption_source,
            21,
            subfloat_ctx,
            subfloat_ctx.find("KappaWord").unwrap(),
            0
        ),
        Some((
            18,
            caption_source
                .lines()
                .nth(17)
                .unwrap()
                .find("KappaWord")
                .unwrap()
        ))
    );
    assert_eq!(
        source_graphics_location(caption_source, 21, 0, 1, true),
        Some((
            20,
            caption_source
                .lines()
                .nth(19)
                .unwrap()
                .find("figs/sub_two.pdf")
                .unwrap()
        ))
    );

    // 3. Forward sync fallback to closing line for multi-line align and \subcaptionbox,
    //    and preference for positive-height formula boxes over zero-height pre-display hboxes.
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    let fwd_tex = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{amsmath,subcaption}\n",
        "\\begin{document}\n",
        "\\begin{align}\n",
        "  a &= b \\\\\n",
        "  c &= d\n",
        "\\end{align}\n",
        "\\subcaptionbox{Heading\n",
        "  text}[0.4\\textwidth]{\n",
        "  Body\n",
        "}\n",
        "\\end{document}\n",
    );
    fs::write(&tex, fwd_tex).unwrap();
    // Line 7 (\end{align}) has a zero-height pre-display hbox at y=100pt (h=0, d=0)
    // followed by the real display formula hbox at y=130pt (h=25pt, d=5pt).
    // Line 11 (closing '}' of \subcaptionbox) has the subcaptionbox hbox at y=220pt (h=40pt, d=0pt).
    let synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{tex}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:65536\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[1,12:0,792:612,792,0\n",
            "(1,7:72,100:200,0,0\n",
            ")\n",
            "(1,7:120,130:180,25,5\n",
            "x1,7:150,125\n",
            ")\n",
            "(1,11:72,220:160,40,0\n",
            "x1,11:100,210\n",
            ")\n",
            "]\n",
            "}}1\n",
            "Postamble:\n",
            "Count:8\n",
        ),
        tex = tex.display()
    );
    fs::write(pdf.with_extension("synctex"), synctex).unwrap();

    // Forward sync from lines 4..=7 (inside align) falls back to closing line 7 and picks the positive-height box
    // over the zero-height pre-display hbox at y=100.
    let bp = 72.0 / 72.27;
    for line in [4, 5, 6, 7] {
        let hit = resolve_forward(&pdf, &tex, line, 1).unwrap();
        assert!(
            (hit.v - 135.0 * bp).abs() < 1e-2,
            "unexpected v {} for line {line}",
            hit.v
        );
        assert!((hit.height - 30.0 * bp).abs() < 1e-2);
    }

    // Forward sync from inside \subcaptionbox heading/body (lines 8..=10) falls back to closing line 11.
    for (line, col) in [(8, 16), (9, 3), (10, 3), (11, 1)] {
        let hit = resolve_forward(&pdf, &tex, line, col).unwrap();
        assert!((hit.v - 220.0 * bp).abs() < 1e-2);
        assert!((hit.height - 40.0 * bp).abs() < 1e-2);
    }

    // 4. Leading bracketed math in environments without optional args is preserved.
    let bracket_eq = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\begin{equation}\n",
        "  [x] + [x]\n",
        "\\end{equation}\n",
        "\\end{document}\n",
    );
    let bracket_row = bracket_eq.lines().nth(3).unwrap();
    assert_eq!(
        source_word_location(bracket_eq, 5, "[x] + [x]", 1, 0),
        Some((4, bracket_row.find('x').unwrap()))
    );
    assert_eq!(
        source_word_location(bracket_eq, 5, "[x] + [x]", 7, 0),
        Some((4, bracket_row.rfind('x').unwrap()))
    );

    // 5. Row-specific inverse anchors in eqnarray and aligned inside equation
    //    preserve row proximity instead of flattening to whole-display scope.
    let row_specific = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{amsmath}\n",
        "\\begin{document}\n",
        "\\begin{equation}\n",
        "  \\begin{aligned}\n",
        "    x &= y \\\\\n",
        "    z &= w \\\\\n",
        "    x &= y\n",
        "  \\end{aligned}\n",
        "\\end{equation}\n",
        "\\begin{eqnarray}\n",
        "  u &=& v \\\\\n",
        "  p &=& q \\\\\n",
        "  u &=& v\n",
        "\\end{eqnarray}\n",
        "\\end{document}\n",
    );
    assert_eq!(
        source_word_location(row_specific, 6, "x = y", 0, 2),
        Some((6, row_specific.lines().nth(5).unwrap().find('x').unwrap()))
    );
    assert_eq!(
        source_word_location(row_specific, 12, "u = v", 0, 2),
        Some((12, row_specific.lines().nth(11).unwrap().find('u').unwrap()))
    );

    // 6. Prose before display math on the same line does not jump into the display on forward search.
    let prose_display = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "Intro text \\[\n",
        "  a = b\n",
        "\\]\n",
        "\\end{document}\n",
    );
    assert_eq!(math::math_closing_line(prose_display, 3, 1), None);
    assert_eq!(math::math_closing_line(prose_display, 3, 12), Some(5));

    // 7. Transitive safe user macros inside a Beamer frame do not block graphics refinement,
    //    whereas a macro that transitively expands to \includegraphics still abstains.
    let frame_macro_gfx = concat!(
        "\\documentclass{beamer}\n",
        "\\newcommand{\\innersym}{\\alpha}\n",
        "\\newcommand{\\outersym}[1]{\\innersym_{#1}}\n",
        "\\begin{document}\n",
        "\\begin{frame}{Sample Title}\n",
        "  $\\outersym{a}$\n",
        "  \\includegraphics[height=.5\\textheight]{figs/sample_grid.png}\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    assert_eq!(
        source_graphics_location(frame_macro_gfx, 8, 0, 1, true),
        Some((
            7,
            frame_macro_gfx
                .lines()
                .nth(6)
                .unwrap()
                .find("figs/sample_grid.png")
                .unwrap()
        ))
    );
    let unsafe_transitive_gfx = concat!(
        "\\documentclass{beamer}\n",
        "\\newcommand{\\innerpic}{\\includegraphics{figs/hidden.png}}\n",
        "\\newcommand{\\outerpic}{\\innerpic}\n",
        "\\begin{document}\n",
        "\\begin{frame}{Sample Title}\n",
        "  \\outerpic\n",
        "  \\includegraphics{figs/visible.png}\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    assert_eq!(
        source_graphics_location(unsafe_transitive_gfx, 8, 0, 1, true),
        None
    );

    // 8. Display math immediately following inline or single-line display math
    //    (`$x$\begin{equation}`, `$x$$$...$$`, `\[z\]\[...\]`) is recognized.
    let inline_then_display = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "$x$\\begin{equation}\n",
        "  a = b\n",
        "\\end{equation}\n",
        "\\end{document}\n",
    );
    assert_eq!(math::math_lines(inline_then_display, 5), Some(2..5));
    assert_eq!(math::math_closing_line(inline_then_display, 3, 2), None);
    assert_eq!(math::math_closing_line(inline_then_display, 3, 4), Some(5));
    assert_eq!(math::math_closing_line(inline_then_display, 4, 3), Some(5));
    let dollar_adjacent = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "Prefix $x$$$\n",
        " a=b\n",
        "$$\n",
        "\\end{document}\n",
    );
    assert_eq!(math::math_closing_line(dollar_adjacent, 3, 11), Some(5));
    let two_displays_one_line = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\[z\\]\\[\n",
        " a=b\n",
        "\\]\n",
        "\\end{document}\n",
    );
    assert_eq!(math::math_closing_line(two_displays_one_line, 3, 1), None);
    assert_eq!(
        math::math_closing_line(two_displays_one_line, 3, 6),
        Some(5)
    );

    // 9. eqnarray opening line with row 1 content stays on the opening line, while
    //    nonprinting header tokens (\label, \nonumber, \notag, \relax) and split \begin
    //    remap the opening line to the following line.
    let eqnarray_row1_on_open = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\begin{eqnarray} a &=& b \\\\\n",
        "  c &=& d\n",
        "\\end{eqnarray}\n",
        "\\end{document}\n",
    );
    assert_eq!(math::math_closing_line(eqnarray_row1_on_open, 3, 18), None);
    let eqnarray_label_open = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "\\begin{eqnarray}\\nonumber\\label\n",
        "  {eq:one}\\relax\n",
        "  a &=& b\n",
        "\\end{eqnarray}\n",
        "\\end{document}\n",
    );
    assert_eq!(math::math_closing_line(eqnarray_label_open, 3, 1), Some(4));
    let eqnarray_split_begin = concat!(
        "\\documentclass{article}\n",
        "\\begin{document}\n",
        "Prefix \\begin\n",
        "{eqnarray} a &=& b \\\\\n",
        "  c &=& d\n",
        "\\end{eqnarray}\n",
        "\\end{document}\n",
    );
    assert_eq!(math::math_closing_line(eqnarray_split_begin, 3, 8), Some(4));

    // 10. Opaque @-named macros inside wrappers or on the line block graphics refinement.
    let at_macro_gfx = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{graphicx}\n",
        "\\makeatletter\n",
        "\\newcommand{\\my@figure}{\\includegraphics[width=30pt]{actual.pdf}}\n",
        "\\newcommand{\\my@discard}[1]{}\n",
        "\\newcommand{\\unused}{\\csname my@figure\\endcsname}\n",
        "\\newcommand{\\wrapper}{\\my@figure}\n",
        "\\begin{document}\n",
        "\\wrapper \\my@discard{\\includegraphics{decoy.pdf}}\n",
        "\\end{document}\n",
    );
    assert_eq!(source_graphics_location(at_macro_gfx, 9, 0, 1, false), None);

    // 11. Unsupported glue after `plus`/`minus` or malformed `#` in a script abstains
    //     when disambiguating duplicate math atoms.
    let unsupported_glue = "$x + \\mskip 5mu plus \\stretch{1} x$\n";
    assert_eq!(
        source_word_location(unsupported_glue, 1, "x + x", 4, 0),
        None
    );
    let malformed_script_hash = "$x_#é$\n";
    assert_eq!(
        source_word_location(malformed_script_hash, 1, "x", 0, 0),
        None
    );
    let malformed_simultaneous_script_hash = "$x_#é^y$\n";
    assert_eq!(
        source_word_location(malformed_simultaneous_script_hash, 1, "x", 0, 0),
        None
    );
}

#[test]
fn tikzpicture_scope_and_foreach_loop_bindings_refine_nodes() {
    let source = concat!(
        "\\begin{frame}{Sample Diagram}\n",
        "Decoy Alpha in frame prose.\n",
        "\\centering\n",
        "\\resizebox{0.8\\linewidth}{!}{%\n",
        "\\begin{tikzpicture}\n",
        "  \\node at (0,0) {Alpha};\n",
        "  \\foreach \\i in {1,...,4} {\n",
        "    \\node at (\\i,0) {$S_{\\i}$};\n",
        "  }\n",
        "  \\foreach \\x/\\tag in {1/FirstTag, 2/SecondTag}\n",
        "    \\node at (\\x,1) {\\tag};\n",
        "\\end{tikzpicture}%\n",
        "}\n",
        "\\end{frame}\n",
    );
    // SyncTeX anchor on \end{tikzpicture} (line 12) or wrapper \centering (line 3)
    // scopes to the tikzpicture (lines 5..12) before falling back to the whole frame,
    // avoiding the frame-level "Alpha" decoy on line 2.
    assert_eq!(
        source_word_location(source, 12, "Alpha", 0, 2),
        Some((6, "  \\node at (0,0) {".len()))
    );
    assert_eq!(
        source_word_location(source, 3, "Alpha", 0, 2),
        Some((6, "  \\node at (0,0) {".len()))
    );
    // \foreach \i in {1,...,4} binds \i = 3 to match rendered "S₃" on line 8.
    assert_eq!(
        source_word_location(source, 12, "S\u{2083}", 0, 2),
        Some((8, "    \\node at (\\i,0) {$".len()))
    );
    assert_eq!(
        source_word_location(source, 12, "S\u{2083}", "S".len(), 2),
        Some((8, "    \\node at (\\i,0) {$S_{".len()))
    );
    // Multi-variable \foreach \x/\tag with semicolon-terminated body binds \tag = SecondTag.
    assert_eq!(
        source_word_location(source, 12, "SecondTag", 0, 2),
        Some((11, "    \\node at (\\x,1) {".len()))
    );
}

#[test]
fn cross_file_preamble_macros_and_redefinition_invalidation() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("main.pdf");
    let main_tex = root.join("main.tex");
    let preamble_tex = root.join("preamble.tex");
    let sub_tex = root.join("section.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &preamble_tex,
        "\\newcommand{\\PreambleSym}{\\Omega}\n\\newcommand{\\ClashSym}{\\alpha}\n",
    )
    .unwrap();
    fs::write(
        &main_tex,
        concat!(
            "\\documentclass{article}\n",
            "\\input{preamble}\n",
            "\\newcommand{\\RootWrapper}[1]{\\textbf{#1}}\n",
            "\\begin{document}\n",
            "\\input{section}\n",
            "\\end{document}\n",
        ),
    )
    .unwrap();
    fs::write(
        &sub_tex,
        concat!(
            "\\renewcommand{\\ClashSym}{\\beta}\n",
            "We study $\\PreambleSym$ and \\RootWrapper{TargetWord} here.\n",
        ),
    )
    .unwrap();

    // Redefined macro across sources is conservatively invalidated.
    let table = crate::math::macros_from_sources(
        &[
            "\\newcommand{\\ClashSym}{\\alpha}\n",
            "\\renewcommand{\\ClashSym}{\\beta}\n",
        ],
        &Operation::default(),
    );
    assert_eq!(table.get("ClashSym"), Some(&None));

    let synctex = format!(
        "SyncTeX Version:1\nInput:1:{}\nInput:2:{}\nInput:3:{}\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,5:0,0:30000000,40000000,0\n(2,2:655360,6553600:6553600,655360,0\nx2,2:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\n",
        main_tex.display(),
        sub_tex.display(),
        preamble_tex.display(),
    );
    fs::write(pdf.with_extension("synctex"), &synctex).unwrap();

    let math_hit = resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 20.0,
            y_from_top: 100.0,
            page_height_pt: 792.0,
        },
        Some(("We study \u{03a9} and TargetWord here.", "We study ".len())),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(math_hit.location.line, 2);
    assert_eq!(math_hit.location.byte_column, "We study $".len());

    let prose_hit = resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 20.0,
            y_from_top: 100.0,
            page_height_pt: 792.0,
        },
        Some((
            "We study \u{03a9} and TargetWord here.",
            "We study \u{03a9} and ".len(),
        )),
        2,
        &Operation::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(prose_hit.location.line, 2);
    assert_eq!(
        prose_hit.location.byte_column,
        "We study $\\PreambleSym$ and \\RootWrapper{".len()
    );

    // Zero-argument fixed literal prose macros refine to their invocation backslash in both local and imported modes.
    let defs_src = "\\newcommand{\\QX}{QX}\n\\newcommand{\\BadParam}[1]{#1}\n\\newcommand{\\BadCtr}{\\arabic{page}}\n";
    let body_src = concat!(
        "\\section{ Target }\n",
        "𐐀 Café amber of teal lanterns drifts \\QX{} by that harbor.\n",
        "\n",
        "Marmalade filler row.\n",
        "Six \\texttt{\\QX{}+ZYW} kettle wanders sideways.\n",
    );
    let line2 = body_src.lines().nth(1).unwrap();
    let plain_qx_byte = line2.find("\\QX").unwrap();
    let plain_qx_utf16 = line2[..plain_qx_byte].encode_utf16().count() + 1;
    let plain_qx_char = line2[..plain_qx_byte].chars().count() + 1;
    assert!(plain_qx_byte > plain_qx_utf16 && plain_qx_utf16 > plain_qx_char);
    let line5 = body_src.lines().nth(4).unwrap();
    let wrapped_qx_byte = line5.find("\\QX").unwrap();

    let plain_ctx = "𐐀 Café amber of teal lanterns drifts QX by that harbor.";
    let plain_off = plain_ctx.find("QX").unwrap();
    let wrapped_ctx = "Six QX+ZYW kettle wanders sideways.";
    let wrapped_off = wrapped_ctx.find("QX").unwrap();

    for extra in [&[defs_src][..], &[][..]] {
        let full_body = if extra.is_empty() {
            format!("{defs_src}{body_src}")
        } else {
            body_src.to_string()
        };
        let line_shift = if extra.is_empty() { 3 } else { 0 };
        assert_eq!(
            source_word_location_with_sources(
                &full_body,
                2 + line_shift,
                plain_ctx,
                plain_off,
                1,
                extra,
                &Operation::new(Duration::from_secs(10)),
            )
            .map(WordOutcome::location),
            Ok(Some((2 + line_shift, plain_qx_byte)))
        );
        assert_eq!(
            source_word_guess_with_sources(
                &full_body,
                2 + line_shift,
                plain_ctx,
                plain_off,
                1,
                extra,
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 2 + line_shift,
                byte: plain_qx_byte,
            }))
        );
        assert_eq!(
            source_word_location_with_sources(
                &full_body,
                5 + line_shift,
                wrapped_ctx,
                wrapped_off,
                1,
                extra,
                &Operation::new(Duration::from_secs(10)),
            )
            .map(WordOutcome::location),
            Ok(Some((5 + line_shift, wrapped_qx_byte)))
        );
        assert_eq!(
            source_word_guess_with_sources(
                &full_body,
                5 + line_shift,
                wrapped_ctx,
                wrapped_off,
                1,
                extra,
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 5 + line_shift,
                byte: wrapped_qx_byte,
            }))
        );
    }

    // End-to-end resolve_inverse on imported body file keeps destination in sub_tex (never preamble_tex) with exact UTF-8/UTF-16/scalar columns.
    fs::write(&preamble_tex, defs_src).unwrap();
    fs::write(&sub_tex, body_src).unwrap();
    for best_guess in [false, true] {
        let qx_hit = resolve_inverse(
            &pdf,
            InversePoint {
                page: 1,
                x: 20.0,
                y_from_top: 100.0,
                page_height_pt: 792.0,
            },
            Some((plain_ctx, plain_off)),
            2,
            &Operation::default(),
            None,
            best_guess,
        )
        .unwrap();
        assert_eq!(qx_hit.location.file, sub_tex);
        assert_eq!(qx_hit.location.line, 2);
        assert_eq!(qx_hit.location.byte_column, plain_qx_byte);
        assert_eq!(qx_hit.location.column, plain_qx_utf16);
        assert_eq!(qx_hit.location.column_char, plain_qx_char);
        assert!(qx_hit.location.precise);
    }

    // Identical redefinition is explicitly permitted by macros_from_sources.
    let identical_redef_src = "\\renewcommand{\\QX}{QX}\nSee \\QX{} here.\n";
    assert_eq!(
        source_word_location_with_sources(
            identical_redef_src,
            2,
            "See QX here.",
            4,
            2,
            &[defs_src],
            &Operation::new(Duration::from_secs(10)),
        )
        .map(WordOutcome::location),
        Ok(Some((2, 4)))
    );

    // Equal-context macro/literal, macro/macro, conflicting, conditional, counter/param, hidden, and opaque cases stay conservative.
    for (bad_src, bad_extra, expect_err_in_guess) in [
        ("See \\QX{} here.\nSee QX here.\n", &[defs_src][..], true),
        ("See \\QX{} here.\nSee \\QX{} here.\n", &[defs_src][..], true),
        (
            "\\renewcommand{\\QX}{\\textbf{QX}}\nSee \\QX{} here.\n",
            &[defs_src][..],
            false,
        ),
        (
            "\\iftrue\\newcommand{\\QX}{QX}\\fi\nSee \\QX{} here.\n",
            &[][..],
            false,
        ),
        ("See \\BadCtr{} here.\n", &[defs_src][..], false),
        ("See \\phantom{\\QX{}} here.\n", &[defs_src][..], false),
        ("See \\phantom{QX} here.\n", &[defs_src][..], false),
        ("See \\note{\\QX{}} here.\n", &[defs_src][..], true),
    ] {
        let line = bad_src.lines().count() as u32;
        let strict_res = source_word_location_with_sources(
            bad_src,
            line,
            "See QX here.",
            4,
            2,
            bad_extra,
            &Operation::new(Duration::from_secs(10)),
        )
        .map(WordOutcome::location);
        assert!(
            matches!(strict_res, Ok(None) | Err(())),
            "strict should abstain on {bad_src:?}: {strict_res:?}"
        );
        let guess_res = source_word_guess_with_sources(
            bad_src,
            line,
            "See QX here.",
            4,
            2,
            bad_extra,
            &Operation::new(Duration::from_secs(10)),
        );
        if expect_err_in_guess {
            assert_eq!(guess_res, Err(()), "guess should error on {bad_src:?}");
        } else {
            assert!(
                matches!(guess_res, Ok(None) | Err(())),
                "guess should not refine on {bad_src:?}: {guess_res:?}"
            );
        }
    }
    // Generated theorem/caption headings must not steal a body zero-arg macro via distant context_bonus alone (score > 0 required),
    // while clicking the body macro occurrence with near-neighbor corroboration resolves to the macro invocation.
    for (heading_src, heading_ctx, macro_token) in [
        (
            "\\newcommand{\\Thm}{Theorem}\n\\begin{thm} By \\Thm{} we have the result.\\end{thm}\n",
            "Theorem 1. By Theorem we have the result.",
            "\\Thm{}",
        ),
        (
            "\\newcommand{\\Fig}{Figure}\n\\caption{By \\Fig{} we have the result.}\n",
            "Figure 1: By Figure we have the result.",
            "\\Fig{}",
        ),
    ] {
        let body_macro_byte = heading_src.lines().nth(1).unwrap().find(macro_token).unwrap();
        let body_pdf_off = heading_ctx.rfind("By ").unwrap() + "By ".len();
        assert_eq!(
            source_word_location_with_sources(
                heading_src,
                2,
                heading_ctx,
                0,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            )
            .map(WordOutcome::location),
            Ok(None)
        );
        assert_eq!(
            source_word_guess_with_sources(
                heading_src,
                2,
                heading_ctx,
                0,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Err(())
        );
        assert_eq!(
            source_word_location_with_sources(
                heading_src,
                2,
                heading_ctx,
                body_pdf_off,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            )
            .map(WordOutcome::location),
            Ok(Some((2, body_macro_byte)))
        );
        assert_eq!(
            source_word_guess_with_sources(
                heading_src,
                2,
                heading_ctx,
                body_pdf_off,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(Some(UnverifiedWordLocation {
                line: 2,
                byte: body_macro_byte,
            }))
        );
    }

    // Known nonprinting spacing/index arguments (\hspace, \hspace*, \vspace, \vspace*, \index) do not expose inner macros to citations,
    // while an explicit user redefinition (\renewcommand{\index}[1]{#1}) remains printable.
    for nonprinting_gap_src in [
        "\\newcommand{\\gap}{2}\nSee \\hspace{\\gap pt} \\cite{k} here.\n",
        "\\newcommand{\\gap}{2}\nSee \\hspace*{\\gap pt} \\cite{k} here.\n",
        "\\newcommand{\\gap}{2}\nSee \\vspace{\\gap pt} \\cite{k} here.\n",
        "\\newcommand{\\gap}{2}\nSee \\vspace*{\\gap pt} \\cite{k} here.\n",
        "\\newcommand{\\gap}{2}\nSee \\index{\\gap} \\cite{k} here.\n",
        "\\newcommand{\\gap}{2}\nSee \\hspace\\gap \\cite{k} here.\n",
    ] {
        let gap_ctx = "See [2] here.";
        let gap_off = gap_ctx.find('2').unwrap();
        assert_eq!(
            source_word_location_with_sources(
                nonprinting_gap_src,
                2,
                gap_ctx,
                gap_off,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            )
            .map(WordOutcome::location),
            Ok(None)
        );
        assert!(matches!(
            source_word_guess_with_sources(
                nonprinting_gap_src,
                2,
                gap_ctx,
                gap_off,
                2,
                &[],
                &Operation::new(Duration::from_secs(10)),
            ),
            Ok(None) | Err(())
        ));
    }
    let printable_override_src =
        "\\newcommand{\\gap}{2}\n\\renewcommand{\\index}[1]{#1}\nSee \\index{\\gap} here.\n";
    let printable_override_byte = printable_override_src
        .lines()
        .nth(2)
        .unwrap()
        .find("\\gap")
        .unwrap();
    assert_eq!(
        source_word_location_with_sources(
            printable_override_src,
            3,
            "See 2 here.",
            4,
            2,
            &[],
            &Operation::new(Duration::from_secs(10)),
        )
        .map(WordOutcome::location),
        Ok(Some((3, printable_override_byte)))
    );
    assert_eq!(
        source_word_guess_with_sources(
            printable_override_src,
            3,
            "See 2 here.",
            4,
            2,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: 3,
            byte: printable_override_byte,
        }))
    );

    // Nested zero-arg macro expansion works within the 64 KiB cap and abstains cleanly when breadth exceeds 64 KiB.
    let nested_ok_src = "\\newcommand{\\Leaf}{Target}\n\\newcommand{\\Mid}{\\Leaf{} \\Leaf{}}\n\\newcommand{\\Top}{\\Mid{} \\Mid{}}\nSee \\Top{} here.\n";
    let nested_ok_ctx = "See Target Target Target Target here.";
    assert_eq!(
        source_word_location_with_sources(
            nested_ok_src,
            4,
            nested_ok_ctx,
            4,
            2,
            &[],
            &Operation::new(Duration::from_secs(10)),
        )
        .map(WordOutcome::location),
        Ok(Some((4, 4)))
    );
    let blowup_src = "\\newcommand{\\Lzero}{Targetword Targetword Targetword Targetword}\n\
                      \\newcommand{\\Lone}{\\Lzero{} \\Lzero{} \\Lzero{} \\Lzero{}}\n\
                      \\newcommand{\\Ltwo}{\\Lone{} \\Lone{} \\Lone{} \\Lone{}}\n\
                      \\newcommand{\\Lthree}{\\Ltwo{} \\Ltwo{} \\Ltwo{} \\Ltwo{}}\n\
                      \\newcommand{\\Lfour}{\\Lthree{} \\Lthree{} \\Lthree{} \\Lthree{}}\n\
                      \\newcommand{\\Lfive}{\\Lfour{} \\Lfour{} \\Lfour{} \\Lfour{}}\n\
                      \\newcommand{\\Lsix}{\\Lfive{} \\Lfive{} \\Lfive{} \\Lfive{}}\n\
                      See \\Lsix{} here.\n";
    assert_eq!(
        source_word_location_with_sources(
            blowup_src,
            8,
            "See Targetword here.",
            4,
            2,
            &[],
            &Operation::new(Duration::from_secs(10)),
        )
        .map(WordOutcome::location),
        Ok(None)
    );
    assert!(matches!(
        source_word_guess_with_sources(
            blowup_src,
            8,
            "See Targetword here.",
            4,
            2,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None) | Err(())
    ));

    assert!(matches!(
        source_word_location_with_sources(
            "See \\QX{} here.\n",
            1,
            "QX",
            0,
            1,
            &[defs_src],
            &Operation::new(Duration::from_secs(10)),
        )
        .map(WordOutcome::location),
        Ok(None) | Err(())
    ));
    assert!(matches!(
        source_word_guess_with_sources(
            "See \\QX{} here.\n",
            1,
            "QX",
            0,
            1,
            &[defs_src],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(None) | Err(())
    ));
    let expired = Operation::new(Duration::ZERO);
    expired.cancel();
    assert_eq!(
        source_word_guess_with_sources(
            "See \\QX{} here.\n",
            1,
            "See QX here.",
            4,
            1,
            &[defs_src],
            &expired,
        ),
        Err(())
    );
}

// Each row earns its evidence from its own neighbors; short neighbors add no context bonus.
const HIGH_LIT: &str = "fen qua zibbet plo.";
const HIGH_MAC: &str = "fen qua \\Zib{} plo.";
const LOW_LIT: &str = "ash qua zibbet elm.";
const LOW_MAC: &str = "ash qua \\Zib{} elm.";
const TIE_DEFS: &str =
    "\\newcommand{\\Zib}{zibbet}\n\\newcommand{\\Rep}{qua zibbet plo qua zibbet plo}\n";

fn macro_literal_tie_strict(rows: &[&str], anchor: u32) -> Result<WordOutcome, ()> {
    source_word_location_with_sources(
        &rows.join("\n"),
        anchor,
        HIGH_LIT,
        HIGH_LIT.find("zibbet").unwrap(),
        4,
        &[TIE_DEFS],
        &Operation::new(Duration::from_secs(10)),
    )
}

fn macro_literal_tie_guess(
    rows: &[&str],
    anchor: u32,
) -> Result<Option<UnverifiedWordLocation>, ()> {
    source_word_guess_with_sources(
        &rows.join("\n"),
        anchor,
        HIGH_LIT,
        HIGH_LIT.find("zibbet").unwrap(),
        4,
        &[TIE_DEFS],
        &Operation::new(Duration::from_secs(10)),
    )
}

#[test]
fn macro_literal_tie_survives_later_literal_proximity() {
    for (rows, anchor) in [
        // A nearer literal replaces the retained one after the macro has already tied it.
        (&[HIGH_LIT, HIGH_MAC, HIGH_LIT][..], 3),
        (&[HIGH_LIT, HIGH_MAC, HIGH_LIT, HIGH_LIT][..], 4),
        // Discarding a lower-evidence tie does not hide a new tie at the stronger level.
        (&[LOW_LIT, LOW_MAC, HIGH_LIT, HIGH_MAC, HIGH_LIT][..], 5),
        // Reversed proximity and macro-first or macro-last orders stay equally coarse.
        (&[HIGH_LIT, HIGH_MAC, HIGH_LIT][..], 1),
        (&[HIGH_MAC, HIGH_LIT, HIGH_LIT][..], 3),
        (&[HIGH_LIT, HIGH_LIT, HIGH_MAC][..], 2),
    ] {
        assert_eq!(
            macro_literal_tie_strict(rows, anchor),
            Ok(WordOutcome::Tied),
            "{rows:?}"
        );
        assert_eq!(macro_literal_tie_guess(rows, anchor), Err(()), "{rows:?}");
    }
}

#[test]
fn macro_literal_tie_yields_to_strictly_stronger_evidence() {
    for (rows, anchor, row, needle) in [
        (&[HIGH_LIT, LOW_LIT, LOW_MAC, LOW_LIT][..], 4, 1, "zibbet"),
        (&[LOW_LIT, LOW_MAC, LOW_LIT, HIGH_LIT][..], 3, 4, "zibbet"),
        (&[HIGH_MAC, LOW_LIT, LOW_MAC][..], 3, 1, "\\Zib"),
        (&[LOW_LIT, LOW_MAC, HIGH_MAC][..], 1, 3, "\\Zib"),
    ] {
        let byte = rows[row as usize - 1].find(needle).unwrap();
        assert_eq!(
            macro_literal_tie_strict(rows, anchor),
            Ok(WordOutcome::Located(row, byte)),
            "{rows:?}"
        );
        assert_eq!(
            macro_literal_tie_guess(rows, anchor),
            Ok(Some(UnverifiedWordLocation { line: row, byte })),
            "{rows:?}"
        );
    }
    // The lower tie is forgotten, so literal proximity decides again at the stronger level.
    assert_eq!(
        macro_literal_tie_strict(&[LOW_LIT, LOW_MAC, HIGH_LIT, HIGH_LIT], 4),
        Ok(WordOutcome::Located(4, HIGH_LIT.find("zibbet").unwrap()))
    );
}

#[test]
fn macro_literal_tie_control_keeps_literal_proximity_and_repeated_tokens() {
    let byte = HIGH_LIT.find("zibbet").unwrap();
    for anchor in 1..=3 {
        assert_eq!(
            macro_literal_tie_strict(&[HIGH_LIT; 3], anchor),
            Ok(WordOutcome::Located(anchor, byte))
        );
    }
    // Equidistant literals have no proximity winner.
    assert_eq!(
        macro_literal_tie_strict(&[HIGH_LIT, "wig oak.", HIGH_LIT], 2),
        Ok(WordOutcome::Tied)
    );
    // Repeated words in one expansion share the invocation's location, so they do not tie.
    let context = "qua zibbet plo";
    let offset = context.find("zibbet").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            "\\Rep{}\n",
            1,
            context,
            offset,
            1,
            &[TIE_DEFS],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(WordOutcome::Located(1, 0))
    );
    assert_eq!(
        source_word_guess_with_sources(
            "\\Rep{}\n",
            1,
            context,
            offset,
            1,
            &[TIE_DEFS],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(Some(UnverifiedWordLocation { line: 1, byte: 0 }))
    );
}

fn macro_literal_tie_document(body: &str) -> String {
    format!(
        "\\documentclass{{article}}\n\\newcommand{{\\Zib}}{{zibbet}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n"
    )
}

// The handwritten sidecar matches the minimized pdflatex witness and clicks outside metadata bands.
fn macro_literal_tie_public(
    source: &str,
    anchor: u32,
    context: &str,
    offset: usize,
    radius: u32,
    best_guess: bool,
) -> SourceLocation {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(&tex, source).unwrap();
    fs::write(
        pdf.with_extension("synctex"),
        format!(
            concat!(
                "SyncTeX Version:1\nInput:1:{tex}\nOutput:pdf\n",
                "Magnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n",
                "{{1\n[1,2:0,0:400,792,0\n(1,{anchor}:10,300:300,10,0\n",
                "g1,{anchor}:10,300\nx1,{anchor}:310,300\n)\n]\n}}1\n",
                "Postamble:\nCount:10\n",
            ),
            tex = tex.display(),
            anchor = anchor,
        ),
    )
    .unwrap();
    resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 50.0,
            y_from_top: 295.0,
            page_height_pt: 792.0,
        },
        Some((context, offset)),
        radius,
        &Operation::default(),
        None,
        best_guess,
    )
    .unwrap()
    .location
}

#[test]
fn macro_literal_tie_public_math_and_literal_fallback_stay_coarse() {
    for (body, anchor, context, radius) in [
        // Proximity alone must not settle a tie, including when both occurrences share a row.
        (
            "qua zibbet\n\nqua \\Zib{}\n\nqua zibbet",
            8,
            "qua zibbet",
            4,
        ),
        ("qua zibbet\n\nqua \\Zib{} qua zibbet", 6, "qua zibbet", 2),
        // A tie must not move to a weaker atom on the clicked row.
        ("qua zibbet\n\nqua \\Zib{} $zibbet$", 6, "qua zibbet", 2),
        (
            "qua zibbet plo.\n\nqua \\Zib{} plo.\n\nqua zibbet plo. $zibbet$",
            8,
            "qua zibbet plo.",
            4,
        ),
        (
            "qua zibbet\n\nwig\n\nqua \\Zib{} $zibbet$\n\nqua zibbet",
            8,
            "qua zibbet",
            4,
        ),
    ] {
        let source = macro_literal_tie_document(body);
        for best_guess in [false, true] {
            let location =
                macro_literal_tie_public(&source, anchor, context, 4, radius, best_guess);
            assert!(!location.precise, "{body:?} {best_guess}: {location:?}");
            assert_eq!(location.line, anchor, "{body:?} {best_guess}");
            assert_eq!(location.byte_column, 0, "{body:?} {best_guess}");
        }
    }
    // Equidistant literals are no more evidence for an atom than one unique literal.
    let source = "\\documentclass{article}\n\\begin{document}\nqua zibbet\n\n$zibbet$\n\nqua zibbet\n\\end{document}\n";
    for best_guess in [false, true] {
        let location = macro_literal_tie_public(source, 5, "qua zibbet", 4, 2, best_guess);
        assert!(!location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 5, "{best_guess}");
    }
}

#[test]
fn macro_literal_tie_public_unsupported_macro_leaves_equal_literal_coarse() {
    let document = |row: &str| {
        format!(
            "\\documentclass{{article}}\n\\newcommand{{\\QX}}{{QX}}\n\\begin{{document}}\n{row}\n\\end{{document}}\n"
        )
    };
    let source = document("\\QX{} QX");
    for best_guess in [false, true] {
        let location = macro_literal_tie_public(&source, 4, "QX", 0, 1, best_guess);
        assert!(!location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 4, "{best_guess}");
    }
    // Corroboration still separates the literal, and an unused macro is no competitor.
    let row = "\\QX{} qua QX plo";
    let location =
        macro_literal_tie_public(&document(row), 4, "qua QX plo", "qua ".len(), 1, false);
    assert!(location.precise, "{location:?}");
    assert_eq!(location.byte_column, row.rfind("QX").unwrap());
    let location = macro_literal_tie_public(&document("wig QX"), 4, "QX", 0, 1, false);
    assert!(location.precise, "{location:?}");
    assert_eq!(location.byte_column, "wig ".len());
    // A lone unsupported macro still cannot claim its invocation.
    assert_eq!(
        source_word_location_with_sources(
            "See \\Zib{} here.\n",
            1,
            "zibbet",
            0,
            1,
            &[TIE_DEFS],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(WordOutcome::Tied)
    );
    assert_eq!(
        source_word_guess_with_sources(
            "See \\Zib{} here.\n",
            1,
            "zibbet",
            0,
            1,
            &[TIE_DEFS],
            &Operation::new(Duration::from_secs(10)),
        ),
        Err(())
    );
}

#[test]
fn macro_literal_tie_public_stronger_evidence_keeps_precision() {
    for (body, anchor, context, byte) in [
        // Stronger literal corroboration still beats a weaker macro and an uncorroborated atom.
        (
            "qua \\Zib{} elm.\n\nqua zibbet plo. $zibbet$",
            6,
            "qua zibbet plo.",
            "qua ".len(),
        ),
        // Stronger macro corroboration still beats a weaker literal.
        (
            "qua zibbet elm.\n\nqua \\Zib{} plo.",
            6,
            "qua zibbet plo.",
            "qua ".len(),
        ),
        // A macro-free literal has no competitor at all.
        ("qua zibbet plo.", 4, "qua zibbet plo.", "qua ".len()),
    ] {
        let source = macro_literal_tie_document(body);
        for best_guess in [false, true] {
            let location = macro_literal_tie_public(&source, anchor, context, 4, 4, best_guess);
            assert!(location.precise, "{body:?} {best_guess}: {location:?}");
            assert_eq!(location.line, anchor, "{body:?} {best_guess}");
            assert_eq!(location.byte_column, byte, "{body:?} {best_guess}");
        }
    }
}

#[test]
fn macro_literal_tie_keeps_math_and_reference_disambiguation() {
    // Occurrences inside formulas remain separable by the operator between their words.
    let formulas = "$a+x$\nwig\n$a-x$\n";
    assert_eq!(
        source_word_location_with_sources(
            formulas,
            2,
            "a+x",
            2,
            2,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(WordOutcome::Located(1, 3))
    );
    // Reference flanks must beat the tie, as they must beat a unique prose occurrence.
    let equal = "qua zibbet\nqua \\Zib{}\nqua \\cref{sec:a}\n";
    assert_eq!(
        source_word_location_with_sources(
            equal,
            3,
            "qua zibbet",
            4,
            2,
            &[TIE_DEFS],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(WordOutcome::Tied)
    );
    let stronger = "zibbet\nqua \\cref{sec:a} plo\nzibbet\n";
    let label = stronger.lines().nth(1).unwrap().find("sec:a").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            stronger,
            2,
            "qua zibbet plo",
            4,
            1,
            &[],
            &Operation::new(Duration::from_secs(10)),
        ),
        Ok(WordOutcome::Located(2, label))
    );
}

// One click between records of different tags yields both compiler anchors, as real boxes do.
fn macro_literal_tie_two_anchors(
    source: &str,
    anchors: (u32, u32),
    best_guess: bool,
) -> io::Result<InverseResolution> {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(&tex, source).unwrap();
    fs::write(
        pdf.with_extension("synctex"),
        format!(
            concat!(
                "SyncTeX Version:1\nInput:1:{tex}\nOutput:pdf\n",
                "Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n!100\n{{1\n",
                "[1,1:4736286,46220574:26673152,41484288,0\n",
                "(1,{first}:8799518,8865054:22609920,455111,127431\n",
                "h1,{first}:8799518,8865054:983040,0,0\n",
                "x1,{first}:10182558,8865054\nx1,{second}:10582558,8865054\n",
                ")\n]\n}}1\nPostamble:\nCount:7\nPost scriptum:\n",
            ),
            tex = tex.display(),
            first = anchors.0,
            second = anchors.1,
        ),
    )
    .unwrap();
    resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 157.84,
            y_from_top: 134.76,
            page_height_pt: 792.0,
        },
        Some(("qua zibbet", 4)),
        4,
        &Operation::default(),
        None,
        best_guess,
    )
}

#[test]
fn macro_literal_tie_opposes_a_precise_anchor_but_absent_words_do_not() {
    let document = |early: &str, middle: &str, near: &str| {
        macro_literal_tie_document(&format!(
            "{early}\n\n{middle}\n\n{near}\nwig\n\noak\n\nelm\nqua zibbet"
        ))
    };
    for (source, reason) in [
        // Anchor 8 ties inside its own window while anchor 14 finds one occurrence.
        (
            document("qua zibbet", "qua \\Zib{}", "qua zibbet"),
            "tied words",
        ),
        // Two precise anchors keep their established failure.
        (
            document("wig oak", "oak wig", "qua zibbet"),
            "multiple precise",
        ),
    ] {
        let error = macro_literal_tie_two_anchors(&source, (8, 14), false)
            .expect_err("competing anchors must stay ambiguous");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(matches!(
            error
                .get_ref()
                .and_then(|error| error.downcast_ref::<InverseFailure>()),
            Some(InverseFailure::Ambiguous(_))
        ));
        assert!(error.to_string().contains(reason), "{error}");
        let guess = macro_literal_tie_two_anchors(&source, (8, 14), true).unwrap();
        assert!(!guess.location.precise, "{:?}", guess.location);
        assert_eq!(guess.location.line, 8);
        let warning = guess.warning.unwrap();
        assert!(warning.contains(reason), "{warning}");
        assert!(
            warning.contains("using the first compiler anchor"),
            "{warning}"
        );
    }
    // An anchor without the word has no competing evidence, so the other anchor stands.
    let source = document("wig", "oak", "wig oak");
    for best_guess in [false, true] {
        let hit = macro_literal_tie_two_anchors(&source, (8, 14), best_guess).unwrap();
        assert!(hit.location.precise, "{best_guess}: {:?}", hit.location);
        assert_eq!(hit.location.line, 14, "{best_guess}");
        assert_eq!(hit.location.byte_column, 4, "{best_guess}");
    }
}

#[test]
fn macro_literal_tie_yields_to_corroborated_math_only_at_zero_evidence() {
    let beamer = |preamble: &str, body: &str| {
        format!(
            "\\documentclass{{beamer}}\n{preamble}\\begin{{document}}\n\\begin{{frame}}\n{body}\n\\end{{frame}}\n\\end{{document}}\n"
        )
    };
    let sin_macro = "\\newcommand{\\Fn}{sin}\n";
    for (source, anchor, context, offset, line, byte) in [
        // Neither tied occurrence has evidence, and the formula matches its neighbors.
        (
            beamer(sin_macro, "See \\Fn{} wig.\n$\\sin(x)+1$"),
            7,
            "sin(x)+1",
            0,
            6,
            1,
        ),
        (
            beamer("", "See a here.\nSee a there.\n$\\alpha+a$"),
            7,
            "\u{03b1}+a",
            "\u{03b1}+".len(),
            6,
            8,
        ),
        // The same formula without a competing macro.
        (beamer("", "See wig.\n$\\sin(x)+1$"), 6, "sin(x)+1", 0, 5, 1),
    ] {
        for best_guess in [false, true] {
            let location =
                macro_literal_tie_public(&source, anchor, context, offset, 4, best_guess);
            assert!(location.precise, "{source:?} {best_guess}: {location:?}");
            assert_eq!(location.line, line, "{source:?} {best_guess}");
            assert_eq!(location.byte_column, byte, "{source:?} {best_guess}");
        }
    }
    // A math atom with no evidence of its own stays tied with the macro.
    let source = "\\documentclass{article}\n\\newcommand{\\QX}{QX}\n\\begin{document}\n\\QX{} $QX$\n\\end{document}\n";
    for best_guess in [false, true] {
        let location = macro_literal_tie_public(source, 4, "QX", 0, 1, best_guess);
        assert!(!location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 4, "{best_guess}");
    }
    // A math neighbor cannot outrank a tie whose occurrences have evidence too.
    let source = macro_literal_tie_document("qua zibbet\n\nqua \\Zib{}\n\n$qua zibbet$");
    for best_guess in [false, true] {
        let location = macro_literal_tie_public(&source, 8, "qua zibbet", 4, 4, best_guess);
        assert!(!location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 8, "{best_guess}");
    }
}

#[test]
fn macro_literal_tie_blocks_shared_compiler_line_metadata_only() {
    let frame = |body: &str| {
        format!(
            "\\documentclass{{beamer}}\n\\newcommand{{\\Zib}}{{zibbet}}\n\\begin{{document}}\n\\begin{{frame}}\n{body}\n\\end{{frame}}\n\\end{{document}}\n"
        )
    };
    let tied = frame("\\title{qua zibbet}\nqua \\Zib{}\nqua zibbet");
    let literal_tied = frame("\\title{qua zibbet}\nqua zibbet\nqua zibbet");
    for best_guess in [false, true] {
        // The frame's closing line does not separate an unprinted title from tied body words.
        for source in [&tied, &literal_tied] {
            let location = macro_literal_tie_public(source, 8, "qua zibbet", 4, 4, best_guess);
            assert!(!location.precise, "{source:?} {best_guess}: {location:?}");
            assert_eq!(location.line, 8, "{source:?} {best_guess}");
        }
        // A compiler anchor on the title's own row still names that title.
        let location = macro_literal_tie_public(&tied, 5, "qua zibbet", 4, 4, best_guess);
        assert!(location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 5, "{best_guess}");
        assert_eq!(location.byte_column, "\\title{qua ".len(), "{best_guess}");
    }
    // Without a tie the title stays reachable, and an opaque alias still suppresses it.
    let body = "\\begin{frame}\n\\title{Zephyr}\nNimbus\n";
    let plain = format!("% no alias here\n{body}wig\n\\end{{frame}}\n");
    let opaque = format!(
        "\\let\\cue\\note\\noexpand\\bgroup\\renewcommand{{\\note}}[1]{{#1}}\\noexpand\\egroup\n{body}\\cue{{Zephyr}}\n\\end{{frame}}\n"
    );
    for best_guess in [false, true] {
        let location = macro_literal_tie_public(&plain, 6, "Zephyr", 0, 10, best_guess);
        assert!(location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 3, "{best_guess}");
        assert_eq!(location.byte_column, "\\title{".len(), "{best_guess}");
        let location = macro_literal_tie_public(&opaque, 6, "Zephyr", 0, 10, best_guess);
        assert!(!location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 6, "{best_guess}");
    }
}

#[test]
fn macro_literal_tie_outside_math_does_not_depend_on_source_order() {
    for (body, anchor) in [
        ("$qua \\Zib{}$\n$qua zibbet$\nqua zibbet", 5),
        ("qua zibbet\n$qua zibbet$\n$qua \\Zib{}$", 5),
        ("$qua zibbet$\nqua zibbet\n$qua \\Zib{}$", 4),
    ] {
        let source = macro_literal_tie_document(body);
        for best_guess in [false, true] {
            let location =
                macro_literal_tie_public(&source, anchor, "qua zibbet", 4, 4, best_guess);
            assert!(!location.precise, "{body:?} {best_guess}: {location:?}");
            assert_eq!(location.line, anchor, "{body:?} {best_guess}");
        }
    }
    // Without a macro, literal proximity still selects the nearer formula.
    let source = macro_literal_tie_document("qua zibbet\n$qua zibbet$");
    for best_guess in [false, true] {
        let location = macro_literal_tie_public(&source, 5, "qua zibbet", 4, 4, best_guess);
        assert!(location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 5, "{best_guess}");
        assert_eq!(location.byte_column, "$qua ".len(), "{best_guess}");
    }
}

#[test]
fn macro_literal_tie_reference_flanks_guard_math_that_won_a_zero_evidence_tie() {
    let document = |row: &str| {
        format!(
            "\\documentclass{{article}}\n\\newcommand{{\\Fn}}{{sin}}\n\\begin{{document}}\nWig \\Fn{{}} oak.\n{row}\n\\end{{document}}\n"
        )
    };
    let offset = "\u{03b2} ".len();
    for (row, context, byte) in [
        // One reference flank does not replace the corroborated formula on the anchor row.
        (
            "$\\beta\\sin(x)+1$ \u{03b2} \\cref{b} wig",
            "\u{03b2} sin(x)+1",
            6,
        ),
        // Both flanks still outrank a zero-evidence tie.
        (
            "$\\beta\\sin(x)+1$ \u{03b2} \\cref{b} plo",
            "\u{03b2} sin plo",
            26,
        ),
    ] {
        let source = document(row);
        for best_guess in [false, true] {
            let location = macro_literal_tie_public(&source, 5, context, offset, 2, best_guess);
            assert!(location.precise, "{row:?} {best_guess}: {location:?}");
            assert_eq!(location.line, 5, "{row:?} {best_guess}");
            assert_eq!(location.byte_column, byte, "{row:?} {best_guess}");
        }
    }
}

#[test]
fn macro_literal_tie_ignores_repeated_arguments_of_one_metadata_declaration() {
    let frame = |body: &str| {
        format!(
            "\\documentclass{{beamer}}\n\\begin{{document}}\n\\begin{{frame}}\n{body}\n\\end{{frame}}\n\\end{{document}}\n"
        )
    };
    let title = "\\title[qua zibbet]{qua zibbet}\\maketitle";
    let source = frame(title);
    for best_guess in [false, true] {
        // The short and full arguments are one declaration, not competing occurrences.
        let location = macro_literal_tie_public(&source, 5, "qua zibbet", 4, 4, best_guess);
        assert!(location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 4, "{best_guess}");
        assert_eq!(
            location.byte_column,
            title.rfind("zibbet").unwrap(),
            "{best_guess}"
        );
    }
    // An occurrence outside the declaration still blocks the shared compiler line.
    let source = frame(&format!("{title}\nqua zibbet"));
    for best_guess in [false, true] {
        let location = macro_literal_tie_public(&source, 6, "qua zibbet", 4, 4, best_guess);
        assert!(!location.precise, "{best_guess}: {location:?}");
        assert_eq!(location.line, 6, "{best_guess}");
    }
}

// The click is on a verbatim box of the frame closing on row 10, between two tagged records.
fn macro_literal_tie_fragile_click(
    frame_row: &str,
    tags: (u32, u32),
    best_guess: bool,
) -> io::Result<InverseResolution> {
    let boxes = format!(
        concat!(
            "(2,1:8799518,8865054:22609920,455111,127431\n",
            "h2,1:8799518,8865054:983040,0,0\n",
            "x1,{first}:10182558,8865054\nx1,{second}:10582558,8865054\n",
            ")\n",
        ),
        first = tags.0,
        second = tags.1,
    );
    macro_literal_tie_fragile_sheet(frame_row, &boxes, best_guess)
}

fn macro_literal_tie_fragile_sheet(
    frame_row: &str,
    boxes: &str,
    best_guess: bool,
) -> io::Result<InverseResolution> {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let tex = root.join("doc.tex");
    let vrb = root.join("doc.vrb");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &tex,
        format!(
            concat!(
                "\\documentclass{{beamer}}\n\\newcommand{{\\Zib}}{{zibbet}}\n\\begin{{document}}\n",
                "qua zibbet\nqua \\Zib{{}}\nqua zibbet\nwig\n",
                "\\begin{{frame}}[fragile]\n{frame_row}\n\\end{{frame}}\n",
                "wig\nwig\nwig\nqua zibbet\n\\end{{document}}\n",
            ),
            frame_row = frame_row,
        ),
    )
    .unwrap();
    fs::write(&vrb, format!("{frame_row}\n")).unwrap();
    fs::write(
        pdf.with_extension("synctex"),
        format!(
            concat!(
                "SyncTeX Version:1\nInput:1:{tex}\nInput:2:{vrb}\nOutput:pdf\n",
                "Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n!100\n{{1\n",
                "[1,10:4736286,46220574:26673152,41484288,0\n",
                "{boxes}]\n}}1\nPostamble:\nCount:7\nPost scriptum:\n",
            ),
            tex = tex.display(),
            vrb = vrb.display(),
            boxes = boxes,
        ),
    )
    .unwrap();
    resolve_inverse(
        &pdf,
        InversePoint {
            page: 1,
            x: 157.84,
            y_from_top: 134.76,
            page_height_pt: 792.0,
        },
        Some(("qua zibbet", 4)),
        4,
        &Operation::default(),
        None,
        best_guess,
    )
}

#[test]
fn macro_literal_tie_generated_frame_owns_the_click_over_outside_rows() {
    for (frame_row, tags, reason, first_anchor) in [
        // Rows 6 and 14 lie outside the owning frame, whether or not it has the word.
        ("plain oak", (6, 14), "multiple coarse", 6),
        ("qua zibbet", (6, 14), "multiple coarse", 6),
        // A displaced word on row 14 still competes with the frame's own location.
        ("qua zibbet", (9, 14), "tied words", 9),
    ] {
        let error = macro_literal_tie_fragile_click(frame_row, tags, false)
            .expect_err("no anchor owns this click uniquely");
        assert!(error.to_string().contains(reason), "{tags:?}: {error}");
        let guess = macro_literal_tie_fragile_click(frame_row, tags, true).unwrap();
        assert!(!guess.location.precise, "{tags:?}: {:?}", guess.location);
        assert_eq!(guess.location.line, first_anchor, "{tags:?}");
        let warning = guess.warning.unwrap();
        assert!(warning.contains(reason), "{tags:?}: {warning}");
    }
    for best_guess in [false, true] {
        // A word only on an outside row stays coarse, and Best Guess cannot restore it.
        for frame_row in ["plain oak", "qua zibbet"] {
            let hit = macro_literal_tie_fragile_click(frame_row, (14, 14), best_guess).unwrap();
            assert!(!hit.location.precise, "{best_guess}: {:?}", hit.location);
            assert_eq!(hit.location.line, 14, "{best_guess}");
        }
        // Inside the frame the location stands, and a host tie yields to that frame's result.
        for tags in [(9, 9), (10, 10), (6, 9)] {
            let hit = macro_literal_tie_fragile_click("qua zibbet", tags, best_guess).unwrap();
            assert!(hit.location.precise, "{tags:?}: {:?}", hit.location);
            assert_eq!(hit.location.line, 9, "{tags:?}");
            assert_eq!(hit.location.byte_column, 4, "{tags:?}");
        }
    }
}

#[test]
fn macro_literal_tie_frame_ownership_reports_geometry_errors_but_keeps_other_boxes() {
    let generated_box = concat!(
        "(2,1:8799518,8865054:22609920,455111,127431\n",
        "h2,1:8799518,8865054:983040,0,0\n",
        "x1,14:10182558,8865054\nx1,14:10582558,8865054\n",
        ")\n",
    );
    let host_box_beside_generated = concat!(
        "(1,14:8799518,8865054:22609920,455111,127431\n",
        "h1,14:8799518,8865054:983040,0,0\n",
        "x1,14:10182558,8865054\nx1,14:10582558,8865054\n",
        ")\n",
        "(2,1:8799518,30000000:22609920,455111,127431\n",
        "h2,1:8799518,30000000:983040,0,0\n",
        "x2,1:10182558,30000000\n",
        ")\n",
    );
    for best_guess in [false, true] {
        // Only the geometry reader rejects this record, and ownership depends on its answer.
        let boxes = format!("{generated_box}k1,14:abc\n");
        let error = macro_literal_tie_fragile_sheet("plain oak", &boxes, best_guess)
            .expect_err("an unreadable ownership lookup must not keep host precision");
        assert!(error.to_string().contains("invalid position"), "{error}");
        // A complete map with a host box at the click does not displace its word.
        let hit =
            macro_literal_tie_fragile_sheet("plain oak", host_box_beside_generated, best_guess)
                .unwrap();
        assert!(hit.location.precise, "{best_guess}: {:?}", hit.location);
        assert_eq!(hit.location.line, 14, "{best_guess}");
        assert_eq!(hit.location.byte_column, 4, "{best_guess}");
    }
}

#[test]
fn beamer_only_overlay_graphics_disambiguates_by_frame_slide() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("slides.pdf");
    let tex = root.join("slides.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    let source = concat!(
        "\\documentclass{beamer}\n",
        "\\begin{document}\n",
        "\\begin{frame}{Overlay Figures}\n",
        "  \\only<1>{\\includegraphics{slide_one.pdf}}\n",
        "  \\only<2>{\\includegraphics{slide_two.pdf}}\n",
        "  \\only<3>{\\includegraphics{slide_three.pdf}}\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    fs::write(&tex, source).unwrap();

    // Three slides generated from the same frame closing line (line 7), each with 1 rendered figure box.
    let page_box = concat!(
        "[1,7:0,792:612,792,0\n",
        "(1,7:10,200:200,120,0\n",
        "(1,7:50,200:30,30,0\n",
        "(1,7:50,200:30,30,0\n",
        "(1,7:50,200:0,30,0\n",
        "(1,7:50,200:0,15,0\n",
        "(1,7:50,200:15,15,0\n",
        "k1,7:65,200:0\n",
        ")\n",
        "k1,7:50,200:-15\n",
        ")\n",
        ")\n",
        ")\n",
        ")\n",
        ")\n",
        "]\n",
    );
    let synctex = format!(
        "SyncTeX Version:1\nInput:1:{0}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{1\n{1}}}1\n{{2\n{1}}}2\n{{3\n{1}}}3\nPostamble:\nCount:30\n",
        tex.display(),
        page_box,
    );
    fs::write(pdf.with_extension("synctex"), &synctex).unwrap();

    for (page, expected_line, expected_fig) in [
        (1, 4, "slide_one.pdf"),
        (2, 5, "slide_two.pdf"),
        (3, 6, "slide_three.pdf"),
    ] {
        let hit = resolve_inverse(
            &pdf,
            InversePoint {
                page,
                x: 60.0,
                y_from_top: 185.0,
                page_height_pt: 792.0,
            },
            None,
            2,
            &Operation::default(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(hit.location.line, expected_line, "failed on slide {page}");
        let row = source.lines().nth(expected_line as usize - 1).unwrap();
        assert_eq!(hit.location.byte_column, row.find(expected_fig).unwrap());
    }
}

#[test]
fn tikzpicture_wrapper_and_foreach_abstains_on_ambiguous_constructs() {
    // 1. Wrapper lines that also contain prose or close before \begin{tikzpicture} must not
    //    hijack the anchor into the following tikzpicture.
    let inline_prose_after_centering = concat!(
        "\\centering Alpha\\\\\n",
        "\\begin{tikzpicture}\n",
        "  \\node {Alpha};\n",
        "\\end{tikzpicture}\n",
    );
    assert_eq!(
        crate::tex::source_tikzpicture_range(inline_prose_after_centering, 1),
        None
    );

    let closed_makebox = concat!(
        "\\makebox[0pt][l]{Alpha}\n",
        "\\begin{tikzpicture}\n",
        "  \\node {Alpha};\n",
        "\\end{tikzpicture}\n",
    );
    assert_eq!(
        crate::tex::source_tikzpicture_range(closed_makebox, 1),
        None
    );

    let blank_line_before_tikz = concat!(
        "Alpha\n",
        "\n",
        "\\begin{tikzpicture}\n",
        "  \\node {Alpha};\n",
        "\\end{tikzpicture}\n",
    );
    assert_eq!(
        crate::tex::source_tikzpicture_range(blank_line_before_tikz, 2),
        None
    );

    // 2. \foreach starting before the target line (non-tikzpicture scope) is still discovered.
    let mid_loop_prose = concat!(
        "\\foreach \\tag in {FirstWord, SecondWord} {\n",
        "  Item \\tag here.\n",
        "}\n",
    );
    assert_eq!(
        source_word_location(mid_loop_prose, 2, "Item SecondWord here.", 5, 0),
        Some((2, "  Item ".len()))
    );

    // 3. Comment inside \foreach item list is stripped.
    let comment_item = concat!(
        "\\begin{tikzpicture}\n",
        "\\foreach \\tag in {\n",
        "  Alpha % trailing comment\n",
        "} {\n",
        "  \\node {\\tag};\n",
        "}\n",
        "\\end{tikzpicture}\n",
    );
    assert_eq!(
        source_word_location(comment_item, 7, "Alpha", 0, 2),
        Some((5, "  \\node {".len()))
    );

    // 4. Ambiguous \foreach constructs must abstain (return None) rather than guess a decoy.
    for ambiguous in [
        // Macro list (\mylist)
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\tag in \\mylist { \\node {\\tag}; }\n",
            "\\node {Alpha};\n",
            "\\end{tikzpicture}\n",
        ),
        // Options ([evaluate=...], [count=...], [remember=...], [parse=true])
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach [count=\\i] \\tag in {Alpha, Beta} { \\node {\\tag}; }\n",
            "\\node {Alpha};\n",
            "\\end{tikzpicture}\n",
        ),
        // Fractional step / non-integer "..."
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\x in {0, 0.5, ..., 2} { \\node {\\x}; }\n",
            "\\node {0.5};\n",
            "\\end{tikzpicture}\n",
        ),
        // Prefix/suffix "..." pattern
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\x in {a1, ..., a3} { \\node {\\x}; }\n",
            "\\node {a2};\n",
            "\\end{tikzpicture}\n",
        ),
        // Zero step or i32 overflow in "..."
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\x in {-2000000000, 2000000000, ..., 0} { \\node {\\x}; }\n",
            "\\node {0};\n",
            "\\end{tikzpicture}\n",
        ),
        // Stepped alphabetic range
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\x in {a, c, ..., g} { \\node {\\x}; }\n",
            "\\node {c};\n",
            "\\end{tikzpicture}\n",
        ),
        // Braced slash in multi-var item
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\a/\\b in {{Alpha/Beta}/Gamma} { \\node {\\a}; }\n",
            "\\node {Alpha};\n",
            "\\end{tikzpicture}\n",
        ),
        // Empty item in list
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\tag in {,Alpha} { \\node {\\tag}; }\n",
            "\\node {Alpha};\n",
            "\\end{tikzpicture}\n",
        ),
        // Formatting macro inside item
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\tag in {\\textbf{Alpha}} { \\node {\\tag}; }\n",
            "\\node {Alpha};\n",
            "\\end{tikzpicture}\n",
        ),
        // >4 loop variables
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\a/\\b/\\c/\\d/\\e in {1/2/3/4/Alpha} { \\node {\\e}; }\n",
            "\\node {Alpha};\n",
            "\\end{tikzpicture}\n",
        ),
        // >2 loops in scope
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\a in {1,2} { \\node {\\a}; }\n",
            "\\foreach \\b in {3,4} { \\node {\\b}; }\n",
            "\\foreach \\c in {Alpha,Beta} { \\node {\\c}; }\n",
            "\\node {Alpha};\n",
            "\\end{tikzpicture}\n",
        ),
        // Cartesian product > 32
        concat!(
            "\\begin{tikzpicture}\n",
            "\\foreach \\a in {1,...,8} {\n",
            "  \\foreach \\b in {1,...,8} {\n",
            "    \\node {\\a};\n",
            "  }\n",
            "}\n",
            "\\end{tikzpicture}\n",
        ),
    ] {
        let last_line = ambiguous.lines().count() as u32;
        assert_eq!(
            source_word_location(ambiguous, last_line, "Alpha", 0, 2),
            None,
            "expected None for:\n{ambiguous}"
        );
    }

    // 5. Unbraced \foreach body scans up to the first top-level semicolon (and nested \foreach).
    let unbraced_macro_body = concat!(
        "\\begin{tikzpicture}\n",
        "\\newcommand{\\mycmd}[1]{}\n",
        "\\foreach \\tag in {Alpha, Beta} \\mycmd{x} \\node {\\tag};\n",
        "\\node {Alpha};\n",
        "\\end{tikzpicture}\n",
    );
    assert_eq!(
        source_word_location(unbraced_macro_body, 5, "Beta", 0, 2),
        Some((
            3,
            "\\foreach \\tag in {Alpha, Beta} \\mycmd{x} \\node {".len()
        ))
    );
    let nested_unbraced_foreach = concat!(
        "\\begin{tikzpicture}\n",
        "\\foreach \\x in {Alpha, Beta} \\foreach \\y in {1, 2} { \\node {\\x}; \\node {\\y}; }\n",
        "\\node {Alpha};\n",
        "\\end{tikzpicture}\n",
    );
    assert_eq!(
        source_word_location(nested_unbraced_foreach, 4, "Beta", 0, 2),
        Some((
            2,
            "\\foreach \\x in {Alpha, Beta} \\foreach \\y in {1, 2} { \\node {".len()
        ))
    );

    // 6. Loop variable clashing with a defined or poisoned macro abstains, and \DeclareMathOperator
    //    inside a loop body poisons the loop.
    let clashing_loop_var = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{tikz}\n",
        "\\let\\tag\\relax\n",
        "\\begin{document}\n",
        "\\begin{tikzpicture}\n",
        "  \\foreach \\tag in {\\alpha, \\beta} { \\node {$\\tag + \\alpha$}; }\n",
        "  \\node {$\\alpha$};\n",
        "\\end{tikzpicture}\n",
        "\\end{document}\n",
    );
    assert_eq!(
        source_word_location(clashing_loop_var, 8, "\u{03b1}", 0, 2),
        None
    );
    let dmo_in_loop = concat!(
        "\\begin{tikzpicture}\n",
        "  \\foreach \\tag in {Alpha, Beta} { \\DeclareMathOperator{\\tag}{Op} \\node {\\tag}; }\n",
        "  \\node {Alpha};\n",
        "\\end{tikzpicture}\n",
    );
    assert_eq!(source_word_location(dmo_in_loop, 4, "Alpha", 0, 2), None);

    // 7. Trailing prose after \end{tikzpicture} (or after its wrapper) prevents exclusive
    //    tikzpicture ownership and does not resolve to the trailing prose on the closing line.
    let trailing_prose_after_tikz = concat!(
        "\\documentclass{article}\n",
        "\\usepackage{tikz}\n",
        "\\begin{document}\n",
        "\\begin{tikzpicture}\n",
        "  \\node {Alpha};\n",
        "\\end{tikzpicture} Alpha\n",
        "\\end{document}\n",
    );
    assert_eq!(
        crate::tex::source_tikzpicture_range(trailing_prose_after_tikz, 6),
        None
    );
    assert_eq!(
        source_word_location(trailing_prose_after_tikz, 6, "Alpha", 0, 2),
        None
    );
    let trailing_prose_after_wrapper = concat!(
        "\\resizebox{0.5\\linewidth}{!}{%\n",
        "\\begin{tikzpicture}\n",
        "  \\node {Alpha};\n",
        "\\end{tikzpicture}} Alpha\n",
    );
    assert_eq!(
        crate::tex::source_tikzpicture_range(trailing_prose_after_wrapper, 1),
        None
    );
    assert_eq!(
        crate::tex::source_tikzpicture_range(trailing_prose_after_wrapper, 4),
        None
    );
}

#[test]
fn recursive_preamble_splicing_and_beamer_overlay_guards() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("main.pdf");
    let sub_a = root.join("sub_a.tex");
    let sub_b = root.join("sub_b.tex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(&sub_a, "\\input{sub_b}\n").unwrap();
    fs::write(&sub_b, "\\newcommand{\\DeepSym}{\\Lambda}\n").unwrap();

    let mut sources = crate::QuerySources::default();

    let replay_main = root.join("replay.tex");
    fs::write(
        &replay_main,
        "\\futurelet\\peek\\input{sub_a}\n\\begin{document}\n$\\DeepSym+1$\n",
    )
    .unwrap();
    let replay_spliced = crate::macro_preamble_source(
        &replay_main,
        None,
        None,
        &mut sources,
        &Operation::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        crate::tex::source_word_location_with_sources(
            "$\\DeepSym+1$\n",
            1,
            "Λ+1",
            0,
            2,
            &[replay_spliced.source.as_str()],
            &Operation::default()
        )
        .map(WordOutcome::location),
        Ok(Some((1, 1))),
    );

    // Compiler-recorded Input: paths disambiguate CWD-relative preamble imports when main.tex
    // lives in a subdirectory, and unrecorded ambiguous imports are poisoned.
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let sub_main = src_dir.join("provenance.tex");
    let root_cwd_defs = root.join("cwd_defs.tex");
    let decoy_src_defs = src_dir.join("cwd_defs.tex");
    fs::write(&root_cwd_defs, "\\newcommand{\\Pick}{\\beta}\n").unwrap();
    fs::write(&decoy_src_defs, "\\newcommand{\\Pick}{\\alpha}\n").unwrap();
    fs::write(
        &sub_main,
        concat!(
            "\\documentclass{article}\n",
            "\\input{cwd_defs}\n",
            "\\begin{document}\n",
            "$\\Pick$ and $\\alpha$\n",
            "\\end{document}\n",
        ),
    )
    .unwrap();
    let compiled_spliced = crate::macro_preamble_source(
        &sub_main,
        Some(std::slice::from_ref(&root_cwd_defs)),
        Some(&root),
        &mut sources,
        &Operation::default(),
    )
    .unwrap()
    .unwrap();
    let compiled_table = crate::math::macros_from_sources(
        &[compiled_spliced.source.as_str()],
        &Operation::default(),
    );
    assert_eq!(
        compiled_table
            .get("Pick")
            .and_then(|d| d.as_ref())
            .and_then(|d| d.body),
        Some("\\beta")
    );
    let unrecorded_spliced = crate::macro_preamble_source(
        &sub_main,
        None,
        Some(&root),
        &mut sources,
        &Operation::default(),
    )
    .unwrap()
    .unwrap();
    let unrecorded_table = crate::math::macros_from_sources(
        &[unrecorded_spliced.source.as_str()],
        &Operation::default(),
    );
    assert_eq!(unrecorded_table.get("Pick"), Some(&None));

    // TEXINPUTS-style compiler provenance takes precedence over an uncompiled decoy in pdf_dir.
    let search_dir = root.join("search");
    let out_dir = root.join("out");
    fs::create_dir_all(&search_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();
    let texinputs_defs = search_dir.join("ext_defs.tex");
    let decoy_out_defs = out_dir.join("ext_defs.tex");
    let texinputs_main = root.join("texinputs_main.tex");
    fs::write(&texinputs_defs, "\\newcommand{\\ExtSym}{\\beta}\n").unwrap();
    fs::write(&decoy_out_defs, "\\newcommand{\\ExtSym}{\\alpha}\n").unwrap();
    fs::write(
        &texinputs_main,
        "\\documentclass{article}\n\\input{ext_defs}\n\\begin{document}\n$\\ExtSym$\n\\end{document}\n",
    )
    .unwrap();
    let texinputs_spliced = crate::macro_preamble_source(
        &texinputs_main,
        Some(std::slice::from_ref(&texinputs_defs)),
        Some(&out_dir),
        &mut sources,
        &Operation::default(),
    )
    .unwrap()
    .unwrap();
    let texinputs_table = crate::math::macros_from_sources(
        &[texinputs_spliced.source.as_str()],
        &Operation::default(),
    );
    assert_eq!(
        texinputs_table
            .get("ExtSym")
            .and_then(|d| d.as_ref())
            .and_then(|d| d.body),
        Some("\\beta")
    );

    // \pgfmathsetmacro and \pgfmathtruncatemacro are masked as nonprinting definitions,
    // and repeated multi-word or word-only frame titles abstain instead of breaking ties.
    let pgf_source = concat!("\\pgfmathsetmacro{\\unused}{42}\n", "Result 42 here.\n",);
    assert_eq!(
        source_word_location(pgf_source, 2, "Result 42 here.", "Result ".len(), 2),
        Some((2, "Result ".len()))
    );
    let repeated_frame = concat!(
        "\\begin{frame}{Repeated Topic}\n",
        "Repeated Topic\n",
        "\\end{frame}\n",
    );
    assert_eq!(
        source_word_location(repeated_frame, 3, "Repeated Topic", 0, 3),
        None
    );
    assert_eq!(
        source_word_location(repeated_frame, 3, "Repeated", 0, 3),
        None
    );

    // Postfix \only{...}<2> resolves, whereas frame-level [label=...], [1-2], \againframe, or \note abstains.
    let postfix_only = concat!(
        "\\begin{frame}{Figures}\n",
        "  \\only{\\includegraphics{fig1.pdf}}<1>\n",
        "  \\only{\\includegraphics{fig2.pdf}}<2>\n",
        "\\end{frame}\n",
    );
    assert_eq!(
        crate::tex::source_graphics_location_with_context(
            postfix_only,
            4,
            0,
            1,
            true,
            Some(2),
            &[],
            crate::tex::WordLocationMode::Strict,
            None,
            &Operation::default()
        ),
        Some((3, "  \\only{\\includegraphics{".len()))
    );

    for unsafe_frame in [
        concat!(
            "\\begin{frame}[fragile]<2->{Figures}\n",
            "  \\only<1>{\\includegraphics{fig1.pdf}}\n",
            "  \\only<2>{\\includegraphics{fig2.pdf}}\n",
            "\\end{frame}\n",
        ),
        concat!(
            "\\begin{frame}[1-2]{Figures}\n",
            "  \\only<1>{\\includegraphics{fig1.pdf}}\n",
            "  \\only<2>{\\includegraphics{fig2.pdf}}\n",
            "\\end{frame}\n",
        ),
        concat!(
            "\\begin{frame}[label=myframe]{Figures}\n",
            "  \\only<1>{\\includegraphics{fig1.pdf}}\n",
            "  \\only<2>{\\includegraphics{fig2.pdf}}\n",
            "\\end{frame}\n",
        ),
        concat!(
            "\\begin{frame}{Figures}\n",
            "  \\note{Speaker note}\n",
            "  \\only<1>{\\includegraphics{fig1.pdf}}\n",
            "  \\only<2>{\\includegraphics{fig2.pdf}}\n",
            "\\end{frame}\n",
        ),
        concat!(
            "{\\begin{center}\\end{center}}\\begin{frame}<2->{Figures}\n",
            "  \\only<1>{\\includegraphics{fig1.pdf}}\n",
            "  \\only<2>{\\includegraphics{fig2.pdf}}\n",
            "\\end{frame}\n",
        ),
    ] {
        let end_line = unsafe_frame.lines().count() as u32;
        assert_eq!(
            crate::tex::source_graphics_location_with_context(
                unsafe_frame,
                end_line,
                0,
                1,
                true,
                Some(2),
                &[],
                crate::tex::WordLocationMode::Strict,
                None,
                &Operation::default()
            ),
            None
        );
    }
    assert_eq!(
        crate::tex::source_graphics_location_with_context(
            postfix_only,
            4,
            0,
            1,
            true,
            Some(2),
            &["\\againframe<2>{other}"],
            crate::tex::WordLocationMode::Strict,
            None,
            &Operation::default()
        ),
        None
    );

    // Late Input: tags emitted after sheet 1 are still captured by source_map_from_reader.
    let late_input_synctex = concat!(
        "SyncTeX Version:1\n",
        "Input:1:/tmp/main.tex\n",
        "Output:pdf\n",
        "Magnification:1000\n",
        "Unit:1\n",
        "X Offset:0\n",
        "Y Offset:0\n",
        "Content:\n",
        "{1\n",
        "[1,1:0,0:0,0,0\n",
        "]\n",
        "}1\n",
        "Input:2:/tmp/sub.tex\n",
        "{2\n",
        "[2,1:0,0:0,0,0\n",
        "]\n",
        "}2\n",
        "Postamble:\n",
        "Count:2\n",
    );
    let late_point = InversePoint {
        page: 1,
        x: 0.0,
        y_from_top: 0.0,
        page_height_pt: 792.0,
    };
    let late_map = source_map_from_reader(
        std::io::Cursor::new(late_input_synctex.as_bytes()),
        &pdf,
        late_point,
        Path::new("/tmp/doc.synctex"),
        false,
        &Operation::default(),
        Some(&root),
    )
    .unwrap();
    assert_eq!(late_map.inputs.len(), 2);

    // A single-character control symbol does not hide unrelated math.
    let star_def = "\\def\\*{\\times}\n$\\alpha + 1$ and $\\beta + 1$\n";
    assert_eq!(
        source_word_location(star_def, 2, "\u{03b2} + 1", 0, 2),
        Some((2, "$\\alpha + 1$ and $".len()))
    );

    // Discarded user-macro arguments are masked from prose matching, and unbraced non-TikZ \foreach bodies do not scan to ';'.
    let discarded_arg_src = "\\newcommand{\\ignore}[1]{}\n\\ignore{Target} Target;\n";
    assert_eq!(
        source_word_location(discarded_arg_src, 2, "Target;", 0, 2),
        Some((2, "\\ignore{Target} ".len()))
    );
    let prose_foreach_src = "\\foreach \\x in {Target} { Static }\n";
    assert_eq!(
        source_word_location(prose_foreach_src, 1, "Target", 0, 1),
        None
    );

    // \operatorname*{argmax}_x, @-named macros, \csname...\endcsname, redefined \label, and glue with repeated plus.
    let math_fixes_src = concat!(
        "\\makeatletter\\newcommand{\\foo@bar}{\\beta}\\makeatother\n",
        "$\\operatorname*{argmax}_x$ and $\\foo@bar$\n",
    );
    assert_eq!(
        source_word_location(math_fixes_src, 2, "argmax", 0, 2),
        Some((2, "$\\operatorname*{".len()))
    );
    assert_eq!(
        source_word_location(math_fixes_src, 2, "\u{03b2}", 0, 2),
        Some((2, "$\\operatorname*{argmax}_x$ and $".len()))
    );
    let csname_src = concat!(
        "\\newcommand{\\dyn}{\\alpha}\n",
        "\\expandafter\\def\\csname dyn\\endcsname{\\beta}\n",
        "$\\dyn$ and $\\alpha$\n",
    );
    assert_eq!(
        crate::math::macros_from_sources(&[csname_src], &Operation::default()).get("dyn"),
        Some(&None)
    );
    assert_eq!(source_word_location(csname_src, 3, "\u{03b1}", 0, 3), None);

    let redefined_label_src = "\\renewcommand{\\label}[1]{\\alpha}\n$\\label{x} + \\beta$\n";
    assert_eq!(
        source_word_location(redefined_label_src, 2, "\u{03b1}+\u{03b2}", 0, 2),
        Some((2, "$".len()))
    );

    let double_plus_glue = "$\\mskip 1mu plus 2mu plus 3mu \\alpha$\n";
    assert_eq!(
        source_word_location(double_plus_glue, 1, "plus 3mu \u{03b1}", 0, 1),
        Some((1, "$\\mskip 1mu plus 2mu ".len()))
    );

    let row_break_tikz = concat!(
        "\\begin{tikzpicture}\n",
        "  \\node at (0,0) {the first label $X_1$};\n",
        "  \\node at (1,0) {$Y$ is placed below $X_1$:\\\\the second block is aligned};\n",
        "\\end{tikzpicture}\n",
    );
    let row_break_ctx = "the first label X1 Y is placed below X1:\r\nthe second block is aligned";
    let second_the = row_break_ctx.rfind("the ").unwrap();
    assert_eq!(
        source_word_location(row_break_tikz, 3, row_break_ctx, second_the, 2),
        Some((3, "  \\node at (1,0) {$Y$ is placed below $X_1$:\\\\".len()))
    );
    let forward_row_break = crate::ForwardWord::at("Alpha\\\\beta gamma", 8).unwrap();
    assert_eq!(forward_row_break.words, vec!["Alpha", "beta", "gamma"]);
    assert_eq!(forward_row_break.selected, 1);
    let accent_after_break = "First line\nRow\\\\\\'example here\n";
    assert_eq!(
        source_word_location(accent_after_break, 1, "Row \u{00e9}xample here", 4, 2),
        Some((2, "Row\\\\".len()))
    );
    let active_custom_env_prose = concat!(
        "\\NewDocumentEnvironment{badge}{}{Sharedword }{}%\n",
        "\\begin{badge}Body\\end{badge}\n",
        "Sharedword text\n",
    );
    assert_eq!(
        source_word_location(active_custom_env_prose, 3, "Sharedword", 0, 1),
        None
    );
    let unused_custom_env_prose = concat!(
        "\\NewDocumentEnvironment{badge}{}{Sharedword }{}%\n",
        "Sharedword text\n",
    );
    assert_eq!(
        source_word_location(unused_custom_env_prose, 2, "Sharedword", 0, 1),
        Some((2, 0))
    );

    // Metadata lookup with explicit compiled_inputs refuses uncompiled decoy in main.parent().
    let meta_dir = root.join("meta_prov");
    let meta_sub = meta_dir.join("compiled");
    fs::create_dir_all(&meta_sub).unwrap();
    let meta_main = meta_dir.join("main.tex");
    let meta_decoy = meta_dir.join("defs.tex");
    let meta_real = meta_sub.join("defs.tex");
    fs::write(
        &meta_main,
        "\\documentclass{article}\n\\input{defs}\n\\begin{document}\nBody\n\\end{document}\n",
    )
    .unwrap();
    fs::write(&meta_decoy, "\\title{Sample Title}\n").unwrap();
    fs::write(&meta_real, "\\title{Other Heading}\n").unwrap();
    assert_eq!(
        crate::document_metadata_word_location_with_context(
            &meta_main,
            Some(std::slice::from_ref(&meta_real)),
            Some(&meta_sub),
            "Sample Title",
            0,
            &mut sources,
            &Operation::default(),
        )
        .unwrap(),
        None
    );

    // Concatenated gzip members in .synctex.gz are decoded across member boundaries.
    {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let gz_path = root.join("multi.synctex.gz");
        let part1 = concat!(
            "SyncTeX Version:1\n",
            "Input:1:/tmp/main.tex\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
        );
        let part2 = concat!(
            "{1\n",
            "[1,1:0,0:0,0,0\n",
            "]\n",
            "}1\n",
            "Postamble:\n",
            "Count:1\n",
        );
        let mut combined = Vec::new();
        {
            let mut enc1 = GzEncoder::new(&mut combined, Compression::default());
            enc1.write_all(part1.as_bytes()).unwrap();
            enc1.finish().unwrap();
        }
        {
            let mut enc2 = GzEncoder::new(&mut combined, Compression::default());
            enc2.write_all(part2.as_bytes()).unwrap();
            enc2.finish().unwrap();
        }
        fs::write(&gz_path, &combined).unwrap();
        let decoded =
            crate::decode_synctex_file(fs::File::open(&gz_path).unwrap(), 1024 * 1024).unwrap();
        let parsed = source_map_from_reader(
            decoded,
            &pdf,
            late_point,
            &gz_path,
            false,
            &Operation::default(),
            Some(&root),
        )
        .unwrap();
        assert_eq!(parsed.inputs.len(), 1);
    }

    // Symlink alias recorded in SourceInfo/SourceSnapshot still normalizes after symlink retargeting.
    #[cfg(unix)]
    {
        use sha2::{Digest, Sha256};
        use std::collections::BTreeMap;
        use std::fmt::Write as _;
        use std::os::unix::fs::PermissionsExt;

        let alias_dir = root.join("alias_snap");
        fs::create_dir_all(&alias_dir).unwrap();
        let target_a = alias_dir.join("target_a.tex");
        let target_b = alias_dir.join("target_b.tex");
        let link_tex = alias_dir.join("chapter.tex");
        let snap_pdf = alias_dir.join("doc.pdf");
        let snap_synctex = alias_dir.join("doc.synctex");
        let snap_dir = alias_dir.join("snap");
        fs::create_dir_all(&snap_dir).unwrap();
        fs::set_permissions(&snap_dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&target_a, "Hello world\n").unwrap();
        fs::write(&target_b, "Different file\n").unwrap();
        fs::write(&snap_pdf, b"%PDF-1.4\n").unwrap();
        std::os::unix::fs::symlink(&target_a, &link_tex).unwrap();
        let synctex_body = format!(
            concat!(
                "SyncTeX Version:1\n",
                "Input:1:{}\n",
                "Output:pdf\n",
                "Magnification:1000\n",
                "Unit:1\n",
                "X Offset:0\n",
                "Y Offset:0\n",
                "Content:\n",
                "{{1\n",
                "[1,1:0,0:0,0,0\n",
                "]\n",
                "}}\n",
                "Postamble:\n",
                "Count:1\n",
            ),
            link_tex.display()
        );
        fs::write(&snap_synctex, synctex_body).unwrap();
        let info = crate::source_snapshot::SourceInfo::read_with_operation(
            &snap_pdf,
            &Operation::default(),
        )
        .unwrap();
        let canonical_a = fs::canonicalize(&target_a).unwrap();
        assert_eq!(info.aliases.get(&link_tex), Some(&canonical_a));

        fs::write(snap_dir.join("1.txt"), "Hello world\n").unwrap();
        let mut digest = String::with_capacity(64);
        for byte in Sha256::digest(b"Hello world\n").iter() {
            write!(digest, "{byte:02x}").unwrap();
        }
        let manifest_path = snap_dir.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec(&serde_json::json!({
                "pdf": fs::canonicalize(&snap_pdf).unwrap(),
                "revision": crate::DocumentRevision::read(&snap_pdf).unwrap(),
                "cwd": &alias_dir,
                "main": &canonical_a,
                "aliases": info.aliases,
                "files": BTreeMap::from([(canonical_a.clone(), "1.txt")]),
                "digests": BTreeMap::from([(canonical_a.clone(), digest)]),
                "warnings": {},
            }))
            .unwrap(),
        )
        .unwrap();
        fs::remove_file(&link_tex).unwrap();
        std::os::unix::fs::symlink(&target_b, &link_tex).unwrap();
        let loaded =
            crate::source_snapshot::SourceSnapshot::load(&manifest_path, &snap_pdf).unwrap();
        assert_eq!(loaded.normalize(&link_tex).unwrap(), canonical_a);
        assert_eq!(loaded.read(&link_tex).unwrap(), "Hello world\n");
    }
}

#[test]
fn strutbox_rule_tags_multipage_bbl_origins_and_zero_size_wrapper_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();

    // 1. Zero-width \copy\strutbox hbox from preamble (1,54) must not overwrite a
    // cross-file zero-width rule `r2,25` on Page 2 and hijack preamble forward search.
    let strut_dir = root.join("strutbox_case");
    fs::create_dir_all(&strut_dir).unwrap();
    let strut_pdf = strut_dir.join("main.pdf");
    let strut_main = strut_dir.join("main.tex");
    let strut_sub = strut_dir.join("chapter.tex");
    fs::write(&strut_pdf, b"%PDF-1.4\n").unwrap();
    let mut main_lines = String::new();
    for idx in 1..=59 {
        main_lines.push_str(&format!("% preamble line {idx}\n"));
    }
    main_lines.push_str("\\begin{document}\nFirst page text.\n\\input{chapter}\n\\end{document}\n");
    fs::write(&strut_main, &main_lines).unwrap();
    fs::write(&strut_sub, "Aligned equation text.\n".repeat(30)).unwrap();
    let strut_synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{}\n",
            "Input:2:{}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[1,60:0,0:30000000,40000000,0\n",
            "(1,61:655360,6553600:6553600,655360,0\n",
            "x1,61:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}1\n",
            "{{2\n",
            "[2,30:0,0:30000000,40000000,0\n",
            "(2,25:655360,6553600:6553600,655360,0\n",
            "[2,25:655360,6553600:0,623900,267389\n",
            "(1,54:655360,6553600:0,623900,267389\n",
            "r2,25:655360,6553600:0,623900,267389\n",
            ")\n",
            "]\n",
            "x2,25:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}2\n",
            "Postamble:\n",
            "Count:13\n",
        ),
        strut_main.display(),
        strut_sub.display()
    );
    fs::write(strut_pdf.with_extension("synctex"), strut_synctex).unwrap();
    let forward_preamble = resolve_forward(&strut_pdf, &strut_main, 10, 1).unwrap();
    assert_eq!(forward_preamble.page, 1);

    // 2. Multi-page generated `.bbl` sheets whose sheet roots point into `.bbl`
    // remap inverse hits back to the host `.tex` file instead of failing.
    let bbl_dir = root.join("bbl_case");
    fs::create_dir_all(&bbl_dir).unwrap();
    let bbl_pdf = bbl_dir.join("paper.pdf");
    let bbl_tex = bbl_dir.join("paper.tex");
    let bbl_file = bbl_dir.join("paper.bbl");
    fs::write(&bbl_pdf, b"%PDF-1.4\n").unwrap();
    fs::write(
        &bbl_tex,
        concat!(
            "\\documentclass{article}\n",
            "\\begin{document}\n",
            "Intro paragraph.\n",
            "\\bibliography{refs}\n",
            "Appendix text.\n",
            "\\end{document}\n",
        ),
    )
    .unwrap();
    fs::write(
        &bbl_file,
        "\\begin{thebibliography}{99}\n\\bibitem{a} Ref.\n\\end{thebibliography}\n",
    )
    .unwrap();
    let bbl_synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{}\n",
            "Input:2:{}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[1,4:0,0:30000000,40000000,0\n",
            "(2,1:655360,6553600:6553600,655360,0\n",
            "x2,1:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}1\n",
            "{{2\n",
            "[2,2:0,0:30000000,40000000,0\n",
            "(2,2:655360,6553600:6553600,655360,0\n",
            "x2,2:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}2\n",
            "{{3\n",
            "[1,5:0,0:30000000,40000000,0\n",
            "(1,5:655360,6553600:6553600,655360,0\n",
            "x1,5:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}3\n",
            "Postamble:\n",
            "Count:12\n",
        ),
        bbl_tex.display(),
        bbl_file.display()
    );
    fs::write(bbl_pdf.with_extension("synctex"), bbl_synctex).unwrap();
    let bbl_hit = resolve_inverse(
        &bbl_pdf,
        InversePoint {
            page: 2,
            x: 19.0,
            y_from_top: 98.0,
            page_height_pt: 792.0,
        },
        Some(("Appendix", 0)),
        2,
        &Operation::default(),
        None,
        true,
    )
    .unwrap();
    assert_eq!(Path::new(&bbl_hit.location.file), bbl_tex.as_path());
    assert_eq!(bbl_hit.location.line, 5);
    assert!(!bbl_hit.location.precise);

    // When the sheet after a multi-page `.bbl` belongs to a different `\input`
    // (`appendix.tex`), prefer the preceding bibliography host sheet (`paper.tex`)
    // instead of attributing bibliography pages to `appendix.tex`.
    let appendix_tex = bbl_dir.join("appendix.tex");
    fs::write(&appendix_tex, "\\section{Appendix}\nSharedword text.\n").unwrap();
    let bbl_split_synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{}\n",
            "Input:2:{}\n",
            "Input:3:{}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[1,4:0,0:30000000,40000000,0\n",
            "(2,1:655360,6553600:6553600,655360,0\n",
            "x2,1:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}1\n",
            "{{2\n",
            "[2,2:0,0:30000000,40000000,0\n",
            "(2,2:655360,6553600:6553600,655360,0\n",
            "x2,2:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}2\n",
            "{{3\n",
            "[3,1:0,0:30000000,40000000,0\n",
            "(3,2:655360,6553600:6553600,655360,0\n",
            "x3,2:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}3\n",
            "Postamble:\n",
            "Count:12\n",
        ),
        bbl_tex.display(),
        bbl_file.display(),
        appendix_tex.display()
    );
    fs::write(bbl_pdf.with_extension("synctex"), bbl_split_synctex).unwrap();
    let bbl_split_hit = resolve_inverse(
        &bbl_pdf,
        InversePoint {
            page: 2,
            x: 19.0,
            y_from_top: 98.0,
            page_height_pt: 792.0,
        },
        Some(("Sharedword", 0)),
        2,
        &Operation::default(),
        None,
        true,
    )
    .unwrap();
    assert_eq!(Path::new(&bbl_split_hit.location.file), bbl_tex.as_path());
    assert_eq!(bbl_split_hit.location.line, 4);
    assert!(!bbl_split_hit.location.precise);

    // When a preceding sheet (`appendix.tex`) only has `\bibliographystyle`,
    // an unused macro/environment definition, `\iffalse\bibliography{decoy}\fi`,
    // manual `thebibliography`, or `\verb`/`\lstinline` mentioning `paper.bbl`
    // (or another generated sidecar/graphics wrapper like `other.vrb` or
    // `figure.pdf_tex` precedes the `.bbl` sheet), fall through to the following
    // owning sheet (`paper.tex`); when no adjacent sheet owns `paper.bbl`,
    // preserve the raw line `paper.bbl:2` even if the sheet-root is `paper.bbl:3`.
    let vrb_sidecar = dir.path().join("other.vrb");
    fs::write(&vrb_sidecar, "\\input{paper.bbl}\nSharedword\n").unwrap();
    let pdf_tex_sidecar = dir.path().join("figure.pdf_tex");
    fs::write(&pdf_tex_sidecar, "\\input{paper.bbl}\nSharedword\n").unwrap();
    fs::write(
        &appendix_tex,
        concat!(
            "\\bibliographystyle{plain}\n",
            "\\newcommand{\\example}{\\bibliography{unused}}\n",
            "\\NewDocumentEnvironment{unusedrefs}{}{\\bibliography{decoy}}{}\n",
            "\\newif\\ifunusedbib \\iffalse\\bibliography{decoy}\\fi\n",
            "\\begin{thebibliography}{1}\\bibitem{k}Sharedword\\end{thebibliography}\n",
            "\\verb|paper.bbl| \\lstinline|\\input{paper.bbl}|\n",
            "\\detokenize{\\input{paper.bbl}} \\string\\input{paper.bbl}\n",
            "\\NewExpandableDocumentCommand{\\unused}{}{\\input{paper.bbl}}\n",
            "\\toks0{\\input{paper.bbl}} \\makeatletter\\@firstoftwo{safe}{\\input{paper.bbl}}\\@gobbletwo\\input{paper.bbl}\\makeatother\n",
            "\\input{other/paper.bbl}\n",
            "Sharedword text.\n",
            "{\\endinput}\n",
            "\\input{paper.bbl}\n",
        ),
    )
    .unwrap();
    let bbl_after_foreign_synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{}\n",
            "Input:2:{}\n",
            "Input:3:{}\n",
            "Input:4:{}\n",
            "Input:5:{}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[4,1:0,0:30000000,40000000,0\n",
            "(4,1:655360,6553600:6553600,655360,0\n",
            "x4,1:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}1\n",
            "{{2\n",
            "[5,1:0,0:30000000,40000000,0\n",
            "(5,1:655360,6553600:6553600,655360,0\n",
            "x5,1:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}2\n",
            "{{3\n",
            "[3,7:0,0:30000000,40000000,0\n",
            "(3,7:655360,6553600:6553600,655360,0\n",
            "x3,7:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}3\n",
            "{{4\n",
            "[2,3:0,0:30000000,40000000,0\n",
            "(2,2:655360,6553600:6553600,655360,0\n",
            "x2,2:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}4\n",
            "{{5\n",
            "[1,5:0,0:30000000,40000000,0\n",
            "(1,5:655360,6553600:6553600,655360,0\n",
            "x1,5:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}5\n",
            "Postamble:\n",
            "Count:20\n",
        ),
        bbl_tex.display(),
        bbl_file.display(),
        appendix_tex.display(),
        vrb_sidecar.display(),
        pdf_tex_sidecar.display()
    );
    fs::write(bbl_pdf.with_extension("synctex"), bbl_after_foreign_synctex).unwrap();
    for best in [false, true] {
        let bbl_after_foreign_hit = resolve_inverse(
            &bbl_pdf,
            InversePoint {
                page: 4,
                x: 19.0,
                y_from_top: 98.0,
                page_height_pt: 792.0,
            },
            Some(("Sharedword", 0)),
            3,
            &Operation::default(),
            None,
            best,
        )
        .unwrap();
        assert_eq!(
            Path::new(&bbl_after_foreign_hit.location.file),
            bbl_tex.as_path()
        );
        assert_eq!(bbl_after_foreign_hit.location.line, 5);
        assert!(!bbl_after_foreign_hit.location.precise);
    }

    let bbl_unowned_synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{}\n",
            "Input:2:{}\n",
            "Input:3:{}\n",
            "Input:4:{}\n",
            "Input:5:{}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[4,1:0,0:30000000,40000000,0\n",
            "(4,1:655360,6553600:6553600,655360,0\n",
            "x4,1:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}1\n",
            "{{2\n",
            "[5,1:0,0:30000000,40000000,0\n",
            "(5,1:655360,6553600:6553600,655360,0\n",
            "x5,1:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}2\n",
            "{{3\n",
            "[3,7:0,0:30000000,40000000,0\n",
            "(3,7:655360,6553600:6553600,655360,0\n",
            "x3,7:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}3\n",
            "{{4\n",
            "[2,3:0,0:30000000,40000000,0\n",
            "(2,2:655360,6553600:6553600,655360,0\n",
            "x2,2:1310720,6553600\n",
            ")\n",
            "]\n",
            "}}4\n",
            "Postamble:\n",
            "Count:16\n",
        ),
        bbl_tex.display(),
        bbl_file.display(),
        appendix_tex.display(),
        vrb_sidecar.display(),
        pdf_tex_sidecar.display()
    );
    fs::write(bbl_pdf.with_extension("synctex"), bbl_unowned_synctex).unwrap();
    for best in [false, true] {
        let hit = resolve_inverse(
            &bbl_pdf,
            InversePoint {
                page: 4,
                x: 19.0,
                y_from_top: 98.0,
                page_height_pt: 792.0,
            },
            Some(("Sharedword", 0)),
            3,
            &Operation::default(),
            None,
            best,
        )
        .unwrap();
        assert_eq!(Path::new(&hit.location.file), bbl_file.as_path());
        assert_eq!(hit.location.line, 2);
        assert!(!hit.location.precise);
    }

    assert_eq!(
        source_word_location(
            "\\NewDocumentEnvironment{badge}{}{Sharedword }{}\néaebadge Sharedword\n",
            2,
            "Sharedword",
            0,
            2,
        ),
        Some((2, 10)),
    );
    assert_eq!(
        source_word_location(
            "\\begin{thebibliography}{1}\n\\bibitem{Sharedword} Sharedword\n\\end{thebibliography}\n",
            2,
            "Sharedword",
            0,
            2,
        ),
        Some((2, 21)),
    );

    for env_src in [
        concat!(
            "\\NewDocumentEnvironment{badge}{O{Sharedword}}{#1 }{}\n",
            "\\begin{badge}Body\\end{badge} Sharedword\n",
        ),
        concat!(
            "\\newcommand{\\usebadge}{\\begin{badge}Body\\end{badge}}\n",
            "\\NewDocumentEnvironment{badge}{}{Sharedword }{}\n",
            "\\usebadge Sharedword\n",
        ),
        concat!(
            "\\NewDocumentEnvironment{badge}{}{Sharedword }{}\n",
            "\\csname badge\\endcsname Body\\csname endbadge\\endcsname Sharedword\n",
        ),
        concat!(
            "\\newcommand{\\leaf}{Sharedword}\n",
            "\\newcommand{\\outerword}{\\leaf}\n",
            "\\NewDocumentEnvironment{badge}{}{\\outerword }{}\n",
            "\\begin{badge}Body\\end{badge} Sharedword\n",
        ),
    ] {
        let last = env_src.lines().count() as u32;
        assert_eq!(
            source_word_location(env_src, last, "Sharedword", 0, 2),
            None,
            "expected None for:\n{env_src}"
        );
    }

    // 3. Zero-size TikZ wrapper hbox uses upstream one-sided `contain_box` (`else if`)
    // so a straddling child hbox does not inflate both sides of the zero-size parent.
    let tikz_synctex = format!(
        concat!(
            "SyncTeX Version:1\n",
            "Input:1:{}\n",
            "Output:pdf\n",
            "Magnification:1000\n",
            "Unit:1\n",
            "X Offset:0\n",
            "Y Offset:0\n",
            "Content:\n",
            "{{1\n",
            "[1,6:0,0:30000000,40000000,0\n",
            "(1,6:6553600,6553600:0,0,0\n",
            "(1,3:3276800,6553600:6553600,655360,655360\n",
            "x1,3:5898240,6553600\n",
            ")\n",
            ")\n",
            "]\n",
            "}}1\n",
            "Postamble:\n",
            "Count:5\n",
        ),
        bbl_tex.display()
    );
    let tikz_hits = inverse_candidates_from_reader_with_options(
        tikz_synctex.as_bytes(),
        InversePoint {
            page: 1,
            x: 120.0,
            y_from_top: 105.0,
            page_height_pt: 792.0,
        },
        true,
    )
    .unwrap();
    assert_eq!(tikz_hits.len(), 1);
    assert_eq!(tikz_hits[0].line, 3);
}

#[test]
fn stored_self_redefinitions_are_not_invocations() {
    for definition in ["{\\let\\let\\note}", "{\\def\\def{\\note{QuartzWord}}}"] {
        let source = format!("\\begin{{frame}}\n{definition}\nQuartzWord\n\\end{{frame}}\n");
        assert_eq!(
            source_word_location(&source, 4, "QuartzWord", 0, 10),
            Some((3, 0)),
            "{definition}"
        );
    }
}

#[test]
fn overridden_constructed_quotes_do_not_execute_stored_declarations() {
    for name in ["string", "meaning", "noexpand"] {
        let source = format!(
            "\\renewcommand{{\\{name}}}[1]{{#1}}\n\\begin{{frame}}\nQuartzWord\n\
             \\csname {name}\\endcsname\\newcommand{{\\unused}}{{\\note{{QuartzWord}}}}\n\
             \\end{{frame}}\n"
        );
        assert_eq!(
            source_word_location(&source, 5, "QuartzWord", 0, 10),
            Some((3, 0)),
            "{name}"
        );
    }
}

#[test]
fn agy_v9_stored_delimiters_preserve_executing_macro_state() {
    for token in ["\\endgroup", "\\egroup", "\\end{unused}"] {
        let source = format!(
            "\\begingroup\n\\newcommand{{\\unused}}{{{token}}}\n\
             \\renewcommand{{\\textbf}}[1]{{}}\n\\endgroup\n\
             \\begin{{frame}}\n\\textbf{{QuartzWord}}\nQuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(source_word_location(&source, 8, "QuartzWord", 0, 10), None);
    }
    for token in [
        "\\begingroup",
        "\\bgroup",
        "\\begin{document}",
        "\\begin{unused}",
    ] {
        let source = format!(
            "\\newcommand{{\\unused}}{{{token}}}\n\\newcommand{{\\drop}}[1]{{}}\n\
             \\begin{{frame}}\n\\drop{{QuartzWord}}\nQuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(
            source_word_location(&source, 6, "QuartzWord", 0, 10),
            Some((5, 0)),
            "{token}"
        );
    }
    let source = "\\newcommand{\\drop}[1]{}\n\
                  \\newcommand{\\unused}{\\begin{verbatim}}\n\
                  \\renewcommand{\\drop}[1]{#1}\n\
                  \\begin{frame}\n\\drop{QuartzWord}\nQuartzWord\n\\end{frame}\n";
    assert_eq!(source_word_location(source, 7, "QuartzWord", 0, 10), None);
    for default in ["$", "\\(", "\\["] {
        let source = format!(
            "\\newcommand{{\\unused}}[1][{default}]{{}}\n\\newcommand{{\\drop}}[1]{{}}\n\
             \\begin{{frame}}\n\\drop{{QuartzWord}}\nQuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(
            source_word_location(&source, 6, "QuartzWord", 0, 10),
            Some((5, 0)),
            "{default}"
        );
    }
}

#[test]
fn agy_v9_stored_delimiters_preserve_preamble_import_scope() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let main = root.join("main.tex");
    let child = root.join("macros.tex");
    for (prefix, definition, call, expected) in [
        (
            "\\newcommand{\\unused}{\\begin{document}}\n\\input{macros}\n",
            "\\newcommand{\\drop}[1]{}\n",
            "\\drop{QuartzWord}",
            Some((3, 0)),
        ),
        (
            "\\newcommand{\\unused}[1][$]{}\n\\input{macros}\n",
            "\\newcommand{\\drop}[1]{}\n",
            "\\drop{QuartzWord}",
            Some((3, 0)),
        ),
        (
            "\\begingroup\n\\newcommand{\\unused}{\\endgroup}\n\
             \\input{macros}\n\\endgroup\n",
            "\\renewcommand{\\textbf}[1]{}\n",
            "\\textbf{QuartzWord}",
            None,
        ),
    ] {
        let frame = format!("\\begin{{frame}}\n{call}\nQuartzWord\n\\end{{frame}}\n");
        fs::write(&main, format!("{prefix}\\begin{{document}}\n{frame}")).unwrap();
        fs::write(&child, definition).unwrap();
        let spliced = crate::macro_preamble_source(
            &main,
            None,
            None,
            &mut crate::QuerySources::default(),
            &Operation::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            crate::tex::source_word_location_with_sources(
                &frame,
                4,
                "QuartzWord",
                0,
                10,
                &[spliced.source.as_str()],
                &Operation::default()
            )
            .map(WordOutcome::location)
            .ok()
            .flatten(),
            expected,
            "{prefix}"
        );
    }
}

#[test]
fn agy_v10_parameterized_environment_notes_preserve_competing_ownership() {
    for (command, prefix, signature) in [
        ("newenvironment", "", "[1]"),
        ("renewenvironment", "\\newenvironment{notenv}{}{}\n", "[1]"),
        ("provideenvironment", "", "[1]"),
        ("NewDocumentEnvironment", "", "{m}"),
        (
            "RenewDocumentEnvironment",
            "\\newenvironment{notenv}{}{}\n",
            "{m}",
        ),
        ("ProvideDocumentEnvironment", "", "{m}"),
        ("DeclareDocumentEnvironment", "", "{m}"),
    ] {
        for (argument, expected) in [("QuartzWord", None), ("OtherWord", Some(7))] {
            let source = format!(
                "{prefix}\\{command}{{notenv}}{signature}{{\\note{{Copper #1}}}}{{}}\n\
                 \\begin{{frame}}\nCopper QuartzWord\n\
                 \\begin{{notenv}}{{{argument}}}\\end{{notenv}}\n\\end{{frame}}\n"
            );
            let body_line = source
                .lines()
                .position(|line| line == "Copper QuartzWord")
                .unwrap() as u32
                + 1;
            assert_eq!(
                source_word_location(
                    &source,
                    source.lines().count() as u32,
                    "Copper QuartzWord",
                    7,
                    20,
                ),
                expected.map(|column| (body_line, column)),
                "{command} {argument}"
            );
        }
    }
}

#[test]
fn agy_v10_nonexecuting_environments_keep_literal_precision() {
    for invocation in [
        "",
        "\\detokenize{\\begin{setup}\\end{setup}}",
        "\\string\\begin{setup}\\string\\end{setup}",
        "\\drop{\\begin{setup}\\end{setup}}",
        "\\newcommand{\\unused}{\\begin{setup}\\end{setup}}",
    ] {
        let source = format!(
            "\\newcommand{{\\drop}}[1]{{}}\n\
             \\newcommand{{\\cue}}[1]{{\\note{{#1}}}}\n\
             \\newenvironment{{setup}}{{\\csname le%\nt\\endcsname\\drop\\cue}}{{}}\n\
             \\NewDocumentEnvironment{{badge}}{{}}{{QuartzWord}}{{}}\n\
             \\begin{{frame}}\n{invocation}\nCopper QuartzWord\n\\end{{frame}}\n"
        );
        let owner = source
            .lines()
            .position(|line| line == "Copper QuartzWord")
            .unwrap() as u32
            + 1;
        assert_eq!(
            source_word_location(
                &source,
                source.lines().count() as u32,
                "Copper QuartzWord",
                7,
                20,
            ),
            Some((owner, 7)),
            "{invocation}"
        );
    }
    let source = "\\renewcommand{\\note}[1]{}\n\
                  \\newenvironment{notenv}[1]{\\note{Copper #1}}{}\n\
                  \\begin{frame}\nCopper QuartzWord\n\
                  \\begin{notenv}{QuartzWord}\\end{notenv}\n\\end{frame}\n";
    assert_eq!(
        source_word_location(source, 6, "Copper QuartzWord", 7, 10),
        Some((4, 7)),
    );
}

#[test]
fn optional_defaults_and_stored_quotes_preserve_execution_ownership() {
    let source = "\\newcommand{\\drop}[1]{}\n\
                  \\newcommand{\\setup}[1][\\let\\drop\\textbf]{#1}\n\\setup\n\
                  \\begin{frame}\n\\drop{QuartzWord}\nCopper QuartzWord\n\\end{frame}\n";
    assert_eq!(source_word_location(source, 7, "QuartzWord", 0, 16), None);
    for quote in [
        "\\string\\let x",
        "\\meaning\\def\\drop{}",
        "\\noexpand\\input{missing}",
    ] {
        for signature in ["", "[1][UnprintedDefault]"] {
            let source = format!(
                "\\newcommand{{\\unused}}{signature}{{{quote}}}\n\
                 \\newcommand{{\\drop}}[1]{{}}\n\\begin{{frame}}\n\
                 \\drop{{QuartzWord}}\nCopper QuartzWord\n\\end{{frame}}\n"
            );
            assert_eq!(
                source_word_location(&source, 6, "Copper QuartzWord", 7, 16),
                Some((5, 7)),
                "{signature} {quote}"
            );
        }
    }
}

#[test]
fn discarded_environment_arguments_do_not_execute_stored_declarations() {
    let source = "\\newcommand{\\drop}[1]{}\n\
                  \\def\\enddrop{}\n\
                  \\newenvironment{fresh}{}{}\n\
                  \\newcommand{\\setup}{\\begin{drop}{\\renewenvironment{fresh}{}{}}\\end{drop}}\n\
                  \\setup\n\\begin{frame}\nCopper QuartzWord\n\\end{frame}\n";
    assert_eq!(
        source_word_location(source, 8, "Copper QuartzWord", 7, 16),
        Some((7, 7)),
    );
}

#[test]
fn discarded_begin_tokens_do_not_consume_following_printed_arguments() {
    let source = "\\begin{frame}\n\
                  \\begin{drop}\\begin{eat} {QuartzWord}\\end{drop}\n\
                  Copper QuartzWord\n\\end{frame}\n";
    let defs = "\\newcommand{\\drop}[1]{}\n\\def\\enddrop{}\n\
                \\newcommand{\\eat}[1]{}\n\\def\\endeat{}\n";
    assert!(matches!(
        tex::source_word_location_with_sources(
            source,
            4,
            "QuartzWord",
            0,
            16,
            &[defs],
            &Operation::default()
        )
        .map(WordOutcome::location),
        Ok(None) | Err(()),
    ));
}

#[test]
fn closing_environment_parameters_do_not_consume_physical_source() {
    for signature in ["", "#1", "#1#2"] {
        let source = format!(
            "\\def\\badge{{}}\n\\def\\endbadge{signature}{{}}\n\\begin{{frame}}\n\
             \\begin{{badge}}\\end{{badge}}{{QuartzWord}}{{}}\n\
             Copper QuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(source_word_location(&source, 6, "QuartzWord", 0, 16), None);
    }
}

#[test]
fn incomplete_information_does_not_execute_unrebound_builtin_arguments() {
    for command in ["\\includegraphics{\\figdir/plot}", "\\label{\\figdir}"] {
        let source = format!(
            "\\newcommand{{\\defalias}}[1]{{\\expandafter\\let\\csname #1\\endcsname\\relax}}\n\
             \\newcommand{{\\figdir}}{{figs}}\n\\begin{{frame}}\n{command}\n\\end{{frame}}\n\
             \\begin{{frame}}\nCopper QuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(
            source_word_location(&source, 8, "Copper QuartzWord", 7, 16),
            Some((7, 7)),
            "{command}"
        );
    }
}

#[test]
fn discarded_arguments_preserve_paragraph_and_unicode_token_boundaries() {
    for boundary in ["\n\n", "% comment\n\n", "\r\n\r\n", "\u{a0}", "\u{2003}"] {
        let source = format!(
            "\\newcommand{{\\drop}}[1]{{}}\n\\begin{{frame}}\n\
             Copper QuartzWord\n\\drop{boundary}QuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(
            source_word_location(&source, source.lines().count() as u32, "QuartzWord", 0, 16),
            None,
            "{boundary:?}"
        );
    }
    let source = "\\newcommand{\\drop}[1]{}\n\\begin{frame}\n\
                  \\drop \n{QuartzWord}\nCopper QuartzWord\n\\end{frame}\n";
    assert_eq!(
        source_word_location(source, 6, "Copper QuartzWord", 7, 16),
        Some((5, 7)),
    );
}

#[test]
fn rebound_opaque_commands_do_not_hide_executed_redefinitions() {
    for command in ["url", "path", "string", "detokenize"] {
        let source = format!(
            "\\newcommand{{\\drop}}[1]{{}}\n\\renewcommand{{\\{command}}}[1]{{#1}}\n\
             \\{command}{{\\renewcommand{{\\drop}}[1]{{#1}}}}\n\
             \\begin{{frame}}\n\\drop{{QuartzWord}}\nCopper QuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(source_word_location(&source, 7, "QuartzWord", 0, 16), None);
    }
}

#[test]
fn unused_import_defaults_do_not_read_or_change_macro_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let main = root.join("main.tex");
    fs::write(root.join("child.tex"), "\\renewcommand{\\drop}[1]{#1}\n").unwrap();
    for imported in ["child", "missing"] {
        let source = format!(
            "\\newcommand{{\\drop}}[1]{{}}\n\
             \\newcommand{{\\setup}}[1][\\input{{{imported}}}]{{#1}}\n\n\
             \\begin{{frame}}\n\\drop{{QuartzWord}}\nCopper QuartzWord\n\\end{{frame}}\n"
        );
        fs::write(&main, &source).unwrap();
        let spliced = macro_preamble_source(
            &main,
            None,
            Some(&root),
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .unwrap()
        .unwrap();
        let actual = tex::source_word_location_with_sources(
            &source,
            7,
            "Copper QuartzWord",
            7,
            16,
            &[&spliced.source],
            &Operation::default(),
        )
        .map(WordOutcome::location);
        assert_eq!(actual, Ok(Some((6, 7))), "{imported}");
    }
}

#[test]
fn discarded_raw_hash_does_not_consume_the_following_digit() {
    let source = "\\newcommand{\\drop}[1]{}\n\\begin{frame}\n\
                  \\drop#1QuartzWord\nCopper 1QuartzWord\n\\end{frame}\n";
    assert_eq!(source_word_location(source, 5, "1QuartzWord", 0, 16), None);
    for discarded in ["\\begingroup", "\\input{missing}"] {
        let source = format!(
            "\\newcommand{{\\dropTwo}}[2]{{}}\n\
             \\newcommand{{\\setup}}[1]{{\\dropTwo#1{discarded}}}\n\\setup A\n\
             \\begin{{frame}}\nCopper QuartzWord\n\\end{{frame}}\n"
        );
        assert_eq!(
            source_word_location(&source, 6, "Copper QuartzWord", 7, 16),
            Some((5, 7)),
            "{discarded}"
        );
    }
}

#[test]
fn builtin_note_math_remains_target_sensitive_with_optional_arguments() {
    for math in ["$\\alpha$", "\\(\\alpha\\)", "\\[\\alpha\\]"] {
        for options in ["", "<1>[item]"] {
            let source = format!(
                "\\newcommand{{\\cue}}{{\\note{options}{{{math}}}}}\n\
                 \\begin{{frame}}\nNimbus\n\\cue\n\\end{{frame}}\n"
            );
            assert_eq!(
                source_word_location(&source, 5, "Nimbus", 0, 16),
                Some((3, 0)),
                "{options} {math}"
            );
        }
    }
}

#[test]
fn note_consumed_command_does_not_discard_following_printed_word() {
    for options in ["", "<1>", "[item]", "<1>[item]"] {
        let source = format!(
            "\\newcommand{{\\drop}}[1]{{}}\n\\begin{{frame}}\n\
             Copper QuartzWord\n\\note{options}\\drop QuartzWord\n\\end{{frame}}\n"
        );
        let byte = source.lines().nth(3).unwrap().find("QuartzWord").unwrap();
        let actual = source_word_location(&source, 5, "Copper QuartzWord QuartzWord", 18, 16);
        assert!(
            actual.is_none() || actual == Some((4, byte)),
            "{options}: {actual:?}"
        );
    }
}

#[test]
fn forward_guess_words_require_valid_unchanged_printing_source() {
    use std::time::{Duration, UNIX_EPOCH};
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let pdf = root.join("doc.pdf");
    let source = root.join("doc.tex");
    let sidecar = pdf.with_extension("synctex");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        &source,
        "\\documentclass{article}\n\\begin{document}\ncafé Amber\n\\end{document}\n",
    )
    .unwrap();
    fs::write(
        &sidecar,
        format!(
            "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\nMagnification:1000\nUnit:65536\nX Offset:0\nY Offset:0\nContent:\n{{1\n[1,3:0,0:30000000,40000000,0\n(1,3:655360,6553600:6553600,655360,0\nx1,3:1310720,6553600\n)\n]\n}}1\nPostamble:\nCount:4\n",
            source.display(),
        ),
    ).unwrap();
    let set_time = |path: &Path, seconds| {
        fs::File::open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    };
    set_time(&source, 1000);
    set_time(&sidecar, 2000);
    let resolve = |context, column| {
        resolve_forward_with_selector(&pdf, &source, 3, column, context, |_, _, _, anchor| {
            Ok(anchor)
        })
    };
    let strict = resolve(ForwardSource::default(), 6).unwrap();
    let strict_hint = strict.word.as_ref().unwrap();
    assert_eq!(strict_hint.words[strict_hint.selected], "Amber");
    let context = ForwardSource {
        best_guess: true,
        ..ForwardSource::default()
    };
    let guess = resolve(context, 6).unwrap();
    let hint = guess.word.as_ref().unwrap();
    assert_eq!(hint.words[hint.selected], "Amber");
    assert_eq!(
        (guess.page, guess.h, guess.v, guess.width, guess.height),
        (strict.page, strict.h, strict.v, strict.width, strict.height)
    );
    let anchor = resolve(
        ForwardSource {
            anchor_only: true,
            ..context
        },
        6,
    )
    .unwrap();
    assert!(anchor.word.is_none());
    assert!(resolve(context, 999).is_err());

    set_time(&source, 3000);
    let changed_strict = resolve(ForwardSource::default(), 6).unwrap();
    assert!(changed_strict.word.is_none());
    assert_eq!(
        (changed_strict.page, changed_strict.h, changed_strict.v),
        (strict.page, strict.h, strict.v)
    );
    let changed = resolve(context, 6).unwrap();
    assert!(changed.word.is_none());
    assert!(resolve(context, 999).is_err());
    assert_eq!(
        (changed.page, changed.h, changed.v),
        (strict.page, strict.h, strict.v)
    );

    fs::write(
        &source,
        "\\documentclass{article}\n\\newcommand{\\stash}{\nInvisible\n}\n\\begin{document}\nVisible\n\\end{document}\n",
    ).unwrap();
    set_time(&source, 1000);
    let hidden = resolve(context, 3).unwrap();
    assert!(hidden.word.is_none());

    for (bytes, kind) in [
        (
            b"\\documentclass{article}\n\\begin{document}\ncaf\xe9 Amber\n\\end{document}\n"
                .to_vec(),
            io::ErrorKind::InvalidData,
        ),
        (vec![b'x'; 2 * 1024 * 1024 + 1], io::ErrorKind::FileTooLarge),
    ] {
        fs::write(&source, bytes).unwrap();
        set_time(&source, 1000);
        assert_eq!(
            resolve(ForwardSource::default(), 6).unwrap_err().kind(),
            kind
        );
        let unsupported = resolve(context, 6).unwrap();
        assert!(unsupported.word.is_none());
        assert!(unsupported.warning.is_some());
        assert_eq!(
            (unsupported.page, unsupported.h, unsupported.v),
            (strict.page, strict.h, strict.v),
        );
    }
    let missing_snapshot = root.join("missing.synctex.sources.json");
    assert!(
        resolve(
            ForwardSource {
                snapshot: Some(&missing_snapshot),
                ..context
            },
            6,
        )
        .is_err()
    );
}

#[test]
fn strict_fixed_output_inspection_preserves_unrelated_words() {
    for body in ["{\\small #1}", "#1\\xspace", "\\LaTeX{} #1"] {
        let source = format!(
            "\\newcommand{{\\aside}}[1]{{{body}}}\n\
             \\begin{{frame}}\n\
             Quartz zephyr.\n\
             \\aside{{Copper}}\n\
             \\end{{frame}}"
        );
        assert_eq!(
            source_word_location_with_sources(
                &source,
                5,
                "Quartz zephyr.",
                0,
                10,
                &[],
                &Operation::default()
            )
            .map(WordOutcome::location),
            Ok(Some((3, 0))),
            "{body}",
        );
    }
}

#[test]
fn strict_fixed_output_nested_helpers_preserve_roots_and_original_owners() {
    for (source, anchor, context, offset, expected) in [
        (
            "\\newcommand{\\unused}{\\def\\pick{Amber}}\n\
             \\newcommand{\\pick}[1]{\\textbf{#1}}\n\
             \\newcommand{\\outer}[1]{\\pick{#1}}\n\
             \\begin{frame}\n\
             Quartz \\outer{Silver} zephyr.\n\
             Amber stays here.\n\
             \\end{frame}",
            7,
            "Amber stays here.",
            0,
            (6, 0),
        ),
        (
            "\\providecommand{\\inner}[1]{\\textbf{#1}}\n\
             \\newcommand{\\outer}[1]{\\inner{#1}}\n\
             \\begin{frame}\n\
             Quartz \\outer{Silver} zephyr.\n\
             \\end{frame}",
            5,
            "Quartz Silver zephyr.",
            7,
            (4, 14),
        ),
    ] {
        assert_eq!(
            source_word_location_with_sources(
                source,
                anchor,
                context,
                offset,
                10,
                &[],
                &Operation::default()
            )
            .map(WordOutcome::location),
            Ok(Some(expected)),
            "{source}",
        );
    }
}

#[test]
fn environment_helper_reachability_has_no_depth_cutoff_or_fanout_revisits() {
    let operation = Operation::default();
    for fanout in [1, 10] {
        let mut source = "\\newcommand{\\hA}{Amber}\n".to_string();
        for level in 1..10u8 {
            let name = char::from(b'A' + level);
            let previous = char::from(b'A' + level - 1);
            source.push_str(&format!(
                "\\newcommand{{\\h{name}}}{{{}}}\n",
                format!("\\h{previous} ").repeat(fanout)
            ));
        }
        source.push_str("\\newenvironment{wrap}{\\hJ}{}\n\\begin{frame}\n\
                         Quartz \\begin{wrap}\\end{wrap} zephyr.\nAmber stays here.\nCopper nimbus.\n\\end{frame}");
        let anchor = source.lines().count() as u32;
        assert_eq!(
            source_word_location_with_sources(
                &source,
                anchor,
                "Copper nimbus.",
                0,
                10,
                &[],
                &operation
            )
            .map(WordOutcome::location),
            Ok(Some((anchor - 1, 0))),
        );
        assert_eq!(
            source_word_location_with_sources(
                &source,
                anchor,
                "Quartz Amber zephyr.",
                7,
                10,
                &[],
                &operation
            )
            .map(WordOutcome::location)
            .unwrap_or(None),
            None,
        );
    }
}

#[test]
fn equation_helper_graph_reuses_root_context_and_preserves_literal_owner() {
    let mut source = "\\newcommand{\\eqA}{\\begin{equation}x\\end{equation}}\n".to_string();
    for level in 1..4u8 {
        let name = char::from(b'A' + level);
        let previous = char::from(b'A' + level - 1);
        source.push_str(&format!(
            "\\newcommand{{\\eq{name}}}{{{}}}\n",
            format!("\\eq{previous} ").repeat(10)
        ));
    }
    source.push_str(&"% unused context padding without an executed macro\n".repeat(22_000));
    source.push_str("\\begin{frame}\nQuartz \\eqD{} zephyr.\nCopper nimbus.\n\\end{frame}");
    let anchor = source.lines().count() as u32;
    assert_eq!(
        source_word_location_with_sources(
            &source,
            anchor,
            "Copper nimbus.",
            0,
            10,
            &[],
            &Operation::default()
        )
        .map(WordOutcome::location),
        Ok(Some((anchor - 1, 0))),
    );
}

#[test]
fn cancelled_source_refinement_does_not_return_a_literal_owner() {
    let operation = Operation::default();
    operation.cancel();
    assert_eq!(
        source_word_location_with_sources(
            "Copper nimbus.",
            1,
            "Copper nimbus.",
            0,
            1,
            &[],
            &operation
        )
        .map(WordOutcome::location),
        Err(()),
    );
    assert_eq!(
        tex::source_word_guess_with_sources(
            "Copper nimbus.",
            1,
            "Copper nimbus.",
            0,
            1,
            &[r"\csname\string x\endcsname"],
            &operation
        ),
        Err(()),
    );
}

#[test]
fn guess_context_prefix_requires_transparent_once_only_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let main = root.join("main.tex");
    let chapter = root.join("chapter.tex");
    let nested = root.join("nested.tex");
    let helper = root.join("helper.tex");
    fs::write(&chapter, "\\textit{Copper} and Amber literal.\n").unwrap();
    fs::write(&nested, "\\input{chapter}\n").unwrap();
    fs::write(&helper, "\\newcommand{\\Run}[3]{#3#1{#2}}\n").unwrap();
    for (label, preamble, body, prefix) in [
        ("plain", "", "\\input{chapter}", true),
        ("nested literal", "", "\\input{nested}", true),
        (
            "unverified package frontend reaching the input",
            "",
            "\\PackageFrontend\\input{chapter}",
            false,
        ),
        (
            "unverified package frontend beyond nine arguments",
            "",
            "\\PackageFrontend{a}{b}{c}{d}{e}{f}{g}{h}{i}\n\\input{chapter}",
            true,
        ),
        (
            "modeled literal argument",
            "\\newcommand{\\Run}[1]{#1}",
            "\\Run{Title}\\input{chapter}",
            true,
        ),
        (
            "unused imported local definition",
            "\\input{helper}",
            "\\input{chapter}",
            true,
        ),
        ("group", "", "{\\input{chapter}}", false),
        ("conditional", "", "\\iftrue\\input{chapter}\\fi", false),
        (
            "environment body",
            "",
            "\\begin{custom}\\input{chapter}\\end{custom}",
            false,
        ),
        (
            "braced reordered arguments",
            "\\newcommand{\\Run}[2]{#2#1}",
            "\\Run{\\input{chapter}}{\\Mutate}",
            false,
        ),
        (
            "unbraced reordered arguments",
            "\\newcommand{\\Run}[3]{#3#1{#2}}",
            "\\Run\\input{chapter}{\\Mutate}",
            false,
        ),
        (
            "nested parameter forwarding",
            "\\newcommand{\\Run}[1]{#1}\\newcommand{\\Reorder}[3]{#3#1{#2}}",
            "\\Run\\Reorder\\input{chapter}{\\Mutate}",
            false,
        ),
        (
            "tail forwarding",
            "\\newcommand{\\Run}{\\Reorder}\\newcommand{\\Reorder}[3]{#3#1{#2}}",
            "\\Run\\input{chapter}{\\Mutate}",
            false,
        ),
        (
            "local environment end",
            "\\newenvironment{Run}{}{\\Reorder}",
            "\\begin{Run}\\end{Run}\\input{chapter}{\\Mutate}",
            false,
        ),
        (
            "deferred arguments",
            "\\newcommand{\\Run}[3]{#3#1{#2}}",
            "\\afterassignment\\Run\\let\\Unused\\relax\\input{chapter}{\\Mutate}",
            false,
        ),
        (
            "imported local definition",
            "\\input{helper}",
            "\\Run\\input{chapter}{\\Mutate}",
            false,
        ),
        (
            "missing earlier input",
            "\\input{missing}",
            "\\input{chapter}",
            false,
        ),
        (
            "dynamic earlier input",
            "\\input{\\Unknown}",
            "\\input{chapter}",
            false,
        ),
        ("repeated", "", "\\input{chapter}\\input{chapter}", false),
        ("omitted", "", "\\subfile{chapter}", false),
    ] {
        fs::write(
            &main,
            format!(
                "\\documentclass{{article}}\n{preamble}\n\
                     \\begin{{document}}\n{body}\nLATER_SENTINEL\n\\end{{document}}\n"
            ),
        )
        .unwrap();
        let mut spliced = macro_preamble_source(
            &main,
            None,
            None,
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(spliced.inputs.len(), spliced.input_ends.len(), "{label}");
        let guessed = spliced.guess_source(&chapter);
        assert_eq!(guessed.len() < spliced.source.len(), prefix, "{label}");
        assert_eq!(guessed.as_ptr(), spliced.source.as_ptr(), "{label}");
        assert_eq!(!guessed.contains("LATER_SENTINEL"), prefix, "{label}");
        assert_eq!(spliced.guess_source(&main), spliced.source, "{label}");
        assert_eq!(
            spliced.guess_source(&root.join("absent.tex")),
            spliced.source,
            "{label}"
        );
        spliced.ambiguous_inputs = true;
        assert_eq!(spliced.guess_source(&chapter), spliced.source, "{label}");
    }
}

#[test]
fn guess_context_prefix_counts_unresolved_compiler_openings() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let main = root.join("main.tex");
    let chapter = root.join("chapter.tex");
    fs::write(&chapter, "Copper and Amber literal.\n").unwrap();
    fs::write(
        &main,
        "\\begin{document}\n\\input{chapter}\nLATER_SENTINEL\n\\end{document}\n",
    )
    .unwrap();
    for (compiled, prefix) in [
        (None, true),
        (Some(vec![]), false),
        (Some(vec![main.clone(), chapter.clone()]), true),
        (Some(vec![chapter.clone(), chapter.clone()]), false),
        (
            Some(vec![chapter.clone(), root.join("missing/chapter.tex")]),
            false,
        ),
        (
            Some(vec![chapter.clone(), root.join("missing/unrelated.tex")]),
            true,
        ),
    ] {
        let spliced = macro_preamble_source(
            &main,
            compiled.as_deref(),
            None,
            &mut QuerySources::default(),
            &Operation::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            !spliced.guess_source(&chapter).contains("LATER_SENTINEL"),
            prefix,
            "{compiled:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn ambiguous_inverse_keeps_first_compiler_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("paper.tex");
    let pdf = dir.path().join("paper.pdf");
    fs::write(&pdf, b"pdf-v1").unwrap();
    fs::write(
        pdf.with_extension("synctex"),
        format!(
            "SyncTeX Version:1\nInput:1:{}\nOutput:pdf\n\
             Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n\
             {{1\n[1,6:0,0:30000000,40000000,0\n\
             (1,3:655360,6553600:6553600,655360,0\nx1,3:1310720,6553600\n)\n]\n\
             }}1\nPostamble:\nCount:4\n",
            main.display(),
        ),
    )
    .unwrap();
    for prefix in ["\\textit{Copper} ", ""] {
        fs::write(
            &main,
            format!(
                "\\csname\\string x\\endcsname\n\\begin{{document}}\n\
                 {prefix}Amber literal.\n\n{prefix}Amber literal.\n\\end{{document}}\n",
            ),
        )
        .unwrap();
        for lines in [[3, 5, 3], [5, 3, 5]] {
            let output = format!(
                "Input:paper.tex\nLine:{}\nColumn:1\n\
                 Input:paper.tex\nLine:{}\nColumn:1\n\
                 Input:paper.tex\nLine:{}\nColumn:2\n",
                lines[0], lines[1], lines[2],
            );
            let result = resolve_inverse_output(
                &pdf,
                InversePoint {
                    page: 1,
                    x: 72.0,
                    y_from_top: 120.0,
                    page_height_pt: 792.0,
                },
                Some(("Amber literal.", 0)),
                0,
                &Operation::default(),
                &output,
                QuerySources {
                    best_guess: true,
                    ..QuerySources::default()
                },
            )
            .unwrap();
            assert_eq!(
                (result.location.line, result.location.byte_column),
                (lines[0], 0)
            );
            assert!(!result.location.precise);
            assert!(result.warning.is_some());
        }
    }
}

#[test]
fn clearpage_bounds_footnote_reference_competitors_to_same_page_segment() {
    let source = concat!(
        "Endfn.\\footnote{One two three see \\cref{sec:first}.}\\clearpage\n",
        "\n",
        "Page five text.\\footnote{Note see \\cref{sec:first} for details.}\\par\\clearpage\n",
        "\n",
        "See \\cite{kpar} for details.\\footnote{Note see \\cref{sec:first} for details.} Following material.\n",
        "\\begin{itemize}\n",
        "\\item See \\cite{kitm} for details.\\footnote{Note see \\cref{sec:first} for details.} Following material.\n",
        "\\end{itemize}\n",
    );
    let fn2_line = 3u32;
    let fn2_sec_byte = source
        .lines()
        .nth(fn2_line as usize - 1)
        .unwrap()
        .find("sec:first")
        .unwrap();
    let fn2_ctx = "1Note see section 1 for details.";
    let fn2_off = fn2_ctx.find("section").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            source,
            fn2_line,
            fn2_ctx,
            fn2_off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(Some((fn2_line, fn2_sec_byte)))
    );
    assert_eq!(
        source_word_guess_with_sources(
            source,
            fn2_line,
            fn2_ctx,
            fn2_off,
            1,
            &[],
            &Operation::default(),
        ),
        Ok(Some(UnverifiedWordLocation {
            line: fn2_line,
            byte: fn2_sec_byte,
        }))
    );

    let cite_ctx = "See [73] for details.1 Following material.";
    let cite_off = cite_ctx.find("73").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            source,
            5,
            cite_ctx,
            cite_off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(None)
    );
    assert!(matches!(
        source_word_guess_with_sources(
            source,
            5,
            cite_ctx,
            cite_off,
            1,
            &[],
            &Operation::default(),
        ),
        Ok(None) | Err(())
    ));
}

#[test]
fn beamer_frame_headline_and_footline_furniture_do_not_steal_edge_references() {
    let footline_last = concat!(
        "\\documentclass{beamer}\n",
        "\\setbeamertemplate{footline}[frame number]\n",
        "\\begin{document}\n",
        "\\setcounter{section}{70}\\refstepcounter{section}\\label{sec:frame}\n",
        "\\begin{frame}\n",
        "Top lead.\n",
        "\\begin{itemize}\n",
        "\\item See \\ref{sec:frame}\n",
        "\\end{itemize}\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    let foot_ctx = "Top lead. See 71 1 / 1";
    let foot_off = foot_ctx.rfind("1 / 1").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            footline_last,
            10,
            foot_ctx,
            foot_off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(None)
    );
    assert!(matches!(
        source_word_guess_with_sources(
            footline_last,
            10,
            foot_ctx,
            foot_off,
            1,
            &[],
            &Operation::default(),
        ),
        Ok(None) | Err(())
    ));

    let footline_first = concat!(
        "\\documentclass{beamer}\n",
        "\\setbeamertemplate{footline}[frame number]\n",
        "\\begin{document}\n",
        "\\setcounter{section}{70}\\refstepcounter{section}\\label{sec:frame}\n",
        "\\begin{frame}\n",
        "\\ref{sec:frame} done.\n",
        "Bottom tail.\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    let foot_first_ctx = "71 done. Bottom tail. 1 / 1";
    let foot_first_off = foot_first_ctx.rfind("1 / 1").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            footline_first,
            8,
            foot_first_ctx,
            foot_first_off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(None)
    );
    assert!(matches!(
        source_word_guess_with_sources(
            footline_first,
            8,
            foot_first_ctx,
            foot_first_off,
            1,
            &[],
            &Operation::default(),
        ),
        Ok(None) | Err(())
    ));

    let headline_first = concat!(
        "\\documentclass{beamer}\n",
        "\\setbeamertemplate{headline}{Headerword}\n",
        "\\begin{document}\n",
        "\\setcounter{section}{70}\\refstepcounter{section}\\label{sec:frame}\n",
        "\\begin{frame}\n",
        "\\ref{sec:frame} done.\n",
        "Bottom tail.\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    let head_ctx = "Headerword 71 done. Bottom tail.";
    let head_off = head_ctx.find("Headerword").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            headline_first,
            8,
            head_ctx,
            head_off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(None)
    );
    assert!(matches!(
        source_word_guess_with_sources(
            headline_first,
            8,
            head_ctx,
            head_off,
            1,
            &[],
            &Operation::default(),
        ),
        Ok(None) | Err(())
    ));

    let headline_last = concat!(
        "\\documentclass{beamer}\n",
        "\\setbeamertemplate{headline}{Headerword}\n",
        "\\begin{document}\n",
        "\\setcounter{section}{70}\\refstepcounter{section}\\label{sec:frame}\n",
        "\\begin{frame}\n",
        "Top lead.\n",
        "\\begin{itemize}\n",
        "\\item See \\ref{sec:frame}\n",
        "\\end{itemize}\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    let head_last_ctx = "Headerword Top lead. See 71";
    let head_last_off = head_last_ctx.find("Headerword").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            headline_last,
            10,
            head_last_ctx,
            head_last_off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(None)
    );
    assert!(matches!(
        source_word_guess_with_sources(
            headline_last,
            10,
            head_last_ctx,
            head_last_off,
            1,
            &[],
            &Operation::default(),
        ),
        Ok(None) | Err(())
    ));

    let pause_footline = concat!(
        "\\documentclass{beamer}\n",
        "\\setbeamertemplate{navigation symbols}{}\n",
        "\\setbeamertemplate{footline}[frame number]\n",
        "\\begin{document}\n",
        "\\setcounter{section}{70}\\refstepcounter{section}\\label{sec:frame}\n",
        "\\begin{frame}\n",
        "\\begin{itemize}\n",
        "\\item See \\ref{sec:frame}\\pause then.\n",
        "\\end{itemize}\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    for pause_ctx in ["See 71 1 / 1", "See 71then. 1 / 1"] {
        for pause_off in [pause_ctx.find("1 / 1").unwrap(), pause_ctx.rfind('1').unwrap()] {
            assert_eq!(
                source_word_location_with_sources(
                    pause_footline,
                    10,
                    pause_ctx,
                    pause_off,
                    1,
                    &[],
                    &Operation::default(),
                )
                .map(WordOutcome::location),
                Ok(None),
                "pause_ctx={pause_ctx:?} pause_off={pause_off}"
            );
            assert!(
                matches!(
                    source_word_guess_with_sources(
                        pause_footline,
                        10,
                        pause_ctx,
                        pause_off,
                        1,
                        &[],
                        &Operation::default(),
                    ),
                    Ok(None) | Err(())
                ),
                "pause_ctx={pause_ctx:?} pause_off={pause_off}"
            );
        }
    }

    let flanked_frame = concat!(
        "\\documentclass{beamer}\n",
        "\\begin{document}\n",
        "\\setcounter{section}{70}\\refstepcounter{section}\\label{sec:frame}\n",
        "\\begin{frame}\n",
        "Unique \\ref{sec:frame} here.\n",
        "\\end{frame}\n",
        "\\end{document}\n",
    );
    let pos_ctx = "Headerword Unique 71 here. 1 / 1";
    let pos_off = pos_ctx.find("71").unwrap();
    let expected_byte = flanked_frame
        .lines()
        .nth(4)
        .unwrap()
        .find("sec:frame")
        .unwrap();
    assert_eq!(
        source_word_location_with_sources(
            flanked_frame,
            6,
            pos_ctx,
            pos_off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(Some((5, expected_byte)))
    );
}

#[test]
fn soft_or_opaque_pagebreaks_do_not_slice_away_reference_competitors() {
    for sep in [
        "\\pagebreak[0]",
        "\\newpage",
        "\\verb|\\clearpage|",
        "\\item[\\clearpage]",
    ] {
        let same_line = format!(
            "\\begin{{itemize}}\\item See \\ref{{sec:a}} for details. {sep} \\item See \\cite{{k}} for details.\\end{{itemize}}\n"
        );
        let ctx = "See 74 for details.";
        let off = ctx.find("74").unwrap();
        assert_eq!(
            source_word_location_with_sources(
                &same_line,
                1,
                ctx,
                off,
                1,
                &[],
                &Operation::default(),
            )
            .map(WordOutcome::location),
            Ok(None),
            "sep={sep}"
        );
        assert!(
            matches!(
                source_word_guess_with_sources(
                    &same_line,
                    1,
                    ctx,
                    off,
                    1,
                    &[],
                    &Operation::default(),
                ),
                Ok(None) | Err(())
            ),
            "sep={sep}"
        );

        let in_frame = format!(
            "\\begin{{frame}}\n\\begin{{itemize}}\n\\item See \\ref{{sec:a}} for details.\n{sep}\n\\item See \\cite{{k}} for details.\n\\end{{itemize}}\n\\end{{frame}}\n"
        );
        assert_eq!(
            source_word_location_with_sources(
                &in_frame,
                7,
                ctx,
                off,
                1,
                &[],
                &Operation::default(),
            )
            .map(WordOutcome::location),
            Ok(None),
            "frame sep={sep}"
        );
        assert!(
            matches!(
                source_word_guess_with_sources(
                    &in_frame,
                    7,
                    ctx,
                    off,
                    1,
                    &[],
                    &Operation::default(),
                ),
                Ok(None) | Err(())
            ),
            "frame sep={sep}"
        );
    }

    let overridden_clearpage = concat!(
        "\\renewcommand{\\clearpage}{}\n",
        "\\begin{document}\n",
        "\\begin{itemize}\\item See \\ref{sec:a} for details. \\clearpage \\item See \\cite{k} for details.\\end{itemize}\n",
        "\\end{document}\n",
    );
    let ctx = "See 74 for details.";
    let off = ctx.find("74").unwrap();
    assert_eq!(
        source_word_location_with_sources(
            overridden_clearpage,
            3,
            ctx,
            off,
            1,
            &[],
            &Operation::default(),
        )
        .map(WordOutcome::location),
        Ok(None)
    );
    assert!(matches!(
        source_word_guess_with_sources(
            overridden_clearpage,
            3,
            ctx,
            off,
            1,
            &[],
            &Operation::default(),
        ),
        Ok(None) | Err(())
    ));
}
