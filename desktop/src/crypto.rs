//! Self-contained implementations of the primitives Ferry needs:
//! SHA-256 / HMAC / HKDF (RFC 6234, 2104, 5869), ChaCha20-Poly1305 (RFC 8439)
//! and X25519 (RFC 7748, field arithmetic after TweetNaCl).
//! Verified against the RFC test vectors in the tests at the bottom of this file
//! and cross-checked against independent implementations (see tests/).

#[cfg(not(win))]
use std::fs::File;
#[cfg(not(win))]
use std::io::Read;

// ---------------------------------------------------------------- random

/// OS randomness: /dev/urandom on Linux, BCryptGenRandom (system RNG) on Windows.
#[cfg(not(win))]
pub fn random_bytes(buf: &mut [u8]) {
    let mut f = File::open("/dev/urandom").expect("cannot open /dev/urandom");
    f.read_exact(buf).expect("cannot read /dev/urandom");
}

#[cfg(win)]
pub fn random_bytes(buf: &mut [u8]) {
    #[link(name = "bcrypt")]
    extern "system" {
        // NTSTATUS BCryptGenRandom(BCRYPT_ALG_HANDLE, PUCHAR, ULONG, ULONG)
        fn BCryptGenRandom(alg: isize, buf: *mut u8, len: u32, flags: u32) -> i32;
    }
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x2;
    for chunk in buf.chunks_mut(1 << 30) {
        let st = unsafe { BCryptGenRandom(0, chunk.as_mut_ptr(), chunk.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        assert!(st == 0, "BCryptGenRandom failed (status {:#x})", st);
    }
}

pub fn random_array<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    random_bytes(&mut b);
    b
}

/// Constant-time equality.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    d == 0
}

// ---------------------------------------------------------------- SHA-256

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buflen: usize,
    total: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0; 64],
            buflen: 0,
            total: 0,
        }
    }

    fn compress(h: &mut [u32; 8], block: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.buflen > 0 {
            let n = (64 - self.buflen).min(data.len());
            self.buf[self.buflen..self.buflen + n].copy_from_slice(&data[..n]);
            self.buflen += n;
            data = &data[n..];
            if self.buflen == 64 {
                let b = self.buf;
                Self::compress(&mut self.h, &b);
                self.buflen = 0;
            }
        }
        while data.len() >= 64 {
            Self::compress(&mut self.h, &data[..64]);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buflen = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buflen != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for i in 0..8 {
            out[4 * i..4 * i + 4].copy_from_slice(&self.h[i].to_be_bytes());
        }
        out
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut s = Sha256::new();
    s.update(data);
    s.finish()
}

pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    for p in parts {
        inner.update(p);
    }
    let ih = inner.finish();
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&ih);
    outer.finish()
}

/// HKDF-SHA256 (extract + expand).
pub fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    assert!(out.len() <= 255 * 32);
    let prk = hmac_sha256(salt, &[ikm]);
    let mut t: Vec<u8> = Vec::new();
    let mut pos = 0;
    let mut counter = 1u8;
    while pos < out.len() {
        let block = hmac_sha256(&prk, &[&t, info, &[counter]]);
        let n = (out.len() - pos).min(32);
        out[pos..pos + n].copy_from_slice(&block[..n]);
        pos += n;
        t = block.to_vec();
        counter = counter.wrapping_add(1);
    }
}

// ---------------------------------------------------------------- ChaCha20

