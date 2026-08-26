//! CityHash64, the 1.0.x variant.
//!
//! This matters: SCS archives key every entry by CityHash64 of its path, and the
//! engine was built against CityHash 1.0.x. Version 1.1 rewrote HashLen0to16,
//! HashLen17to32 and HashLen33to64 - exactly the length range every archive path
//! falls into. Verified against a real archive: with 1.0.x all seven root
//! children of a HashFS v1 mod resolve, with 1.1 none do.

const K0: u64 = 0xc3a5c85c97cb3127;
const K1: u64 = 0xb492b66fbe98f273;
const K2: u64 = 0x9ae16a3b2f90404f;
const K3: u64 = 0xc949d7c7509e6557;

#[inline]
fn fetch64(s: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(s[i..i + 8].try_into().unwrap())
}

#[inline]
fn fetch32(s: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(s[i..i + 4].try_into().unwrap())
}

#[inline]
fn rotate(val: u64, shift: u32) -> u64 {
    if shift == 0 { val } else { (val >> shift) | (val << (64 - shift)) }
}

/// Only valid for shift in 1..=63; the caller guarantees that.
#[inline]
fn rotate_by_at_least_1(val: u64, shift: u32) -> u64 {
    (val >> shift) | (val << (64 - shift))
}

#[inline]
fn shift_mix(val: u64) -> u64 {
    val ^ (val >> 47)
}

#[inline]
fn hash128_to_64(lo: u64, hi: u64) -> u64 {
    const KMUL: u64 = 0x9ddfea08eb382d69;
    let mut a = (lo ^ hi).wrapping_mul(KMUL);
    a ^= a >> 47;
    let mut b = (hi ^ a).wrapping_mul(KMUL);
    b ^= b >> 47;
    b.wrapping_mul(KMUL)
}

#[inline]
fn hash_len16(u: u64, v: u64) -> u64 {
    hash128_to_64(u, v)
}

fn hash_len0to16(s: &[u8]) -> u64 {
    let n = s.len();
    if n > 8 {
        let a = fetch64(s, 0);
        let b = fetch64(s, n - 8);
        return hash_len16(a, rotate_by_at_least_1(b.wrapping_add(n as u64), n as u32)) ^ b;
    }
    if n >= 4 {
        let a = fetch32(s, 0) as u64;
        return hash_len16((n as u64).wrapping_add(a << 3), fetch32(s, n - 4) as u64);
    }
    if n > 0 {
        let a = s[0] as u64;
        let b = s[n >> 1] as u64;
        let c = s[n - 1] as u64;
        let y = a.wrapping_add(b << 8);
        let z = (n as u64).wrapping_add(c << 2);
        return shift_mix(y.wrapping_mul(K2) ^ z.wrapping_mul(K3)).wrapping_mul(K2);
    }
    K2
}

fn hash_len17to32(s: &[u8]) -> u64 {
    let n = s.len();
    let a = fetch64(s, 0).wrapping_mul(K1);
    let b = fetch64(s, 8);
    let c = fetch64(s, n - 8).wrapping_mul(K2);
    let d = fetch64(s, n - 16).wrapping_mul(K0);
    hash_len16(
        rotate(a.wrapping_sub(b), 43)
            .wrapping_add(rotate(c, 30))
            .wrapping_add(d),
        a.wrapping_add(rotate(b ^ K3, 20))
            .wrapping_sub(c)
            .wrapping_add(n as u64),
    )
}

