use winnow::ascii::{Caseless, crlf};
use winnow::combinator::{alt, cut_err, empty, eof, fail, opt, preceded};
use winnow::combinator::{repeat_till, separated, separated_pair, seq, terminated};
use winnow::error::{ErrMode, FromExternalError, ModalResult};
use winnow::prelude::*;
use winnow::stream::{Accumulate, StreamIsPartial, UpdateSlice};
use winnow::token::{take_until, take_while};

use crate::state::{Delimiter, Progress};
use crate::{Entry, EntryInfo, Stream};

use crate::error::Error;
use crate::trivia::{
    is_horizontal_space, is_token, is_visible_ascii, quoted_string, take_until_crlf, trim, ws,
};

#[derive(Copy, Clone, Debug)]
enum Field<'a> {
    /// form-data field.
    FormData,
    /// The `name` attribute in a `Content-Disposition` header.
    Name(&'a [u8]),
    /// The `filename` attribute in a `Content-Disposition` header.
    Filename(&'a [u8]),
    /// Any other unrecognized or unsupported field.
    Other,
}

#[derive(Copy, Clone, Debug)]
enum Header<'a> {
    /// A `Content-Disposition` header, possibly with a filename.
    ContentDisposition {
        name: &'a [u8],
        filename: Option<&'a [u8]>,
    },
    /// A `Content-Type` header.
    ContentType(&'a [u8]),
    /// Any other unrecognized header.
    Other,
}

#[derive(Default)]
struct Headers<'a> {
    name: Option<&'a [u8]>,
    filename: Option<&'a [u8]>,
    content_type: Option<&'a [u8]>,
}

impl<'a> Accumulate<Header<'a>> for Headers<'a> {
    fn initial(_capacity: Option<usize>) -> Self {
        Self::default()
    }

    fn accumulate(&mut self, header: Header<'a>) {
        match header {
            Header::ContentDisposition { name, filename } => {
                self.name = Some(name);
                self.filename = filename;
            }
            Header::ContentType(content_type) => self.content_type = Some(content_type),
            Header::Other => {}
        }
    }
}

#[derive(Default)]
struct Disposition<'a> {
    name: Option<&'a [u8]>,
    filename: Option<&'a [u8]>,
    has_formdata: bool,
}

impl<'a> Accumulate<Field<'a>> for Disposition<'a> {
    fn initial(_capacity: Option<usize>) -> Self {
        Self::default()
    }

    fn accumulate(&mut self, field: Field<'a>) {
        match field {
            Field::FormData => self.has_formdata = true,
            Field::Name(n) if self.has_formdata => self.name = Some(n),
            Field::Filename(n) if self.has_formdata => self.filename = Some(n),
            _ => {}
        }
    }
}

// Adopted from gecko MultipartParser:
// https://searchfox.org/mozilla-central/source/dom/base/BodyUtil.cpp#67

// Parse preamble that is to be ignored.
// https://datatracker.ietf.org/doc/html/rfc2046#section-5.1.1:
pub(crate) fn skip_preamble(input: &mut Stream) -> ModalResult<()> {
    let state = input.state;
    let initial_len = state.initial_marker().len();

    let initial_candidate_pending = input.state.search_off == 0
        && input
            .state
            .progress
            .is_none_or(|progress| progress.marker_off == 0);

    if initial_candidate_pending {
        let mut initial = input.input;
        let initial_result: ModalResult<&[u8]> = state.initial_marker().parse_next(&mut initial);

        match initial_result {
            Ok(_) => {
                let resuming_initial = input.state.progress.is_some_and(|progress| {
                    progress.marker_off == 0 && progress.marker_len == initial_len
                });
                if !resuming_initial {
                    input.state.progress = Some(Progress::new(0, initial_len));
                }

                match delimiter_suffix(input) {
                    Ok(()) => {
                        input.state.search_off = 0;
                        input.state.progress = None;
                        return Ok(());
                    }
                    Err(error) if error.is_incomplete() => return Err(error),
                    Err(_) => input.state.progress = None,
                }
            }
            Err(error) if error.is_incomplete() => return Err(error),
            Err(_) => {}
        }
    }

    match find_valid_marker(input) {
        Ok(offset) => {
            let _ = input.next_slice(offset + 2);
            Ok(())
        }
        Err(error) if error.is_incomplete() => Err(error),
        Err(_) => Err(ErrMode::from_external_error(
            input,
            Error::MissingOpeningBoundary,
        )),
    }
}

