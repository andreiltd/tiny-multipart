use std::ffi::CStr;
use std::os::raw::c_char;

use crate::MultipartParser;

/// A slice of bytes as seen from C.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Slice {
    /// Pointer to the first byte, or null when `len` is zero.
    pub data: *const u8,
    /// Number of bytes in the slice.
    pub len: usize,
}

impl Slice {
    const EMPTY: Self = Self {
        data: std::ptr::null(),
        len: 0,
    };
}

/// A C view of a parsed entry. For optional fields, a NULL data pointer means not present.
#[repr(C)]
pub struct Entry {
    /// The entry's `name` parameter.
    pub name: Slice,
    /// The raw entry body.
    pub value: Slice,
    /// The optional `filename` parameter.
    pub filename: Slice,
    /// The optional `Content-Type` header.
    pub content_type: Slice,
}

/// Result of advancing a parser through the C API.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RetCode {
    /// An entry was written to the output pointer.
    Ok = 0,
    /// The parser reached the closing boundary.
    Eos = 1,
    /// The input or parser state is invalid.
    Error = 2,
}

/// Opaque multipart parser for the C API.
pub struct Parser {
    input: &'static [u8],
    parser: MultipartParser,
}

unsafe fn slice_from_ffi<'a>(slice: Slice) -> Option<&'a [u8]> {
    if slice.len == 0 {
        return Some(&[]);
    }
    if slice.data.is_null() {
        return None;
    }

    // SAFETY: The caller guarantees that `data` is readable for `len` bytes.
    Some(unsafe { std::slice::from_raw_parts(slice.data, slice.len) })
}

/// Creates a new parser with the provided data.
///
/// # Safety
///
/// `data` must point to a valid [`Slice`]. Its bytes must remain valid for the
/// lifetime of the returned parser and must not be modified, including by
/// another thread, during that time. `boundary` must point to a
/// NUL-terminated string for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tiny_multipart_parser_new(
    data: *const Slice,
    boundary: *const c_char,
) -> *mut Parser {
    if data.is_null() || boundary.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: The caller guarantees that `data` points to a valid `Slice`.
    // SAFETY: The caller guarantees that the slice's bytes are readable.
    let Some(data_slice) = (unsafe { slice_from_ffi(*data) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: The function contract requires the body to remain valid until
    // the parser is freed.
    let data_static = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(data_slice) };

    // SAFETY: The caller guarantees a NUL-terminated boundary string.
    let boundary = unsafe { CStr::from_ptr(boundary) }.to_bytes();

    let Ok(parser) = MultipartParser::new(boundary) else {
        return std::ptr::null_mut();
    };
    let parser = Parser {
        input: data_static,
        parser,
    };

    Box::into_raw(Box::new(parser))
}

/// Frees a parser created by [`tiny_multipart_parser_new`].
///
/// # Safety
///
/// `parser` must be null or a pointer returned by
/// [`tiny_multipart_parser_new`] that has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tiny_multipart_parser_free(parser: *mut Parser) {
    if parser.is_null() {
        return;
    }

    // SAFETY: The caller guarantees that this is a live parser pointer.
    drop(unsafe { Box::from_raw(parser) });
}

