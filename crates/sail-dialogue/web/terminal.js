/**
 * SAIL In-Browser Interactive Terminal
 *
 * Provides a low-latency, full ANSI terminal emulator over WebSocket (/ws/terminal).
 * Connects directly to the user's host shell (PTY on Linux / piped fallback),
 * supporting interactive command execution, tab completion, ANSI colors,
 * truecolor, command history, and window resizing.
 */

(function () {
  'use strict';

  // 16 Standard ANSI Color Palette
  const ANSI_COLORS = {
    0: 'var(--term-black, #1e293b)',
    1: 'var(--term-red, #ef4444)',
    2: 'var(--term-green, #22c55e)',
    3: 'var(--term-yellow, #eab308)',
    4: 'var(--term-blue, #3b82f6)',
    5: 'var(--term-magenta, #d946ef)',
    6: 'var(--term-cyan, #06b6d4)',
    7: 'var(--term-white, #f8fafc)',
    8: 'var(--term-bright-black, #64748b)',
    9: 'var(--term-bright-red, #f87171)',
    10: 'var(--term-bright-green, #4ade80)',
    11: 'var(--term-bright-yellow, #facc15)',
    12: 'var(--term-bright-blue, #60a5fa)',
    13: 'var(--term-bright-magenta, #e879f9)',
    14: 'var(--term-bright-cyan, #22d3ee)',
    15: 'var(--term-bright-white, #ffffff)',
  };

  function escapeHtml(str) {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  // 256-color lookup
  function get256Color(n) {
    if (n < 16) return ANSI_COLORS[n] || '#ffffff';
    if (n >= 16 && n <= 231) {
      const idx = n - 16;
      const b = (idx % 6) * 51;
      const g = (Math.floor(idx / 6) % 6) * 51;
      const r = Math.floor(idx / 36) * 51;
      return `rgb(${r},${g},${b})`;
    }
    // Grayscale (232-255)
    const gray = (n - 232) * 10 + 8;
    return `rgb(${gray},${gray},${gray})`;
  }

  class AnsiParser {
    constructor() {
      this.bold = false;
      this.dim = false;
      this.italic = false;
      this.underline = false;
      this.inverse = false;
      this.fg = null;
      this.bg = null;
    }

    reset() {
      this.bold = false;
      this.dim = false;
      this.italic = false;
      this.underline = false;
      this.inverse = false;
      this.fg = null;
      this.bg = null;
    }

    getStyle() {
      const styles = [];
      let fgColor = this.fg;
      let bgColor = this.bg;

      if (this.inverse) {
        const tmp = fgColor;
        fgColor = bgColor || '#0d1117';
        bgColor = tmp || '#f8fafc';
      }

      if (fgColor) styles.push(`color: ${fgColor}`);
      if (bgColor) styles.push(`background-color: ${bgColor}`);
      if (this.bold) styles.push('font-weight: 700');
      if (this.dim) styles.push('opacity: 0.65');
      if (this.italic) styles.push('font-style: italic');
      if (this.underline) styles.push('text-decoration: underline');

      return styles.join('; ');
    }

    parseToHtml(rawText) {
      // Regex matches ANSI escape sequences: CSI [ ... m / K / J / H
      const ansiRegex = /\x1b\[([0-9;]*)([a-zA-Z])/g;
      let html = '';
      let lastIndex = 0;
      let match;

      while ((match = ansiRegex.exec(rawText)) !== null) {
        const textBefore = rawText.slice(lastIndex, match.index);
        if (textBefore) {
          const style = this.getStyle();
          const escaped = escapeHtml(textBefore);
          html += style ? `<span style="${style}">${escaped}</span>` : escaped;
        }

        const params = match[1] ? match[1].split(';').map((p) => parseInt(p, 10)) : [0];
        const cmd = match[2];

        if (cmd === 'm') {
          // SGR styling
          for (let i = 0; i < params.length; i++) {
            const p = isNaN(params[i]) ? 0 : params[i];
            if (p === 0) {
              this.reset();
            } else if (p === 1) {
              this.bold = true;
            } else if (p === 2) {
              this.dim = true;
            } else if (p === 3) {
              this.italic = true;
            } else if (p === 4) {
              this.underline = true;
            } else if (p === 7) {
              this.inverse = true;
            } else if (p === 22) {
              this.bold = false;
              this.dim = false;
            } else if (p === 23) {
              this.italic = false;
            } else if (p === 24) {
              this.underline = false;
            } else if (p === 27) {
              this.inverse = false;
            } else if (p >= 30 && p <= 37) {
              this.fg = ANSI_COLORS[p - 30];
            } else if (p === 38) {
              // 256 or truecolor
              if (params[i + 1] === 5 && params[i + 2] !== undefined) {
                this.fg = get256Color(params[i + 2]);
                i += 2;
              } else if (params[i + 1] === 2 && params[i + 4] !== undefined) {
                this.fg = `rgb(${params[i + 2]},${params[i + 3]},${params[i + 4]})`;
                i += 4;
              }
            } else if (p === 39) {
              this.fg = null;
            } else if (p >= 40 && p <= 47) {
              this.bg = ANSI_COLORS[p - 40];
            } else if (p === 48) {
              if (params[i + 1] === 5 && params[i + 2] !== undefined) {
                this.bg = get256Color(params[i + 2]);
                i += 2;
              } else if (params[i + 1] === 2 && params[i + 4] !== undefined) {
                this.bg = `rgb(${params[i + 2]},${params[i + 3]},${params[i + 4]})`;
                i += 4;
              }
            } else if (p === 49) {
              this.bg = null;
            } else if (p >= 90 && p <= 97) {
              this.fg = ANSI_COLORS[p - 90 + 8];
            } else if (p >= 100 && p <= 107) {
              this.bg = ANSI_COLORS[p - 100 + 8];
            }
          }
        } else if (cmd === 'K') {
          // Clear line (handled in buffer)
        } else if (cmd === 'J') {
          // Clear screen
          if (params[0] === 2 || params[0] === 3) {
            html = '';
          }
        }

        lastIndex = ansiRegex.lastIndex;
      }

      const remainder = rawText.slice(lastIndex);
      if (remainder) {
        const style = this.getStyle();
        const escaped = escapeHtml(remainder);
        html += style ? `<span style="${style}">${escaped}</span>` : escaped;
      }

      return html;
    }
  }

  class TerminalApp {
    constructor() {
      this.socket = null;
      this.reconnectTimer = null;
      this.parser = new AnsiParser();
      this.history = [];
      this.historyIdx = -1;
      this.maxHistory = 200;
      this.maxLines = 1500;
      this.cwd = '~';
      this.shell = 'bash';
      this.isMaximized = false;

      this.initDOMElements();
      this.bindEvents();
      this.connect();
    }

    initDOMElements() {
      this.container = document.getElementById('terminalContainer');
      this.output = document.getElementById('terminalOutput');
      this.input = document.getElementById('terminalInput');
      this.promptSymbol = document.getElementById('terminalPromptSymbol');
      this.badge = document.getElementById('terminalLiveBadge');
      this.restartBtn = document.getElementById('terminalRestartBtn');
      this.clearBtn = document.getElementById('terminalClearBtn');
      this.maximizeBtn = document.getElementById('terminalMaximizeBtn');
      this.railBtn = document.getElementById('terminalRailBtn');
      this.headerBtn = document.getElementById('terminalHeaderBtn');
      this.terminalCard = document.getElementById('terminalCard');
    }

    bindEvents() {
      if (!this.container || !this.input) return;

      // Click terminal to focus input
      this.container.addEventListener('click', (e) => {
        // If user didn't make a selection, focus input
        if (window.getSelection().toString().length === 0) {
          this.input.focus();
        }
      });

      // Handle keyboard commands
      this.input.addEventListener('keydown', (e) => this.handleKeyDown(e));

      // Quick command buttons
      document.querySelectorAll('.quick-cmd-btn').forEach((btn) => {
        btn.addEventListener('click', () => {
          const cmd = btn.getAttribute('data-cmd');
          if (cmd) {
            if (cmd === 'clear') {
              this.clearScreen();
            } else {
              this.sendCommand(cmd);
            }
          }
        });
      });

      // Clear button
      if (this.clearBtn) {
        this.clearBtn.addEventListener('click', () => this.clearScreen());
      }

      // Restart button
      if (this.restartBtn) {
        this.restartBtn.addEventListener('click', () => {
          this.sendRaw({ type: 'restart' });
          this.appendOutput('\n\x1b[33m--- Shell restarted ---\x1b[0m\n');
        });
      }

      // Maximize button
      if (this.maximizeBtn) {
        this.maximizeBtn.addEventListener('click', () => this.toggleMaximize());
      }

      // Side-rail navigation button
      if (this.railBtn) {
        this.railBtn.addEventListener('click', () => this.focusTerminal());
      }

      // Header button
      if (this.headerBtn) {
        this.headerBtn.addEventListener('click', () => this.focusTerminal());
      }

      // Global keyboard shortcut: Ctrl+` or Alt+T to toggle terminal focus
      window.addEventListener('keydown', (e) => {
        if ((e.ctrlKey && e.key === '`') || (e.altKey && (e.key === 't' || e.key === 'T'))) {
          e.preventDefault();
          this.focusTerminal();
        } else if (e.key === 'Escape' && this.isMaximized) {
          this.toggleMaximize();
        }
      });

      // Resize observer to update PTY rows and cols
      if (window.ResizeObserver && this.container) {
        const ro = new ResizeObserver(() => this.updateTerminalSize());
        ro.observe(this.container);
      }
    }

    connect() {
      if (this.socket && (this.socket.readyState === WebSocket.OPEN || this.socket.readyState === WebSocket.CONNECTING)) {
        return;
      }

      this.updateStatus('connecting');

      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const wsUrl = `${protocol}//${window.location.host}/ws/terminal`;

      try {
        this.socket = new WebSocket(wsUrl);
      } catch (err) {
        console.error('Terminal WebSocket creation failed:', err);
        this.scheduleReconnect();
        return;
      }

      this.socket.onopen = () => {
        this.updateStatus('connected');
        this.updateTerminalSize();
      };

      this.socket.onmessage = (event) => {
        try {
          const msg = JSON.parse(event.data);
          this.handleServerMessage(msg);
        } catch {
          // Raw string output fallback
          this.appendOutput(event.data);
        }
      };

      this.socket.onerror = (err) => {
        console.warn('Terminal socket error:', err);
        this.updateStatus('disconnected');
      };

      this.socket.onclose = () => {
        this.updateStatus('disconnected');
        this.scheduleReconnect();
      };
    }

    scheduleReconnect() {
      if (this.reconnectTimer) return;
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = null;
        this.connect();
      }, 3000);
    }

    updateStatus(status) {
      if (!this.badge) return;
      this.badge.classList.remove('active', 'inactive', 'connecting');
      if (status === 'connected') {
        this.badge.classList.add('active');
        this.badge.innerHTML = '<i class="live-dot"></i>Connected';
      } else if (status === 'connecting') {
        this.badge.classList.add('connecting');
        this.badge.innerHTML = '<i class="live-dot"></i>Connecting…';
      } else {
        this.badge.classList.add('inactive');
        this.badge.innerHTML = '<i class="live-dot"></i>Disconnected';
      }
    }

    handleServerMessage(msg) {
      switch (msg.type) {
        case 'connected':
          this.shell = msg.shell || 'bash';
          this.cwd = msg.cwd || '~';
          const shellName = this.shell.split('/').pop();
          if (this.promptSymbol) {
            this.promptSymbol.textContent = `${shellName}$`;
          }
          this.appendOutput(
            `\x1b[32m✔ Connected to SAIL local shell (${shellName})\x1b[0m\n\x1b[90mDirectory: ${this.cwd}\x1b[0m\n\n`
          );
          break;
        case 'output':
          this.appendOutput(msg.data);
          break;
        case 'exit':
          this.appendOutput('\n\x1b[33m[Process exited]\x1b[0m\n');
          break;
        case 'error':
          this.appendOutput(`\n\x1b[31mError: ${escapeHtml(msg.message)}\x1b[0m\n`);
          break;
      }
    }

    sendRaw(payload) {
      if (this.socket && this.socket.readyState === WebSocket.OPEN) {
        this.socket.send(JSON.stringify(payload));
      }
    }

    sendInput(data) {
      this.sendRaw({ type: 'input', data });
    }

    sendCommand(cmd) {
      if (!cmd) return;
      this.history.push(cmd);
      if (this.history.length > this.maxHistory) this.history.shift();
      this.historyIdx = this.history.length;

      // Echo command visually with prompt
      const prompt = this.promptSymbol ? this.promptSymbol.textContent : 'sail$';
      this.appendOutput(`\x1b[36m${prompt} \x1b[1m${cmd}\x1b[0m\n`);

      this.sendInput(cmd + '\n');
      if (this.input) this.input.value = '';
    }

    handleKeyDown(e) {
      // Ctrl+C -> SIGINT
      if (e.ctrlKey && e.key === 'c') {
        e.preventDefault();
        this.sendInput('\x03');
        this.input.value = '';
        return;
      }

      // Ctrl+D -> EOF
      if (e.ctrlKey && e.key === 'd') {
        e.preventDefault();
        this.sendInput('\x04');
        return;
      }

      // Ctrl+L -> Clear
      if (e.ctrlKey && (e.key === 'l' || e.key === 'L')) {
        e.preventDefault();
        this.clearScreen();
        return;
      }

      // Tab -> auto-completion
      if (e.key === 'Tab') {
        e.preventDefault();
        this.sendInput(this.input.value + '\t');
        return;
      }

      // Enter -> submit command
      if (e.key === 'Enter') {
        e.preventDefault();
        const cmd = this.input.value;
        this.sendCommand(cmd);
        return;
      }

      // Arrow Up -> previous history
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (this.history.length === 0) return;
        if (this.historyIdx > 0) {
          this.historyIdx--;
          this.input.value = this.history[this.historyIdx] || '';
        }
        return;
      }

      // Arrow Down -> next history
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (this.historyIdx < this.history.length - 1) {
          this.historyIdx++;
          this.input.value = this.history[this.historyIdx] || '';
        } else {
          this.historyIdx = this.history.length;
          this.input.value = '';
        }
        return;
      }
    }

    appendOutput(text) {
      if (!this.output) return;

      // Check if user is scrolled near bottom
      const isAtBottom =
        this.container.scrollHeight - this.container.scrollTop <= this.container.clientHeight + 60;

      // Handle carriage return \r progress bars (replace current active row)
      const lines = text.split('\n');
      for (let i = 0; i < lines.length; i++) {
        let line = lines[i];
        if (line.includes('\r')) {
          const parts = line.split('\r');
          line = parts[parts.length - 1]; // Keep last part after carriage return
        }

        const parsedHtml = this.parser.parseToHtml(line);
        if (i === 0 && this.output.lastElementChild && !text.startsWith('\n')) {
          // Append to last line
          this.output.lastElementChild.innerHTML += parsedHtml;
        } else {
          // Create new line div
          const lineEl = document.createElement('div');
          lineEl.className = 'terminal-line';
          lineEl.innerHTML = parsedHtml || '&nbsp;';
          this.output.appendChild(lineEl);
        }
      }

      // Trim excess scrollback buffer lines
      while (this.output.children.length > this.maxLines) {
        this.output.removeChild(this.output.firstChild);
      }

      if (isAtBottom) {
        this.container.scrollTop = this.container.scrollHeight;
      }
    }

    clearScreen() {
      if (!this.output) return;
      this.output.innerHTML = '';
      this.sendInput('clear\n');
    }

    updateTerminalSize() {
      if (!this.container) return;
      // Approximate 8px per column, 18px per row
      const width = this.container.clientWidth || 800;
      const height = this.container.clientHeight || 300;
      const cols = Math.max(40, Math.min(240, Math.floor(width / 8.5)));
      const rows = Math.max(10, Math.min(80, Math.floor(height / 19)));

      this.sendRaw({ type: 'resize', cols, rows });
    }

    toggleMaximize() {
      this.isMaximized = !this.isMaximized;
      if (!this.terminalCard) return;

      if (this.isMaximized) {
        this.terminalCard.classList.add('terminal-fullscreen');
        if (this.maximizeBtn) this.maximizeBtn.textContent = '✕';
        document.body.style.overflow = 'hidden';
      } else {
        this.terminalCard.classList.remove('terminal-fullscreen');
        if (this.maximizeBtn) this.maximizeBtn.textContent = '⤢';
        document.body.style.overflow = '';
      }
      setTimeout(() => this.updateTerminalSize(), 150);
      this.input.focus();
    }

    focusTerminal() {
      if (!this.terminalCard) return;
      // Switch to home view if currently on a subsystem page
      const homeNav = document.getElementById('homeNav');
      if (homeNav) homeNav.click();

      this.terminalCard.scrollIntoView({ behavior: 'smooth', block: 'center' });
      setTimeout(() => {
        if (this.input) this.input.focus();
      }, 300);
    }
  }

  // Initialize once DOM is ready
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => {
      window.sailTerminal = new TerminalApp();
    });
  } else {
    window.sailTerminal = new TerminalApp();
  }
})();
