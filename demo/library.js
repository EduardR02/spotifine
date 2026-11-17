/* The demo's music: invented artists, records and playlists, shaped the way
   the engine hands them to the app. Every name here is made up. The covers
   are stock photographs with type set on them (demo/art/), and their sources
   and licenses are listed in demo/public/CREDITS.md. */

/** The listener the demo is signed in as. */
export const ME = "demo";

/** A stable 22-character id, the shape of a Spotify id, from a name. */
function sid(text) {
  const alphabet = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
  let a = 0x811c9dc5;
  let b = 0x9e3779b9;
  let out = "";
  for (let round = 0; out.length < 22; round++) {
    for (let i = 0; i < text.length; i++) {
      const c = text.charCodeAt(i) + round;
      a = Math.imul(a ^ c, 0x01000193) >>> 0;
      b = Math.imul(b ^ (c + a), 0x85ebca6b) >>> 0;
    }
    out += alphabet[a % 62] + alphabet[b % 62];
  }
  return out.slice(0, 22);
}

const cover = (key) => `media/covers/${key}.webp`;
const time = (text) => {
  const [m, s] = text.split(":").map(Number);
  return (m * 60 + s) * 1000;
};

/* Records: [cover, title, artist, [[track, length], ...]]. */
const RECORDS = [
  ["tidepool", "Tidepool Hymns", "Marisol Vey", [["Glass Bodies", "3:48"], ["Undertow", "4:05"], ["Saltwater Choir", "3:31"]]],
  ["afterglow", "Afterglow District", "Kestrel Lane", [["Pink Static", "3:21"], ["Rooftop Antenna", "4:02"], ["Last Call Neon", "3:56"]]],
  ["slowcoast", "Slow Coast", "June Arbor", [["Low Tide", "4:12"], ["Paper Boats", "3:27"], ["Seaglass", "3:49"]]],
  ["goldleaf", "Gold Leaf", "Noor Halloway", [["Petal Logic", "3:05"], ["Honey, Wait", "3:38"]]],
  ["milemarker", "Mile Marker 61", "The Dry Creek Social", [["Sixty-One", "3:40"], ["Gas Station Roses", "4:18"]]],
  ["hollowpines", "Hollow Pines", "Ilse Marwood", [["Under the Canopy", "4:30"], ["Kettle Smoke", "3:14"]]],
  ["coralhour", "Coral Hour", "Teo Valcourt", [["Coral Hour", "3:57"], ["Swimmers", "4:26"]]],
  ["bloomtheory", "Bloom Theory", "Sugar Atlas", [["Cotton Candy Physics", "2:58"], ["Pastel Riot", "3:16"]]],
  ["monolith", "Monolith Club", "Halvard", [["Concrete Choir", "6:12"], ["Brutal Light", "5:41"]]],
  ["lasttrain", "Last Train Home", "Mio & the Midnight Line", [["Red Lantern", "3:44"], ["Platform Nine", "3:09"]]],
  ["waning", "waning", "sleepwalker fm", [["crescent", "2:14"], ["3am tea", "2:31"]]],
  ["palmcinema", "Palm Cinema", "Rosa Vallée", [["Hollywood Dusk", "3:37"], ["Matinee", "3:02"]]],
  ["shallowend", "Shallow End", "Lido Twins", [["Deep End Kids", "2:49"], ["Chlorine", "3:11"]]],
  ["overpass", "Overpass", "Cass Ortega", [["Exit 9", "4:21"], ["Tail Lights", "3:52"]]],
  ["wildfire", "Wildfire Sermon", "Ember & Vine", [["Sparks", "3:12"], ["Kindling", "3:46"]]],
  ["quietranges", "Quiet Ranges", "Aske Nordahl", [["Fourth Ridge", "5:02"], ["Snowline", "4:37"]]],
  ["golden", "Golden Interval", "Solar Choir", [["Long Exposure", "4:44"], ["Daybreak Static", "3:58"]]],
  ["tangerine", "Tangerine Static", "Coastal Ghosts", [["Tangerine", "3:30"], ["Static Bloom", "3:19"]]],
];

