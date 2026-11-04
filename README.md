# LyricsOverlay

I've always wanted a minimalist, transparent window that showed the lyrics of the currently playing song. 
I wasn't satisfied with any currently existing apps I found after a brief search a couple of years ago. 
No better excuse to make your own and learn some new things in the process. 
Enjoy! 

## Getting started
Download `LyricsOverlay.exe` from the [latest release](https://github.com/ATantalizedLion/LyricsOverlay/releases/latest) and run it - no installation needed. 
It works with whatever is playing on Windows (Spotify, browsers, other media players) out of the box, no setup required.

The exe isn't code signed, so on first run Windows SmartScreen may warn that it "protected your PC". Click **More info → Run anyway** to start it. 
Each release also comes with a `.sha256` checksum file if you want to verify your download.

If you want to compile it yourself, all you need is the rust toolchain and `cargo run`.

### Spotify (optional)
Connecting Spotify adds Spotify's own lyrics as an extra source, and lets the overlay follow Spotify Connect playback on other devices. 
Create an app in the [Spotify developer dashboard](https://developer.spotify.com/dashboard), then add its client id and secret in the settings. After pressing connect, your default browser will open to allow this app to access your currently playing information.

### Translation
Lyrics can be translated automatically into a language of your choice (Settings → Translation), either shown below each line or replacing it. 
You can choose which languages get translated, e.g. translate Japanese but leave Dutch and German as is.

### Syllable timing (optional)
If a song's lyrics come with word or syllable timing (Enhanced LRC), the current line fills in as it's sung. 
For songs you have as audio files, the alignment tool in [`tools/align`](tools/align/README.md) can add syllable timing to lyrics the overlay has already cached, using vocal separation and a forced-alignment model. It's a separate Python tool, since the models are several GB. 
With Settings → Record Spotify for syllable timing on, songs you play from start to end in the Spotify app are recorded for the tool to process with `--pending`.

## Where files are stored
Config, cached lyrics, logs and custom themes live in `%APPDATA%\LyricsOverlay` (Settings → Advanced → Data folder → Open). 
To make your own theme, copy `themes\example.toml.sample` in there to a new `.toml` file and edit the colors.

## License
[MIT](LICENSE)
