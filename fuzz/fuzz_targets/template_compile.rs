#![no_main]
//! libFuzzer target: template compilation (a recursive-descent parse over
//! arbitrary source) must never panic or overflow; anything that compiles must
//! render without panicking. Run: `cargo +nightly fuzz run template_compile`.

use libfuzzer_sys::fuzz_target;
use sutegi_json::Json;
use sutegi_template::Template;

fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data) {
        if let Ok(t) = Template::compile(src) {
            let ctx = Json::obj(vec![("x", Json::str("v")), ("items", Json::arr(vec![]))]);
            let _ = t.render(&ctx);
        }
    }
});
