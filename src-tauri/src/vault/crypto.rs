use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const DEK_LEN: usize = 32;
const NONCE_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

pub fn production_kdf() -> KdfParams {
    KdfParams {
        memory_kib: 65_536,
        iterations: 3,
        parallelism: 1,
    }
}

pub fn test_kdf() -> KdfParams {
    KdfParams {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
    }
}

/// Lowest parameters unlock will accept. Below this, a tampered database could
/// make offline guessing cheap. Tests that only exercise AES use `test_kdf`.
pub fn minimum_kdf() -> KdfParams {
    KdfParams {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

#[derive(Debug)]
pub struct CryptoError;

pub fn random_dek() -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    random_bytes(DEK_LEN)
}

pub fn encrypt(key: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError)?;
    let nonce = random_bytes(NONCE_LEN)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| CryptoError)?;
    Ok((nonce.to_vec(), ciphertext))
}

pub fn decrypt(
    key: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if nonce.len() != NONCE_LEN {
        return Err(CryptoError);
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError)?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| CryptoError)?;
    Ok(Zeroizing::new(plaintext))
}

pub struct WrappedDek {
    pub salt: Vec<u8>,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub fn wrap_dek(password: &str, params: &KdfParams, dek: &[u8]) -> Result<WrappedDek, CryptoError> {
    let salt = random_bytes(16)?;
    let kek = derive_kek(password, &salt, params)?;
    let (nonce, ciphertext) = encrypt(&kek, dek)?;
    Ok(WrappedDek {
        salt: salt.to_vec(),
        nonce,
        ciphertext,
    })
}

pub fn unwrap_dek(
    password: &str,
    params: &KdfParams,
    wrapped: &WrappedDek,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let kek = derive_kek(password, &wrapped.salt, params)?;
    decrypt(&kek, &wrapped.nonce, &wrapped.ciphertext)
}

fn derive_kek(
    password: &str,
    salt: &[u8],
    params: &KdfParams,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let argon = Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        None,
    )
    .map_err(|_| CryptoError)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon);
    let mut kek = Zeroizing::new(vec![0u8; DEK_LEN]);
    argon
        .hash_password_into(password.as_bytes(), salt, &mut kek)
        .map_err(|_| CryptoError)?;
    Ok(kek)
}

fn random_bytes(len: usize) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|_| CryptoError)?;
    Ok(Zeroizing::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::{decrypt, encrypt, random_dek, test_kdf, unwrap_dek, wrap_dek};

    #[test]
    fn encrypt_roundtrip_and_rejects_tampering() {
        let key = random_dek().expect("dek");
        let (nonce, ciphertext) = encrypt(&key, b"telegraf-secret").expect("encrypt");
        let plaintext = decrypt(&key, &nonce, &ciphertext).expect("decrypt");
        assert_eq!(plaintext.as_slice(), b"telegraf-secret");

        let mut tampered = ciphertext.clone();
        tampered[0] ^= 0xff;
        assert!(decrypt(&key, &nonce, &tampered).is_err());
    }

    #[test]
    fn wrong_password_does_not_unwrap_the_dek() {
        let dek = random_dek().expect("dek");
        let params = test_kdf();
        let wrapped = wrap_dek("correct horse", &params, &dek).expect("wrap");
        assert!(unwrap_dek("wrong horse", &params, &wrapped).is_err());
        let opened = unwrap_dek("correct horse", &params, &wrapped).expect("unwrap");
        assert_eq!(opened.as_slice(), dek.as_slice());
        assert!(
            !wrapped
                .ciphertext
                .windows(dek.len())
                .any(|window| window == dek.as_slice())
        );
    }
}