#[inline(always)]
fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut st = [0u32; 16];
    st[0] = 0x61707865;
    st[1] = 0x3320646e;
    st[2] = 0x79622d32;
    st[3] = 0x6b206574;
    for i in 0..8 {
        st[4 + i] = u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }
    st[12] = counter;
    for i in 0..3 {
        st[13 + i] =
            u32::from_le_bytes([nonce[4 * i], nonce[4 * i + 1], nonce[4 * i + 2], nonce[4 * i + 3]]);
    }
    let mut w = st;
    for _ in 0..10 {
        qr(&mut w, 0, 4, 8, 12);
        qr(&mut w, 1, 5, 9, 13);
        qr(&mut w, 2, 6, 10, 14);
        qr(&mut w, 3, 7, 11, 15);
        qr(&mut w, 0, 5, 10, 15);
        qr(&mut w, 1, 6, 11, 12);
        qr(&mut w, 2, 7, 8, 13);
        qr(&mut w, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&w[i].wrapping_add(st[i]).to_le_bytes());
    }
    out
}

pub fn chacha20_xor(key: &[u8; 32], mut counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for chunk in data.chunks_mut(64) {
        let ks = chacha20_block(key, counter, nonce);
        for (b, k) in chunk.iter_mut().zip(ks.iter()) {
            *b ^= k;
        }
        counter = counter.wrapping_add(1);
    }
}

// ---------------------------------------------------------------- Poly1305 (donna-32)

pub struct Poly1305 {
    r: [u32; 5],
    h: [u32; 5],
    pad: [u32; 4],
    buf: [u8; 16],
    left: usize,
}

