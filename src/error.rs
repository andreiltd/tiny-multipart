use std::fmt::{self, Display, Formatter};

use winnow::error::{ContextError, ErrMode};

/// An error encountered while validating or parsing multipart form data.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// The supplied boundary is empty, too long, or contains an invalid byte.
    InvalidBoundary,
    /// The body does not contain a valid opening boundary delimiter.
    MissingOpeningBoundary,
    /// A part is not followed by another part or a closing boundary delimiter.
    MissingClosingBoundary,
    /// A `Content-Disposition` header has no `name` parameter.
    MissingName,
    /// A part has no valid `Content-Disposition` header.
    MissingContentDisposition,
    /// The multipart body is otherwise malformed.
    MalformedBody,
}

impl Error {
    pub(crate) fn from_winnow(error: ErrMode<ContextError>) -> Self {
        let context = match error {
            ErrMode::Backtrack(context) | ErrMode::Cut(context) => Some(context),
            ErrMode::Incomplete(_) => None,
        };

        context
            .as_ref()
            .and_then(ContextError::cause)
            .and_then(|cause| cause.downcast_ref::<Self>())
            .copied()
            .unwrap_or(Self::MalformedBody)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBoundary => f.write_str("invalid multipart boundary"),
            Self::MissingOpeningBoundary => f.write_str("multipart body has no opening boundary"),
            Self::MissingClosingBoundary => f.write_str("multipart part has no closing boundary"),
            Self::MissingName => f.write_str("name is missing from the content-disposition header"),
            Self::MissingContentDisposition => {
                f.write_str("content-disposition is missing from the part headers")
            }
            Self::MalformedBody => f.write_str("malformed multipart body"),
        }
    }
}

impl std::error::Error for Error {}
