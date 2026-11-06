use std::{cell::Cell, rc::Rc};

use granit_parser::{
    BorrowedInput, BufferedInput, ErrorKind, Event, FallibleBufferedInput, Input, Options, Parser,
    ScalarStyle, ScanError, Scanner, Span, StrInput, StructureStyle, Token, TokenType,
};

fn go_options() -> Options {
    granit_parser::options! {
        go_compatible_anchors: true,
        emit_anchor_events: true,
        emit_comments: false,
    }
}

fn trace_all_inputs(source: &str, options: Options) -> Vec<Result<(Event<'_>, Span), ScanError>> {
    let mut parser = Parser::new_from_str_with_options(source, options.clone());
    let trace = parser.by_ref().collect::<Vec<_>>();
    assert!(parser.next().is_none());
    assert_eq!(
        trace,
        Parser::new_from_iter_with_options(source.chars(), options.clone()).collect::<Vec<_>>(),
        "{source:?}"
    );
    assert_eq!(
        trace,
        Parser::new_from_fallible_iter_with_options(source.chars().map(Ok), options)
            .collect::<Vec<_>>(),
        "{source:?}"
    );
    trace
}

fn parse_all_inputs(source: &str, options: Options) -> Vec<(Event<'_>, Span)> {
    trace_all_inputs(source, options)
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap_or_else(|error| panic!("failed to parse {source:?}: {error:?}"))
}

fn scan_all_inputs(source: &str, options: Options) -> Vec<Result<Token<'_>, ScanError>> {
    let mut scanner = Scanner::with_options(StrInput::new(source), options.clone());
    let trace = scanner.by_ref().collect::<Vec<_>>();
    assert!(scanner.next().is_none());
    assert_eq!(
        trace,
        Scanner::with_options(BufferedInput::new(source.chars()), options.clone())
            .collect::<Vec<_>>(),
        "{source:?}"
    );
    assert_eq!(
        trace,
        Scanner::with_options(FallibleBufferedInput::new(source.chars().map(Ok)), options,)
            .collect::<Vec<_>>(),
        "{source:?}"
    );
    trace
}

fn plain(value: &str) -> Event<'_> {
    Event::Scalar(value.into(), ScalarStyle::Plain, 0, None)
}

fn assert_token_span(span: Span, source: &str, start: usize, token: &str) {
    assert_eq!(span.slice(source), Some(token));
    assert_eq!(span.byte_range(), Some(start..start + token.len()));
    assert_eq!(span.start.index(), source[..start].chars().count());
    assert_eq!(
        span.end.index(),
        source[..start + token.len()].chars().count()
    );
    assert_eq!(span.start.line(), source[..start].matches('\n').count() + 1);
    assert_eq!(
        span.start.col(),
        source[..start].rsplit('\n').next().unwrap().chars().count()
    );
}

#[test]
fn go_mode_resolves_a_compact_quoted_alias_key_without_rewriting() {
    let source = "a: &a k\nb: {*a:\"short\"}\n";
    let result =
        Parser::new_from_str_with_options(source, go_options()).collect::<Result<Vec<_>, _>>();
    assert!(
        result.is_ok(),
        "the enabled lexical mode must resolve the compact alias key: {result:?}"
    );
    let events = result.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|(event, _)| event.clone())
            .collect::<Vec<_>>(),
        [
            Event::StreamStart,
            Event::DocumentStart(false, None),
            Event::MappingStart(StructureStyle::Block, 0, None),
            plain("a"),
            Event::Scalar("k".into(), ScalarStyle::Plain, 1, None),
            Event::Anchor(1, "a".into()),
            plain("b"),
            Event::MappingStart(StructureStyle::Flow, 0, None),
            Event::Alias(1),
            Event::Scalar("short".into(), ScalarStyle::DoubleQuoted, 0, None),
            Event::MappingEnd,
            Event::MappingEnd,
            Event::DocumentEnd,
            Event::StreamEnd,
        ]
    );
    assert_token_span(events[5].1, source, 3, "&a");
    assert_token_span(events[8].1, source, 12, "*a");
    assert_token_span(events[9].1, source, 15, "\"short\"");
}

