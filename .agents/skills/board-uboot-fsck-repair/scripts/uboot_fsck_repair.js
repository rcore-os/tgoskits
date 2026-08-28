#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');

const DEFAULT_BOARD_TYPE = 'OrangePi-5-Plus';
const DEFAULT_PORT = 2999;
const DEFAULT_BOOT_ARG = 'fsckfix';
const DEFAULT_ENV_NAME = 'extraboardargs';
const DEFAULT_TIMEOUT_MS = 10 * 60 * 1000;
const MAX_LOGIN_REGEX_LENGTH = 512;
const DEFAULT_LOGIN_REGEX = String.raw`(root@[^#\r\n]*#\s*$|[A-Za-z0-9_.-]+\s+login:\s*$|[A-Za-z0-9_.-]+@[A-Za-z0-9_.-]+:[^\r\n]*[#$]\s*$)`;

function usage() {
  console.log(`Usage:
  uboot_fsck_repair.js [options]

Options:
  -b, --board-type <type>   Board type to allocate (default: ${DEFAULT_BOARD_TYPE})
      --server <host>       ostool-server host or URL (default: ~/.ostool/config.toml)
      --port <port>         ostool-server port (default: ${DEFAULT_PORT})
      --boot-arg <arg>      Linux repair argument to inject (default: ${DEFAULT_BOOT_ARG})
      --env-name <name>     U-Boot env var to set (default: ${DEFAULT_ENV_NAME})
      --login-regex <regex> Regex that proves Linux reached a prompt
      --timeout-ms <ms>     Overall timeout (default: ${DEFAULT_TIMEOUT_MS})
      --log <path>          Serial log path (default: /tmp/board-uboot-fsck-repair-*.log)
      --quiet               Do not mirror serial output to stdout
  -h, --help                Show this help
`);
}

function parseArgs(argv) {
  const opts = {
    boardType: DEFAULT_BOARD_TYPE,
    bootArg: DEFAULT_BOOT_ARG,
    envName: DEFAULT_ENV_NAME,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    loginRegex: DEFAULT_LOGIN_REGEX,
    quiet: false,
    log: path.join(
      os.tmpdir(),
      `board-uboot-fsck-repair-${new Date().toISOString().replace(/[:.]/g, '-')}.log`,
    ),
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      i += 1;
      if (i >= argv.length) {
        throw new Error(`${arg} requires a value`);
      }
      return argv[i];
    };

    switch (arg) {
      case '-b':
      case '--board-type':
        opts.boardType = next();
        break;
      case '--server':
        opts.server = next();
        break;
      case '--port':
        opts.port = Number.parseInt(next(), 10);
        break;
      case '--boot-arg':
        opts.bootArg = next();
        break;
      case '--env-name':
        opts.envName = next();
        break;
      case '--login-regex':
        opts.loginRegex = next();
        break;
      case '--timeout-ms':
        opts.timeoutMs = Number.parseInt(next(), 10);
        break;
      case '--log':
        opts.log = next();
        break;
      case '--quiet':
        opts.quiet = true;
        break;
      case '-h':
      case '--help':
        opts.help = true;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (opts.port !== undefined && (!Number.isFinite(opts.port) || opts.port <= 0)) {
    throw new Error(`invalid port: ${opts.port}`);
  }
  if (!Number.isFinite(opts.timeoutMs) || opts.timeoutMs <= 0) {
    throw new Error(`invalid timeout: ${opts.timeoutMs}`);
  }
  if (opts.loginRegex.length > MAX_LOGIN_REGEX_LENGTH) {
    throw new Error(`--login-regex is too long: ${opts.loginRegex.length} > ${MAX_LOGIN_REGEX_LENGTH}`);
  }
  try {
    opts.loginPattern = new RegExp(opts.loginRegex, 'm');
  } catch (err) {
    throw new Error(`invalid --login-regex: ${err.message}`);
  }
  return opts;
}

function readOstoolBoardConfig() {
  const configPath = path.join(os.homedir(), '.ostool', 'config.toml');
  if (!fs.existsSync(configPath)) {
    return {};
  }

  const config = fs.readFileSync(configPath, 'utf8');
  const result = {};
  let inBoard = false;
  for (const rawLine of config.split(/\r?\n/)) {
    const line = rawLine.replace(/#.*/, '').trim();
    const section = line.match(/^\[([^\]]+)]$/);
    if (section) {
      inBoard = section[1] === 'board';
      continue;
    }
    if (!inBoard) {
      continue;
    }

    const kv = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.+)$/);
    if (!kv) {
      continue;
    }
    const key = kv[1];
    const value = kv[2].trim().replace(/^"(.*)"$/, '$1');
    if (key === 'server_ip' || key === 'server') {
      result.server = value;
    } else if (key === 'port') {
      result.port = Number.parseInt(value, 10);
    }
  }
  return result;
}

