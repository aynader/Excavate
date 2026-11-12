//! Checking a signature with Windows' own cryptography (CNG): ECDSA on the P-256 curve over a SHA-256 hash, the
//! signature as r and s (64 bytes), the key as Windows exports a public P-256 key (`BCRYPT_ECCPUBLIC_BLOB`: 72 bytes).
//! For Excavate's updates (app/src/update.rs): a release is used only if it was signed with the key its maker keeps.

use windows::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDSA_P256_ALGORITHM, BCRYPT_FLAGS, BCRYPT_KEY_HANDLE,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCryptCloseAlgorithmProvider, BCryptDestroyKey, BCryptImportKeyPair,
    BCryptOpenAlgorithmProvider, BCryptVerifySignature,
};

/// Whether `signature` is `public_key`'s over the SHA-256 `hash`.
pub fn verify_p256(public_key: &[u8], hash: &[u8; 32], signature: &[u8]) -> bool {
    if public_key.len() != 72 || signature.len() != 64 {
        return false;
    }
    // SAFETY: handles opened here and closed before returning; the slices outlive the calls.
    unsafe {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        if BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_ECDSA_P256_ALGORITHM, windows::core::PCWSTR::null(),
                                       BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0)).is_err() {
            return false;
        }
        let mut key = BCRYPT_KEY_HANDLE::default();
        let imported = BCryptImportKeyPair(algorithm, None, BCRYPT_ECCPUBLIC_BLOB, &mut key, public_key, 0).is_ok();
        let good = imported && BCryptVerifySignature(key, None, hash, signature, BCRYPT_FLAGS(0)).is_ok();
        if imported {
            let _ = BCryptDestroyKey(key);
        }
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
        good
    }
}

/// `count` random bytes from Windows' own generator (BCryptGenRandom, the system's preferred one): for keys that must
/// not be guessed, such as the one Excavate's llama.cpp servers ask of every request.
pub fn random_bytes(count: usize) -> Vec<u8> {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let mut bytes = vec![0u8; count];
    // SAFETY: the buffer is passed with its length; no algorithm handle with this flag.
    let made = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(made.is_ok(), "Windows' random number generator failed");
    bytes
}

/// `random_bytes` as hexadecimal text.
pub fn random_hex(count: usize) -> String {
    random_bytes(count).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod random_tests {
    #[test]
    fn random_keys_differ() {
        let (a, b) = (super::random_hex(16), super::random_hex(16));
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
