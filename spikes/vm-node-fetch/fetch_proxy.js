const { spawnSync } = require("child_process");

const DEFAULT_CID = process.env.PEP_VSOCK_CID || "2";
const DEFAULT_PORT = process.env.PEP_VSOCK_PORT || "4040";
const CLIENT_BIN = process.env.PEP_VSOCK_CLIENT || "./avf-vsock-host";

function normalizeHeaders(headers) {
  if (!headers) return [];
  if (Array.isArray(headers)) return headers;
  if (headers instanceof Map) return Array.from(headers.entries());
  return Object.entries(headers);
}

async function fetchProxy(url, options = {}) {
  const method = options.method || "GET";
  const headers = normalizeHeaders(options.headers);
  const body = options.body ? Buffer.from(options.body).toString("base64") : null;

  const args = [
    "vsock-client",
    "--cid",
    DEFAULT_CID,
    "--port",
    DEFAULT_PORT,
    "--url",
    url.toString(),
    "--method",
    method,
    "--body-stdin",
  ];

  for (const [key, value] of headers) {
    args.push("--header", `${key}: ${value}`);
  }

  const payload = spawnSync(CLIENT_BIN, args, {
    input: body ? Buffer.from(body, "base64") : undefined,
    encoding: "utf8",
  });

  if (payload.status !== 0) {
    throw new Error(
      `vsock client failed: ${payload.stderr || payload.stdout || payload.status}`
    );
  }

  const response = JSON.parse(payload.stdout);
  if (response.error) {
    const message = response.error.message || "unknown error";
    throw new Error(`host error: ${message}`);
  }

  const responseBody = response.body_base64
    ? Buffer.from(response.body_base64, "base64")
    : Buffer.alloc(0);

  return new Response(responseBody, {
    status: response.status,
    headers: response.headers,
  });
}

globalThis.fetch = fetchProxy;

async function main() {
  const target = process.argv[2] || "https://example.com";
  const res = await fetch(target);
  const text = await res.text();
  console.log(`status=${res.status}`);
  console.log(text.slice(0, 200));
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
