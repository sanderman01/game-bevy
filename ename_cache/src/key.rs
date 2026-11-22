use std::fmt;

use bytemuck::NoUninit;

/// A cache entry's name: the BLAKE3 hash of its inputs.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey(blake3::Hash);

impl CacheKey {
    pub(crate) fn to_hex(self) -> String {
        self.0.to_hex().to_string()
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_hex())
    }
}

impl fmt::Debug for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CacheKey({self})")
    }
}

/// Builds a [`CacheKey`] from inputs fed in a fixed order. Each input is tagged with its kind and
/// length, so no two different sequences of calls hash alike: `("ab", "c")` differs from
/// `("a", "bc")`, and bytes differ from a value with the same bytes. Feed every input on every
/// call, and an optional one through [`KeyHasher::option`] rather than skipping it. Values hash in
/// native byte order: a cache stays on the machine that wrote it.
#[derive(Clone, Default)]
pub struct KeyHasher(blake3::Hasher);

/// The kind tags.
const BYTES: u8 = 0;
const VALUE: u8 = 1;
const NONE: u8 = 2;

impl KeyHasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.tagged(BYTES, bytes)
    }

    pub fn str(&mut self, value: &str) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    /// A slice of plain values, such as vertex positions or texels.
    pub fn slice<T: NoUninit>(&mut self, values: &[T]) -> &mut Self {
        self.bytes(bytemuck::cast_slice(values))
    }

    /// One plain value, such as a `u32`, an `f32` or a `[f32; 3]`. Floats hash by bits, so `0.0`
    /// and `-0.0` differ.
    pub fn value<T: NoUninit>(&mut self, value: T) -> &mut Self {
        self.tagged(VALUE, bytemuck::bytes_of(&value))
    }

    /// An optional input: `None` hashes as its own kind, so it never matches a present value.
    pub fn option<T>(&mut self, value: Option<T>, hash: impl FnOnce(&mut Self, T)) -> &mut Self {
        match value {
            Some(value) => hash(self, value),
            None => {
                self.tagged(NONE, &[]);
            }
        }
        self
    }

    fn tagged(&mut self, tag: u8, bytes: &[u8]) -> &mut Self {
        self.0.update(&[tag]);
        self.0.update(&(bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
        self
    }

    pub fn finish(&self) -> CacheKey {
        CacheKey(self.0.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefixes_keep_inputs_apart() {
        let a = KeyHasher::new().str("ab").str("c").finish();
        let b = KeyHasher::new().str("a").str("bc").finish();
        assert_ne!(a, b);
    }

    #[test]
    fn kinds_keep_inputs_apart() {
        let bytes = KeyHasher::new().bytes(b"abcde").finish();
        let values = KeyHasher::new().value(5u64).value(*b"abcde").finish();
        assert_ne!(bytes, values);
        let none = KeyHasher::new()
            .option(None::<u32>, |h, v| {
                h.value(v);
            })
            .finish();
        assert_ne!(none, KeyHasher::new().bytes(&[]).finish());
        assert_ne!(none, KeyHasher::new().finish());
    }

    #[test]
    fn same_inputs_give_the_same_key() {
        let key = || {
            KeyHasher::new()
                .value(3u32)
                .slice(&[[1.0f32, 2.0, 3.0]])
                .finish()
        };
        assert_eq!(key(), key());
        assert_eq!(key().to_string().len(), 64);
    }
}
