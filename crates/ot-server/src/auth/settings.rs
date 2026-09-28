//! Sign-in settings (Settings → Security): session length, password
//! sign-in, SAML single sign-on, trusting OpenStare's sign-in, which
//! client certificates act as which account, and the account policy
//! (passwords, lockout, sessions, inactivity, audit retention) with DoD ASD
//! STIG defaults.

use serde::{Deserialize, Serialize};

use super::Role;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthSettings {
    /// How long a sign-in lasts (hours; default 24).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_hours: Option<f64>,
    /// Only single sign-on (SAML or OpenStare): no password sign-in.
    pub disable_password_login: bool,
    pub saml: SamlSettings,
    pub openstare: OpenstareSettings,
    /// Client certificates (by subject common name) and the account each
    /// acts as.
    pub client_certs: Vec<CertUser>,
    /// Rules for local accounts' passwords.
    pub password: PasswordPolicy,
    /// Locking an account after failed password sign-ins.
    pub lockout: LockoutPolicy,
    /// Browser sessions: idle timeout and how many at once.
    pub sessions: SessionPolicy,
    /// Turning off accounts nobody uses.
    pub inactivity: InactivityPolicy,
    /// Keeping the audit record.
    pub audit: AuditPolicy,
}

/// Password rules for local accounts. They apply when a password is set;
/// existing passwords keep working until they change or expire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PasswordPolicy {
    pub min_length: usize,
    pub require_upper: bool,
    pub require_lower: bool,
    pub require_digit: bool,
    /// A character that is not a letter or a digit.
    pub require_special: bool,
    /// A new password may not be any of the last this many (0: any).
    pub history: usize,
    /// When you change your own password, at least this many characters
    /// must differ from the current one (edit distance; 0: any change).
    pub min_changed_chars: usize,
    /// Hours before you can change your own password again (an admin's
    /// reset and a forced change are exempt; 0: any time).
    pub min_age_hours: f64,
    /// Days a password lasts; then it must change at the next sign-in
    /// (0: forever).
    pub max_age_days: f64,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: 15,
            require_upper: true,
            require_lower: true,
            require_digit: true,
            require_special: true,
            history: 5,
            min_changed_chars: 8,
            min_age_hours: 24.0,
            max_age_days: 60.0,
        }
    }
}

/// Consecutive failed password sign-ins within the window lock the account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LockoutPolicy {
    /// Failures that lock (0: never lock).
    pub max_failures: u32,
    pub window_minutes: f64,
    /// How long it stays locked (0: until an admin unlocks it).
    pub lock_minutes: f64,
}

impl Default for LockoutPolicy {
    fn default() -> Self {
        Self {
            max_failures: 3,
            window_minutes: 15.0,
            lock_minutes: 15.0,
        }
    }
}

/// Browser sessions (API tokens are not sessions: they only expire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionPolicy {
    /// Minutes without use that end a session.
    pub idle_minutes: f64,
    /// The same for admins (`None`: as everyone's).
    pub admin_idle_minutes: Option<f64>,
    /// Sessions an account may have at once; a new sign-in ends the oldest
    /// (0: no limit).
    pub max_per_account: u32,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            idle_minutes: 15.0,
            admin_idle_minutes: Some(10.0),
            max_per_account: 3,
        }
    }
}

impl SessionPolicy {
    /// The idle timeout for an account of `role`, in ms.
    pub fn idle_ms(&self, role: Role) -> i64 {
        let m = match (role, self.admin_idle_minutes) {
            (Role::Admin, Some(a)) => a,
            _ => self.idle_minutes,
        };
        (m * 60_000.0) as i64
    }
}

/// Accounts not signed in for this long are turned off; an admin turns
/// them on again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InactivityPolicy {
    /// 0: never.
    pub disable_after_days: f64,
    /// Accounts (emails) never turned off this way: break-glass accounts.
    pub exempt: Vec<String>,
}

impl Default for InactivityPolicy {
    fn default() -> Self {
        Self {
            disable_after_days: 35.0,
            exempt: Vec::new(),
        }
    }
}

impl InactivityPolicy {
    pub fn exempts(&self, email: &str) -> bool {
        self.exempt
            .iter()
            .any(|e| e.trim().eq_ignore_ascii_case(email.trim()))
    }
}

/// How long the audit record keeps rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuditPolicy {
    /// Days; older rows are purged (the purge is recorded and the chain
    /// stays verifiable). 0: keep forever.
    pub retention_days: f64,
}

