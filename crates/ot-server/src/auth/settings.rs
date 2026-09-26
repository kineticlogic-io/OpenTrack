//! Sign-in settings (Settings → Security): session length, password
//! sign-in, SAML single sign-on, trusting OpenStare's sign-in, and which
//! client certificates act as which account.

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
    fn roles_map_first_match_or_default() {
        let m = OpenstareSettings::default().role_mapping;
        let r = |v: &str, d| AuthSettings::map_role(&m, &[v.to_owned()], d);
        assert_eq!(r("Operator", None), Some(Role::TrackManager));
        assert_eq!(r("guest", None), None);
        assert_eq!(r("guest", Some(Role::Viewer)), Some(Role::Viewer));
    }
}