#[test]
fn scanner_splits_flow_alias_keys_and_preserves_default_yaml12_names() {
    let source = "a: &a k\nb: {*a:\"short\"}\n";
    let tokens = scan_all_inputs(source, go_options())
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        tokens
            .iter()
            .map(|token| token.token_type().clone())
            .collect::<Vec<_>>(),
        [
            TokenType::StreamStart,
            TokenType::BlockMappingStart,
            TokenType::Key,
            TokenType::Scalar(ScalarStyle::Plain, "a".into()),
            TokenType::Value,
            TokenType::Anchor("a".into()),
            TokenType::Scalar(ScalarStyle::Plain, "k".into()),
            TokenType::Key,
            TokenType::Scalar(ScalarStyle::Plain, "b".into()),
            TokenType::Value,
            TokenType::FlowMappingStart,
            TokenType::Key,
            TokenType::Alias("a".into()),
            TokenType::Value,
            TokenType::Scalar(ScalarStyle::DoubleQuoted, "short".into()),
            TokenType::FlowMappingEnd,
            TokenType::BlockEnd,
            TokenType::StreamEnd,
        ]
    );
    assert_token_span(tokens[5].span(), source, 3, "&a");
    assert_token_span(tokens[12].span(), source, 12, "*a");
    assert_eq!(tokens[13].span().start.byte_offset(), Some(14));
    assert_token_span(tokens[14].span(), source, 15, "\"short\"");

    let default = scan_all_inputs(source, Options::default());
    assert_eq!(
        default,
        Scanner::new(StrInput::new(source)).collect::<Vec<_>>()
    );
    assert_eq!(
        default,
        scan_all_inputs(
            source,
            granit_parser::options! { go_compatible_anchors: false },
        )
    );
    let tokens = default.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    let alias = tokens
        .iter()
        .find(|token| matches!(token.token_type(), TokenType::Alias(_)))
        .unwrap();
    assert_eq!(alias.token_type(), &TokenType::Alias("a:\"short\"".into()));
    assert_token_span(alias.span(), source, 12, "*a:\"short\"");
}

#[test]
fn compact_flow_alias_values_cover_scalars_tags_and_collections() {
    for value in [
        "\"short\"",
        "'short'",
        "short",
        "1",
        "!!str \"short\"",
        "[short]",
        "{description: short}",
    ] {
        for sequence in [false, true] {
            let pair = format!("*a:{value}");
            let collection = if sequence {
                format!("[{pair}]")
            } else {
                format!("{{{pair}}}")
            };
            let source = format!("a: &a k\nb: {collection}\n");
            let events = parse_all_inputs(&source, go_options());
            let alias_index = events
                .iter()
                .position(|(event, _)| matches!(event, Event::Alias(1)))
                .unwrap();
            let mut expected_prefix = vec![
                Event::StreamStart,
                Event::DocumentStart(false, None),
                Event::MappingStart(StructureStyle::Block, 0, None),
                plain("a"),
                Event::Scalar("k".into(), ScalarStyle::Plain, 1, None),
                Event::Anchor(1, "a".into()),
                plain("b"),
            ];
            if sequence {
                expected_prefix.push(Event::SequenceStart(StructureStyle::Flow, 0, None));
            }
            expected_prefix.extend([
                Event::MappingStart(StructureStyle::Flow, 0, None),
                Event::Alias(1),
            ]);
            assert_eq!(
                events[..=alias_index]
                    .iter()
                    .map(|(event, _)| event.clone())
                    .collect::<Vec<_>>(),
                expected_prefix,
                "{source:?}"
            );
            assert_token_span(
                events[alias_index].1,
                &source,
                source.find("*a:").unwrap(),
                "*a",
            );

            let tail = &events[alias_index + 1..];
            let value_length = match value {
                "[short]" => {
                    assert_eq!(
                        tail[..3]
                            .iter()
                            .map(|(event, _)| event.clone())
                            .collect::<Vec<_>>(),
                        [
                            Event::SequenceStart(StructureStyle::Flow, 0, None),
                            plain("short"),
                            Event::SequenceEnd,
                        ]
                    );
                    3
                }
                "{description: short}" => {
                    assert_eq!(
                        tail[..4]
                            .iter()
                            .map(|(event, _)| event.clone())
                            .collect::<Vec<_>>(),
                        [
                            Event::MappingStart(StructureStyle::Flow, 0, None),
                            plain("description"),
                            plain("short"),
                            Event::MappingEnd,
                        ]
                    );
                    4
                }
                _ => {
                    let Event::Scalar(text, style, 0, tag) = &tail[0].0 else {
                        panic!("expected a scalar value in {source:?}");
                    };
                    assert_eq!(text, if value == "1" { "1" } else { "short" });
                    assert_eq!(
                        *style,
                        match value {
                            "\"short\"" | "!!str \"short\"" => ScalarStyle::DoubleQuoted,
                            "'short'" => ScalarStyle::SingleQuoted,
                            _ => ScalarStyle::Plain,
                        }
                    );
                    if value.starts_with("!!") {
                        assert!(tag.as_ref().unwrap().is_yaml_core_schema_tag("str"));
                        assert_eq!(tail[0].1.tag_span().unwrap().slice(&source), Some("!!str"));
                    } else {
                        assert_eq!(tag, &None);
                    }
                    1
                }
            };
            let mut expected_suffix = vec![Event::MappingEnd];
            if sequence {
                expected_suffix.push(Event::SequenceEnd);
            }
            expected_suffix.extend([Event::MappingEnd, Event::DocumentEnd, Event::StreamEnd]);
            assert_eq!(
                tail[value_length..]
                    .iter()
                    .map(|(event, _)| event.clone())
                    .collect::<Vec<_>>(),
                expected_suffix,
                "{source:?}"
            );
            let tokens = scan_all_inputs(&source, go_options())
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let index = tokens
                .iter()
                .position(|token| matches!(token.token_type(), TokenType::Alias(_)))
                .unwrap();
            assert_eq!(tokens[index].token_type(), &TokenType::Alias("a".into()));
            assert_eq!(tokens[index + 1].token_type(), &TokenType::Value);
        }
    }
}

