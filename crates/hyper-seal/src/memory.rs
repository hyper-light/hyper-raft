//! A key's bytes in memory (`docs/seal.md` §8): on the heap, so moving a key moves a pointer and
//! leaves no copy of its bytes behind, and wiped with volatile writes when dropped, so the wipe is
//! never elided as a store to memory about to be freed.
#![allow(unsafe_code)]

use std::sync::atomic::{Ordering, compiler_fence};

/// 32 secret bytes: a key at any level of the hierarchy. Never `Clone`, never printed.
pub struct Secret32(Box<[u8; 32]>);

impl Secret32 {
    /// 32 zero bytes, to be filled.
    pub(crate) fn zeroed() -> Self {
        Self(Box::new([0u8; 32]))
    }

    /// A secret from bytes the caller holds; the caller wipes its own copy.
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        let mut secret = Self::zeroed();
        *secret.0 = *bytes;
        secret
    }

    /// The bytes, for a call into the library.
    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn bytes_mut(&mut self) -> &mut [u8; 32] {
        &mut self.0
    }
}

impl Drop for Secret32 {
    fn drop(&mut self) {
        wipe(self.0.as_mut_slice());
    }
}

impl std::fmt::Debug for Secret32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret32(..)")
    }
}

/// Zeroes `bytes` with volatile writes, then fences, so the compiler keeps every write.
pub(crate) fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        let at: *mut u8 = byte;
        // SAFETY: `at` comes from a `&mut u8` borrowed for this statement, so it is valid, aligned
        // and exclusively ours for the write.
        unsafe { std::ptr::write_volatile(at, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wipe_zeroes_every_byte() {
        let mut bytes = [0xA5u8; 47];
        wipe(&mut bytes);
        assert_eq!(bytes, [0u8; 47]);
    }

    #[test]
    fn a_secret_never_prints_its_bytes() {
        let secret = Secret32::from_bytes(&[7; 32]);
        assert_eq!(format!("{secret:?}"), "Secret32(..)");
    }
}
