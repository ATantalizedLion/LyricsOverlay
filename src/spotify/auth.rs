//! Module for talking with spotify, implements only the parts of the API needed for this app
use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, HttpClientError,
    PkceCodeChallenge, RedirectUrl, RequestTokenError, Scope, StandardErrorResponse, TokenResponse,
    TokenUrl,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, info, trace};
use url::Url;
use warp::Filter;

use crate::settings::Settings;

//TODO: Add dialogue to home screen for when client and secret are not yet added
//TODO: deal with revoking of refresh tokens

const SPOTIFY_AUTH_URL: &str = "https://accounts.spotify.com/authorize";
const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

type TokenError = RequestTokenError<
    HttpClientError<oauth2::reqwest::Error>,
    StandardErrorResponse<BasicErrorResponseType>,
>;

#[derive(Error, Debug)]
/// Error enum for spotify authentication requests
pub enum SpotifyClientAuthError {
    #[error("Missing client id")]
    MissingClientId,
    #[error("Missing client secret")]
    MissingClientSecret,
    #[error("Missing code in auth callback URL")]
    MissingCodeAuthError,
    #[error("Missing state in auth callback URL")]
    MissingStateAuthError,
    #[error("Missing refresh token")]
    MissingRefreshToken,
    #[error("CRSF token mismatch")]
    CrsfMismatch,
    #[error("Url Error")]
    UrlParse(#[from] url::ParseError),
    #[error("IO error")]
    IoError(#[from] std::io::Error), // RequestTokenError
    #[error("OAuth token request failed: {0}")]
    TokenRequest(#[from] TokenError),
    #[error("Request failed: {0}")]
    ReqwestError(#[from] reqwest::Error),
}

/// Spotify client state
pub struct SpotifyAuthClient {
    /// Our very important amazing access token
    access_token: Arc<TokioRwLock<Option<String>>>,
    /// Settings!
    settings: Arc<TokioRwLock<Settings>>,
    refresh_token: Arc<TokioRwLock<Option<String>>>,
    token_expiry: Arc<TokioRwLock<Option<std::time::Instant>>>,
}

impl SpotifyAuthClient {
    pub fn new(settings: Arc<TokioRwLock<Settings>>) -> Self {
        Self {
            access_token: Arc::new(TokioRwLock::new(None)),
            settings,
            refresh_token: Arc::new(TokioRwLock::new(None)),
            token_expiry: Arc::new(TokioRwLock::new(None)),
        }
    }

    pub async fn authenticate(&mut self) -> Result<(), SpotifyClientAuthError> {
        let (
            client_id,
            client_secret,
            redirect,
            saved_refresh,
            stored_access_token,
            stored_expiry_time,
        ) = {
            let settings_lock = self.settings.read().await;
            (
                settings_lock.client_id.clone(),
                settings_lock.client_secret.clone(),
                settings_lock.redirect_url(),
                settings_lock.refresh_token.clone(),
                settings_lock.access_token.clone(),
                settings_lock.expiry_time_as_unix.clone(),
            )
        };

        if let Some(a_token) = stored_access_token
            && let Some(exp) = stored_expiry_time
        {
            if exp > get_unix_time() {
                info!(
                    "Using stored access token expiring in {} secs",
                    exp - get_unix_time()
                );
                let mut token_guard = self.access_token.write().await;
                *token_guard = Some(a_token);
                return Ok(());
            } else {
                debug!(
                    "Stored access token expired {} secs ago",
                    get_unix_time() - exp
                );
            }
        }

        if saved_refresh.clone().is_some_and(|x| !x.is_empty()) {
            let mut guard = self.refresh_token.write().await;
            *guard = saved_refresh;
            drop(guard);
            info!("Getting access token from stored refresh token",);
            return self.refresh_access_token().await;
        }

        if client_id.is_empty() {
            return Err(SpotifyClientAuthError::MissingClientId);
        }
        if client_secret.is_empty() {
            return Err(SpotifyClientAuthError::MissingClientSecret);
        }

        let client = BasicClient::new(ClientId::new(client_id))
            .set_client_secret(ClientSecret::new(client_secret))
            .set_auth_uri(AuthUrl::new(SPOTIFY_AUTH_URL.to_string())?)
            .set_token_uri(TokenUrl::new(SPOTIFY_TOKEN_URL.to_string())?)
            .set_redirect_uri(RedirectUrl::new(format!("{redirect}/callback"))?);

        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let (auth_url, csrf_token) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("user-read-currently-playing".to_string()))
            .add_scope(Scope::new("user-read-playback-state".to_string()))
            .set_pkce_challenge(pkce_challenge)
            .url();

        debug!("Opening browser");
        webbrowser::open(auth_url.as_str())?;

        // Spawn the warp server on a blocking thread with its own single-threaded runtime
        let url = Url::parse(&redirect).expect("Invalid URL");
        let host = url.host_str().expect("Missing host").to_owned();
        let port = url.port().expect("Missing port");

        let (code, state) = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (tx_content, rx_content) = oneshot::channel::<(Option<String>, Option<String>)>();
            let tx_content_mutex = Arc::new(Mutex::new(Some(tx_content)));
            let (tx_shutdown, rx_shutdown) = oneshot::channel();
            let tx_shutdown_mutex = Arc::new(Mutex::new(Some(tx_shutdown)));

            let callback_route = warp::path("callback")
                .and(warp::query::<std::collections::HashMap<String, String>>())
                .map(move |params: std::collections::HashMap<String, String>| {
                    let code = params.get("code").cloned();
                    let state = params.get("state").cloned();
                    if let Some(tx_inner) = tx_content_mutex.lock().unwrap().take() {
                        trace!("Sending code and state");
                        tx_inner.send((code, state)).unwrap();
                    }
                    if let Some(tx_shutdown_inner) = tx_shutdown_mutex.lock().unwrap().take() {
                        trace!("Sending shutdown!");
                        tx_shutdown_inner.send(()).unwrap();
                    }
                    warp::reply::html(
                        "<html><body><h1>Authentication successful!</h1><p>You can close this window.</p></body></html>".to_string()
                    )
                });

            let addr: SocketAddr = format!("{host}:{port}").parse().expect("Invalid socket address");
          warp::serve(callback_route)
            .bind(addr)
            .await
            .graceful(async move {
                rx_shutdown.await.unwrap();
                trace!("Server shutdown received");
            })
            .run()
            .await;

            rx_content.await.unwrap()
        })
    })
    .await
    .unwrap();