/// SAML 2.0 single sign-on, as OpenStare's: OpenTrack is the service
/// provider, the identity provider (Keycloak, Entra ID...) signs users in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SamlSettings {
    pub enabled: bool,
    /// The identity provider's metadata XML (fills the next three).
    pub idp_metadata_xml: String,
    pub idp_entity_id: String,
    /// Where users are sent to sign in (HTTP-Redirect binding).
    pub sso_url: String,
    /// The identity provider's signing certificate (PEM or base64 DER).
    pub signing_cert: String,
    /// The assertion attribute that carries the user's role.
    pub role_attribute: String,
    /// Role attribute values to OpenTrack roles; the first match wins.
    pub role_mapping: Vec<RoleMap>,
    /// The role of a user whose attribute matches nothing; `None` refuses them.
    pub default_role: Option<Role>,
    /// Whether single sign-on may make an admin (else the mapping stops at
    /// track manager).
    pub allow_admin: bool,
    pub button_label: String,
}

impl Default for SamlSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            idp_metadata_xml: String::new(),
            idp_entity_id: String::new(),
            sso_url: String::new(),
            signing_cert: String::new(),
            role_attribute: "Role".into(),
            role_mapping: Vec::new(),
            default_role: None,
            allow_admin: false,
            button_label: "Sign in with SSO".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleMap {
    pub value: String,
    pub role: Role,
}

/// Accept OpenStare's sign-in: a browser signed in to OpenStare on this
/// host, or an OpenStare API token, is let in with a role mapped from its
/// OpenStare role. OpenStare checks each (with a short cache), so its
/// sign-outs and revocations hold here too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenstareSettings {
    pub enabled: bool,
    /// OpenStare's API as this server reaches it (`/api/auth/me` is under it).
    pub api_url: String,
    /// OpenStare's sign-in page as browsers reach it (for the button).
    pub login_url: String,
    /// OpenStare roles (admin, operator, analyst, guest) to OpenTrack roles;
    /// a role not listed is refused.
    pub role_mapping: Vec<RoleMap>,
}

