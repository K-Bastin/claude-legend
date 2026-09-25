//! Passwords and key passphrases are kept in the system keyring (Secret
//! Service on Linux, Credential Manager on Windows), never in settings.json.

use crate::config::SyncTarget;

const SERVICE: &str = "claude-legend";

fn account(target: &SyncTarget) -> Option<String> {
    match target {
        SyncTarget::Sftp {
            host, port, user, ..
        } => Some(format!("sftp:{user}@{host}:{port}")),
        SyncTarget::Ftp {
            host, port, user, ..
        } => Some(format!("ftp:{user}@{host}:{port}")),
        SyncTarget::Webdav { url, user } => Some(format!("webdav:{user}@{url}")),
        SyncTarget::None | SyncTarget::Folder { .. } => None,
    }
}

pub fn load(target: &SyncTarget) -> Option<String> {
    let entry = keyring::Entry::new(SERVICE, &account(target)?).ok()?;
    entry.get_password().ok()
}

pub fn has(target: &SyncTarget) -> bool {
    load(target).is_some()
}

/// Stores the secret, or removes it when empty.
pub fn save(target: &SyncTarget, secret: &str) -> anyhow::Result<()> {
    let Some(account) = account(target) else {
        return Ok(());
    };
    let entry = keyring::Entry::new(SERVICE, &account)
        .map_err(|e| anyhow::anyhow!("trousseau du système indisponible : {e}"))?;
    if secret.is_empty() {
        let _ = entry.delete_credential();
    } else {
        entry.set_password(secret).map_err(|e| {
            anyhow::anyhow!("impossible d'enregistrer le mot de passe dans le trousseau : {e}")
        })?;
    }
    Ok(())
}