// https://datatracker.ietf.org/doc/html/rfc2046#section-5.1.1:
pub(crate) fn next_entry<'s>(input: &mut Stream<'s>) -> ModalResult<Option<Entry<'s>>> {
    // Read over a boundary and advance stream to the position after the end of it.
    let state = input.state;
    preceded("--", state.value()).parse_next(input)?;

    // Check for end of data, which is boundary followed by two dashes: X-BOUNDARY--.
    if opt("--").parse_next(input)?.is_some() {
        let was_partial = input.complete();
        let result = (ws, opt(crlf)).parse_next(input);
        input.restore_partial(was_partial);
        result?;
        return Ok(None);
    }

    // Position the input at the beginning of the headers (after the CRLF). Allow horizontal spaces before.
    (ws, crlf).parse_next(input)?;

    Ok(Some(
        seq!(Entry {
            info: entry_info,
            value: value_body,
        })
        .parse_next(input)?,
    ))
}

// https://datatracker.ietf.org/doc/html/rfc2046#section-5.1.1:
pub(crate) fn value_body<'s>(input: &mut Stream<'s>) -> ModalResult<&'s [u8]> {
    match find_valid_marker(input) {
        Ok(offset) => {
            let value = input.next_slice(offset);
            let _ = input.next_slice(2);
            Ok(value)
        }
        Err(error) if error.is_incomplete() => Err(error),
        Err(_) => Err(ErrMode::from_external_error(
            input,
            Error::MissingClosingBoundary,
        )),
    }
}

// https://datatracker.ietf.org/doc/html/rfc2046#section-5.1.1:
pub(crate) fn entry_info<'s>(input: &mut Stream<'s>) -> ModalResult<EntryInfo<'s>> {
    let (headers, _): (Headers<'s>, _) =
        repeat_till(1.., terminated(header, crlf), (ws, crlf)).parse_next(input)?;

    headers
        .name
        .map(|name| EntryInfo {
            name,
            filename: headers.filename,
            content_type: headers.content_type,
        })
        .ok_or_else(|| ErrMode::from_external_error(input, Error::MissingContentDisposition))
}

// https://datatracker.ietf.org/doc/html/rfc7230#section-3.2.4
//
// No whitespace is allowed between the header field-name and colon.
fn header<'s>(input: &mut Stream<'s>) -> ModalResult<Header<'s>> {
    preceded(
        ws,
        alt((
            preceded(
                Caseless("content-disposition:"),
                cut_err(content_disposition),
            ),
            preceded(Caseless("content-type:"), cut_err(content_type)),
            (
                take_while(1.., is_token),
                cut_err((':', ws, take_until_crlf)),
            )
                .value(Header::Other),
        )),
    )
    .parse_next(input)
}

fn take_value<'s>(input: &mut Stream<'s>) -> ModalResult<&'s [u8]> {
    alt((
        terminated(quoted_string, ws),
        terminated(take_while(1.., is_token), ws),
    ))
    .parse_next(input)
}

fn field<'s>(input: &mut Stream<'s>) -> ModalResult<Field<'s>> {
    preceded(
        ws,
        alt((
            Caseless("form-data").value(Field::FormData),
            separated_pair(take_while(1.., is_token), (ws, '=', ws), take_value).map(
                |(name, value): (&[u8], &[u8])| {
                    if name.eq_ignore_ascii_case(b"name") {
                        Field::Name(value)
                    } else if name.eq_ignore_ascii_case(b"filename") {
                        Field::Filename(value)
                    } else {
                        Field::Other
                    }
                },
            ),
        )),
    )
    .parse_next(input)
}

fn content_disposition<'s>(input: &mut Stream<'s>) -> ModalResult<Header<'s>> {
    let disposition: Disposition<'s> = separated(1.., field, (ws, ';', ws)).parse_next(input)?;
    let filename = disposition.filename;

    disposition
        .name
        .map(|name| Header::ContentDisposition { name, filename })
        .ok_or_else(|| ErrMode::from_external_error(input, Error::MissingName))
}

fn content_type<'s>(input: &mut Stream<'s>) -> ModalResult<Header<'s>> {
    take_while(1.., is_visible_ascii)
        .map(trim)
        .map(Header::ContentType)
        .parse_next(input)
}

