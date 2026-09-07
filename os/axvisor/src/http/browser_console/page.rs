//! Self-contained single-page frontend served directly by Axvisor.

pub(super) const XTERM_JAVASCRIPT: &str = include_str!("assets/xterm-6.0.0/xterm.js");
pub(super) const XTERM_STYLESHEET: &str = include_str!("assets/xterm-6.0.0/xterm.css");

pub(super) const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Axvisor board console</title>
  <link rel="stylesheet" href="/assets/xterm.css">
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
    .terminal { min-width: 0; min-height: 0; padding: .55rem; overflow: hidden; outline: none; background: #090e18; }
    .terminal .xterm { height: 100%; }
    @media (max-width: 900px) { main { grid-template-columns: 1fr; height: auto; } .console { height: calc(100vh - 4.8rem); } }
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
      <div class="terminal"></div>
    </section>
  </template>
  <script src="/assets/xterm.js"></script>
  <script>
    (() => {
      const encoder = new TextEncoder();

      class Pane {
        constructor(card) {
          this.card = card;
          this.channel = card.dataset.channel;
          this.status = card.querySelector('.status');
          this.element = card.querySelector('.terminal');
          this.terminal = new Terminal({
            cols: 80,
            rows: 40,
            scrollback: 4000,
            cursorBlink: true,
            fontSize: 13,
            lineHeight: 1,
            fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
            theme: { background: '#090e18', foreground: '#d7deea', cursor: '#d7deea' },
          });
          this.terminal.open(this.element);
          this.socket = null;
          this.generation = 0;
          this.terminal.onData(data => this.send(data));
          this.terminal.attachCustomKeyEventHandler(event => this.handleCopy(event));
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
          socket.onmessage = event => {
            if (generation !== this.generation) return;
            this.terminal.write(typeof event.data === 'string' ? event.data : new Uint8Array(event.data));
          };
          socket.onerror = () => { if (generation === this.generation) this.state('closed', 'error'); };
          socket.onclose = () => { if (generation === this.generation) { this.state('closed', 'disconnected'); setTimeout(() => this.connect(), 1500); } };
        }
        handleCopy(event) {
          if (event.type !== 'keydown' || !event.ctrlKey ||
              event.key.toLowerCase() !== 'c' || !this.terminal.hasSelection()) return true;
          event.preventDefault();
          const text = this.terminal.getSelection();
          if (navigator.clipboard && window.isSecureContext) {
            navigator.clipboard.writeText(text).catch(() => document.execCommand('copy'));
          } else {
            document.execCommand('copy');
          }
          return false;
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
