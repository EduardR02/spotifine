<!--
  Every icon defined once as an SVG <symbol>; components reference them
  with <use href="#i-name">. Rendered once at the app root. Inlining the
  paths per instance would multiply DOM nodes across a virtualized list
  of hundreds of rows, each carrying three or four icons.
-->

<!-- Taken out of flow AND zero-sized AND overflow-clipped: the sprite host
     itself can never contribute a pixel of layout, whatever it contains. -->
<svg
  width="0"
  height="0"
  style="position:absolute;left:0;top:0;width:0;height:0;overflow:hidden"
  aria-hidden="true"
><defs>
<symbol id="i-search" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><circle cx="10.6" cy="10.6" r="6.4"/><path d="m15.6 15.6 5 5"/></symbol>
<symbol id="i-queue" viewBox="0 0 24 24" fill="currentColor"><rect x="3.4" y="6.2" width="11" height="1.9" rx=".9"/><rect x="3.4" y="11.1" width="8" height="1.9" rx=".9"/><rect x="3.4" y="16" width="5" height="1.9" rx=".9"/><path d="M16.8 9.3v5.4c0 .6.6 1 1.2.7l4.7-2.7a.8.8 0 0 0 0-1.4L18 8.6c-.6-.3-1.2.1-1.2.7Z"/></symbol>
<symbol id="i-settings" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="3.2"/><path d="M12 2.6v2.2M12 19.2v2.2M2.6 12h2.2M19.2 12h2.2M5.4 5.4l1.6 1.6M17 17l1.6 1.6M18.6 5.4 17 7M7 17l-1.6 1.6"/></symbol>
<symbol id="i-plus" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M12 5.5v13M5.5 12h13"/></symbol>
<!-- Play and pause are one pair, drawn to the same box: 16 units tall, soft
     all round. Pause is two slim bars with fully round ends, a bar's width
     apart twice over. Play is a triangle with its corners rounded by a round
     stroke join, set a unit right of centre: a triangle's weight sits at a
     third of its width, so a mathematically centred one reads as left of
     centre inside a circle. -->
