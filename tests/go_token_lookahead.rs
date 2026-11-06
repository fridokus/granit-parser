use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use granit_parser::{
    BorrowedInput, ErrorKind, Event, FallibleBufferedInput, Input, Options, Parser, ScanError,
    Scanner, Span, StrInput, TokenType,
};

fn options(enabled: bool, comments: bool) -> Options {
    granit_parser::options! {
        go_compatible_token_lookahead: enabled,
        emit_comments: comments,
        emit_anchor_events: true,
    }
}

fn source_error() -> ErrorKind {
    ErrorKind::InputDecoding {
        message: "incomplete encoded character".into(),
    }
}

fn completed(trace: &[Result<(Event<'_>, Span), ScanError>]) -> usize {
    trace
        .iter()
        .filter(|entry| matches!(entry, Ok((Event::DocumentEnd, _))))
        .count()
}

fn fallible_trace(source: &str, settings: Options) -> Vec<Result<(Event<'_>, Span), ScanError>> {
    let input = source
        .chars()
        .map(Ok)
        .chain(std::iter::once(Err(source_error())))
        .chain(std::iter::from_fn(|| {
            panic!("source polled after its terminal error")
        }));
    let mut parser = Parser::new_from_fallible_iter_with_options(input, settings);
    let trace = parser.by_ref().collect();
    assert!(parser.next().is_none());
    trace
}

#[test]
fn go_mode_withholds_uncommitted_document_boundary_on_source_failure() {
    let source = format!("a: safe\n---\n#{}\n", "x".repeat(2048));
    let ordinary = fallible_trace(&source, options(false, false));
    assert_eq!(completed(&ordinary), 1);
    let enabled = fallible_trace(&source, options(true, false));
    assert_eq!(completed(&enabled), 0);
    assert_eq!(
        enabled.last().unwrap().as_ref().unwrap_err().kind(),
        &source_error()
    );
}

struct CurrentWindowInput<'a> {
    chars: std::str::Chars<'a>,
    window: usize,
    remaining: usize,
    failed: Rc<Cell<bool>>,
    error: Option<ErrorKind>,
    consumed: Rc<Cell<usize>>,
    requests: Rc<RefCell<Vec<(usize, usize)>>>,
}

impl<'a> CurrentWindowInput<'a> {
    fn new(source: &'a str, consumed: Rc<Cell<usize>>) -> Self {
        Self {
            chars: source.chars(),
            window: 0,
            remaining: source.chars().count(),
            failed: Rc::new(Cell::new(false)),
            error: None,
            consumed,
            requests: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn advance(&mut self, count: usize) {
        let count = count.min(self.remaining);
        self.remaining -= count;
        self.consumed.set(self.consumed.get() + count);
    }
}

impl Input for CurrentWindowInput<'_> {
    fn lookahead(&mut self, count: usize) {
        self.requests
            .borrow_mut()
            .push((self.consumed.get(), count));
        self.window = count;
        if count > self.remaining && !self.failed.get() {
            self.failed.set(true);
            self.error = Some(source_error());
        }
    }
    fn buflen(&self) -> usize {
        self.window
    }
    fn bufmaxlen(&self) -> usize {
        FallibleBufferedInput::new(std::iter::empty::<Result<char, ErrorKind>>()).bufmaxlen()
    }
    fn raw_read_ch(&mut self) -> char {
        self.chars.next().map_or('\0', |value| {
            self.advance(1);
            value
        })
    }
    fn raw_read_non_breakz_ch(&mut self) -> Option<char> {
        if granit_parser::input::is_breakz(self.peek()) {
            None
        } else {
            Some(self.raw_read_ch())
        }
    }
    fn skip(&mut self) {
        if self.chars.next().is_some() {
            self.advance(1);
        }
    }
    fn skip_n(&mut self, count: usize) {
        let count = self.chars.by_ref().take(count).count();
        self.advance(count);
    }
    fn peek(&self) -> char {
        self.chars.clone().next().unwrap_or('\0')
    }
    fn peek_nth(&self, n: usize) -> char {
        self.chars.clone().nth(n).unwrap_or('\0')
    }
    fn take_source_error(&mut self) -> Option<ErrorKind> {
        self.error.take()
    }
}

impl<'a> BorrowedInput<'a> for CurrentWindowInput<'a> {
    fn slice_borrowed(&self, _start: usize, _end: usize) -> Option<&'a str> {
        None
    }
}