impl Default for OpenstareSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            api_url: "http://127.0.0.1:3001".into(),
            login_url: String::new(),
            role_mapping: vec![
                RoleMap {
                    value: "admin".into(),
                    role: Role::Admin,
                },
                RoleMap {
                    value: "operator".into(),
                    role: Role::TrackManager,
                },
                RoleMap {
                    value: "analyst".into(),
                    role: Role::Viewer,
                },
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertUser {
    /// The certificate subject's common name (CN).
    pub common_name: String,
    /// The account (email) it acts as.
    pub user: String,
}

impl AuthSettings {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(h) = self.session_hours
            && !(0.25..=720.0).contains(&h)
        {
            return Err("session_hours: between 0.25 and 720".into());
        }
        if self.saml.enabled {
            if self.saml.idp_entity_id.trim().is_empty()
                || self.saml.sso_url.trim().is_empty()
                || self.saml.signing_cert.trim().is_empty()
            {
                return Err(
                    "saml: the identity provider's entity id, sign-in URL and signing certificate are needed (paste its metadata)"
                        .into(),
                );
            }
            if !cfg!(feature = "saml") {
                return Err("saml: this build has no SAML support".into());
            }
        }
        if self.openstare.enabled && !self.openstare.api_url.starts_with("http") {
            return Err("openstare.api_url: an http(s) URL".into());
        }
        if self.disable_password_login && !self.saml.enabled && !self.openstare.enabled {
            return Err(
                "disable_password_login: turn on SAML or OpenStare sign-in first, or no one can sign in"
                    .into(),
            );
        }
        for c in &self.client_certs {
            if c.common_name.trim().is_empty() || c.user.trim().is_empty() {
                return Err("client_certs: each needs a common name and an account".into());
            }
        }
        let p = &self.password;
        if !(super::MIN_PASSWORD..=128).contains(&p.min_length) {
            return Err(format!(
                "password.min_length: between {} and 128",
                super::MIN_PASSWORD
            ));
        }
        if p.history > 24 {
            return Err("password.history: at most 24".into());
        }
        if p.min_changed_chars > p.min_length {
            return Err("password.min_changed_chars: at most password.min_length".into());
        }
        let range = |name: &str, v: f64, max: f64| {
            if v.is_finite() && (0.0..=max).contains(&v) {
                Ok(())
            } else {
                Err(format!("{name}: between 0 and {max}"))
            }
        };
        range("password.min_age_hours", p.min_age_hours, 720.0)?;
        range("password.max_age_days", p.max_age_days, 3650.0)?;
        if p.max_age_days > 0.0 && p.min_age_hours >= p.max_age_days * 24.0 {
            return Err("password.min_age_hours: less than password.max_age_days".into());
        }
        let l = &self.lockout;
        if l.max_failures > 100 {
            return Err("lockout.max_failures: at most 100".into());
        }
        range("lockout.window_minutes", l.window_minutes, 1440.0)?;
        range("lockout.lock_minutes", l.lock_minutes, 10_080.0)?;
        let sn = &self.sessions;
        let idle = |name: &str, v: f64| {
            if v.is_finite() && (1.0..=1440.0).contains(&v) {
                Ok(())
            } else {
                Err(format!("{name}: between 1 and 1440 minutes"))
            }
        };
        idle("sessions.idle_minutes", sn.idle_minutes)?;
        if let Some(a) = sn.admin_idle_minutes {
            idle("sessions.admin_idle_minutes", a)?;
        }
        if sn.max_per_account > 100 {
            return Err("sessions.max_per_account: at most 100".into());
        }
        range(
            "inactivity.disable_after_days",
            self.inactivity.disable_after_days,
            3650.0,
        )?;
        let r = self.audit.retention_days;
        if !(r == 0.0 || (r.is_finite() && (7.0..=36_500.0).contains(&r))) {
            return Err("audit.retention_days: 0 (keep forever) or 7 to 36500".into());
        }
        Ok(())
    }

    /// The role an SSO role value maps to.
    pub fn map_role(mapping: &[RoleMap], values: &[String], default: Option<Role>) -> Option<Role> {
        mapping
            .iter()
            .find(|m| {
                values
                    .iter()
                    .any(|v| v.eq_ignore_ascii_case(m.value.trim()))
            })
            .map(|m| m.role)
            .or(default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_validate() {
        let s: AuthSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(s, AuthSettings::default());
        s.validate().unwrap();
        let bad = AuthSettings {
            disable_password_login: true,
            ..Default::default()
        };
        assert!(bad.validate().is_err(), "no way left to sign in");
        assert!(serde_json::from_value::<AuthSettings>(serde_json::json!({"x": 1})).is_err());
    }

    #[test]
    fn the_account_policy_defaults_to_the_stig_and_is_checked() {
        let s = AuthSettings::default();
        assert_eq!(s.password.min_length, 15);
        assert_eq!(s.password.history, 5);
        assert_eq!(s.lockout.max_failures, 3);
        assert_eq!(s.sessions.idle_ms(Role::Viewer), 15 * 60_000);
        assert_eq!(s.sessions.idle_ms(Role::Admin), 10 * 60_000);
        assert_eq!(s.inactivity.disable_after_days, 35.0);
        assert_eq!(s.audit.retention_days, 0.0);
        // A saved partial policy keeps the other defaults.
        let s: AuthSettings =
            serde_json::from_value(serde_json::json!({"password": {"min_length": 20}})).unwrap();
        assert_eq!((s.password.min_length, s.password.history), (20, 5));
        s.validate().unwrap();
        let bad = |v: serde_json::Value| {
            serde_json::from_value::<AuthSettings>(v)
                .unwrap()
                .validate()
                .is_err()
        };
        assert!(bad(serde_json::json!({"password": {"min_length": 4}})));
        assert!(bad(serde_json::json!({"sessions": {"idle_minutes": 0}})));
        assert!(bad(serde_json::json!({"audit": {"retention_days": 1}})));
        assert!(bad(
            serde_json::json!({"password": {"min_age_hours": 48, "max_age_days": 1}})
        ));
        assert!(
            InactivityPolicy {
                exempt: vec![" Break@Glass ".into()],
                ..Default::default()
            }
            .exempts("break@glass")
        );
    }

    #[test]
    fn roles_map_first_match_or_default() {
        let m = OpenstareSettings::default().role_mapping;
        let r = |v: &str, d| AuthSettings::map_role(&m, &[v.to_owned()], d);
        assert_eq!(r("Operator", None), Some(Role::TrackManager));
        assert_eq!(r("guest", None), None);
        assert_eq!(r("guest", Some(Role::Viewer)), Some(Role::Viewer));
    }
}