/* Two tracks carry a Canvas: a short looping video, as Spotify's do. */
const CANVAS = {
  "Glass Bodies": "media/canvas/jellyfish.mp4",
  Sparks: "media/canvas/ink.mp4",
};

/** Every track, by name (names are unique in this catalogue). */
const tracks = new Map();
for (const [key, album, artist, list] of RECORDS) {
  const albumId = sid(`album:${album}`);
  const artistId = sid(`artist:${artist}`);
  for (const [name, length] of list) {
    const id = sid(`track:${name}`);
    tracks.set(name, {
      id,
      uri: `spotify:track:${id}`,
      name,
      artist_names: [artist],
      artist_ids: [artistId],
      artist_id: artistId,
      album_id: albumId,
      album_name: album,
      cover_url: cover(key),
      duration_ms: time(length),
      unavailable: false,
      unavailable_reason: null,
    });
  }
}

/** The track a name stands for, with the date it was added to a list. */
function row(name, addedAt) {
  const track = tracks.get(name);
  if (!track) throw new Error(`No demo track named ${name}`);
  return { ...track, added_at: addedAt };
}

const DAY = 86_400_000;
/** Fixed, so the page's poster of the demo stays true to it. */
const LATEST = Date.UTC(2026, 9, 28, 19, 30);

/* Playlists: [name, tracks newest first, own cover?]. A playlist without its
   own cover is drawn as a mosaic of its first four records, as the app draws
   any playlist Spotify gives no picture for. */
const LISTS = [
  ["after hours", [
    "Glass Bodies", "Pink Static", "Low Tide", "Petal Logic", "Red Lantern", "Deep End Kids",
    "Sparks", "Under the Canopy", "Exit 9", "Coral Hour", "Cotton Candy Physics", "Concrete Choir",
    "Hollywood Dusk", "crescent", "Sixty-One", "Long Exposure", "Tangerine", "Fourth Ridge",
    "Rooftop Antenna", "Seaglass", "Swimmers", "Tail Lights",
  ]],
  ["sunday slow", [
    "Paper Boats", "Kettle Smoke", "crescent", "3am tea", "Low Tide", "Snowline", "Honey, Wait",
    "Seaglass", "Undertow", "Matinee", "Fourth Ridge", "Saltwater Choir",
  ]],
  ["deep focus, no words", [
    "Concrete Choir", "Fourth Ridge", "Long Exposure", "Brutal Light", "Snowline", "Undertow",
    "Daybreak Static", "Saltwater Choir", "Under the Canopy",
  ]],
  ["run club", [
    "Deep End Kids", "Pink Static", "Sparks", "Exit 9", "Cotton Candy Physics", "Tangerine",
    "Last Call Neon", "Chlorine", "Kindling", "Static Bloom", "Pastel Riot", "Rooftop Antenna",
  ]],
  ["summer '26", [
    "Chlorine", "Deep End Kids", "Hollywood Dusk", "Coral Hour", "Tangerine", "Pastel Riot",
    "Low Tide", "Swimmers", "Matinee", "Petal Logic", "Paper Boats",
  ]],
  ["rainy windows", [
    "Red Lantern", "Platform Nine", "Tail Lights", "Last Call Neon", "3am tea", "Kettle Smoke",
    "Glass Bodies", "Snowline",
  ], "rainywindows"],
  ["coastal", [
    "Seaglass", "Coral Hour", "Saltwater Choir", "Low Tide", "Swimmers", "Undertow", "Paper Boats",
  ], "coastal"],
  ["golden hour", [
    "Long Exposure", "Hollywood Dusk", "Petal Logic", "Sixty-One", "Gas Station Roses", "Honey, Wait",
    "Daybreak Static",
  ]],
  ["3am", [
    "crescent", "3am tea", "Platform Nine", "Exit 9", "Glass Bodies", "Brutal Light",
  ]],
  ["old favourites", [
    "Gas Station Roses", "Sixty-One", "Kindling", "Matinee", "Platform Nine", "Static Bloom",
    "Rooftop Antenna", "Honey, Wait", "Seaglass", "Pink Static", "Under the Canopy", "Snowline",
    "Daybreak Static", "Paper Boats",
  ]],
];

