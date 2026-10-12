//! C `printf`-family formatting engine with Windows (LLP64) semantics.

use crate::process::{Process, Stop};

/// Source of variadic arguments.
pub trait Args {
    fn next(&mut self, p: &Process) -> Result<u64, Stop>;
}

/// Direct call: arguments are the Win64 argument slots starting at `n`
/// (variadic floating-point values are duplicated in the integer slots by the caller).
pub struct SlotArgs(pub usize);
impl Args for SlotArgs {
    fn next(&mut self, p: &Process) -> Result<u64, Stop> {
        let v = p.arg(self.0);
        self.0 += 1;
        Ok(v)
    }
}

/// `va_list` on Win64 is a pointer to consecutive 8-byte argument slots.
pub struct VaList(pub u64);
impl Args for VaList {
    fn next(&mut self, p: &Process) -> Result<u64, Stop> {
        let v = p.rd64(self.0)?;
        self.0 += 8;
        Ok(v)
    }
}

#[derive(Default, Clone, Copy)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    prec: Option<usize>,
}

fn pad(out: &mut Vec<u8>, s: &[u8], sp: &Spec, numeric: bool) {
    let n = s.len();
    if n >= sp.width {
        out.extend_from_slice(s);
    } else if sp.left {
        out.extend_from_slice(s);
        out.resize(out.len() + sp.width - n, b' ');
    } else if sp.zero && numeric {
        // zeros go after the sign / 0x prefix
        let split = if s.starts_with(b"0x") || s.starts_with(b"0X") {
            2
        } else if matches!(s.first(), Some(b'-' | b'+' | b' ')) {
            1
        } else {
            0
        };
        out.extend_from_slice(&s[..split]);
        out.resize(out.len() + sp.width - n, b'0');
        out.extend_from_slice(&s[split..]);
    } else {
        out.resize(out.len() + sp.width - n, b' ');
        out.extend_from_slice(s);
    }
}

fn fmt_exp(v: f64, prec: usize, upper: bool) -> String {
    let s = format!("{:.*e}", prec, v);
    let (m, e) = s.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap();
    let r = format!("{}e{}{:02}", m, if e < 0 { '-' } else { '+' }, e.abs());
    if upper {
        r.to_uppercase()
    } else {
        r
    }
}