fn weak_hash32_seeds(w: u64, x: u64, y: u64, z: u64, mut a: u64, mut b: u64) -> (u64, u64) {
    a = a.wrapping_add(w);
    b = rotate(b.wrapping_add(a).wrapping_add(z), 21);
    let c = a;
    a = a.wrapping_add(x);
    a = a.wrapping_add(y);
    b = b.wrapping_add(rotate(a, 44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

fn weak_hash32(s: &[u8], i: usize, a: u64, b: u64) -> (u64, u64) {
    weak_hash32_seeds(
        fetch64(s, i),
        fetch64(s, i + 8),
        fetch64(s, i + 16),
        fetch64(s, i + 24),
        a,
        b,
    )
}

fn hash_len33to64(s: &[u8]) -> u64 {
    let n = s.len();
    let mut z = fetch64(s, 24);
    let mut a = fetch64(s, 0)
        .wrapping_add((n as u64).wrapping_add(fetch64(s, n - 16)).wrapping_mul(K0));
    let mut b = rotate(a.wrapping_add(z), 52);
    let mut c = rotate(a, 37);
    a = a.wrapping_add(fetch64(s, 8));
    c = c.wrapping_add(rotate(a, 7));
    a = a.wrapping_add(fetch64(s, 16));
    let vf = a.wrapping_add(z);
    let vs = b.wrapping_add(rotate(a, 31)).wrapping_add(c);

    a = fetch64(s, 16).wrapping_add(fetch64(s, n - 32));
    z = fetch64(s, n - 8);
    b = rotate(a.wrapping_add(z), 52);
    c = rotate(a, 37);
    a = a.wrapping_add(fetch64(s, n - 24));
    c = c.wrapping_add(rotate(a, 7));
    a = a.wrapping_add(fetch64(s, n - 16));
    let wf = a.wrapping_add(z);
    let ws = b.wrapping_add(rotate(a, 31)).wrapping_add(c);

    let r = shift_mix(
        vf.wrapping_add(ws)
            .wrapping_mul(K2)
            .wrapping_add(wf.wrapping_add(vs).wrapping_mul(K0)),
    );
    shift_mix(r.wrapping_mul(K0).wrapping_add(vs)).wrapping_mul(K2)
}

pub fn city_hash64(data: &[u8]) -> u64 {
    let n = data.len();
    if n <= 32 {
        return if n <= 16 { hash_len0to16(data) } else { hash_len17to32(data) };
    }
    if n <= 64 {
        return hash_len33to64(data);
    }

    // The >64 loop is NOT the 1.0.x one - SCS uses the later (1.1) form here even
    // though its short branches are 1.0.x. Established empirically: over 4773 real
    // paths from a shipped mod, 1.0.x short branches score 2322/2322 on <=64 and
    // 0/2451 on >64, while this loop scores 2451/2451. Do not "fix" this to match
    // any single published CityHash release.
    let s = data;
    let mut x = fetch64(s, n - 40);
    let mut y = fetch64(s, n - 16).wrapping_add(fetch64(s, n - 56));
    let mut z = hash_len16(fetch64(s, n - 48).wrapping_add(n as u64), fetch64(s, n - 24));
    let mut v = weak_hash32(s, n - 64, n as u64, z);
    let mut w = weak_hash32(s, n - 32, y.wrapping_add(K1), x);
    x = x.wrapping_mul(K1).wrapping_add(fetch64(s, 0));

    let mut len = (n - 1) & !63;
    let mut i = 0usize;
    loop {
        x = rotate(
            x.wrapping_add(y).wrapping_add(v.0).wrapping_add(fetch64(s, i + 8)),
            37,
        )
        .wrapping_mul(K1);
        y = rotate(y.wrapping_add(v.1).wrapping_add(fetch64(s, i + 48)), 42).wrapping_mul(K1);
        x ^= w.1;
        y = y.wrapping_add(v.0).wrapping_add(fetch64(s, i + 40));
        z = rotate(z.wrapping_add(w.0), 33).wrapping_mul(K1);
        v = weak_hash32(s, i, v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = weak_hash32(
            s,
            i + 32,
            z.wrapping_add(w.1),
            y.wrapping_add(fetch64(s, i + 16)),
        );
        std::mem::swap(&mut z, &mut x);
        i += 64;
        len -= 64;
        if len == 0 {
            break;
        }
    }

    hash_len16(
        hash_len16(v.0, w.0)
            .wrapping_add(shift_mix(y).wrapping_mul(K1))
            .wrapping_add(z),
        hash_len16(v.1, w.1).wrapping_add(x),
    )
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Every vector below was read out of a real shipped HashFS v1 mod: the path
    /// exists in the archive and the hash is the key its entry is stored under.
    /// They cover all four length branches.
    #[test]
    fn vectors_from_a_real_archive() {
        let cases: &[(&str, u64)] = &[
            // len 0 - the archive root
            ("", 0x9ae16a3b2f90404f),
            // len 1..16
            ("def", 0x2c6f469efb31c45a),
            ("unit", 0x2c5cc344e4724a8d),
            ("sound", 0x5993d862b6353dfd),
            ("ui", 0x9ec7c23efdbecbe3),
            ("manifest.sii", 0xb97fff7ce7377c95),
            ("logo.jpg", 0x9e8aa1f54dd600f9),
            ("def/vehicle", 0x24fdaf288cc01f62),
            // len 17..32
            ("mod_description.txt", 0xe6ef2b3d04463499),
            // len 33..64
            ("vehicle/truck/thaco_mbh_25/truck.pmg", 0x2baf66afe1257aae),
            (
                "unit/hookup/vehicle/flare/vehicle_yellow_led_d.sii",
                0xf16d701cb8f70245,
            ),
            (
                "def/vehicle/truck/thaco.mbh.25/accessory/steering_w/standard.sii",
                0xa840151fcbce38bf,
            ),
        ];
        for (path, expect) in cases {
            assert_eq!(
                city_hash64(path.as_bytes()),
                *expect,
                "path {:?} (len {})",
                path,
                path.len()
            );
        }
    }

    #[test]
    fn long_path_uses_the_right_loop() {
        // >64 bytes: the branch that a naive 1.0.x port gets wrong
        let cases: &[(&str, u64)] = &[
            (
                "def/vehicle/trailer_owned/crsthn.t_passag/chassis/chassis_incompatible.sui",
                0x6598e9df95dee91a,
            ),
            (
                "def/vehicle/trailer_owned/passenger/configurations/single/single.sii",
                0xf3b5875135b82b06,
            ),
        ];
        for (p, expect) in cases {
            assert!(p.len() > 64);
            assert_eq!(city_hash64(p.as_bytes()), *expect, "path {:?}", p);
        }
    }
}