<symbol id="i-play" viewBox="0 0 24 24"><path d="M7.9 5.7v12.6L18.3 12Z" fill="currentColor" stroke="currentColor" stroke-width="3.2" stroke-linejoin="round"/></symbol>
<symbol id="i-pause" viewBox="0 0 24 24" fill="currentColor"><rect x="5.6" y="4" width="3.2" height="16" rx="1.6"/><rect x="15.2" y="4" width="3.2" height="16" rx="1.6"/></symbol>
<symbol id="i-next" viewBox="0 0 24 24" fill="currentColor"><path d="M6.2 5.7v12.6c0 .8.9 1.3 1.6.9l9.2-6.3a1 1 0 0 0 0-1.8L7.8 4.8c-.7-.5-1.6 0-1.6.9Z"/><rect x="18.6" y="4.9" width="1.8" height="14.2" rx=".9"/></symbol>
<symbol id="i-prev" viewBox="0 0 24 24" fill="currentColor"><path d="M17.8 5.7v12.6c0 .8-.9 1.3-1.6.9l-9.2-6.3a1 1 0 0 1 0-1.8l9.2-6.3c.7-.5 1.6 0 1.6.9Z"/><rect x="3.6" y="4.9" width="1.8" height="14.2" rx=".9"/></symbol>
<symbol id="i-shuffle" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 18h1.4c1.3 0 2.5-.6 3.2-1.7l6-8.6c.8-1.1 2-1.7 3.3-1.7h5"/><path d="m18 2.6 3.6 3.4-3.6 3.4"/><path d="M2.5 6h1.8c1.4 0 2.7.9 3.4 2.1"/><path d="M21.5 18h-5.3c-1.3 0-2.5-.7-3.2-1.8l-.5-.7"/><path d="m18 14.6 3.6 3.4-3.6 3.4"/></symbol>
<symbol id="i-repeat" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="m17 2.6 3.6 3.4L17 9.4"/><path d="M3.4 11v-1a4 4 0 0 1 4-4h13.2"/><path d="m7 21.4-3.6-3.4L7 14.6"/><path d="M20.6 13v1a4 4 0 0 1-4 4H3.4"/></symbol>
<symbol id="i-heart-f" viewBox="0 0 24 24" fill="currentColor"><path d="M12 20.6l-1.3-1.2C6 15.1 3 12.3 3 8.9 3 6.2 5.1 4 7.8 4c1.5 0 3 .7 4.2 2 1.1-1.3 2.6-2 4.2-2C18.9 4 21 6.2 21 8.9c0 3.4-3 6.2-7.7 10.5L12 20.6Z"/></symbol>
<symbol id="i-heart" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20.6l-1.3-1.2C6 15.1 3 12.3 3 8.9 3 6.2 5.1 4 7.8 4c1.5 0 3 .7 4.2 2 1.1-1.3 2.6-2 4.2-2C18.9 4 21 6.2 21 8.9c0 3.4-3 6.2-7.7 10.5L12 20.6Z"/></symbol>
<symbol id="i-more" viewBox="0 0 24 24" fill="currentColor"><circle cx="5.4" cy="12" r="1.7"/><circle cx="12" cy="12" r="1.7"/><circle cx="18.6" cy="12" r="1.7"/></symbol>
<symbol id="i-swap" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M4 9h13m-3-3 3 3-3 3"/><path d="M20 15H7m3-3-3 3 3 3"/></symbol>
<symbol id="i-back" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m14.5 5-7 7 7 7"/></symbol>
<symbol id="i-fwd" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m9.5 5 7 7-7 7"/></symbol>
<symbol id="i-clock" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="8.4"/><path d="M12 7.4V12l3 2.5"/></symbol>
<symbol id="i-vol" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5.2 6.6 8.9H3.4v6.2h3.2L11 18.8V5.2Z" fill="currentColor"/><path d="M15.3 9.3a3.8 3.8 0 0 1 0 5.4"/><path d="M18.1 6.5a7.7 7.7 0 0 1 0 11"/></symbol>
<symbol id="i-x" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M17.5 6.5 6.5 17.5M6.5 6.5l11 11"/></symbol>
<symbol id="i-note" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><path d="M9.3 17.6V6.1l9.4-2.1v11.7"/><circle cx="6.8" cy="17.6" r="2.6" fill="currentColor" stroke="none"/><circle cx="16.2" cy="15.7" r="2.6" fill="currentColor" stroke="none"/></symbol>
<symbol id="i-check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12.5 4.5 4.5L19 7"/></symbol>
<symbol id="i-library" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M3.6 21v-8.2M8.4 21V5.8M13.2 21V9.2"/><path d="M17.4 8.6l3.4 11.4"/></symbol>
<symbol id="i-panel" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/></symbol>
<symbol id="i-repeat-one" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="m17 2.6 3.6 3.4L17 9.4"/><path d="M3.4 11v-1a4 4 0 0 1 4-4h13.2"/><path d="m7 21.4-3.6-3.4L7 14.6"/><path d="M20.6 13v1a4 4 0 0 1-4 4H3.4"/><path d="M11.2 10.4h1.2v4"/></symbol>
<symbol id="i-chevron-up" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m18 15-6-6-6 6"/></symbol>
<symbol id="i-chevron-down" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></symbol>
<symbol id="i-logout" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/></symbol>
<symbol id="i-login" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4M10 17l5-5-5-5M15 12H3"/></symbol>
<!-- The cached mark, and it is a SOLID DISC with the arrow punched out of it
     rather than an outlined circle with an arrow inside. It is drawn at 11px
     next to an artist name: at that size a 1.8px stroke describing two nested
     shapes is mush, while filled area still reads as a shape and as a colour.
     Same argument the palette block in app.css makes about small warm marks.
     `evenodd` is what cuts the arrow out, so the row's own background shows
     through and the glyph stays correct on hover and on the playing row. -->
<symbol id="i-cached" viewBox="0 0 24 24"><path fill="currentColor" fill-rule="evenodd" d="M12 2.6a9.4 9.4 0 1 0 0 18.8 9.4 9.4 0 0 0 0-18.8Zm1.3 4.6h-2.6v4.5H8.4L12 16.6l3.6-4.9h-2.3V7.2Z"/></symbol>
<!-- The playlist-skip mark: a thin ring with one diagonal stroke through it.
     Outlined, not filled, on purpose — it sits in the subtitle line as quiet
     status next to the solid cached disc, and a filled prohibition sign there
     would shout "blocked" about what is only a listening preference. Same
     1.8 stroke as every other small outline glyph, so it holds its shape at
     the 13px it renders at instead of going mushy. -->
