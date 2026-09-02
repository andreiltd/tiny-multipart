# tiny-multipart

`tiny-multipart` is a small, zero-copy parser for `multipart/form-data`.

The parsing implementation uses [Winnow](https://crates.io/crates/winnow)

```rust
use tiny_multipart::{boundary_from_content_type, MultipartParser};

let content_type = b"multipart/form-data; boundary=example";
let boundary = boundary_from_content_type(content_type).expect("a boundary");

let body = b"--example\r\n\
    Content-Disposition: form-data; name=\"message\"\r\n\
    \r\n\
    hello\r\n\
    --example--\r\n";

let mut parser = MultipartParser::new(boundary)?;
let mut entries = parser.entries(body);

let entry = entries.next().transpose()?.expect("has entry");

assert_eq!(entry.name(), b"message");
assert_eq!(entry.value(), b"hello");
assert_eq!(entries.next(), None);

# Ok::<(), tiny_multipart::Error>(())
```

## Streaming

[`MultipartParser`](https://docs.rs/tiny-multipart/latest/tiny_multipart/struct.MultipartParser.html)
keeps parsing state but borrows entries from a caller owned buffer. Append
chunks when more input is needed and discard only the consumed prefix.

```rust
use std::io::{Error, ErrorKind};
use tiny_multipart::{MultipartParser, Outcome};

let body = b"--example\r\n\
    Content-Disposition: form-data; name=message\r\n\
    \r\n\
    hello\r\n\
    --example--\r\n";

let mut parser = MultipartParser::new("example")?;
let mut chunks = body.chunks(7);
let mut buffer = Vec::new();
let mut fields = Vec::new();

'stream: loop {
    let Some(chunk) = chunks.next() else {
        // NeedMore at transport EOF means the multipart body is truncated.
        return Err(Error::new(ErrorKind::UnexpectedEof, "truncated multipart body").into());
    };

    buffer.extend_from_slice(chunk);

    loop {
        match parser.parse_next(&buffer)? {
            Outcome::NeedMore => {
                // Keep the full unconsumed buffer unchanged and append more.
                break;
            }
            Outcome::Entry { entry, consumed } => {
                // The entry borrows buffer, so use or copy it before draining.
                fields.push((entry.name().to_vec(), entry.value().to_vec()));
                buffer.drain(..consumed);
            }
            Outcome::Done { consumed } => {
                // The explicit closing boundary completes the stream.
                buffer.drain(..consumed);
                break 'stream;
            }
        }
    }
}

assert_eq!(fields, [(b"message".to_vec(), b"hello".to_vec())]);

# Ok::<(), Box<dyn std::error::Error>>(())
```

## Features

- `simd` enables Winnow's `memchr`-backed byte searches and is enabled by
  default.
- `capi` exports the optional C ABI declared in `tiny-multipart.h`.

This crate was originally developed in
[StarlingMonkey](https://github.com/bytecodealliance/StarlingMonkey).
