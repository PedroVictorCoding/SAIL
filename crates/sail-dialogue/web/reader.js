'use strict';
const SailReader = (() => {
  const compare = (a, b) => a.section - b.section || a.block - b.block || a.char - b.char;
  function chapterLabel(toc, anchor, fallback) {
    let nearest = null;
    function visit(items) {
      for (const item of items) {
        const position = { section: item.section, block: item.block || 0, char: 0 };
        if (compare(position, anchor) <= 0 && (!nearest || compare(position, nearest.position) >= 0))
          nearest = { position, title: item.title };
        visit(item.children || []);
      }
    }
    visit(toc);
    return nearest?.title || fallback;
  }
  const key = book => encodeURIComponent(book.id);
  class Api {
    constructor(fetcher, storage) {
      this.fetcher = fetcher;
      this.storage = storage;
      this.token = null;
    }
    async getToken(force = false) {
      if (!force && this.token)
        return this.token;
      if (!force) {
        try {
          const t = this.storage.getItem('sail_owner_token');
          if (t?.trim().length >= 32)
            return (this.token = t.trim());
        }
        catch (_) {
        }
      }
      const response = await this.fetcher('/api/guardian/token', { cache: 'no-store' });
      if (!response.ok)
        throw new Error('Owner token unavailable. Open SAIL settings and enter the owner token.');
      this.token = (await response.json()).token;
      try {
        this.storage.setItem('sail_owner_token', this.token);
      }
      catch (_) {
      }
      return this.token;
    }
    async request(path, { method = 'GET', body, filename, keepalive = false } = {}) {
      for (let attempt = 0; attempt < 2; attempt++) {
        const token = await this.getToken(attempt > 0);
        const response = await this.fetcher(`/api/reader${path}`, {
          method, cache: 'no-store', keepalive,
          headers: {
            Authorization: `Bearer ${token}`, ...(filename ? {
              'X-Filename': /^[\x20-\x7e]+$/.test(filename)
                ? filename
                : `book.${filename.split('.').pop().toLowerCase()}`
            } : body !== undefined ? { 'Content-Type': 'application/json' } : {})
          },
          body: body === undefined ? undefined : filename ? body : JSON.stringify(body),
        });
        if (response.status === 401 && attempt === 0) {
          this.token = null;
          continue;
        }
        if (response.status === 204)
          return null;
        const data = await response.json();
        if (!response.ok) {
          const error = new Error(data.error || 'Reader request failed.');
          error.status = response.status;
          error.position = data.position;
          throw error;
        }
        return data;
      }
    }
  }
  function start() {
    const $ = id => document.getElementById(id), api = new Api(fetch.bind(window), localStorage);
    const state = {
      books: [],
      book: null,
      sections: [],
      section: null,
      page: 0,
      pages: [],
      anchor: null,
      revision: 0,
      dirty: false,
      change: 0,
      openSerial: 0,
      sectionSerial: 0,
      saveTimer: null,
      saving: null,
      conflict: null,
      narrator: null,
      context: null,
      lastSpoken: null,
      highlight: null
    };
    state.audioSerial = 0;
    const prefs = { size: 22, spacing: 1.7, width: 'comfortable', theme: 'dark' };
    try {
      const p = JSON.parse(localStorage.getItem('sail.reader.preferences'));
      if (p)
        Object.assign(prefs, p);
    }
    catch (_) {
    }
    function status(text = '', error = false) {
      $('status').textContent = text;
      $('status').classList.toggle('error', error);
    }
    function el(tag, text, className) {
      const node = document.createElement(tag);
      if (text !== undefined)
        node.textContent = text;
      if (className)
        node.className = className;
      return node;
    }
    function button(text, run) {
      const node = el('button', text);
      node.type = 'button';
      node.onclick = () => Promise.resolve().then(run).catch(() => {
        status('This action could not finish. Please try again.', true);
      });
      return node;
    }
    function updatePrefs() {
      prefs.size = Math.max(16, Math.min(36, Number(prefs.size) || 22));
      prefs.spacing = Math.max(1.3, Math.min(2.2, Number(prefs.spacing) || 1.7));
      if (!['comfortable', 'wide', 'narrow'].includes(prefs.width))
        prefs.width = 'comfortable';
      if (!['dark', 'light'].includes(prefs.theme))
        prefs.theme = 'dark';
      document.documentElement.style.setProperty('--font-size', `${prefs.size}px`);
      document.documentElement.style.setProperty('--line-spacing', prefs.spacing);
      document.documentElement.style.setProperty('--reading-width', ({
        comfortable: '1120px',
        wide: '1440px',
        narrow: '820px'
      })[prefs.width]);
      document.body.dataset.theme = prefs.theme;
      $('font-size').value = prefs.size;
      $('line-spacing').value = prefs.spacing;
      $('reading-width').value = prefs.width;
      $('theme').value = prefs.theme;
      try {
        localStorage.setItem('sail.reader.preferences', JSON.stringify(prefs));
      }
      catch (_) {
      }
    }
    function cover(book) {
      const node = button('', () => openBook(book));
      node.className = 'cover';
      node.append(el('strong', book.title), el('span', book.authors.join(', ') || 'Unknown author'));
      node.setAttribute('aria-label', `Open ${book.title}`);
      return node;
    }
    function renderLibrary() {
      $('shelf').replaceChildren();
      $('continue').replaceChildren();
      const query = $('search').value.trim().toLocaleLowerCase();
      const books = state.books.filter(book => {
        const description = `${book.title} ${book.authors.join(' ')}`;
        return description.toLocaleLowerCase().includes(query);
      });
      const recent = state.books.find(book => book.position && book.position.percent < 100);
      if (recent && !query) {
        const card = el('div', undefined, 'continue-card'), info = el('div');
        const progress = `${recent.authors.join(', ')} · ${recent.position.percent.toFixed(1)}% by text`;
        info.append(
          el('p', 'CONTINUE READING', 'eyebrow'),
          el('h2', recent.title),
          el('p', progress, 'muted')
        );
        card.append(info, button('Continue reading', () => openBook(recent)));
        $('continue').append(card);
      }
      for (const book of books) {
        const item = el('article', undefined, 'book');
        item.append(cover(book), el('p', `${book.format.toUpperCase()} · ${book.position
          ? `${book.position.percent.toFixed(1)}% by text`
          : 'Not started'}`));
        const remove = button('Remove from library', async () => {
          const question = `Remove “${book.title}” from SAIL's library? `
            + 'Your original file will remain untouched.';
          if (!window.confirm(question))
            return;
          await api.request(`/books/${key(book)}`, { method: 'DELETE' });
          await library();
        });
        remove.setAttribute('aria-label', `Remove ${book.title} from library`);
        item.append(remove);
        $('shelf').append(item);
      }
      $('empty').hidden = books.length > 0;
      $('empty').textContent = state.books.length
        ? 'No books match this search.'
        : 'Your library is empty. Import a book to begin, or drop an EPUB or text file here.';
    }
    async function library() {
      status('Loading library…');
      try {
        state.books = (await api.request('/library')).books;
        renderLibrary();
        status();
      }
      catch (error) {
        status(error.message, true);
      }
    }
    async function importBook(file) {
      if (!file)
        return;
      if (!/\.(epub|txt)$/i.test(file.name)) {
        status('Choose a DRM-free EPUB or UTF-8 text file.', true);
        return;
      }
      if (file.size > 50 * 1024 * 1024) {
        status('Books must be 50 MB or smaller.', true);
        return;
      }
      $('import').disabled = true;
      status('Importing book…');
      try {
        await api.request('/import', { method: 'POST', body: file, filename: file.name });
        await library();
        status('Book added to your library.');
      }
      catch (error) {
        status(error.message, true);
      }
      finally {
        $('import').disabled = false;
        $('import').value = '';
      }
    }
    function currentAnchor() {
      return state.anchor
        || state.pages[state.page]
        || {
        section: state.section?.index
          || 0,
        block: 0,
        char: 0
      };
    }
    function sourceAnchor() {
      return state.lastSpoken
        && state.narrator?.follow
        && ['playing',
          'buffering',
          'paused',
          'completed'].includes(state.narrator?.state)
        ? state.lastSpoken
        : currentAnchor();
    }
    function metrics() {
      const viewport = $('page-viewport'), style = getComputedStyle($('pages'));
      const columns = Number.parseInt(style.columnCount, 10)
        || 1, gap = Number.parseFloat(style.columnGap)
          || 0;
      return {
        left: viewport.getBoundingClientRect().left,
        width: viewport.clientWidth,
        stride: viewport.clientWidth + gap,
        columns,
        gap
      };
    }
    function charPage(block, char, m = metrics()) {
      const node = $('pages').children[block]?.firstChild;
      if (!node)
        return 0;
      const range = document.createRange();
      range.setStart(node, Math.min(char, node.length));
      range.setEnd(node, Math.min(char + 1, node.length));
      const rect = range.getClientRects()[0] || range.getBoundingClientRect();
      return Math.max(0, Math.floor((rect.left - m.left + state.page * m.stride + .5) / m.stride));
    }
    function pageFor(anchor) {
      let page = 0;
      for (let i = 0; i < state.pages.length; i++)
        if (compare(state.pages[i], anchor) <= 0)
          page = i;
        else
          break;
      return page;
    }
    function measure(anchor) {
      if (!state.section || $('reader').hidden)
        return;
      state.page = 0;
      $('pages').style.transform = 'translateX(0)';
      state.layoutSignature = layoutSignature();
      const m = metrics();
      if (!m.width)
        return;
      const found = [{ section: state.section.index, block: 0, char: 0 }];
      state.section.blocks.forEach((block, b) => {
        if (!block.text.length)
          return;
        let start = 0, page = charPage(b, 0, m);
        if (!found[page])
          found[page] = { section: state.section.index, block: b, char: 0 };
        while (start < block.text.length) {
          let lo = start + 1, hi = block.text.length;
          while (lo < hi) {
            const mid = Math.floor((lo + hi) / 2);
            if (charPage(b, mid, m) > page)
              hi = mid;
            else
              lo = mid + 1;
          }
          if (lo >= block.text.length)
            break;
          let next = lo;
          if (/[\uDC00-\uDFFF]/.test(block.text[next]) && /[\uD800-\uDBFF]/.test(block.text[next - 1]))
            next--;
          const nextPage = charPage(b, lo, m);
          if (nextPage <= page)
            break;
          if (!found[nextPage])
            found[nextPage] = { section: state.section.index, block: b, char: next };
          start = lo;
          page = nextPage;
        }
      });
      state.pages = Array.from({ length: found.length }, (_, i) => found[i]
        || found.slice(0, i).reverse().find(Boolean));
      showPage(pageFor(anchor || currentAnchor()), false);
    }
    function showPage(page, save = true) {
      state.page = Math.max(0, Math.min(page, state.pages.length - 1));
      $('pages').style.transform = `translateX(${-state.page * metrics().stride}px)`;
      const anchor = currentAnchor();
      const sectionLabel = chapterLabel(state.toc || [], anchor,
        state.section.title || `Section ${state.section.index + 1}`);
      const progress = `${percent(anchor).toFixed(1)}%`;
      $('page-label').replaceChildren(document.createTextNode(`${sectionLabel} · ${progress}`),
        el('span', ` · ${state.page + 1} / ${state.pages.length} spreads · by text`, 'page-detail'));
      $('page-label').title = `${sectionLabel} · ${progress} by text`;
      $('previous-page').disabled = state.page === 0 && state.section.index === 0;
      $('next-page').disabled = state.page === state.pages.length - 1
        && state.section.index === state.sections.length - 1;
      if (save) {
        state.anchor = state.pages[state.page];
        markDirty();
      }
    }
    function percent(anchor) {
      if (!state.book?.chars)
        return 0;
      const before = state.sections.filter(s => s.index < anchor.section).reduce((n, s) => n + s.chars, 0);
      const spoken = state.section?.index === anchor.section
        ? state.section : preparedSections.get(anchor.section)?.section;
      const blocks = spoken?.blocks || [];
      const priorChars = blocks.slice(0, anchor.block).reduce((n, block) => n + Array.from(block.text).length, 0);
      const blockChars = Array.from((blocks[anchor.block]?.text || '').slice(0, anchor.char)).length;
      const here = priorChars + blockChars;
      return Math.max(0, Math.min(100, (before + here) / state.book.chars * 100));
    }
    function markDirty() {
      state.dirty = true;
      state.change++;
      clearTimeout(state.saveTimer);
      state.saveTimer = setTimeout(() => savePosition(), 650);
    }
    async function savePosition() {
      clearTimeout(state.saveTimer);
      if (!state.book || !state.section || !state.dirty || state.conflict)
        return;
      if (state.saving) {
        await state.saving;
        if (state.dirty && !state.conflict)
          return savePosition();
        return;
      }
      const book = state.book, anchor = sourceAnchor(), change = state.change;
      const task = async () => {
        try {
          const { position } = await api.request(`/books/${key(book)}/position`, {
            method: 'PUT',
            body: {
              anchor,
              percent: percent(anchor),
              expected_revision: state.revision
            },
            keepalive: true
          });
          if (state.book !== book)
            return;
          state.revision = position.revision;
          book.position = position;
          if (state.change === change)
            state.dirty = false;
        }
        catch (error) {
          if (state.book !== book)
            return;
          if (error.status === 409) {
            state.conflict = error.position;
            state.narrator?.pause();
            $('conflict').hidden = false;
            status('Opened elsewhere — choose a position before saving.', true);
          }
          else
            status('Position not saved. Reconnect to SAIL and try again.', true);
        }
      };
      state.saving = task();
      await state.saving;
      state.saving = null;
    }
    function tocList(items) {
      const list = el('ul');
      for (const item of items) {
        const li = el('li');
        li.append(button(item.title || `Section ${item.section + 1}`, async () => {
          await navigateSection(item.section, { section: item.section, block: item.block || 0, char: 0 });
          toggleDrawer('toc', false);
        }));
        if (item.children?.length)
          li.append(tocList(item.children));
        list.append(li);
      }
      return list;
    }
    async function openBook(book) {
      await savePosition();
      if (state.conflict)
        return;
      stopAudio();
      const serial = ++state.openSerial;
      status('Opening book…');
      try {
        const data = await api.request(`/books/${key(book)}`);
        if (serial !== state.openSerial)
          return;
        const saved = await api.request(`/books/${key(book)}/position`);
        if (serial !== state.openSerial)
          return;
        state.book = data.book;
        state.sections = data.sections;
        state.toc = data.toc;
        $('reader-actions').hidden = false;
        syncToolbar();
        state.revision = saved.position?.revision || 0;
        state.dirty = false;
        $('book-title').textContent = data.book.title;
        document.title = `${data.book.title} · SAIL Reader`;
        $('library').hidden = true;
        $('reader').hidden = false;
        for (const id of ['toc-button', 'appearance-button', 'read-aloud'])
          $(id).hidden = false;
        $('toc-list').replaceChildren(tocList(data.toc.length
          ? data.toc
          : data.sections.map(s => ({
            title: s.title,
            section: s.index,
            children: []
          }))));
        await loadSection(saved.position?.anchor?.section || 0, saved.position?.anchor);
        if (serial === state.openSerial) {
          status();
          $('page-viewport').focus();
        }
      }
      catch (error) {
        if (serial === state.openSerial)
          status(error.message, true);
      }
    }
    function renderSection(section, anchor) {
      state.section = section;
      state.lastSpoken = null;
      state.anchor = anchor || { section: section.index, block: 0, char: 0 };
      const fragment = document.createDocumentFragment();
      const tags = {
        quote: 'blockquote',
        p: 'p',
        li: 'li',
        pre: 'pre',
        hr: 'hr',
        h1: 'h1',
        h2: 'h2',
        h3: 'h3',
        h4: 'h4',
        h5: 'h5',
        h6: 'h6'
      };
      for (const block of section.blocks)
        fragment.append(el(tags[block.kind] || 'p', block.text));
      $('pages').replaceChildren(fragment);
    }
    async function loadSection(index, anchor) {
      const book = state.book, serial = ++state.sectionSerial;
      const section = await api.request(`/books/${key(book)}/sections/${index}`);
      if (book !== state.book || serial !== state.sectionSerial)
        return false;
      renderSection(section, anchor);
      measure(anchor || { section: index, block: 0, char: 0 });
      return true;
    }
    // Measure the next section synchronously and restore the visible DOM before paint.
    // Only playback start installs its prepared pages, so fetching cannot turn a page.
    const preparedSections = new Map();
    async function nextNarrationSection(last, generation) {
      const narrator = state.narrator, book = state.book;
      let index = last.anchor.section + 1;
      while (index < state.sections.length) {
        const section = await api.request(`/books/${key(book)}/sections/${index}`);
        if (state.book !== book
          || state.narrator !== narrator
          || narrator.generation !== generation)
          return [];
        const previous = {
          section: state.section,
          pages: state.pages,
          page: state.page,
          anchor: state.anchor,
          lastSpoken: state.lastSpoken
        };
        const nodes = [...$('pages').childNodes], transform = $('pages').style.transform;
        const label = $('page-label').textContent;
        const disabled = [$('previous-page').disabled, $('next-page').disabled];
        let boundaries;
        try {
          renderSection(section);
          measure({ section: index, block: 0, char: 0 });
          boundaries = state.pages;
        }
        finally {
          Object.assign(state, previous);
          $('pages').replaceChildren(...nodes);
          $('pages').style.transform = transform;
          $('page-label').textContent = label;
          $('previous-page').disabled = disabled[0];
          $('next-page').disabled = disabled[1];
        }
        const segments = SailReaderAudio.segment(section.blocks, boundaries, 320, index);
        if (segments.length) {
          for (const key of preparedSections.keys())
            if (key < last.anchor.section)
              preparedSections.delete(key);
          preparedSections.set(index, { section, boundaries });
          return segments;
        }
        index++;
      }
      return [];
    }
    function manual() {
      state.narrator?.manualNavigation();
      $('follow').hidden = !state.narrator;
    }
    async function navigateSection(index, anchor) {
      if (!state.book || index < 0 || index >= state.sections.length)
        return;
      manual();
      stopAudio();
      await savePosition();
      if (state.conflict)
        return;
      if (await loadSection(index, anchor))
        markDirty();
    }
    async function turn(delta) {
      if (!state.section)
        return;
      manual();
      if (state.page + delta < 0) {
        if (state.section.index > 0) {
          await navigateSection(state.section.index - 1);
          showPage(state.pages.length - 1);
        }
      }
      else if (state.page + delta >= state.pages.length)
        await navigateSection(state.section.index + 1);
      else
        showPage(state.page + delta);
    }
    function clearHighlight() {
      if (!state.highlight)
        return;
      const parent = state.highlight.parentNode;
      state.highlight.replaceWith(document.createTextNode(state.highlight.textContent));
      parent?.normalize();
      state.highlight = null;
    }
    function highlight(segment) {
      clearHighlight();
      const block = $('pages').children[segment.anchor.block];
      if (!block?.firstChild)
        return;
      const range = document.createRange();
      range.setStart(block.firstChild, segment.anchor.char);
      range.setEnd(block.firstChild, segment.end);
      const mark = document.createElement('mark');
      range.surroundContents(mark);
      state.highlight = mark;
    }
    let voiceRouter, deviceVoices = [];
    const stored = (key, fallback) => {
      try { return localStorage.getItem(`sail.reader.${key}`) || fallback; }
      catch (_) { return fallback; }
    };
    const persist = (key, value) => {
      try { localStorage.setItem(`sail.reader.${key}`, value); } catch (_) {}
    };
    const speeds = [0.8, 0.9, 1, 1.1, 1.25, 1.5];
    let speed = Number(stored('speed', '1'));
    if (!speeds.includes(speed)) speed = 1;
    $('speed').value = String(speed);
    function syncVoiceControls(message = '') {
      const host = voiceRouter.backend === 'host';
      $('backend').value = host ? 'host' : 'device';
      $('backend').querySelector('option[value="host"]').disabled = !voiceRouter.capabilities.available;
      $('speed-control').hidden = !host;
      const voices = host ? voiceRouter.capabilities.voices : deviceVoices;
      if (!voices?.length) throw new Error('No voice available.');
      const preferred = state.narrator?.voice || stored('voice', 'af_heart');
      $('voice').replaceChildren(...voices.map(v => {
        const option = el('option', v);
        option.value = v;
        return option;
      }));
      $('voice').value = voices.includes(preferred) ? preferred
        : host && voices.includes(voiceRouter.capabilities.default_voice)
          ? voiceRouter.capabilities.default_voice : voices[0];
      if (state.narrator) state.narrator.voice = $('voice').value;
      if (message) $('narration-note').textContent = message;
      persist('backend', voiceRouter.backend);
    }
    function audioState(value) {
      const backend = voiceRouter?.backend === 'host' ? 'SAIL host' : 'this device (WebAssembly)';
      $('audio-status').textContent = ({
        tts_loading: `Loading voice · ${backend}…`,
        buffering: state.narrator?.hasPlayed ? 'Waiting for the voice…' : 'Preparing audio…',
        playing: `Playing · ${backend}`,
        paused: `Paused · ${backend}`,
        completed: 'Finished',
        seeking: 'Seeking…',
        error: 'Read-aloud unavailable. Stop and try again.',
        canceled: 'Stopped'
      })[value] || 'Text ready';
      if (value === 'error')
        $('read-aloud').disabled = true;
      $('play-pause').textContent = ['playing',
        'buffering',
        'tts_loading'].includes(value)
        ? 'Pause'
        : 'Play';
      if (value === 'completed' && state.lastSpoken) {
        const spoken = preparedSections.get(state.lastSpoken.section)?.section || state.section;
        const block = spoken.blocks.length - 1;
        state.lastSpoken = { section: spoken.index, block, char: spoken.blocks[block].text.length };
        markDirty();
      }
    }
    function setupNarration(anchor) {
      clearHighlight();
      preparedSections.clear();
      const segments = SailReaderAudio.segment(state.section.blocks, state.pages, 320, state.section.index);
      let index = segments.findIndex(s => compare(s.anchor, anchor) >= 0
        || (s.anchor.block === anchor.block
          && s.end > anchor.char));
      if (index < 0)
        index = segments.length;
      const narrator = state.narrator;
      narrator.setText(segments);
      if (index < segments.length
        && segments[index].anchor.block === anchor.block
        && segments[index].anchor.char < anchor.char) {
        const s = segments[index];
        segments[index] = { ...s, text: s.text.slice(anchor.char - s.anchor.char), anchor: { ...anchor } };
      }
      narrator.index = index;
    }
    async function loadReaderVoice() {
      await voiceRouter.load();
      syncVoiceControls();
      $('voice-note').textContent = `Book language: ${state.book.language || 'not specified'}. `
        + 'Choose a matching voice; pronunciation is not automatically verified.';
    }
    async function chooseEngine() {
      let capabilities;
      try { capabilities = await api.request('/speech'); }
      catch (_) { capabilities = { available: false }; }
      const device = {
        load: async () => {
          const loaded = await SailClientSpeech.loadTts();
          deviceVoices = loaded.voices;
        },
        speak: (text, voice) => SailClientSpeech.speak(text,
          deviceVoices.includes(voice) ? voice : deviceVoices[0])
      };
      voiceRouter = new SailReaderAudio.VoiceRouter({
        host: new SailReaderAudio.HostEngine({
          fetcher: fetch.bind(window), getToken: () => api.getToken(), speed
        }),
        device, capabilities, preference: stored('backend', 'host'),
        onChange: (_, message) => syncVoiceControls(message)
      });
      $('backend').querySelector('option[value="host"]').disabled = !capabilities.available;
      await loadReaderVoice();
    }
    async function readAloud() {
      if (!state.section)
        return;
      if (state.narrator) {
        $('audio').hidden = false;
        return state.narrator.play();
      }
      const Context = window.AudioContext || window.webkitAudioContext;
      if (!Context) {
        status('Read-aloud is unavailable in this browser.', true);
        $('read-aloud').disabled = true;
        return;
      }
      try {
        state.context ||= new Context();
        await state.context.resume();
        const anchor = currentAnchor(), book = state.book, section = state.section;
        const serial = ++state.audioSerial;
        $('audio').hidden = false;
        document.body.classList.add('narrating');
        audioState('tts_loading');
        await chooseEngine();
        if (serial !== state.audioSerial || state.book !== book || state.section !== section) return;
        fitReadingSurface();
        measure(anchor);
        state.narrator = new SailReaderAudio.Narrator({
          tts: voiceRouter,
          prefetch: 2,
          clock: new SailReaderAudio.AudioClock(state.context, () => {
            state.narrator?.cancel();
            audioState('error');
          }),
          onState: audioState,
          onSlow: () => {
            if (voiceRouter.backend !== 'device') return;
            $('narration-note').textContent = 'This device speaks slower than real time, '
              + 'so narration will pause between sentences.';
          },
          loadMore: nextNarrationSection,
          onSegment: (segment, follow) => {
            if (follow && segment.anchor.section !== state.section.index) {
              const prepared = preparedSections.get(segment.anchor.section);
              if (!prepared)
                return;
              clearHighlight();
              renderSection(prepared.section, segment.anchor);
              state.pages = prepared.boundaries;
              state.page = 0;
            }
            state.lastSpoken = segment.anchor;
            if (segment.anchor.section === state.section.index)
              highlight(segment);
            else
              clearHighlight();
            if (follow) {
              state.anchor = segment.anchor;
              showPage(pageFor(segment.anchor), false);
            }
            markDirty();
          },
        });
        state.narrator.voice = $('voice').value;
        setupNarration(anchor);
        $('audio').hidden = false;
        document.body.classList.add('narrating');
        measure(anchor);
        setupNarration(anchor);
        state.channel?.postMessage('playing');
        await state.narrator.play();
      }
      catch (_) {
        status('Read-aloud is unavailable. Check the SAIL host or local voice assets.', true);
        audioState('error');
        $('read-aloud').disabled = true;
      }
    }
    function stopAudio() {
      state.audioSerial++;
      state.narrator?.cancel();
      state.narrator = null;
      $('auto-turn').setAttribute('aria-pressed', 'true');
      $('auto-turn').textContent = 'Auto turn: on';
      state.lastSpoken = null;
      clearHighlight();
      $('audio').hidden = true;
      $('follow').hidden = true;
      document.body.classList.remove('narrating', 'audio-minimised');
      $('audio-controls').hidden = false;
      $('minimise-audio').textContent = 'Minimise';
      $('minimise-audio').setAttribute('aria-expanded', 'true');
    }
    function toggleDrawer(id, visible) {
      $(id).hidden = !visible;
      $(`${id === 'toc' ? 'toc' : 'appearance'}-button`).setAttribute('aria-expanded', String(visible));
      if (visible)
        $(id).querySelector('button,input,select')?.focus();
      else
        $(`${id}-button`).focus();
    }
    const phoneToolbar = window.matchMedia('(max-width: 600px)');
    const syncToolbar = () => { $('reader-actions').open = !phoneToolbar.matches; };
    phoneToolbar.addEventListener('change', syncToolbar);
    syncToolbar();
    $('reader-actions').addEventListener('click', event => {
      if (phoneToolbar.matches && event.target.closest('button'))
        $('reader-actions').open = false;
    });
    $('search').oninput = renderLibrary;
    $('import').onchange = event => importBook(event.target.files[0]);
    for (const type of ['dragenter', 'dragover'])
      document.addEventListener(type, event => {
        if ($('library').hidden)
          return;
        event.preventDefault();
        document.body.classList.add('dragging');
      });
    document.addEventListener('dragleave', event => {
      if (!event.relatedTarget)
        document.body.classList.remove('dragging');
    });
    document.addEventListener('drop', event => {
      event.preventDefault();
      document.body.classList.remove('dragging');
      if (!$('library').hidden)
        importBook(event.dataTransfer.files[0]);
    });
    $('library-button').onclick = async () => {
      await savePosition();
      if (state.conflict)
        return;
      ++state.openSerial;
      ++state.sectionSerial;
      stopAudio();
      state.book = null;
      state.section = null;
      $('reader').hidden = true;
      $('library').hidden = false;
      for (const id of ['toc', 'appearance', 'toc-button', 'appearance-button', 'read-aloud'])
        $(id).hidden = true;
      document.title = 'Reader · SAIL';
      $('book-title').textContent = '';
      $('reader-actions').hidden = true;
      $('reader-actions').open = false;
      await library();
    };
    for (const id of ['toc', 'appearance']) {
      $(`${id}-button`).onclick = () => toggleDrawer(id, $(id).hidden);
      $(`${id}-close`).onclick = () => toggleDrawer(id, false);
    }
    $('previous-page').onclick = () => turn(-1).catch(() => status('Could not open this section.', true));
    $('next-page').onclick = () => turn(1).catch(() => status('Could not open this section.', true));
    document.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        if (phoneToolbar.matches && $('reader-actions').open) {
          $('reader-actions').open = false;
          $('reader-actions').querySelector('summary').focus();
        }
        for (const id of ['toc', 'appearance'])
          if (!$(id).hidden)
            toggleDrawer(id, false);
        return;
      }
      if ($('reader').hidden || document.activeElement.isContentEditable)
        return;
      if (['INPUT',
        'SELECT',
        'TEXTAREA',
        'BUTTON'].includes(document.activeElement.tagName)
        || (phoneToolbar.matches && $('reader-actions').open)
        || !$('toc').hidden
        || !$('appearance').hidden
        || !window.getSelection().isCollapsed)
        return;
      if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
        event.preventDefault();
        turn(event.key === 'ArrowRight' ? 1 : -1).catch(() => status('Could not turn page.', true));
      }
    });
    let touch = null;
    $('page-viewport').addEventListener('pointerdown', event => {
      if (event.isPrimary)
        touch = { x: event.clientX, y: event.clientY, id: event.pointerId, time: performance.now() };
    });
    $('page-viewport').addEventListener('pointercancel', () => {
      touch = null;
    });
    $('page-viewport').addEventListener('pointerup', event => {
      if (!touch || touch.id !== event.pointerId)
        return;
      const first = touch;
      touch = null;
      if (!window.getSelection().isCollapsed || performance.now() - first.time > 700)
        return;
      const dx = event.clientX - first.x, dy = event.clientY - first.y;
      const rect = $('page-viewport').getBoundingClientRect();
      let delta = 0;
      if (Math.abs(dx) > 50 && Math.abs(dx) > Math.abs(dy) * 1.5)
        delta = dx < 0 ? 1 : -1;
      else if (Math.abs(dx) < 10 && Math.abs(dy) < 10) {
        if (event.clientX < rect.left + rect.width * .15)
          delta = -1;
        else if (event.clientX > rect.right - rect.width * .15)
          delta = 1;
      }
      if (delta)
        turn(delta).catch(() => status('Could not turn page.', true));
    });
    // Reserve the measured controller height, including mobile wrapping, so its
    // controls never cover the last lines or the page navigation.
    function fitReadingSurface() {
      if ($('reader').hidden)
        return;
      const top = $('reader').getBoundingClientRect().top;
      $('reader').style.height = `${Math.max(180, window.innerHeight - top - 12)}px`;
      const space = $('audio').hidden ? 0 : $('audio').offsetHeight + 24;
      $('reader').style.setProperty('--audio-space', `${space}px`);
    }
    function layoutSignature() {
      return [$('page-viewport').clientWidth, $('page-viewport').clientHeight, prefs.size, prefs.spacing, prefs.width].join(':');
    }
    let reflowTimer;
    function reflow() {
      clearTimeout(reflowTimer);
      reflowTimer = setTimeout(() => {
        if (!state.section || $('reader').hidden)
          return;
        fitReadingSurface();
        if (state.layoutSignature === layoutSignature()) return;
        const anchor = sourceAnchor(), narrator = state.narrator, wasActive = narrator
          && ['playing',
            'buffering',
            'tts_loading'].includes(narrator.state);
        clearHighlight();
        measure(anchor.section === state.section.index ? anchor : currentAnchor());
        // Re-segment against new measured boundaries. Resume at the source anchor,
        // restarting only the current segment; synthesized positions never count as read.
        const spoken = narrator?.segments[narrator.index]?.anchor;
        if (spoken && spoken.section !== state.section.index) return;
        if (narrator) {
          const follow = narrator.follow;
          setupNarration(anchor);
          narrator.follow = follow;
          if (wasActive)
            narrator.play();
          else
            narrator.setState('paused');
        }
      }, 120);
    }
    for (const [id, prop] of [
      ['font-size', 'size'], ['line-spacing', 'spacing'], ['reading-width', 'width'], ['theme', 'theme']
    ])
      $(id).oninput = event => {
        prefs[prop] = event.target.value;
        updatePrefs();
        reflow();
      };
    const layoutObserver = new ResizeObserver(reflow);
    for (const id of ['page-viewport', 'audio', 'status', 'conflict'])
      layoutObserver.observe($(id));
    window.addEventListener('resize', reflow);
    document.fonts?.ready.then(reflow);
    $('read-aloud').onclick = readAloud;
    $('play-pause').onclick = () => {
      const narrator = state.narrator;
      if (!narrator)
        return;
      if (['playing', 'buffering', 'tts_loading'].includes(narrator.state))
        narrator.pause();
      else {
        state.context.resume().catch(() => audioState('error'));
        state.channel?.postMessage('playing');
        narrator.play();
      }
    };
    for (const [id, delta] of [['previous-sentence', -1], ['next-sentence', 1]])
      $(id).onclick = () => {
        const n = state.narrator;
        if (n)
          n.seek(n.index + delta, n.state !== 'paused');
      };
    $('backend').onchange = async event => {
      if (!state.narrator) return;
      const narrator = state.narrator, autoplay = narrator.state !== 'paused';
      narrator.pause();
      narrator.cancel();
      try {
        await voiceRouter.select(event.target.value);
        await narrator.seek(narrator.index, autoplay);
      }
      catch (_) { audioState('error'); }
    };
    $('speed').onchange = event => {
      speed = Number(event.target.value);
      persist('speed', String(speed));
      state.narrator?.setSpeed(speed);
    };
    $('voice').onchange = event => {
      try {
        localStorage.setItem('sail.reader.voice', event.target.value);
      }
      catch (_) {
      }
      state.narrator?.setVoice(event.target.value);
    };
    $('auto-turn').onclick = () => {
      const n = state.narrator;
      if (!n)
        return;
      n.auto = !n.auto;
      $('auto-turn').setAttribute('aria-pressed', String(n.auto));
      $('auto-turn').textContent = `Auto turn: ${n.auto ? 'on' : 'off'}`;
    };
    $('follow').onclick = () => {
      const n = state.narrator;
      if (!n)
        return;
      n.follow = true;
      $('follow').hidden = true;
      if (state.lastSpoken && n.state === 'playing') {
        const anchor = state.lastSpoken;
        const prepared = preparedSections.get(anchor.section);
        if (prepared && state.section.index !== anchor.section) {
          clearHighlight();
          renderSection(prepared.section, anchor);
          state.pages = prepared.boundaries;
          state.page = 0;
        }
        state.lastSpoken = anchor;
        state.anchor = anchor;
        showPage(pageFor(anchor), false);
      }
    };
    $('stop-audio').onclick = () => {
      const anchor = sourceAnchor();
      stopAudio();
      measure(anchor);
      markDirty();
    };
    $('minimise-audio').onclick = () => {
      const hidden = !$('audio-controls').hidden;
      $('audio-controls').hidden = hidden;
      document.body.classList.toggle('audio-minimised', hidden);
      $('minimise-audio').textContent = hidden ? 'Expand' : 'Minimise';
      $('minimise-audio').setAttribute('aria-expanded', String(!hidden));
    };
    $('keep-here').onclick = async () => {
      state.revision = state.conflict?.revision || 0;
      state.conflict = null;
      $('conflict').hidden = true;
      state.dirty = true;
      await savePosition();
      if (!state.conflict)
        status('Position saved here.');
    };
    $('resume-there').onclick = async () => {
      const remote = state.conflict;
      if (!remote)
        return;
      stopAudio();
      state.revision = remote.revision;
      state.conflict = null;
      state.dirty = false;
      $('conflict').hidden = true;
      try {
        await loadSection(remote.anchor.section, remote.anchor);
        status('Resumed saved position.');
      }
      catch (_) {
        status('Could not load saved position.', true);
      }
    };
    // Hiding an iframe does not fire document.visibilitychange. The workspace
    // explicitly pauses and checkpoints it; returning never auto-resumes audio.
    if (window.parent !== window) {
      document.querySelector('.toolbar > a')?.remove();
      window.addEventListener('message', event => {
        if (event.origin !== location.origin || event.source !== window.parent) return;
        if (event.data?.type !== 'sail:reader-visibility' || event.data.visible !== false) return;
        if (state.narrator) state.narrator.pause();
        else stopAudio(); // Discard a still-loading Read Aloud action.
        savePosition();
      });
    }
    document.addEventListener('visibilitychange', () => {
      if (document.hidden) {
        state.narrator?.pause();
        savePosition();
      }
    });
    window.addEventListener('pagehide', () => {
      savePosition();
      state.narrator?.cancel();
    });
    window.addEventListener('online', () => savePosition());
    if (typeof BroadcastChannel !== 'undefined') {
      state.channel = new BroadcastChannel('sail-reader-audio');
      state.channel.onmessage = event => {
        if (event.data === 'playing') {
          state.narrator?.pause();
          if (state.narrator)
            status('Narration paused because another reader tab started playback.');
        }
      };
    }
    updatePrefs();
    library();
  }
  return { Api, compare, chapterLabel, start };
})();
if (typeof module !== 'undefined')
  module.exports = SailReader;
if (typeof document !== 'undefined')
  SailReader.start();
