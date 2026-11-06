"""SAIL Music player in a real browser: uploads and Spotify in one queue (fixture APIs).

Uploads play through a real <audio> element from a stream ticket (WAV fixtures). Spotify
plays through a fake of the Web Playback SDK, served in place of sdk.scdn.co (the real SDK
needs Widevine and a Premium account, which a test browser does not have). Checks:
- one list of uploads + Spotify search results, badges per source, Play/Shuffle;
- a mixed queue: upload -> (ended) -> Spotify on the "SAIL Player" device with exactly that
  track URI -> (Spotify reports it finished) -> next upload; only one engine plays at a time;
- pause/resume and seek reach the right engine; volume applies to both;
- upload: file input posts raw bytes with a percent-encoded X-Filename;
- Spotify states: not connected, needs reconnect (button), SDK init error message;
- embedded in SAIL's Entertainment pane as a credentialless iframe;
- no page errors; layout fits at 1440 and 390px.

Run with: .venv-voice/bin/python scripts/ui/music_player_browser_test.py [shot-dir]
"""
import json, math, struct, sys
from pathlib import Path
from urllib.parse import unquote
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / "crates/sail-dialogue/web"
ICONS = ROOT / "assets/source/icons/sail-icons.svg"
SHOTS = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp")
ALIASES = {"/": "index.html", "/music": "music.html", "/vendor/paper-shaders.js": "vendor/paper-shaders/paper-shaders.js",
           "/vendor/liquidglass.js": "vendor/liquidglass/liquidglass.js", "/vendor/xterm.js": "vendor/xterm/xterm.js",
           "/vendor/addon-fit.js": "vendor/xterm/addon-fit.js", "/vendor/xterm.css": "vendor/xterm/xterm.css"}


def tone(seconds, freq=440.0, rate=8000):
    n = int(seconds * rate)
    data = b"".join(struct.pack("<h", int(6000 * math.sin(2 * math.pi * freq * i / rate))) for i in range(n))
    return b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16) + b"data" + struct.pack("<I", len(data)) + data


AUDIO = {"aaaaaaaaaaaaaaaaaaaaaaa1": tone(1.2), "aaaaaaaaaaaaaaaaaaaaaaa2": tone(1.2, 660)}
LOCAL = [
    {"source": "local", "id": "aaaaaaaaaaaaaaaaaaaaaaa1", "title": "Harbour Lights", "artist": "Mara Vale", "album": "Low Tide", "duration_ms": 1200, "has_cover": False, "format": "wav"},
    {"source": "local", "id": "aaaaaaaaaaaaaaaaaaaaaaa2", "title": "Paper Moons", "artist": "Mara Vale", "album": "Low Tide", "duration_ms": 1200, "has_cover": False, "format": "wav"},
]
SPOT = [{"source": "spotify", "id": "spotify:track:4uLU6hMCjMI75M1A2tKUQC", "title": "Harbour Song", "artist": "Spotify Artist", "album": "Single", "duration_ms": 180000, "art": ""}]
FAKE_SDK = """
window.__sdk = {calls: [], token: null, player: null};
class Player {
  constructor(o) { this.o = o; this.l = {}; __sdk.player = this; __sdk.name = o.name; }
  addListener(e, f) { (this.l[e] = this.l[e] || []).push(f); return true; }
  emit(e, d) { (this.l[e] || []).forEach(f => f(d)); }
  connect() {
    if (window.__sdkFail) { setTimeout(() => this.emit('initialization_error', {message: 'no EME'}), 10); return Promise.resolve(true); }
    this.o.getOAuthToken(t => { __sdk.token = t; setTimeout(() => this.emit('ready', {device_id: 'fakedevice123'}), 10); });
    return Promise.resolve(true);
  }
  pause() { __sdk.calls.push('pause'); return Promise.resolve(); }
  resume() { __sdk.calls.push('resume'); return Promise.resolve(); }
  seek(ms) { __sdk.calls.push('seek:' + ms); return Promise.resolve(); }
  setVolume(v) { __sdk.calls.push('volume:' + v); return Promise.resolve(); }
  activateElement() { __sdk.calls.push('activate'); return Promise.resolve(); }
  nextTrack() { __sdk.calls.push('next'); return Promise.resolve(); }
  previousTrack() { __sdk.calls.push('previous'); return Promise.resolve(); }
}
window.Spotify = {Player};
if (window.onSpotifyWebPlaybackSDKReady) window.onSpotifyWebPlaybackSDKReady();
"""
WS_STUB = "class W{constructor(){this.readyState=0;setTimeout(()=>{this.readyState=1;this.onopen&&this.onopen()},10)}send(){}close(){}};W.OPEN=1;window.WebSocket=W;"


