//! Named accounts: one `MLRC/accounts.json`, one entry per login.
//!
//! A fork picks one by label; the configuration names the default for everything else.
//! The file that used to hold a single session (`auth.json`) is migrated on first read
//! and then removed, so an upgraded installation keeps its account without being asked.

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::port::DataRoot;

use super::yggdrasil::Session;

pub const FILE_NAME: &str = "accounts.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Accounts {
    pub schema: u32,
    /// Label a launch uses when neither the fork nor the configuration names one.
    pub default: String,
    pub accounts: Vec<Account>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// What the user calls this account: the profile name by default, or `--name`.
    pub label: String,
    #[serde(flatten)]
    pub session: Session,
}

impl Default for Accounts {
    fn default() -> Self {
        Self {
            schema: 1,
            default: String::new(),
            accounts: Vec::new(),
        }
    }
}

impl Accounts {
    pub fn path(root: &DataRoot) -> std::path::PathBuf {
        root.mlrc_dir().join(FILE_NAME)
    }

    pub fn load(root: &DataRoot) -> MlrcResult<Self> {
        let path = Self::path(root);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|err| {
                MlrcError::new(
                    ErrorCode::Config,
                    format!("invalid {}: {err}", path.display()),
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::migrate_legacy(root))
            }
            Err(err) => Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", path.display()),
            )),
        }
    }

    /// Adopts the single-session file written before accounts existed.
    fn migrate_legacy(root: &DataRoot) -> Self {
        let Some(session) = Session::load(root).ok().flatten() else {
            return Self::default();
        };

        let mut accounts = Self::default();
        let label = accounts.put(None, session);
        accounts.default = label;

        // Only drop the old file once the new one is safely on disk.
        if accounts.save(root).is_ok() {
            let _ = Session::remove(root);
        }
        accounts
    }

    pub fn save(&self, root: &DataRoot) -> MlrcResult<()> {
        let path = Self::path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let text = serde_json::to_string_pretty(self)
            .map_err(|err| MlrcError::new(ErrorCode::Io, err.to_string()))?;

        let part = path.with_extension("json.part");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&part).map_err(|err| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot create {}: {err}", part.display()),
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }

        std::fs::rename(&part, &path).map_err(|err| {
            let _ = std::fs::remove_file(&part);
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot write {}: {err}", path.display()),
            )
        })
    }

    pub fn get(&self, label: &str) -> Option<&Account> {
        self.accounts
            .iter()
            .find(|account| account.label.eq_ignore_ascii_case(label))
    }

    /// Stores a session, replacing an entry with the same label.
    pub fn put(&mut self, label: Option<&str>, session: Session) -> String {
        let label = match label.map(str::trim).filter(|label| !label.is_empty()) {
            Some(label) => label.to_string(),
            None => session.name.clone(),
        };

        match self
            .accounts
            .iter_mut()
            .find(|account| account.label.eq_ignore_ascii_case(&label))
        {
            Some(existing) => existing.session = session,
            None => self.accounts.push(Account {
                label: label.clone(),
                session,
            }),
        }

        if self.default.is_empty() {
            self.default = label.clone();
        }
        label
    }

    pub fn remove(&mut self, label: &str) -> bool {
        let before = self.accounts.len();
        self.accounts
            .retain(|account| !account.label.eq_ignore_ascii_case(label));
        let removed = self.accounts.len() != before;

        if removed && self.default.eq_ignore_ascii_case(label) {
            self.default = self
                .accounts
                .first()
                .map(|account| account.label.clone())
                .unwrap_or_default();
        }
        removed
    }

    pub fn set_default(&mut self, label: &str) -> MlrcResult<()> {
        if self.get(label).is_none() {
            return Err(self.unknown(label));
        }
        self.default = self
            .get(label)
            .map(|account| account.label.clone())
            .unwrap_or_else(|| label.to_string());
        Ok(())
    }

    /// The account a launch should use.
    ///
    /// An empty label means "the default one"; with no default set and exactly one
    /// account stored, that account is the answer, because a single login needs no
    /// configuration to be used.
    pub fn resolve(&self, label: &str) -> MlrcResult<&Account> {
        let label = label.trim();

        if label.is_empty() {
            if !self.default.is_empty() {
                return self
                    .get(&self.default)
                    .ok_or_else(|| self.unknown(&self.default.clone()));
            }
            return match self.accounts.as_slice() {
                [] => Err(MlrcError::new(
                    ErrorCode::AuthFailed,
                    "no account is stored; run `mlrc auth login`",
                )),
                [only] => Ok(only),
                _ => Err(MlrcError::new(
                    ErrorCode::AuthFailed,
                    format!(
                        "several accounts are stored ({}); name one with auth.account",
                        self.labels().join(", ")
                    ),
                )
                .with("hint", "`mlrc config set auth.account <label>`")),
            };
        }

        self.get(label).ok_or_else(|| self.unknown(label))
    }

    pub fn labels(&self) -> Vec<String> {
        self.accounts
            .iter()
            .map(|account| account.label.clone())
            .collect()
    }

    fn unknown(&self, label: &str) -> MlrcError {
        let known = self.labels();
        MlrcError::new(ErrorCode::AuthFailed, format!("no account named {label}"))
            .with("account", label.to_string())
            .with(
                "hint",
                if known.is_empty() {
                    "run `mlrc auth login` first".to_string()
                } else {
                    format!("known accounts: {}", known.join(", "))
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> Session {
        Session {
            server: "https://example.test/".to_string(),
            username: name.to_string(),
            name: name.to_string(),
            uuid: "b50ad385-829d-3141-a216-7e7d7539ba7f".to_string(),
            access_token: "token".to_string(),
            client_token: "client".to_string(),
            kind: String::new(),
            server_id: String::new(),
        }
    }

    #[test]
    fn a_login_becomes_the_first_default() {
        let mut accounts = Accounts::default();
        assert_eq!(accounts.put(None, session("Steve")), "Steve");
        assert_eq!(accounts.default, "Steve");

        // A second account does not steal the default.
        accounts.put(None, session("Alex"));
        assert_eq!(accounts.default, "Steve");
        assert_eq!(accounts.labels(), vec!["Steve", "Alex"]);
    }

    #[test]
    fn a_label_can_be_chosen() {
        let mut accounts = Accounts::default();
        assert_eq!(accounts.put(Some("main"), session("Steve")), "main");
        assert_eq!(accounts.get("main").expect("stored").session.name, "Steve");
        // The same label replaces, it does not duplicate.
        accounts.put(Some("main"), session("Alex"));
        assert_eq!(accounts.accounts.len(), 1);
        assert_eq!(accounts.get("main").expect("stored").session.name, "Alex");
    }

    #[test]
    fn resolution_prefers_the_named_account() {
        let mut accounts = Accounts::default();
        accounts.put(None, session("Steve"));
        accounts.put(None, session("Alex"));
        accounts.default = "Alex".to_string();

        assert_eq!(accounts.resolve("").expect("default").session.name, "Alex");
        assert_eq!(
            accounts.resolve("Steve").expect("named").session.name,
            "Steve"
        );
        assert!(accounts.resolve("Nobody").is_err());
    }

    #[test]
    fn one_account_needs_no_configuration() {
        let mut accounts = Accounts::default();
        accounts.put(Some("main"), session("Steve"));
        // `put` set the default; clear it to test the fallback.
        accounts.default.clear();
        assert_eq!(accounts.resolve("").expect("the only one").label, "main");
    }

    #[test]
    fn removing_the_default_promotes_another() {
        let mut accounts = Accounts::default();
        accounts.put(None, session("Steve"));
        accounts.put(None, session("Alex"));
        accounts.default = "Steve".to_string();

        assert!(accounts.remove("Steve"));
        assert_eq!(accounts.default, "Alex");
        assert!(!accounts.remove("Steve"));
    }
}