#[test]
fn terminal_input_exposes_only_the_current_window_without_borrowed_capabilities() {
    let consumed = Rc::new(Cell::new(0));
    let mut input = CurrentWindowInput::new("abcdefgh\n", Rc::clone(&consumed));
    assert_eq!(input.bufmaxlen(), 16);
    input.lookahead(4);
    assert_eq!(input.buflen(), 4);
    input.lookahead(2);
    assert_eq!(input.buflen(), 2);
    input.lookahead(1);
    assert_eq!(input.buflen(), 1);
    assert!(!input.failed.get());
    assert_eq!(input.byte_offset(), None);
    assert_eq!(input.slice_bytes(0, 0), None);
    assert_eq!(input.slice_borrowed(0, 0), None);
    assert_eq!(consumed.get(), 0);
    input.lookahead(16);
    assert!(input.failed.get());
    assert_eq!(input.take_source_error(), Some(source_error()));
    assert_eq!(input.take_source_error(), None);
}

#[test]
fn plain_scalar_content_uses_required_lookahead_only_in_go_mode() {
    let source = "ordinary_scalar_α_with_an_inner:colon\n";
    for enabled in [false, true] {
        let consumed = Rc::new(Cell::new(0));
        let input = CurrentWindowInput::new(source, Rc::clone(&consumed));
        let requests = Rc::clone(&input.requests);
        let trace = Scanner::with_options(input, options(enabled, false)).collect::<Vec<_>>();
        assert_eq!(
            trace.last().unwrap().as_ref().unwrap_err().kind(),
            &source_error()
        );
        let requests = requests.borrow();
        let first_content_request = requests.iter().find(|(position, _)| *position == 1);
        assert_eq!(
            first_content_request,
            Some(&(1, if enabled { 2 } else { 16 }))
        );
        assert_eq!(consumed.get(), source.chars().count());
    }
}

#[test]
fn clean_eof_preserves_tokens_events_spans_and_anchor_ids() {
    for source in [
        "",
        "short\n",
        "---\n...\n---\n",
        "a: &a [first, second]\nb: *a\n",
        "%TAG !e! tag:example.test:\n---\na: !e!thing &a value\nb: *a\n",
        "# before\n---\na: value # right\n...\n# between\n---\n[one, two]\n",
        "{[first, second]: value, other: next}\n",
    ] {
        for comments in [false, true] {
            let baseline = options(false, comments);
            let enabled = options(true, comments);
            let expected = Parser::new_from_str_with_options(source, baseline.clone())
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let events = Parser::new_from_str_with_options(source, enabled.clone())
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(events, expected, "{source:?}");
            for ((_, actual), (_, expected)) in events.iter().zip(&expected) {
                assert_eq!(actual.byte_range(), expected.byte_range());
                assert_eq!(actual.slice(source), expected.slice(source));
            }
            assert_eq!(
                Parser::new_from_iter_with_options(source.chars(), enabled.clone())
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                expected
            );
            assert_eq!(
                Parser::new_from_fallible_iter_with_options(
                    source.chars().map(Ok),
                    enabled.clone()
                )
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
                expected
            );
            let expected_tokens = Scanner::with_options(StrInput::new(source), baseline)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let tokens = Scanner::with_options(StrInput::new(source), enabled.clone())
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(tokens, expected_tokens, "{source:?}");
            for (actual, expected) in tokens.iter().zip(&expected_tokens) {
                assert_eq!(actual.span().byte_range(), expected.span().byte_range());
                assert_eq!(actual.span().slice(source), expected.span().slice(source));
            }
            assert_eq!(
                Scanner::with_options(FallibleBufferedInput::new(source.chars().map(Ok)), enabled)
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                expected_tokens
            );
        }
    }
}

#[test]
fn omitted_and_disabled_policy_keep_the_default_error_prefix() {
    let source = format!("a: safe\n---\n#{}\n", "x".repeat(2048));
    let input = || {
        source
            .chars()
            .map(Ok)
            .chain(std::iter::once(Err(source_error())))
    };
    let baseline = Parser::new_from_fallible_iter(input()).collect::<Vec<_>>();
    for settings in [
        Options::default(),
        granit_parser::options! { go_compatible_token_lookahead: false },
    ] {
        let trace =
            Parser::new_from_fallible_iter_with_options(input(), settings).collect::<Vec<_>>();
        assert_eq!(trace, baseline);
        assert_eq!(completed(&trace), 1);
    }
}