#[test]
fn alias_value_positions_and_compact_block_forms_remain_invalid() {
    for value in [
        "\"short\"",
        "'short'",
        "short",
        "1",
        "!!str \"short\"",
        "[short]",
        "{description: short}",
    ] {
        for suffix in [
            format!("b: {{\"x\": *a:{value}}}\n"),
            format!("*a:{value}\n"),
            format!("b: *a:{value}\n"),
        ] {
            let source = format!("a: &a k\n{suffix}");
            for emit_unresolved_aliases in [false, true] {
                let mut options = go_options();
                options.emit_unresolved_aliases = emit_unresolved_aliases;
                let trace = trace_all_inputs(&source, options);
                assert!(trace.iter().any(Result::is_err), "{source:?}");
                assert_eq!(
                    trace
                        .iter()
                        .filter(|entry| matches!(entry, Ok((Event::DocumentEnd, _))))
                        .count(),
                    0,
                    "{source:?}"
                );
            }
        }
    }
}

#[test]
fn separated_aliases_and_flow_key_whitespace_keep_their_structure() {
    for suffix in [
        "*a : short\n",
        "b: *a\n",
        "b: {*a : short}\n",
        "b: {*a :\"short\"}\n",
        "b: [*a :\"short\"]\n",
        "b: {x: *a}\n",
        "b: [*a]\n",
        "? *a\n: short\n",
        "b: {? *a : short}\n",
    ] {
        let source = format!("a: &a k\n{suffix}");
        let events = parse_all_inputs(&source, go_options());
        let aliases = events
            .iter()
            .filter(|(event, _)| event.is_alias())
            .collect::<Vec<_>>();
        assert_eq!(aliases.len(), 1, "{source:?}");
        assert_eq!(aliases[0].0, Event::Alias(1));
        assert_token_span(aliases[0].1, &source, source.find("*a").unwrap(), "*a");
        assert!(matches!(events.last(), Some((Event::StreamEnd, _))));
    }
}

#[test]
fn ascii_anchor_and_alias_names_share_the_same_enabled_grammar() {
    for name in [
        "A0_z-9",
        "9",
        "_",
        "-",
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-",
    ] {
        let source = format!("a: &{name} k\nb: {{*{name}:\"short\"}}\n");
        let events = parse_all_inputs(&source, go_options());
        let (anchor, anchor_span) = events
            .iter()
            .find(|(event, _)| matches!(event, Event::Anchor(..)))
            .unwrap();
        assert_eq!(*anchor, Event::Anchor(1, name.into()));
        assert_token_span(*anchor_span, &source, 3, &format!("&{name}"));
        let (_, alias_span) = events.iter().find(|(event, _)| event.is_alias()).unwrap();
        assert_token_span(
            *alias_span,
            &source,
            source.find('*').unwrap(),
            &format!("*{name}"),
        );
    }
}