/** Each playlist, as the library lists it and with its rows. */
export const playlists = LISTS.map(([name, names, own], index) => {
  const id = sid(`playlist:${name}`);
  const spacing = (index + 1) * 2.3 * DAY;
  const rows = names.map((track, at) => row(track, LATEST - index * 3 * DAY - Math.round(at * spacing)));
  const covers = [...new Set(rows.map((track) => track.cover_url))].slice(0, 4);
  return {
    summary: {
      id,
      uri: `spotify:playlist:${id}`,
      name,
      description: "",
      owner: ME,
      owner_id: ME,
      cover_url: own ? cover(own) : "",
      cover_urls: covers,
      collaborative: false,
      tracks_total: rows.length,
      snapshot_id: sid(`snapshot:${name}`),
      last_played: null,
      // The rail and Home order the library by last use: this order.
      last_activity: Math.round((LATEST - index * 9 * 3600_000) / 1000),
    },
    rows,
  };
});

/** The playlist the demo opens on, and plays. */
export const OPENING = playlists[0];

/** The rail's tree: a folder of two late-hour lists, the rest loose. */
const byName = (name) => playlists.find((list) => list.summary.name === name).summary.id;
export const tree = [
  ...["after hours", "sunday slow", "deep focus, no words", "run club", "summer '26", "rainy windows", "coastal"]
    .map((name) => ({ kind: "playlist", id: byName(name) })),
  { kind: "folder", id: sid("folder:moods"), name: "moods", children: [
    { kind: "playlist", id: byName("golden hour") },
    { kind: "playlist", id: byName("3am") },
  ] },
  { kind: "playlist", id: byName("old favourites") },
];

/** Liked Songs, newest first. */
export const liked = [
  "Glass Bodies", "Petal Logic", "Low Tide", "Red Lantern", "Sparks", "Coral Hour", "Hollywood Dusk",
  "Under the Canopy", "crescent", "Long Exposure", "Tangerine", "Seaglass", "Matinee", "Kindling",
].map((name, at) => row(name, LATEST - Math.round(at * 4.1 * DAY)));

/** The Canvas for a track id, or "" for none. */
export function canvasFor(id) {
  for (const [name, url] of Object.entries(CANVAS)) {
    if (tracks.get(name).id === id) return url;
  }
  return "";
}

/** The artists the rail's Artists tab lists. */
export const followedArtists = ["Marisol Vey", "Kestrel Lane", "June Arbor", "Noor Halloway", "Ilse Marwood", "Halvard", "Rosa Vallée"]
  .map((name) => ({ id: sid(`artist:${name}`), uri: `spotify:artist:${sid(`artist:${name}`)}`, name, cover_url: "" }));

/* Credits: the performer, and two writers and a producer drawn from a small
   pool of invented names, the same every time for a track. */
const PEOPLE = [
  "Ada Rowe", "Milo Brandt", "Priya Castell", "Jonah Weil", "Sana Okoro", "Felix Marr", "Lena Duvall",
  "Tomas Reyes", "Iris Calloway", "Benji Holt", "Wren Matsuda", "Caleb Stroud", "Noa Lindqvist",
  "Ruben Achterberg", "Hana Petrov", "Oscar Delmar",
];

export function creditsFor(id) {
  const track = [...tracks.values()].find((entry) => entry.id === id);
  if (!track) return null;
  let seed = 0;
  for (const char of id) seed = (seed * 31 + char.charCodeAt(0)) >>> 0;
  const pick = (n) => PEOPLE[(seed + n * 7) % PEOPLE.length];
  const person = (name, subroles) => ({ id: sid(`person:${name}`), uri: "", url: "", name, subroles });
  return {
    track_uri: track.uri,
    track_name: track.name,
    groups: [
      { title: "Performers", contributors: [person(track.artist_names[0], ["Main Artist"])] },
      { title: "Writers", contributors: [person(track.artist_names[0], ["Composer", "Lyricist"]), person(pick(1), ["Composer"]), person(pick(2), ["Lyricist"])] },
      { title: "Producers", contributors: [person(pick(3), ["Producer"]), person(pick(4), ["Mixing Engineer"])] },
    ],
    source: "",
  };
}
