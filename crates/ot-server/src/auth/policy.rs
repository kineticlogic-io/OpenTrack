//! The role each API path needs, in one table: reading needs a viewer,
//! managing tracks a track manager, and every other change an admin (a
//! new route that writes is an admin's until it is listed here).

use axum::http::Method;

use super::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Anyone, signed in or not (the sign-in page's own calls).
    Public,
    /// Any signed-in account (its own profile and password).
    SignedIn,
    Role(Role),
}

/// Paths anyone may call.
const PUBLIC: &[&str] = &[
    "/auth/login",
    "/auth/logout",
    "/auth/public",
    "/auth/saml/login",
    "/auth/saml/acs",
    "/auth/saml/metadata",
    "/public/banner",
    "/public/warning-banner",
];

/// Paths any signed-in account may call, whatever its role.
const SIGNED_IN: &[&str] = &["/auth/me", "/auth/password"];

/// Reads that are an admin's: they show secrets (source credentials) or
/// accounts.
const ADMIN_READS: &[&str] = &["/auth/", "/export/config", "/probe"];

/// Changes a track manager may make: the picture and its curation.
const TRACK_MANAGEMENT: &[&str] = &[
    "/tracks/",
    "/groups",
    "/registry",
    "/correlation/suggestions/",
    "/decisions/",
    "/history/",
];

fn under(path: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        path == *p
            || path.starts_with(p)
                && (p.ends_with('/') || path.as_bytes().get(p.len()) == Some(&b'/'))
    })
}

pub fn need(method: &Method, path: &str) -> Need {
    let path = path.trim_end_matches('/');
    if PUBLIC.contains(&path) {
        return Need::Public;
    }
    if SIGNED_IN.contains(&path) {
        return Need::SignedIn;
    }
    if under(path, ADMIN_READS) {
        return Need::Role(Role::Admin);
    }
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Need::Role(Role::Viewer);
    }
    if under(path, TRACK_MANAGEMENT) {
        return Need::Role(Role::TrackManager);
    }
    Need::Role(Role::Admin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(m: Method, p: &str) -> Need {
        need(&m, p)
    }

    #[test]
    fn each_path_needs_its_role() {
        let (public, signed_in) = (Need::Public, Need::SignedIn);
        let viewer = Need::Role(Role::Viewer);
        let track_manager = Need::Role(Role::TrackManager);
        let admin = Need::Role(Role::Admin);
        assert_eq!(n(Method::POST, "/auth/login"), public);
        assert_eq!(n(Method::GET, "/public/banner"), public);
        assert_eq!(n(Method::GET, "/auth/me"), signed_in);
        assert_eq!(n(Method::POST, "/auth/password"), signed_in);
        assert_eq!(n(Method::GET, "/auth/users"), admin);
        assert_eq!(n(Method::GET, "/export/config"), admin);
        assert_eq!(n(Method::GET, "/tracks"), viewer);
        assert_eq!(n(Method::GET, "/sources/ais"), viewer);
        assert_eq!(n(Method::POST, "/tracks/pair"), track_manager);
        assert_eq!(n(Method::POST, "/tracks/OTK1/split"), track_manager);
        assert_eq!(n(Method::PUT, "/groups/3"), track_manager);
        assert_eq!(n(Method::POST, "/registry/import-sheet"), track_manager);
        assert_eq!(
            n(Method::POST, "/correlation/suggestions/4/accept"),
            track_manager
        );
        assert_eq!(n(Method::POST, "/decisions/9/undo"), track_manager);
        // Configuration is an admin's.
        assert_eq!(n(Method::PUT, "/correlation/settings"), admin);
        assert_eq!(n(Method::POST, "/sources"), admin);
        assert_eq!(n(Method::PUT, "/settings"), admin);
        assert_eq!(n(Method::POST, "/plugins"), admin);
        assert_eq!(n(Method::POST, "/admin/purge"), admin);
        // A prefix is a whole path segment: /groupsx is not /groups.
        assert_eq!(n(Method::POST, "/groupsx"), admin);
        assert_eq!(n(Method::POST, "/some/new/route"), admin);
    }
}
