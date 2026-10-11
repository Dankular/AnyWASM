#[path = "support/diff_filter.rs"]
mod diff_filter;

use diff_filter::skip;

#[test]
fn excludes_nondeterministic_instructions_inside_sequences() {
    assert_eq!(skip("nop; rdtsc; nop"), Some("nondeterministic"));
}

#[test]
fn excludes_approximate_instructions_inside_sequences() {
    assert_eq!(
        skip("movaps xmm0, xmm1; rsqrtps xmm0, xmm0"),
        Some("hardware approximation")
    );
}
