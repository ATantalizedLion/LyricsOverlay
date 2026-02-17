use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, HttpClientError,
    PkceCodeChallenge, RedirectUrl, RequestTokenError, Scope, StandardErrorResponse, TokenResponse,
    TokenUrl,
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::trace;
use url::Url;
use warp::Filter;

const SPOTIFY_AUTH_URL: &str = "https://accounts.spotify.com/authorize";
const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

type TokenError = RequestTokenError<
    HttpClientError<oauth2::reqwest::Error>,
    StandardErrorResponse<BasicErrorResponseType>,
>;

#[derive(Error, Debug)]
pub enum SpotifyClientError {
    // TODO: Cleanup error enum
    #[error("unknown spotify client error")]
    Unknown,
    #[error("Not authenticated")]
    NotAuthenticated,
    #[error("Missing code in: {url}")]
    MissingCodeAuthError { url: String },
    #[error("Missing state in: {url}")]
    MissingStateAuthError { url: String },
    #[error("CRSF token mismatch")]
    CrsfMismatch,
    #[error("Url Error")]
    UrlParse(#[from] url::ParseError),
    #[error("IO error")]
    IoError(#[from] std::io::Error), // RequestTokenError
    #[error("OAuth2 request token error")]
    OAuth2Error(String),
    #[error("OAuth token request failed: {0}")]
    TokenRequest(#[from] TokenError),
    #[error("OAuth token request failed: {0}")]
    ReqwestError(#[from] reqwest::Error),
}

#[derive(Debug, Deserialize)]
struct CurrentlyPlayingResponse {
    currently_playing_type: String,
    item: Option<Track>,
    is_playing: bool,
    progress_ms: usize,
}

#[derive(Debug, Deserialize)]
struct Track {
    name: String,
    id: String,
    duration_ms: usize,
    artists: Vec<Artist>,
}

#[derive(Debug, Deserialize)]
struct Artist {
    name: String,
}

pub struct SpotifyClient {
    access_token: Arc<Mutex<Option<String>>>,
    client: reqwest::Client,
}

impl SpotifyClient {
    pub fn new() -> Self {
        Self {
            access_token: Arc::new(Mutex::new(None)),
            client: reqwest::Client::new(),
        }
    }

    pub async fn authenticate(
        &mut self,
        client_id: &str,
        client_secret: &str,
        redirect: &str,
    ) -> Result<(), SpotifyClientError> {
        let client = BasicClient::new(ClientId::new(client_id.into()))
            .set_client_secret(ClientSecret::new(client_secret.into()))
            .set_auth_uri(AuthUrl::new(SPOTIFY_AUTH_URL.into())?)
            .set_token_uri(TokenUrl::new(SPOTIFY_TOKEN_URL.into())?)
            .set_redirect_uri(RedirectUrl::new(format!("{redirect}/callback"))?);

        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let (auth_url, csrf_token) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("user-read-currently-playing".to_string()))
            .add_scope(Scope::new("user-read-playback-state".to_string()))
            .set_pkce_challenge(pkce_challenge)
            .url();

        // Channels for callback and shutdown
        let (tx_content, rx_content) = oneshot::channel::<(String, String)>();
        let tx_content_mutex = Arc::new(StdMutex::new(Some(tx_content)));
        let (tx_shutdown, rx_shutdown) = oneshot::channel();
        let tx_shutdown_mutex = Arc::new(StdMutex::new(Some(tx_shutdown)));

        let callback_route = warp::path("callback")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .map(move |params: std::collections::HashMap<String, String>| {
            let code = params.get("code").cloned().unwrap();
            let state = params.get("state").cloned().unwrap();

            if let Some(tx_inner) = tx_content_mutex.lock().unwrap().take() {
                trace!("Sending code and state");
                tx_inner.send((code,state)).unwrap();
            }
            if let Some(tx_shutdown_inner) = tx_shutdown_mutex.lock().unwrap().take() {
                trace!("Sending shutdown!");
                tx_shutdown_inner.send(()).unwrap();
            }
            warp::reply::html(
                "<html><body><h1>Authentication successful!</h1><p>You can close this window.</p></body></html>"
            )
        });

        webbrowser::open(auth_url.as_str())?;

        trace!("Starting server");

        let url = Url::parse(redirect).expect("Invalid URL");
        let host = url.host_str().expect("Missing host");
        let port = url.port().expect("Missing port");
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .expect("Invalid socket address");

        let server = warp::serve(callback_route)
            .bind(addr)
            .await
            .graceful(async {
                // some signal in here, such as ctrl_c
                rx_shutdown.await.unwrap();
                trace!("Server shutdown received");
            })
            .run();

        trace!("Awaiting server");

        server.await;

        trace!("Awaiting rx_content");

        let (code, state) = rx_content.await.map_err(|_| SpotifyClientError::Unknown)?;

        trace!("rx_content awaited!");

        // Verify the CSRF token
        if state != *csrf_token.secret() {
            return Err(SpotifyClientError::CrsfMismatch);
        }

        let http_client = oauth2::reqwest::ClientBuilder::new()
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .build()
            .expect("Client should build");

        // Now you can trade it for an access token.
        let token_result = client
            .exchange_code(AuthorizationCode::new(code))
            // Set the PKCE code verifier.
            .set_pkce_verifier(pkce_verifier)
            .request_async(&http_client)
            .await?;

        let mut token_guard = self.access_token.lock().await;
        *token_guard = Some(token_result.access_token().secret().clone());

        trace!("Successfully authenticated!");

        Ok(())
    }

    pub async fn get_current_track(&self) -> Result<Option<(String, String)>, SpotifyClientError> {
        let token_opt = self.access_token.lock().await.clone();

        let Some(token) = token_opt else {
            return Err(SpotifyClientError::NotAuthenticated);
        };

        let response: reqwest::Response = self
            .client
            .get("https://api.spotify.com/v1/me/player/currently-playing")
            .bearer_auth(token)
            .send()
            .await?;

        if response.status().as_u16() == 204 {
            // No content - nothing playing
            return Ok(None);
        }

        let playing: CurrentlyPlayingResponse = response.json().await?;

        trace!("CurrentlyPlayingResponse {playing:?}");

        if !playing.is_playing || playing.currently_playing_type != "track" {
            return Ok(None);
        }

        if let Some(track) = playing.item {
            let artist = track
                .artists
                .first()
                .map_or_else(|| "Unknown Artist".to_string(), |a| a.name.clone());

            Ok(Some((track.name, artist)))
        } else {
            Ok(None)
        }
    }
}
