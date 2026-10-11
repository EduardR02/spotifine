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
The panel holds its previous picture while the next Canvas loads, including
after closing and reopening it. With no previous picture, it waits for the
video/artwork decision instead of flashing a temporary cover. Missing Canvas
or a load timeout falls back to artwork. Videos pause in cover mode, with the
panel closed, or while the window is hidden or unfocused.

Drag the left edge of the now-playing panel to resize it. The width is
remembered; double-click the edge to reset it. As the playlist narrows, album
and then date-added disappear, while title, artist and duration stay visible.
The panel leaves at least 400px for the main pane and never grows wider than
the Canvas can fill at the window's height and display resolution.
A drag interrupted by closing the panel or shrinking the window cancels without
changing your saved width. Space activates a focused button; on a noninteractive
surface it toggles playback.

Right-click the player bar to show or hide its speed controls, mute button,
cover artwork, title and artist, or library checkmark/heart. These choices are
remembered. Hiding a control does not change playback speed, volume or likes.

### Likes, follows and devices

Without any setup, Spotifine can read your Liked Songs, the artists you follow
and your saved podcasts, but can't change them. To like songs, follow artists
and people, and play on your other devices, connect a Spotify developer app of
your own. It's free, takes two minutes, and is a second
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
engine once with `bun run build:engine`, then run `bun tauri dev`.
Run native tests serially: audio and cache epochs are process-global.

```bash
cargo test -p renderer-engine --lib --bin PlaybackEngine -- --test-threads=1
cargo test -p librespot-audio -- --test-threads=1
cargo test -p spotifine -- --test-threads=1
```

Frontend checks are `bun test` and `bun run build`. For an account-free engine
smoke, build the normal executable with `cargo build -p renderer-engine --bin
PlaybackEngine`, then launch it with `--state-dir` pointing to a new absolute
temporary directory, never your app data. Its JSON-lines stdin accepts `status`
and `clear_audio_cache` requests with a nonempty `request_id`; stdout reports
the correlated responses. Verify cached audio is absent when a successful clear
reply arrives, unrelated files survive, and closing stdin exits cleanly.

Publishing a GitHub release runs the [release workflow](../../actions/workflows/release.yml),
which builds, tests and signs both platforms and attaches the installers and
the update files.

Your login and caches stay on your computer, under `%LOCALAPPDATA%\Spotifine`
on Windows or `~/Library/Application Support/Spotifine` on macOS. The
developer-app authorization is kept in the system's credential store. Neither
sign-in gives this project your Spotify password.

### Profiling on Windows

With the app running, measure its own process tree without administrator access:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File dev/profile-app.ps1 -Seconds 60
```

The helper integrates CPU time and CPU cycles across the interval and samples
working set, private commit and I/O for the shell, audio engine and its WebView2
subprocesses. It excludes other apps' WebView2 processes. `CpuMsPerSecond` makes
small sustained work visible even when Task Manager rounds usage to zero;
`CpuPercentMachine` is normalized across all logical processors.

Capture playing, paused, blurred and minimized states separately. Keep window
size, panel width, cover/Canvas mode and interaction workload consistent; passive
captures while using the app normally are not comparable benchmarks. Window
physical bounds, DPI and minimized/foreground state are recorded at the start
and end, not continuously. Add `-Json` for machine-readable results, or
`-ProcessId` when more than one app copy is running. The tree is captured at the start, so
rerun after an engine restart or a new WebView subprocess. Summed working sets
count shared pages more than once; private commit is not resident RAM, and I/O
includes pipes/network rather than only disk. These counters do not measure power.

For sampled call stacks and thread wakeups, use Windows Performance Recorder
from an elevated terminal, then inspect the trace in
[Windows Performance Analyzer](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/windows-performance-analyzer):

```powershell
wpr -start CPU
# Exercise one workload for about a minute.
wpr -stop "$env:TEMP\Spotifine-cpu.etl"
```

WPR uses a bounded in-memory recording here. Keep traces private: they contain
system-wide process/activity data. Native function-level analysis also needs
matching symbols for the executable being measured.

## How it works

The playhead uses a transform instead of changing width on every tick. The haze
is rendered on source or layout changes, and the panes show a pre-frosted copy
instead of continuously re-blurring the window.

The library rail, Home grid and discography track lists render a viewport window
with a small buffer, retaining rows needed for focus and active interactions.
Loaded collections and tracks remain available in memory: bounded rendering is
not a cap on resident library data.

Track drags and seek gestures stay bound to their source collection or playback
target; navigation and track changes cannot redirect an old gesture. Pending
playlist edits reconcile against the latest snapshot, and newer account/library
data takes precedence over stale replies and optimistic rollbacks.

Playing-row meters are decorative, not audio measurements. Visible meters share
one nominal 60 Hz clock while playback runs in a focused, visible window. Timer
delays can skip updates rather than trigger catch-up bursts. Pausing, hiding a
meter or removing its row stops its updates; reduced motion keeps the bars static.
Each meter draws four cached bar sprites into an independent OffscreenCanvas,
using curve samples computed once at module load. This avoids per-frame DOM
style updates, but browser compositing and presentation still do work.

On Windows, native minimize state pauses Canvas playback and decorative meters,
even if the WebView still reports a focused, visible document. The same visibility
gate defers new cover and haze work. Music continues while minimized; restoring
the window lets its visual work resume.

Two processes. `engine/` wraps [librespot](https://github.com/librespot-org/librespot)
and handles everything to do with sound. The Tauri shell in `src-tauri/`
supervises it, holds the caches, and serves a Svelte 5 frontend from `src/`.
Audio being in its own process means the interface can't interrupt playback, and
if the engine dies the shell restarts it and puts the queue back.
Even an engine that exits during startup goes through restart backoff; shutdown
cancels pending restart waits.
Recovery settles against the replacement engine's actual state before publishing
playback, including a crash during reconnection. Playlist edits fence older
pending reads, and local library activity is written atomically. Library,
playlist-detail and membership caches are scoped to the authenticated account:
disconnecting retains that account's offline data without exposing it to another
account. A one-time migration preserves older listening/activity data only when
its recorded owner matches; unowned detail and membership caches are not imported.
New download marks arrive when a complete song file is saved, or playback opens
an existing cached file, without periodic polling or another Spotify request.
Playback supplies the requested song and selected file IDs, including regional
substitutes and restored queues that have not been browsed. Marks reflect files
that still exist, and removals also clear marks on songs outside the current queue.
Clearing audio stops its producers and fences pending downloads before deleting
files; success is acknowledged only after deletion. It resets playback and
download marks, but preserves credentials and the audio-cache layout marker.
A deletion failure is reported as a failure, not a successful reset.
Cache measurements are fenced through the clear's actual completion, including
cancelled callers and deletion lasting beyond the ordinary command deadline.
An older scan cannot overwrite or return a superseded cache measurement.

The audio path has its own resampler. Spotify decodes at 44.1 kHz and most
Windows devices run at 48 kHz, and librespot's stock conversion comes out about
half a percent slow and slightly flat. This one converts exactly, and tests pin
both the timing and the quality.
Load and seek configuration is committed by the decoder thread; each PCM packet
carries its owning revision and exact source frame. Codec preroll and trimming
keep cuts and loops aligned after a seek. An empty output callback zero-fills the rest of its block after one ring probe.

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
