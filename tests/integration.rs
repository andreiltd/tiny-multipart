mod fixtures;

use tiny_multipart::{Entry, Error, MultipartParser, Outcome};

#[derive(Debug, PartialEq)]
struct ExpectedEntry<'a> {
    name: &'a [u8],
    body: &'a [u8],
    filename: Option<&'a [u8]>,
    content_type: Option<&'a [u8]>,
}

impl<'a> ExpectedEntry<'a> {
    fn new(name: &'a [u8], body: &'a [u8]) -> Self {
        Self {
            name,
            body,
            filename: None,
            content_type: None,
        }
    }

    fn filename(mut self, filename: &'a [u8]) -> Self {
        self.filename = Some(filename);
        self
    }

    fn content_type(mut self, content_type: &'a [u8]) -> Self {
        self.content_type = Some(content_type);
        self
    }
}

pub struct TestCase<'a> {
    data: &'a [u8],
    boundary: &'a str,
    expected_entries: Vec<ExpectedEntry<'a>>,
}

impl<'a> TestCase<'a> {
    fn new(data: &'a [u8], boundary: &'a str) -> Self {
        Self {
            data,
            boundary,
            expected_entries: Vec::new(),
        }
    }

    fn expected_entry(mut self, entry: ExpectedEntry<'a>) -> Self {
        self.expected_entries.push(entry);
        self
    }

    fn run(self) {
        let mut parser = MultipartParser::new(self.boundary).expect("valid boundary");
        let entries = parser.entries(self.data);
        let mut idx = 0;

        for result in entries {
            let entry = result.expect("valid multipart body");
            let actual = ExpectedEntry {
                name: entry.name(),
                body: entry.value(),
                filename: entry.filename(),
                content_type: entry.content_type(),
            };

            assert_eq!(actual, self.expected_entries[idx]);
            idx += 1;
        }

        assert_eq!(
            idx,
            self.expected_entries.len(),
            "Not all expected entries were parsed"
        );
    }
}

// Some of the test inputs are copied over from multer crate, see:
// https://github.com/rwf2/multer/blob/master/tests/integration.rs

#[test]
fn test_basic() {
    let data = b"--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_text_field\"\r\n\r\nabcd\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_file_field\"; filename=\"a-text-file.txt\"\r\nContent-Type: text/plain\r\n\r\nHello world\nHello\r\nWorld\rAgain\r\n--X-BOUNDARY--\r\n";
    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b"my_text_field", b"abcd"))
        .expected_entry(
            ExpectedEntry::new(b"my_file_field", b"Hello world\nHello\r\nWorld\rAgain")
                .filename(b"a-text-file.txt")
                .content_type(b"text/plain"),
        )
        .run();
}

#[test]
fn test_empty() {
    let empty = b"--X-BOUNDARY--\r\n";
    TestCase::new(empty, "X-BOUNDARY").run();
}

#[test]
fn test_whitespace_boundaries() {
    let data = b"--X-BOUNDARY \t \r\nContent-Disposition: form-data; name=\"my_text_field\"\r\n\r\nabcd\r\n--X-BOUNDARY     \r\nContent-Disposition: form-data; name=\"my_file_field\"; filename=\"a-text-file.txt\"\r\nContent-Type: text/plain\r\n\r\nHello world\nHello\r\nWorld\rAgain\r\n--X-BOUNDARY--\t\t\t\t\t\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b"my_text_field", b"abcd"))
        .expected_entry(
            ExpectedEntry::new(b"my_file_field", b"Hello world\nHello\r\nWorld\rAgain")
                .filename(b"a-text-file.txt")
                .content_type(b"text/plain"),
        )
        .run();
}

#[test]
fn test_ignored_header() {
    let data = b"ignored header\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_text_field\"\r\n\r\nabcd\r\n--X-BOUNDARY--\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b"my_text_field", b"abcd"))
        .run();
}

#[test]
fn test_ignored_header_with_leading_newline() {
    let data = b"\r\nignored header\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_text_field\"\r\n\r\nabcd\r\n--X-BOUNDARY--\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b"my_text_field", b"abcd"))
        .run();
}

#[test]
fn test_leading_newline() {
    let data = b"\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_text_field\"\r\n\r\nabcd\r\n--X-BOUNDARY--\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b"my_text_field", b"abcd"))
        .run();
}

