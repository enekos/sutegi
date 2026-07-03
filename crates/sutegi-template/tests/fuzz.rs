//! Adversarial coverage for the Blade-lite template engine.
//!
//! Compilation is a recursive-descent parse and rendering is a recursive walk,
//! so pathological nesting is a stack-overflow risk, and directive/interp
//! parsing runs over arbitrary source. This checks that compile and render
//! only ever return `Ok`/`Err` (never panic or overflow), that deep nesting is
//! rejected rather than crashing, and that `@include` cycles terminate.
//!
//! Deterministic (fixed-seed splitmix64) for reproducibility in CI.

use sutegi_json::Json;
use sutegi_template::{Template, Templates};

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn compile_arbitrary_source_never_panics() {
    let mut seed = 0x0000_00E3_9741_0000u64;
    for _ in 0..40_000 {
        let len = (splitmix(&mut seed) as usize) % 80;
        // Bias toward directive syntax so the parser reaches real states.
        let alphabet = b"@if()els endforeach{{ }}!!includ.abc0123 \n\t";
        let src: String = (0..len)
            .map(|_| alphabet[(splitmix(&mut seed) as usize) % alphabet.len()] as char)
            .collect();
        if let Ok(t) = Template::compile(&src) {
            // If it compiled, rendering against an arbitrary context must also
            // never panic.
            let ctx = Json::obj(vec![("x", Json::str("v")), ("items", Json::arr(vec![]))]);
            let _ = t.render(&ctx);
        }
    }
}

#[test]
fn deep_nesting_is_rejected_not_overflowed() {
    // Before the depth guard this recursed once per level at parse time and
    // could abort the process with a stack overflow.
    let deep_if = format!("{}x{}", "@if(a)".repeat(5_000), "@endif".repeat(5_000));
    let err = match Template::compile(&deep_if) {
        Err(e) => e,
        Ok(_) => panic!("expected a nesting error"),
    };
    assert!(
        err.contains("nesting"),
        "expected a nesting error, got: {err}"
    );

    let deep_each = format!(
        "{}x{}",
        "@foreach(items as i)".repeat(5_000),
        "@endforeach".repeat(5_000)
    );
    assert!(Template::compile(&deep_each).is_err());

    // A reasonably-nested template still compiles and renders.
    let ok = "@if(a)@if(b)hi@endif@endif";
    let t = Template::compile(ok).unwrap();
    let ctx = Json::obj(vec![("a", Json::Bool(true)), ("b", Json::Bool(true))]);
    assert_eq!(t.render(&ctx).unwrap(), "hi");
}

#[test]
fn include_cycles_terminate() {
    // A self-including template must hit the include-depth limit and error,
    // not recurse forever.
    let mut reg = Templates::new();
    reg.add("a", "x@include(a)").unwrap();
    let err = reg.render("a", &Json::Null).unwrap_err();
    assert!(
        err.contains("depth") || err.contains("include"),
        "got: {err}"
    );

    // Mutual recursion too.
    let mut reg = Templates::new();
    reg.add("a", "@include(b)").unwrap();
    reg.add("b", "@include(a)").unwrap();
    assert!(reg.render("a", &Json::Null).is_err());
}
