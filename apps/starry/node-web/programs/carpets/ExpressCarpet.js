'use strict';
// INDUSTRIAL carpet for express 4.21.2 on Node.js 22 LTS.
// Drives a real express app over IPv4 loopback (127.0.0.1) with node's built-in
// http client. Every assertion is an exact-value check against a golden observed
// from the real package on this exact version. Deterministic only.

const express = require('express');
const http = require('http');
const fs = require('fs');
const path = require('path');

// ---- self-check harness (mirrors java-web carpets) -------------------------
let ok = 0, fail = 0;
function chk(cond, name) {
  if (cond) { ok++; } else { fail++; console.log('FAIL ' + name); }
}
function eq(actual, expected, name) {
  const c = actual === expected;
  if (!c) console.log('  expected=' + JSON.stringify(expected) + ' actual=' + JSON.stringify(actual));
  chk(c, name);
}

// ---- deterministic on-disk assets (self-contained) -------------------------
const ROOT = process.cwd();
const PUB = path.join(ROOT, 'pub');
const VIEWS = path.join(ROOT, 'views');
fs.mkdirSync(PUB, { recursive: true });
fs.mkdirSync(VIEWS, { recursive: true });
fs.writeFileSync(path.join(PUB, 'hello.txt'), 'hello static world\n');
fs.writeFileSync(path.join(VIEWS, 'page.pug'),
  'html\n  head\n    title= title\n  body\n    h1= heading\n    p Hello #{name}\n');

// ---- build ONE app mounting every feature ----------------------------------
const app = express();
app.set('etag', false);                 // deterministic static (no mtime ETag)
app.set('views', VIEWS);
app.set('view engine', 'pug');
app.set('custom-setting', 'CVAL');
app.locals.appName = 'CarpetApp';

// body parsers
app.use(express.json());
app.use(express.urlencoded({ extended: true }));
app.use(express.text());
app.use(express.raw());

// global middleware: sets a header + a req prop, then next()
app.use((req, res, next) => { res.set('X-Global', 'G'); req.globalFlag = 'GLOBAL'; next(); });

// path-scoped middleware
app.use('/scoped', (req, res, next) => { req.scopedFlag = 'SCOPED'; next(); });

// --- Routing verbs -----------------------------------------------------------
app.get('/json', (req, res) => res.json({ a: 1, b: 'x' }));
app.get('/send-str', (req, res) => res.send('<p>hi</p>'));
app.get('/send-buf', (req, res) => res.send(Buffer.from('abc')));
app.get('/send-obj', (req, res) => res.send({ k: 2 }));
app.get('/type', (req, res) => res.type('txt').send('plain'));
app.get('/typeext', (req, res) => { res.type('application/json'); res.send('{}'); });
app.get('/ss204', (req, res) => res.sendStatus(204));
app.get('/ss404', (req, res) => res.sendStatus(404));
app.get('/redir', (req, res) => res.redirect('/dest'));
app.get('/redir301', (req, res) => res.redirect(301, '/dest'));
app.get('/cookie', (req, res) => { res.cookie('foo', 'bar'); res.send('ck'); });
app.get('/links', (req, res) => { res.links({ next: 'http://x/2', last: 'http://x/5' }); res.send('lk'); });
app.get('/vary', (req, res) => { res.vary('Accept'); res.send('vy'); });
app.get('/loc', (req, res) => { res.location('/somewhere'); res.end(); });
app.get('/append', (req, res) => { res.append('X-Multi', 'a'); res.append('X-Multi', 'b'); res.send('ap'); });
app.get('/getset', (req, res) => { res.set('X-Foo', 'FV'); res.json({ got: res.get('X-Foo') }); });
app.get('/setobj', (req, res) => { res.set({ 'X-A': '1', 'X-B': '2' }); res.send('ok'); });
app.get('/status201', (req, res) => res.status(201).send('created'));
app.get('/jstatus', (req, res) => res.status(422).json({ e: 'bad' }));
app.get('/jsonp', (req, res) => res.jsonp({ j: 1 }));
app.get('/attach', (req, res) => { res.attachment('report.pdf'); res.send('PDF'); });
app.get('/xpb', (req, res) => res.send('xpb'));