#[test]
fn lexer_prefetches_before_yielding_without_reordering_tokens() {
    let source = "a: [first, second]\nnext: value\n";
    let mut ordinary = Scanner::with_options(StrInput::new(source), options(false, false));
    let mut prefetched = Scanner::with_options(StrInput::new(source), options(true, false));
    let first = ordinary.next().unwrap().unwrap();
    assert_eq!(first.token_type(), &TokenType::StreamStart);
    assert_eq!(prefetched.next().unwrap().unwrap(), first);
    assert_eq!(ordinary.mark().index(), 0);
    assert!(prefetched.mark().index() > ordinary.mark().index());
    assert_eq!(prefetched.collect::<Vec<_>>(), ordinary.collect::<Vec<_>>());
}

#[test]
fn terminal_lexical_failure_suppresses_uncommitted_tokens_and_comments() {
    for comments in [false, true] {
        let source = "# first\n# second\n@\n";
        let mut scanner = Scanner::with_options(StrInput::new(source), options(true, comments));
        let trace = scanner.by_ref().collect::<Vec<_>>();
        assert_eq!(trace.len(), 1);
        assert_eq!(
            trace[0].as_ref().unwrap_err().kind(),
            &ErrorKind::UnexpectedCharacter { character: '@' }
        );
        assert!(scanner.next().is_none());
        let ordinary = Scanner::with_options(StrInput::new(source), options(false, comments))
            .collect::<Vec<_>>();
        assert_eq!(
            ordinary[0].as_ref().unwrap().token_type(),
            &TokenType::StreamStart
        );
    }
}

#[test]
fn comments_never_substitute_for_following_syntax_tokens() {
    for count in [1, 3, 32] {
        let source = format!("a: safe\n---\n{}@\n", "# literal\n".repeat(count));
        for comments in [false, true] {
            let trace = Parser::new_from_str_with_options(&source, options(true, comments))
                .collect::<Vec<_>>();
            assert_eq!(completed(&trace), 0);
            assert!(trace.last().unwrap().is_err());
        }
    }
}

#[test]
fn committed_document_end_does_not_wait_for_the_next_root_to_parse() {
    let source = "a: safe\n---\n[*missing, harmless, later]\n# harmless padding\n";
    let trace = Parser::new_from_str_with_options(source, options(true, false)).collect::<Vec<_>>();
    assert_eq!(completed(&trace), 1);
    assert_eq!(
        trace.last().unwrap().as_ref().unwrap_err().kind(),
        &ErrorKind::UnknownAnchor
    );
}

#[test]
fn syntax_token_window_controls_completed_prefix_documents() {
    for (tail, expected) in [
        ("#\n", 0),
        ("description: harmless\n#\n", 1),
        ("[harmless]\n", 1),
        ("{description: harmless}\n", 1),
        ("*missing\n", 0),
        ("&a harmless\n", 0),
        ("!!str harmless\n", 0),
        ("!missing!thing harmless\n", 0),
        ("\"harmless\"\n", 0),
        ("[\n", 0),
        ("[}\n", 0),
        ("\"unterminated\n", 0),
    ] {
        let source = format!("a: safe\n---\n{tail}");
        let consumed = Rc::new(Cell::new(0));
        let input = CurrentWindowInput::new(&source, Rc::clone(&consumed));
        let source_failed = Rc::clone(&input.failed);
        let mut parser = Parser::with_options(input, options(true, false));
        let mut trace = Vec::new();
        for event in parser.by_ref() {
            if matches!(event, Ok((Event::DocumentEnd, _))) {
                assert!(!source_failed.get(), "{source:?}");
            }
            trace.push(event);
        }
        assert_eq!(completed(&trace), expected, "{source:?}: {trace:?}");
        assert!(trace.last().unwrap().is_err(), "{source:?}");
        if expected == 1 {
            assert!(source_failed.get(), "{source:?}");
            assert_eq!(
                trace.last().unwrap().as_ref().unwrap_err().kind(),
                &source_error()
            );
        }
        assert!(parser.next().is_none());
        assert!(consumed.get() <= source.chars().count());
    }
}

