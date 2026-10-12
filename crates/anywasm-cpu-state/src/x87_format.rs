//! Allocation-free conversions between the stored f64 x87 values and the 80-bit FXSAVE format.

pub fn f64_to_f80(x: f64) -> [u8; 10] {
    let bits = x.to_bits();
    let sign = (bits >> 63) as u16;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (e, m): (u16, u64) = if exp == 0 {
        if frac == 0 {
            (0, 0)
        } else {
            let lz = frac.leading_zeros() as i32 - 11;
            let m = frac << (lz + 11);
            let e = 1 - 1023 - lz + 16383;
            (e as u16, m)
        }
    } else if exp == 0x7ff {
        (0x7fff, (1u64 << 63) | (frac << 11))
    } else {
        ((exp - 1023 + 16383) as u16, (1u64 << 63) | (frac << 11))
    };
    let mut out = [0u8; 10];
    out[..8].copy_from_slice(&m.to_le_bytes());
    out[8..].copy_from_slice(&(e | sign << 15).to_le_bytes());
    out
}

pub fn f80_to_f64(bytes: &[u8; 10]) -> f64 {
    let m = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let sign_exp = u16::from_le_bytes([bytes[8], bytes[9]]);
    let sign = (sign_exp >> 15) as u64;
    let exponent = (sign_exp & 0x7fff) as i32;
    if exponent == 0 && m == 0 {
        return f64::from_bits(sign << 63);
    }
    if exponent == 0x7fff {
        let frac = (m << 1) >> 12;
        let nan = (m << 1) != 0;
        return f64::from_bits(sign << 63 | 0x7ff << 52 | if nan { frac | 1 << 51 } else { 0 });
    }

    // Apply the wide exponent in exact normal powers-of-two chunks, avoiding intermediate
    // underflow and overflow before the final f64 rounding.
    let mut value = m as f64;
    let mut power = exponent - 16383 - 63;
    while power != 0 {
        let step = power.clamp(-1000, 1000);
        let scale = f64::from_bits(((step + 1023) as u64) << 52);
        value *= scale;
        power -= step;
    }
    if sign != 0 {
        -value
    } else {
        value
    }
}
