//! [`Fingerprint`]: a short hash of the original lines a chunk replaces.
//!
//! `ingest` and `search` write it into the chunk header (`@path:line:len #xxxxxxxx`)
//! and `apply` recomputes it from the bytes it is about to replace. A mismatch means
//! the file changed since the chunk was generated — or the chunk was already
//! applied — and the whole plan is refused instead of writing edits at shifted
//! positions. This is the same idea as the `index <blob>..<blob>` line git records
//! in a patch, at chunk granularity.
//!
//! The hash is 64-bit FNV-1a folded to 32 bits and shown as 8 hex digits. It only
//! has to notice accidental change, not resist an adversary, and it must be stable
//! across builds (which `std`'s `DefaultHasher` does not promise). A hand-written
//! chunk simply omits the tag and is applied unchecked.

use std::fmt;
use std::str::FromStr;

/// The fingerprint of a byte sequence; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(u32);

impl Fingerprint {
    /// Number of hex digits in the textual form.
    pub const HEX_LEN: usize = 8;

    /// Fingerprint of `bytes` in one go.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = FingerprintHasher::new();
        hasher.update(bytes);
        hasher.finish()
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

/// The text was not exactly [`Fingerprint::HEX_LEN`] hex digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFingerprintError;

impl FromStr for Fingerprint {
    type Err = ParseFingerprintError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != Self::HEX_LEN || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseFingerprintError);
        }
        u32::from_str_radix(s, 16)
            .map(Self)
            .map_err(|_| ParseFingerprintError)
    }
}

/// Incremental fingerprint computation, so a streaming reader can feed the bytes
/// it skips without buffering a whole chunk's worth of original text.
#[derive(Debug, Clone)]
pub struct FingerprintHasher(u64);

impl FingerprintHasher {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    #[must_use]
    pub fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    #[must_use]
    pub fn finish(self) -> Fingerprint {
        // xor-fold 64 → 32 bits so both halves contribute.
        #[allow(clippy::cast_possible_truncation)]
        let folded = ((self.0 >> 32) ^ self.0) as u32;
        Fingerprint(folded)
    }
}

impl Default for FingerprintHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_known_values_are_stable() {
        // Reference values from an independent FNV-1a implementation. If these
        // change, every `.bk` file in the wild stops applying.
        assert_eq!(Fingerprint::of(b"").to_string(), "4fd0bfc1");
        assert_eq!(Fingerprint::of(b"a").to_string(), "296230c0");
        assert_eq!(Fingerprint::of(b"b\n").to_string(), "bdebb25a");
        assert_eq!(Fingerprint::of(b"hello\nworld\n").to_string(), "391d0a3d");
    }

    #[test]
    fn test_incremental_matches_one_shot() {
        let mut h = FingerprintHasher::new();
        h.update(b"hel");
        h.update(b"");
        h.update(b"lo\nwor");
        h.update(b"ld\n");
        assert_eq!(h.finish(), Fingerprint::of(b"hello\nworld\n"));
    }

    #[test]
    fn test_fingerprint_notices_a_changed_line() {
        assert_ne!(Fingerprint::of(b"b\n"), Fingerprint::of(b"B\n"));
        assert_ne!(Fingerprint::of(b"b\n"), Fingerprint::of(b"b"));
    }

    #[test]
    fn test_fingerprint_parse_roundtrip_and_rejections() {
        let fp = Fingerprint::of(b"hello\nworld\n");
        assert_eq!(fp.to_string().parse::<Fingerprint>(), Ok(fp));
        assert_eq!("391D0A3D".parse::<Fingerprint>(), Ok(fp));

        assert_eq!("391d0a3".parse::<Fingerprint>(), Err(ParseFingerprintError));
        assert_eq!(
            "391d0a3dd".parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
        assert_eq!(
            "391d0a3g".parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
        assert_eq!("".parse::<Fingerprint>(), Err(ParseFingerprintError));
    }
}
