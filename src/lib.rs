#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]

use std::iter::FusedIterator;

use winnow::error::ErrMode;
use winnow::prelude::*;
use winnow::stream::StreamIsPartial;
use winnow::{Partial, Stateful};

mod error;
mod parser;
mod state;
mod trivia;

/// C ABI bindings, available with the `capi` feature.
#[cfg(feature = "capi")]
pub mod capi;

pub use error::Error;

use state::{Boundary, State};

type Stream<'s> = Stateful<Partial<&'s [u8]>, State>;

/// Holds metadata for a single multipart form data entry.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct EntryInfo<'a> {
    /// The `name` attribute from `Content-Disposition`.
    name: &'a [u8],
    /// The optional `filename` attribute from `Content-Disposition`.
    filename: Option<&'a [u8]>,
    /// The optional `Content-Type`.
    content_type: Option<&'a [u8]>,
}

/// Represents a single multipart form data entry, including its metadata and value.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Entry<'a> {
    /// Metadata describing this form data entry.
    info: EntryInfo<'a>,
    /// The raw bytes of the entry's content.
    value: &'a [u8],
}

impl<'a> Entry<'a> {
    /// Returns the `name` attribute from the `Content-Disposition` header.
    #[must_use]
    pub fn name(&self) -> &'a [u8] {
        self.info.name
    }

    /// Returns the optional `filename` attribute from the `Content-Disposition` header.
    #[must_use]
    pub fn filename(&self) -> Option<&'a [u8]> {
        self.info.filename
    }

    /// Returns the optional `Content-Type` header value.
    #[must_use]
    pub fn content_type(&self) -> Option<&'a [u8]> {
        self.info.content_type
    }

    /// Returns the raw bytes of the entry's content.
    #[must_use]
    pub fn value(&self) -> &'a [u8] {
        self.value
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Phase {
    /// Reading the preamble section, which appears before the first boundary.
    /// This data is ignored as per the multipart/form-data specification.
    Preamble,
    /// Reading the multipart body form entries.
    Body,
    /// Parsing has completed or failed.
    Done,
}

/// The result of one call to [`MultipartParser::parse_next`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Outcome<'a> {
    /// A complete entry was parsed.
    ///
    /// The caller may discard the first `consumed` bytes before the next call.
    Entry {
        /// The parsed entry, borrowing from the supplied input.
        entry: Entry<'a>,
        /// The number of input bytes consumed through this entry.
        consumed: usize,
    },
    /// The closing boundary was parsed.
    Done {
        /// The number of bytes consumed through the closing boundary line.
        consumed: usize,
    },
    /// More bytes are required.
    ///
    /// The caller must retain the current input, append more bytes, and retry
    /// with the full retained slice.
    NeedMore,
}

/// A caller-buffered, resumable `multipart/form-data` parser.
///
/// This parser retains only parsing state and the boundary. The caller owns the
/// input buffer. After [`Outcome::NeedMore`], append data and retry with the
/// entire unconsumed slice. After [`Outcome::Entry`], discard the reported
/// `consumed` prefix before parsing the next entry.
#[derive(Debug)]
pub struct MultipartParser {
    phase: Phase,
    state: State,
    cursor: usize,
}

impl MultipartParser {
    /// Creates a multipart parser.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBoundary`] when `boundary` does not satisfy the
    /// RFC 2046 boundary grammar.
    pub fn new(boundary: impl AsRef<[u8]>) -> Result<Self, Error> {
        let boundary = Boundary::new(boundary.as_ref())?;

        Ok(Self {
            phase: Phase::Preamble,
            state: State::new(boundary),
            cursor: 0,
        })
    }

