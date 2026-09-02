//! Resumable state threaded through the internal Winnow parsers.

use crate::Error;

const MAX_BOUNDARY_LEN: usize = 70;

/// Persistent parser data threaded through Winnow's [`winnow::Stateful`]
/// stream and retained between partial-input retries.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) struct State {
    /// Boundary prefixed with `\r\n--` for delimiter matching.
    pub(crate) marker: [u8; MAX_BOUNDARY_LEN + 4],
    /// Used length of `marker`.
    pub(crate) marker_len: u8,
    /// Offset where the next boundary search should resume.
    pub(crate) search_off: usize,
    /// Validation progress for a boundary candidate split across input chunks.
    pub(crate) progress: Option<Progress>,
}

impl State {
    /// Builds parser state from a previously validated boundary.
    pub(crate) fn new(boundary: Boundary<'_>) -> Self {
        let value = boundary.as_bytes();

        let mut marker = [0; MAX_BOUNDARY_LEN + 4];
        marker[..4].copy_from_slice(b"\r\n--");
        marker[4..4 + value.len()].copy_from_slice(value);

        Self {
            marker,
            marker_len: (value.len() + 4) as u8,
            search_off: 0,
            progress: None,
        }
    }

    pub(crate) fn value(&self) -> &[u8] {
        &self.marker[4..usize::from(self.marker_len)]
    }

    pub(crate) fn initial_marker(&self) -> &[u8] {
        &self.marker[2..usize::from(self.marker_len)]
    }

    pub(crate) fn line_marker(&self) -> &[u8] {
        &self.marker[..usize::from(self.marker_len)]
    }

    /// Builds state for parsers that do not inspect the multipart boundary.
    pub(crate) fn for_metadata() -> Self {
        Self::new(Boundary(b"_"))
    }
}

/// A boundary that satisfies the RFC 2046 boundary grammar.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) struct Boundary<'a>(&'a [u8]);

impl<'a> Boundary<'a> {
    /// Validates and wraps a boundary value.
    pub(crate) fn new(value: &'a [u8]) -> Result<Self, Error> {
        // RFC 2046: boundary := 0*69<bchars> bcharsnospace
        if value.is_empty()
            || !value.iter().copied().all(is_boundary_byte)
            || value.len() > MAX_BOUNDARY_LEN
            || value.last() == Some(&b' ')
        {
            return Err(Error::InvalidBoundary);
        }

        Ok(Self(value))
    }

    fn as_bytes(self) -> &'a [u8] {
        self.0
    }
}

/// Cached progress while validating a possible boundary delimiter.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) struct Progress {
    /// Offset of the candidate marker in the retained input.
    pub(crate) marker_off: usize,
    /// Length of the initial or line marker that was matched.
    pub(crate) marker_len: usize,
    /// Transport-padding bytes already checked after the marker.
    pub(crate) padding_len: usize,
    /// Resolved delimiter kind.
    pub(crate) delim: Delimiter,
}

impl Progress {
    pub(crate) fn new(marker_off: usize, marker_len: usize) -> Self {
        Self {
            marker_off,
            marker_len,
            padding_len: 0,
            delim: Delimiter::Unknown,
        }
    }
}

/// The kind of boundary delimiter currently being validated.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Delimiter {
    /// Not enough of the suffix has been parsed to identify the delimiter.
    Unknown,
    /// A delimiter introducing another multipart entry.
    Open,
    /// The delimiter terminating the multipart body.
    Close,
}

fn is_boundary_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'\''
                | b'('
                | b')'
                | b'+'
                | b'_'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'='
                | b'?'
                | b' '
        )
}