#[test]
fn test_multiple_leading_newlines() {
    let data = b"\r\n\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_text_field\"\r\n\r\nabcd\r\n--X-BOUNDARY--\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b"my_text_field", b"abcd"))
        .run();
}

#[test]
fn test_fuzz_simple_seed() {
    TestCase::new(fixtures::SIMPLE, "X-BOUNDARY")
        .expected_entry(
            ExpectedEntry::new(b"field1", b"Joe owes =E2=82=AC100.")
                .content_type(b"text/plain;charset=UTF-8"),
        )
        .run();
}

#[test]
fn test_fuzz_multi_seed() {
    TestCase::new(fixtures::MULTI, "--BoundaryjXo5N4HEAXWcKrw7")
        .expected_entry(ExpectedEntry::new(b"field1", b"value1"))
        .expected_entry(ExpectedEntry::new(b"field2", b"value2"))
        .expected_entry(
            ExpectedEntry::new(b"file1", b"Hello World!")
                .filename(b"dummy.txt")
                .content_type(b"foo"),
        )
        .run();
}

#[test]
fn test_entry_accessors_preserve_the_input_lifetime() {
    fn into_fields<'a>(entry: Entry<'a>) -> ExpectedEntry<'a> {
        ExpectedEntry {
            name: entry.name(),
            body: entry.value(),
            filename: entry.filename(),
            content_type: entry.content_type(),
        }
    }

    let mut parser = MultipartParser::new("X-BOUNDARY").unwrap();
    let entry = parser.entries(BASIC_DATA).next().unwrap().unwrap();

    assert_eq!(
        into_fields(entry),
        ExpectedEntry::new(b"my_text_field", b"abcd")
    );
}

#[test]
fn test_malformed() {
    let data = b"--Boundary_with_capital_letters\r\nContent-Type: application/json\r\nContent-Disposition: form-data; name=\"does_this_work\"\r\n\r\nYES\r\n--Boundary_with_capital_letters-Random junk";
    let mut parser = MultipartParser::new("--Boundary_with_capital_letters").unwrap();
    let mut entries = parser.entries(data);

    assert_eq!(entries.next(), Some(Err(Error::MissingOpeningBoundary)));
    assert_eq!(entries.next(), None);
}

#[test]
fn test_empty_entry_body() {
    let data =
        b"--X-BOUNDARY\r\nContent-Disposition: form-data; name=empty\r\n\r\n\r\n--X-BOUNDARY--\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b"empty", b""))
        .run();
}

#[test]
fn test_boundary_like_text_in_body() {
    let data = b"--X-BOUNDARY\r\nContent-Disposition: form-data; name=field\r\n\r\nbefore\r\n--X-BOUNDARY-not-a-delimiter\r\nafter\r\n--X-BOUNDARY--\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(
            b"field",
            b"before\r\n--X-BOUNDARY-not-a-delimiter\r\nafter",
        ))
        .run();
}

#[test]
fn test_unknown_and_case_insensitive_disposition_parameters() {
    let data = b"--X-BOUNDARY\r\nContent-Disposition: FORM-DATA; ignored=value; NAME=\" field \"; FILENAME=file.txt\r\n\r\nbody\r\n--X-BOUNDARY--\r\n";

    TestCase::new(data, "X-BOUNDARY")
        .expected_entry(ExpectedEntry::new(b" field ", b"body").filename(b"file.txt"))
        .run();
}

#[test]
fn test_quoted_parameter_escapes_are_rejected() {
    let data = b"--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"escaped\\\"quote\"\r\n\r\nbody\r\n--X-BOUNDARY--\r\n";
    let mut parser = MultipartParser::new("X-BOUNDARY").unwrap();
    let mut entries = parser.entries(data);

    assert_eq!(entries.next(), Some(Err(Error::MissingName)));
}

#[test]
fn test_entries_borrow_from_input_not_parser() {
    let data = b"--X-BOUNDARY\r\nContent-Disposition: form-data; name=first\r\n\r\none\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=second\r\n\r\ntwo\r\n--X-BOUNDARY--\r\n";
    let mut parser = MultipartParser::new(String::from("X-BOUNDARY")).unwrap();
    let mut entries = parser.entries(data);

    let first = entries.next().unwrap().unwrap();
    let second = entries.next().unwrap().unwrap();

    assert_eq!(first.value(), b"one");
    assert_eq!(second.value(), b"two");
}