#[test]
fn colon_after_a_go_anchor_begins_scalar_content_not_its_name() {
    let source = "a: &a:short harmless\nb: *a\n";
    let events = parse_all_inputs(source, go_options());
    assert!(events.iter().any(|(event, span)| {
        *event == Event::Anchor(1, "a".into()) && span.slice(source) == Some("&a")
    }));
    assert!(events.iter().any(|(event, span)| {
        *event == Event::Scalar(":short harmless".into(), ScalarStyle::Plain, 1, None)
            && span.slice(source) == Some(":short harmless")
    }));
    assert!(events
        .iter()
        .any(|(event, span)| { *event == Event::Alias(1) && span.slice(source) == Some("*a") }));
    let tokens = scan_all_inputs(source, go_options())
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(tokens.iter().any(|token| {
        token.token_type() == &TokenType::Anchor("a".into())
            && token.span().slice(source) == Some("&a")
    }));
    assert!(tokens.iter().any(|token| {
        token.token_type() == &TokenType::Scalar(ScalarStyle::Plain, ":short harmless".into())
            && token.span().slice(source) == Some(":short harmless")
    }));
    let defaults = trace_all_inputs(source, Options::default());
    assert_eq!(
        defaults.last().unwrap().as_ref().unwrap_err().kind(),
        &ErrorKind::UnknownAnchor
    );
}

#[test]
fn real_alias_and_anchor_spans_survive_unicode_prefixes_and_crlf() {
    let source = "note: 'é🦀'\r\na: &a k\r\nb: {*a:\"short\"}\r\n";
    let events = parse_all_inputs(source, go_options());
    for (event, span) in &events {
        if let Event::Anchor(1, _) = event {
            assert_token_span(*span, source, source.find("&a").unwrap(), "&a");
        } else if let Event::Alias(1) = event {
            assert_token_span(*span, source, source.find("*a:").unwrap(), "*a");
        }
    }
    assert_eq!(
        events.iter().filter(|(event, _)| event.is_alias()).count(),
        1
    );
}