pub(crate) fn boundary_from_content_type<'s>(input: &mut Stream<'s>) -> ModalResult<&'s [u8]> {
    (ws, Caseless("multipart/form-data"), ws).parse_next(input)?;
    let mut boundary = None;

    while !input.input.is_empty() {
        (';', ws).parse_next(input)?;
        let name = take_while(1.., is_token).parse_next(input)?;
        (ws, '=', ws).parse_next(input)?;
        let value = take_value(input)?;

        if name.eq_ignore_ascii_case(b"boundary") && boundary.replace(value).is_some() {
            return fail.parse_next(input);
        }
    }

    boundary.map_or_else(|| fail.parse_next(input), Ok)
}

fn find_valid_marker(input: &mut Stream) -> ModalResult<usize> {
    let data = input.input;
    let state = input.state;
    let marker = state.line_marker();

    loop {
        if let Some(progress) = input.state.progress {
            match delimiter_suffix(input) {
                Ok(()) => {
                    input.state.search_off = 0;
                    input.state.progress = None;
                    return Ok(progress.marker_off);
                }
                Err(error) if error.is_incomplete() => return Err(error),
                Err(_) => {
                    input.state.search_off = progress.marker_off + 1;
                    input.state.progress = None;
                }
            }
        }

        let search_off = input.state.search_off;
        let Some(unsearched) = data.get(search_off..) else {
            return fail.parse_next(input);
        };

        let mut search = data.update_slice(unsearched);
        let next: ModalResult<&[u8]> = take_until(0.., marker).parse_next(&mut search);

        match next {
            Ok(prefix) => {
                let marker_off = search_off + prefix.len();
                input.state.progress = Some(Progress::new(marker_off, marker.len()));
            }
            Err(error) => {
                if error.is_incomplete() {
                    input.state.search_off =
                        data.len().saturating_sub(marker.len().saturating_sub(1));
                }
                return Err(error);
            }
        }
    }
}