    /// Attempts to parse one entry from the caller-owned input.
    ///
    /// An otherwise valid prefix returns [`Outcome::NeedMore`]. Bytes
    /// already supplied must remain unchanged when retrying with additional
    /// data. If the transport reaches EOF while this method still returns
    /// [`Outcome::NeedMore`], the multipart body is truncated.
    ///
    /// # Errors
    ///
    /// Returns a multipart syntax error. The parser is exhausted after an
    /// error.
    pub fn parse_next<'a>(&mut self, input: &'a [u8]) -> Result<Outcome<'a>, Error> {
        self.parse_next_impl(input, false)
    }

    fn parse_next_impl<'a>(
        &mut self,
        input: &'a [u8],
        complete: bool,
    ) -> Result<Outcome<'a>, Error> {
        if self.phase == Phase::Done {
            return Ok(Outcome::Done { consumed: 0 });
        }

        let Some(unparsed) = input.get(self.cursor..) else {
            self.phase = Phase::Done;
            return Err(Error::MalformedBody);
        };

        let mut partial = Partial::new(unparsed);

        if complete {
            let _ = partial.complete();
        }

        let mut stream = Stream {
            input: partial,
            state: self.state,
        };

        if self.phase == Phase::Preamble {
            if let Err(error) = parser::skip_preamble(&mut stream) {
                self.state = stream.state;
                return self.handle_error(error, complete);
            }
            self.phase = Phase::Body;
            self.cursor = input.len() - stream.input.len();
        }

        let entry_start = self.cursor;

        match parser::next_entry.parse_next(&mut stream) {
            Ok(Some(entry)) => {
                self.state = stream.state;
                self.phase = Phase::Body;
                self.cursor = 0;
                Ok(Outcome::Entry {
                    entry,
                    consumed: input.len() - stream.input.len(),
                })
            }
            Ok(None) => {
                self.state = stream.state;
                self.phase = Phase::Done;
                self.cursor = 0;
                Ok(Outcome::Done {
                    consumed: input.len() - stream.input.len(),
                })
            }
            Err(error) => {
                self.state = stream.state;
                self.cursor = entry_start;
                self.handle_error(error, complete)
            }
        }
    }

    fn handle_error<'a>(
        &mut self,
        error: ErrMode<winnow::error::ContextError>,
        complete: bool,
    ) -> Result<Outcome<'a>, Error> {
        if error.is_incomplete() && !complete {
            Ok(Outcome::NeedMore)
        } else {
            self.phase = Phase::Done;
            Err(Error::from_winnow(error))
        }
    }

    /// Returns an iterator over a complete multipart body.
    ///
    /// Unlike [`MultipartParser::parse_next`], this uses complete-input
    /// semantics, so truncated input produces an error instead of
    /// [`Outcome::NeedMore`].
    pub fn entries<'parser, 'input>(
        &'parser mut self,
        input: &'input [u8],
    ) -> Entries<'parser, 'input> {
        Entries {
            parser: self,
            input,
        }
    }
}

/// An iterator over entries in a complete multipart body.
#[derive(Debug)]
pub struct Entries<'parser, 'input> {
    parser: &'parser mut MultipartParser,
    input: &'input [u8],
}

impl<'input> Iterator for Entries<'_, 'input> {
    type Item = Result<Entry<'input>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let input = self.input;

        match self.parser.parse_next_impl(input, true) {
            Ok(Outcome::Entry { entry, consumed }) => {
                self.input = &input[consumed..];
                Some(Ok(entry))
            }
            Ok(Outcome::Done { consumed }) => {
                self.input = &input[consumed..];
                None
            }
            Ok(Outcome::NeedMore) => {
                self.parser.phase = Phase::Done;
                Some(Err(Error::MalformedBody))
            }
            Err(error) => Some(Err(error)),
        }
    }
}

impl FusedIterator for Entries<'_, '_> {}

/// Extracts a valid boundary from a `multipart/form-data` Content-Type value.
#[must_use]
pub fn boundary_from_content_type(input: &[u8]) -> Option<&[u8]> {
    let mut partial = Partial::new(input);
    let _ = partial.complete();
    let mut stream = Stream {
        input: partial,
        state: State::for_metadata(),
    };

    parser::boundary_from_content_type(&mut stream)
        .ok()
        .filter(|boundary| Boundary::new(boundary).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_boundary() {
        let content_type = b"multipart/form-data; boundary=X-BOUNDARY";
        assert_eq!(
            boundary_from_content_type(content_type),
            Some(b"X-BOUNDARY".as_slice())
        );

        let content_type = b"multipart/form-data; boundary=\"--X-BOUNDARY\"";
        assert_eq!(
            boundary_from_content_type(content_type),
            Some(b"--X-BOUNDARY".as_slice())
        );

        let content_type = b"multipart/form-data; boundary=------X-BOUNDARY";
        assert_eq!(
            boundary_from_content_type(content_type),
            Some(b"------X-BOUNDARY".as_slice())
        );

        let content_type =
            b"Multipart/Form-Data; charset=utf-8; BOUNDARY=\"X-BOUNDARY\"; ignored=yes";
        assert_eq!(
            boundary_from_content_type(content_type),
            Some(b"X-BOUNDARY".as_slice())
        );

        let content_type = b"boundary=------X-BOUNDARY";
        assert!(boundary_from_content_type(content_type).is_none());

        let content_type = b"multipart/form-data; boundary=valid@invalid";
        assert!(boundary_from_content_type(content_type).is_none());

        let content_type = b"multipart/form-data; boundary=first; boundary=second";
        assert!(boundary_from_content_type(content_type).is_none());
    }
}
