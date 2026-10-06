/**
 * Read-through bridge from the UI harness to the owner's running app.
 *
 * Dev server only (see `realBridge()` at the bottom, `apply: "serve"`).
 *
 *   POST /__real/invoke  {cmd, args, mode?, maxAgeMs?}
 *   GET  /__real/status
 *
 * An invoke is answered, in order, by:
 *   live   the real app's own `window.__TAURI_INTERNALS__.invoke(cmd, args)`,
 *          evaluated over the WebView2 DevTools protocol (start the app with
 *          `bun dev/real-app.js` to open the port). Every success is written to
 *          dev/.real-cache/, which is gitignored: it is the owner's account.
 *   cache  that last live answer, when the app is closed or has no port.
 *   disk   for a few commands, the app's own persisted state under
 *          %LOCALAPPDATA%\Spotifine (queue, library, the 25 most
 *          recently opened playlists, history, settings) — read, never written.
 * When the stored answers disagree, the newer one wins. When nothing answers,
 * the reply is 404 and the harness falls back to its mock.
 *
 * Only commands in READ_COMMANDS are ever forwarded; anything else is refused
 * here with a 403, whatever the harness asks.
 *
 * `mode: "fill"` (used to warm the cache) answers from the cache when an entry
 * younger than `maxAgeMs` exists and goes live only otherwise, so reloading the
 * harness does not re-query the app for everything it already knows.
 */

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { READ_COMMANDS } from "./real-commands.js";

export const CDP_PORT = Number(process.env.REAL_APP_CDP_PORT) || 9341;

/** The page Tauri serves the app from: http://tauri.localhost on Windows. */
const TAURI_PAGE = /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/;

const LIVE_TIMEOUT_MS = { get_track_waveform: 120_000 };
const DEFAULT_LIVE_TIMEOUT_MS = 30_000;
/** After a failed connection, answer from storage for this long before
    trying the port again, so a closed app costs one refused connect per
    burst rather than one per request. */
const RECONNECT_BACKOFF_MS = 1_500;

/* ------------------------------------------------------------------ CDP */

