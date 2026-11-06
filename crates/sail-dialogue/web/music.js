'use strict';
// SAIL Music: one player for the owner's uploaded (bought, DRM-free) music and Spotify.
//
// Two engines sit behind one queue:
//   local   — an <audio> element streaming /api/music/stream/<ticket> (Range, so seeking works);
//   spotify — Spotify's Web Playback SDK, which makes this page a Spotify Connect device
//             ("SAIL Player"). SAIL asks Spotify to play exactly one track URI on it, so the
//             SAIL queue (not Spotify's) decides what comes next.
// Only one engine plays at a time; switching items pauses the other. This page is served
// without cross-origin isolation (the SDK loads from sdk.scdn.co) and is embedded in SAIL
// as a credentialless iframe. Nothing here reaches Mind or any model.
(function () {
  try { if (window.top !== window.self) document.documentElement.dataset.embedded = 'true'; } catch (_) { document.documentElement.dataset.embedded = 'true'; }
  const $ = id => document.getElementById(id);
  const SDK_URL = 'https://sdk.scdn.co/spotify-player.js';
  const fmt = ms => { const s = Math.max(0, Math.round((ms || 0) / 1000)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; };

  // ------------------------------------------------------------------ owner API
  const api = {
    token: null,
    async auth() {
      if (this.token) return this.token;
      const r = await fetch('/api/guardian/token', {cache: 'no-store'});
      if (!r.ok) throw new Error('SAIL is not ready yet.');
      this.token = (await r.json()).token;
      return this.token;
    },
    async request(path, {method = 'GET', body, headers = {}, binary = false} = {}) {
      const token = await this.auth();
      const init = {method, cache: 'no-store', headers: {Authorization: `Bearer ${token}`, ...headers}};
      if (body !== undefined) {
        if (body instanceof Blob || body instanceof ArrayBuffer) init.body = body;
        else { init.body = JSON.stringify(body); init.headers['Content-Type'] = 'application/json'; }
      }
      const r = await fetch(path, init);
      if (r.status === 204) return null;
      if (!r.ok) {
        let data = {};
        try { data = await r.json(); } catch (_) {}
        const error = new Error(data.error || `Request failed (${r.status})`);
        error.code = data.code; error.status = r.status;
        throw error;
      }
      return binary ? r.blob() : r.json();
    },
  };

  // ------------------------------------------------------------------ state
  const state = {
    view: 'all', query: '', local: [], spotify: {configured: false, connected: false, can_stream: false},
    playlists: [], results: null, listTracks: null, listTitle: '', listContext: null,
    queue: [], index: -1, playing: false, position: 0, duration: 0, volume: 0.8,
  };
  try { state.volume = Math.min(1, Math.max(0, +(localStorage.getItem('sail.music.volume') ?? 0.8))); } catch (_) {}

  function status(text = '', error = false) {
    $('status').textContent = text;
    $('status').classList.toggle('error', error);
  }

  // ------------------------------------------------------------------ covers
  const covers = new Map();
  function coverUrl(item) {
    if (item.source === 'spotify') return Promise.resolve(item.art || '');
    if (!item.has_cover) return Promise.resolve('');
    if (!covers.has(item.id)) covers.set(item.id, api.request(`/api/music/tracks/${item.id}/cover`, {binary: true})
      .then(b => URL.createObjectURL(b)).catch(() => ''));
    return covers.get(item.id);
  }
  function artNode(item, cls = 'mp-thumb') {
    const node = document.createElement('span');
    node.className = cls;
    node.dataset.source = item.source;
    node.textContent = (item.title || '?').trim().charAt(0).toUpperCase();
    coverUrl(item).then(url => {
      if (!url) return;
      const img = new Image();
      img.alt = ''; img.decoding = 'async';
      if (item.source === 'spotify') { img.crossOrigin = 'anonymous'; img.referrerPolicy = 'no-referrer'; }
      img.onload = () => { node.textContent = ''; node.append(img); };
      img.src = url;
    });
    return node;
  }

  // ------------------------------------------------------------------ engines
  const local = {
    audio: new Audio(),
    current: null,
    init() {
      const a = this.audio;
      a.preload = 'auto';
      a.addEventListener('timeupdate', () => { if (player.engine === this) player.progress(a.currentTime * 1000, a.duration * 1000); });
      a.addEventListener('playing', () => { if (player.engine === this) player.setPlaying(true); });
      a.addEventListener('pause', () => { if (player.engine === this && !a.ended) player.setPlaying(false); });
      a.addEventListener('ended', () => { if (player.engine === this) player.ended(); });
      a.addEventListener('error', () => { if (player.engine === this && a.src) player.failed('This file could not be played in this browser.'); });
    },
    async load(item, autoplay = true) {
      this.current = item;
      const {url} = await api.request(`/api/music/tracks/${item.id}/stream`, {method: 'POST'});
      if (player.current() !== item) return;
      this.audio.src = url;
      this.audio.volume = state.volume;
      if (autoplay) await this.audio.play();
    },
    pause() { this.audio.pause(); },
    resume() { return this.audio.play(); },
    seek(ms) { this.audio.currentTime = ms / 1000; },
    volume(v) { this.audio.volume = v; },
    stop() { this.audio.pause(); },
  };

  const spotify = {
    player: null, deviceId: null, ready: null, current: null, lastPos: 0, lastDuration: 0, ticking: 0, error: '',
    // Load the SDK and register this page as a Spotify Connect device, once.
    connect() {
      if (this.ready) return this.ready;
      this.ready = new Promise((resolve, reject) => {
        const fail = message => { this.error = message; this.ready = null; reject(new Error(message)); renderSpotifyState(); };
        window.onSpotifyWebPlaybackSDKReady = () => {
          const p = new window.Spotify.Player({
            name: 'SAIL Player',
            volume: state.volume,
            getOAuthToken: cb => api.request('/api/spotify/player/token', {method: 'POST'})
              .then(r => cb(r.access_token)).catch(e => fail(e.message)),
          });
          p.addListener('ready', ({device_id}) => { this.deviceId = device_id; this.error = ''; renderSpotifyState(); resolve(device_id); });
          p.addListener('not_ready', () => { this.deviceId = null; renderSpotifyState(); });
          p.addListener('initialization_error', () => fail('This browser cannot play Spotify (it lacks the DRM support Spotify requires).'));
          p.addListener('authentication_error', () => fail('Spotify sign-in for the player failed. Reconnect Spotify.'));
          p.addListener('account_error', () => fail('Spotify only plays in the browser with a Premium account.'));
          p.addListener('playback_error', ({message}) => { if (player.engine === this) player.failed(`Spotify could not play this track${message ? ` (${message})` : ''}.`); });
          p.addListener('player_state_changed', s => this.state(s));
          this.player = p;
          p.connect().then(ok => { if (!ok) fail('Spotify did not accept the player connection.'); });
        };
        if (window.Spotify?.Player) window.onSpotifyWebPlaybackSDKReady();
        else {
          const script = document.createElement('script');
          script.src = SDK_URL;
          script.onerror = () => fail('Spotify’s player could not be loaded. Check the connection.');
          document.head.append(script);
        }
      });
      return this.ready;
    },
    state(s) {
      if (player.engine !== this || !s || !this.current) return;
      this.last = s;
      const track = s.track_window?.current_track;
      const context = !!this.current.context;
      // A whole playlist (context) is ours whatever song Spotify is on; a single item must match.
      const ours = track && (context || track.uri === this.current.id || track.linked_from?.uri === this.current.id);
      // Spotify reports a finished track in previous_tracks and pauses at 0; a finished
      // playlist also has nothing left in next_tracks.
      const finished = s.paused && s.position === 0 && (context
        ? !s.track_window?.next_tracks?.length && this.lastDuration && this.lastPos > this.lastDuration - 2500
        : (s.track_window?.previous_tracks?.some(t => t.uri === this.current.id || t.linked_from?.uri === this.current.id)
          || (this.lastDuration && this.lastPos > this.lastDuration - 2500)));
      if (finished && this.started) { this.started = false; player.ended(); return; }
      if (!ours) return;
      if (context && track.uri !== this.trackUri) { this.trackUri = track.uri; player.showContextTrack(this.current, track); }
      this.started = true;
      this.lastPos = s.position; this.lastDuration = s.duration;
      player.progress(s.position, s.duration);
      player.setPlaying(!s.paused);
      clearInterval(this.ticking);
      if (!s.paused) {
        const t0 = performance.now(), p0 = s.position;
        this.ticking = setInterval(() => {
          if (player.engine !== this) return clearInterval(this.ticking);
          const pos = Math.min(s.duration, p0 + performance.now() - t0);
          this.lastPos = pos;
          player.progress(pos, s.duration);
        }, 500);
      }
    },
    async load(item, autoplay = true) {
      this.current = item; this.started = false; this.lastPos = 0; this.lastDuration = item.duration_ms; this.trackUri = null; this.last = null;
      const device = this.deviceId || await this.connect();
      if (player.current() !== item) return;
      // Activating the element inside the user gesture lets the SDK start audio (autoplay rules).
      this.player.activateElement?.();
      const body = item.context ? {device_id: device, context_uri: item.id} : {device_id: device, uris: [item.id], position_ms: 0};
      await api.request('/api/spotify/player/play', {method: 'POST', body});
      if (!autoplay) await this.player.pause();
    },
    pause() { clearInterval(this.ticking); return this.player?.pause(); },
    resume() { return this.player?.resume(); },
    seek(ms) { return this.player?.seek(Math.round(ms)); },
    volume(v) { return this.player?.setVolume(v); },
    stop() { clearInterval(this.ticking); this.started = false; return this.player?.pause(); },
  };

  // ------------------------------------------------------------------ player
  const player = {
    engine: null,
    current() { return state.queue[state.index] || null; },
    async start(items, index = 0) {
      if (!items.length) return;
      state.queue = items.slice();
      state.index = Math.max(0, Math.min(index, items.length - 1));
      await this.load();
    },
    async load(autoplay = true) {
      const item = this.current();
      if (!item) return;
      const engine = item.source === 'spotify' ? spotify : local;
      if (this.engine && this.engine !== engine) await Promise.resolve(this.engine.stop()).catch(() => {});
      this.engine = engine;
      state.position = 0; state.duration = item.duration_ms || 0;
      renderNow(); renderQueue();
      status('');
      try { await engine.load(item, autoplay); }
      catch (e) {
        if (e?.name === 'NotAllowedError') { this.setPlaying(false); status('Press play to start.'); return; }
        this.failed(e.message || 'This track could not be played.');
      }
    },
    async toggle() {
      if (!this.engine) { const first = visibleTracks(); if (first.length) await this.start(first); return; }
      try { if (state.playing) await this.engine.pause(); else await this.engine.resume(); }
      catch (e) { status(e.message, true); }
    },
    async next() {
      const item = this.current();
      if (this.engine === spotify && item?.context && spotify.last?.track_window?.next_tracks?.length) { await spotify.player.nextTrack(); return; }
      if (state.index < state.queue.length - 1) { state.index++; await this.load(); }
      else { await Promise.resolve(this.engine?.stop()); this.setPlaying(false); status('End of the queue.'); }
    },
    async previous() {
      const item = this.current();
      if (this.engine === spotify && item?.context && state.position <= 3000 && spotify.last?.track_window?.previous_tracks?.length) { await spotify.player.previousTrack(); return; }
      if (state.position > 3000 || state.index === 0) { this.seek(0); return; }
      state.index--; await this.load();
    },
    seek(ms) { this.engine?.seek(ms); this.progress(ms, state.duration); },
    // A song inside a whole-playlist item: show it, keep the playlist as the source line.
    showContextTrack(item, track) {
      const artists = (track.artists || []).map(a => a.name).join(', ');
      $('nowTitle').textContent = track.name || item.title;
      $('nowArtist').textContent = [artists, item.title].filter(Boolean).join(' · ');
      const art = track.album?.images?.[0]?.url;
      if (art) {
        const node = artNode({source: 'spotify', title: track.name || item.title, art}, 'mp-art-inner');
        $('nowArt').replaceChildren(node);
      }
      if ('mediaSession' in navigator) {
        try { navigator.mediaSession.metadata = new MediaMetadata({title: track.name || '', artist: artists, album: item.title, artwork: art ? [{src: art}] : []}); } catch (_) {}
      }
    },
    volume(v) {
      state.volume = v;
      local.volume(v); spotify.volume(v);
      try { localStorage.setItem('sail.music.volume', String(v)); } catch (_) {}
    },
    ended() { this.next(); },
    failed(message) {
      status(message, true);
      this.setPlaying(false);
    },
    setPlaying(playing) {
      state.playing = playing;
      const t = $('toggle');
      t.textContent = playing ? '⏸' : '▶';
      t.setAttribute('aria-label', playing ? 'Pause' : 'Play');
      if ('mediaSession' in navigator) navigator.mediaSession.playbackState = playing ? 'playing' : 'paused';
      document.documentElement.dataset.playing = String(playing);
    },
    progress(position, duration) {
      state.position = position || 0;
      if (duration && isFinite(duration)) state.duration = duration;
      const seek = $('seek');
      seek.max = String(Math.round(state.duration));
      if (!seek.matches(':active')) seek.value = String(Math.round(state.position));
      seek.disabled = !state.duration;
      $('elapsed').textContent = fmt(state.position);
      $('total').textContent = fmt(state.duration);
    },
  };

  // ------------------------------------------------------------------ views
  function matches(item, q) {
    return `${item.title} ${item.artist} ${item.album}`.toLocaleLowerCase().includes(q);
  }
  function visibleTracks() {
    if (state.listTracks) return state.listTracks;
    const q = state.query.toLocaleLowerCase();
    const uploads = state.view === 'spotify' ? [] : state.local.filter(t => !q || matches(t, q));
    const spot = state.view === 'local' ? [] : (state.results || []);
    return [...uploads, ...spot];
  }

  function row(item, list, index) {
    const li = document.createElement('li');
    li.className = 'mp-row';
    const playing = player.current() && player.current().id === item.id && player.current().source === item.source;
    li.classList.toggle('is-current', !!playing);
    const btn = document.createElement('button');
    btn.type = 'button'; btn.className = 'mp-row-main';
    btn.setAttribute('aria-label', `Play ${item.title} by ${item.artist}`);
    btn.addEventListener('click', () => player.start(list, index));
    const text = document.createElement('span'); text.className = 'mp-row-text';
    const title = document.createElement('strong'); title.textContent = item.title;
    const sub = document.createElement('span'); sub.className = 'mp-muted';
    sub.textContent = [item.artist, item.album].filter(Boolean).join(' · ');
    text.append(title, sub);
    const badge = document.createElement('span');
    badge.className = 'mp-badge'; badge.dataset.source = item.source;
    badge.textContent = item.source === 'spotify' ? 'Spotify' : 'Upload';
    const time = document.createElement('span'); time.className = 'mp-time'; time.textContent = fmt(item.duration_ms);
    btn.append(artNode(item), text, badge, time);
    li.append(btn);
    const add = document.createElement('button');
    add.type = 'button'; add.className = 'mp-row-tool'; add.textContent = '+';
    add.setAttribute('aria-label', `Add ${item.title} to the queue`);
    add.addEventListener('click', () => {
      state.queue.push(item);
      if (state.index < 0) state.index = 0;
      renderQueue(); status(`Added “${item.title}” to the queue.`);
    });
    li.append(add);
    if (item.source === 'local') {
      const del = document.createElement('button');
      del.type = 'button'; del.className = 'mp-row-tool'; del.textContent = '✕';
      del.setAttribute('aria-label', `Remove ${item.title} from uploads`);
      del.addEventListener('click', async () => {
        if (!window.confirm(`Remove “${item.title}” from SAIL? Your original file is not touched.`)) return;
        try { await api.request(`/api/music/tracks/${item.id}`, {method: 'DELETE'}); await loadLocal(); status(`Removed “${item.title}”.`); }
        catch (e) { status(e.message, true); }
      });
      li.append(del);
    }
    return li;
  }

  function section(title, items, list, offset, empty) {
    const wrap = document.createElement('section');
    wrap.className = 'mp-group';
    const h = document.createElement('h3'); h.textContent = title;
    const count = document.createElement('span'); count.className = 'mp-muted'; count.textContent = items.length ? `${items.length}` : '';
    h.append(' ', count);
    wrap.append(h);
    if (!items.length) {
      const p = document.createElement('p'); p.className = 'mp-empty'; p.textContent = empty; wrap.append(p);
      return wrap;
    }
    const ol = document.createElement('ol'); ol.className = 'mp-rows';
    items.forEach((item, i) => ol.append(row(item, list, offset + i)));
    wrap.append(ol);
    return wrap;
  }

  function render() {
    const lists = $('lists');
    lists.replaceChildren();
    for (const b of document.querySelectorAll('.mp-nav button')) b.setAttribute('aria-pressed', String(!state.listTracks && b.dataset.view === state.view));
    if (state.listTracks) {
      $('viewEyebrow').textContent = 'SPOTIFY PLAYLIST';
      $('viewTitle').textContent = state.listTitle;
      if (state.listContext) {
        const {p, message} = state.listContext;
        const box = document.createElement('section'); box.className = 'mp-context';
        const text = document.createElement('p'); text.className = 'mp-muted'; text.textContent = message;
        const play = document.createElement('button'); play.type = 'button'; play.className = 'mp-primary';
        play.textContent = 'Play this playlist';
        play.addEventListener('click', () => player.start([{source: 'spotify', context: true, id: p.uri, title: p.name,
          artist: p.by || 'Spotify playlist', album: 'Spotify playlist', duration_ms: 0, art: ''}]));
        box.append(text, play);
        lists.append(box);
        return;
      }
      lists.append(section('Tracks', state.listTracks, state.listTracks, 0, 'This playlist has no playable tracks.'));
      return;
    }
    const titles = {all: 'All music', local: 'Uploads', spotify: 'Spotify'};
    $('viewEyebrow').textContent = state.query ? 'SEARCH' : 'LIBRARY';
    $('viewTitle').textContent = state.query ? `“${state.query}”` : titles[state.view];
    const all = visibleTracks();
    const q = state.query.toLocaleLowerCase();
    const uploads = state.view === 'spotify' ? null : state.local.filter(t => !q || matches(t, q));
    const spot = state.view === 'local' ? null : state.results;
    if (uploads) lists.append(section('Your uploads', uploads, all, 0,
      state.query ? 'No uploads match.' : 'No uploads yet. Upload music you own with “Upload music” or drop files here.'));
    if (spot !== null && (state.query || state.view === 'spotify')) {
      const empty = !state.spotify.connected ? 'Connect Spotify from the Now Playing card in SAIL to search it here.'
        : state.query ? 'No Spotify tracks match.' : 'Search to find Spotify tracks, or open one of your playlists.';
      lists.append(section('Spotify', spot || [], all, uploads ? uploads.length : 0, empty));
    }
  }

  function renderNow() {
    const item = player.current();
    $('nowTitle').textContent = item ? item.title : 'Nothing playing';
    $('nowArtist').textContent = item ? [item.artist, item.album].filter(Boolean).join(' · ') : 'Choose a song from uploads or Spotify';
    const badge = $('nowSource');
    badge.hidden = !item;
    if (item) { badge.dataset.source = item.source; badge.textContent = item.source === 'spotify' ? 'Spotify' : 'Upload'; }
    const art = $('nowArt');
    art.replaceChildren();
    if (item) {
      const node = artNode(item, 'mp-art-inner');
      art.append(node);
      // Light the page with the cover (ambient-backdrop.js), when its colours are readable.
      coverUrl(item).then(url => {
        if (!url || player.current() !== item) return;
        const img = new Image();
        if (item.source === 'spotify') img.crossOrigin = 'anonymous';
        img.onload = () => { const p = window.SailBackdrop?.paletteFromImage(img); if (p && player.current() === item) window.SailBackdrop.current?.setLights(p); };
        img.src = url;
      });
    }
    document.querySelectorAll('.mp-row').forEach(r => r.classList.remove('is-current'));
    render();
    if ('mediaSession' in navigator && item) {
      coverUrl(item).then(url => {
        try {
          navigator.mediaSession.metadata = new MediaMetadata({title: item.title, artist: item.artist, album: item.album || '',
            artwork: url ? [{src: url, sizes: '600x600'}] : []});
        } catch (_) {}
      });
    }
  }

  function renderQueue() {
    const ol = $('queue');
    ol.replaceChildren();
    state.queue.forEach((item, i) => {
      const li = document.createElement('li');
      li.className = i === state.index ? 'is-current' : '';
      const b = document.createElement('button');
      b.type = 'button';
      b.append(artNode(item), Object.assign(document.createElement('span'), {className: 'mp-row-text', textContent: `${item.title} — ${item.artist}`}));
      b.addEventListener('click', () => { state.index = i; player.load(); });
      li.append(b);
      ol.append(li);
    });
    if (!state.queue.length) ol.append(Object.assign(document.createElement('li'), {className: 'mp-empty', textContent: 'The queue is empty.'}));
  }

  function renderSpotifyState() {
    const box = $('spotifyState');
    box.replaceChildren();
    const s = state.spotify;
    const say = (text, action, run) => {
      const p = document.createElement('p'); p.className = 'mp-muted'; p.textContent = text; box.append(p);
      if (action) { const b = document.createElement('button'); b.type = 'button'; b.textContent = action; b.addEventListener('click', run); box.append(b); }
    };
    if (!s.configured || !s.connected) say('Spotify is not connected. Set it up from the Now Playing card in SAIL.');
    else if (!s.can_stream) say('Reconnect Spotify once so it can play here, next to your uploads.', 'Reconnect Spotify', reconnect);
    else if (spotify.error) say(spotify.error);
    else if (spotify.deviceId) say('Playing in this browser as “SAIL Player”.');
  }

  async function reconnect() {
    try {
      const {url} = await api.request('/api/spotify/login', {method: 'POST'});
      window.open(url, '_blank', 'noopener');
      status('Finish signing in to Spotify in the new tab, then come back.');
    } catch (e) { status(e.message, true); }
  }

  function renderPlaylists() {
    const nav = $('playlists');
    nav.replaceChildren();
    for (const p of state.playlists) {
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = p.name;
      b.title = p.by ? `${p.name} · ${p.by}` : p.name;
      b.addEventListener('click', () => openList(p));
      nav.append(b);
    }
  }

  async function openList(p) {
    status(`Opening “${p.name}”…`);
    state.listContext = null;
    try {
      const {tracks} = await api.request(`/api/spotify/player/tracks?uri=${encodeURIComponent(p.uri)}`);
      state.listTracks = tracks; state.listTitle = p.name; render(); status('');
    } catch (e) {
      if (e.code !== 'not_owner') { status(e.message, true); return; }
      // Spotify shares the songs only of playlists you created or collaborate on; this one
      // can still be played as a whole, in Spotify's order.
      state.listTracks = []; state.listTitle = p.name; state.listContext = {p, message: e.message};
      render(); status('');
    }
  }

  // ------------------------------------------------------------------ data
  async function loadLocal() {
    try { state.local = (await api.request('/api/music/library')).tracks; }
    catch (e) { state.local = []; status(e.status === 404 ? 'Uploads are not available on this SAIL host.' : e.message, true); }
    render();
  }
  async function loadSpotify() {
    try {
      const s = await (await fetch('/api/spotify/status', {cache: 'no-store'})).json();
      state.spotify = {configured: !!s.configured, connected: !!s.connected, can_stream: !!s.can_stream};
    } catch (_) { state.spotify = {configured: false, connected: false, can_stream: false}; }
    renderSpotifyState();
    if (state.spotify.connected) {
      try { state.playlists = (await api.request('/api/spotify/player/playlists')).playlists; } catch (_) { state.playlists = []; }
      renderPlaylists();
    }
  }

  let searchSerial = 0, searchTimer = 0;
  function search(q) {
    state.query = q.trim(); state.listTracks = null;
    clearTimeout(searchTimer);
    const serial = ++searchSerial;
    if (!state.query || !state.spotify.connected || state.view === 'local') { state.results = state.query ? [] : null; render(); return; }
    render();
    searchTimer = setTimeout(async () => {
      try {
        const {tracks} = await api.request(`/api/spotify/player/search?q=${encodeURIComponent(state.query)}`);
        if (serial === searchSerial) { state.results = tracks; render(); }
      } catch (e) { if (serial === searchSerial) { state.results = []; render(); status(e.message, true); } }
    }, 350);
  }

  async function upload(files) {
    const list = [...files];
    let added = 0, failed = 0;
    for (const [i, file] of list.entries()) {
      status(`Uploading ${i + 1} of ${list.length}: ${file.name}…`);
      try {
        const r = await api.request('/api/music/import', {method: 'POST', body: file, headers: {'X-Filename': encodeURIComponent(file.name).slice(0, 500)}});
        if (r.new) added++;
      } catch (e) { failed++; status(`${file.name}: ${e.message}`, true); }
    }
    await loadLocal();
    if (!failed) status(added ? `Added ${added} ${added === 1 ? 'song' : 'songs'}.` : 'Those songs were already in your library.');
  }

  // ------------------------------------------------------------------ wiring
  function wire() {
    local.init();
    $('toggle').addEventListener('click', () => player.toggle());
    $('next').addEventListener('click', () => player.next());
    $('prev').addEventListener('click', () => player.previous());
    $('seek').addEventListener('change', e => player.seek(+e.target.value));
    $('seek').addEventListener('input', e => { $('elapsed').textContent = fmt(+e.target.value); });
    $('volume').value = String(Math.round(state.volume * 100));
    $('volume').addEventListener('input', e => player.volume(+e.target.value / 100));
    $('search').addEventListener('input', e => search(e.target.value));
    $('upload').addEventListener('change', e => { if (e.target.files.length) upload(e.target.files); e.target.value = ''; });
    $('playAll').addEventListener('click', () => player.start(visibleTracks()));
    $('shuffleAll').addEventListener('click', () => {
      const t = visibleTracks().slice();
      for (let i = t.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [t[i], t[j]] = [t[j], t[i]]; }
      player.start(t);
    });
    for (const b of document.querySelectorAll('.mp-nav button')) b.addEventListener('click', () => {
      state.view = b.dataset.view; state.listTracks = null; search($('search').value);
    });
    const panel = $('queuePanel');
    const toggleQueue = open => { panel.hidden = !open; $('queueButton').setAttribute('aria-expanded', String(open)); if (open) renderQueue(); };
    $('queueButton').addEventListener('click', () => toggleQueue(panel.hidden));
    $('queueClose').addEventListener('click', () => toggleQueue(false));
    document.addEventListener('keydown', e => {
      if (e.code !== 'Space' || e.target.closest('input, button, textarea, select, [contenteditable]')) return;
      e.preventDefault(); player.toggle();
    });
    // Drag and drop uploads.
    const main = $('player');
    main.addEventListener('dragover', e => { if ([...e.dataTransfer.types].includes('Files')) { e.preventDefault(); main.classList.add('dragging'); } });
    main.addEventListener('dragleave', e => { if (e.target === main) main.classList.remove('dragging'); });
    main.addEventListener('drop', e => { e.preventDefault(); main.classList.remove('dragging'); if (e.dataTransfer.files.length) upload(e.dataTransfer.files); });
    if ('mediaSession' in navigator) {
      const ms = navigator.mediaSession;
      const on = (a, f) => { try { ms.setActionHandler(a, f); } catch (_) {} };
      on('play', () => player.toggle()); on('pause', () => player.toggle());
      on('nexttrack', () => player.next()); on('previoustrack', () => player.previous());
      on('seekto', d => player.seek(d.seekTime * 1000));
    }
    window.addEventListener('focus', () => { if (!state.spotify.can_stream) loadSpotify(); });
  }

  wire();
  renderQueue();
  player.setPlaying(false);
  loadLocal();
  loadSpotify();
  window.SailMusic = {state, player, spotify, local};
})();