#[test]
fn omitted_and_disabled_mode_preserve_legal_yaml12_anchor_names() {
    let source = "[&a:b first, *a:b, &a\"q second, *a\"q, &é third, *é]\n";
    let omitted = granit_parser::options! { emit_anchor_events: true };
    let explicit = granit_parser::options! {
        emit_anchor_events: true,
        go_compatible_anchors: false,
    };
    let events = parse_all_inputs(source, omitted.clone());
    assert_eq!(events, parse_all_inputs(source, explicit.clone()));
    assert_eq!(
        scan_all_inputs(source, omitted),
        scan_all_inputs(source, explicit)
    );
    assert_eq!(
        events
            .iter()
            .filter_map(|(event, _)| {
                if let Event::Anchor(id, name) = event {
                    Some((*id, name.as_ref()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>(),
        [(1, "a:b"), (2, "a\"q"), (3, "é")]
    );
    assert_eq!(
        events
            .iter()
            .filter_map(|(event, _)| event.alias_id())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let compact = "a: &a k\nb: {*a:\"short\"}\n";
    let ordinary = Parser::new_from_str(compact).collect::<Vec<_>>();
    for options in [
        Options::default(),
        granit_parser::options! { go_compatible_anchors: false },
    ] {
        let trace = trace_all_inputs(compact, options);
        assert_eq!(trace, ordinary);
        assert_eq!(
            trace.last().unwrap().as_ref().unwrap_err().kind(),
            &ErrorKind::UnknownAnchor
        );
    }
}

#[test]
fn malformed_names_fail_even_when_unresolved_alias_emission_is_enabled() {
    for source in [
        "[& value]\n",
        "[*]\n",
        "[&é value]\n",
        "[*é]\n",
        "a: &α harmless\nb: *α\n",
        "[&a.b value]\n",
        "[*a.b]\n",
        "[&a!tag value]\n",
        "[*a!tag]\n",
        "[&a\"quote value]\n",
        "[*a\"quote]\n",
        "[&a[short]]\n",
        "[*a#not-separated]\n",
    ] {
        for emit_unresolved_aliases in [false, true] {
            let mut options = go_options();
            options.emit_unresolved_aliases = emit_unresolved_aliases;
            let tokens = scan_all_inputs(source, options.clone());
            let error = tokens.last().unwrap().as_ref().unwrap_err();
            assert!(
                matches!(
                    error.kind(),
                    ErrorKind::MissingAnchorOrAliasName | ErrorKind::UnexpectedCharacter { .. }
                ),
                "{source:?}: {error:?}"
            );
            let trace = trace_all_inputs(source, options);
            assert!(trace.last().unwrap().is_err(), "{source:?}");
            assert!(!trace
                .iter()
                .any(|entry| matches!(entry, Ok((Event::DocumentEnd, _)))));
        }
    }
}

#[test]
fn compact_values_keep_inner_alias_lookalikes_and_raw_spans() {
    for (raw, decoded, style) in [
        ("\"*b:short\"", "*b:short", ScalarStyle::DoubleQuoted),
        (
            "\"literal *b:short [x y] suffix\"",
            "literal *b:short [x y] suffix",
            ScalarStyle::DoubleQuoted,
        ),
        (
            "'literal *b:short {x y}'",
            "literal *b:short {x y}",
            ScalarStyle::SingleQuoted,
        ),
        (
            "'literal ''*b:short'' [x y]'",
            "literal '*b:short' [x y]",
            ScalarStyle::SingleQuoted,
        ),
        (
            "\"é🦀 \\\"*b:short\\\" [x y]\\n\"",
            "é🦀 \"*b:short\" [x y]\n",
            ScalarStyle::DoubleQuoted,
        ),
        ("prefix*b:short", "prefix*b:short", ScalarStyle::Plain),
        (
            "prefix*b:short suffix",
            "prefix*b:short suffix",
            ScalarStyle::Plain,
        ),
        (
            "hello *b:short suffix",
            "hello *b:short suffix",
            ScalarStyle::Plain,
        ),
    ] {
        for sequence in [false, true] {
            let pair = format!("*a:{raw}");
            let collection = if sequence {
                format!("[{pair}]")
            } else {
                format!("{{{pair}}}")
            };
            let source = format!("a: &a k\nb: {collection}\n");
            let events = parse_all_inputs(&source, go_options());
            let index = events
                .iter()
                .position(|(event, _)| event.is_alias())
                .unwrap();
            assert_eq!(events[index].0, Event::Alias(1));
            assert_eq!(
                events[index + 1].0,
                Event::Scalar(decoded.into(), style, 0, None)
            );
            let start = source.find(raw).unwrap();
            assert_token_span(events[index + 1].1, &source, start, raw);
            assert_eq!(
                events.iter().filter(|(event, _)| event.is_alias()).count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|(event, _)| matches!(event, Event::Anchor(..)))
                    .count(),
                1
            );
            let tokens = scan_all_inputs(&source, go_options())
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .filter(|token| matches!(token.token_type(), TokenType::Alias(_)))
                    .count(),
                1
            );
            let value = tokens
                .iter()
                .find(|token| matches!(token.token_type(), TokenType::Scalar(_, text) if text == decoded))
                .unwrap();
            assert_eq!(
                value.token_type(),
                &TokenType::Scalar(style, decoded.into())
            );
            assert_token_span(value.span(), &source, start, raw);
        }
    }
}

#[test]
fn quoted_trailing_spaces_survive_a_later_compact_collection_value() {
    for (raw, style) in [
        ("\"hello *b:short [x] {y} \"", ScalarStyle::DoubleQuoted),
        ("'hello *b:short [x] {y} '", ScalarStyle::SingleQuoted),
    ] {
        let source = format!("a: &a k\nb: {{*a:{raw},*a:[short]}}\n");
        let events = parse_all_inputs(&source, go_options());
        let index = events
            .iter()
            .position(|(event, _)| event.is_alias())
            .unwrap();
        assert_eq!(
            events[index + 1].0,
            Event::Scalar("hello *b:short [x] {y} ".into(), style, 0, None)
        );
        assert_token_span(events[index + 1].1, &source, source.find(raw).unwrap(), raw);
        let aliases = events
            .iter()
            .filter(|(event, _)| event.is_alias())
            .collect::<Vec<_>>();
        assert_eq!(aliases.len(), 2);
        assert!(aliases.iter().all(|(event, _)| *event == Event::Alias(1)));
        assert_eq!(
            events[events.len() - 7].0,
            Event::SequenceStart(StructureStyle::Flow, 0, None)
        );
        assert_eq!(events[events.len() - 6].0, plain("short"));
        let tokens = scan_all_inputs(&source, go_options())
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            tokens
                .iter()
                .filter(|token| matches!(token.token_type(), TokenType::Alias(_)))
                .count(),
            2
        );
    }
}

#[test]
fn scalar_comment_tag_and_directive_tokens_are_identical_in_all_modes() {
    for source in [
        "a:&a harmless\n",
        "literal: {\"x\":\"*a:short [x y]\", 'y':'*b:short {x y}', z: prefix*b:short suffix}\n",
        "a: &a k\nb: [*a # *b:short [x]\n, short]\n",
        "%TAG !e! tag:*a:[short]\n---\na: !e!thing value\nb: !<tag:*a:[short]> value\n",
        concat!(
            "%TAG !e! tag:*a:short\n%FUTURE *a:short [x]\n---\n",
            "a: &a k\nb: {*a : short}\n",
            "verbatim: !<tag:*a:short> value\nshorthand: !e!thing value\n",
            "local: !literal*a:short value\n",
            "quoted: \"a:&a harmless *b:short [x y]\"\n",
            "single: '*a:short'\nplain: prefix*a:short suffix\n",
            "literal: |\n  *a:\"short\" [x y]\n",
            "folded: >\n  *a:short\n  [x y]\n",
            "# *a:\"short\" &fake [x y]\n",
        ),
    ] {
        for emit_comments in [false, true] {
            let omitted = granit_parser::options! {
                emit_anchor_events: true,
                emit_comments: emit_comments,
            };
            let mut disabled = omitted.clone();
            disabled.go_compatible_anchors = false;
            let mut enabled = omitted.clone();
            enabled.go_compatible_anchors = true;
            let events = parse_all_inputs(source, omitted.clone());
            assert_eq!(
                events,
                parse_all_inputs(source, disabled.clone()),
                "{source:?}"
            );
            assert_eq!(
                events,
                parse_all_inputs(source, enabled.clone()),
                "{source:?}"
            );
            let tokens = scan_all_inputs(source, omitted);
            assert_eq!(tokens, scan_all_inputs(source, disabled), "{source:?}");
            assert_eq!(tokens, scan_all_inputs(source, enabled), "{source:?}");
        }
    }
}

#[test]
fn tag_uris_remain_intact_next_to_real_compact_alias_keys() {
    for (prefix, anchor_tag, value_tag, raw_tag) in [
        ("%TAG !e! tag:*a:short\n---\n", "", "!e!thing ", "!e!thing"),
        (
            "%TAG !e! tag:*a:[short]\n---\n",
            "",
            "!e!thing ",
            "!e!thing",
        ),
        ("", "!<tag:*a:short> ", "", "!<tag:*a:short>"),
        ("", "!<tag:*a:[short]> ", "", "!<tag:*a:[short]>"),
        ("", "", "!<tag:*b:short> ", "!<tag:*b:short>"),
        ("", "", "!literal*b:short ", "!literal*b:short"),
    ] {
        let source = format!(
            "{prefix}a: {anchor_tag}&a k\nb: {{*a:{value_tag}\"literal *b:short [x y]\"}}\n"
        );
        let events = parse_all_inputs(&source, go_options());
        let (_, tag_span) = events
            .iter()
            .find(|(event, _)| event.tag().is_some())
            .unwrap();
        assert_token_span(
            tag_span.tag_span().unwrap(),
            &source,
            source.find(raw_tag).unwrap(),
            raw_tag,
        );
        assert_eq!(
            events.iter().filter(|(event, _)| event.is_alias()).count(),
            1
        );
        assert!(events
            .iter()
            .any(|(event, span)| *event == Event::Alias(1) && span.slice(&source) == Some("*a")));
        let (_, value_span) = events.iter().find(|(event, _)| matches!(event, Event::Scalar(value, ..) if value == "literal *b:short [x y]")).unwrap();
        assert_eq!(
            value_span.slice(&source),
            Some("\"literal *b:short [x y]\"")
        );
        let tokens = scan_all_inputs(&source, go_options())
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        if !prefix.is_empty() {
            let expected_suffix = if prefix.contains('[') {
                "tag:*a:[short]"
            } else {
                "tag:*a:short"
            };
            assert!(tokens.iter().any(|token| matches!(token.token_type(), TokenType::TagDirective(handle, suffix) if handle == "!e!" && suffix == expected_suffix)));
        }
    }
}

#[test]
fn unknown_compact_aliases_require_the_existing_unresolved_option() {
    let source = "b: {*missing:\"short\"}\n";
    for emit_unresolved_aliases in [None, Some(false)] {
        let mut options = go_options();
        if let Some(enabled) = emit_unresolved_aliases {
            options.emit_unresolved_aliases = enabled;
        }
        let trace = trace_all_inputs(source, options);
        let error = trace.last().unwrap().as_ref().unwrap_err();
        assert_eq!(error.kind(), &ErrorKind::UnknownAnchor);
        assert_eq!(error.marker().byte_offset(), Some(4));
        assert!(!trace
            .iter()
            .any(|entry| matches!(entry, Ok((Event::DocumentEnd, _)))));
    }
    let mut options = go_options();
    options.emit_unresolved_aliases = true;
    let events = parse_all_inputs(source, options.clone());
    assert_eq!(
        events
            .iter()
            .map(|(event, _)| event.clone())
            .collect::<Vec<_>>(),
        [
            Event::StreamStart,
            Event::DocumentStart(false, None),
            Event::MappingStart(StructureStyle::Block, 0, None),
            plain("b"),
            Event::MappingStart(StructureStyle::Flow, 0, None),
            Event::UnresolvedAlias("missing".into()),
            Event::Scalar("short".into(), ScalarStyle::DoubleQuoted, 0, None),
            Event::MappingEnd,
            Event::MappingEnd,
            Event::DocumentEnd,
            Event::StreamEnd,
        ]
    );
    assert_token_span(events[5].1, source, 4, "*missing");
    let source = "[*a:\"first\", *a:[short], &a k, *a:\"last\"]\n";
    let mut parser = Parser::new_from_str_with_options(source, options.clone());
    parser.set_anchor_offset(23);
    let events = parser.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|(event, _)| event.is_alias())
            .map(|(event, _)| event.clone())
            .collect::<Vec<_>>(),
        [
            Event::UnresolvedAlias("a".into()),
            Event::UnresolvedAlias("a".into()),
            Event::Alias(23),
        ]
    );
    assert_eq!(parser.anchor_offset(), 24);
    parse_all_inputs(source, options);
}

#[test]
fn duplicate_definitions_and_document_boundaries_keep_resolution_rules() {
    let source = "a: &a first\nb: &a second\nc: {*a:\"short\"}\n---\nd: {*a:[short]}\n";
    let mut options = go_options();
    options.emit_unresolved_aliases = true;
    let events = parse_all_inputs(source, options);
    assert_eq!(
        events
            .iter()
            .filter(|(event, _)| matches!(event, Event::Anchor(..)) || event.is_alias())
            .map(|(event, _)| event.clone())
            .collect::<Vec<_>>(),
        [
            Event::Anchor(1, "a".into()),
            Event::Anchor(2, "a".into()),
            Event::Alias(2),
            Event::UnresolvedAlias("a".into()),
        ]
    );
    assert_eq!(
        events
            .iter()
            .filter(|(event, _)| matches!(event, Event::DocumentEnd))
            .count(),
        2
    );
    let trace = trace_all_inputs(source, go_options());
    assert_eq!(
        trace.last().unwrap().as_ref().unwrap_err().kind(),
        &ErrorKind::UnknownAnchor
    );
    assert_eq!(
        trace
            .iter()
            .filter(|entry| matches!(entry, Ok((Event::DocumentEnd, _))))
            .count(),
        1
    );
}

#[test]
fn duplicate_properties_and_properties_on_aliases_still_fail() {
    for source in ["[&a &b k]\n", "[&a k, &b *a]\n"] {
        for emit_unresolved_aliases in [false, true] {
            let mut options = go_options();
            options.emit_unresolved_aliases = emit_unresolved_aliases;
            let trace = trace_all_inputs(source, options.clone());
            assert!(trace.last().unwrap().is_err(), "{source:?}");
            assert!(!trace
                .iter()
                .any(|entry| matches!(entry, Ok((Event::DocumentEnd, _)))));
            options.go_compatible_anchors = false;
            assert_eq!(trace, trace_all_inputs(source, options), "{source:?}");
        }
    }
    let source = "a: &a [*a:\"short\"]\n";
    let events = parse_all_inputs(source, go_options());
    let references = events
        .iter()
        .filter(|(event, _)| matches!(event, Event::Anchor(..)) || event.is_alias())
        .map(|(event, _)| event.clone())
        .collect::<Vec<_>>();
    assert_eq!(references, [Event::Anchor(1, "a".into()), Event::Alias(1)]);
}

#[test]
fn go_mode_does_not_relax_existing_resource_or_directive_limits() {
    for (source, options, kind) in [
        (
            "[[short]]\n",
            granit_parser::options! { go_compatible_anchors: true, flow_nesting_limit: 1 },
            ErrorKind::RecursionLimitExceeded,
        ),
        (
            "- - short\n",
            granit_parser::options! { go_compatible_anchors: true, block_nesting_limit: 1 },
            ErrorKind::RecursionLimitExceeded,
        ),
        (
            "a: x\nlong: short\n",
            granit_parser::options! { go_compatible_anchors: true, simple_key_max_lookahead: 1 },
            ErrorKind::SimpleKeyExpected,
        ),
        (
            "%TAG !e! tag:*a:short\n---\nshort\n",
            granit_parser::options! { go_compatible_anchors: true, disallow_directives: true },
            ErrorKind::DirectiveDisallowed,
        ),
        (
            "%TAG !e! tag:*a:short\n---\nshort\n",
            granit_parser::options! { go_compatible_anchors: true, max_directive_bytes: 1 },
            ErrorKind::DirectiveByteLimitExceeded { limit: 1 },
        ),
    ] {
        let trace = trace_all_inputs(source, options.clone());
        let mut default_grammar = options.clone();
        default_grammar.go_compatible_anchors = false;
        assert_eq!(
            trace,
            trace_all_inputs(source, default_grammar),
            "{source:?}"
        );
        assert_eq!(
            trace.last().unwrap().as_ref().unwrap_err().kind(),
            &kind,
            "{source:?}"
        );
        let tokens = scan_all_inputs(source, options);
        assert_eq!(
            tokens.last().unwrap().as_ref().unwrap_err().kind(),
            &kind,
            "{source:?}"
        );
    }
}

struct CountedInput<'a> {
    input: StrInput<'a>,
    operations: Rc<Cell<usize>>,
}

