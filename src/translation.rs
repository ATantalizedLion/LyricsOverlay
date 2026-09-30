//! Automatic translation of lyrics into the user's chosen language. The source language
//! is detected locally first, so songs in languages the user doesn't want translated
//! never get sent anywhere; the rest go to Google Translate's free (keyless) endpoint,
//! and results are cached next to the lyrics themselves.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, trace, warn};

use crate::{
    MessageToUI, paths,
    runtime::{Messages, RuntimeError},
    settings::Settings,
};

const GOOGLE_TRANSLATE_URL: &str = "https://translate.googleapis.com/translate_a/single";
/// Lines are batched into requests of at most this many characters, well below where
/// the endpoint starts rejecting requests.
const MAX_CHUNK_CHARS: usize = 3000;

pub struct Language {
    /// Code as understood by Google Translate (mostly ISO 639-1)
    pub code: &'static str,
    pub name: &'static str,
    /// What local detection reports for this language, if it can detect it at all
    detect: Option<whatlang::Lang>,
}

const fn lang(code: &'static str, name: &'static str, detect: whatlang::Lang) -> Language {
    Language {
        code,
        name,
        detect: Some(detect),
    }
}

/// Languages offered as translation target and in the language filter.
pub const LANGUAGES: &[Language] = {
    use whatlang::Lang;
    &[
        lang("en", "English", Lang::Eng),
        lang("ja", "Japanese", Lang::Jpn),
        lang("ko", "Korean", Lang::Kor),
        lang("zh-CN", "Chinese", Lang::Cmn),
        lang("nl", "Dutch", Lang::Nld),
        lang("de", "German", Lang::Deu),
        lang("fr", "French", Lang::Fra),
        lang("es", "Spanish", Lang::Spa),
        lang("pt", "Portuguese", Lang::Por),
        lang("it", "Italian", Lang::Ita),
        lang("sv", "Swedish", Lang::Swe),
        lang("no", "Norwegian", Lang::Nob),
        lang("da", "Danish", Lang::Dan),
        lang("fi", "Finnish", Lang::Fin),
        lang("pl", "Polish", Lang::Pol),
        lang("cs", "Czech", Lang::Ces),
        lang("hu", "Hungarian", Lang::Hun),
        lang("ro", "Romanian", Lang::Ron),
        lang("el", "Greek", Lang::Ell),
        lang("tr", "Turkish", Lang::Tur),
        lang("ru", "Russian", Lang::Rus),
        lang("uk", "Ukrainian", Lang::Ukr),
        lang("ar", "Arabic", Lang::Ara),
        lang("he", "Hebrew", Lang::Heb),
        lang("hi", "Hindi", Lang::Hin),
        lang("th", "Thai", Lang::Tha),
        lang("vi", "Vietnamese", Lang::Vie),
        lang("id", "Indonesian", Lang::Ind),
        lang("tl", "Tagalog", Lang::Tgl),
        lang("af", "Afrikaans", Lang::Afr),
        lang("la", "Latin", Lang::Lat),
    ]
};

/// Display name for a language code, falling back to the code itself.
pub fn language_name(code: &str) -> &str {
    let primary = primary_language(code);
    LANGUAGES
        .iter()
        .find(|l| primary_language(l.code) == primary)
        .map_or(code, |l| l.name)
}

/// Normalizes a language code down to its primary subtag, so e.g. `zh-CN` and `zh-TW`
/// compare equal, and Google's legacy codes match the ones we use.
pub fn primary_language(code: &str) -> String {
    let primary = code.split(['-', '_']).next().unwrap_or(code).to_lowercase();
    match primary.as_str() {
        "iw" => "he".into(),
        "jw" => "jv".into(),
        "nb" | "nn" => "no".into(),
        "fil" => "tl".into(),
        _ => primary,
    }
}

/// Best-effort local guess at the language of `text`. `None` when unsure, in which case
/// the translation service's own detection gets the final say.
pub fn detect_language(text: &str) -> Option<&'static str> {
    // Kana only occurs in Japanese, while kanji-heavy lyrics can otherwise read as Chinese.
    if text.chars().any(|c| matches!(c, '\u{3040}'..='\u{30FF}')) {
        return Some("ja");
    }
    let info = whatlang::detect(text)?;
    if !info.is_reliable() {
        return None;
    }
    LANGUAGES
        .iter()
        .find(|l| l.detect == Some(info.lang()))
        .map(|l| l.code)
}