/// Retrieve the next entry from the parser into provided entry.
///
/// # Safety
///
/// `parser` must point to a live parser and `entry` must point to writable
/// memory for one [`Entry`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tiny_multipart_parser_next(
    parser: *mut Parser,
    entry: *mut Entry,
) -> RetCode {
    if parser.is_null() || entry.is_null() {
        return RetCode::Error;
    }

    // SAFETY: `parser` was checked for null and is valid by contract.
    let parser = unsafe { &mut *parser };
    let input = parser.input;

    let (ret, output) = match parser.parser.parse_next_impl(input, true) {
        Ok(crate::Outcome::Entry { entry: e, consumed }) => {
            parser.input = &input[consumed..];
            (
                RetCode::Ok,
                Entry {
                    name: Slice {
                        data: e.name().as_ptr(),
                        len: e.name().len(),
                    },
                    value: Slice {
                        data: e.value().as_ptr(),
                        len: e.value().len(),
                    },
                    filename: e.filename().map_or(Slice::EMPTY, |filename| Slice {
                        data: filename.as_ptr(),
                        len: filename.len(),
                    }),
                    content_type: e.content_type().map_or(Slice::EMPTY, |content_type| Slice {
                        data: content_type.as_ptr(),
                        len: content_type.len(),
                    }),
                },
            )
        }
        Ok(crate::Outcome::Done { consumed }) => {
            parser.input = &input[consumed..];
            (
                RetCode::Eos,
                Entry {
                    name: Slice::EMPTY,
                    value: Slice::EMPTY,
                    filename: Slice::EMPTY,
                    content_type: Slice::EMPTY,
                },
            )
        }
        Ok(crate::Outcome::NeedMore) | Err(_) => (
            RetCode::Error,
            Entry {
                name: Slice::EMPTY,
                value: Slice::EMPTY,
                filename: Slice::EMPTY,
                content_type: Slice::EMPTY,
            },
        ),
    };

    // SAFETY: `entry` points to writable storage by contract. `ptr::write`
    // initializes it without first reading a potentially uninitialized value.
    unsafe { std::ptr::write(entry, output) };
    ret
}

#[unsafe(no_mangle)]
/// Retrieves the boundary from a Content-Type header.
///
/// # Safety
///
/// `content_type` must point to a valid [`Slice`]. Its backing bytes must
/// remain valid and immutable while the returned boundary slice is used.
/// `boundary` must point to writable memory for one [`Slice`]. The descriptor
/// pointers may alias.
pub unsafe extern "C" fn tiny_multipart_boundary_from_content_type(
    content_type: *const Slice,
    boundary: *mut Slice,
) {
    if content_type.is_null() || boundary.is_null() {
        return;
    }

    // Read and parse the input before writing output so both descriptor
    // pointers may refer to the same `Slice`.
    let value =
        unsafe { slice_from_ffi(*content_type) }.and_then(crate::boundary_from_content_type);

    let output = value.map_or(Slice::EMPTY, |value| Slice {
        data: value.as_ptr(),
        len: value.len(),
    });

    // SAFETY: `boundary` points to writable storage by contract. `ptr::write`
    // initializes it without first reading a potentially uninitialized value.
    unsafe { std::ptr::write(boundary, output) };
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    use super::*;

    #[test]
    fn boundary_descriptors_may_alias() {
        let content_type = b"multipart/form-data; boundary=example";
        let mut slice = Slice {
            data: content_type.as_ptr(),
            len: content_type.len(),
        };
        let slice_ptr = std::ptr::addr_of_mut!(slice);

        // SAFETY: The input bytes remain valid and `slice_ptr` is writable.
        unsafe {
            tiny_multipart_boundary_from_content_type(slice_ptr as *const Slice, slice_ptr);
        }

        // SAFETY: The output points into the still-live `content_type`.
        assert_eq!(
            unsafe { slice_from_ffi(slice) },
            Some(b"example".as_slice())
        );
    }

    #[test]
    fn parser_initializes_uninitialized_output() {
        let body =
            b"--example\r\nContent-Disposition: form-data; name=field\r\n\r\nvalue\r\n--example--";
        let data = Slice {
            data: body.as_ptr(),
            len: body.len(),
        };
        let boundary = CString::new("example").unwrap();

        // SAFETY: The body and boundary remain valid through all parser calls.
        let state = unsafe { tiny_multipart_parser_new(&data, boundary.as_ptr()) };
        assert!(!state.is_null());

        let mut entry = MaybeUninit::<Entry>::uninit();
        // SAFETY: `state` is live and `entry` points to writable storage.
        assert_eq!(
            unsafe { tiny_multipart_parser_next(state, entry.as_mut_ptr()) },
            RetCode::Ok
        );
        // SAFETY: A successful call initializes the output entry.
        let entry = unsafe { entry.assume_init() };
        // SAFETY: The entry points into the still-live body.
        assert_eq!(
            unsafe { slice_from_ffi(entry.value) },
            Some(b"value".as_slice())
        );

        // SAFETY: `state` is live and has not already been freed.
        unsafe { tiny_multipart_parser_free(state) };
    }
}
