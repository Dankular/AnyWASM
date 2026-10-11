pub fn mnemonic(asm: &str) -> &str {
    let mut it = asm.split_whitespace();
    let first = it.next().unwrap();
    if matches!(first, "rep" | "repe" | "repne") {
        it.next().unwrap()
    } else {
        first
    }
}

/// Results that legitimately differ from the host CPU or are nondeterministic.
pub fn skip(asm: &str) -> Option<&'static str> {
    asm.split(';')
        .map(str::trim)
        .filter(|instruction| !instruction.is_empty())
        .find_map(|instruction| match mnemonic(instruction) {
            "rdtsc" | "rdrand" | "cpuid" => Some("nondeterministic"),
            "rcpps" | "rsqrtps" | "rcpss" | "rsqrtss" => Some("hardware approximation"),
            _ => None,
        })
}