#[inline(always)]
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Self {
        Poly1305 {
            r: [
                le32(&key[0..]) & 0x3ffffff,
                (le32(&key[3..]) >> 2) & 0x3ffff03,
                (le32(&key[6..]) >> 4) & 0x3ffc0ff,
                (le32(&key[9..]) >> 6) & 0x3f03fff,
                (le32(&key[12..]) >> 8) & 0x00fffff,
            ],
            h: [0; 5],
            pad: [le32(&key[16..]), le32(&key[20..]), le32(&key[24..]), le32(&key[28..])],
            buf: [0; 16],
            left: 0,
        }
    }

    fn block(&mut self, m: &[u8], hibit: u32) {
        let [r0, r1, r2, r3, r4] = self.r;
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;
        h0 += le32(&m[0..]) & 0x3ffffff;
        h1 += (le32(&m[3..]) >> 2) & 0x3ffffff;
        h2 += (le32(&m[6..]) >> 4) & 0x3ffffff;
        h3 += (le32(&m[9..]) >> 6) & 0x3ffffff;
        h4 += (le32(&m[12..]) >> 8) | hibit;
        let m64 = |a: u32, b: u32| a as u64 * b as u64;
        let d0 = m64(h0, r0) + m64(h1, s4) + m64(h2, s3) + m64(h3, s2) + m64(h4, s1);
        let mut d1 = m64(h0, r1) + m64(h1, r0) + m64(h2, s4) + m64(h3, s3) + m64(h4, s2);
        let mut d2 = m64(h0, r2) + m64(h1, r1) + m64(h2, r0) + m64(h3, s4) + m64(h4, s3);
        let mut d3 = m64(h0, r3) + m64(h1, r2) + m64(h2, r1) + m64(h3, r0) + m64(h4, s4);
        let mut d4 = m64(h0, r4) + m64(h1, r3) + m64(h2, r2) + m64(h3, r1) + m64(h4, r0);
        let mut c = (d0 >> 26) as u32;
        h0 = d0 as u32 & 0x3ffffff;
        d1 += c as u64;
        c = (d1 >> 26) as u32;
        h1 = d1 as u32 & 0x3ffffff;
        d2 += c as u64;
        c = (d2 >> 26) as u32;
        h2 = d2 as u32 & 0x3ffffff;
        d3 += c as u64;
        c = (d3 >> 26) as u32;
        h3 = d3 as u32 & 0x3ffffff;
        d4 += c as u64;
        c = (d4 >> 26) as u32;
        h4 = d4 as u32 & 0x3ffffff;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= 0x3ffffff;
        h1 += c;
        self.h = [h0, h1, h2, h3, h4];
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.left > 0 {
            let n = (16 - self.left).min(data.len());
            self.buf[self.left..self.left + n].copy_from_slice(&data[..n]);
            self.left += n;
            data = &data[n..];
            if self.left < 16 {
                return;
            }
            let b = self.buf;
            self.block(&b, 1 << 24);
            self.left = 0;
        }
        while data.len() >= 16 {
            self.block(&data[..16], 1 << 24);
            data = &data[16..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.left = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 16] {
        if self.left > 0 {
            let mut b = [0u8; 16];
            b[..self.left].copy_from_slice(&self.buf[..self.left]);
            b[self.left] = 1;
            self.block(&b, 0);
        }
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;
        let mut c = h1 >> 26;
        h1 &= 0x3ffffff;
        h2 += c;
        c = h2 >> 26;
        h2 &= 0x3ffffff;
        h3 += c;
        c = h3 >> 26;
        h3 &= 0x3ffffff;
        h4 += c;
        c = h4 >> 26;
        h4 &= 0x3ffffff;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= 0x3ffffff;
        h1 += c;

        let mut g0 = h0.wrapping_add(5);
        c = g0 >> 26;
        g0 &= 0x3ffffff;
        let mut g1 = h1.wrapping_add(c);
        c = g1 >> 26;
        g1 &= 0x3ffffff;
        let mut g2 = h2.wrapping_add(c);
        c = g2 >> 26;
        g2 &= 0x3ffffff;
        let mut g3 = h3.wrapping_add(c);
        c = g3 >> 26;
        g3 &= 0x3ffffff;
        let mut g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);

        let mut mask = (g4 >> 31).wrapping_sub(1);
        g0 &= mask;
        g1 &= mask;
        g2 &= mask;
        g3 &= mask;
        g4 &= mask;
        mask = !mask;
        h0 = (h0 & mask) | g0;
        h1 = (h1 & mask) | g1;
        h2 = (h2 & mask) | g2;
        h3 = (h3 & mask) | g3;
        h4 = (h4 & mask) | g4;

        let w0 = h0 | (h1 << 26);
        let w1 = (h1 >> 6) | (h2 << 20);
        let w2 = (h2 >> 12) | (h3 << 14);
        let w3 = (h3 >> 18) | (h4 << 8);

        let mut f = w0 as u64 + self.pad[0] as u64;
        let o0 = f as u32;
        f = w1 as u64 + self.pad[1] as u64 + (f >> 32);
        let o1 = f as u32;
        f = w2 as u64 + self.pad[2] as u64 + (f >> 32);
        let o2 = f as u32;
        f = w3 as u64 + self.pad[3] as u64 + (f >> 32);
        let o3 = f as u32;

        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&o0.to_le_bytes());
        out[4..8].copy_from_slice(&o1.to_le_bytes());
        out[8..12].copy_from_slice(&o2.to_le_bytes());
        out[12..16].copy_from_slice(&o3.to_le_bytes());
        out
    }
}

// ---------------------------------------------------------------- AEAD

fn aead_tag(otk: &[u8; 32], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let zeros = [0u8; 16];
    let mut p = Poly1305::new(otk);
    p.update(aad);
    p.update(&zeros[..(16 - aad.len() % 16) % 16]);
    p.update(ct);
    p.update(&zeros[..(16 - ct.len() % 16) % 16]);
    p.update(&(aad.len() as u64).to_le_bytes());
    p.update(&(ct.len() as u64).to_le_bytes());
    p.finish()
}

fn poly_key(key: &[u8; 32], nonce: &[u8; 12]) -> [u8; 32] {
    let b = chacha20_block(key, 0, nonce);
    let mut k = [0u8; 32];
    k.copy_from_slice(&b[..32]);
    k
}

/// Encrypts `buf` in place and appends the 16-byte tag.
pub fn aead_seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], buf: &mut Vec<u8>) {
    let otk = poly_key(key, nonce);
    chacha20_xor(key, 1, nonce, buf);
    let tag = aead_tag(&otk, aad, buf);
    buf.extend_from_slice(&tag);
}