#[test]
fn document_recovery_retains_prefetched_next_document_content() {
    let source = "*missing\n---\nkey: value\n";
    for enabled in [false, true] {
        let mut parser = Parser::new_from_str_with_options(source, options(enabled, false));
        assert_eq!(
            parser.find_map(Result::err).unwrap().kind(),
            &ErrorKind::UnknownAnchor
        );
        assert!(parser.proceed_next_document().unwrap());
        let result = parser.collect::<Result<Vec<_>, _>>();
        assert!(result.is_ok(), "lookahead={enabled}: {result:?}");
        let events = result.unwrap();
        assert!(events
            .iter()
            .any(|(event, _)| matches!(event, Event::Scalar(value, ..) if value == "key")));
        assert!(events
            .iter()
            .any(|(event, _)| matches!(event, Event::Scalar(value, ..) if value == "value")));
    }
}

fn recover_parser<'a, I: BorrowedInput<'a>>(
    mut parser: Parser<'a, I>,
    source: &str,
) -> Vec<(Event<'a>, Span)> {
    parser
        .by_ref()
        .find_map(Result::err)
        .expect("broken first document");
    assert!(parser.proceed_next_document().unwrap());
    parser
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("{source:?}: {error:?}"))
}

#[test]
fn recovery_preserves_queued_and_unread_document_roots_on_all_backends() {
    for broken in [
        "*missing\n",
        "broken: *missing\n",
        "[*missing, one, two, three]\n",
        "broken: [*missing, one, two, three]\n",
        "broken: [1, 2\n",
        "broken: {key: [1, 2}\n",
        "broken: \"unfinished\n",
        "broken: 'unfinished\n",
        "broken: \"\\q\"\n",
    ] {
        for following in [
            "---\nkey: value\n",
            "---\n{key: [one, {two: three}]}\n",
            "---\n[one, {two: three}]\n",
            "---\n---\nkey: value\n",
            "---\n...\n---\n",
            "...\nkey: value\n",
            "---\n&a [one, two]\n---\nkey: value\n",
        ] {
            let source = format!("{broken}{following}");
            for comments in [false, true] {
                let expected = [
                    recover_parser(
                        Parser::new_from_str_with_options(&source, options(false, comments)),
                        &source,
                    ),
                    recover_parser(
                        Parser::new_from_iter_with_options(
                            source.chars(),
                            options(false, comments),
                        ),
                        &source,
                    ),
                    recover_parser(
                        Parser::new_from_fallible_iter_with_options(
                            source.chars().map(Ok::<_, ErrorKind>),
                            options(false, comments),
                        ),
                        &source,
                    ),
                ];
                let settings = options(true, comments);
                let actual = [
                    recover_parser(
                        Parser::new_from_str_with_options(&source, settings.clone()),
                        &source,
                    ),
                    recover_parser(
                        Parser::new_from_iter_with_options(source.chars(), settings.clone()),
                        &source,
                    ),
                    recover_parser(
                        Parser::new_from_fallible_iter_with_options(
                            source.chars().map(Ok::<_, ErrorKind>),
                            settings.clone(),
                        ),
                        &source,
                    ),
                ];
                for (actual, expected) in actual.into_iter().zip(expected) {
                    assert_eq!(actual, expected, "{source:?}, comments={comments}");
                    for ((_, actual), (_, expected)) in actual.iter().zip(&expected) {
                        assert_eq!(actual.byte_range(), expected.byte_range(), "{source:?}");
                        assert_eq!(actual.slice(&source), expected.slice(&source), "{source:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn both_go_options_preserve_compact_alias_values_and_tags() {
    let source = concat!(
        "%TAG !e! tag:*a:[short]\n---\n",
        "a: &a key\nb: {*a:\"hello *b:short [x] {y}  \", next: [*a:'short']}\n",
    );
    let ordinary = granit_parser::options! {
        go_compatible_anchors: true,
        emit_anchor_events: true,
    };
    let mut enabled = ordinary.clone();
    enabled.go_compatible_token_lookahead = true;
    let expected = Parser::new_from_str_with_options(source, ordinary)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        Parser::new_from_str_with_options(source, enabled)
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        expected,
    );
}

#[cfg(feature = "comments")]
#[test]
fn prefetched_comments_respect_the_existing_buffer_budget() {
    let settings = granit_parser::options! {
        go_compatible_token_lookahead: true,
        emit_comments: true,
        max_buffered_comment_events: 2,
    };
    let mut scanner =
        Scanner::with_options(StrInput::new("# one\n# two\n# three\nvalue\n"), settings);
    let error = scanner.next().unwrap().unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::TooManyComments);
    assert_eq!(error.marker().line(), 3);
    assert!(scanner.next().is_none());
}