class Fixture:
    def __init__(self, spotify="stream"):
        self.spotify = spotify  # "none" | "noscope" | "stream"
        self.plays, self.imports, self.local = [], [], [dict(t) for t in LOCAL]

    def route(self, r):
        url = r.request.url
        if url.startswith("https://sdk.scdn.co/"):
            return r.fulfill(body=FAKE_SDK, content_type="text/javascript")
        path = "/" + url.split("://", 1)[1].split("/", 1)[1]
        path, _, query = path.partition("?")
        if path == "/icons.svg": return r.fulfill(body=ICONS.read_bytes(), content_type="image/svg+xml")
        if path == "/api/guardian/token": return r.fulfill(json={"token": "t" * 32})
        # Library and Spotify player routes need the owner; streams are ticket-authorised.
        if (path.startswith("/api/music/") and not path.startswith("/api/music/stream/")) or path.startswith("/api/spotify/player"):
            if r.request.headers.get("authorization") != "Bearer " + "t" * 32:
                return r.fulfill(status=401, json={"error": "owner"})
        if path == "/api/music/library": return r.fulfill(json={"tracks": self.local})
        if path == "/api/music/import":
            name = unquote(r.request.headers["x-filename"])
            self.imports.append((name, len(r.request.post_data_buffer or b"")))
            t = {"source": "local", "id": "aaaaaaaaaaaaaaaaaaaaaaa3", "title": Path(name).stem, "artist": "Unknown artist", "album": "", "duration_ms": 1200, "has_cover": False}
            self.local.append(t); AUDIO[t["id"]] = tone(1.2, 550)
            return r.fulfill(json={"track": t, "new": True})
        if path.startswith("/api/music/tracks/") and path.endswith("/stream"):
            tid = path.split("/")[4]
            return r.fulfill(json={"url": f"/api/music/stream/ticket-{tid}"})
        if path.startswith("/api/music/stream/ticket-"):
            return r.fulfill(body=AUDIO[path.rsplit("-", 1)[1]], content_type="audio/wav", headers={"accept-ranges": "bytes"})
        if path == "/api/spotify/status":
            return r.fulfill(json={"configured": self.spotify != "none", "connected": self.spotify != "none", "can_stream": self.spotify == "stream", "now_playing": None})
        if path == "/api/spotify/player/playlists": return r.fulfill(json={"playlists": [{"kind": "playlist", "name": "Evening", "by": "Pedro", "uri": "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"},
            {"kind": "playlist", "name": "Thall Radio", "by": "KeifCheif4209", "uri": "spotify:playlist:followedplaylist0000000"}]})
        if path == "/api/spotify/player/tracks":
            if "followedplaylist" in query:
                return r.fulfill(status=409, json={"code": "not_owner", "error": "Spotify only shares the songs of playlists you created or collaborate on. You can still play this playlist as a whole."})
            return r.fulfill(json={"tracks": SPOT})
        if path == "/api/spotify/player/search": return r.fulfill(json={"tracks": SPOT})
        if path == "/api/spotify/player/token": return r.fulfill(json={"access_token": "browser-token", "expires_at": 9999999999})
        if path == "/api/spotify/player/play":
            self.plays.append(json.loads(r.request.post_data)); return r.fulfill(json={"playing": True})
        if path == "/api/spotify/login": return r.fulfill(json={"url": "https://accounts.spotify.com/authorize?x=1"})
        if path.startswith("/api/"): return r.fulfill(status=404, json={})
        f = WEB / ALIASES.get(path, path.lstrip("/"))
        if not f.is_file(): return r.fulfill(status=404, body="")
        headers = {} if path == "/music" else {"cross-origin-embedder-policy": "require-corp", "cross-origin-opener-policy": "same-origin"}
        r.fulfill(body=f.read_bytes(), headers=headers, content_type={".css": "text/css", ".js": "text/javascript"}.get(f.suffix, "text/html"))


