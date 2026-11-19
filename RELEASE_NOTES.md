## Lighter playback

- Playback at 48 kHz takes less CPU on PCs with AVX2, with exactly the same sound, including seeks, loops and gapless transitions.
- Waveforms in the track editor draw with less CPU.
- A podcast opens at once, before its full episode list has loaded.
- Playing another song from the same playlist starts without sending the whole queue again.

## Lighter glass

- The glass is cheap to run: its edge light is drawn once per song, scrolling runs on the GPU, and a song change only touches what it changes.
- The volume level is a rod of tinted glass that takes the playing record's colour.
- The download page has a live demo of the app.

## Glass, polished

- The glass has real edges: every pane catches light along its rim and glows faintly in the playing record's colour just inside it.
- The row under the pointer lights up where the pointer is, and the selected playlist in the library takes the colour of what's playing.
- Find inside a playlist: the magnifier next to a playlist's "…" narrows its songs by name and artist as you type.
- The search page shows what you last opened from search, with covers, instead of an empty page.
- Search fields, the discography selector, dropdowns and the volume slider are glass like everything else. The accent colour is left for what's playing.
- The play controls sit a little lower in the player bar, and the seek line brightens toward the current position.
- Credits and Up next over a Canvas keep more of the video's colour.

## Explore without waiting

- A song asks Spotify for its key once. Replays never ask again, so clicking through music doesn't run into Spotify's limit on keys.
- If Spotify makes a new song wait, it stays selected and starts by itself, usually within half a minute. Nothing is skipped or removed from the cache.
- Clicking past songs quickly, or seeking while one loads, costs nothing extra.
- The volume bar keeps its full length with the Now playing panel open, as long as the window has room.
- Searching for local devices while receivers are asleep or offline shows an empty list, not a timeout error.

## Spotify Connect fixes

- Fixed connecting to local Spotify Connect receivers without first activating them in Spotify’s app, and fixed playback failing with “Spotify returned an invalid player response” after switching devices.
