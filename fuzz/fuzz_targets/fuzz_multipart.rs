#![no_main]

use libfuzzer_sys::fuzz_target;
use tiny_multipart::MultipartParser;

fuzz_target!(|data: &[u8]| {
    let Ok(mut parser) = MultipartParser::new("X-BOUNDARY") else {
        return;
    };

    for entry in parser.entries(data) {
        if entry.is_err() {
            break;
        }
    }
});