#[derive(Debug, Clone)]
pub struct TranslationRequest {
    /// Identifies the song (and its cache folder), see `LyricsRequestInfo::get_track_identifier`
    pub track_identifier: String,
    /// Lyric lines, in display order
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Translation {
    /// Language the lyrics were detected to be in
    pub source_lang: String,
    pub target_lang: String,
    /// Translation per lyric line, parallel to the lines that were requested. Empty for
    /// lines that didn't need translating (blank, or already in the target language).
    pub lines: Vec<String>,
}

#[derive(Error, Debug)]
pub enum TranslationError {
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Unexpected response from translation service")]
    UnexpectedResponse,
    #[error("Translation returned {got} lines, expected {expected}")]
    LineCountMismatch { expected: usize, got: usize },
}

pub struct Translator {
    client: reqwest::Client,
    settings: Arc<TokioRwLock<Settings>>,
}

impl Translator {
    pub fn new(settings: Arc<TokioRwLock<Settings>>) -> Self {
        Self {
            client: reqwest::Client::new(),
            settings,
        }
    }

    pub async fn translate(&self, req: TranslationRequest) -> Result<Messages, RuntimeError> {
        let settings = self.settings.read().await.clone();
        if !settings.translation_enabled || req.lines.iter().all(|l| l.trim().is_empty()) {
            return Ok(Messages::none());
        }
        let target = settings.translation_target.clone();

        if let Some(detected) = detect_language(&req.lines.join("\n"))
            && !settings.should_translate_from(detected)
        {
            debug!("Not translating lyrics detected as '{detected}'");
            return Ok(Messages::none());
        }

        let cache_file = settings.caching_enabled.then(|| {
            paths::resolve(&settings.cache_folder)
                .join(&req.track_identifier)
                .join(format!("translation.{target}.json"))
        });

        let cached = cache_file.as_deref().and_then(read_cached);
        let translation = if let Some(cached) = cached {
            trace!("Using cached translation for {}", req.track_identifier);
            cached
        } else {
            let translation = self.request_google(&req.lines, &target).await?;
            if let Some(path) = &cache_file {
                store_cached(path, &translation);
            }
            translation
        };

        // Local detection can be unsure (or wrong on short lyrics), so check once more
        // against what the translation service itself detected.
        if !settings.should_translate_from(&translation.source_lang) {
            debug!(
                "Not showing translation from '{}' as it's filtered out",
                translation.source_lang
            );
            return Ok(Messages::none());
        }

        Ok(Messages::to_ui(MessageToUI::GotTranslation {
            track_identifier: req.track_identifier,
            translation,
        }))
    }

    async fn request_google(
        &self,
        lines: &[String],
        target: &str,
    ) -> Result<Translation, TranslationError> {
        // Blank lines are left out entirely, as the service collapses them and that would
        // throw off mapping the translated lines back onto the originals.
        let non_blank: Vec<usize> = (0..lines.len())
            .filter(|&i| !lines[i].trim().is_empty())
            .collect();

        let mut translated = vec![String::new(); lines.len()];
        // Characters seen per detected source language, to settle on the dominant one
        let mut source_chars: HashMap<String, usize> = HashMap::new();

        for chunk in chunk_lines(&non_blank, lines) {
            let text = chunk
                .iter()
                .map(|&i| lines[i].trim())
                .collect::<Vec<_>>()
                .join("\n");
            let (result, source) = self.google_request(&text, target).await?;

            let result_lines: Vec<&str> = result.trim_end_matches('\n').split('\n').collect();
            if result_lines.len() != chunk.len() {
                return Err(TranslationError::LineCountMismatch {
                    expected: chunk.len(),
                    got: result_lines.len(),
                });
            }
            for (&i, line) in chunk.iter().zip(result_lines) {
                let line = line.trim();
                // e.g. an English chorus in an otherwise Japanese song: nothing to show
                if !line.eq_ignore_ascii_case(lines[i].trim()) {
                    line.clone_into(&mut translated[i]);
                }
            }
            *source_chars.entry(source).or_default() += text.chars().count();
        }

        let source_lang = source_chars
            .into_iter()
            .max_by_key(|(_, chars)| *chars)
            .map(|(lang, _)| lang)
            .unwrap_or_default();

        Ok(Translation {
            source_lang,
            target_lang: target.to_owned(),
            lines: translated,
        })
    }