function makeBaseUrl(protocol, server, port) {
  const raw = server.includes('://') ? server : `${protocol}://${server}:${port}`;
  const url = new URL(raw);
  url.protocol = `${protocol}:`;
  if (!url.port && port) {
    url.port = String(port);
  }
  if (!url.pathname.endsWith('/')) {
    url.pathname += '/';
  }
  return url;
}

function endpoint(baseUrl, apiPath) {
  return new URL(apiPath.replace(/^\//, ''), baseUrl);
}

async function requestJson(baseUrl, method, apiPath, body = undefined) {
  const response = await fetch(endpoint(baseUrl, apiPath), {
    method,
    headers: body === undefined ? undefined : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${method} ${apiPath} failed: HTTP ${response.status} ${text}`);
  }
  return text ? JSON.parse(text) : {};
}

async function requestEmpty(baseUrl, method, apiPath) {
  const response = await fetch(endpoint(baseUrl, apiPath), { method });
  if (!response.ok && response.status !== 404) {
    const text = await response.text();
    throw new Error(`${method} ${apiPath} failed: HTTP ${response.status} ${text}`);
  }
}

function resolveWsUrl(wsBaseUrl, wsUrl) {
  if (wsUrl.startsWith('ws://') || wsUrl.startsWith('wss://')) {
    return wsUrl;
  }
  return endpoint(wsBaseUrl, wsUrl).toString();
}

function stripAnsi(text) {
  return text.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
}

function appendRecent(recent, chunk, limit = 65536) {
  const merged = recent + chunk;
  return merged.length > limit ? merged.slice(merged.length - limit) : merged;
}

function dataToBuffer(data) {
  if (Buffer.isBuffer(data)) {
    return Promise.resolve(data);
  }
  if (typeof data === 'string') {
    return Promise.resolve(Buffer.from(data, 'utf8'));
  }
  if (data instanceof ArrayBuffer) {
    return Promise.resolve(Buffer.from(data));
  }
  if (ArrayBuffer.isView(data)) {
    return Promise.resolve(Buffer.from(data.buffer, data.byteOffset, data.byteLength));
  }
  if (data && typeof data.arrayBuffer === 'function') {
    return data.arrayBuffer().then((buffer) => Buffer.from(buffer));
  }
  return Promise.resolve(Buffer.from(String(data), 'utf8'));
}

function isOpen(ws) {
  return ws.readyState === WebSocket.OPEN;
}

function sendSerial(ws, text) {
  ws.send(Buffer.from(text, 'utf8'));
}

async function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function main() {
  if (typeof fetch !== 'function' || typeof WebSocket !== 'function') {
    throw new Error('Node.js with built-in fetch and WebSocket support is required');
  }

  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    usage();
    return;
  }

  const config = readOstoolBoardConfig();
  const server = opts.server || config.server;
  const port = opts.port ?? config.port ?? DEFAULT_PORT;
  if (!server) {
    throw new Error('missing ostool-server host; pass --server or configure ~/.ostool/config.toml');
  }

  fs.mkdirSync(path.dirname(opts.log), { recursive: true });
  const logStream = fs.createWriteStream(opts.log, { flags: 'a' });
  const httpBase = makeBaseUrl('http', server, port);
  const wsBase = makeBaseUrl('ws', server, port);

  let sessionId;
  let ws;
  let heartbeat;
  let done = false;
  const state = {
    recent: '',
    sentStop: false,
    sentBoot: false,
    sawFsckRepair: false,
    fsckModified: false,
    stage: 'allocating board',
  };

  async function cleanup() {
    clearInterval(heartbeat);
    if (ws && isOpen(ws)) {
      try {
        ws.send(JSON.stringify({ type: 'close' }));
        await sleep(300);
        ws.close();
      } catch (_) {
        // Best-effort serial close; session deletion below is authoritative.
      }
    }
    if (sessionId) {
      await requestEmpty(httpBase, 'DELETE', `/api/v1/sessions/${sessionId}`);
    }
    await new Promise((resolve) => logStream.end(resolve));
  }

  function finishOk() {
    if (done) {
      return;
    }
    done = true;
    console.log(
      `\nRESULT boot_arg_injected=${state.sentBoot} fsck_repair_observed=${state.sawFsckRepair} fsck_modified=${state.fsckModified} linux_login=true log=${opts.log}`,
    );
    cleanup()
      .then(() => process.exit(0))
      .catch((err) => {
        console.error(`cleanup failed: ${err.message}`);
        process.exit(1);
      });
  }

  function finishErr(err) {
    if (done) {
      return;
    }
    done = true;
    console.error(`\nERROR stage=${state.stage}: ${err.message}`);
    console.error(`serial log: ${opts.log}`);
    console.error('recent serial output:');
    console.error(state.recent.slice(-4000));
    cleanup()
      .then(() => process.exit(1))
      .catch((cleanupErr) => {
        console.error(`cleanup failed: ${cleanupErr.message}`);
        process.exit(1);
      });
  }

  function processSerialText(text) {
    const clean = stripAnsi(text);
    state.recent = appendRecent(state.recent, clean);

    if (!state.sentStop && /Hit any key to stop autoboot:/i.test(state.recent)) {
      state.stage = 'interrupting U-Boot autoboot';
      sendSerial(ws, ' ');
      state.sentStop = true;
    }

    if (!state.sentBoot && /(?:^|[\r\n])=>\s*$/m.test(state.recent)) {
      state.stage = `booting Linux with ${opts.envName}=${opts.bootArg}`;
      sendSerial(ws, `setenv ${opts.envName} ${opts.bootArg}\nboot\n`);
      state.sentBoot = true;
      return;
    }

    if (/fsck\.ext4\b.*\s-y\b|\s-y\b.*fsck\.ext4\b|fsckfix\b/i.test(state.recent)) {
      state.sawFsckRepair = true;
    }
    if (/FILE SYSTEM WAS MODIFIED|filesystem was modified|CLEARED|FIXED|REPAIRED/i.test(state.recent)) {
      state.fsckModified = true;
    }
    if (/UNEXPECTED INCONSISTENCY;\s*RUN fsck MANUALLY/i.test(state.recent)) {
      finishErr(new Error('fsck still requires manual repair after boot'));
      return;
    }
    if (state.sentBoot && opts.loginPattern.test(state.recent)) {
      finishOk();
    }
  }

  process.on('SIGINT', () => finishErr(new Error('interrupted')));
  process.on('SIGTERM', () => finishErr(new Error('terminated')));

  console.error(`Allocating ${opts.boardType} from ${httpBase.origin} ...`);
  const session = await requestJson(httpBase, 'POST', '/api/v1/sessions', {
    board_type: opts.boardType,
    required_tags: [],
    client_name: 'board-uboot-fsck-repair',
  });
  sessionId = session.session_id;
  if (!session.ws_url) {
    throw new Error('allocated board has no serial websocket URL');
  }
  console.error(`Allocated board_id=${session.board_id} session_id=${sessionId}`);

  heartbeat = setInterval(() => {
    requestJson(httpBase, 'POST', `/api/v1/sessions/${sessionId}/heartbeat`).catch((err) => {
      console.error(`heartbeat failed: ${err.message}`);
    });
  }, 1000);

  const wsUrl = resolveWsUrl(wsBase, session.ws_url);
  state.stage = 'connecting serial websocket';
  ws = new WebSocket(wsUrl);
  ws.addEventListener('open', () => {
    state.stage = 'waiting for U-Boot autoboot prompt';
    console.error(`Connected serial websocket: ${wsUrl}`);
  });
  ws.addEventListener('message', async (event) => {
    try {
      const buffer = await dataToBuffer(event.data);
      const text = buffer.toString('utf8');
      if (text.startsWith('{')) {
        try {
          const control = JSON.parse(text);
          if (control.type === 'opened' || control.type === 'closed') {
            return;
          }
          if (control.type === 'error') {
            finishErr(new Error(control.message || 'serial websocket error'));
            return;
          }
        } catch (_) {
          // Treat non-control JSON-looking text as serial output.
        }
      }

      logStream.write(buffer);
      if (!opts.quiet) {
        process.stdout.write(buffer);
      }
      processSerialText(text);
    } catch (err) {
      finishErr(err);
    }
  });
  ws.addEventListener('error', () => finishErr(new Error('serial websocket error')));
  ws.addEventListener('close', () => {
    if (!done) {
      finishErr(new Error('serial websocket closed before Linux prompt'));
    }
  });

  setTimeout(() => {
    finishErr(new Error(`timed out after ${opts.timeoutMs}ms`));
  }, opts.timeoutMs).unref();
}

main().catch((err) => {
  console.error(`ERROR: ${err.message}`);
  process.exit(1);
});
