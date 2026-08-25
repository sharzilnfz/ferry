//! Content-defined chunking: Rabin fingerprint CDC per `docs/store-format.md`
//! ("Chunking"), the restic/chunker lineage. All constants are v1 format
//! constants and MUST NOT change without bumping the store format version.

use std::fmt;

use rand::Rng;
use thiserror::Error;

/// Sliding window of bytes feeding the fingerprint.
pub const WINDOW_SIZE: usize = 64;
/// No natural cut may fire before this many bytes.
pub const MIN_SIZE: usize = 524_288;
/// Average target is `2^AVG_BITS` bytes.
pub const AVG_BITS: u32 = 20;
/// Cut when the fingerprint's low `AVG_BITS` bits are all zero.
pub const SPLIT_MASK: u64 = (1 << AVG_BITS) - 1; // 1_048_575
/// Hard upper bound for one chunk.
pub const MAX_SIZE: usize = 8_388_608;
/// Degree of the per-folder polynomial over GF(2).
pub const POLY_DEGREE: u32 = 53;
/// Slide-out exponent: after the window fills, the outgoing byte has been
/// shifted left by 8 bits 63 times (8 * 63 = 504).
const SLIDE_OUT_X: u32 = 504;

#[derive(Debug, Error)]
#[error("polynomial {0:#x} is not monic irreducible of degree 53")]
pub struct PolynomialError(pub u64);

/// A polynomial that has passed [`is_irreducible`], validated exactly once
/// at folder-open/config-load time. Scan/snapshot APIs take this type so
/// downstream code cannot hold an invalid poly and panic mid-scan on it.
///
/// Construct from raw storage/CLI input with [`ValidatedPoly::new`] (or
/// `TryFrom<u64>`); generate fresh ones with [`ValidatedPoly::generate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidatedPoly(u64);

impl ValidatedPoly {
    /// Validate once; every later use is infallible by construction.
    pub fn new(p: u64) -> Result<Self, PolynomialError> {
        if !is_irreducible(p) {
            return Err(PolynomialError(p));
        }
        Ok(ValidatedPoly(p))
    }

    /// Draw a fresh valid polynomial from an RNG (see
    /// [`generate_polynomial`]).
    pub fn generate(rng: &mut impl Rng) -> Self {
        ValidatedPoly(generate_polynomial(rng))
    }

