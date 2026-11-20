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
    pub server_id: u32,
    pub is_auth: bool,
}

impl AuthMgr {
    pub fn set_auth(&mut self, account: &str, session_key: &str) {
        self.account = account.to_owned();
        self.session_key = session_key.to_owned();
        self.is_auth = !self.session_key.is_empty();
    }

    pub fn cancel_auth(&mut self) {
        self.is_auth = false;
        self.account.clear();
        self.session_key.clear();
    }
}

/// Mirrors `PlatformMgr` (`mPlatform`, `useSdk`, `isCreateRole`).
#[derive(Resource, Debug, Clone, Default)]
pub struct PlatformMgr {
    /// `0` = account/test login, anything else would route through a platform SDK.
    pub platform: i32,
    /// Independent native flag (`PlatformMgr.UseSdk`), set by platform initialization.
    #[allow(
        dead_code,
        reason = "Preserved native SDK state; local account mode has no SDK initialization."
    )]
    pub sdk_enabled: bool,
    /// `PlatformMgr.isCreateRole`: a role was created during this login, not ownership.
    pub is_create_role: bool,
}

impl PlatformMgr {
    #[allow(
        dead_code,
        reason = "Preserved native SDK interface, verified separately from local account login."
    )]
    pub fn use_sdk(&self) -> bool {
        self.sdk_enabled
    }

    /// Account login with the local replacement's account/server ownership lookup.
    /// The APK supplies this outcome through its server response, not a client-side list.
    pub fn login(
        &mut self,
        auth: &mut AuthMgr,
        account: &str,
        session_key: &str,
        server_id: u32,
        has_role: bool,
    ) -> LoginOutcome {
        if self.platform != 0 {
            return LoginOutcome::Rejected("SDK login is not available in this client".into());
        }
        if account.trim().is_empty() {
            return LoginOutcome::Rejected("Account is empty".into());
        }
        auth.set_auth(account, session_key);
        auth.server_id = server_id;
        self.is_create_role = false;
        if has_role {
            LoginOutcome::LoggedIn
        } else {
            LoginOutcome::NoCreateRole
        }
    }

    /// `PlatformMgr.OnCreateRole`.
    pub fn on_create_role(&mut self, auth: &AuthMgr) -> bool {
        if auth.account.is_empty() {
            return false;
        }
        self.is_create_role = true;
        true
    }
}

/// `EstablishPanel.CheckName` error codes (string ids in `cfg.Strings`).
pub const NAME_ERR_EMPTY: i32 = 132;
pub const NAME_ERR_SPECIAL: i32 = 131;
pub const NAME_ERR_LENGTH: i32 = 255;

// EstablishPanel.REGEX_STRING, observed in the native constructor at RVA 0x16D46D4.
pub const SPECIAL_NAME_CHARACTERS: &str =
    " `~!@#$%^&*()+=|{}':;',[].<>/?~！@#￥%&*（）——+|{}【】‘；：”“’。，、？\"‥…▪•";

/// Returns the localization string id of the first violated rule, or `None` when valid.
pub fn check_name(name: &str) -> Option<i32> {
    if name.is_empty() {
        return Some(NAME_ERR_EMPTY);
    }
    if name.chars().any(|c| SPECIAL_NAME_CHARACTERS.contains(c)) {
        return Some(NAME_ERR_SPECIAL);
    }
    // OnClickOk uses System.String.Length, which counts UTF-16 code units.
    if name.encode_utf16().count() <= 1 {
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
        assert_eq!(
            plat.login(&mut auth, "Andlua", "", 5, false),
            LoginOutcome::NoCreateRole
        );
        assert!(!auth.is_auth);
        assert!(plat.on_create_role(&auth));
        assert_eq!(
            plat.login(&mut auth, "Andlua", "", 5, true),
            LoginOutcome::LoggedIn
        );
    }

    #[test]
    fn empty_account_and_sdk_are_rejected() {
        let mut auth = AuthMgr::default();
        let mut plat = PlatformMgr::default();
        assert!(matches!(
            plat.login(&mut auth, "  ", "", 5, false),
            LoginOutcome::Rejected(_)
        ));
        plat.platform = 3;
        assert!(matches!(
            plat.login(&mut auth, "a", "", 5, false),
            LoginOutcome::Rejected(_)
        ));
    }

    #[test]
    fn check_name_matches_establish_panel_rules() {
        assert_eq!(check_name(""), Some(NAME_ERR_EMPTY));
        assert_eq!(check_name("a!b"), Some(NAME_ERR_SPECIAL));
        assert_eq!(check_name("a"), Some(NAME_ERR_LENGTH));
        assert_eq!(check_name("Builder_UA"), None);
        assert_eq!(check_name("亚历山大"), None);
        assert_eq!(check_name("Two Words"), Some(NAME_ERR_SPECIAL));
        assert_eq!(check_name("\t\t"), None);
        assert_eq!(check_name("🧑"), None);
    }

    #[test]
    fn unauthenticated_role_completion_is_rejected() {
        let mut plat = PlatformMgr::default();
        assert!(!plat.on_create_role(&AuthMgr::default()));
        assert!(!plat.is_create_role);
    }

    #[test]
    fn auth_uses_native_session_key_semantics_and_cancel_clears_identity() {
        let mut auth = AuthMgr::default();
        auth.set_auth("  Account  ", "");
        assert_eq!(auth.account, "  Account  ");
        assert!(!auth.is_auth);
        auth.set_auth("", "session");
        assert!(auth.is_auth);
        auth.cancel_auth();
        assert!(auth.account.is_empty());
        assert!(auth.session_key.is_empty());
        assert!(!auth.is_auth);
    }

    #[test]
    fn sdk_flag_and_creation_event_are_independent_of_account_ownership() {
        let mut auth = AuthMgr::default();
        let mut plat = PlatformMgr {
            sdk_enabled: true,
            ..Default::default()
        };
        assert!(plat.use_sdk());
        assert_eq!(
            plat.login(&mut auth, "Account", "session", 10, true),
            LoginOutcome::LoggedIn
        );
        assert!(!plat.is_create_role);
        assert!(plat.on_create_role(&auth));
        assert!(plat.is_create_role);
        assert_eq!(
            plat.login(&mut auth, "Account", "session", 10, true),
            LoginOutcome::LoggedIn
        );
        assert!(!plat.is_create_role);
        plat.platform = 3;
        plat.sdk_enabled = false;
        assert!(!plat.use_sdk());
        assert!(matches!(
            plat.login(&mut auth, "Account", "session", 10, true),
            LoginOutcome::Rejected(_)
        ));
    }
}
