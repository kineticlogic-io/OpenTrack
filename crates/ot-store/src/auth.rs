//! Accounts, revoked session tokens, API tokens and the sign-in settings.
//! Password hashing and token signing are the server's; this stores what
//! they need.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::Value;

use crate::sqlite::{Db, Result, StoreError, now_ms};

/// An account. The password hash never leaves the store in this.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub active: bool,
    /// `local` (made here) or `saml` (made at its first single sign-on).
    pub origin: String,
    /// Whether it can sign in with a password.
    pub has_password: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_login_at_ms: Option<i64>,
    #[serde(skip)]
    pub tokens_valid_from_ms: i64,
}

/// A new account.
#[derive(Debug, Clone)]
pub struct NewUser<'a> {
    pub id: &'a str,
    pub email: &'a str,
    pub name: &'a str,
    pub role: &'a str,
    pub password_hash: Option<&'a str>,
    pub origin: &'a str,
}

/// An API token's record (the token itself is shown once, never stored).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiToken {
    pub jti: String,
    pub name: String,
    pub user_id: String,
    pub user_email: String,
    pub created_by: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

const USER_COLUMNS: &str = "id, email, name, role, active, origin, password_hash IS NOT NULL,
    created_at_ms, updated_at_ms, last_login_at_ms, tokens_valid_from_ms";

fn user(r: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        email: r.get(1)?,
        name: r.get(2)?,
        role: r.get(3)?,
        active: r.get(4)?,
        origin: r.get(5)?,
        has_password: r.get(6)?,
        created_at_ms: r.get(7)?,
        updated_at_ms: r.get(8)?,
        last_login_at_ms: r.get(9)?,
        tokens_valid_from_ms: r.get(10)?,
    })
}

impl Db {
    pub fn users(&self) -> Result<Vec<User>> {
        let mut st = self
            .connection()
            .prepare(&format!("SELECT {USER_COLUMNS} FROM users ORDER BY email"))?;
        let rows = st.query_map([], user)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn user_count(&self) -> Result<i64> {
        Ok(self
            .connection()
            .query_row("SELECT count(*) FROM users", [], |r| r.get(0))?)
    }

    pub fn user(&self, id: &str) -> Result<Option<User>> {
        Ok(self
            .connection()
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
                [id],
                user,
            )
            .optional()?)
    }