        let Some(code) = code else {
            return Err(SpotifyClientAuthError::MissingCodeAuthError);
        };
        let Some(state) = state else {
            return Err(SpotifyClientAuthError::MissingStateAuthError);
        };

        if state != *csrf_token.secret() {
            return Err(SpotifyClientAuthError::CrsfMismatch);
        }

        let http_client = oauth2::reqwest::ClientBuilder::new()
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .build()
            .expect("Client should build");

        let token_result = client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(pkce_verifier)
            .request_async(&http_client)
            .await?;

        self.process_token_result(token_result).await;

        debug!("Successfully authenticated!");
        Ok(())
    }

    pub async fn refresh_access_token(&self) -> Result<(), SpotifyClientAuthError> {
        let refresh_token = {
            let guard = self.refresh_token.read().await;
            guard
                .clone()
                .ok_or(SpotifyClientAuthError::MissingRefreshToken)?
        };

        let (client_id, client_secret) = {
            let s = self.settings.read().await;
            (s.client_id.clone(), s.client_secret.clone())
        };

        let client = BasicClient::new(ClientId::new(client_id))
            .set_client_secret(ClientSecret::new(client_secret))
            .set_auth_uri(AuthUrl::new(SPOTIFY_AUTH_URL.to_string())?)
            .set_token_uri(TokenUrl::new(SPOTIFY_TOKEN_URL.to_string())?);

        let http_client = oauth2::reqwest::ClientBuilder::new()
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .build()
            .expect("Client should build");

        let token_result = client
            .exchange_refresh_token(&oauth2::RefreshToken::new(refresh_token))
            .request_async(&http_client)
            .await?;

        self.process_token_result(token_result).await;

        Ok(())
    }

    pub async fn invalidate_token(&self) {
        let mut token_opt = self.access_token.write().await;
        *token_opt = None;
    }

    pub fn retreive_token_handle(&self) -> Arc<TokioRwLock<Option<String>>> {
        self.access_token.clone()
    }

    /// Process the token result,
    /// Grab the access token, refresh tokens, and store the expiry times
    pub async fn process_token_result(
        &self,
        token_result: oauth2::StandardTokenResponse<
            oauth2::EmptyExtraTokenFields,
            oauth2::basic::BasicTokenType,
        >,
    ) {
        let mut rw_settings = self.settings.write().await;

        let mut token_guard = self.access_token.write().await;
        *token_guard = Some(token_result.access_token().secret().clone());
        rw_settings.access_token = token_guard.clone();

        if let Some(new_refresh) = token_result.refresh_token() {
            let mut refresh_guard = self.refresh_token.write().await;
            *refresh_guard = Some(new_refresh.secret().clone());
            rw_settings.refresh_token = Some(new_refresh.secret().clone());
        }

        if let Some(duration) = token_result.expires_in() {
            let mut expiry_guard = self.token_expiry.write().await;
            *expiry_guard = Some(std::time::Instant::now() + duration);
            rw_settings.expiry_time_as_unix =
                Some(get_unix_time() + token_result.expires_in().unwrap().as_secs());
        }

        rw_settings.save().unwrap();
    }
}

fn get_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