<symbol id="i-skipped" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="8.4"/><path d="M6.1 6.1 17.9 17.9"/></symbol>
<!-- Added with the round-button tier. Same 1.8 stroke and round joins as the
     outline set above, drawn to hold at 16-20px in a 32-40px circle. -->
<symbol id="i-queue-add" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3.8 6.8h11M3.8 11.8h8M3.8 16.8h5.5"/><path d="M17.6 11.6v8M13.6 15.6h8"/></symbol>
<symbol id="i-playlist-add" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3.4" y="7.4" width="13.2" height="13.2" rx="2.6"/><path d="M7.6 3.6h9.8a3 3 0 0 1 3 3v9.8"/><path d="M10 10.8v6M7 13.8h6"/></symbol>
<symbol id="i-devices" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><rect x="3.4" y="3.4" width="11.2" height="17.2" rx="2.4"/><circle cx="9" cy="14.2" r="2.7"/><circle cx="9" cy="7.7" r=".6" fill="currentColor"/><rect x="17.4" y="8.2" width="3.6" height="12.4" rx="1.3"/></symbol>
<symbol id="i-speaker" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><rect x="5.6" y="2.8" width="12.8" height="18.4" rx="2.6"/><circle cx="12" cy="14.4" r="3.3"/><circle cx="12" cy="7.4" r=".6" fill="currentColor"/></symbol>
<symbol id="i-computer" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4.4" width="18" height="12.2" rx="2"/><path d="M8.6 20.2h6.8M12 16.6v3.6"/></symbol>
<symbol id="i-phone" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="6.6" y="2.6" width="10.8" height="18.8" rx="2.6"/><path d="M11 17.8h2"/></symbol>
<!-- The pin is FILLED: it is drawn at 11px beside a playlist name, where an
     outline pushpin is two strokes of mush. -->
<symbol id="i-pin" viewBox="0 0 24 24" fill="currentColor"><path d="M8.4 2.8h7.2l-1.1 5.7 3.6 3.6v2H5.9v-2l3.6-3.6-1.1-5.7Z"/><rect x="11.1" y="13.4" width="1.8" height="8" rx=".9"/></symbol>
<symbol id="i-eye" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2.4 12s3.6-6.6 9.6-6.6 9.6 6.6 9.6 6.6-3.6 6.6-9.6 6.6S2.4 12 2.4 12Z"/><circle cx="12" cy="12" r="2.9"/></symbol>
<symbol id="i-eye-off" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9.9 5.7c.7-.2 1.4-.3 2.1-.3 6 0 9.6 6.6 9.6 6.6a17 17 0 0 1-2.6 3.4M6.4 7.3C3.9 9 2.4 12 2.4 12s3.6 6.6 9.6 6.6c1.8 0 3.3-.6 4.6-1.4"/><path d="M9.9 9.9a2.9 2.9 0 0 0 4.2 4.2"/><path d="m3.6 3.6 16.8 16.8"/></symbol>
<symbol id="i-copy" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><rect x="8.4" y="8.4" width="12.2" height="12.2" rx="2.4"/><path d="M15.6 5.2A2 2 0 0 0 13.6 3.4H5.4a2 2 0 0 0-2 2v8.2a2 2 0 0 0 1.8 2"/></symbol>
<symbol id="i-sort" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3.8 6.4h10M3.8 11.8h7M3.8 17.2h4.4"/><path d="M18 5v14m-3.2-3.2L18 19l3.2-3.2"/></symbol>
<symbol id="i-refresh" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M19.6 12.4a7.6 7.6 0 1 1-2.3-5.8"/><path d="M19.8 3.6v4.8H15"/></symbol>
<!-- The rest of the device set, for the device list: same outline weight as
     computer, phone and speaker above. -->
<symbol id="i-tablet" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="4.4" y="2.8" width="15.2" height="18.4" rx="2.4"/><path d="M11 17.8h2"/></symbol>
<symbol id="i-tv" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="2.6" y="4.4" width="18.8" height="12.6" rx="2"/><path d="M7.4 20.4h9.2"/></symbol>
<symbol id="i-cast" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3.4 8.4V6.6a2 2 0 0 1 2-2h13.2a2 2 0 0 1 2 2v10.8a2 2 0 0 1-2 2h-5.2"/><path d="M3.4 12.8a6.6 6.6 0 0 1 6.6 6.6M3.4 16.4a3 3 0 0 1 3 3"/><circle cx="3.6" cy="19.2" r=".6" fill="currentColor" stroke="none"/></symbol>
<!-- The action-menu set: every item of a "…" menu leads with one of these,
     at 16px, so they hold the outline weight of the rest. A "go to" item
     wears the glyph of where it goes. -->
