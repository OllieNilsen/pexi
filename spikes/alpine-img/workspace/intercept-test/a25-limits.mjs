/**
 * Milestone A2.5: Buffered Fulfill Limits Spike
 *
 * Load an asset-heavy site (~5–20MB) via browser interception through PEP.
 * Record per-request latency, total bytes, memory usage, and any cap violations.
 *
 * Acceptance criteria:
 *   - Page renders under caps OR fails with explicit policy error.
 *   - Memory usage and added latency recorded in /workspace/a25-results.json.
 */

import { createConnection } from 'net';
import puppeteer from 'puppeteer-core';
import { writeFileSync } from 'fs';

const PEP_HOST = '127.0.0.1';
const PEP_PORT = parseInt(process.env.PEP_PORT || '4040');
const TARGET_URL = process.env.A25_TARGET_URL || 'https://en.wikipedia.org/wiki/Earth';
const NAV_TIMEOUT = parseInt(process.env.A25_NAV_TIMEOUT || '90000');
const RESULTS_PATH = process.env.A25_RESULTS_PATH || '/workspace/a25-results.json';

// ── PEP client (length-prefixed JSON over TCP→vsock) ──────────────────────

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
        buf = buf.subarray(4);
      }
      if (expectedLen !== null && buf.length >= expectedLen) {
        resolved = true;
        const json = buf.subarray(0, expectedLen).toString('utf8');
        sock.end();
        try { resolve(JSON.parse(json)); }
        catch (e) { reject(new Error(`bad JSON: ${json.slice(0, 200)}`)); }
      }
    });

    sock.on('error', (err) => { if (!resolved) reject(err); });
    sock.on('end', () => { if (!resolved) reject(new Error('PEP empty response')); });
    sock.setTimeout(30000, () => { sock.destroy(); if (!resolved) reject(new Error('PEP timeout')); });
  });
}

// ── Helpers ────────────────────────────────────────────────────────────────

function waitForPort(host, port, timeoutMs = 15000) {
  const start = Date.now();
  return new Promise((resolve) => {
    const attempt = () => {
      if (Date.now() - start > timeoutMs) return resolve(false);
      const sock = createConnection({ host, port }, () => { sock.end(); resolve(true); });
      sock.on('error', () => setTimeout(attempt, 500));
      sock.setTimeout(1000, () => { sock.destroy(); setTimeout(attempt, 500); });
    };
    attempt();
  });
}

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const idx = Math.ceil((p / 100) * sorted.length) - 1;
  return sorted[Math.max(0, idx)];
}

// ── Main ──────────────────────────────────────────────────────────────────

