// Minimal, dependency-free Markdown renderer for SAIL conversation messages.
//
// Builds DOM nodes directly (never innerHTML), so model output cannot inject
// markup or script. Supports the subset language models commonly emit:
// headings, paragraphs, emphasis, strikethrough, inline code, fenced code,
// ordered/unordered (nested) lists, blockquotes, tables, horizontal rules and
// http(s)/mailto links. Incomplete constructs (e.g. an unterminated code fence
// while a reply is still streaming) render gracefully.
(function (global) {
  'use strict';

  const LIST_RE = /^( *)([-*+]|\d{1,9}[.)])( +|$)(.*)$/;
  const FENCE_RE = /^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)[^`]*$/;
  const HEADING_RE = /^ {0,3}(#{1,6})(?:\s+(.*?))?\s*$/;
  const HR_RE = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
  const QUOTE_RE = /^ {0,3}>/;
  const TABLE_SEP_RE = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;
  const ESCAPABLE = /[\\`*_{}[\]()#+\-.!~|>]/;

  function el(doc, tag, className) {
    const node = doc.createElement(tag);
    if (className) node.className = className;
    return node;
  }

  function safeHref(url) {
    try {
      const parsed = new URL(url.trim());
      return ['http:', 'https:', 'mailto:'].includes(parsed.protocol) ? parsed.href : null;
    } catch {
      return null;
    }
  }

  // `label` is Markdown for [label](url) links, or plain text for autolinks.
  function appendLink(doc, parent, label, url, plainLabel = false) {
    const href = safeHref(url);
    const fill = (target) => plainLabel ? target.append(doc.createTextNode(label)) : renderInline(doc, label, target);
    if (!href) { fill(parent); return; }
    const link = el(doc, 'a');
    link.href = href; link.target = '_blank'; link.rel = 'noopener noreferrer';
    fill(link);
    parent.append(link);
  }

  // [label](url "optional title") starting at text[start] === '['.
  function matchLink(text, start) {
    let depth = 0, i = start;
    for (; i < text.length; i++) {
      if (text[i] === '\\') { i++; continue; }
      if (text[i] === '[') depth++;
      else if (text[i] === ']' && --depth === 0) break;
    }
    if (i >= text.length || text[i + 1] !== '(') return null;
    const label = text.slice(start + 1, i);
    let parens = 0, j = i + 2;
    for (; j < text.length; j++) {
      if (text[j] === '\n') return null;
      if (text[j] === '(') parens++;
      else if (text[j] === ')' && parens-- === 0) break;
    }
    if (j >= text.length) return null;
    const url = text.slice(i + 2, j).trim().split(/\s+/)[0].replace(/^<(.*)>$/, '$1');
    return { label, url, end: j + 1 };
  }

  const isWord = (ch) => ch !== undefined && /[\p{L}\p{N}]/u.test(ch);
  const isSpace = (ch) => ch === undefined || /\s/.test(ch);

  // Emphasis/strong/strikethrough opening at text[start].
  function matchEmphasis(text, start) {
    const ch = text[start];
    let run = 0;
    while (text[start + run] === ch) run++;
    if (ch === '~' && run !== 2) return null;
    if (run > 3) return null;
    // `_` must not open inside a word (snake_case identifiers).
    if (ch === '_' && isWord(text[start - 1])) return null;
    if (isSpace(text[start + run])) return null;
    const delim = ch.repeat(run);
    let j = start + run;
    while ((j = text.indexOf(delim, j + 1)) !== -1) {
      if (isSpace(text[j - 1])) continue;
      if (text[j + run] === ch || text[j - 1] === ch) continue;
      if (ch === '_' && isWord(text[j + run])) continue;
      const inner = text.slice(start + run, j);
      if (inner.includes('\n\n')) return null;
      const tags = ch === '~' ? ['del'] : run === 1 ? ['em'] : run === 2 ? ['strong'] : ['strong', 'em'];
      return { tags, inner, end: j + run };
    }
    return null;
  }

  function renderInline(doc, text, parent) {
    let buffer = '';
    const flush = () => { if (buffer) { parent.append(doc.createTextNode(buffer)); buffer = ''; } };
    let i = 0;
    while (i < text.length) {
      const ch = text[i];
      if (ch === '\\' && ESCAPABLE.test(text[i + 1] || '')) { buffer += text[i + 1]; i += 2; continue; }
      if (ch === '\n') { flush(); parent.append(el(doc, 'br')); i++; continue; }
      if (ch === '`') {
        let n = 1; while (text[i + n] === '`') n++;
        const fence = '`'.repeat(n);
        const end = text.indexOf(fence, i + n);
        if (end !== -1) {
          flush();
          const code = el(doc, 'code');
          let body = text.slice(i + n, end).replace(/\n/g, ' ');
          if (/^ .*[^ ].* $/.test(body)) body = body.slice(1, -1);
          code.textContent = body;
          parent.append(code); i = end + n; continue;
        }
        buffer += fence; i += n; continue;
      }
      if (ch === '[') {
        const link = matchLink(text, i);
        if (link) { flush(); appendLink(doc, parent, link.label, link.url); i = link.end; continue; }
      }
      if (ch === '<') {
        const auto = /^<((?:https?:\/\/|mailto:)[^\s<>]+)>/i.exec(text.slice(i));
        if (auto) { flush(); appendLink(doc, parent, auto[1], auto[1], true); i += auto[0].length; continue; }
      }
      if ((ch === 'h' || ch === 'H') && !isWord(text[i - 1])) {
        const bare = /^https?:\/\/[^\s<>]*[^\s<>.,;:!?'")*_\]]/i.exec(text.slice(i));
        if (bare) { flush(); appendLink(doc, parent, bare[0], bare[0], true); i += bare[0].length; continue; }
      }
      if (ch === '*' || ch === '_' || ch === '~') {
        const emphasis = matchEmphasis(text, i);
        if (emphasis) {
          flush();
          let outer = null, inner = null;
          for (const tag of emphasis.tags) {
            const node = el(doc, tag);
            if (inner) inner.append(node); else outer = node;
            inner = node;
          }
          renderInline(doc, emphasis.inner, inner);
          parent.append(outer); i = emphasis.end; continue;
        }
        // Keep an unmatched delimiter run together so it is not reconsidered.
        let n = 1; while (text[i + n] === ch) n++;
        buffer += ch.repeat(n); i += n; continue;
      }
      buffer += ch; i++;
    }
    flush();
  }

  function isTableStart(lines, i) {
    return i + 1 < lines.length && lines[i].includes('|') && TABLE_SEP_RE.test(lines[i + 1])
      && (lines[i + 1].includes('|') || splitRow(lines[i]).length > 1);
  }

  function startsBlock(lines, i) {
    const line = lines[i];
    return FENCE_RE.test(line) || HEADING_RE.test(line) || HR_RE.test(line) || QUOTE_RE.test(line)
      || LIST_RE.test(line) && LIST_RE.exec(line)[4].trim() !== '' || isTableStart(lines, i);
  }

  function splitRow(line) {
    let row = line.trim();
    if (row.startsWith('|')) row = row.slice(1);
    if (row.endsWith('|') && !row.endsWith('\\|')) row = row.slice(0, -1);
    const cells = []; let cell = '', inCode = false;
    for (let i = 0; i < row.length; i++) {
      const ch = row[i];
      if (ch === '\\' && row[i + 1] === '|') { cell += '|'; i++; continue; }
      if (ch === '`') inCode = !inCode;
      if (ch === '|' && !inCode) { cells.push(cell.trim()); cell = ''; continue; }
      cell += ch;
    }
    cells.push(cell.trim());
    return cells;
  }

  function renderTable(doc, lines, start, parent) {
    const header = splitRow(lines[start]);
    const aligns = splitRow(lines[start + 1]).map((spec) =>
      spec.startsWith(':') && spec.endsWith(':') ? 'center' : spec.endsWith(':') ? 'right' : spec.startsWith(':') ? 'left' : '');
    const wrap = el(doc, 'div', 'md-table-wrap');
    const table = el(doc, 'table');
    const addRow = (section, cells, tag) => {
      const tr = el(doc, 'tr');
      header.forEach((_, index) => {
        const cell = el(doc, tag);
        if (aligns[index]) cell.style.textAlign = aligns[index];
        renderInline(doc, cells[index] || '', cell);
        tr.append(cell);
      });
      section.append(tr);
    };
    const thead = el(doc, 'thead'); addRow(thead, header, 'th'); table.append(thead);
    const tbody = el(doc, 'tbody');
    let i = start + 2;
    while (i < lines.length && lines[i].trim() && lines[i].includes('|')) { addRow(tbody, splitRow(lines[i]), 'td'); i++; }
    if (tbody.childNodes.length) table.append(tbody);
    wrap.append(table); parent.append(wrap);
    return i;
  }

  function stripIndent(line, amount) {
    let n = 0; while (n < amount && line[n] === ' ') n++;
    return line.slice(n);
  }

  function renderList(doc, lines, start, parent) {
    const first = LIST_RE.exec(lines[start]);
    const baseIndent = first[1].length;
    const ordered = /\d/.test(first[2]);
    const list = el(doc, ordered ? 'ol' : 'ul');
    if (ordered) { const n = parseInt(first[2], 10); if (n !== 1) list.start = n; }
    const items = [];
    let current = null, loose = false, i = start;
    while (i < lines.length) {
      const line = lines[i];
      const m = LIST_RE.exec(line);
      if (m && m[1].length < baseIndent + 2 && /\d/.test(m[2]) === ordered) {
        const spacing = m[3].length > 4 ? 1 : Math.max(1, m[3].length);
        current = { indent: m[1].length + m[2].length + spacing, lines: [m[4]] };
        items.push(current); i++; continue;
      }
      if (!line.trim()) {
        let k = i + 1; while (k < lines.length && !lines[k].trim()) k++;
        if (k >= lines.length) break;
        const next = LIST_RE.exec(lines[k]);
        const sibling = next && next[1].length < baseIndent + 2 && /\d/.test(next[2]) === ordered;
        const indented = /^ */.exec(lines[k])[0].length > baseIndent;
        if (!sibling && !indented) break;
        loose = true;
        for (; i < k; i++) current.lines.push('');
        continue;
      }
      const indent = /^ */.exec(line)[0].length;
      if (indent > baseIndent) { current.lines.push(stripIndent(line, current.indent)); i++; continue; }
      // Lazy paragraph continuation, unless the line starts another block.
      if (lines[i - 1].trim() && !startsBlock(lines, i)) { current.lines.push(line.trim()); i++; continue; }
      break;
    }
    for (const item of items) {
      const li = el(doc, 'li');
      renderBlocks(doc, item.lines.join('\n'), li);
      if (!loose) {
        for (const child of Array.from(li.childNodes)) {
          if (child.nodeName === 'P') child.replaceWith(...Array.from(child.childNodes));
        }
      }
      list.append(li);
    }
    parent.append(list);
    return i;
  }

  function renderBlocks(doc, source, parent) {
    const lines = source.replace(/\r\n?/g, '\n').replace(/\t/g, '    ').split('\n');
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (!line.trim()) { i++; continue; }
      let m;
      if ((m = FENCE_RE.exec(line))) {
        const marker = m[1];
        const closing = new RegExp(`^ {0,3}${marker[0] === '`' ? '`' : '~'}{${marker.length},}\\s*$`);
        const body = [];
        i++;
        while (i < lines.length && !closing.test(lines[i])) body.push(lines[i++]);
        i++;
        const pre = el(doc, 'pre'); const code = el(doc, 'code');
        if (m[2]) { code.dataset.lang = m[2]; code.className = `language-${m[2].replace(/[^\w-]/g, '')}`; }
        code.textContent = body.join('\n');
        pre.append(code); parent.append(pre);
        continue;
      }
      if ((m = HEADING_RE.exec(line))) {
        const heading = el(doc, `h${m[1].length}`);
        renderInline(doc, (m[2] || '').replace(/\s+#+$/, ''), heading);
        parent.append(heading); i++; continue;
      }
      if (HR_RE.test(line)) { parent.append(el(doc, 'hr')); i++; continue; }
      if (QUOTE_RE.test(line)) {
        const quoted = [];
        while (i < lines.length && QUOTE_RE.test(lines[i])) quoted.push(lines[i++].replace(/^ {0,3}> ?/, ''));
        const quote = el(doc, 'blockquote');
        renderBlocks(doc, quoted.join('\n'), quote);
        parent.append(quote); continue;
      }
      if (LIST_RE.test(line) && LIST_RE.exec(line)[4].trim() !== '') { i = renderList(doc, lines, i, parent); continue; }
      if (isTableStart(lines, i)) { i = renderTable(doc, lines, i, parent); continue; }
      const paragraph = [line.trim()];
      i++;
      while (i < lines.length && lines[i].trim() && !startsBlock(lines, i)) paragraph.push(lines[i++].trim());
      const p = el(doc, 'p');
      renderInline(doc, paragraph.join('\n'), p);
      parent.append(p);
    }
  }

  /** Render Markdown `text` into a DocumentFragment. */
  function render(text, doc = global.document) {
    const fragment = doc.createDocumentFragment();
    renderBlocks(doc, String(text ?? ''), fragment);
    return fragment;
  }

  /** Replace `container`'s children with rendered Markdown. */
  function renderInto(container, text) {
    container.replaceChildren(render(text, container.ownerDocument));
    return container;
  }

  global.SailMarkdown = { render, renderInto };
})(typeof window !== 'undefined' ? window : globalThis);