app.put('/put', (req, res) => res.send('put-ok'));
app.patch('/patch', (req, res) => res.send('patch-ok'));
app.delete('/del', (req, res) => res.send('del-ok'));
app.head('/h', (req, res) => res.set('X-Head', 'yes').end());
app.all('/all', (req, res) => res.send('all:' + req.method));
app.get('/headauto', (req, res) => res.send('getbody'));

// route params / multi-params / regex / query
app.get('/users/:id', (req, res) => res.send('user:' + req.params.id));
app.get('/a/:x/b/:y', (req, res) => res.json(req.params));
app.get(/^\/regex\/[0-9]+$/, (req, res) => res.send('regexmatch'));
app.get('/q', (req, res) => res.json(req.query));
app.get('/qarr', (req, res) => res.json(req.query));

// app.route().get().post() chaining
app.route('/chained')
  .get((req, res) => res.send('chained-GET'))
  .post((req, res) => res.send('chained-POST'));

// next('route') skipping
app.get('/skip', (req, res, next) => next('route'), (req, res) => res.send('should-not-run'));
app.get('/skip', (req, res) => res.send('after-skip'));

// multiple handlers chained via next()
app.get('/arr', (req, res, next) => { req.step = '1'; next(); }, (req, res) => res.send('step:' + req.step));

// scoped flag consumer
app.get('/scoped/x', (req, res) => res.send('flag:' + req.scopedFlag + ',' + req.globalFlag));

// request API introspection
app.get('/reqinfo', (req, res) => res.json({
  method: req.method, path: req.path, isjson: req.is('json'),
  acc: req.accepts(['html', 'json']), h: req.get('X-Custom') || null
}));
app.get('/reqx', (req, res) => res.json({
  proto: req.protocol, host: req.hostname, ip: req.ip, xhr: req.xhr,
  ourl: req.originalUrl, burl: req.baseUrl, url: req.url, secure: req.secure
}));

// content negotiation
app.get('/format', (req, res) => {
  res.format({
    'text/plain': () => res.send('plain-fmt'),
    'application/json': () => res.json({ f: 'json' })
  });
});

// app settings introspection
app.get('/settings', (req, res) => res.json({
  cs: app.get('custom-setting'), en: app.enabled('x-powered-by'),
  dis: app.disabled('etag'), loc: req.app.locals.appName
}));

// pug view rendering
app.get('/view', (req, res) => res.render('page', { title: 'T', heading: 'H', name: 'World' }));

// --- body parsing routes ----
app.post('/pjson', (req, res) => res.json(req.body));
app.post('/pform', (req, res) => res.json(req.body));
app.post('/ptext', (req, res) => res.send('text-body:' + req.body));

// --- error handling ----
app.get('/err', (req, res) => { throw new Error('boom'); });
app.get('/nexterr', (req, res, next) => { next(new Error('viaNext')); });

// --- Router() sub-app mounted at /api with router.param preprocessing ----
const r = express.Router();
r.param('id', (req, res, next, id) => { req.pid = 'P' + id; next(); });
r.get('/info', (req, res) => res.json({ where: 'router-info' }));
r.get('/item/:id', (req, res) => res.json({ pid: req.pid, id: req.params.id }));
app.use('/api', r);

// --- nested sub-application mounted at /mounted ----
const sub = express();
sub.get('/ping', (req, res) => res.send('sub-pong baseUrl=' + req.baseUrl));
app.use('/mounted', sub);

// --- static dir ----
app.use('/files', express.static(PUB));

// --- custom 404 handler (fires when nothing matched & not the default html one) ----
app.use('/missing', (req, res) => res.status(404).type('txt').send('custom-404'));

// --- error middleware (err,req,res,next) ----
app.use((err, req, res, next) => { res.status(500).json({ error: err.message }); });

// ---- promisified loopback http client --------------------------------------
let PORT = 0;
function call(method, p, body, headers) {
  return new Promise((resolve, reject) => {
    const opts = { host: '127.0.0.1', port: PORT, path: p, method, headers: headers || {} };
    const rq = http.request(opts, (res) => {
      let d = '';
      res.on('data', (c) => d += c);
      res.on('end', () => resolve({ s: res.statusCode, h: res.headers, b: d }));
    });
    rq.on('error', reject);
    if (body != null) rq.write(body);
    rq.end();
  });
}

