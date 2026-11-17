use anyhow::Result;
use serde::{Serialize, de::DeserializeOwned};

const SERVICE: &str = "Sonic Native";

pub fn read<T: DeserializeOwned>(account: &str) -> Result<Option<T>> {
    match keyring::Entry::new(SERVICE, account)?.get_secret() {
        Ok(secret) => Ok(Some(serde_json::from_slice(&secret)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn save<T: Serialize>(account: &str, value: &T) -> Result<()> {
    keyring::Entry::new(SERVICE, account)?.set_secret(&serde_json::to_vec(value)?)?;
    Ok(())
}

pub fn remove(account: &str) -> Result<()> {
    match keyring::Entry::new(SERVICE, account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