    /// Translates `text`, returning the translation and the detected source language.
    async fn google_request(
        &self,
        text: &str,
        target: &str,
    ) -> Result<(String, String), TranslationError> {
        let mut url = reqwest::Url::parse(GOOGLE_TRANSLATE_URL).expect("valid url");
        url.query_pairs_mut()
            .append_pair("client", "gtx")
            .append_pair("sl", "auto")
            .append_pair("tl", target)
            .append_pair("dt", "t");
        // Sent as a form body rather than in the query, since lyrics easily get too long
        // for a URL once percent-encoded.
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", text)
            .finish();

        let response = self
            .client
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded;charset=utf-8",
            )
            .body(body)
            .send()
            .await?
            .error_for_status()?;
        let json: serde_json::Value = response.json().await?;
        trace!("Translation response: {json}");

        parse_google_response(&json).ok_or(TranslationError::UnexpectedResponse)
    }
}

/// The response looks like `[[["translated", "original", ...], ...], null, "ja", ...]`,
/// with the text split into sentence-ish segments.
fn parse_google_response(json: &serde_json::Value) -> Option<(String, String)> {
    let segments = json.get(0)?.as_array()?;
    let translated: String = segments
        .iter()
        .filter_map(|seg| seg.get(0)?.as_str())
        .collect();
    let source = json.get(2)?.as_str()?.to_owned();
    Some((translated, source))
}

/// Groups the `indices` of `lines` into consecutive batches of at most
/// `MAX_CHUNK_CHARS` characters (a single longer line gets a batch of its own).
fn chunk_lines(indices: &[usize], lines: &[String]) -> Vec<Vec<usize>> {
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_chars = 0;
    for &i in indices {
        let chars = lines[i].trim().chars().count() + 1;
        if !current.is_empty() && current_chars + chars > MAX_CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(i);
        current_chars += chars;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn read_cached(path: &Path) -> Option<Translation> {
    let file = fs::File::open(path).ok()?;
    match serde_json::from_reader(file) {
        Ok(translation) => Some(translation),
        Err(e) => {
            warn!("Ignoring unreadable cached translation {path:?}: {e}");
            None
        }
    }
}

fn store_cached(path: &PathBuf, translation: &Translation) {
    let res = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .map_err(|e| e.to_string())
        .and_then(|()| serde_json::to_string_pretty(translation).map_err(|e| e.to_string()))
        .and_then(|json| fs::write(path, json).map_err(|e| e.to_string()));
    if let Err(e) = res {
        warn!("Failed caching translation at {path:?}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_japanese_by_kana() {
        assert_eq!(detect_language("今日はいい天気ですね\n散歩に行こう"), Some("ja"));
    }

    #[test]
    fn detects_dutch_and_german() {
        assert_eq!(
            detect_language(
                "Het regent vandaag de hele dag en ik blijf lekker binnen zitten
                 Maar morgen gaan we samen naar het strand
                 De zon schijnt en de vogels zingen"
            ),
            Some("nl")
        );
        assert_eq!(
            detect_language(
                "Heute regnet es den ganzen Tag und ich bleibe lieber zu Hause
                 Aber morgen fahren wir zusammen an den Strand
                 Die Sonne scheint und die Vögel singen"
            ),
            Some("de")
        );
    }

    #[test]
    fn normalizes_language_codes() {
        assert_eq!(primary_language("zh-CN"), "zh");
        assert_eq!(primary_language("iw"), "he");
        assert_eq!(language_name("zh-TW"), "Chinese");
        assert_eq!(language_name("xx"), "xx");
    }

    #[test]
    fn parses_google_response() {
        let json: serde_json::Value = serde_json::from_str(
            r#"[[["Good morning.\n","おはよう。\n",null,null,10],["See you later","またね",null,null,10]],null,"ja"]"#,
        )
        .unwrap();
        let (text, source) = parse_google_response(&json).unwrap();
        assert_eq!(text, "Good morning.\nSee you later");
        assert_eq!(source, "ja");
    }

    #[test]
    fn chunks_respect_limit() {
        let lines: Vec<String> = (0..10).map(|_| "a".repeat(1000)).collect();
        let indices: Vec<usize> = (0..10).collect();
        let chunks = chunk_lines(&indices, &lines);
        assert!(chunks.iter().all(|c| c.len() <= 2));
        assert_eq!(chunks.concat(), indices);
    }
}