fn strip_zeros(s: &str) -> String {
    if let Some((mant, exp)) = s.split_once(['e', 'E']) {
        let sep = if s.contains('E') { 'E' } else { 'e' };
        let m = if mant.contains('.') {
            mant.trim_end_matches('0').trim_end_matches('.')
        } else {
            mant
        };
        format!("{m}{sep}{exp}")
    } else if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

fn fmt_float(v: f64, conv: u8, sp: &Spec) -> String {
    let upper = conv.is_ascii_uppercase();
    if v.is_nan() {
        return if upper { "NAN".into() } else { "nan".into() };
    }
    let neg = v.is_sign_negative();
    let a = v.abs();
    let body = if a.is_infinite() {
        if upper {
            "INF".to_string()
        } else {
            "inf".to_string()
        }
    } else {
        let prec = sp.prec.unwrap_or(6);
        match conv.to_ascii_lowercase() {
            b'f' => {
                let s = format!("{:.*}", prec, a);
                if sp.alt && prec == 0 {
                    s + "."
                } else {
                    s
                }
            }
            b'e' => fmt_exp(a, prec, upper),
            b'g' => {
                let p = if prec == 0 { 1 } else { prec };
                if a == 0.0 {
                    if sp.alt {
                        format!("{:.*}", p - 1, 0.0)
                    } else {
                        "0".into()
                    }
                } else {
                    let e_form = fmt_exp(a, p - 1, false);
                    let x: i32 = e_form.split_once('e').unwrap().1.parse().unwrap();
                    let s = if x >= -4 && x < p as i32 {
                        format!("{:.*}", (p as i32 - 1 - x).max(0) as usize, a)
                    } else {
                        e_form
                    };
                    let s = if sp.alt { s } else { strip_zeros(&s) };
                    if upper {
                        s.to_uppercase()
                    } else {
                        s
                    }
                }
            }
            _ => format!("{a}"),
        }
    };
    let sign = if neg {
        "-"
    } else if sp.plus {
        "+"
    } else if sp.space {
        " "
    } else {
        ""
    };
    format!("{sign}{body}")
}

pub(crate) fn format_ostream_float(value: f64, precision: usize, flags: u32) -> String {
    let conv = match flags & 0x3000 {
        0x2000 => b'f', // fixed
        0x1000 => b'e', // scientific
        _ => b'g',      // defaultfloat (and the current hexfloat fallback)
    };
    let conv = if flags & 0x4 != 0 {
        conv.to_ascii_uppercase()
    } else {
        conv
    };
    let spec = Spec {
        plus: flags & 0x20 != 0,
        alt: flags & 0x10 != 0,
        prec: Some(precision),
        ..Spec::default()
    };
    let mut output = fmt_float(value, conv, &spec);
    if flags & 0x10 != 0 && value.is_finite() && !output.contains('.') {
        let point = output.find(['e', 'E']).unwrap_or(output.len());
        output.insert(point, '.');
    }
    output
}

fn wide_to_utf8(p: &Process, a: u64, max: Option<usize>) -> Result<Vec<u8>, Stop> {
    let mut u = Vec::new();
    let mut i = 0u64;
    loop {
        if let Some(m) = max {
            if i as usize >= m {
                break;
            }
        }
        let c = p.rd16(a + i * 2)?;
        if c == 0 {
            break;
        }
        u.push(c);
        i += 1;
    }
    Ok(String::from_utf16_lossy(&u).into_bytes())
}

fn narrow_bytes(p: &Process, a: u64, max: Option<usize>) -> Result<Vec<u8>, Stop> {
    let mut v = Vec::new();
    let mut i = 0u64;
    loop {
        if let Some(m) = max {
            if i as usize >= m {
                break;
            }
        }
        let c = p.rd8(a + i)?;
        if c == 0 {
            break;
        }
        v.push(c);
        i += 1;
    }
    Ok(v)
}

/// Format `fmt` (UTF-8 / raw bytes). `wide_default` makes `%s`/`%c` mean wide (wprintf family).
/// The result is raw bytes (UTF-8 for wide strings).
pub fn format(
    p: &Process,
    fmt: &[u8],
    args: &mut dyn Args,
    wide_default: bool,
) -> Result<Vec<u8>, Stop> {
    let mut out = Vec::with_capacity(fmt.len() + 16);
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        if c != b'%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= fmt.len() {
            break;
        }
        let mut sp = Spec::default();
        while i < fmt.len() {
            match fmt[i] {
                b'-' => sp.left = true,
                b'+' => sp.plus = true,
                b' ' => sp.space = true,
                b'#' => sp.alt = true,
                b'0' => sp.zero = true,
                _ => break,
            }
            i += 1;
        }
        if i < fmt.len() && fmt[i] == b'*' {
            let w = args.next(p)? as i32;
            if w < 0 {
                sp.left = true;
            }
            sp.width = w.unsigned_abs() as usize;
            i += 1;
        } else {
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                sp.width = sp.width * 10 + (fmt[i] - b'0') as usize;
                i += 1;
            }
        }
        if i < fmt.len() && fmt[i] == b'.' {
            i += 1;
            if i < fmt.len() && fmt[i] == b'*' {
                let v = args.next(p)? as i32;
                sp.prec = if v < 0 { None } else { Some(v as usize) };
                i += 1;
            } else {
                let mut v = 0;
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    v = v * 10 + (fmt[i] - b'0') as usize;
                    i += 1;
                }
                sp.prec = Some(v);
            }
        }
        // length
        let mut size = 4u8; // bytes of the integer argument
        let mut wide_arg: Option<bool> = None;
        loop {
            match fmt.get(i) {
                Some(b'h') => {
                    size = if size == 2 { 1 } else { 2 };
                    wide_arg = Some(false);
                    i += 1;
                }
                Some(b'l') => {
                    if fmt.get(i + 1) == Some(&b'l') {
                        size = 8;
                        i += 2;
                    } else {
                        // `long` is 32-bit on Windows; for strings/chars it selects wide.
                        wide_arg = Some(true);
                        i += 1;
                    }
                }
                Some(b'L') | Some(b'j') | Some(b'z') | Some(b't') | Some(b'q') => {
                    size = 8;
                    i += 1;
                }
                Some(b'w') => {
                    wide_arg = Some(true);
                    i += 1;
                }
                Some(b'I') => {
                    if fmt[i..].starts_with(b"I64") {
                        size = 8;
                        i += 3;
                    } else if fmt[i..].starts_with(b"I32") {
                        size = 4;
                        i += 3;
                    } else {
                        size = 8;
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        match conv {
            b'%' => out.push(b'%'),
            b'd' | b'i' => {
                let raw = args.next(p)?;
                let v: i64 = match size {
                    1 => raw as i8 as i64,
                    2 => raw as i16 as i64,
                    4 => raw as i32 as i64,
                    _ => raw as i64,
                };
                let mut digits = v.unsigned_abs().to_string();
                if let Some(pr) = sp.prec {
                    while digits.len() < pr {
                        digits.insert(0, '0');
                    }
                    if pr == 0 && v == 0 {
                        digits.clear();
                    }
                }
                let sign = if v < 0 {
                    "-"
                } else if sp.plus {
                    "+"
                } else if sp.space {
                    " "
                } else {
                    ""
                };
                let mut s = format!("{sign}{digits}");
                if sp.prec.is_some() {
                    sp.zero = false;
                }
                pad(&mut out, std::mem::take(&mut s).as_bytes(), &sp, true);
            }
            b'u' | b'o' | b'x' | b'X' => {
                let raw = args.next(p)?;
                let v: u64 = match size {
                    1 => raw & 0xff,
                    2 => raw & 0xffff,
                    4 => raw & 0xffff_ffff,
                    _ => raw,
                };
                let mut digits = match conv {
                    b'u' => v.to_string(),
                    b'o' => format!("{v:o}"),
                    b'x' => format!("{v:x}"),
                    _ => format!("{v:X}"),
                };
                if let Some(pr) = sp.prec {
                    while digits.len() < pr {
                        digits.insert(0, '0');
                    }
                    if pr == 0 && v == 0 {
                        digits.clear();
                    }
                    sp.zero = false;
                }
                if sp.alt && v != 0 {
                    match conv {
                        b'x' => digits.insert_str(0, "0x"),
                        b'X' => digits.insert_str(0, "0X"),
                        b'o' => digits.insert(0, '0'),
                        _ => {}
                    }
                }
                pad(&mut out, digits.as_bytes(), &sp, true);
            }
            b'p' => {
                let v = args.next(p)?;
                let s = format!("{:016X}", v);
                pad(&mut out, s.as_bytes(), &sp, false);
            }
            b'c' | b'C' => {
                let v = args.next(p)? as u32;
                let wide = if conv == b'C' {
                    !wide_default
                } else {
                    wide_arg.unwrap_or(wide_default)
                };
                let s: Vec<u8> = if wide || v > 0x7f && wide_default {
                    char::from_u32(v & 0xffff)
                        .map(|ch| ch.to_string().into_bytes())
                        .unwrap_or_default()
                } else {
                    vec![v as u8]
                };
                pad(&mut out, &s, &sp, false);
            }
            b's' | b'S' => {
                let a = args.next(p)?;
                let wide = if conv == b'S' {
                    !wide_default
                } else {
                    wide_arg.unwrap_or(wide_default)
                };
                let bytes = if a == 0 {
                    b"(null)".to_vec()
                } else if wide {
                    wide_to_utf8(p, a, sp.prec)?
                } else {
                    narrow_bytes(p, a, sp.prec)?
                };
                sp.zero = false;
                pad(&mut out, &bytes, &sp, false);
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                let v = f64::from_bits(args.next(p)?);
                let s = fmt_float(v, conv, &sp);
                let finite = v.is_finite();
                pad(&mut out, s.as_bytes(), &sp, finite);
            }
            b'n' => {
                let a = args.next(p)?;
                let _ = a; // %n is disabled in the UCRT by default
            }
            other => {
                out.push(b'%');
                out.push(other);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct V(Vec<u64>, usize);
    impl Args for V {
        fn next(&mut self, _p: &Process) -> Result<u64, Stop> {
            let v = self.0[self.1];
            self.1 += 1;
            Ok(v)
        }
    }
    fn f(fmt: &str, a: Vec<u64>) -> String {
        let p = Process::new(crate::Vfs::new());
        String::from_utf8(format(&p, fmt.as_bytes(), &mut V(a, 0), false).unwrap()).unwrap()
    }

    #[test]
    fn integers() {
        assert_eq!(
            f("%d|%5d|%-5d|%05d|%+d", vec![42, 42, 42, 42, 42]),
            "42|   42|42   |00042|+42"
        );
        assert_eq!(
            f(
                "%x %X %#x %o %u",
                vec![255, 255, 255, 8, (-1i32) as u32 as u64]
            ),
            "ff FF 0xff 10 4294967295"
        );
        assert_eq!(
            f(
                "%lld %I64d %hd %hhd",
                vec![-5i64 as u64, 1 << 40, 70000, 300]
            ),
            "-5 1099511627776 4464 44"
        );
        assert_eq!(f("%.3d|%.0d|%d", vec![5, 0, (-7i64) as u64]), "005||-7");
    }

    #[test]
    fn floats() {
        let b = |x: f64| x.to_bits();
        assert_eq!(
            f(
                "%f %.2f %8.3f %-8.1f|",
                vec![b(3.14159), b(2.0), b(-1.5), b(9.99)]
            ),
            "3.141590 2.00   -1.500 10.0    |"
        );
        assert_eq!(
            f("%e %E %.0e", vec![b(12345.678), b(0.00012), b(5.0)]),
            "1.234568e+04 1.200000E-04 5e+00"
        );
        assert_eq!(
            f(
                "%g %g %g %g %g",
                vec![b(100000.0), b(1e6), b(0.0001), b(0.00001), b(1.5)]
            ),
            "100000 1e+06 0.0001 1e-05 1.5"
        );
        assert_eq!(f("%g|%G", vec![b(f64::INFINITY), b(f64::NAN)]), "inf|NAN");
        assert_eq!(f("%.3g %.10g", vec![b(1234.5678), b(0.1)]), "1.23e+03 0.1");
    }

    #[test]
    fn misc() {
        assert_eq!(f("100%%", vec![]), "100%");
        assert_eq!(f("%c%c", vec![72, 105]), "Hi");
        assert_eq!(f("%*d|%-*d|", vec![6, 42, 4, 7]), "    42|7   |");
        assert_eq!(f("%p", vec![0xdeadbeef]), "00000000DEADBEEF");
    }
}
