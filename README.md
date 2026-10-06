# Spotifine

Spotify for Windows and macOS, without the bloat.

I built it because the official desktop app used more CPU than a music player
has any business using. On my liquid-cooled Ryzen 9 5950X it would push the
CPU to 80 °C, hotter than all-core benchmarks get it, and every time a song
started the fans spun up loud enough to hear through open-back headphones.

By now it covers the essentials of the official app, without the bloat,
and adds some quality-of-life improvements:

- Random shuffle
- Skip selected playlist tracks during normal playback
- Playback speed controls
- Cut out or loop parts of a song
- Open Spotify links directly in the app
- Play an artist’s discography continuously

<p align="center">
  <img src="docs/now-playing.png" alt="A playlist with a video Canvas in the Now Playing panel" width="900">
  <br>
  <sub>The background takes its color from the playing Canvas or album cover. Close the right panel for a compact, darker layout that keeps the adaptive background.</sub>
</p>

## Install

The [download page](https://eduardr02.github.io/spotifine/) picks the right
file for your computer.

You need a Spotify Premium account. On first launch the app opens the Spotify
login in your browser.

- **Windows:** download `Spotifine-windows-x64-setup.exe` from
  [Releases](../../releases) and run it.
- **macOS 14+ (Apple Silicon):** download `Spotifine-macos-arm64.zip` from
  [Releases](../../releases), extract `Spotifine.app` and open it. The app is
  ad-hoc signed, not notarized, so Gatekeeper may ask you to approve it. Only
  do that for builds you trust.

Settings checks for and installs updates.

## Not affiliated with Spotify

This is an unofficial client with no connection to Spotify AB. "Spotify" is
their trademark and is used here only to say what this connects to.

Running an unofficial client very likely breaks Spotify's Terms of Use. This one
authenticates as a desktop client and uses internal endpoints that aren't part of
the public Web API, so it can get your account suspended. It needs your own
Premium account and gives you nothing you aren't already paying for. Your call.
No audio, metadata or artwork ships with this repository.

## Features

Playlists, albums, artist pages, search, the queue, credits, radio, profiles and
audio podcasts. The pages only contain the music parts. An artist page has the
discography, popular tracks, bio, monthly listeners and top cities. No merch, no
concert tickets, no AI DJ, no home feed, no video podcasts or audiobooks.

The interface is frosted glass lit by whatever is playing: a still haze taken
from the song's Canvas or cover. It changes with the song and costs nothing
while the song plays.

Audio is 320 kbps and gapless. Media keys work when the app isn't focused, and
it shows up in Windows Quick Settings and on the lock screen. Played songs are
cached, so replaying them uses no network. When the system's audio output
changes, playback follows it. It can launch at login, minimized if you want.

Playlists can be created, renamed, deleted and reordered, and tracks added or
removed by drag and drop or in bulk by rules (artist, album, title, length),
with a preview of exactly which entries go. Playlists can be pinned to the top
of the library, and folders are kept.

Drag and drop songs to add to a playlist or reorder them.

Some extra things I added because we control playback here:

- Paste a shared Spotify link (song, album, artist, playlist, podcast, episode
  or profile) into search, and it opens here instead of the web player.
- Cut a section out of a song, or loop an exact range. Set per playlist, edited
  in a waveform view.
- Playback speed from 0.5× to 4×.
- Listening history, kept locally.
- A mark on songs that are already in the local audio cache.

Canvas, the short looping video some songs have, is an account setting on
Spotify's side. Spotifine shows it whenever it's on for your account.

### Likes, follows and devices

Without any setup, Spotifine can read your Liked Songs and the artists you
follow, but can't change them. To like songs, follow artists and people, see
your saved podcasts, and play on your other devices, connect a Spotify
developer app of your own. It's free, takes two minutes, and is a second
authorization for the same account, not another account.

1. Open the [Spotify developer dashboard](https://developer.spotify.com/dashboard),
   log in with your Premium account, and choose **Create app**.
2. Give it any name and description. Under **Redirect URIs**, add
   `http://127.0.0.1:5589/personal-api/callback`. Under the APIs you plan to
   use, tick **Web API**. Accept the terms and save.
3. Open the app's settings, copy its **Client ID** and paste it into
   Spotifine's Settings. Only the Client ID: never the Client Secret.
4. Choose **Connect**. Spotify asks you, in the browser, to allow access to
   your library and follows.
5. To play on other devices, choose **Allow** next to device access. Spotify
   asks once more, for permission to see and control your playback.

The player bar then shows a device button. Your queue stays in charge on the
other device, and that device's own controls work with it. Editing the queue
while it plays elsewhere can make it rebuffer for a moment. Other devices play
Spotify's original audio, so playback speed and song edits only apply on this
computer.

## Limitations

Windows and macOS 14 or later only, and you need Spotify Premium.

No lossless. Spotify has it and the official app plays it, but this client isn't
offered it: the track metadata we get back lists AAC 24 and Ogg Vorbis
96/160/320 and no FLAC. It's gated on presenting as a client they serve it to,
which would take far heavier reverse engineering than anything else here, if
it's reachable at all.

It doesn't matter much in practice. 320 kbps Vorbis and lossless aren't
something people reliably pick apart in a blind ABX test. There's a difference
on paper, but not at a level that affects listening.

## Building

You need [Rust](https://rustup.rs) and [Bun](https://bun.sh). Build on the OS
you intend to run. Cargo compiles every dependency from source, so the build
directory ends up several GB. On macOS, install the Xcode Command Line Tools
(`xcode-select --install`) and use macOS 14 or later.

```bash
bun install
bun tauri build
```

The installer lands in `target/release/bundle/nsis/` on Windows, and the app in
`target/release/bundle/macos/` on macOS. For development, build the playback
engine once with `bun run build:engine`, then run `bun tauri dev`. The checks
are `cargo test -p renderer-engine`, `cargo test -p spotifine`, `bun test` and
`bun run build`.

Publishing a GitHub release runs the [release workflow](../../actions/workflows/release.yml),
which builds, tests and signs both platforms and attaches the installers and
the update files.

Your login and caches stay on your computer, under `%LOCALAPPDATA%\Spotifine`
on Windows or `~/Library/Application Support/Spotifine` on macOS. The
developer-app authorization is kept in the system's credential store. Neither
sign-in gives this project your Spotify password.

## How it works

It has to be cheap to run while sitting open all day, so a few things follow
from that. While nothing plays, it does nothing: the sound device is closed and
no timers tick. The playhead is animated with a transform instead of a width,
so it doesn't force layout on every tick. Long lists are virtualized. The glass
doesn't re-blur the window as things move: the haze is rendered once per song
on the GPU, and the panes show a pre-frosted copy of it.

Two processes. `engine/` wraps [librespot](https://github.com/librespot-org/librespot)
and handles everything to do with sound. The Tauri shell in `src-tauri/`
supervises it, holds the caches, and serves a Svelte 5 frontend from `src/`.
Audio being in its own process means the interface can't interrupt playback, and
if the engine dies the shell restarts it and puts the queue back.

The audio path has its own resampler. Spotify decodes at 44.1 kHz and most
Windows devices run at 48 kHz, and the stock path came out about half a percent
slow and slightly flat. The replacement converts exactly, and tests pin both
the timing and the quality.

Tauri and a web frontend are an odd pick for this. I used them because the UI
needed the most iteration and HTML and CSS were much faster to work in.
Rewriting the frontend lower-level would be straightforward to hand to agents
now, but it's already light enough that I'd rather keep it easy to change.

| Path         | What's in it                                                |
| ------------ | ----------------------------------------------------------- |
| `engine/`    | Playback engine: librespot, audio pipeline, browse, history  |
| `src-tauri/` | Tauri shell: engine supervision, caches, commands            |
| `src/`       | Svelte 5 frontend                                            |
| `dev/`       | Development harnesses, not part of the build                 |

`AGENTS.md` has the conventions the code follows, and is a better starting point
than this file if you want to change something.

## On the code

Nearly all of it was written by AI agents. The decisions about what to build and
how weren't left to them, and plenty of it got thrown out and redone.

It's an easy codebase to extend. If Spotify is missing something you want, point
an agent at it.

## License

MIT, see [LICENSE](LICENSE). Covers the code here and nothing belonging to
Spotify AB.
