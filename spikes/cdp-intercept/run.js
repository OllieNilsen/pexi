const CDP = require("chrome-remote-interface");
const { spawn, spawnSync } = require("node:child_process");
const net = require("node:net");

const PROJECT_ROOT = process.env.PEP_PROJECT_ROOT || "/Users/on/p/pexi";
const CHROMIUM_BIN =
  process.env.CHROMIUM_BIN ||
  `${PROJECT_ROOT}/spikes/ubuntu-img/chromium/chrome-linux64/chrome`;
const DEBUG_PORT = Number(process.env.CDP_DEBUG_PORT || 9222);

const DEFAULT_CID = process.env.PEP_VSOCK_CID || "2";
const DEFAULT_PORT = process.env.PEP_VSOCK_PORT || "4040";
const CLIENT_BIN =
  process.env.PEP_VSOCK_CLIENT ||
  `${PROJECT_ROOT}/pep-daemon/target/debug/avf-vsock-host`;

const ALLOW_URLS = (process.env.PEP_BROWSER_ALLOW_URLS || "https://example.com")
  .split(",")
  .map((entry) => entry.trim())
  .filter(Boolean);
const DENY_URL = process.env.PEP_BROWSER_DENY_URL || "";

function isPassthroughScheme(url) {
  return (
    url.startsWith("data:") ||
    url.startsWith("blob:") ||
    url.startsWith("about:") ||
    url.startsWith("file:")
  );
}

function pepFetch(method, url, headers, body) {
  const request = {
    method,
    url,
    headers: headers || {},
    body_base64: body ? Buffer.from(body).toString("base64") : null,
  };

  const helperScript = `${PROJECT_ROOT}/spikes/cdp-intercept/vsock_pep.py`;
  const env = {
    ...process.env,
    PEP_VSOCK_CID: DEFAULT_CID,
    PEP_VSOCK_PORT: DEFAULT_PORT,
  };

  const payload = spawnSync("python3", [helperScript], {
    input: JSON.stringify(request),
    encoding: "utf8",
    env,
    timeout: 20000,
  });

  if (payload.status !== 0) {
    const message = payload.stderr || payload.stdout || `exit ${payload.status}`;
    console.error(`[pepFetch] python helper failed: ${message}`);
    return {
      error: { code: "vsock_error", message },
    };
  }

  try {
    return JSON.parse(payload.stdout);
  } catch (err) {
    console.error(`[pepFetch] bad JSON: ${payload.stdout}`);
    return {
      error: { code: "parse_error", message: `bad response: ${payload.stdout}` },
    };
  }
}

function errorStatus(code) {
  if (!code) return 502;
  if (
    code === "denied_by_policy" ||
    code === "redirect_blocked" ||
    code === "ssrf_blocked" ||
    code === "constraint_violation"
  ) {
    return 403;
  }
  return 502;
}

function waitForPort(port, timeoutMs) {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const tryOnce = () => {
      const socket = net.connect({ port }, () => {
        socket.end();
        resolve();
      });
      socket.on("error", () => {
        socket.destroy();
        if (Date.now() - start > timeoutMs) {
          reject(new Error(`Timed out waiting for port ${port}`));
        } else {
          setTimeout(tryOnce, 200);
        }
      });
    };
    tryOnce();
  });
}

