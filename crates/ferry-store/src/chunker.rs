use std::fmt;
use std::sync::Mutex;

use rand::Rng;
use thiserror::Error;

pub const WINDOW_SIZE: usize = 64;

pub const MIN_SIZE: usize = 524_288;

pub const AVG_BITS: u32 = 20;

pub const SPLIT_MASK: u64 = (1 << AVG_BITS) - 1;

pub const MAX_SIZE: usize = 8_388_608;

pub const POLY_DEGREE: u32 = 53;

const SLIDE_OUT_X: u32 = 504;

#[derive(Debug, Error)]
#[error("polynomial {0:#x} is not monic irreducible of degree 53")]
pub struct PolynomialError(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidatedPoly(u64);

impl ValidatedPoly {
    pub fn new(p: u64) -> Result<Self, PolynomialError> {
        if !is_irreducible(p) {
            return Err(PolynomialError(p));
        }
        Ok(ValidatedPoly(p))
    }

    pub fn generate(rng: &mut impl Rng) -> Self {
        ValidatedPoly(generate_polynomial(rng))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for ValidatedPoly {
    type Error = PolynomialError;

    fn try_from(p: u64) -> Result<Self, Self::Error> {
        ValidatedPoly::new(p)
    }
}

impl From<ValidatedPoly> for u64 {
    fn from(p: ValidatedPoly) -> u64 {
        p.0
    }
}

fn poly_degree(v: u64) -> Option<u32> {
    if v == 0 {
        None
    } else {
        Some(v.ilog2())
    }
}

pub fn gf_mul(a: u64, b: u64) -> u64 {
    let mut acc = 0u64;
    let mut b = b;
    while b != 0 {
        let i = b.trailing_zeros();
        acc ^= a << i;
        b &= b - 1;
    }
    acc
}

fn gf_mod_deg(mut v: u64, p: u64, d: u32) -> u64 {
    loop {
        let dv = match poly_degree(v) {
            Some(dv) if dv >= d => dv,
            _ => return v,
        };
        v ^= p << (dv - d);
    }
}

pub fn gf_mod(v: u64, p: u64) -> u64 {
    gf_mod_deg(v, p, POLY_DEGREE)
}

#[cfg(test)]
fn poly_degree128(v: u128) -> u32 {
    if v == 0 {
        0
    } else {
        v.ilog2()
    }
}

fn mulmod_deg(mut a: u64, b: u64, p: u64, d: u32) -> u64 {
    a = gf_mod_deg(a, p, d);
    let db = poly_degree(b).unwrap_or(0);
    let mut acc = 0u64;
    for i in (0..=db).rev() {
        acc = gf_mod_deg(acc << 1, p, d);
        if (b >> i) & 1 == 1 {
            acc ^= a;
        }
    }
    acc
}

pub fn gf_mulmod(a: u64, b: u64, p: u64) -> u64 {
    mulmod_deg(a, b, p, POLY_DEGREE)
}

pub fn gf_pow_x(n: u32, p: u64) -> u64 {
    let mut v = 1u64;
    for _ in 0..n {
        v = gf_mod_deg(v << 1, p, POLY_DEGREE);
    }
    v
}

#[cfg(test)]
fn pow_x_square_multiply(n: u32, p: u64) -> u64 {
    let base = n;
    let mut result = 1u64;
    let mut sq = 2u64;
    let mut k = 0u32;
    while (1u32 << k) <= base {
        if (n >> k) & 1 == 1 {
            result = gf_mulmod(result, sq, p);
        }
        sq = gf_mulmod(sq, sq, p);
        k += 1;
    }
    result
}

pub fn is_irreducible(p: u64) -> bool {
    if p >> POLY_DEGREE != 1 {
        return false;
    }

    if (p & 1) == 0 || p.count_ones().is_multiple_of(2) {
        return false;
    }

    let mut g = 2u64;
    for _ in 0..POLY_DEGREE {
        g = gf_mulmod(g, g, p);
    }
    g == 2
}

pub fn generate_polynomial(rng: &mut impl Rng) -> u64 {
    loop {
        let candidate = (rng.gen::<u64>() & ((1 << POLY_DEGREE) - 1)) | (1 << POLY_DEGREE);
        if is_irreducible(candidate) {
            return candidate;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    Natural,

    Max,
}

impl fmt::Display for Cut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cut::Natural => f.write_str("natural"),
            Cut::Max => f.write_str("max"),
        }
    }
}

pub struct Chunker {
    poly: u64,
    win: [u8; WINDOW_SIZE],
    wpos: usize,
    filled: usize,
    fp: u64,
    len: usize,
    tables: &'static DerivedTables,
}

struct DerivedTables {
    out_table: [u64; 256],
}

fn derived_tables(p: u64) -> &'static DerivedTables {
    type Memo = std::collections::HashMap<u64, &'static DerivedTables>;
    static MEMO: std::sync::OnceLock<Mutex<Memo>> = std::sync::OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut guard = memo
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.entry(p).or_insert_with(|| {
        let slide_out = gf_pow_x(SLIDE_OUT_X, p);
        let mut out_table = [0u64; 256];
        for (i, slot) in out_table.iter_mut().enumerate() {
            *slot = gf_mod(gf_mul(i as u64, slide_out), p);
        }
        Box::leak(Box::new(DerivedTables { out_table }))
    })
}

impl Chunker {
    pub fn new(p: u64) -> Result<Self, PolynomialError> {
        if !is_irreducible(p) {
            return Err(PolynomialError(p));
        }
        Ok(Chunker {
            poly: p,
            win: [0; WINDOW_SIZE],
            wpos: 0,
            filled: 0,
            fp: 0,
            len: 0,
            tables: derived_tables(p),
        })
    }

    pub fn reset(&mut self) {
        self.win = [0; WINDOW_SIZE];
        self.wpos = 0;
        self.filled = 0;
        self.fp = 0;
        self.len = 0;
    }

    pub fn pending_len(&self) -> usize {
        self.len
    }

    pub fn push(&mut self, b: u8) -> Option<usize> {
        self.append(b);
        self.len += 1;
        if self.cut_at_this_byte().is_some() {
            let finished = self.len;
            self.reset();
            return Some(finished);
        }
        None
    }

    fn cut_at_this_byte(&self) -> Option<Cut> {
        if self.len >= MIN_SIZE && (self.fp & SPLIT_MASK) == 0 {
            Some(Cut::Natural)
        } else if self.len == MAX_SIZE {
            Some(Cut::Max)
        } else {
            None
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<usize> {
        let mut out = Vec::new();
        for &b in data {
            if let Some(len) = self.push(b) {
                out.push(len);
            }
        }
        out
    }

    pub fn finish(&self) -> usize {
        self.pending_len()
    }

    fn append(&mut self, b: u8) {
        if self.filled < WINDOW_SIZE {
            self.win[self.wpos] = b;
            self.wpos = (self.wpos + 1) % WINDOW_SIZE;
            self.filled += 1;
            self.fp = gf_mod((self.fp << 8) | u64::from(b), self.poly);
        } else {
            let out = self.win[self.wpos];
            self.win[self.wpos] = b;
            self.wpos = (self.wpos + 1) % WINDOW_SIZE;
            self.fp ^= self.tables.out_table[out as usize];
            self.fp = gf_mod((self.fp << 8) | u64::from(b), self.poly);
        }
    }
}

pub fn chunk_offsets(poly: u64, data: &[u8]) -> Result<Vec<(usize, usize)>, PolynomialError> {
    let mut c = Chunker::new(poly)?;
    let mut out = Vec::new();
    let mut start = 0usize;
    for len in c.feed(data) {
        out.push((start, len));
        start += len;
    }
    let tail = c.finish();
    if tail > 0 {
        out.push((start, tail));
    }
    Ok(out)
}

pub fn chunk(poly: u64, data: &[u8]) -> Result<Vec<&[u8]>, PolynomialError> {
    Ok(chunk_offsets(poly, data)?
        .into_iter()
        .map(|(off, len)| &data[off..off + len])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    fn test_poly(seed: u64) -> u64 {
        generate_polynomial(&mut StdRng::seed_from_u64(seed))
    }

    fn prng_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..len).map(|_| rng.gen()).collect()
    }

    #[test]
    fn gf_mul_matches_shift_xor_reference() {
        let mut rng = StdRng::seed_from_u64(1);
        for _ in 0..2000 {
            let a = rng.gen::<u64>() & ((1 << 28) - 1);
            let b = rng.gen::<u64>() & ((1 << 28) - 1);
            let mut expect = 0u64;
            for i in 0..64 {
                if (b >> i) & 1 == 1 {
                    expect ^= a << i;
                }
            }
            assert_eq!(gf_mul(a, b), expect, "gf_mul({a:#x}, {b:#x})");
        }
    }

    #[test]
    fn gf_distributes_over_xor() {
        let mut rng = StdRng::seed_from_u64(2);
        for _ in 0..500 {
            let (a, b, c) = (
                rng.gen::<u64>() & ((1 << 20) - 1),
                rng.gen::<u64>() & ((1 << 20) - 1),
                rng.gen::<u64>() & ((1 << 20) - 1),
            );
            assert_eq!(
                gf_mul(a ^ b, c),
                gf_mul(a, c) ^ gf_mul(b, c),
                "left distributivity"
            );
        }
    }

    #[test]
    fn gf_mod_leaves_degree_below_poly_and_agrees_with_u128_reference() {
        let mut rng = StdRng::seed_from_u64(3);
        let p = test_poly(7);
        for _ in 0..2000 {
            let v = rng.gen::<u64>();
            let r = gf_mod(v, p);
            assert!(poly_degree(r).unwrap_or(0) < POLY_DEGREE || r == 0);

            let mut wide = u128::from(v);
            let pw = u128::from(p);
            while poly_degree128(wide) >= POLY_DEGREE {
                wide ^= pw << (poly_degree128(wide) - POLY_DEGREE);
            }
            assert_eq!(u128::from(r), wide, "gf_mod({v:#x})");
        }
    }

    #[allow(clippy::many_single_char_names)]
    #[test]
    fn gf_mulmod_matches_gf_mul_then_mod() {
        let p = test_poly(11);
        let mut rng = StdRng::seed_from_u64(4);
        for _ in 0..1000 {
            let a = rng.gen::<u64>() & ((1 << 5) - 1);
            let b = rng.gen::<u64>() & ((1 << 5) - 1);
            assert_eq!(gf_mulmod(a, b, p), gf_mod(gf_mul(a, b), p));
        }

        let a = 0x123456789abcdef;
        let b = 0x0f1e2d3c4b5a6978 & ((1 << 53) - 1);
        let r = gf_mulmod(a, b, p);
        assert!(r < (1 << 53));

        let c = 0x00ff00ff00ff00ff & ((1 << 53) - 1);
        assert_eq!(
            gf_mulmod(a ^ c, b, p),
            gf_mulmod(a, b, p) ^ gf_mulmod(c, b, p)
        );
    }

    #[test]
    fn gf_pow_x_agrees_between_doubling_and_square_multiply() {
        for seed in [7u64, 8, 9] {
            let p = test_poly(seed);
            assert_eq!(gf_pow_x(0, p), 1, "x^0 = 1");
            assert_eq!(gf_pow_x(1, p), 2, "x^1 = 2");
            for n in [2u32, 3, 8, 63, 64, 504, 1000, 65_535] {
                let by_doubling = gf_pow_x(n, p);
                let by_sqmul = pow_x_square_multiply(n, p);
                assert_eq!(by_doubling, by_sqmul, "x^{n} mod {p:#x}");
            }
        }
    }

    #[test]
    fn rabin_test_agrees_with_trial_division_on_all_small_prime_degrees() {
        fn rabin_generic(p: u64, d: u32) -> bool {
            if poly_degree(p) != Some(d) {
                return false;
            }

            let mut v = 2u64;
            for _ in 0..d {
                v = mulmod_deg(v, v, p, d);
            }
            if v != 2 {
                return false;
            }

            (p & 1) == 1 && p.count_ones() % 2 == 1
        }

        fn trial_division(p: u64, d: u32) -> bool {
            for k in 1..=(d / 2) {
                for q in (1u64 << k)..(1u64 << (k + 1)) {
                    if is_irreducible_by_rabin_generic(q, k, d) && gf_mod_deg(p, q, k) == 0 {
                        return false;
                    }
                }
            }
            true
        }

        fn is_irreducible_by_rabin_generic(q: u64, k: u32, _ctx: u32) -> bool {
            if !k.is_power_of_two() && !is_prime(k) {
                return false;
            }

            if k <= 1 {
                return true;
            }
            let mut v = 2u64;
            for _ in 0..k {
                v = mulmod_deg(v, v, q, k);
            }
            let frobenius_ok = v == 2;
            let coprime_ok = (q & 1) == 1 && q.count_ones() % 2 == 1;
            frobenius_ok && coprime_ok && { true }
        }

        fn is_prime(n: u32) -> bool {
            n >= 2 && (2..n).all(|i| !n.is_multiple_of(i))
        }

        for d in [2u32, 3, 5, 7] {
            for p in (1u64 << d)..(1u64 << (d + 1)) {
                assert_eq!(
                    rabin_generic(p, d),
                    trial_division(p, d),
                    "degree {d}, p={p:#x}"
                );
            }
        }
    }

    #[test]
    fn rabin_test_rejects_known_shapes() {
        assert!(!is_irreducible(1u64 << 53));

        assert!(!is_irreducible((1u64 << 53) | 0b11));

        assert!(!is_irreducible(0));
        assert!(!is_irreducible(1));
        assert!(!is_irreducible(1u64 << 54));
    }

    #[test]
    fn generate_polynomial_yields_valid_distinct_polynomials() {
        let mut rng = StdRng::seed_from_u64(42);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..24 {
            let p = generate_polynomial(&mut rng);

            assert_eq!(p >> 53, 1);
            assert_eq!(p >> 54, 0);
            assert!(is_irreducible(p));
            seen.insert(p);
        }

        assert_eq!(seen.len(), 24);
    }

    #[test]
    fn chunker_new_rejects_invalid_polynomials() {
        assert!(Chunker::new(test_poly(5)).is_ok());
        assert!(Chunker::new(0).is_err());
        assert!(Chunker::new(1u64 << 53).is_err());
        assert!(Chunker::new(0x1234).is_err());
    }

    #[test]
    fn free_functions_return_typed_error_on_reducible_polynomial() {
        let bad = 1u64 << 53;
        let err = chunk_offsets(bad, b"hello ferry").unwrap_err();
        assert_eq!(err.0, bad, "the error names the offending polynomial");
        assert!(matches!(chunk(bad, &[1, 2, 3]), Err(PolynomialError(p)) if p == bad));

        let good = test_poly(41);
        assert!(chunk_offsets(good, b"hello ferry").is_ok());
    }

    #[test]
    fn clamping_order_min_gates_split_max_fires_only_without_natural_cut() {
        let mut c = Chunker::new(test_poly(6)).unwrap();

        c.len = MIN_SIZE - 1;
        c.fp = 0;
        assert!(c.cut_at_this_byte().is_none());

        c.len = MIN_SIZE;
        c.fp = 0;
        assert!(matches!(c.cut_at_this_byte(), Some(Cut::Natural)));

        c.len = MIN_SIZE;
        c.fp = SPLIT_MASK;
        assert!(c.cut_at_this_byte().is_none());

        c.len = MAX_SIZE;
        c.fp = SPLIT_MASK;
        assert!(matches!(c.cut_at_this_byte(), Some(Cut::Max)));

        c.len = MAX_SIZE;
        c.fp = 0;
        assert!(matches!(c.cut_at_this_byte(), Some(Cut::Natural)));

        c.len = MAX_SIZE - 1;
        c.fp = SPLIT_MASK;
        assert!(c.cut_at_this_byte().is_none());
    }

    #[test]
    fn empty_input_yields_zero_chunks() {
        let offs = chunk_offsets(test_poly(21), &[]).unwrap();
        assert!(offs.is_empty());
        assert!(chunk(test_poly(21), &[]).unwrap().is_empty());
    }

    #[test]
    fn files_below_min_are_never_split() {
        let p = test_poly(22);
        for size in [1usize, 2, 63, 64, 65, 4096, MIN_SIZE - 1] {
            let data = prng_bytes(size as u64, size);
            let ch = chunk(p, &data).unwrap();
            assert_eq!(ch.len(), 1, "size {size}");
            assert_eq!(ch[0], &data[..]);
        }
    }

    #[test]
    fn exactly_min_is_one_chunk() {
        let p = test_poly(23);
        let data = prng_bytes(99, MIN_SIZE);
        let ch = chunk(p, &data).unwrap();
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].len(), MIN_SIZE);
    }

    #[test]
    fn zero_run_cuts_at_exactly_min_every_time() {
        let p = test_poly(24);
        let data = vec![0u8; MIN_SIZE * 4 + 12345];
        let ch = chunk(p, &data).unwrap();
        assert_eq!(ch.len(), 5);
        for c in &ch[..4] {
            assert_eq!(c.len(), MIN_SIZE);
        }
        assert_eq!(ch[4].len(), 12345);
        assert!(ch.iter().all(|c| c.iter().all(|&b| b == 0)));
    }

    #[test]
    fn round_trip_property_concatenation_identity_and_size_bounds() {
        let p = test_poly(25);

        let sizes = [
            0usize,
            1,
            31,
            63,
            64,
            65,
            127,
            MIN_SIZE - 1,
            MIN_SIZE,
            MIN_SIZE + 1,
            MIN_SIZE * 2,
            MIN_SIZE * 3 + 777,
            MAX_SIZE - 1,
            MAX_SIZE,
            MAX_SIZE + 1,
            MAX_SIZE + MIN_SIZE,
            2 * 1024 * 1024 + 3,
            3 * 1024 * 1024 + 999_999,
        ];
        for (i, size) in sizes.iter().enumerate() {
            let data = prng_bytes(1000 + i as u64, *size);
            let parts = chunk(p, &data).unwrap();

            let rejoined: Vec<u8> = parts.concat();
            assert_eq!(rejoined, data, "round trip failed at size {size}");

            let mut off = 0usize;
            for part in &parts {
                assert_eq!(part.as_ptr() as usize - data.as_ptr() as usize, off);
                off += part.len();
            }
            assert_eq!(off, data.len());

            for (j, part) in parts.iter().enumerate() {
                assert!(!part.is_empty());
                if j + 1 < parts.len() {
                    assert!(
                        part.len() >= MIN_SIZE && part.len() <= MAX_SIZE,
                        "interior chunk {} has len {}",
                        j,
                        part.len()
                    );
                } else {
                    assert!(part.len() <= MAX_SIZE);
                }
            }
        }
    }

    #[test]
    fn streaming_push_matches_bulk_chunking() {
        let p = test_poly(26);
        let data = prng_bytes(27, MIN_SIZE * 2 + 4096);

        let bulk = chunk_offsets(p, &data).unwrap();

        let mut c = Chunker::new(p).unwrap();
        let mut streamed = Vec::new();
        let mut consumed = 0usize;
        for &b in &data {
            if let Some(l) = c.push(b) {
                streamed.push((consumed, l));
                consumed += l;
            }
        }
        if c.pending_len() > 0 {
            streamed.push((consumed, c.pending_len()));
            consumed += c.pending_len();
        }
        assert_eq!(consumed, data.len());
        assert_eq!(streamed, bulk);
    }

    #[test]
    fn streaming_feed_boundaries_are_identical_to_slice_output() {
        let p = test_poly(46);
        let avg = 1usize << AVG_BITS;

        let sizes = [
            0usize,
            1,
            63,
            64,
            65,
            MIN_SIZE - 1,
            MIN_SIZE,
            MIN_SIZE + 1,
            avg - 1,
            avg,
            avg + 1,
            MIN_SIZE * 2 + 777,
            MAX_SIZE - 1,
            MAX_SIZE,
            MAX_SIZE + 1,
        ];

        fn stream_in_blocks(p: u64, data: &[u8], block: usize) -> Vec<(usize, usize)> {
            let mut c = Chunker::new(p).unwrap();
            let mut out = Vec::new();
            let mut start = 0usize;
            for piece in data.chunks(block.max(1)) {
                for len in c.feed(piece) {
                    out.push((start, len));
                    start += len;
                }
            }
            let tail = c.finish();
            if tail > 0 {
                out.push((start, tail));
            }
            out
        }

        for (i, size) in sizes.iter().enumerate() {
            let data = prng_bytes(2000 + i as u64, *size);
            let expected = chunk_offsets(p, &data).unwrap();

            let total: usize = expected.iter().map(|(_, l)| l).sum();
            assert_eq!(total, *size, "slice tiling broke at size {size}");

            for block in [
                1usize,
                WINDOW_SIZE - 1,
                WINDOW_SIZE,
                WINDOW_SIZE + 1,
                4096,
                256 * 1024,
            ] {
                assert_eq!(
                    stream_in_blocks(p, &data, block),
                    expected,
                    "streamed boundaries diverged at size {size}, block {block}"
                );
            }
        }
    }

    #[test]
    fn chunking_is_deterministic_per_polynomial() {
        let p = test_poly(28);
        let data = prng_bytes(29, MIN_SIZE * 3 + 11);
        let a = chunk_offsets(p, &data).unwrap();
        let b = chunk_offsets(p, &data).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_polynomials_cut_differently_on_fixed_data() {
        let pa = test_poly(31);
        let pb = test_poly(32);
        assert_ne!(pa, pb);
        let data = prng_bytes(33, MIN_SIZE * 4);
        assert_ne!(
            chunk_offsets(pa, &data).unwrap(),
            chunk_offsets(pb, &data).unwrap(),
            "two unrelated degree-53 polynomials agreeing on 4 MiB of cuts \
             would indicate broken fingerprinting"
        );
    }

    #[test]
    fn common_suffix_produces_identical_boundaries_from_different_prefixes() {
        let poly = test_poly(78);
        let mut rng = StdRng::seed_from_u64(31);
        let p1: Vec<u8> = (0..8 * 1024 * 1024).map(|_| rng.gen()).collect();
        let p2: Vec<u8> = (0..8 * 1024 * 1024).map(|_| rng.gen()).collect();
        let c: Vec<u8> = (0..4 * 1024 * 1024).map(|_| rng.gen()).collect();

        let total = 12 * 1024 * 1024;
        let a = chunk_offsets(poly, &[p1.as_slice(), c.as_slice()].concat()).unwrap();
        let b = chunk_offsets(poly, &[p2.as_slice(), c.as_slice()].concat()).unwrap();

        let ends_a: std::collections::HashSet<usize> = a.iter().map(|(o, _)| total - o).collect();
        let ends_b: std::collections::HashSet<usize> = b.iter().map(|(o, _)| total - o).collect();
        let shared = ends_a.intersection(&ends_b).count();

        assert!(
            shared >= 3,
            "only {shared} shared boundaries across a 4 MiB common suffix; \
             window locality is broken"
        );
    }

    #[test]
    fn reset_restores_initial_state() {
        let p = test_poly(34);
        let data = prng_bytes(35, MIN_SIZE + 5000);
        let mut c = Chunker::new(p).unwrap();
        for &b in &data {
            let _ = c.push(b);
        }
        c.reset();
        assert_eq!(c.pending_len(), 0);

        let mut fresh_results = Vec::new();
        let mut fresh = Chunker::new(p).unwrap();
        for &b in &data {
            if let Some(l) = fresh.push(b) {
                fresh_results.push(l);
            }
        }
        let mut reused_results = Vec::new();
        for &b in &data {
            if let Some(l) = c.push(b) {
                reused_results.push(l);
            }
        }
        assert_eq!(fresh_results, reused_results);
    }
}
