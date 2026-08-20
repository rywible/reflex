#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    reflex_fuzz::fuzz_envelope(data);
});
