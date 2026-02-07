import { spawnSync } from "node:child_process";
import { chromium, firefox, webkit, Route, Request, BrowserType } from "playwright";

type PepResponse = {
  status: number;
  headers: Array<[string, string]>;
  body_base64?: string | null;
  error?: {
    code?: string;
    message?: string;
  } | null;
};

const DEFAULT_CID = process.env.PEP_VSOCK_CID || "2";
const DEFAULT_PORT = process.env.PEP_VSOCK_PORT || "4040";
const PROJECT_ROOT = process.env.PEP_PROJECT_ROOT || "/Users/on/p/pexi";
const CLIENT_BIN =
  process.env.PEP_VSOCK_CLIENT ||
  `${PROJECT_ROOT}/pep-daemon/target/debug/avf-vsock-host`;

const ALLOW_URLS = (process.env.PEP_BROWSER_ALLOW_URLS || "https://example.com")
  .split(",")
  .map((entry) => entry.trim())
  .filter(Boolean);
const DENY_URL = process.env.PEP_BROWSER_DENY_URL || "";
const ENGINE = (process.env.PEP_BROWSER_ENGINE || "chromium").toLowerCase();

function isPassthroughScheme(url: string): boolean {
  return (
    url.startsWith("data:") ||
    url.startsWith("blob:") ||
    url.startsWith("about:") ||
    url.startsWith("file:")
  );
}

function normalizeHeaders(headers: Record<string, string>): Array<[string, string]> {
  return Object.entries(headers);
}

function toHeaderObject(headers: Array<[string, string]>): Record<string, string> {
  const result: Record<string, string> = {};
  for (const [key, value] of headers) {
    result[key] = value;
  }
  return result;
}

function pepFetch(request: Request): PepResponse {
  const method = request.method();
  const url = request.url();
  const headers = normalizeHeaders(request.headers());
  const body = request.postDataBuffer();

  const args = [
    "vsock-client",
    "--cid",
    DEFAULT_CID,
    "--port",
    DEFAULT_PORT,
    "--url",
    url,
    "--method",
    method,
  ];

  for (const [key, value] of headers) {
    args.push("--header", `${key}: ${value}`);
  }

  if (body && body.length > 0) {
    args.push("--body-stdin");
  }

  const payload = spawnSync(CLIENT_BIN, args, {
    input: body && body.length > 0 ? body : undefined,
    encoding: "utf8",
  });

  if (payload.status !== 0) {
    const message = payload.stderr || payload.stdout || `${payload.status}`;
    return {
      status: 502,
      headers: [],
      body_base64: Buffer.from(`vsock client failed: ${message}`).toString(
        "base64"
      ),
    };
  }

  return JSON.parse(payload.stdout) as PepResponse;
}

function errorStatus(code?: string): number {
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

async function handleRoute(route: Route, request: Request): Promise<void> {
  const url = request.url();
  const resourceType = request.resourceType();

  if (isPassthroughScheme(url)) {
    await route.continue();
    return;
  }

  if (!url.startsWith("http://") && !url.startsWith("https://")) {
    await route.abort();
    return;
  }

  const response = pepFetch(request);
  if (response.error) {
    const code = response.error.code || "unknown_error";
    const message = response.error.message || "unknown error";
    const body = Buffer.from(`host error (${code}): ${message}`);
    await route.fulfill({
      status: errorStatus(code),
      headers: { "content-type": "text/plain" },
      body,
    });
    console.warn(`deny ${resourceType} ${url} -> ${code}`);
    return;
  }

  const body = response.body_base64
    ? Buffer.from(response.body_base64, "base64")
    : Buffer.alloc(0);
  const headers = toHeaderObject(response.headers);
  await route.fulfill({
    status: response.status || 200,
    headers,
    body,
  });
}

async function main(): Promise<void> {
  let browserType: BrowserType;
  switch (ENGINE) {
    case "firefox":
      browserType = firefox;
      break;
    case "webkit":
      browserType = webkit;
      break;
    default:
      browserType = chromium;
      break;
  }
  const browser = await browserType.launch({ headless: true });
  const context = await browser.newContext({ acceptDownloads: true });
  await context.route("**/*", (route, request) => {
    handleRoute(route, request).catch((err) => {
      console.error("route error:", err);
      route.abort().catch(() => undefined);
    });
  });

  for (const url of ALLOW_URLS) {
    const page = await context.newPage();
    console.log(`navigate: ${url}`);
    const response = await page.goto(url, { waitUntil: "domcontentloaded" });
    console.log(`status=${response?.status() ?? 0}`);
    const title = await page.title();
    console.log(`title=${title}`);
    await page.close();
  }

  if (DENY_URL) {
    const page = await context.newPage();
    console.log(`navigate (deny): ${DENY_URL}`);
    try {
      await page.goto(DENY_URL, { waitUntil: "domcontentloaded", timeout: 15000 });
      const title = await page.title();
      console.log(`deny title=${title}`);
    } catch (err) {
      console.log("deny navigation blocked");
    }
    await page.close();
  }

  await browser.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