<symbol id="i-artist" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="8.2" r="3.9"/><path d="M4.6 20.4c.9-3.7 3.9-6 7.4-6s6.5 2.3 7.4 6"/></symbol>
<symbol id="i-album" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="12" r="8.4"/><circle cx="12" cy="12" r="2.3"/></symbol>
<symbol id="i-radio" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="1.9" fill="currentColor" stroke="none"/><path d="M8.3 8.3a5.2 5.2 0 0 0 0 7.4M15.7 8.3a5.2 5.2 0 0 1 0 7.4M5.4 5.4a9.3 9.3 0 0 0 0 13.2M18.6 5.4a9.3 9.3 0 0 1 0 13.2"/></symbol>
<symbol id="i-podcast" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="9" y="3" width="6" height="11.2" rx="3"/><path d="M5.6 11.4a6.4 6.4 0 0 0 12.8 0M12 17.8v3.2"/></symbol>
<symbol id="i-episode" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><circle cx="12" cy="12" r="8.4"/><path d="M10.3 9v6l4.8-3Z"/></symbol>
<symbol id="i-credits" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="4.8" y="3.4" width="14.4" height="17.2" rx="2.4"/><path d="M8.6 8.4h6.8M8.6 12h6.8M8.6 15.6h4"/></symbol>
<symbol id="i-scissors" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="6.4" cy="6.6" r="2.6"/><circle cx="6.4" cy="17.4" r="2.6"/><path d="M8.6 8.1 20 17.4M8.6 15.9 20 6.6"/></symbol>
<symbol id="i-waveform" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M4 10v4M8 7v10M12 4.5v15M16 8v8M20 10.5v3"/></symbol>
<symbol id="i-restore" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4.4 12.4a7.6 7.6 0 1 0 2.3-5.8"/><path d="M4.2 3.6v4.8H9"/></symbol>
<symbol id="i-pencil" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M15.4 4.8a2.2 2.2 0 0 1 3.1 0l.7.7a2.2 2.2 0 0 1 0 3.1L9 18.8l-4.4 1 1-4.4L15.4 4.8Z"/><path d="m13.6 6.6 3.8 3.8"/></symbol>
<symbol id="i-filter" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3.8 5.2h16.4l-6.3 7.6v5.4l-3.8 2v-7.4L3.8 5.2Z"/></symbol>
<symbol id="i-trash" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4.4 6.8h15.2M9.6 6.8V4.9c0-.6.5-1.1 1.1-1.1h2.6c.6 0 1.1.5 1.1 1.1v1.9"/><path d="m6.4 6.8.9 12.4c.1 1 .9 1.8 1.9 1.8h5.6c1 0 1.8-.8 1.9-1.8l.9-12.4"/></symbol>
<!-- The pin again, as an outline: in a menu it stands in a column of
     outlines at 16px, where the filled 11px mark would be the one heavy
     shape. -->
<symbol id="i-pin-line" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M8.6 3.4h6.8l-1 5.2 3.4 3.4v1.8H6.2V12l3.4-3.4-1-5.2Z"/><path d="M12 13.8v7"/></symbol>
<!-- The window's own controls (WindowControls), drawn on a 12-unit grid for
     the 10px they are shown at: hairline glyphs, as the system's are. -->
<symbol id="i-win-min" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"><path d="M2 6h8"/></symbol>
<symbol id="i-win-max" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.1"><rect x="2" y="2" width="8" height="8" rx="1.4"/></symbol>
<symbol id="i-win-restore" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"><rect x="1.8" y="3.8" width="6.4" height="6.4" rx="1.2"/><path d="M4 3.8v-.4c0-.7.5-1.2 1.2-1.2h3.6c.7 0 1.2.5 1.2 1.2V7c0 .7-.5 1.2-1.2 1.2h-.6"/></symbol>
<symbol id="i-win-close" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"><path d="m2.6 2.6 6.8 6.8m0-6.8-6.8 6.8"/></symbol>
</defs></svg>