    /// The raw bitfield, for wire/storage serialization.
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

/// Degree of a nonzero GF(2)[x] polynomial stored in a u64 bitfield.
fn poly_degree(v: u64) -> Option<u32> {
    if v == 0 {
        None
    } else {
        Some(v.ilog2())
    }
}

/// Carryless multiply (schoolbook): XOR `a << i` for each set bit i of b.
///
/// The full product must fit in a u64, i.e. deg(a) + deg(b) < 63. Callers
/// outside tests must guarantee that or use [`gf_mulmod`].
pub fn gf_mul(a: u64, b: u64) -> u64 {
    let mut acc = 0u64;
    let mut b = b;
    while b != 0 {
        let i = b.trailing_zeros();
        acc ^= a << i;
        b &= b - 1; // clear lowest set bit
    }
    acc
}

/// Long-division remainder of v modulo p, for a polynomial of degree d.
fn gf_mod_deg(mut v: u64, p: u64, d: u32) -> u64 {
    loop {
        let dv = match poly_degree(v) {
            Some(dv) if dv >= d => dv,
            _ => return v,
        };
        v ^= p << (dv - d);
    }
}

/// Remainder of v modulo the folder polynomial p (degree [`POLY_DEGREE`]).
pub fn gf_mod(v: u64, p: u64) -> u64 {
    gf_mod_deg(v, p, POLY_DEGREE)
}

/// Degree of a nonzero GF(2)[x] polynomial stored in a u128 bitfield
/// (test-side reference implementation).
#[cfg(test)]
fn poly_degree128(v: u128) -> u32 {
    if v == 0 {
        0
    } else {
        v.ilog2()
    }
}

/// Carryless multiply of two reduced operands followed by reduction, safe for
/// any operand size (Horner: high bit to low bit, reducing every step).
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

/// `(a * b) mod p` without overflow risk at degree 53 operand sizes.
pub fn gf_mulmod(a: u64, b: u64, p: u64) -> u64 {
    mulmod_deg(a, b, p, POLY_DEGREE)
}

/// `x^n mod p`, computed by n doublings as the spec describes.
pub fn gf_pow_x(n: u32, p: u64) -> u64 {
    let mut v = 1u64;
    for _ in 0..n {
        v = gf_mod_deg(v << 1, p, POLY_DEGREE);
    }
    v
}

/// Independent route to `x^n mod p` by square-and-multiply; the spec requires
/// both routes to agree, which the tests pin down.
#[cfg(test)]
fn pow_x_square_multiply(n: u32, p: u64) -> u64 {
    // x^(2^k) mod p by repeated squaring of x.
    let base = n;
    let mut result = 1u64;
    let mut sq = 2u64; // x
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

/// Rabin irreducibility test for prime degree 53 (`docs/store-format.md`,
/// "The chunker polynomial", step 2).
pub fn is_irreducible(p: u64) -> bool {
    // Shape check first: monic, degree exactly 53, bits 54..63 zero.
    if p >> POLY_DEGREE != 1 {
        return false;
    }
    // Cheap coprimality clause: gcd(p, x^2 + x) == 1 holds iff p has a
    // nonzero constant term (no factor x) and an odd number of set bits
    // (p(1) == 1, no factor x + 1).
    if (p & 1) == 0 || p.count_ones().is_multiple_of(2) {
        return false;
    }
    // Frobenius clause: g = x^(2^53) mod p must equal x. Computed by
    // squaring x 53 times.
    let mut g = 2u64; // x
    for _ in 0..POLY_DEGREE {
        g = gf_mulmod(g, g, p);
    }
    g == 2
}

/// Draw a random monic irreducible degree-53 polynomial from the given RNG
/// (the OS CSPRNG in production). Expected ~53/2 draws.
pub fn generate_polynomial(rng: &mut impl Rng) -> u64 {
    loop {
        let candidate = (rng.gen::<u64>() & ((1 << POLY_DEGREE) - 1)) | (1 << POLY_DEGREE);
        if is_irreducible(candidate) {
            return candidate;
        }
    }
}

/// Why a byte ended a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// Fingerprint low bits were zero past `MIN_SIZE`.
    Natural,
    /// Hard clamp at `MAX_SIZE`.
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

/// Streaming CDC state machine for one file, bound to one folder polynomial.
///
/// Feed bytes with [`Chunker::push`] (one byte, yields a boundary) or
/// [`Chunker::feed`] (a block, yields every boundary completed inside it);
/// [`Chunker::finish`] reports the trailing unterminated chunk. A returned
/// length ends a chunk of exactly that many bytes ending at the last byte
/// fed. State resets fully between chunks; fingerprints never carry across
/// boundaries.
pub struct Chunker {
    poly: u64,
    win: [u8; WINDOW_SIZE],
    wpos: usize,
    filled: usize,
    fp: u64,
    len: usize,
    out_table: [u64; 256],
}

impl Chunker {
    /// Build a chunker for `p`. Rejects polynomials that are not monic
    /// irreducible of degree 53; generation guarantees validity but storage
    /// round trips do not, so this is checked here.
    pub fn new(p: u64) -> Result<Self, PolynomialError> {
        if !is_irreducible(p) {
            return Err(PolynomialError(p));
        }
        let slide_out = gf_pow_x(SLIDE_OUT_X, p);
        let mut out_table = [0u64; 256];
        for (i, slot) in out_table.iter_mut().enumerate() {
            // The raw product reaches degree <= 60; it MUST be reduced back
            // under degree 53 or every later fold inherits garbage high
            // terms and the fingerprint stops being window-local.
            *slot = gf_mod(gf_mul(i as u64, slide_out), p);
        }
        Ok(Chunker {
            poly: p,
            win: [0; WINDOW_SIZE],
            wpos: 0,
            filled: 0,
            fp: 0,
            len: 0,
            out_table,
        })
    }

    /// Full state reset between chunks (fp=0, filled=0, wpos=0, window
    /// cleared, length 0), per the spec's cutting loop.
    pub fn reset(&mut self) {
        self.win = [0; WINDOW_SIZE];
        self.wpos = 0;
        self.filled = 0;
        self.fp = 0;
        self.len = 0;
    }

    /// Bytes accumulated in the current unterminated chunk.
    pub fn pending_len(&self) -> usize {
        self.len
    }

    /// Append one byte to the rolling fingerprint, then evaluate the cutting
    /// rules in the spec's order: the minimum clamp gates the split test, the
    /// maximum clamp fires only when no natural cut happened first.
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

    /// Decision function, exposed for tests: would this byte end a chunk?
    fn cut_at_this_byte(&self) -> Option<Cut> {
        if self.len >= MIN_SIZE && (self.fp & SPLIT_MASK) == 0 {
            Some(Cut::Natural)
        } else if self.len == MAX_SIZE {
            Some(Cut::Max)
        } else {
            None
        }
    }

    /// Feed a block of bytes; returns the lengths of every chunk COMPLETED
    /// inside this block, in order. Byte-identical boundaries to pushing the
    /// same bytes one at a time — block edges never influence cutting.
    ///
    /// This is the whole-file-buffer-free entry point (T-09): callers stream
    /// a bounded read buffer through here and keep only the current chunk's
    /// bytes resident.
    pub fn feed(&mut self, data: &[u8]) -> Vec<usize> {
        let mut out = Vec::new();
        for &b in data {
            if let Some(len) = self.push(b) {
                out.push(len);
            }
        }
        out
    }

    /// End of stream: the length of the trailing unterminated chunk (0 when
    /// the input was empty or ended exactly on a boundary). The caller owns
    /// those trailing bytes; this only reports how many there are.
    pub fn finish(&self) -> usize {
        self.pending_len()
    }

    /// Fingerprint update for one appended byte, exactly as specified:
    /// during warm-up just fold the byte in; afterwards remove the outgoing
    /// byte's contribution (`out * x^504`) via the precomputed table, then
    /// fold in the new byte.
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
            self.fp ^= self.out_table[out as usize];
            self.fp = gf_mod((self.fp << 8) | u64::from(b), self.poly);
        }
    }
}

