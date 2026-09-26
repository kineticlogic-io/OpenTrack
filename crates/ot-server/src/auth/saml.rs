//! SAML 2.0 single sign-on (filled in below).

use axum::Router;

use crate::control::AppState;

#[derive(Default)]
pub struct State;

pub fn routes() -> Router<AppState> {
    Router::new()
}
