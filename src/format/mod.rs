pub mod escaping;
pub mod fingerprint;
pub mod parse;
pub mod range;
pub mod types;

// Re-export public types
pub use fingerprint::{Fingerprint, FingerprintHasher};
pub use range::LineRange;
pub use types::{Chunk, Format};
