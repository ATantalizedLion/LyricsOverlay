use std::sync::{Arc, RwLock};

use crate::{
    MessageToRT, MessageToUI,
    runtime::{Messages, RuntimeError},
    settings::Settings,
    spotify::{CurrentlyPlayingResponse, SpotifyClientTrackError},
};
use tokio::sync::mpsc;

use super::SpotifyClient;

pub struct SpotifyPoller {
    client: Arc<SpotifyClient>,
    settings: Arc<RwLock<Settings>>,
}

impl SpotifyPoller {
    pub fn new(client: Arc<SpotifyClient>, settings: Arc<RwLock<Settings>>) -> Self {
        Self { client, settings }
    }

    pub async fn run(self, tx_rt: mpsc::Sender<MessageToRT>, tx_ui: mpsc::Sender<MessageToUI>) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(
            self.settings.read().unwrap().poll_interval_ms,
        ));
        loop {
            interval.tick().await;
            let res = self.poll().await;
            match res {
                Ok(msg) => {
                    msg.send(tx_ui.clone(), tx_rt.clone()).await;
                }
                Err(x) => {
                    tx_ui
                        .clone()
                        .send(MessageToUI::DisplayError(format!("{x:?}")))
                        .await
                        .unwrap();
                }
            };
        }
    }

    pub async fn poll(&self) -> Result<Messages, RuntimeError> {
        process_current_track_response(self.client.get_current_track().await).await
    }
}

pub async fn process_current_track_response(
    res: Result<CurrentlyPlayingResponse, SpotifyClientTrackError>,
) -> Result<Messages, RuntimeError> {
    match res {
        Ok(song) => Ok(Messages::to_ui(MessageToUI::CurrentlyPlaying(song))),
        Err(err) => match err {
            SpotifyClientTrackError::NotATrack => Ok(Messages::to_ui(
                MessageToUI::NotCurrentlyPlaying("Not playing a song".to_owned()),
            )),
            SpotifyClientTrackError::NoContentResponse => Ok(Messages::to_ui(
                MessageToUI::NotCurrentlyPlaying("Not playing anything".to_owned()),
            )),
            SpotifyClientTrackError::ReqwestError(error) => Ok(Messages::to_ui(
                MessageToUI::NotCurrentlyPlaying(format!("anything: {error}").to_owned()),
            )),
            SpotifyClientTrackError::NotAuthenticated | SpotifyClientTrackError::TokenError => Ok(
                Messages::to_ui(MessageToUI::AuthenticationStateUpdate(false)),
            ),
            SpotifyClientTrackError::BadRequest => todo!(),
            SpotifyClientTrackError::RateLimitsExceeded => todo!(),
        },
    }
}