/// Validates the cached delimiter candidate without advancing `input`.
fn delimiter_suffix(input: &mut Stream) -> ModalResult<()> {
    let Some(mut progress) = input.state.progress else {
        return fail.parse_next(input);
    };

    let Some(suffix) = input.input.get(progress.marker_off + progress.marker_len..) else {
        return fail.parse_next(input);
    };

    let suffix = input.input.update_slice(suffix);

    if progress.delim == Delimiter::Unknown {
        let mut prefix = suffix;
        let delimiter = alt(("--".value(Delimiter::Close), empty.value(Delimiter::Open)))
            .parse_next(&mut prefix);

        progress.delim = match delimiter {
            Ok(delimiter) => delimiter,
            Err(error) => {
                input.state.progress = Some(progress);
                return Err(error);
            }
        };
    }

    if progress.delim == Delimiter::Close && suffix.is_partial() {
        input.state.progress = Some(progress);
        return Ok(());
    }

    let prefix_len = match progress.delim {
        Delimiter::Open => 0,
        Delimiter::Close => 2,
        Delimiter::Unknown => unreachable!("delimiter kind was resolved"),
    };

    let Some(unparsed) = suffix.get(prefix_len + progress.padding_len..) else {
        input.state.progress = Some(progress);
        return fail.parse_next(input);
    };

    let mut terminator = suffix.update_slice(unparsed);
    let was_partial = terminator.complete();
    let padding = take_while(0.., is_horizontal_space).parse_next(&mut terminator);

    terminator.restore_partial(was_partial);

    progress.padding_len += match padding {
        Ok(padding) => padding.len(),
        Err(error) => {
            input.state.progress = Some(progress);
            return Err(error);
        }
    };

    let result = match progress.delim {
        Delimiter::Close => alt((crlf.void(), eof.void())).parse_next(&mut terminator),
        Delimiter::Open => crlf.void().parse_next(&mut terminator),
        Delimiter::Unknown => unreachable!("delimiter kind was resolved"),
    };
    input.state.progress = Some(progress);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Boundary, State};
    use winnow::Partial;
    use winnow::stream::StreamIsPartial;

    fn state(boundary: &[u8]) -> State {
        State::new(Boundary::new(boundary).unwrap())
    }

    fn suffix_stream(input: &[u8], complete: bool, saved_state: Option<State>) -> Stream<'_> {
        let mut input = Partial::new(input);
        if complete {
            let _ = input.complete();
        }

        let mut state = saved_state.unwrap_or_else(|| state(b"B"));
        state.progress.get_or_insert_with(|| Progress::new(0, 0));
        Stream { input, state }
    }

    #[test]
    fn test_delimiter_suffix_complete_and_partial_inputs() {
        for suffix in [
            b"\r\n".as_slice(),
            b" \t\r\n",
            b"--",
            b"-- \t",
            b"--\r\n",
            b"-- \t\r\nepilogue",
        ] {
            let mut input = suffix_stream(suffix, true, None);
            assert!(delimiter_suffix(&mut input).is_ok(), "{suffix:?}");
        }

        for suffix in [b"".as_slice(), b"-", b" ", b"\r"] {
            let mut input = suffix_stream(suffix, false, None);
            assert!(
                delimiter_suffix(&mut input).is_err_and(|error| error.is_incomplete()),
                "{suffix:?}"
            );
        }

        for suffix in [b"".as_slice(), b"-", b"x", b"--x", b" \t"] {
            let mut input = suffix_stream(suffix, true, None);
            assert!(
                delimiter_suffix(&mut input).is_err_and(|error| !error.is_incomplete()),
                "{suffix:?}"
            );
        }

        for suffix in [b"--".as_slice(), b"-- ", b"--\r", b"--x"] {
            let mut input = suffix_stream(suffix, false, None);
            assert!(delimiter_suffix(&mut input).is_ok(), "{suffix:?}");
        }
    }

    #[test]
    fn test_delimiter_suffix_resumes_after_padding() {
        let mut suffix = b" \t".to_vec();
        let mut input = suffix_stream(&suffix, false, None);

        assert!(delimiter_suffix(&mut input).is_err_and(|error| error.is_incomplete()));
        assert_eq!(input.state.progress.unwrap().delim, Delimiter::Open);
        assert_eq!(input.state.progress.unwrap().padding_len, 2);

        let state = input.state;
        suffix.extend_from_slice(b"  ");
        let mut input = suffix_stream(&suffix, false, Some(state));
        assert!(delimiter_suffix(&mut input).is_err_and(|error| error.is_incomplete()));
        assert_eq!(input.state.progress.unwrap().padding_len, 4);

        let state = input.state;
        suffix.extend_from_slice(b"\r\n");
        let mut input = suffix_stream(&suffix, false, Some(state));
        assert!(delimiter_suffix(&mut input).is_ok());
    }

    #[test]
    fn test_marker_search_resumes_with_boundary_overlap() {
        let marker = b"\r\n--X-BOUNDARY";
        let mut data = vec![b'x'; 1024];
        let mut input = Stream {
            input: Partial::new(data.as_slice()),
            state: state(b"X-BOUNDARY"),
        };

        let error = find_valid_marker(&mut input).unwrap_err();
        assert!(error.is_incomplete());
        assert_eq!(input.state.search_off, data.len() - (marker.len() - 1));

        let state = input.state;
        data.extend_from_slice(marker);
        data.extend_from_slice(b"\r\n");
        let mut input = Stream {
            input: Partial::new(data.as_slice()),
            state,
        };

        assert_eq!(find_valid_marker(&mut input), Ok(1024));
        assert_eq!(input.state.search_off, 0);
    }

    #[test]
    fn test_preamble_preserves_later_delimiter_progress() {
        let mut data = b"--B invalid\r\npreamble\r\n--B \t".to_vec();
        let mut stream = Stream {
            input: Partial::new(data.as_slice()),
            state: state(b"B"),
        };

        assert!(skip_preamble(&mut stream).is_err_and(|error| error.is_incomplete()));
        let progress = stream.state.progress.unwrap();
        assert!(progress.marker_off > 0);
        assert_eq!(progress.padding_len, 2);

        let state = stream.state;
        data.extend_from_slice(b"  ");
        let mut stream = Stream {
            input: Partial::new(data.as_slice()),
            state,
        };

        assert!(skip_preamble(&mut stream).is_err_and(|error| error.is_incomplete()));
        let progress = stream.state.progress.unwrap();
        assert_eq!(progress.padding_len, 4);

        let state = stream.state;
        data.extend_from_slice(b"\r\n");
        let mut stream = Stream {
            input: Partial::new(data.as_slice()),
            state,
        };

        assert!(skip_preamble(&mut stream).is_ok());
    }

    #[derive(Debug)]
    struct TestCase<'a> {
        input: &'a [u8],
        expected_name: Option<&'a [u8]>,
        expected_filename: Option<&'a [u8]>,
        expected_content_type: Option<&'a [u8]>,
        expect_error: bool,
    }

    impl<'a> TestCase<'a> {
        /// Create a new test case with the given input.
        fn new(input: &'a [u8]) -> Self {
            Self {
                input,
                expected_name: None,
                expected_filename: None,
                expected_content_type: None,
                expect_error: false,
            }
        }

        /// Set the expected `name` value.
        #[must_use]
        fn expected_name(mut self, name: &'a [u8]) -> Self {
            self.expected_name = Some(name);
            self
        }

        /// Set the expected `filename` value.
        #[must_use]
        fn expected_filename(mut self, filename: &'a [u8]) -> Self {
            self.expected_filename = Some(filename);
            self
        }

        /// Set the expected `content_type` value.
        #[must_use]
        fn expected_content_type(mut self, content_type: &'a [u8]) -> Self {
            self.expected_content_type = Some(content_type);
            self
        }

        /// Mark this test case as expecting a parse error.
        #[must_use]
        fn expect_error(mut self) -> Self {
            self.expect_error = true;
            self
        }

        /// Run the test case.
        fn run(self) {
            let mut input = Partial::new(self.input);
            let _ = input.complete();
            let mut stream = winnow::Stateful {
                input,
                state: State::for_metadata(),
            };
            let result = entry_info(&mut stream);

            match self.expect_error {
                true => assert!(result.is_err(),),
                false => {
                    let info = result.expect("Parsing should succeed");
                    assert_eq!(info.name, self.expected_name.expect("Name is set"));
                    assert_eq!(info.filename, self.expected_filename);
                    assert_eq!(info.content_type, self.expected_content_type);
                }
            }
        }
    }

    #[test]
    fn test_valid_headers_with_filename() {
        TestCase::new(b"Content-Disposition: form-data; name=foo; filename=dummy.txt\r\nContent-Type: text/plain\r\n\r\n")
        .expected_name(b"foo")
        .expected_filename(b"dummy.txt")
        .expected_content_type(b"text/plain")
        .run();
    }

    #[test]
    fn test_valid_headers_without_filename() {
        TestCase::new(b"Content-Disposition: form-data; name=foo\r\n\r\n")
            .expected_name(b"foo")
            .run();
    }

    #[test]
    fn test_extra_whitespace_and_case_insensitivity() {
        TestCase::new(b"  CoNtEnT-DiSpOsItIoN:   form-data  ;   name  =   foo  ; filename  =   dummy.txt  \r\n content-type:  text/plain \r\n  \r\n")
        .expected_name(b"foo")
        .expected_filename(b"dummy.txt")
        .expected_content_type(b"text/plain")
        .run();
    }

    #[test]
    fn test_multiple_headers_last_one_wins() {
        TestCase::new(b"Content-Disposition: form-data; name=first\r\nContent-Disposition: form-data; name=second; filename=second.txt\r\nContent-Type: text/plain\r\n\r\n")
        .expected_name(b"second")
        .expected_filename(b"second.txt")
        .expected_content_type(b"text/plain")
        .run();
    }

    #[test]
    fn test_ignore_unknown_headers_among_known_headers() {
        TestCase::new(b"Content-Disposition: form-data; name=foo\r\nX-Custom: ignoreme\r\nContent-Type: text/plain\r\n\r\n")
        .expected_name(b"foo")
        .expected_content_type(b"text/plain")
        .run();
    }

    #[test]
    fn test_ignore_multiple_unknown_headers() {
        TestCase::new(b"X-Ignored: value1\r\nContent-Disposition: form-data; name=foo; filename=dummy.txt\r\nX-Extra: value2\r\nContent-Type: text/plain\r\nX-Another: value3\r\n\r\n")
        .expected_name(b"foo")
        .expected_filename(b"dummy.txt")
        .expected_content_type(b"text/plain")
        .run();
    }

    #[test]
    fn test_only_unknown_headers_results_in_error() {
        TestCase::new(b"X-Unknown: foo\r\nAnother-Header: bar\r\n")
            .expect_error()
            .run();
    }

    #[test]
    fn test_missing_content_disposition() {
        TestCase::new(b"Content-Type: text/plain\r\n\r\n")
            .expect_error()
            .run();
    }

    #[test]
    fn test_missing_name_in_content_disposition() {
        TestCase::new(b"Content-Disposition: form-data; filename=dummy.txt\r\n\r\n")
            .expect_error()
            .run();
    }

    #[test]
    fn test_invalid_header_token() {
        TestCase::new(
            b"Content-Disposition: form-data; name=foo\r\n\
              invalid-header-name() : value\r\n\r\n",
        )
        .expect_error()
        .run();

        TestCase::new(
            b"Content-Disposition: form-data; name=foo\r\n\
              invalid\x7fheader: value\r\n\r\n",
        )
        .expect_error()
        .run();
    }
}