errors = []


def open_page(b, fx, width=1440, height=900, init=()):
    ctx = b.new_context(viewport={"width": width, "height": height})
    pg = ctx.new_page()
    pg.on("pageerror", lambda e: errors.append(str(e)))
    for s in init: pg.add_init_script(s)
    pg.route("**/*", fx.route)
    pg.goto("https://sail.test/music")
    pg.wait_for_function("window.SailMusic && document.querySelectorAll('.mp-row').length >= 2")
    return ctx, pg


with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox", "--autoplay-policy=no-user-gesture-required", "--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"])

    # ---- one library, two sources, mixed queue
    fx = Fixture("stream")
    ctx, pg = open_page(b, fx)
    assert pg.locator(".mp-row .mp-badge[data-source=local]").count() == 2
    assert pg.locator("#playlists button").all_inner_texts() == ["Evening", "Thall Radio"]
    pg.fill("#search", "harbour")
    pg.wait_for_function("document.querySelectorAll('.mp-row .mp-badge[data-source=spotify]').length === 1")
    titles = pg.locator(".mp-row strong").all_inner_texts()
    assert titles == ["Harbour Lights", "Harbour Song"], titles
    pg.screenshot(path=str(SHOTS / "music-search-1440.png"))

    # Queue: upload, Spotify, upload.
    pg.evaluate("""() => { const s = SailMusic.state; SailMusic.player.start([s.local[0], {source:'spotify', id:'spotify:track:4uLU6hMCjMI75M1A2tKUQC', title:'Harbour Song', artist:'Spotify Artist', album:'Single', duration_ms:180000, art:''}, s.local[1]]); }""")
    pg.wait_for_function("SailMusic.state.playing && SailMusic.player.engine === SailMusic.local", timeout=8000)
    assert pg.inner_text("#nowTitle") == "Harbour Lights"
    assert pg.get_attribute("#nowSource", "data-source") == "local"
    # The upload ends on its own and the Spotify track starts on the SAIL Player device.
    pg.wait_for_function("SailMusic.player.engine === SailMusic.spotify", timeout=8000)
    pg.wait_for_function("window.__sdk && __sdk.player", timeout=5000)
    pg.wait_for_function(f"{len(fx.plays)} < 1 || true")
    pg.wait_for_timeout(300)
    assert fx.plays == [{"device_id": "fakedevice123", "uris": ["spotify:track:4uLU6hMCjMI75M1A2tKUQC"], "position_ms": 0}], fx.plays
    assert pg.evaluate("__sdk.name") == "SAIL Player"
    assert pg.evaluate("__sdk.token") == "browser-token"
    assert pg.evaluate("SailMusic.local.audio.paused"), "only one engine plays"
    uri = "spotify:track:4uLU6hMCjMI75M1A2tKUQC"
    pg.evaluate(f"__sdk.player.emit('player_state_changed', {{paused:false, position:42000, duration:180000, track_window:{{current_track:{{uri:'{uri}'}}, previous_tracks:[]}}}})")
    assert pg.evaluate("SailMusic.state.playing") is True
    assert pg.inner_text("#nowTitle") == "Harbour Song" and pg.get_attribute("#nowSource", "data-source") == "spotify"
    assert pg.inner_text("#elapsed") == "0:42"
    # Pause/seek/volume go to Spotify while it is current.
    pg.click("#toggle"); pg.wait_for_timeout(100)
    assert "pause" in pg.evaluate("__sdk.calls")
    pg.evaluate("SailMusic.player.seek(60000)")
    assert "seek:60000" in pg.evaluate("__sdk.calls")
    pg.fill("#volume", "50"); pg.dispatch_event("#volume", "input")
    assert "volume:0.5" in pg.evaluate("__sdk.calls") and pg.evaluate("SailMusic.local.audio.volume") == 0.5
    pg.screenshot(path=str(SHOTS / "music-spotify-1440.png"))
    # Spotify reports the track finished: SAIL's queue moves on to the next upload.
    pg.evaluate(f"__sdk.player.emit('player_state_changed', {{paused:true, position:0, duration:180000, track_window:{{current_track:{{uri:'spotify:track:other'}}, previous_tracks:[{{uri:'{uri}'}}]}}}})")
    pg.wait_for_function("SailMusic.player.engine === SailMusic.local && SailMusic.state.playing", timeout=8000)
    assert pg.inner_text("#nowTitle") == "Paper Moons"
    # Queue panel lists all three, current marked.
    pg.click("#queueButton")
    assert pg.locator("#queue li").count() == 3 and "Paper Moons" in pg.locator("#queue li.is-current").inner_text()
    # Local pause/seek.
    pg.click("#toggle"); pg.wait_for_function("SailMusic.local.audio.paused")
    pg.evaluate("SailMusic.player.seek(500)")
    assert abs(pg.evaluate("SailMusic.local.audio.currentTime") - 0.5) < 0.05

    # Upload with a non-ASCII name.
    pg.set_input_files("#upload", files=[{"name": "Café Night.wav", "mimeType": "audio/wav", "buffer": tone(1.2, 550)}])
    pg.wait_for_function("document.querySelectorAll('.mp-row .mp-badge[data-source=local]').length === 3 || SailMusic.state.local.length === 3")
    assert fx.imports and fx.imports[0][0] == "Café Night.wav" and fx.imports[0][1] > 1000, fx.imports
    # Playlist view lists Spotify tracks.
    pg.fill("#search", ""); pg.click("#playlists button >> nth=0")
    pg.wait_for_function("document.getElementById('viewTitle').textContent === 'Evening'")
    assert pg.locator(".mp-row").count() == 1
    assert pg.evaluate("document.documentElement.scrollWidth <= innerWidth")

    # Followed playlist: Spotify won't share its songs (Feb 2026), so it plays as a whole.
    pg.click("#playlists button >> nth=1")
    pg.wait_for_selector(".mp-context button")
    assert "play this playlist as a whole" in pg.inner_text(".mp-context")
    assert "Forbidden" not in pg.inner_text("body")
    pg.click(".mp-context button")
    pg.wait_for_function("SailMusic.player.engine === SailMusic.spotify && SailMusic.state.queue[0].context")
    pg.wait_for_timeout(300)
    assert fx.plays[-1] == {"device_id": "fakedevice123", "context_uri": "spotify:playlist:followedplaylist0000000"}, fx.plays[-1]
    ctx_state = lambda name, uri, paused, pos, nxt: (
        f"__sdk.player.emit('player_state_changed', {{paused:{str(paused).lower()}, position:{pos}, duration:200000, "
        f"track_window:{{current_track:{{uri:'{uri}', name:'{name}', artists:[{{name:'Thall Band'}}], album:{{images:[]}}}}, "
        f"previous_tracks:[], next_tracks:{nxt}}}}})")
    pg.evaluate(ctx_state("Djent One", "spotify:track:aaaaaaaaaaaaaaaaaaaaaa", False, 1000, "[{uri:'spotify:track:bbbbbbbbbbbbbbbbbbbbbb'}]"))
    assert pg.inner_text("#nowTitle") == "Djent One" and pg.inner_text("#nowArtist") == "Thall Band · Thall Radio"
    pg.click("#next"); pg.wait_for_timeout(100)
    assert pg.evaluate("__sdk.calls").count("next") == 1, "next moves within the playlist"
    pg.evaluate(ctx_state("Djent Two", "spotify:track:bbbbbbbbbbbbbbbbbbbbbb", False, 199000, "[]"))
    assert pg.inner_text("#nowTitle") == "Djent Two"
    pg.evaluate(ctx_state("Djent Two", "spotify:track:bbbbbbbbbbbbbbbbbbbbbb", True, 0, "[]"))
    pg.wait_for_function("document.getElementById('status').textContent.includes('End of the queue')", timeout=5000)
    pg.screenshot(path=str(SHOTS / "music-followed-playlist.png"))
    ctx.close()

    # ---- phone layout
    ctx, pg = open_page(b, Fixture("stream"), 390, 844)
    assert pg.evaluate("document.documentElement.scrollWidth <= innerWidth")
    bar = pg.locator(".mp-bar").bounding_box(); assert bar["y"] + bar["height"] <= 844 + 1, bar
    pg.screenshot(path=str(SHOTS / "music-390.png"))
    ctx.close()

    # ---- Spotify states
    ctx, pg = open_page(b, Fixture("none"))
    assert "not connected" in pg.inner_text("#spotifyState")
    pg.fill("#search", "harbour"); pg.wait_for_timeout(500)
    assert pg.locator(".mp-row .mp-badge[data-source=spotify]").count() == 0
    ctx.close()
    ctx, pg = open_page(b, Fixture("noscope"))
    pg.wait_for_selector("#spotifyState button")
    assert pg.inner_text("#spotifyState button") == "Reconnect Spotify"
    ctx.close()
    ctx, pg = open_page(b, Fixture("stream"), init=("window.__sdkFail = true;",))
    pg.evaluate("SailMusic.spotify.connect().catch(() => {})")
    pg.wait_for_function("document.getElementById('spotifyState').textContent.includes('DRM')", timeout=5000)
    ctx.close()

    # ---- embedded in SAIL's Entertainment pane
    ctx = b.new_context(viewport={"width": 1440, "height": 900}); pg = ctx.new_page()
    pg.on("pageerror", lambda e: errors.append(str(e)))
    pg.add_init_script(WS_STUB); pg.route("**/*", Fixture("stream").route)
    pg.goto("https://sail.test/")
    pg.wait_for_selector(".tm-label[data-station=entertainment]")
    pg.click(".tm-label[data-station=entertainment]"); pg.click(".tm-label[data-station=entertainment]")
    pg.click("#tmMediaTab-music")
    frame_el = pg.wait_for_selector("#tmMusic")
    assert frame_el.get_attribute("credentialless") == "" and frame_el.get_attribute("src") == "/music"
    frame = frame_el.content_frame()
    frame.wait_for_function("window.SailMusic && document.querySelectorAll('.mp-row').length >= 2", timeout=10000)
    assert frame.evaluate("document.documentElement.dataset.embedded") == "true"
    assert pg.evaluate("crossOriginIsolated"), "the SAIL app stays cross-origin isolated"
    pg.screenshot(path=str(SHOTS / "music-embedded.png"))
    ctx.close()
    b.close()

assert not errors, errors
print("Music player: one list of uploads + Spotify, mixed queue upload -> Spotify (SAIL Player device, exact URI) -> upload, "
      "single active engine, pause/seek/volume per engine, upload with encoded name, playlist view, Spotify states, "
      "phone layout, credentialless embed in an isolated SAIL passed (fixture APIs, fake SDK)")