async function main() {
  const chromiumEnv = {
    ...process.env,
    LD_LIBRARY_PATH: "/usr/lib/chromium:/usr/lib/aarch64-linux-gnu",
    CHROME_DEVEL_SANDBOX: "/usr/lib/chromium/chrome-sandbox",
  };
  console.log(`node platform: ${process.platform} arch: ${process.arch}`);
  const versionResult = spawnSync(CHROMIUM_BIN, ["--version"], {
    encoding: "utf8",
    env: chromiumEnv,
  });
  if (versionResult.status === 0) {
    console.log(`chromium version: ${versionResult.stdout.trim()}`);
  } else {
    console.log(
      `chromium version error: ${versionResult.stderr || versionResult.stdout}`
    );
  }
  const chromeArgs = [
    `--remote-debugging-port=${DEBUG_PORT}`,
    "--headless",
    "--disable-gpu",
    "--disable-extensions",
    "--disable-software-rasterizer",
    "--no-sandbox",
    "--disable-dev-shm-usage",
    "--user-data-dir=/tmp/chromium-profile",
    '--host-resolver-rules=MAP * 127.0.0.1',
    "about:blank",
  ];
  console.log(`chromium bin: ${CHROMIUM_BIN}`);
  console.log(`chromium args: ${chromeArgs.join(" ")}`);
  const chrome = spawn(CHROMIUM_BIN, chromeArgs, {
    stdio: "pipe",
    env: chromiumEnv,
  });
  chrome.on("error", (err) => {
    console.error("chromium spawn error:", err);
  });
  chrome.on("exit", (code, signal) => {
    console.log(`chromium exit code=${code} signal=${signal || "none"}`);
  });
  chrome.on("close", (code, signal) => {
    console.log(`chromium close code=${code} signal=${signal || "none"}`);
  });
  chrome.stdout.on("data", (chunk) => {
    process.stdout.write(chunk);
  });
  chrome.stderr.on("data", (chunk) => {
    process.stderr.write(chunk);
  });

  await waitForPort(DEBUG_PORT, 15000);

  // Wait briefly for Chrome to register targets after port opens
  await new Promise((r) => setTimeout(r, 1000));

  // Retry CDP connection (targets may take a moment to appear)
  let client;
  for (let attempt = 0; attempt < 5; attempt++) {
    try {
      client = await CDP({ port: DEBUG_PORT });
      break;
    } catch (err) {
      console.log(`CDP connect attempt ${attempt + 1}/5 failed: ${err.message}`);
      if (attempt === 4) throw err;
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  const { Fetch, Page, Network } = client;

  await Network.enable();
  await Fetch.enable({ patterns: [{ urlPattern: "*", requestStage: "Request" }] });
  await Page.enable();

  Fetch.requestPaused(async (event) => {
    const { requestId, request } = event;
    const url = request.url;
    console.log(`[fetch-intercept] ${request.method} ${url}`);

    if (isPassthroughScheme(url)) {
      console.log(`[fetch-intercept] passthrough: ${url}`);
      await Fetch.continueRequest({ requestId });
      return;
    }
    if (!url.startsWith("http://") && !url.startsWith("https://")) {
      console.log(`[fetch-intercept] abort non-http: ${url}`);
      await Fetch.failRequest({ requestId, errorReason: "Aborted" });
      return;
    }

    console.log(`[fetch-intercept] pepFetch: ${request.method} ${url}`);
    const response = pepFetch(
      request.method,
      url,
      request.headers || {},
      event.request && event.request.postData
        ? Buffer.from(event.request.postData, "utf8")
        : null
    );

    if (response.error) {
      const code = response.error.code || "unknown_error";
      const message = response.error.message || "unknown error";
      const body = Buffer.from(`host error (${code}): ${message}`).toString(
        "base64"
      );
      await Fetch.fulfillRequest({
        requestId,
        responseCode: errorStatus(code),
        responseHeaders: [{ name: "content-type", value: "text/plain" }],
        body,
      });
      console.warn(`deny ${url} -> ${code}`);
      return;
    }

    const body = response.body_base64 || "";
    const responseHeaders = (response.headers || []).map(([name, value]) => ({
      name,
      value,
    }));
    await Fetch.fulfillRequest({
      requestId,
      responseCode: response.status || 200,
      responseHeaders,
      body,
    });
  });

  for (const url of ALLOW_URLS) {
    console.log(`navigate: ${url}`);
    const navResult = await Page.navigate({ url });
    console.log(`nav result: ${JSON.stringify(navResult)}`);
    await Page.loadEventFired();
    console.log(`load event fired for: ${url}`);
  }

  if (DENY_URL) {
    console.log(`navigate (deny): ${DENY_URL}`);
    try {
      const denyResult = await Page.navigate({ url: DENY_URL });
      console.log(`deny nav result: ${JSON.stringify(denyResult)}`);
      await Page.loadEventFired();
      console.log(`deny load event fired`);
    } catch (err) {
      console.log(`deny navigation error: ${err.message}`);
    }
  }

  await client.close();
  chrome.kill("SIGTERM");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
