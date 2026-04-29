use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    handshake::{
        client::Request as ClientRequest,
        server::{ErrorResponse, Request as ServerRequest, Response},
    },
    http::{header::AUTHORIZATION, HeaderValue, StatusCode},
};

pub const ROLE_HEADER: &str = "x-agent-role";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthRole {
    Worker,
    Client,
}

impl AuthRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Client => "client",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    worker_token: Option<String>,
    client_token: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum AuthFailure {
    MissingRole,
    InvalidRole,
    MissingAuthorization,
    InvalidAuthorization,
    RoleDisabled,
    InvalidToken,
}

impl AuthConfig {
    pub fn from_files(
        shared_token_file: Option<&PathBuf>,
        worker_token_file: Option<&PathBuf>,
        client_token_file: Option<&PathBuf>,
    ) -> Result<Self> {
        let shared_token = read_optional_token(shared_token_file)?;
        let worker_token = read_optional_token(worker_token_file)?.or_else(|| shared_token.clone());
        let client_token = read_optional_token(client_token_file)?.or(shared_token);

        Ok(Self {
            worker_token,
            client_token,
        })
    }

    pub fn enabled(&self) -> bool {
        self.worker_token.is_some() || self.client_token.is_some()
    }

    pub fn token_for_role(&self, role: AuthRole) -> Option<&str> {
        match role {
            AuthRole::Worker => self.worker_token.as_deref(),
            AuthRole::Client => self.client_token.as_deref(),
        }
    }
}

pub fn websocket_request(
    coordinator: &str,
    role: AuthRole,
    token: Option<&str>,
) -> Result<ClientRequest> {
    let mut request = coordinator.into_client_request()?;
    request
        .headers_mut()
        .insert(ROLE_HEADER, HeaderValue::from_static(role.as_str()));
    if let Some(token) = token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("token contains characters that are invalid in an HTTP header")?;
        request.headers_mut().insert(AUTHORIZATION, value);
    }
    Ok(request)
}

pub fn authenticate_request(
    request: &ServerRequest,
    auth: &AuthConfig,
) -> std::result::Result<AuthRole, AuthFailure> {
    let role = request
        .headers()
        .get(ROLE_HEADER)
        .ok_or(AuthFailure::MissingRole)
        .and_then(parse_role)?;
    let authorization = request
        .headers()
        .get(AUTHORIZATION)
        .ok_or(AuthFailure::MissingAuthorization)
        .and_then(parse_bearer_token)?;
    let expected = auth.token_for_role(role).ok_or(AuthFailure::RoleDisabled)?;

    if token_matches(authorization, expected) {
        Ok(role)
    } else {
        Err(AuthFailure::InvalidToken)
    }
}

pub fn unauthorized_response(_failure: AuthFailure) -> ErrorResponse {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(Some("unauthorized\n".to_owned()))
        .expect("static unauthorized response should build")
}

pub fn read_optional_token(path: Option<&PathBuf>) -> Result<Option<String>> {
    path.map(read_token_file).transpose()
}

pub fn read_token_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let token = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read token file {}", path.display()))?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(anyhow!("token file {} is empty", path.display()));
    }
    Ok(token)
}

fn parse_role(value: &HeaderValue) -> std::result::Result<AuthRole, AuthFailure> {
    match value.to_str().map_err(|_| AuthFailure::InvalidRole)? {
        "worker" => Ok(AuthRole::Worker),
        "client" => Ok(AuthRole::Client),
        _ => Err(AuthFailure::InvalidRole),
    }
}

fn parse_bearer_token(value: &HeaderValue) -> std::result::Result<&str, AuthFailure> {
    let value = value
        .to_str()
        .map_err(|_| AuthFailure::InvalidAuthorization)?;
    let Some((scheme, token)) = value.split_once(' ') else {
        return Err(AuthFailure::InvalidAuthorization);
    };
    if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
        return Err(AuthFailure::InvalidAuthorization);
    }
    Ok(token)
}

fn token_matches(provided: &str, expected: &str) -> bool {
    let provided = provided.as_bytes();
    let expected = expected.as_bytes();
    let mut diff = provided.len() ^ expected.len();
    for index in 0..provided.len().max(expected.len()) {
        let left = provided.get(index).copied().unwrap_or_default();
        let right = expected.get(index).copied().unwrap_or_default();
        diff |= usize::from(left ^ right);
    }
    diff == 0
}
