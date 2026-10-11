#[path = "support/diff_filter.rs"]
mod diff_filter;

use anywasm_cpu::cpu::{ARITH_FLAGS, DF};
use diff_filter::{flag_mask, skip};

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

#[test]
fn control_flow_sequences_compare_all_defined_arithmetic_flags() {
    let gpr = [0; 16];
    let fallthrough = "mov ecx, 3; xor eax, eax; add eax, 1; loop -5; cmp eax, 3; jne +2; inc eax";
    let taken = "mov ecx, 3; xor eax, eax; add eax, 1; loop -5; cmp eax, 4; jne +5; mov ebx, 4";

    // CMP defines every arithmetic flag. INC changes all but CF on the fallthrough path;
    // the taken path preserves CMP's flags through the MOV. Neither path changes DF.
    assert_eq!(flag_mask(fallthrough, &gpr), ARITH_FLAGS | DF);
    assert_eq!(flag_mask(taken, &gpr), ARITH_FLAGS | DF);
}