async function fetchJson(url, timeoutMs) {
  const response = await fetch(url, { signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw new Error(`${url} answered ${response.status}`);
  return response.json();
}

/**
 * Runs in the real page. Returns a plain envelope rather than throwing, so a
 * command's own rejection (a Tauri `Err(String)`) is told apart from a broken
 * connection.
 */
const PAGE_INVOKE = `async (cmd, args) => {
  try {
    const value = await window.__TAURI_INTERNALS__.invoke(cmd, args);
    return { ok: true, value: value === undefined ? null : value };
  } catch (error) {
    return { ok: false, error: typeof error === "string" ? error : String(error?.message ?? error) };
  }
}`;

class CdpPage {
  constructor(port) {
    this.port = port;
    this.ws = null;
    this.target = null;
    this.connecting = null;
    this.pending = new Map();
    this.nextId = 1;
    this.retryAt = 0;
    this.lastError = "not connected yet";
  }

  get live() {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  connect() {
    if (this.live) return Promise.resolve();
    if (this.connecting) return this.connecting;
    if (Date.now() < this.retryAt) return Promise.reject(new Error(this.lastError));
    this.connecting = this.open()
      .catch((error) => {
        this.lastError = error.message;
        this.retryAt = Date.now() + RECONNECT_BACKOFF_MS;
        throw error;
      })
      .finally(() => {
        this.connecting = null;
      });
    return this.connecting;
  }

  async open() {
    let list;
    try {
      list = await fetchJson(`http://127.0.0.1:${this.port}/json/list`, 1_000);
    } catch {
      throw new Error(`nothing answers on DevTools port ${this.port} (run \`bun dev/real-app.js\`)`);
    }
    const page = list.find((target) => target.type === "page" && TAURI_PAGE.test(target.url));
    if (!page) throw new Error(`DevTools port ${this.port} answers, but lists no Tauri page`);
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = () => reject(new Error(`DevTools refused the websocket for ${page.url}`));
    });
    ws.onerror = null;
    ws.onmessage = (message) => this.receive(message.data);
    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      for (const { reject } of this.pending.values()) reject(new Error("DevTools connection closed"));
      this.pending.clear();
    };
    this.ws = ws;
    this.target = { url: page.url, title: page.title };
  }

  receive(data) {
    let message;
    try {
      message = JSON.parse(String(data));
    } catch {
      return;
    }
    const waiter = this.pending.get(message.id);
    if (!waiter) return;
    this.pending.delete(message.id);
    clearTimeout(waiter.timer);
    if (message.error) waiter.reject(new Error(message.error.message ?? "DevTools error"));
    else waiter.resolve(message.result);
  }

  async send(method, params, timeoutMs) {
    await this.connect();
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out after ${timeoutMs} ms`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression, timeoutMs) {
    const result = await this.send(
      "Runtime.evaluate",
      { expression, awaitPromise: true, returnByValue: true },
      timeoutMs,
    );
    if (result.exceptionDetails) {
      throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    }
    return result.result?.value;
  }

  /** `{ok, value}` or `{ok: false, error}` from the real page; throws only
      when the page could not be reached. */
  invoke(cmd, args) {
    const expression = `(${PAGE_INVOKE})(${JSON.stringify(cmd)}, ${JSON.stringify(args ?? {})})`;
    return this.evaluate(expression, LIVE_TIMEOUT_MS[cmd] ?? DEFAULT_LIVE_TIMEOUT_MS);
  }

  close() {
    this.ws?.close();
    this.ws = null;
  }
}

/* ---------------------------------------------------------------- cache */

/** JSON with object keys sorted at every depth, so equal args share a key. */
export function stableStringify(value) {
  if (value === null || typeof value !== "object") return JSON.stringify(value) ?? "null";
  if (Array.isArray(value)) return `[${value.map((item) => stableStringify(item ?? null)).join(",")}]`;
  const keys = Object.keys(value).filter((key) => value[key] !== undefined).sort();
  return `{${keys.map((key) => `${JSON.stringify(key)}:${stableStringify(value[key])}`).join(",")}}`;
}

class AnswerCache {
  constructor(dir) {
    this.dir = dir;
  }

  file(cmd, args) {
    const hash = crypto.createHash("sha1").update(stableStringify(args ?? {})).digest("hex").slice(0, 20);
    return path.join(this.dir, encodeURIComponent(cmd), `${hash}.json`);
  }

  read(cmd, args) {
    try {
      return JSON.parse(fs.readFileSync(this.file(cmd, args), "utf8"));
    } catch {
      return null;
    }
  }

  write(cmd, args, value) {
    const file = this.file(cmd, args);
    const entry = { cmd, args: args ?? {}, savedAt: Date.now(), value };
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const temp = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temp, JSON.stringify(entry));
    fs.renameSync(temp, file);
    return entry;
  }

  count() {
    try {
      return fs
        .readdirSync(this.dir, { withFileTypes: true })
        .filter((entry) => entry.isDirectory())
        .reduce((sum, entry) => sum + fs.readdirSync(path.join(this.dir, entry.name)).length, 0);
    } catch {
      return 0;
    }
  }
}

/* ------------------------------------------------------ the app's disk */

/** Mirrors `data_dir()` in src-tauri/src/app.rs. */
export function appDataDir() {
  const base = process.env.LOCALAPPDATA ?? path.join(process.env.USERPROFILE ?? "", "AppData", "Local");
  return path.join(base, "Spotifine");
}

/**
 * Read-only views of what the app persists, shaped like the command answers.
 * Each returns `{value, savedAt}` or null. Parsed files are memoised by mtime.
 */
class AppDisk {
  constructor(dir) {
    this.dir = dir;
    this.memo = new Map();
  }

  load(name, parse = JSON.parse) {
    const file = path.join(this.dir, name);
    let stat;
    try {
      stat = fs.statSync(file);
    } catch {
      return null;
    }
    const hit = this.memo.get(file);
    if (hit && hit.mtimeMs === stat.mtimeMs) return hit;
    try {
      const entry = { mtimeMs: stat.mtimeMs, data: parse(fs.readFileSync(file, "utf8")) };
      this.memo.set(file, entry);
      return entry;
    } catch {
      return null;
    }
  }

  get available() {
    return fs.existsSync(path.join(this.dir, "playback_state.json"));
  }

  answer(cmd, args = {}) {
    switch (cmd) {
      case "get_state":
        return this.state();
      case "browse_playlist":
        return this.playlist(String(args.id ?? ""));
      case "get_track_playlists":
        return this.trackPlaylists(String(args.uri ?? "").trim());
      case "get_history":
        return this.history(args);
      default:
        return null;
    }
  }

  library() {
    return this.load("playlist_list.json");
  }

  /** `AppStateSnapshot` from playback_state.json + playlist_list.json. */
  state() {
    const saved = this.load("playback_state.json");
    if (!saved) return null;
    const list = this.library();
    const settings = this.settings();
    const { queue = [], current_index: index = null } = saved.data;
    const current = index == null ? null : queue[index];
    const meId = list?.data.me_id ?? "";
    return {
      savedAt: Math.max(saved.mtimeMs, list?.mtimeMs ?? 0, settings?.savedAt ?? 0),
      value: {
        playback: {
          ready: true,
          auth_state: "ready",
          auth_url: "",
          playing: false,
          buffering: false,
          preview: false,
          username: meId,
          position_ms: saved.data.position_ms ?? 0,
          duration_ms: current?.duration_ms ?? 0,
          volume: saved.data.volume ?? 70,
          shuffle: Boolean(saved.data.shuffle),
          repeat: saved.data.repeat ?? "off",
          playback_speed: (current?.uri?.startsWith("spotify:episode:") ? saved.data.episode_speed : saved.data.track_speed) ?? 1,
          track_speed: saved.data.track_speed ?? 1,
          episode_speed: saved.data.episode_speed ?? 1,
          current_index: index,
          current_uri: current?.uri ?? "",
          context: current?.context ?? "",
          queue,
          queue_revision: 0,
          order_revision: 0,
          upcoming: [],
          error: "",
        },
        playlists: list?.data.playlists ?? [],
        playlist_tree: list?.data.playlist_tree ?? [],
        settings: settings?.value ?? {},
        me_id: meId,
      },
    };
  }

  /** `playlist_detail_from_cache` in app.rs: library metadata (when the
      playlist is followed) flattened beside the cached rows. */
  playlist(id) {
    const cache = this.load("playlist_tracks_cache.json");
    const entry = cache?.data.playlists?.find((item) => item.id === id);
    if (!entry) return null;
    const meta = this.library()?.data.playlists?.find((item) => item.id === id);
    const coverUrls = [];
    for (const track of entry.tracks) {
      if (!track.cover_url || coverUrls.includes(track.cover_url)) continue;
      coverUrls.push(track.cover_url);
      if (coverUrls.length === 4) break;
    }
    return {
      savedAt: (entry.fetched_at ?? 0) * 1000,
      value: {
        ...(meta ?? { id, uri: `spotify:playlist:${id}`, name: "", description: "", owner: "", owner_id: "", cover_url: "", collaborative: false, last_played: null, last_activity: null }),
        tracks_total: entry.tracks.length,
        snapshot_id: entry.revision ?? "",
        cover_urls: coverUrls,
        tracks: entry.tracks,
        excluded_track_ids: entry.excluded_track_ids ?? [],
      },
    };
  }

  /** `get_track_playlists`: Liked Songs first, then library order. */
  trackPlaylists(uri) {
    const index = this.load("playlist_membership.json");
    if (!index || !uri) return null;
    const holds = (id) => index.data.entries?.some((entry) => entry.id === id && entry.uris.includes(uri));
    const refs = holds("liked") ? [{ id: "liked", name: "Liked Songs" }] : [];
    for (const playlist of this.library()?.data.playlists ?? []) {
      if (holds(playlist.id)) refs.push({ id: playlist.id, name: playlist.name });
    }
    return { savedAt: index.mtimeMs, value: refs };
  }

  /** `get_history` over listening_history.jsonl, filtered and ordered the
      way engine/src/history.rs does it. */
  history({ offset = 0, limit = 100, query = "", sort = "recent" }) {
    const archive = this.load("engine/listening_history.jsonl", (text) =>
      text
        .split("\n")
        .filter(Boolean)
        .map((line) => {
          const { format, ...row } = JSON.parse(line);
          return row;
        })
        .filter((row) => row.track),
    );
    if (!archive) return null;
    const needle = String(query).trim().toLowerCase();
    const artists = (row) => (row.track.artist_names ?? []).join(", ").toLowerCase();
    const title = (row) => String(row.track.name ?? "").toLowerCase();
    let rows = archive.data.slice();
    if (needle) rows = rows.filter((row) => title(row).includes(needle) || artists(row).includes(needle));
    const order = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
    if (sort === "oldest") rows.sort((a, b) => a.started_at - b.started_at);
    else if (sort === "title") rows.sort((a, b) => order(title(a), title(b)) || b.started_at - a.started_at);
    else if (sort === "artist") {
      rows.sort((a, b) => order(artists(a), artists(b)) || order(title(a), title(b)) || b.started_at - a.started_at);
    } else rows.sort((a, b) => b.started_at - a.started_at);
    const start = Math.max(0, Number(offset) || 0);
    const entries = rows.slice(start, start + Math.max(1, Number(limit) || 100));
    const next = start + entries.length;
    return {
      savedAt: archive.mtimeMs,
      value: {
        entries,
        total: rows.length,
        recorded: archive.data.length,
        offset: start,
        next_offset: next < rows.length ? next : null,
      },
    };
  }

  settings() {
    const saved = this.load("settings.json");
    if (!saved) return null;
    const value = { ...saved.data };
    if (![0, 1024, 2048, 4096, 8192].includes(value.audio_cache_limit_mb)) value.audio_cache_limit_mb = 1024;
    return { savedAt: saved.mtimeMs, value };
  }
}

/* --------------------------------------------------------------- bridge */

export function createBridge({ cacheDir, port = CDP_PORT, diskDir = appDataDir() }) {
  const page = new CdpPage(port);
  const cache = new AnswerCache(cacheDir);
  const disk = new AppDisk(diskDir);

  function stored(cmd, args) {
    const cached = cache.read(cmd, args);
    const fromDisk = disk.answer(cmd, args);
    const pick =
      cached && (!fromDisk || cached.savedAt >= fromDisk.savedAt)
        ? { source: "cache", savedAt: cached.savedAt, value: cached.value }
        : fromDisk && { source: "disk", savedAt: fromDisk.savedAt, value: fromDisk.value };
    return pick || null;
  }

  /** Returns `[httpStatus, body]`. */
  async function invoke({ cmd, args = {}, mode = "live", maxAgeMs = 6 * 3600_000 }) {
    if (typeof cmd !== "string" || !READ_COMMANDS.has(cmd)) {
      return [403, { ok: false, source: "refused", error: `${cmd} is not on the read-only allowlist (dev/real-commands.js)` }];
    }
    if (mode === "fill") {
      const cached = cache.read(cmd, args);
      if (cached && Date.now() - cached.savedAt < maxAgeMs) {
        return [200, { ok: true, source: "cache", savedAt: cached.savedAt, value: cached.value }];
      }
    }
    let liveError;
    try {
      const reply = await page.invoke(cmd, args);
      if (reply?.ok) {
        cache.write(cmd, args, reply.value);
        return [200, { ok: true, source: "live", value: reply.value }];
      }
      // The real command itself rejected: that is the answer, not an outage.
      return [200, { ok: false, source: "live", error: reply?.error ?? "the real app gave no answer" }];
    } catch (error) {
      liveError = error.message;
    }
    const answer = stored(cmd, args);
    if (answer) return [200, { ok: true, ...answer, live: liveError }];
    return [404, { ok: false, source: "none", error: `live: ${liveError}; nothing stored for ${cmd} ${stableStringify(args)}` }];
  }

  async function status() {
    let live = false;
    let pageOrigin = null;
    try {
      await page.connect();
      pageOrigin = await page.evaluate("location.origin", 3_000);
      live = true;
    } catch {
      /* reported below */
    }
    return {
      live,
      port,
      target: live ? page.target : null,
      pageOrigin,
      error: live ? null : page.lastError,
      cache: { dir: cacheDir, entries: cache.count() },
      disk: { dir: diskDir, available: disk.available },
      allowlist: [...READ_COMMANDS],
    };
  }

  function readBody(req) {
    return new Promise((resolve, reject) => {
      let body = "";
      req.setEncoding("utf8");
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => resolve(body));
      req.on("error", reject);
    });
  }

  function send(res, statusCode, body) {
    const text = JSON.stringify(body);
    res.writeHead(statusCode, { "content-type": "application/json", "cache-control": "no-store" });
    res.end(text);
  }

  async function middleware(req, res, next) {
    try {
      if (req.method === "GET" && req.url === "/status") return send(res, 200, await status());
      if (req.method === "POST" && req.url === "/invoke") {
        let request;
        try {
          request = JSON.parse(await readBody(req));
        } catch {
          return send(res, 400, { ok: false, source: "refused", error: "body must be JSON {cmd, args}" });
        }
        const [statusCode, body] = await invoke(request ?? {});
        return send(res, statusCode, body);
      }
      next();
    } catch (error) {
      send(res, 500, { ok: false, source: "none", error: String(error?.message ?? error) });
    }
  }

  return { invoke, status, middleware, close: () => page.close() };
}

/** The Vite plugin: mounts the bridge under /__real on the dev server only. */
export function realBridge() {
  let bridge = null;
  return {
    name: "real-app-bridge",
    apply: "serve",
    configureServer(server) {
      bridge = createBridge({ cacheDir: path.join(server.config.root, "dev", ".real-cache") });
      server.middlewares.use("/__real", (req, res, next) => bridge.middleware(req, res, next));
      server.httpServer?.once("close", () => bridge?.close());
    },
  };
}
