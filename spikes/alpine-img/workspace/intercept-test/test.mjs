import { createConnection } from 'net';
import puppeteer from 'puppeteer-core';

const PEP_HOST = '127.0.0.1';
const PEP_PORT = parseInt(process.env.PEP_PORT || '4040');

function pepFetch(method, url, headers, body) {
  return new Promise((resolve, reject) => {
    let buf = Buffer.alloc(0);
    let expectedLen = null;
    let resolved = false;

    const sock = createConnection({ host: PEP_HOST, port: PEP_PORT }, () => {
      const request = {
        method,
        url,
        headers: headers || [],
        body_base64: body ? Buffer.from(body).toString('base64') : null,
      };
      const payload = Buffer.from(JSON.stringify(request), 'utf8');
      const lenBuf = Buffer.alloc(4);
      lenBuf.writeUInt32BE(payload.length);
      sock.write(lenBuf);
      sock.write(payload);
    });

    sock.on('data', (chunk) => {
      buf = Buffer.concat([buf, chunk]);
      if (expectedLen === null && buf.length >= 4) {
        expectedLen = buf.readUInt32BE(0);
        buf = buf.slice(4);
      }
      if (expectedLen !== null && buf.length >= expectedLen) {
        resolved = true;
        const json = buf.slice(0, expectedLen).toString('utf8');
        sock.end();
        try { resolve(JSON.parse(json)); }
        catch (e) { reject(new Error(`bad JSON: ${json.slice(0, 200)}`)); }
      }
    });

    sock.on('error', (err) => { if (!resolved) reject(err); });
    sock.on('end', () => { if (!resolved) reject(new Error('PEP empty response')); });
    sock.setTimeout(15000, () => { sock.destroy(); if (!resolved) reject(new Error('PEP timeout')); });
  });
}

async function waitForPort(host, port, timeoutMs = 15000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      await new Promise((resolve, reject) => {
        const sock = createConnection({ host, port }, () => { sock.end(); resolve(); });
        sock.on('error', reject);
        sock.setTimeout(1000, () => { sock.destroy(); reject(new Error('timeout')); });
      });
      return true;
    } catch { await new Promise(r => setTimeout(r, 500)); }
  }
  return false;
}

async function main() {
  console.log('waiting for Firefox port 9222...');
  await waitForPort('127.0.0.1', 9222, 15000);
  console.log('Firefox ready');

  console.log(`waiting for PEP port ${PEP_PORT}...`);
  await waitForPort(PEP_HOST, PEP_PORT, 10000);
  console.log('PEP ready');

  // Direct PEP test
  console.log('direct PEP test...');
  const t = await pepFetch('GET', 'https://example.com/', [], null);
  console.log(`direct test: status=${t.status}, bodyLen=${t.body_base64?.length || 0}`);

  // Connect Firefox
  console.log('connecting Firefox BiDi...');
  const browser = await puppeteer.connect({
    browserWSEndpoint: 'ws://127.0.0.1:9222/session',
    protocol: 'webDriverBiDi',
  });
  console.log('connected:', await browser.version());

  const page = await browser.newPage();
  
  // Check what API methods are available on request
  await page.setRequestInterception(true);

  let stats = { ok: 0, denied: 0, err: 0 };

  page.on('request', async (request) => {
    const url = request.url();
    const method = request.method();
    console.log(`\n[REQ] ${method} ${url}`);
    
    // Log available methods
    console.log(`  request keys: ${Object.getOwnPropertyNames(Object.getPrototypeOf(request)).join(', ')}`);

    if (!url.startsWith('http://') && !url.startsWith('https://')) {
      console.log('  -> continue (non-http)');
      try { await request.continue(); } catch(e) { console.log(`  continue err: ${e.message}`); }
      return;
    }

    try {
      // Get headers - might differ in BiDi
      let headers = [];
      try {
        const h = request.headers();
        console.log(`  headers type: ${typeof h}, keys: ${Object.keys(h).length}`);
        headers = Object.entries(h);
      } catch(e) {
        console.log(`  headers error: ${e.message}`);
      }
      
      let body = null;
      try { body = request.postData(); } catch(e) { console.log(`  postData error: ${e.message}`); }

      console.log(`  calling pepFetch...`);
      const response = await pepFetch(method, url, headers, body);
      console.log(`  pep response: status=${response.status}, error=${JSON.stringify(response.error)}`);

      if (response.error) {
        stats.denied++;
        console.log(`  -> denied: ${response.error.code}`);
        await request.respond({
          status: 403,
          contentType: 'text/plain',
          body: `Blocked: ${response.error.code}`,
        });
        return;
      }

      const responseBody = response.body_base64
        ? Buffer.from(response.body_base64, 'base64')
        : Buffer.alloc(0);
      
      const responseHeaders = {};
      for (const [k, v] of (response.headers || [])) {
        responseHeaders[k] = v;
      }

      console.log(`  -> fulfilling ${response.status} (${responseBody.length}b)`);
      await request.respond({
        status: response.status || 200,
        headers: responseHeaders,
        body: responseBody,
      });
      stats.ok++;
    } catch (err) {
      stats.err++;
      console.error(`  -> ERROR: name=${err.name} message=${err.message} stack=${err.stack?.slice(0, 300)}`);
      try {
        await request.respond({
          status: 502,
          contentType: 'text/plain',
          body: `PEP error: ${err.message}`,
        });
      } catch(e2) {
        console.error(`  respond also failed: ${e2.message}`);
      }
    }
  });

  // Test 1
  console.log('\n=== Test: example.com ===');
  try {
    const resp = await page.goto('https://example.com', { waitUntil: 'domcontentloaded', timeout: 20000 });
    console.log(`nav status: ${resp?.status()}`);
    const title = await page.title();
    console.log(`title: ${title}`);
  } catch (err) {
    console.log(`nav error: ${err.message?.slice(0, 200)}`);
  }

  // Test 2
  console.log('\n=== Test: evil.com (should be denied) ===');
  try {
    const resp2 = await page.goto('https://evil.com', { waitUntil: 'domcontentloaded', timeout: 10000 });
    console.log(`nav status: ${resp2?.status()}`);
  } catch (err) {
    console.log(`nav error: ${err.message?.slice(0, 200)}`);
  }

  console.log(`\nstats: ${JSON.stringify(stats)}`);
  await browser.close();
  console.log('done');
}

main().catch(err => { console.error('fatal:', err.message); process.exit(1); });
