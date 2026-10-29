use std::sync::Mutex;

use zeroize::Zeroizing;

const SERVICE: &str = "filebridge";
const ACCOUNT: &str = "dek";

#[derive(Debug)]
pub struct KeyStoreError;

pub trait KeyStore: Send + Sync {
    fn available(&self) -> bool;
    fn get_dek(&self) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError>;
    fn set_dek(&self, dek: &[u8]) -> Result<(), KeyStoreError>;
}

pub struct MemoryKeyStore {
    available: bool,
    dek: Mutex<Option<Zeroizing<Vec<u8>>>>,
}

impl MemoryKeyStore {
    #[cfg(test)]
    pub fn available() -> Self {
        Self {
            available: true,
            dek: Mutex::new(None),
        }
    }

    pub fn unavailable() -> Self {
        Self {
            available: false,
            dek: Mutex::new(None),
        }
    }
}

impl KeyStore for MemoryKeyStore {
    fn available(&self) -> bool {
        self.available
    }

    fn get_dek(&self) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        if !self.available {
            return Err(KeyStoreError);
        }
        let dek = self.dek.lock().map_err(|_| KeyStoreError)?;
        Ok(dek.clone())
    }

    fn set_dek(&self, dek: &[u8]) -> Result<(), KeyStoreError> {
        if !self.available {
            return Err(KeyStoreError);
        }
        let mut slot = self.dek.lock().map_err(|_| KeyStoreError)?;
        *slot = Some(Zeroizing::new(dek.to_vec()));
        Ok(())
    }
}

pub struct OsKeyStore;

impl KeyStore for OsKeyStore {
    fn available(&self) -> bool {
        let Ok(entry) = keyring::Entry::new(SERVICE, ACCOUNT) else {
            return false;
        };
        match entry.get_secret() {
            Ok(secret) => {
                // A successful probe returns the DEK. Drop it through Zeroizing.
                let _secret = Zeroizing::new(secret);
                true
            }
            Err(keyring::Error::NoEntry) => true,
            Err(_) => false,
        }
    }

    fn get_dek(&self) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|_| KeyStoreError)?;
        match entry.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(KeyStoreError),
        }
    }

    fn set_dek(&self, dek: &[u8]) -> Result<(), KeyStoreError> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|_| KeyStoreError)?;
        entry.set_secret(dek).map_err(|_| KeyStoreError)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn manifest_enables_real_platform_stores() {
        let manifest = include_str!("../../Cargo.toml");
        assert!(
            manifest.contains("apple-native")
                && manifest.contains("windows-native")
                && manifest.contains("sync-secret-service")
                && manifest.contains("crypto-rust"),
            "keyring must not fall back to its in-memory mock"
        );
    }
}