/// Verifies and decrypts `buf` (ciphertext||tag) in place; strips the tag.
pub fn aead_open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], buf: &mut Vec<u8>) -> bool {
    if buf.len() < 16 {
        return false;
    }
    let n = buf.len() - 16;
    let otk = poly_key(key, nonce);
    let tag = aead_tag(&otk, aad, &buf[..n]);
    if !ct_eq(&tag, &buf[n..]) {
        return false;
    }
    buf.truncate(n);
    chacha20_xor(key, 1, nonce, buf);
    true
}

// ---------------------------------------------------------------- X25519 (TweetNaCl field arithmetic)

type Gf = [i64; 16];
const GF0: Gf = [0; 16];
const GF_121665: Gf = [0xDB41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn car25519(o: &mut Gf) {
    for i in 0..16 {
        o[i] += 1 << 16;
        let c = o[i] >> 16;
        if i < 15 {
            o[i + 1] += c - 1;
        } else {
            o[0] += 38 * (c - 1);
        }
        o[i] -= c << 16;
    }
}

fn sel25519(p: &mut Gf, q: &mut Gf, b: i64) {
    let c = !(b - 1);
    for i in 0..16 {
        let t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

fn pack25519(n: &Gf) -> [u8; 32] {
    let mut t = *n;
    let mut m = GF0;
    car25519(&mut t);
    car25519(&mut t);
    car25519(&mut t);
    for _ in 0..2 {
        m[0] = t[0] - 0xffed;
        for i in 1..15 {
            m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
            m[i - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        let b = (m[15] >> 16) & 1;
        m[14] &= 0xffff;
        sel25519(&mut t, &mut m, 1 - b);
    }
    let mut o = [0u8; 32];
    for i in 0..16 {
        o[2 * i] = (t[i] & 0xff) as u8;
        o[2 * i + 1] = ((t[i] >> 8) & 0xff) as u8;
    }
    o
}

fn unpack25519(n: &[u8; 32]) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = n[2 * i] as i64 + ((n[2 * i + 1] as i64) << 8);
    }
    o[15] &= 0x7fff;
    o
}

fn fadd(a: &Gf, b: &Gf) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

fn fsub(a: &Gf, b: &Gf) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

fn fmul(a: &Gf, b: &Gf) -> Gf {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o = GF0;
    o.copy_from_slice(&t[..16]);
    car25519(&mut o);
    car25519(&mut o);
    o
}

fn fsq(a: &Gf) -> Gf {
    fmul(a, a)
}

fn inv25519(i: &Gf) -> Gf {
    let mut c = *i;
    for a in (0..=253).rev() {
        c = fsq(&c);
        if a != 2 && a != 4 {
            c = fmul(&c, i);
        }
    }
    c
}

pub fn x25519(scalar: &[u8; 32], point: &[u8; 32]) -> [u8; 32] {
    let mut z = *scalar;
    z[31] = (z[31] & 127) | 64;
    z[0] &= 248;
    let x = unpack25519(point);
    let mut a = GF0;
    let mut b = x;
    let mut c = GF0;
    let mut d = GF0;
    a[0] = 1;
    d[0] = 1;
    for i in (0..=254usize).rev() {
        let r = ((z[i >> 3] >> (i & 7)) & 1) as i64;
        sel25519(&mut a, &mut b, r);
        sel25519(&mut c, &mut d, r);
        let mut e = fadd(&a, &c);
        a = fsub(&a, &c);
        c = fadd(&b, &d);
        b = fsub(&b, &d);
        d = fsq(&e);
        let f = fsq(&a);
        a = fmul(&c, &a);
        c = fmul(&b, &e);
        e = fadd(&a, &c);
        a = fsub(&a, &c);
        b = fsq(&a);
        c = fsub(&d, &f);
        a = fmul(&c, &GF_121665);
        a = fadd(&a, &d);
        c = fmul(&c, &a);
        a = fmul(&d, &f);
        d = fmul(&b, &x);
        b = fsq(&e);
        sel25519(&mut a, &mut b, r);
        sel25519(&mut c, &mut d, r);
    }
    let ci = inv25519(&c);
    pack25519(&fmul(&a, &ci))
}

pub const X25519_BASE: [u8; 32] = {
    let mut b = [0u8; 32];
    b[0] = 9;
    b
};

pub fn x25519_keypair() -> ([u8; 32], [u8; 32]) {
    let sk: [u8; 32] = random_array();
    let pk = x25519(&sk, &X25519_BASE);
    (sk, pk)
}

// ---------------------------------------------------------------- hex helpers

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> Vec<u8> {
        from_hex(&s.replace([' ', '\n', ':'], "")).unwrap()
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            to_hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            to_hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut s = Sha256::new();
        for _ in 0..1000 {
            s.update(&[b'a'; 1000]);
        }
        assert_eq!(
            to_hex(&s.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn hmac_rfc4231() {
        let k = [0x0bu8; 20];
        assert_eq!(
            to_hex(&hmac_sha256(&k, &[b"Hi There"])),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn hkdf_rfc5869() {
        let ikm = [0x0bu8; 22];
        let salt = h("000102030405060708090a0b0c");
        let info = h("f0f1f2f3f4f5f6f7f8f9");
        let mut out = [0u8; 42];
        hkdf(&salt, &ikm, &info, &mut out);
        assert_eq!(
            to_hex(&out),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }

    #[test]
    fn chacha20_poly1305_rfc8439() {
        let key: [u8; 32] = h("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f")
            .try_into()
            .unwrap();
        let nonce: [u8; 12] = h("070000004041424344454647").try_into().unwrap();
        let aad = h("50515253c0c1c2c3c4c5c6c7");
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let mut buf = pt.to_vec();
        aead_seal(&key, &nonce, &aad, &mut buf);
        let expect_ct = h("d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116");
        let expect_tag = h("1ae10b594f09e26a7e902ecbd0600691");
        assert_eq!(&buf[..pt.len()], &expect_ct[..]);
        assert_eq!(&buf[pt.len()..], &expect_tag[..]);
        assert!(aead_open(&key, &nonce, &aad, &mut buf));
        assert_eq!(&buf[..], &pt[..]);
        // tamper
        let mut buf2 = pt.to_vec();
        aead_seal(&key, &nonce, &aad, &mut buf2);
        buf2[3] ^= 1;
        assert!(!aead_open(&key, &nonce, &aad, &mut buf2));
    }

    #[test]
    fn poly1305_rfc8439() {
        let key: [u8; 32] = h("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b")
            .try_into()
            .unwrap();
        let mut p = Poly1305::new(&key);
        p.update(b"Cryptographic Forum Research Group");
        assert_eq!(to_hex(&p.finish()), "a8061dc1305136c6c22b8baf0c0127a9");
    }

    #[test]
    fn x25519_rfc7748() {
        let a_sk: [u8; 32] = h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
            .try_into()
            .unwrap();
        let b_sk: [u8; 32] = h("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
            .try_into()
            .unwrap();
        let a_pk = x25519(&a_sk, &X25519_BASE);
        let b_pk = x25519(&b_sk, &X25519_BASE);
        assert_eq!(to_hex(&a_pk), "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        assert_eq!(to_hex(&b_pk), "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
        let s1 = x25519(&a_sk, &b_pk);
        let s2 = x25519(&b_sk, &a_pk);
        assert_eq!(s1, s2);
        assert_eq!(to_hex(&s1), "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    }

    #[test]
    fn x25519_iterated_1000() {
        let mut k = X25519_BASE;
        let mut u = X25519_BASE;
        for _ in 0..1000 {
            let r = x25519(&k, &u);
            u = k;
            k = r;
        }
        assert_eq!(to_hex(&k), "684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51");
    }
}