    pub fn user_by_email(&self, email: &str) -> Result<Option<User>> {
        Ok(self
            .connection()
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE email = ?1"),
                [email.trim()],
                user,
            )
            .optional()?)
    }

    /// The password hash of an active account, by email.
    pub fn password_hash(&self, email: &str) -> Result<Option<(User, Option<String>)>> {
        let Some(u) = self.user_by_email(email)? else {
            return Ok(None);
        };
        let hash: Option<String> = self.connection().query_row(
            "SELECT password_hash FROM users WHERE id = ?1",
            [&u.id],
            |r| r.get(0),
        )?;
        Ok(Some((u, hash)))
    }

    pub fn create_user(&mut self, u: &NewUser<'_>) -> Result<User> {
        let now = now_ms();
        let email = u.email.trim();
        if self.user_by_email(email)?.is_some() {
            return Err(StoreError::Conflict(format!(
                "an account for {email} exists"
            )));
        }
        self.connection().execute(
            "INSERT INTO users (id, email, name, role, password_hash, origin, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![u.id, email, u.name.trim(), u.role, u.password_hash, u.origin, now],
        )?;
        self.user(u.id)?
            .ok_or_else(|| StoreError::NotFound(format!("user {}", u.id)))
    }

    /// Change an account's name, role or active flag; deactivating also
    /// ends its sessions.
    pub fn update_user(
        &mut self,
        id: &str,
        name: Option<&str>,
        role: Option<&str>,
        active: Option<bool>,
    ) -> Result<User> {
        let now = now_ms();
        let n = self.connection().execute(
            "UPDATE users SET name = coalesce(?2, name), role = coalesce(?3, role),
                 active = coalesce(?4, active), updated_at_ms = ?5,
                 tokens_valid_from_ms = CASE WHEN ?4 = 0 THEN ?5 ELSE tokens_valid_from_ms END
             WHERE id = ?1",
            params![id, name.map(str::trim), role, active, now],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("user {id}")));
        }
        self.user(id)?
            .ok_or_else(|| StoreError::NotFound(format!("user {id}")))
    }

    /// Set (or, with `None`, remove) an account's password.
    pub fn set_password_hash(&mut self, id: &str, hash: Option<&str>) -> Result<()> {
        let n = self.connection().execute(
            "UPDATE users SET password_hash = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, hash, now_ms()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("user {id}")));
        }
        Ok(())
    }

    pub fn delete_user(&mut self, id: &str) -> Result<()> {
        let n = self
            .connection()
            .execute("DELETE FROM users WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("user {id}")));
        }
        Ok(())
    }

    pub fn note_login(&mut self, id: &str) -> Result<()> {
        self.connection().execute(
            "UPDATE users SET last_login_at_ms = ?2 WHERE id = ?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }

    /// End every session and API token of an account issued before now.
    pub fn revoke_user_tokens(&mut self, id: &str) -> Result<()> {
        let now = now_ms();
        self.connection().execute(
            "UPDATE users SET tokens_valid_from_ms = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        self.connection().execute(
            "UPDATE api_tokens SET revoked_at_ms = ?2 WHERE user_id = ?1 AND revoked_at_ms IS NULL",
            params![id, now],
        )?;
        Ok(())
    }

    /// Revoke one session token (sign out), and forget expired revocations.
    pub fn revoke_token(&mut self, jti: &str, expires_at_ms: i64) -> Result<()> {
        let now = now_ms();
        self.connection().execute(
            "INSERT OR IGNORE INTO revoked_tokens (jti, expires_at_ms) VALUES (?1, ?2)",
            params![jti, expires_at_ms],
        )?;
        self.connection()
            .execute("DELETE FROM revoked_tokens WHERE expires_at_ms < ?1", [now])?;
        Ok(())
    }

    /// Whether a token id was revoked: signed out, or an API token revoked.
    pub fn token_revoked(&self, jti: &str) -> Result<bool> {
        Ok(self.connection().query_row(
            "SELECT EXISTS (SELECT 1 FROM revoked_tokens WHERE jti = ?1)
                 OR EXISTS (SELECT 1 FROM api_tokens WHERE jti = ?1 AND revoked_at_ms IS NOT NULL)",
            [jti],
            |r| r.get(0),
        )?)
    }

    pub fn add_api_token(
        &mut self,
        jti: &str,
        name: &str,
        user_id: &str,
        created_by: &str,
        expires_at_ms: i64,
    ) -> Result<()> {
        self.connection().execute(
            "INSERT INTO api_tokens (jti, name, user_id, created_by, created_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                jti,
                name.trim(),
                user_id,
                created_by,
                now_ms(),
                expires_at_ms
            ],
        )?;
        Ok(())
    }

    pub fn api_tokens(&self) -> Result<Vec<ApiToken>> {
        let mut st = self.connection().prepare(
            "SELECT t.jti, t.name, t.user_id, u.email, t.created_by, t.created_at_ms,
                    t.expires_at_ms, t.revoked_at_ms
             FROM api_tokens t JOIN users u ON u.id = t.user_id
             ORDER BY t.created_at_ms DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ApiToken {
                jti: r.get(0)?,
                name: r.get(1)?,
                user_id: r.get(2)?,
                user_email: r.get(3)?,
                created_by: r.get(4)?,
                created_at_ms: r.get(5)?,
                expires_at_ms: r.get(6)?,
                revoked_at_ms: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn revoke_api_token(&mut self, jti: &str) -> Result<()> {
        let n = self.connection().execute(
            "UPDATE api_tokens SET revoked_at_ms = ?2 WHERE jti = ?1 AND revoked_at_ms IS NULL",
            params![jti, now_ms()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("live API token {jti}")));
        }
        Ok(())
    }

    /// The sign-in settings (an empty object before any save).
    pub fn auth_settings(&self) -> Result<Value> {
        let text: Option<String> = self
            .connection()
            .query_row("SELECT settings FROM auth_settings WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(match text {
            Some(t) => serde_json::from_str(&t)?,
            None => Value::Object(Default::default()),
        })
    }

    pub fn put_auth_settings(&mut self, settings: &Value) -> Result<()> {
        self.connection().execute(
            "INSERT INTO auth_settings (id, settings, updated_at_ms) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET settings = excluded.settings, updated_at_ms = excluded.updated_at_ms",
            params![settings.to_string(), now_ms()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new<'a>(id: &'a str, email: &'a str) -> NewUser<'a> {
        NewUser {
            id,
            email,
            name: "",
            role: "viewer",
            password_hash: Some("hash"),
            origin: "local",
        }
    }

    #[test]
    fn accounts_tokens_and_settings() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.user_count().unwrap(), 0);
        let u = db.create_user(&new("u1", "Ann@Example.org ")).unwrap();
        assert_eq!(u.email, "Ann@Example.org");
        assert!(u.has_password && u.active);
        // Emails are unique whatever their case.
        assert!(matches!(
            db.create_user(&new("u2", "ann@example.org")),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            db.password_hash("ANN@example.org")
                .unwrap()
                .unwrap()
                .1
                .as_deref(),
            Some("hash")
        );

        let u = db
            .update_user("u1", None, Some("admin"), Some(false))
            .unwrap();
        assert_eq!(u.role, "admin");
        assert!(!u.active);
        assert!(u.tokens_valid_from_ms > 0, "deactivating ends sessions");

        db.revoke_token("s1", now_ms() + 60_000).unwrap();
        assert!(db.token_revoked("s1").unwrap());
        db.add_api_token("t1", "feed script", "u1", "admin@x", now_ms() + 60_000)
            .unwrap();
        assert!(!db.token_revoked("t1").unwrap());
        db.revoke_api_token("t1").unwrap();
        assert!(db.token_revoked("t1").unwrap());
        assert_eq!(db.api_tokens().unwrap()[0].user_email, "Ann@Example.org");

        assert_eq!(db.auth_settings().unwrap(), serde_json::json!({}));
        db.put_auth_settings(&serde_json::json!({"saml": {"enabled": false}}))
            .unwrap();
        assert_eq!(db.auth_settings().unwrap()["saml"]["enabled"], false);

        db.delete_user("u1").unwrap();
        assert!(
            db.api_tokens().unwrap().is_empty(),
            "tokens go with the user"
        );
    }
}
