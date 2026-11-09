//! Saved tokens, kept in the OS credential store (Windows Credential Manager),
//! encrypted for the current Windows user. Never written to a plain file.
//! One entry per account, keyed by Discord user id.

const SERVICE: &str = "ferrochat";
/// The single entry used before multiple accounts; read once and migrated.
const LEGACY_ACCOUNT: &str = "discord-token";

fn entry(user_id: Option<&str>) -> Result<keyring::Entry, String> {
    let account = match user_id {
        Some(id) => format!("discord-token-{id}"),
        None => LEGACY_ACCOUNT.to_owned(),
    };
    keyring::Entry::new(SERVICE, &account).map_err(|e| e.to_string())
}

/// `None` reads the pre-accounts entry.
pub fn load(user_id: Option<&str>) -> Result<Option<String>, String> {
    match entry(user_id)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// User ids with a saved login, found by name in the credential store. For a
/// settings file that doesn't list its accounts (written before accounts
/// existed, or lost), so a saved login is still found at start.
#[cfg(windows)]
pub fn saved_ids() -> Vec<String> {
    use windows::core::w;
    use windows::Win32::Security::Credentials::{CredEnumerateW, CredFree, CREDENTIALW, CRED_ENUMERATE_FLAGS};
    let mut ids = Vec::new();
    unsafe {
        let mut count = 0u32;
        let mut list: *mut *mut CREDENTIALW = std::ptr::null_mut();
        // keyring names entries "<account>.<service>"; the filter may only end in `*`.
        if CredEnumerateW(w!("discord-token-*"), Some(CRED_ENUMERATE_FLAGS(0)), &mut count, &mut list).is_err() {
            return ids;
        }
        for index in 0..count as usize {
            let credential = &**list.add(index);
            let name = credential.TargetName.to_string().unwrap_or_default();
            let id = name.strip_prefix("discord-token-").and_then(|rest| rest.strip_suffix(".ferrochat")).unwrap_or_default();
            if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
                ids.push(id.to_owned());
            }
        }
        CredFree(list as *const std::ffi::c_void);
    }
    ids
}

#[cfg(not(windows))]
pub fn saved_ids() -> Vec<String> {
    Vec::new()
}

/// Saves under the account's id and drops the legacy entry.
pub fn save(user_id: &str, token: &str) -> Result<(), String> {
    entry(Some(user_id))?.set_password(token).map_err(|e| e.to_string())?;
    forget(None)
}

/// `None` removes the legacy entry.
pub fn forget(user_id: Option<&str>) -> Result<(), String> {
    match entry(user_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    /// Uses a separate entry so the real saved login is untouched.
    #[test]
    #[ignore = "writes to the OS credential store"]
    fn round_trip() {
        let entry = keyring::Entry::new("ferrochat-test", "round-trip").unwrap();
        entry.set_password("secret").unwrap();
        assert_eq!(entry.get_password().unwrap(), "secret");
        entry.delete_credential().unwrap();
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
    }
}

#[cfg(all(test, windows))]
mod store_tests {
    /// Reads only names (no secrets) from the real store; must not panic.
    #[test]
    fn lists_saved_logins_without_failing() {
        for id in super::saved_ids() {
            assert!(id.bytes().all(|b| b.is_ascii_digit()), "ids are numbers: {id}");
        }
    }
}