impl CountedInput<'_> {
    fn count(&self, amount: usize) {
        self.operations.set(self.operations.get() + amount);
    }
}

impl Input for CountedInput<'_> {
    fn lookahead(&mut self, count: usize) {
        self.count(1);
        self.input.lookahead(count);
    }
    fn buflen(&self) -> usize {
        self.count(1);
        self.input.buflen()
    }
    fn bufmaxlen(&self) -> usize {
        self.count(1);
        self.input.bufmaxlen()
    }
    fn raw_read_ch(&mut self) -> char {
        self.count(1);
        self.input.raw_read_ch()
    }
    fn raw_read_non_breakz_ch(&mut self) -> Option<char> {
        self.count(1);
        self.input.raw_read_non_breakz_ch()
    }
    fn skip(&mut self) {
        self.count(1);
        self.input.skip();
    }
    fn skip_n(&mut self, count: usize) {
        self.count(count);
        self.input.skip_n(count);
    }
    fn peek(&self) -> char {
        self.count(1);
        self.input.peek()
    }
    fn peek_nth(&self, count: usize) -> char {
        self.count(count + 1);
        self.input.peek_nth(count)
    }
    fn byte_offset(&self) -> Option<usize> {
        self.count(1);
        self.input.byte_offset()
    }
    fn slice_bytes(&self, start: usize, end: usize) -> Option<&str> {
        self.count(1);
        self.input.slice_bytes(start, end)
    }
}