/// Chunk boundaries of `data`: (offset, length) pairs tiling the input.
///
/// Returns a [`PolynomialError`] instead of panicking when `poly` is not
/// monic irreducible of degree 53 — the daemon accepts `--poly HEX16` from
/// the CLI, so a typo must surface as a typed error, not a mid-scan panic.
/// Prefer threading a validated [`ValidatedPoly`] through your APIs and
/// calling `.get()` here.
pub fn chunk_offsets(poly: u64, data: &[u8]) -> Result<Vec<(usize, usize)>, PolynomialError> {
    // Thin wrapper over the streaming state machine (T-09): one code path,
    // so buffered and streamed boundaries cannot drift.
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

/// Slice view of [`chunk_offsets`].
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

    /// Deterministic pseudo-random buffer (xorshift-free: ChaCha-seeded).
    fn prng_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..len).map(|_| rng.gen()).collect()
    }

    // --- GF(2) arithmetic ---

    #[test]
    fn gf_mul_matches_shift_xor_reference() {
        let mut rng = StdRng::seed_from_u64(1);
        for _ in 0..2000 {
            // Keep both operands small enough that the product fits in u64.
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
            // Independent wide reduction: same long division in u128, which
            // cannot overflow for these operand sizes.
            let mut wide = u128::from(v);
            let pw = u128::from(p);
            while poly_degree128(wide) >= POLY_DEGREE {
                wide ^= pw << (poly_degree128(wide) - POLY_DEGREE);
            }
            assert_eq!(u128::from(r), wide, "gf_mod({v:#x})");
        }
    }

    #[allow(clippy::many_single_char_names)] // a/b/c/r mirror the algebra in the comments
    #[test]
    fn gf_mulmod_matches_gf_mul_then_mod() {
        let p = test_poly(11);
        let mut rng = StdRng::seed_from_u64(4);
        for _ in 0..1000 {
            // Small enough for the direct route.
            let a = rng.gen::<u64>() & ((1 << 5) - 1);
            let b = rng.gen::<u64>() & ((1 << 5) - 1);
            assert_eq!(gf_mulmod(a, b, p), gf_mod(gf_mul(a, b), p));
        }
        // Large operands: mulmod result must be a valid remainder satisfying
        // a*b ≡ r (checked via the homomorphism property below instead of
        // reconstructing the overflowing product).
        let a = 0x123456789abcdef;
        let b = 0x0f1e2d3c4b5a6978 & ((1 << 53) - 1);
        let r = gf_mulmod(a, b, p);
        assert!(r < (1 << 53));
        // Homomorphism: (a*b) ⊕ (c*b) ≡ (a⊕c)*b
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

    // --- Irreducibility ---

    #[test]
    fn rabin_test_agrees_with_trial_division_on_all_small_prime_degrees() {
        // Generic Rabin test for prime degree d vs exhaustive trial division.
        fn rabin_generic(p: u64, d: u32) -> bool {
            if poly_degree(p) != Some(d) {
                return false;
            }
            // x^(2^d) ≡ x (mod p)
            let mut v = 2u64;
            for _ in 0..d {
                v = mulmod_deg(v, v, p, d);
            }
            if v != 2 {
                return false;
            }
            // gcd(p, x^2 + x) == 1
            (p & 1) == 1 && p.count_ones() % 2 == 1
        }

        fn trial_division(p: u64, d: u32) -> bool {
            // Irreducible iff no monic irreducible factor of degree <= d/2.
            for k in 1..=(d / 2) {
                for q in (1u64 << k)..(1u64 << (k + 1)) {
                    if is_irreducible_by_rabin_generic(q, k, d) && gf_mod_deg(p, q, k) == 0 {
                        return false;
                    }
                }
            }
            true
        }

        // Helper mirroring the generic test above but allowing recursion into
        // smaller degrees for the trial divisors themselves.
        fn is_irreducible_by_rabin_generic(q: u64, k: u32, _ctx: u32) -> bool {
            if !k.is_power_of_two() && !is_prime(k) {
                return false;
            }
            // For tiny degrees just fall back to brute force divisibility by
            // smaller-degree irreducibles (recursion depth is <= 3).
            if k <= 1 {
                return true; // x and x+1 are irreducible
            }
            let mut v = 2u64;
            for _ in 0..k {
                v = mulmod_deg(v, v, q, k);
            }
            let frobenius_ok = v == 2;
            let coprime_ok = (q & 1) == 1 && q.count_ones() % 2 == 1;
            frobenius_ok && coprime_ok && {
                // Rabin requires the gcd conditions against ALL prime divisors
                // of k; k in {2,3,5,7} is prime so the single condition above
                // suffices.
                true
            }
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
        // Zero constant term => divisible by x.
        assert!(!is_irreducible(1u64 << 53));
        // Even weight => p(1) == 0 => divisible by x+1.
        assert!(!is_irreducible((1u64 << 53) | 0b11));
        // Wrong degree / non-monic shapes are rejected outright.
        assert!(!is_irreducible(0));
        assert!(!is_irreducible(1)); // degree 0
        assert!(!is_irreducible(1u64 << 54)); // bit 54 set: violates storage rule
    }

    #[test]
    fn generate_polynomial_yields_valid_distinct_polynomials() {
        let mut rng = StdRng::seed_from_u64(42);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..24 {
            let p = generate_polynomial(&mut rng);
            // Monic, degree exactly 53, upper bits clean.
            assert_eq!(p >> 53, 1);
            assert_eq!(p >> 54, 0);
            assert!(is_irreducible(p));
            seen.insert(p);
        }
        // 24 draws from ~2^52 candidates colliding is essentially impossible.
        assert_eq!(seen.len(), 24);
    }

    #[test]
    fn chunker_new_rejects_invalid_polynomials() {
        assert!(Chunker::new(test_poly(5)).is_ok());
        assert!(Chunker::new(0).is_err());
        assert!(Chunker::new(1u64 << 53).is_err()); // reducible (divisible by x)
        assert!(Chunker::new(0x1234).is_err()); // not degree 53
    }

    /// T-02 acceptance: the free functions must return the typed error for a
    /// user-supplied reducible polynomial instead of `.expect()`-panicking
    /// mid-scan (the daemon accepts `--poly HEX16` from the CLI).
    #[test]
    fn free_functions_return_typed_error_on_reducible_polynomial() {
        // Reducible (divisible by x): monic degree 53 but zero constant term.
        let bad = 1u64 << 53;
        let err = chunk_offsets(bad, b"hello ferry").unwrap_err();
        assert_eq!(err.0, bad, "the error names the offending polynomial");
        assert!(matches!(chunk(bad, &[1, 2, 3]), Err(PolynomialError(p)) if p == bad));
        // A valid poly still chunks fine through the same entry points.
        let good = test_poly(41);
        assert!(chunk_offsets(good, b"hello ferry").is_ok());
    }

    // --- Cut decision and clamping order ---

    #[test]
    fn clamping_order_min_gates_split_max_fires_only_without_natural_cut() {
        let mut c = Chunker::new(test_poly(6)).unwrap();

        // Below MIN: never cut, even with fingerprint low bits zero.
        c.len = MIN_SIZE - 1;
        c.fp = 0;
        assert!(c.cut_at_this_byte().is_none());

        // At MIN with matching fingerprint: natural cut.
        c.len = MIN_SIZE;
        c.fp = 0;
        assert!(matches!(c.cut_at_this_byte(), Some(Cut::Natural)));

        // At MIN with non-matching fingerprint: no cut.
        c.len = MIN_SIZE;
        c.fp = SPLIT_MASK; // low 20 bits all ones
        assert!(c.cut_at_this_byte().is_none());

        // At MAX with non-matching fingerprint: forced cut.
        c.len = MAX_SIZE;
        c.fp = SPLIT_MASK;
        assert!(matches!(c.cut_at_this_byte(), Some(Cut::Max)));

        // At MAX WITH matching fingerprint: the natural cut wins because the
        // split test is evaluated before the max test.
        c.len = MAX_SIZE;
        c.fp = 0;
        assert!(matches!(c.cut_at_this_byte(), Some(Cut::Natural)));

        // One byte before MAX with non-matching fingerprint: nothing.
        c.len = MAX_SIZE - 1;
        c.fp = SPLIT_MASK;
        assert!(c.cut_at_this_byte().is_none());
    }

    // --- Chunking behavior ---

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
        // Degenerate input keeps the fingerprint at zero once the window is
        // full, so every natural cut lands at exactly MIN_SIZE.
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
        // Sizes span: empty, 1, sub-window, window edges, sub-MIN, around
        // MIN, around MAX, multi-MiB.
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
            // Reassembly is byte-identical.
            let rejoined: Vec<u8> = parts.concat();
            assert_eq!(rejoined, data, "round trip failed at size {size}");

            // Offsets tile the input exactly.
            let mut off = 0usize;
            for part in &parts {
                assert_eq!(part.as_ptr() as usize - data.as_ptr() as usize, off);
                off += part.len();
            }
            assert_eq!(off, data.len());

            // Size rules: interior chunks obey MIN..=MAX; the final chunk may
            // be short; an empty input produces no chunks.
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

        // Bulk.
        let bulk = chunk_offsets(p, &data).unwrap();

        // Streaming byte by byte through a fresh chunker.
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

    /// T-09 acceptance: block-fed `feed`/`finish` must produce byte-identical
    /// boundaries to the buffered slice functions, for every input size
    /// around the min/avg/max clamp boundaries and every feed block size —
    /// including blocks that split a cut decision's window state across two
    /// feeds.
    #[test]
    fn streaming_feed_boundaries_are_identical_to_slice_output() {
        let p = test_poly(46);
        let avg = 1usize << AVG_BITS;
        // Sizes span: empty, sub-window, window edges, sub-MIN, exactly MIN,
        // around AVG, multi-MIN, around MAX, MAX+1 and one past a max clamp.
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

            // Reassembly identity holds through the streaming path too.
            let total: usize = expected.iter().map(|(_, l)| l).sum();
            assert_eq!(total, *size, "slice tiling broke at size {size}");

            // Block sizes: 1, window edge ±1, page-ish, and a big block that
            // swallows whole chunks at once.
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

    /// THE dedup property, pinned as a regression test: two different
    /// prefixes followed by one common suffix must produce IDENTICAL
    /// content-anchored boundaries inside that suffix. This failed when the
    /// slide-out table skipped its final reduction (fingerprint leaked
    /// high-degree terms), which destroyed boundary stability.
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

        // Boundaries expressed as distance-from-end are prefix-independent.
        let ends_a: std::collections::HashSet<usize> = a.iter().map(|(o, _)| total - o).collect();
        let ends_b: std::collections::HashSet<usize> = b.iter().map(|(o, _)| total - o).collect();
        let shared = ends_a.intersection(&ends_b).count();
        // The two streams share 4 MiB; several anchors deep in that region
        // must coincide. (Exactly how many is data-dependent, so pin only
        // the lower bound.)
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
        // After reset the chunker behaves like a fresh one.
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
