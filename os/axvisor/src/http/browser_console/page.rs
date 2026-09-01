//! Self-contained single-page frontend served directly by Axvisor.

pub(super) const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Axvisor board console</title>
  <style>
    :root { color-scheme: dark; font-family: system-ui, sans-serif; background: #080c14; color: #e8edf7; }
    * { box-sizing: border-box; }
    body { margin: 0; min-height: 100vh; background: #080c14; }
    header { display: flex; justify-content: space-between; gap: 1rem; padding: 1rem 1.25rem .8rem; }
    h1 { margin: 0; font-size: 1.1rem; }
    header p { margin: .2rem 0 0; color: #8f9bb2; font-size: .8rem; }
    main { display: grid; grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr)); gap: .75rem; height: calc(100vh - 4.8rem); padding: 0 .75rem .75rem; }
    .console { display: grid; grid-template-rows: auto minmax(0, 1fr); min-width: 0; overflow: hidden; border: 1px solid #1b2638; border-radius: .65rem; background: #090e18; }
    .console:focus-within { border-color: #3b82f6; }
    .bar { display: flex; align-items: center; justify-content: space-between; padding: .6rem .7rem; border-bottom: 1px solid #1b2638; }
    .name { font-weight: 650; }
    .actions { display: flex; align-items: center; gap: .45rem; }
    .status { color: #f59e0b; font-size: .72rem; }
    .console[data-state="open"] .status { color: #34d399; }
    .console[data-state="closed"] .status { color: #f87171; }
    button { border: 1px solid #334155; border-radius: .35rem; padding: .25rem .45rem; color: #dbeafe; background: #172033; cursor: pointer; }
    .terminal { min-height: 0; margin: 0; padding: .55rem; overflow: auto; outline: none; color: #d7deea; background: #090e18; cursor: text; font: 13px/1.35 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; user-select: text; white-space: pre; tab-size: 8; }
    .terminal-cursor { display: inline-block; width: 0; height: 1.08em; border-left: 2px solid #d7deea; margin-left: -1px; vertical-align: -.15em; pointer-events: none; animation: cursor-blink 1s step-end infinite; }
    .terminal:not(:focus) .terminal-cursor { opacity: .45; animation: none; }
    @keyframes cursor-blink { 0%, 45% { opacity: 1; } 50%, 100% { opacity: 0; } }
    @media (max-width: 900px) { main { grid-template-columns: 1fr; height: auto; } .console { height: 34rem; } }
  </style>
</head>
<body>
  <header>
    <div><h1>Axvisor board console</h1><p>Self-contained page served directly by Axvisor</p></div>
    <p>Trusted management LAN only</p>
  </header>
  <main></main>
  <template id="console-pane">
    <section class="console">
      <div class="bar"><span class="name"></span><div class="actions"><span class="status">connecting</span><button>Reconnect</button></div></div>
      <pre class="terminal" tabindex="0"></pre>
    </section>
  </template>
  <script>
    (() => {
      const encoder = new TextEncoder();
      const TAB_WIDTH = 8;
      const keyBytes = new Map([
        ['Enter', '\r'], ['Backspace', '\x7f'], ['Tab', '\t'], ['Escape', '\x1b'],
        ['ArrowUp', '\x1b[A'], ['ArrowDown', '\x1b[B'], ['ArrowRight', '\x1b[C'], ['ArrowLeft', '\x1b[D'],
        ['Home', '\x1b[H'], ['End', '\x1b[F'], ['Delete', '\x1b[3~'],
        ['PageUp', '\x1b[5~'], ['PageDown', '\x1b[6~']
      ]);

      class Screen {
        constructor(element) {
          this.element = element;
          this.decoder = new TextDecoder();
          this.lines = [''];
          this.row = 0;
          this.column = 0;
          this.mode = 'text';
          this.sequence = '';
          this.cursorVisible = true;
          this.renderPaused = false;
          this.renderPending = false;
          this.renderScheduled = false;
          this.render();
        }
        write(payload) {
          const text = typeof payload === 'string' ? payload : this.decoder.decode(payload, { stream: true });
          for (const character of text) this.consume(character);
          this.trim();
          this.scheduleRender();
        }
        consume(character) {
          if (this.mode === 'csi') {
            this.sequence += character;
            if (character >= '@' && character <= '~') {
              this.applyCsi(this.sequence);
              this.mode = 'text';
              this.sequence = '';
            }
            return;
          }
          if (this.mode === 'osc') {
            if (character === '\x07') this.mode = 'text';
            else if (character === '\x1b') this.mode = 'osc-escape';
            return;
          }
          if (this.mode === 'osc-escape') {
            this.mode = character === '\\' ? 'text' : 'osc';
            return;
          }
          if (this.mode === 'escape') {
            if (character === '[') this.mode = 'csi';
            else if (character === ']') this.mode = 'osc';
            else {
              if (character === 'c') this.reset();
              this.mode = 'text';
            }
            return;
          }
          if (character === '\x1b') this.mode = 'escape';
          else if (character === '\r') this.column = 0;
          else if (character === '\n') this.newline();
          else if (character === '\b') this.column = Math.max(0, this.column - 1);
          else if (character === '\t') this.advanceTabStop();
          else if (character >= ' ') this.put(character);
        }
        applyCsi(sequence) {
          const command = sequence.at(-1);
          const parameters = sequence.slice(0, -1);
          if ((command === 'h' || command === 'l') && parameters.split(';').includes('?25')) {
            this.cursorVisible = command === 'h';
            return;
          }
          const values = parameters.replace(/^\?/, '').split(';').map(value => Number(value || 0));
          const amount = values[0] || 1;
          if (command === 'A') this.row = Math.max(0, this.row - amount);
          else if (command === 'B') this.row = Math.min(this.lines.length - 1, this.row + amount);
          else if (command === 'C') this.column += amount;
          else if (command === 'D') this.column = Math.max(0, this.column - amount);
          else if (command === 'G') this.column = Math.max(0, amount - 1);
          else if (command === 'H' || command === 'f') this.move(values);
          else if (command === 'J' && values[0] === 2) this.reset();
          else if (command === 'K') this.eraseLine(values[0]);
        }
        move(values) {
          const row = Math.max(0, (values[0] || 1) - 1);
          while (this.lines.length <= row) this.lines.push('');
          this.row = row;
          this.column = Math.max(0, (values[1] || 1) - 1);
        }
        eraseLine(mode) {
          const line = this.lines[this.row];
          if (mode === 1) this.lines[this.row] = ' '.repeat(this.column) + line.slice(this.column + 1);
          else if (mode === 2) { this.lines[this.row] = ''; this.column = 0; }
          else this.lines[this.row] = line.slice(0, this.column);
        }
        put(character) {
          let line = this.lines[this.row];
          if (line.length < this.column) line += ' '.repeat(this.column - line.length);
          this.lines[this.row] = line.slice(0, this.column) + character + line.slice(this.column + 1);
          this.column += 1;
        }
        advanceTabStop() {
          const stop = this.column + TAB_WIDTH - (this.column % TAB_WIDTH);
          while (this.column < stop) this.put(' ');
        }
        newline() {
          this.row += 1;
          this.column = 0;
          if (this.row === this.lines.length) this.lines.push('');
        }
        reset() { this.lines = ['']; this.row = 0; this.column = 0; }
        pauseRendering() { this.renderPaused = true; }
        resumeRendering() {
          if (!this.renderPaused) return;
          this.renderPaused = false;
          if (this.renderPending) {
            this.renderPending = false;
            this.scheduleRender();
          }
        }
        scheduleRender() {
          if (this.renderPaused) {
            this.renderPending = true;
            return;
          }
          if (this.renderScheduled) return;
          this.renderScheduled = true;
          requestAnimationFrame(() => {
            this.renderScheduled = false;
            this.render();
          });
        }
        trim() {
          if (this.lines.length <= 4000) return;
          const removed = this.lines.length - 4000;
          this.lines.splice(0, removed);
          this.row = Math.max(0, this.row - removed);
        }
        render() {
          if (this.renderPaused) {
            this.renderPending = true;
            return;
          }
          const follow = this.element.scrollTop + this.element.clientHeight + 24 >= this.element.scrollHeight;
          const lines = this.lines.slice();
          if (lines[this.row].length < this.column) {
            lines[this.row] += ' '.repeat(this.column - lines[this.row].length);
          }
          const text = lines.join('\n');
          if (this.cursorVisible) {
            let cursorOffset = this.column;
            for (let row = 0; row < this.row; row += 1) cursorOffset += lines[row].length + 1;
            const fragment = document.createDocumentFragment();
            fragment.append(document.createTextNode(text.slice(0, cursorOffset)));
            const cursor = document.createElement('span');
            cursor.className = 'terminal-cursor';
            cursor.setAttribute('aria-hidden', 'true');
            fragment.append(cursor, document.createTextNode(text.slice(cursorOffset)));
            this.element.replaceChildren(fragment);
          } else {
            this.element.textContent = text;
          }
          if (follow) this.element.scrollTop = this.element.scrollHeight;
        }
      }

      class Pane {
        constructor(card) {
          this.card = card;
          this.channel = card.dataset.channel;
          this.status = card.querySelector('.status');
          this.terminal = card.querySelector('.terminal');
          this.screen = new Screen(this.terminal);
          this.socket = null;
          this.generation = 0;
          this.selecting = false;
          this.terminal.addEventListener('keydown', event => this.keydown(event));
          this.terminal.addEventListener('paste', event => this.paste(event));
          this.terminal.addEventListener('pointerdown', event => this.pointerDown(event));
          window.addEventListener('pointerup', event => this.pointerUp(event));
          document.addEventListener('selectionchange', () => this.selectionChanged());
          card.querySelector('button').addEventListener('click', () => this.connect());
          this.connect();
        }
        state(state, label) { this.card.dataset.state = state; this.status.textContent = label; }
        connect() {
          const generation = ++this.generation;
          if (this.socket) { this.socket.onclose = null; this.socket.close(); }
          this.state('connecting', 'connecting');
          const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
          const socket = new WebSocket(`${scheme}://${location.host}/ws/${this.channel}`);
          socket.binaryType = 'arraybuffer';
          this.socket = socket;
          socket.onopen = () => { if (generation === this.generation) { this.state('open', 'connected'); this.terminal.focus(); } };
          socket.onmessage = event => { if (generation === this.generation) this.screen.write(event.data); };
          socket.onerror = () => { if (generation === this.generation) this.state('closed', 'error'); };
          socket.onclose = () => { if (generation === this.generation) { this.state('closed', 'disconnected'); setTimeout(() => this.connect(), 1500); } };
        }
        keydown(event) {
          const key = event.key.toLowerCase();
          if (event.ctrlKey && key === 'c' && this.hasTerminalSelection()) {
            this.copySelection(event);
            return;
          }
          if ((event.ctrlKey && event.shiftKey && key === 'v') ||
              (event.shiftKey && event.key === 'Insert')) {
            return;
          }
          if (event.metaKey) return;
          let text = keyBytes.get(event.key);
          if (event.ctrlKey && event.key.length === 1) {
            const code = event.key.toUpperCase().charCodeAt(0);
            if (code >= 64 && code <= 95) text = String.fromCharCode(code & 31);
          } else if (!event.ctrlKey && !event.altKey && event.key.length === 1) text = event.key;
          else if (event.altKey && !event.ctrlKey && event.key.length === 1) text = '\x1b' + event.key;
          if (text === undefined) return;
          event.preventDefault();
          this.screen.resumeRendering();
          this.send(text);
        }
        paste(event) {
          event.preventDefault();
          this.screen.resumeRendering();
          this.send(event.clipboardData.getData('text'));
        }
        pointerDown(event) {
          if (event.button !== 0) return;
          this.selecting = true;
          this.screen.pauseRendering();
        }
        pointerUp(event) {
          if (event.button !== 0 || !this.selecting) return;
          this.selecting = false;
          this.resumeRenderingWithoutSelection();
        }
        selectionChanged() {
          if (!this.selecting) this.resumeRenderingWithoutSelection();
        }
        hasTerminalSelection() {
          const selection = window.getSelection();
          return selection && !selection.isCollapsed &&
            this.terminal.contains(selection.anchorNode) &&
            this.terminal.contains(selection.focusNode);
        }
        selectionTouchesTerminal() {
          const selection = window.getSelection();
          return selection && !selection.isCollapsed &&
            (this.terminal.contains(selection.anchorNode) ||
             this.terminal.contains(selection.focusNode));
        }
        resumeRenderingWithoutSelection() {
          if (!this.selectionTouchesTerminal()) this.screen.resumeRendering();
        }
        copySelection(event) {
          const selection = window.getSelection();
          if (!this.hasTerminalSelection()) return;
          event.preventDefault();
          const text = selection.toString();
          if (navigator.clipboard && window.isSecureContext) {
            navigator.clipboard.writeText(text).catch(() => document.execCommand('copy'));
          } else {
            document.execCommand('copy');
          }
        }
        send(text) {
          if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;
          const bytes = encoder.encode(text);
          for (let offset = 0; offset < bytes.length; offset += 4096) this.socket.send(bytes.slice(offset, offset + 4096));
        }
      }

      function addPane(console) {
        if (typeof console.route !== 'string' || typeof console.name !== 'string') return;
        const template = document.querySelector('#console-pane');
        const card = template.content.firstElementChild.cloneNode(true);
        card.dataset.channel = console.route;
        card.querySelector('.name').textContent = console.name;
        card.querySelector('.terminal').setAttribute('aria-label', `${console.name} terminal`);
        document.querySelector('main').append(card);
        new Pane(card);
      }

      async function loadStartupConsoles() {
        let consoles;
        try {
          const response = await fetch('/api/consoles', { cache: 'no-store' });
          if (!response.ok) throw new Error(`console discovery returned ${response.status}`);
          consoles = await response.json();
        } catch (_) {
          consoles = [{ route: 'axvisor', name: 'Axvisor' }];
        }
        consoles.forEach(addPane);
      }

      loadStartupConsoles();
    })();
  </script>
</body>
</html>
"##;