impl<'a> BorrowedInput<'a> for CountedInput<'a> {
    fn slice_borrowed(&self, start: usize, end: usize) -> Option<&'a str> {
        self.count(1);
        self.input.slice_borrowed(start, end)
    }
}

#[test]
fn thousands_of_compact_aliases_take_linear_input_work_without_expansion() {
    let mut previous = None;
    for count in [32, 512, 2048] {
        let source = format!(
            "text: a:&a harmless\na: &a k\nb: [{}]\n",
            "{*a:\"literal *b:short [x y]\",*a:plain*b:short},".repeat(count),
        );
        let operations = Rc::new(Cell::new(0));
        let events = Parser::with_options(
            CountedInput {
                input: StrInput::new(&source),
                operations: Rc::clone(&operations),
            },
            go_options(),
        )
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        let work = operations.get();
        assert_eq!(
            events.iter().filter(|(event, _)| event.is_alias()).count(),
            2 * count
        );
        assert!(events
            .iter()
            .filter(|(event, _)| event.is_alias())
            .all(|(event, _)| *event == Event::Alias(1)));
        assert!(events.iter().any(
            |(event, _)| matches!(event, Event::Scalar(value, ..) if value == "a:&a harmless")
        ));
        assert!(
            work < 256 * source.len(),
            "{count} entries took {work} input operations"
        );
        if let Some((previous_length, previous_work)) = previous {
            assert!(
                work * previous_length <= 2 * previous_work * source.len(),
                "input work grew faster than linearly"
            );
        }
        previous = Some((source.len(), work));
        let tokens = scan_all_inputs(&source, go_options())
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            tokens
                .iter()
                .filter(|token| matches!(token.token_type(), TokenType::Alias(_)))
                .count(),
            2 * count
        );
    }
}
