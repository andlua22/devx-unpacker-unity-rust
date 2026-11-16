//! Account authentication flow mirroring the APK's `AuthMgr` + `PlatformMgr` (platform `0` =
//! account/test login) and the `EstablishPanel.CheckName` rules.
//!
//! Recovered via `devx_cli disasm-il2cpp`: `PlatformMgr.Login()` with `mPlatform == 0` pushes
//! `Account` + `SessionKey` into `AuthMgr.SetAuth` and starts the login request; a server that
//! reports "no role" triggers `SNoCreateRole`, which opens `EstablishPanel` with `type = 1`.

use bevy::prelude::*;

/// Result of a login attempt (`AuthMgr.DoLoginRequest` outcome).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// Account already owns a role -> `OnSLoginFinish` -> `MainLobby`.
    LoggedIn,
    /// Account has no role on this server -> `SNoCreateRole` -> `EstablishPanel(type = 1)`.
    NoCreateRole,
    /// Rejected before reaching the server (empty account, etc.).
    Rejected(String),
}

/// Mirrors `AuthMgr` state (`userId`, `sessionKey`, `IsAuth`).
#[derive(Resource, Debug, Clone, Default)]
pub struct AuthMgr {
    pub account: String,
    pub session_key: String,
    pub is_auth: bool,
}

impl AuthMgr {
    pub fn set_auth(&mut self, account: &str, session_key: &str) {
        self.account = account.trim().to_owned();
        self.session_key = session_key.to_owned();
        self.is_auth = !self.account.is_empty();
    }

    pub fn cancel_auth(&mut self) {
        self.is_auth = false;
        self.session_key.clear();
    }

    /// Mirrors `LoginAccoutPanel.<onInit>b__6_0`: prepends `account` to the persisted
    /// `dev_Accounts` list without duplicates.
    pub fn record_dev_account(dev_accounts: &mut Vec<String>, account: &str) {
        let trimmed = account.trim();
        if trimmed.is_empty() {
            return;
        }
        dev_accounts.retain(|a| !a.eq_ignore_ascii_case(trimmed));
        dev_accounts.insert(0, trimmed.to_owned());
        if dev_accounts.len() > 8 {
            dev_accounts.truncate(8);
        }
    }
}

/// Mirrors `PlatformMgr` (`mPlatform`, `useSdk`, `isCreateRole`).
#[derive(Resource, Debug, Clone, Default)]
pub struct PlatformMgr {
    /// `0` = account/test login, anything else would route through a platform SDK.
    pub platform: i32,
    pub is_create_role: bool,
}

impl PlatformMgr {
    pub fn use_sdk(&self) -> bool {
        self.platform != 0
    }

    /// `PlatformMgr.Login()` for `mPlatform == 0`. `created_roles` is the persisted set of
    /// accounts that already finished character creation.
    pub fn login(
        &mut self,
        auth: &mut AuthMgr,
        account: &str,
        session_key: &str,
        created_roles: &[String],
    ) -> LoginOutcome {
        if self.use_sdk() {
            return LoginOutcome::Rejected("SDK login is not available in this client".into());
        }
        if account.trim().is_empty() {
            return LoginOutcome::Rejected("Account is empty".into());
        }
        auth.set_auth(account, session_key);
        self.is_create_role = created_roles.iter().any(|a| a == &auth.account);
        if self.is_create_role {
            LoginOutcome::LoggedIn
        } else {
            LoginOutcome::NoCreateRole
        }
    }

    /// `PlatformMgr.OnCreateRole`.
    pub fn on_create_role(&mut self, auth: &AuthMgr, created_roles: &mut Vec<String>) {
        if !auth.account.is_empty() && !created_roles.contains(&auth.account) {
            created_roles.push(auth.account.clone());
        }
        self.is_create_role = true;
    }
}

/// `EstablishPanel.CheckName` error codes (string ids in `cfg.Strings`).
pub const NAME_ERR_EMPTY: i32 = 132;
pub const NAME_ERR_SPECIAL: i32 = 131;
pub const NAME_ERR_LENGTH: i32 = 255;

/// Returns the localization string id of the first violated rule, or `None` when valid.
pub fn check_name(name: &str) -> Option<i32> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some(NAME_ERR_EMPTY);
    }
    if trimmed
        .chars()
        .any(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
    {
        return Some(NAME_ERR_SPECIAL);
    }
    if trimmed.chars().count() <= 1 {
        return Some(NAME_ERR_LENGTH);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_account_routes_to_role_creation_then_lobby() {
        let mut auth = AuthMgr::default();
        let mut plat = PlatformMgr::default();
        let mut roles = Vec::new();
        assert_eq!(
            plat.login(&mut auth, "Andlua", "", &roles),
            LoginOutcome::NoCreateRole
        );
        assert!(auth.is_auth);
        plat.on_create_role(&auth, &mut roles);
        assert_eq!(
            plat.login(&mut auth, "Andlua", "", &roles),
            LoginOutcome::LoggedIn
        );
    }

    #[test]
    fn empty_account_and_sdk_are_rejected() {
        let mut auth = AuthMgr::default();
        let mut plat = PlatformMgr::default();
        assert!(matches!(plat.login(&mut auth, "  ", "", &[]), LoginOutcome::Rejected(_)));
        plat.platform = 3;
        assert!(matches!(plat.login(&mut auth, "a", "", &[]), LoginOutcome::Rejected(_)));
    }

    #[test]
    fn check_name_matches_establish_panel_rules() {
        assert_eq!(check_name(""), Some(NAME_ERR_EMPTY));
        assert_eq!(check_name("a!b"), Some(NAME_ERR_SPECIAL));
        assert_eq!(check_name("a"), Some(NAME_ERR_LENGTH));
        assert_eq!(check_name("Builder_UA"), None);
        assert_eq!(check_name("亚历山大"), None);
        let mut accounts = vec!["Alpha".to_owned(), "Beta".to_owned()];
        AuthMgr::record_dev_account(&mut accounts, "beta");
        assert_eq!(accounts, vec!["beta".to_owned(), "Alpha".to_owned()]);
    }
}
