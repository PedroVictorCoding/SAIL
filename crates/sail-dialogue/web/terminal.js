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

  class TerminalApp {
    constructor() {
      this.socket = null;
      this.reconnectTimer = null;
      this.exited = false;
      this.cwd = '~';
      this.shell = 'bash';
      this.isMaximized = false;

      this.initDOMElements();
      if (!this.container) return;
      this.term = new Terminal({
        cursorBlink: !window.matchMedia('(prefers-reduced-motion: reduce)').matches,
        disableStdin: true, scrollback: 5000, fontSize: 14, fontFamily: 'monospace',
        screenReaderMode: true,
        theme: { background: '#191d20', foreground: '#f0f2f3', cursor: '#ffc184',
          selectionBackground: '#515761' },
      });
      this.fit = new FitAddon.FitAddon();
      this.term.loadAddon(this.fit);
      this.term.open(this.container);
      this.term.onData(data => this.sendInput(data));
      this.term.onBinary(data => {
        if (this.socket?.readyState === WebSocket.OPEN)
          this.socket.send(Uint8Array.from(data, ch => ch.charCodeAt(0)));
      });
      this.term.onResize(({ cols, rows }) => this.sendRaw({ type: 'resize', cols, rows }));
      this.bindEvents();
      this.connect();
    }

    initDOMElements() {
      this.container = document.getElementById('terminalContainer');
      this.badge = document.getElementById('terminalLiveBadge');
      this.restartBtn = document.getElementById('terminalRestartBtn');
      this.clearBtn = document.getElementById('terminalClearBtn');
      this.maximizeBtn = document.getElementById('terminalMaximizeBtn');
      this.railBtn = document.getElementById('terminalRailBtn');
      this.headerBtn = document.getElementById('terminalHeaderBtn');
      this.terminalCard = document.getElementById('terminalCard');
    }

    bindEvents() {
      if (!this.container) return;

      // Click terminal to focus input
      this.container.addEventListener('click', (e) => {
        // If user didn't make a selection, focus input
        if (window.getSelection().toString().length === 0) {
          this.term.focus();
        }
      });

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
          this.exited = false;
          if (this.socket?.readyState === WebSocket.OPEN) this.sendRaw({ type: 'restart' });
          else this.connect();
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
        } else if (e.key === 'Escape' && this.isMaximized && !this.container.contains(e.target)) {
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

      this.socket.binaryType = 'arraybuffer';
      this.socket.onopen = async () => {
        // The server spawns no shell until the first message carries the owner token.
        const token = await this.ownerToken();
        this.sendRaw({type: 'auth', token});
      };

      this.socket.onmessage = (event) => {
        if (event.data instanceof ArrayBuffer) {
          this.term.write(new Uint8Array(event.data));
          return;
        }
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
        this.term.options.disableStdin = true;
        this.updateStatus('disconnected');
        if (!this.exited) this.scheduleReconnect();
      };
    }

    async ownerToken() {
      try {
        if (typeof window.getOrFetchOwnerToken === 'function') {
          const token = await window.getOrFetchOwnerToken();
          if (token) return token;
        }
      } catch (_) {}
      try { return localStorage.getItem('sail_owner_token') || ''; } catch (_) { return ''; }
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
          this.exited = false;
          this.term.options.disableStdin = false;
          this.term.reset();
          this.updateStatus('connected');
          this.updateTerminalSize();
          break;
        case 'output':
          this.appendOutput(msg.data);
          break;
        case 'exit':
          this.exited = true;
          this.term.options.disableStdin = true;
          this.updateStatus('disconnected');
          this.appendOutput(`\r\n[Process exited${msg.code == null ? '' : `: ${msg.code}`}]\r\n`);
          break;
        case 'error':
          this.appendOutput(`\r\nError: ${msg.message}\r\n`);
          break;
        case 'locked':
          // No shell was opened; do not retry until the owner token is set.
          this.exited = true;
          this.term.options.disableStdin = true;
          this.updateStatus('disconnected');
          this.appendOutput(`\r\n[Terminal locked: ${msg.reason}]\r\n`);
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
      if (cmd) this.sendInput(cmd + '\r');
      this.term.focus();
    }

    appendOutput(text) {
      this.term.write(text);
    }

    clearScreen() {
      this.term.clear();
      this.term.focus();
    }

    updateTerminalSize() {
      if (!this.container.clientWidth || !this.container.clientHeight) return;
      this.fit.fit();
      this.sendRaw({ type: 'resize', cols: this.term.cols, rows: this.term.rows });
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
      this.term.focus();
    }

    focusTerminal() {
      if (!this.terminalCard) return;
      // Switch to home view if currently on a subsystem page
      const homeNav = document.getElementById('homeNav');
      if (homeNav) homeNav.click();

      this.terminalCard.scrollIntoView({ behavior: window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth', block: 'center' });
      setTimeout(() => {
        this.term.focus();
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