#[test]
fn test_invalid_boundary() {
    assert!(matches!(
        MultipartParser::new(""),
        Err(Error::InvalidBoundary)
    ));
    assert!(matches!(
        MultipartParser::new("x".repeat(71)),
        Err(Error::InvalidBoundary)
    ));
}

#[test]
fn test_missing_opening_boundary_is_an_error() {
    let mut parser = MultipartParser::new("X-BOUNDARY").unwrap();
    let mut entries = parser.entries(b"not multipart");

    assert_eq!(entries.next(), Some(Err(Error::MissingOpeningBoundary)));
    assert_eq!(entries.next(), None);
}

#[test]
fn test_parser_with_single_byte_chunks() {
    #[derive(Debug, Eq, PartialEq)]
    struct OwnedEntry {
        name: Vec<u8>,
        value: Vec<u8>,
    }

    let mut parser = MultipartParser::new("X-BOUNDARY").unwrap();
    let mut buffer = Vec::new();
    let mut offset = 0;
    let mut entries = Vec::new();
    let mut done = false;

    while !done {
        if offset < BASIC_DATA.len() {
            buffer.push(BASIC_DATA[offset]);
            offset += 1;
        }
        loop {
            match parser.parse_next(&buffer).unwrap() {
                Outcome::Entry { entry, consumed } => {
                    entries.push(OwnedEntry {
                        name: entry.name().to_vec(),
                        value: entry.value().to_vec(),
                    });
                    buffer.drain(..consumed);
                }
                Outcome::NeedMore => break,
                Outcome::Done { consumed } => {
                    buffer.drain(..consumed);
                    done = true;
                    break;
                }
            }
        }
    }

    assert_eq!(
        entries,
        [
            OwnedEntry {
                name: b"my_text_field".to_vec(),
                value: b"abcd".to_vec(),
            },
            OwnedEntry {
                name: b"my_file_field".to_vec(),
                value: b"Hello world\nHello\r\nWorld\rAgain".to_vec(),
            },
        ]
    );
    assert_eq!(offset, BASIC_DATA.len() - 2);
    assert!(buffer.is_empty());
}

#[test]
fn test_parser_resumes_first_entry_after_preamble() {
    let body = b"preamble\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=field\r\n\r\nvalue\r\n--X-BOUNDARY--";
    let incomplete = &body[..body.len() - 2];
    let mut parser = MultipartParser::new("X-BOUNDARY").unwrap();

    assert_eq!(parser.parse_next(incomplete), Ok(Outcome::NeedMore));

    let consumed = match parser.parse_next(body).unwrap() {
        Outcome::Entry { entry, consumed } => {
            assert_eq!(entry.name(), b"field");
            assert_eq!(entry.value(), b"value");
            consumed
        }
        status => panic!("expected an entry, got {status:?}"),
    };

    assert_eq!(
        parser.parse_next(&body[consumed..]),
        Ok(Outcome::Done {
            consumed: body.len() - consumed
        })
    );
}

#[test]
fn test_parser_reports_need_more_for_truncated_transport_input() {
    let incomplete = b"--X-BOUNDARY\r\nContent-Disposition: form-data; name=field\r\n\r\nvalue";
    let mut parser = MultipartParser::new("X-BOUNDARY").unwrap();

    assert_eq!(parser.parse_next(incomplete), Ok(Outcome::NeedMore));
    assert_eq!(parser.parse_next(incomplete), Ok(Outcome::NeedMore));

    let mut complete_parser = MultipartParser::new("X-BOUNDARY").unwrap();
    assert_eq!(
        complete_parser.entries(incomplete).next(),
        Some(Err(Error::MissingClosingBoundary))
    );
}

#[test]
fn test_parser_accepts_closing_boundary_at_eof() {
    let body = b"--X-BOUNDARY--";
    let mut parser = MultipartParser::new("X-BOUNDARY").unwrap();

    assert_eq!(
        parser.parse_next(body),
        Ok(Outcome::Done {
            consumed: body.len()
        })
    );
    assert_eq!(parser.parse_next(&[]), Ok(Outcome::Done { consumed: 0 }));
}

const BASIC_DATA: &[u8] = b"--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_text_field\"\r\n\r\nabcd\r\n--X-BOUNDARY\r\nContent-Disposition: form-data; name=\"my_file_field\"; filename=\"a-text-file.txt\"\r\nContent-Type: text/plain\r\n\r\nHello world\nHello\r\nWorld\rAgain\r\n--X-BOUNDARY--\r\n";