async function main() {
  const runStart = Date.now();

  // Wait for Firefox and PEP
  console.log('A2.5: waiting for Firefox port 9222...');
  if (!await waitForPort('127.0.0.1', 9222, 20000)) {
    throw new Error('Firefox not available on port 9222');
  }
  console.log('A2.5: Firefox ready');

  console.log(`A2.5: waiting for PEP port ${PEP_PORT}...`);
  if (!await waitForPort(PEP_HOST, PEP_PORT, 10000)) {
    throw new Error(`PEP not available on port ${PEP_PORT}`);
  }
  console.log('A2.5: PEP ready');

  // Connect to Firefox
  const browser = await puppeteer.connect({
    browserWSEndpoint: 'ws://127.0.0.1:9222/session',
    protocol: 'webDriverBiDi',
  });
  console.log('A2.5: connected:', await browser.version());

  const page = await browser.newPage();
  await page.setRequestInterception(true);

  // Per-request metrics
  const requestLog = [];
  let totalBytes = 0;
  let requestCount = 0;
  let deniedCount = 0;
  let cappedCount = 0;
  let errorCount = 0;

  page.on('request', async (request) => {
    const url = request.url();
    const method = request.method();
    const startMs = Date.now();

    // Pass through non-HTTP schemes (data:, blob:, about:)
    if (!url.startsWith('http://') && !url.startsWith('https://')) {
      try { await request.continue(); } catch { /* ignore */ }
      return;
    }

    requestCount++;
    const entry = { url: url.slice(0, 200), method, startMs, latencyMs: 0, bytes: 0, status: 0, error: null };

    try {
      let headers = [];
      try {
        const h = request.headers();
        headers = Object.entries(h);
      } catch { /* headers unavailable in BiDi */ }

      let body = null;
      try { body = request.postData(); } catch { /* no post data */ }

      const response = await pepFetch(method, url, headers, body);
      entry.latencyMs = Date.now() - startMs;

      if (response.error) {
        entry.error = response.error.code;
        entry.status = 0;

        if (response.error.code === 'constraint_violation') {
          cappedCount++;
          console.log(`A2.5: CAPPED ${url.slice(0, 80)} (${response.error.message})`);
        } else {
          deniedCount++;
          console.log(`A2.5: DENIED ${url.slice(0, 80)} (${response.error.code})`);
        }

        await request.respond({
          status: 403,
          contentType: 'text/plain',
          body: `Blocked: ${response.error.code} - ${response.error.message}`,
        });
        requestLog.push(entry);
        return;
      }

      const responseBody = response.body_base64
        ? Buffer.from(response.body_base64, 'base64')
        : Buffer.alloc(0);

      entry.bytes = responseBody.length;
      entry.status = response.status || 200;
      totalBytes += responseBody.length;

      const responseHeaders = {};
      for (const [k, v] of (response.headers || [])) {
        responseHeaders[k] = v;
      }

      await request.respond({
        status: response.status || 200,
        headers: responseHeaders,
        body: responseBody,
      });

      if (requestCount % 10 === 0) {
        console.log(`A2.5: ${requestCount} requests, ${(totalBytes / (1024 * 1024)).toFixed(2)} MB so far`);
      }
    } catch (err) {
      entry.latencyMs = Date.now() - startMs;
      entry.error = `exception: ${err.message}`;
      errorCount++;
      console.error(`A2.5: ERROR ${url.slice(0, 80)}: ${err.message}`);

      try {
        await request.respond({
          status: 502,
          contentType: 'text/plain',
          body: `PEP error: ${err.message}`,
        });
      } catch {
        /* respond failed, request may be gone */
      }
    }
    requestLog.push(entry);
  });

  // ── Navigate to asset-heavy page ──────────────────────────────────────

  console.log(`\nA2.5: === Loading ${TARGET_URL} ===`);
  const navStart = Date.now();
  let navStatus = null;
  let navError = null;
  let pageTitle = null;

  try {
    const resp = await page.goto(TARGET_URL, {
      waitUntil: 'domcontentloaded',
      timeout: NAV_TIMEOUT,
    });
    navStatus = resp?.status();
    console.log(`A2.5: navigation status: ${navStatus}`);

    // Give time for subresources to load
    await new Promise(r => setTimeout(r, 10000));

    pageTitle = await page.title();
    console.log(`A2.5: page title: ${pageTitle}`);
  } catch (err) {
    navError = err.message?.slice(0, 300);
    console.log(`A2.5: navigation error: ${navError}`);
  }

  const navElapsedMs = Date.now() - navStart;

  // ── Collect metrics ───────────────────────────────────────────────────

  const memUsage = process.memoryUsage();
  const latencies = requestLog
    .filter(r => r.latencyMs > 0)
    .map(r => r.latencyMs)
    .sort((a, b) => a - b);

  const results = {
    milestone: 'A2.5',
    target_url: TARGET_URL,
    timestamp: new Date().toISOString(),
    navigation: {
      status: navStatus,
      error: navError,
      page_title: pageTitle,
      elapsed_ms: navElapsedMs,
    },
    totals: {
      requests: requestCount,
      bytes: totalBytes,
      bytes_mb: +(totalBytes / (1024 * 1024)).toFixed(2),
      denied: deniedCount,
      capped: cappedCount,
      errors: errorCount,
      successful: requestCount - deniedCount - cappedCount - errorCount,
    },
    latency: {
      min_ms: latencies[0] || 0,
      max_ms: latencies[latencies.length - 1] || 0,
      median_ms: percentile(latencies, 50),
      p95_ms: percentile(latencies, 95),
      p99_ms: percentile(latencies, 99),
      count: latencies.length,
    },
    memory: {
      rss_mb: +(memUsage.rss / (1024 * 1024)).toFixed(2),
      heap_used_mb: +(memUsage.heapUsed / (1024 * 1024)).toFixed(2),
      heap_total_mb: +(memUsage.heapTotal / (1024 * 1024)).toFixed(2),
      external_mb: +(memUsage.external / (1024 * 1024)).toFixed(2),
      array_buffers_mb: +((memUsage.arrayBuffers || 0) / (1024 * 1024)).toFixed(2),
    },
    elapsed_total_ms: Date.now() - runStart,
    // Include the 10 largest responses for analysis
    largest_responses: requestLog
      .filter(r => r.bytes > 0)
      .sort((a, b) => b.bytes - a.bytes)
      .slice(0, 10)
      .map(r => ({
        url: r.url,
        bytes: r.bytes,
        bytes_kb: +(r.bytes / 1024).toFixed(1),
        latency_ms: r.latencyMs,
        status: r.status,
      })),
    // Include all denied/capped requests
    denied_or_capped: requestLog
      .filter(r => r.error)
      .map(r => ({
        url: r.url,
        error: r.error,
        latency_ms: r.latencyMs,
      })),
  };

  // ── Write results ─────────────────────────────────────────────────────

  console.log('\n=== A2.5 Results Summary ===');
  console.log(`Requests:  ${results.totals.requests} (${results.totals.successful} ok, ${results.totals.denied} denied, ${results.totals.capped} capped, ${results.totals.errors} errors)`);
  console.log(`Total:     ${results.totals.bytes_mb} MB`);
  console.log(`Latency:   min=${results.latency.min_ms}ms median=${results.latency.median_ms}ms p95=${results.latency.p95_ms}ms max=${results.latency.max_ms}ms`);
  console.log(`Memory:    rss=${results.memory.rss_mb}MB heap=${results.memory.heap_used_mb}MB external=${results.memory.external_mb}MB`);
  console.log(`Elapsed:   ${results.elapsed_total_ms}ms`);

  try {
    writeFileSync(RESULTS_PATH, JSON.stringify(results, null, 2));
    console.log(`\nResults written to ${RESULTS_PATH}`);
  } catch (err) {
    console.error(`Failed to write results: ${err.message}`);
    // Fall back to stdout
    console.log(JSON.stringify(results, null, 2));
  }

  await browser.close();
  console.log('A2.5: done');
}

main().catch(err => {
  console.error('A2.5 fatal:', err.message);
  // Write a failure result so we can see what happened
  try {
    writeFileSync(RESULTS_PATH, JSON.stringify({
      milestone: 'A2.5',
      error: err.message,
      timestamp: new Date().toISOString(),
    }, null, 2));
  } catch { /* best effort */ }
  process.exit(1);
});