// ---- run the sequence ------------------------------------------------------
const server = app.listen(0, '127.0.0.1', async () => {
  PORT = server.address().port;
  try {
    let x;

    // 1. res.json
    x = await call('GET', '/json');
    eq(x.s, 200, 'json.status');
    eq(x.h['content-type'], 'application/json; charset=utf-8', 'json.ct');
    eq(x.b, '{"a":1,"b":"x"}', 'json.body');
    eq(x.h['content-length'], '15', 'json.clen');
    eq(x.h['x-powered-by'], 'Express', 'json.xpb');
    eq(x.h['x-global'], 'G', 'json.global');

    // 2. res.send(string) -> text/html
    x = await call('GET', '/send-str');
    eq(x.s, 200, 'sendstr.status');
    eq(x.h['content-type'], 'text/html; charset=utf-8', 'sendstr.ct');
    eq(x.b, '<p>hi</p>', 'sendstr.body');

    // 3. res.send(Buffer) -> octet-stream
    x = await call('GET', '/send-buf');
    eq(x.h['content-type'], 'application/octet-stream', 'sendbuf.ct');
    eq(x.b, 'abc', 'sendbuf.body');

    // 4. res.send(object) -> json
    x = await call('GET', '/send-obj');
    eq(x.h['content-type'], 'application/json; charset=utf-8', 'sendobj.ct');
    eq(x.b, '{"k":2}', 'sendobj.body');

    // 5. res.type('txt')
    x = await call('GET', '/type');
    eq(x.h['content-type'], 'text/plain; charset=utf-8', 'type.ct');
    eq(x.b, 'plain', 'type.body');

    // 5b. res.type('application/json')
    x = await call('GET', '/typeext');
    eq(x.h['content-type'], 'application/json; charset=utf-8', 'typeext.ct');

    // 6. res.sendStatus(204)
    x = await call('GET', '/ss204');
    eq(x.s, 204, 'ss204.status');
    eq(x.b, '', 'ss204.body');

    // 7. res.sendStatus(404)
    x = await call('GET', '/ss404');
    eq(x.s, 404, 'ss404.status');
    eq(x.h['content-type'], 'text/plain; charset=utf-8', 'ss404.ct');
    eq(x.b, 'Not Found', 'ss404.body');

    // 8. res.redirect default 302
    x = await call('GET', '/redir');
    eq(x.s, 302, 'redir.status');
    eq(x.h['location'], '/dest', 'redir.loc');
    eq(x.b, 'Found. Redirecting to /dest', 'redir.body');

    // 9. res.redirect custom code 301
    x = await call('GET', '/redir301');
    eq(x.s, 301, 'redir301.status');
    eq(x.h['location'], '/dest', 'redir301.loc');
    eq(x.b, 'Moved Permanently. Redirecting to /dest', 'redir301.body');

    // 10. res.cookie -> Set-Cookie
    x = await call('GET', '/cookie');
    chk(Array.isArray(x.h['set-cookie']) && x.h['set-cookie'].length === 1, 'cookie.arr');
    eq(x.h['set-cookie'][0], 'foo=bar; Path=/', 'cookie.val');

    // 11. res.links -> Link header
    x = await call('GET', '/links');
    eq(x.h['link'], '<http://x/2>; rel="next", <http://x/5>; rel="last"', 'links.hdr');

    // 12. res.vary -> Vary header
    x = await call('GET', '/vary');
    eq(x.h['vary'], 'Accept', 'vary.hdr');

    // 12b. res.location
    x = await call('GET', '/loc');
    eq(x.s, 200, 'loc.status');
    eq(x.h['location'], '/somewhere', 'loc.hdr');

    // 13. default 404 (no route matched)
    x = await call('GET', '/nope');
    eq(x.s, 404, 'no404.status');
    eq(x.h['content-type'], 'text/html; charset=utf-8', 'no404.ct');
    eq(x.b, '<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot GET /nope</pre>\n</body>\n</html>\n', 'no404.body');

    // 13b. custom 404 handler under /missing
    x = await call('GET', '/missing/whatever');
    eq(x.s, 404, 'cust404.status');
    eq(x.h['content-type'], 'text/plain; charset=utf-8', 'cust404.ct');
    eq(x.b, 'custom-404', 'cust404.body');

    // 14. express.json body parsing
    x = await call('POST', '/pjson', JSON.stringify({ n: 5, s: 'hi' }), { 'Content-Type': 'application/json' });
    eq(x.s, 200, 'pjson.status');
    eq(x.b, '{"n":5,"s":"hi"}', 'pjson.body');

    // 15. express.urlencoded (extended) body parsing
    x = await call('POST', '/pform', 'a=1&b=two&c[d]=3', { 'Content-Type': 'application/x-www-form-urlencoded' });
    eq(x.b, '{"a":"1","b":"two","c":{"d":"3"}}', 'pform.body');

    // 15b. express.text body parsing
    x = await call('POST', '/ptext', 'raw text here', { 'Content-Type': 'text/plain' });
    eq(x.b, 'text-body:raw text here', 'ptext.body');

    // 16. request API: req.method/path/is/accepts/get
    x = await call('GET', '/reqinfo', null, { 'X-Custom': 'CV', 'Accept': 'application/json' });
    eq(x.b, '{"method":"GET","path":"/reqinfo","isjson":null,"acc":"json","h":"CV"}', 'reqinfo.body');

    // 16b. extended request API
    x = await call('GET', '/reqx?z=1', null, { 'X-Requested-With': 'XMLHttpRequest' });
    eq(x.b, '{"proto":"http","host":"127.0.0.1","ip":"127.0.0.1","xhr":true,"ourl":"/reqx?z=1","burl":"","url":"/reqx?z=1","secure":false}', 'reqx.body');

    // 17. res.append (multi-value header)
    x = await call('GET', '/append');
    eq(x.h['x-multi'], 'a, b', 'append.hdr');
    eq(x.b, 'ap', 'append.body');

    // 18. res.set / res.get
    x = await call('GET', '/getset');
    eq(x.b, '{"got":"FV"}', 'getset.body');
    eq(x.h['x-foo'], 'FV', 'getset.hdr');

    // 18b. res.set(object)
    x = await call('GET', '/setobj');
    eq(x.h['x-a'], '1', 'setobj.a');
    eq(x.h['x-b'], '2', 'setobj.b');

    // 19. HEAD verb explicit
    x = await call('HEAD', '/h');
    eq(x.s, 200, 'head.status');
    eq(x.h['x-head'], 'yes', 'head.hdr');
    eq(x.b, '', 'head.body');

    // 19b. HEAD auto-handled for a GET route
    x = await call('HEAD', '/headauto');
    eq(x.s, 200, 'headauto.status');
    eq(x.b, '', 'headauto.body');
    eq(x.h['content-length'], '7', 'headauto.clen');

    // 20. app.all
    x = await call('GET', '/all');
    eq(x.b, 'all:GET', 'all.get');
    x = await call('DELETE', '/all');
    eq(x.b, 'all:DELETE', 'all.del');

    // 21. put / patch / delete verbs
    x = await call('PUT', '/put');     eq(x.b, 'put-ok', 'put.body');
    x = await call('PATCH', '/patch'); eq(x.b, 'patch-ok', 'patch.body');
    x = await call('DELETE', '/del');  eq(x.b, 'del-ok', 'del.body');

    // 22. regex route
    x = await call('GET', '/regex/123');
    eq(x.s, 200, 'regex.status');
    eq(x.b, 'regexmatch', 'regex.body');
    x = await call('GET', '/regex/abc');
    eq(x.s, 404, 'regex.no');

    // 23/24. error handling (throw + next(err))
    x = await call('GET', '/err');
    eq(x.s, 500, 'err.status');
    eq(x.b, '{"error":"boom"}', 'err.body');
    x = await call('GET', '/nexterr');
    eq(x.s, 500, 'nexterr.status');
    eq(x.b, '{"error":"viaNext"}', 'nexterr.body');

    // 25/26. Router + router.param
    x = await call('GET', '/api/info');
    eq(x.b, '{"where":"router-info"}', 'router.info');
    x = await call('GET', '/api/item/42');
    eq(x.b, '{"pid":"P42","id":"42"}', 'router.param');

    // 27. path-scoped middleware + global flag
    x = await call('GET', '/scoped/x');
    eq(x.b, 'flag:SCOPED,GLOBAL', 'scoped.body');

    // 28. next('route') skip
    x = await call('GET', '/skip');
    eq(x.b, 'after-skip', 'skip.body');

    // 29. app.route().get().post()
    x = await call('GET', '/chained');  eq(x.b, 'chained-GET', 'chained.get');
    x = await call('POST', '/chained'); eq(x.b, 'chained-POST', 'chained.post');

    // 30/31/32. params / multi-params / query
    x = await call('GET', '/users/77');   eq(x.b, 'user:77', 'param.single');
    x = await call('GET', '/a/11/b/22');  eq(x.b, '{"x":"11","y":"22"}', 'param.multi');
    x = await call('GET', '/q?q=hello&n=2'); eq(x.b, '{"q":"hello","n":"2"}', 'query.basic');
    x = await call('GET', '/qarr?arr=1&arr=2&o[k]=v'); eq(x.b, '{"arr":["1","2"],"o":{"k":"v"}}', 'query.nested');

    // 33. express.static
    x = await call('GET', '/files/hello.txt');
    eq(x.s, 200, 'static.status');
    eq(x.h['content-type'], 'text/plain; charset=UTF-8', 'static.ct');
    eq(x.h['content-length'], '19', 'static.clen');
    eq(x.b, 'hello static world\n', 'static.body');

    // 34. multi-handler chain via next()
    x = await call('GET', '/arr');
    eq(x.b, 'step:1', 'arr.body');

    // 35. res.status chaining
    x = await call('GET', '/status201');
    eq(x.s, 201, 'status201.status');
    eq(x.b, 'created', 'status201.body');
    x = await call('GET', '/jstatus');
    eq(x.s, 422, 'jstatus.status');
    eq(x.b, '{"e":"bad"}', 'jstatus.body');

    // 36. res.format negotiation
    x = await call('GET', '/format', null, { 'Accept': 'application/json' });
    eq(x.b, '{"f":"json"}', 'format.json');
    eq(x.h['content-type'], 'application/json; charset=utf-8', 'format.json.ct');
    x = await call('GET', '/format', null, { 'Accept': 'text/plain' });
    eq(x.b, 'plain-fmt', 'format.text');

    // 37. res.jsonp
    x = await call('GET', '/jsonp?callback=cb');
    eq(x.h['content-type'], 'text/javascript; charset=utf-8', 'jsonp.ct');
    eq(x.b, "/**/ typeof cb === 'function' && cb({\"j\":1});", 'jsonp.body');

    // 38. res.attachment -> Content-Disposition
    x = await call('GET', '/attach');
    eq(x.h['content-disposition'], 'attachment; filename="report.pdf"', 'attach.cd');
    eq(x.h['content-type'], 'application/pdf; charset=utf-8', 'attach.ct');
    eq(x.b, 'PDF', 'attach.body');

    // 39. app settings introspection
    x = await call('GET', '/settings');
    eq(x.b, '{"cs":"CVAL","en":true,"dis":true,"loc":"CarpetApp"}', 'settings.body');

    // 39b. x-powered-by default value
    x = await call('GET', '/xpb');
    eq(x.h['x-powered-by'], 'Express', 'xpb.hdr');

    // 40. nested sub-application mount (req.baseUrl)
    x = await call('GET', '/mounted/ping');
    eq(x.b, 'sub-pong baseUrl=/mounted', 'mounted.body');

    // 41. pug view rendering via res.render
    x = await call('GET', '/view');
    eq(x.s, 200, 'view.status');
    eq(x.h['content-type'], 'text/html; charset=utf-8', 'view.ct');
    eq(x.b, '<html><head><title>T</title></head><body><h1>H</h1><p>Hello World</p></body></html>', 'view.body');

  } catch (e) {
    fail++;
    console.log('FAIL exception: ' + (e && e.stack ? e.stack : e));
  } finally {
    server.close(() => {
      console.log('EXPRESS_RESULT ok=' + ok + ' fail=' + fail);
      if (fail === 0) console.log('EXPRESS_DONE');
      process.exit(fail === 0 ? 0 : 1);
    });
  }
});
