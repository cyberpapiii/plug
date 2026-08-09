import { test, expect } from "@playwright/test";
import { createHash, randomBytes } from "node:crypto";
import { execFile, spawn } from "node:child_process";
import { request as httpRequest } from "node:http";
import { createServer } from "node:net";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const REPO_ROOT = resolve(import.meta.dirname, "../..");
const PLUG_BIN = join(REPO_ROOT, "target/debug/plug");
const MOCK_BIN = join(REPO_ROOT, "target/debug/mock-mcp-server");
const PUBLIC_ORIGIN = "https://plug.test";
const PUBLIC_HOST = "plug.test";
const CLIENT_ORIGIN = "https://client.test";
const CALLBACK_URL = `${CLIENT_ORIGIN}/callback`;
const RESOURCE = `${PUBLIC_ORIGIN}/mcp`;
const PROTOCOL_VERSION = "2025-11-25";

async function freePort() {
  return await new Promise((resolvePort, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(error => error ? reject(error) : resolvePort(address.port));
    });
  });
}

async function eventually(operation, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      return await operation();
    } catch (error) {
      lastError = error;
      await new Promise(resolveWait => setTimeout(resolveWait, 100));
    }
  }
  throw lastError || new Error("operation timed out");
}

async function loopbackRequest(port, path, { method = "GET", headers = {}, body } = {}) {
  return await new Promise((resolveResponse, reject) => {
    const request = httpRequest({
      hostname: "127.0.0.1",
      port,
      path,
      method,
      headers: { ...headers, Host: PUBLIC_HOST },
    }, response => {
      const chunks = [];
      response.on("data", chunk => chunks.push(chunk));
      response.once("error", reject);
      response.once("end", () => {
        const payload = Buffer.concat(chunks);
        const responseHeaders = new Headers();
        for (let index = 0; index < response.rawHeaders.length; index += 2) {
          responseHeaders.append(response.rawHeaders[index], response.rawHeaders[index + 1]);
        }
        resolveResponse({
          status: response.statusCode,
          ok: response.statusCode >= 200 && response.statusCode < 300,
          headers: responseHeaders,
          json: async () => JSON.parse(payload.toString("utf8")),
          text: async () => payload.toString("utf8"),
          arrayBuffer: async () => payload,
        });
      });
    });
    request.once("error", reject);
    if (body !== undefined) request.write(body);
    request.end();
  });
}

function formBody(fields) {
  return new URLSearchParams(fields).toString();
}

function pkce() {
  const verifier = randomBytes(48).toString("base64url");
  const challenge = createHash("sha256").update(verifier).digest("base64url");
  return { verifier, challenge };
}

async function findIssuerState(root) {
  const entries = await readdir(root, { recursive: true, withFileTypes: true });
  const state = entries.find(entry => entry.isFile() && /^issuer-v3-[a-f0-9]+\.json$/.test(entry.name));
  if (!state) throw new Error(`issuer state not found under ${root}`);
  return join(state.parentPath || state.path || root, state.name);
}

class PlugProcess {
  constructor(root, port, configPath, environment) {
    this.root = root;
    this.port = port;
    this.configPath = configPath;
    this.environment = environment;
    this.children = [];
    this.output = [];
    this.child = null;
  }

  static async create() {
    // Keep Unix-domain socket path below macOS SUN_LEN while retaining full isolation.
    const root = await mkdtemp("/tmp/plug-e2e-");
    const port = await freePort();
    const configPath = join(root, "config.toml");
    const environment = {
      ...process.env,
      HOME: root,
      XDG_CONFIG_HOME: join(root, "config"),
      XDG_CACHE_HOME: join(root, "cache"),
      XDG_DATA_HOME: join(root, "data"),
      XDG_STATE_HOME: join(root, "state"),
      XDG_RUNTIME_DIR: join(root, "run"),
      PLUG_LOG: "info",
      NO_COLOR: "1",
    };
    await Promise.all([
      mkdir(dirname(configPath), { recursive: true }),
      mkdir(environment.XDG_CONFIG_HOME, { recursive: true }),
      mkdir(environment.XDG_CACHE_HOME, { recursive: true }),
      mkdir(environment.XDG_DATA_HOME, { recursive: true }),
      mkdir(environment.XDG_STATE_HOME, { recursive: true }),
      mkdir(environment.XDG_RUNTIME_DIR, { recursive: true }),
    ]);
    const config = `
log_level = "info"
enable_prefix = true

[http]
auth_mode = "oauth"
public_base_url = "${PUBLIC_ORIGIN}"
oauth_scopes = ["tools:read", "offline_access"]
bind_address = "127.0.0.1"
port = ${port}

[servers.browser_fixture]
command = "${MOCK_BIN.replaceAll("\\", "\\\\")}"
args = ["--tools", "echo"]
`;
    await writeFile(configPath, config, { mode: 0o600 });
    return new PlugProcess(root, port, configPath, environment);
  }

  async start() {
    if (this.child) throw new Error("Plug process already running");
    const child = spawn(PLUG_BIN, ["serve", "--config", this.configPath], {
      cwd: REPO_ROOT,
      env: this.environment,
      stdio: ["ignore", "pipe", "pipe"],
    });
    this.child = child;
    this.children.push(child);
    child.stdout.on("data", chunk => this.output.push(chunk.toString()));
    child.stderr.on("data", chunk => this.output.push(chunk.toString()));
    await eventually(async () => {
      if (child.exitCode !== null) {
        throw new Error(`Plug exited during startup (${child.exitCode}): ${this.output.join("")}`);
      }
      const response = await loopbackRequest(this.port, "/.well-known/oauth-authorization-server");
      if (!response.ok) throw new Error(`Plug not ready: HTTP ${response.status}`);
    });
  }

  async stop() {
    const child = this.child;
    this.child = null;
    if (!child || child.exitCode !== null) return;
    child.kill("SIGTERM");
    const exited = await Promise.race([
      new Promise(resolveExit => child.once("exit", () => resolveExit(true))),
      new Promise(resolveTimeout => setTimeout(() => resolveTimeout(false), 10_000)),
    ]);
    if (!exited && child.exitCode === null) {
      child.kill("SIGKILL");
      await new Promise(resolveExit => child.once("exit", resolveExit));
    }
  }

  async cleanup() {
    await this.stop();
    for (const child of this.children) {
      expect(child.exitCode !== null || child.signalCode !== null, "every real Plug process must exit").toBe(true);
    }
    await rm(this.root, { recursive: true, force: true });
  }

  async cli(...args) {
    return await execFileAsync(PLUG_BIN, [...args, "--config", this.configPath], {
      cwd: REPO_ROOT,
      env: this.environment,
      timeout: 15_000,
      maxBuffer: 1024 * 1024,
    });
  }

  async request(path, { method = "GET", headers = {}, body } = {}) {
    return await loopbackRequest(this.port, path, { method, headers, body });
  }

  async expirePendingConsents() {
    const path = await findIssuerState(this.root);
    const state = JSON.parse(await readFile(path, "utf8"));
    for (const consent of Object.values(state.pending_consents)) consent.expires_at = 0;
    await writeFile(path, `${JSON.stringify(state)}\n`);
  }

  async installPublicProxy(context, onCallback = async () => {}) {
    await context.route(`${CLIENT_ORIGIN}/callback**`, async route => {
      await onCallback(route.request().url());
      await route.fulfill({
        status: 200,
        contentType: "text/html",
        body: "<!doctype html><title>Hosted callback received</title><h1>Connected</h1>",
      });
    });
    await context.route(`${PUBLIC_ORIGIN}/**`, async route => {
      const request = route.request();
      const sourceHeaders = request.headers();
      const headers = { ...sourceHeaders, host: PUBLIC_HOST };
      delete headers["content-length"];
      delete headers.connection;
      const response = await loopbackRequest(this.port, `${new URL(request.url()).pathname}${new URL(request.url()).search}`, {
        method: request.method(),
        headers,
        body: request.method() === "GET" || request.method() === "HEAD" ? undefined : request.postDataBuffer(),
      });
      const responseHeaders = Object.fromEntries(response.headers.entries());
      delete responseHeaders["content-encoding"];
      delete responseHeaders["content-length"];
      delete responseHeaders["transfer-encoding"];
      await route.fulfill({
        status: response.status,
        headers: responseHeaders,
        body: Buffer.from(await response.arrayBuffer()),
      });
    });
  }
}

async function registerClient(plug, name = "Hosted browser client") {
  const response = await plug.request("/oauth/register", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      client_name: name,
      redirect_uris: [CALLBACK_URL],
      token_endpoint_auth_method: "none",
      grant_types: ["authorization_code", "refresh_token"],
      response_types: ["code"],
    }),
  });
  expect(response.status).toBe(201);
  return await response.json();
}

function authorizationUrl(clientId, codeChallenge, state) {
  const query = new URLSearchParams({
    response_type: "code",
    client_id: clientId,
    redirect_uri: CALLBACK_URL,
    state,
    code_challenge: codeChallenge,
    code_challenge_method: "S256",
    scope: "tools:read offline_access",
    resource: RESOURCE,
  });
  return `${PUBLIC_ORIGIN}/oauth/authorize?${query}`;
}

async function exchangeToken(plug, fields) {
  return await plug.request("/oauth/token", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: formBody(fields),
  });
}

async function mcpRequest(plug, accessToken, message, sessionId) {
  const headers = {
    Authorization: `Bearer ${accessToken}`,
    "Content-Type": "application/json",
  };
  if (sessionId) {
    headers["Mcp-Session-Id"] = sessionId;
    headers["MCP-Protocol-Version"] = PROTOCOL_VERSION;
  }
  return await plug.request("/mcp", {
    method: "POST",
    headers,
    body: JSON.stringify(message),
  });
}

test.describe("downstream OAuth owner passkey", () => {
  let plug;

  test.beforeEach(async ({ context }) => {
    plug = await PlugProcess.create();
    await plug.start();
    await plug.installPublicProxy(context);
  });

  test.afterEach(async ({}, testInfo) => {
    if (testInfo.status !== testInfo.expectedStatus) {
      await testInfo.attach("plug-process.log", {
        body: Buffer.from(plug.output.join("")),
        contentType: "text/plain",
      });
    }
    await plug.cleanup();
  });

  test("Chromium completes enrollment, approval, PKCE, MCP, rotation, restart, and revocation", async ({ browserName, context, page }) => {
    test.skip(browserName !== "chromium", "Chromium CDP provides virtual WebAuthn in CI");

    const cdp = await context.newCDPSession(page);
    await cdp.send("WebAuthn.enable");
    await cdp.send("WebAuthn.addVirtualAuthenticator", {
      options: {
        protocol: "ctap2",
        transport: "internal",
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    });

    const enrollment = await plug.cli("auth", "owner", "enroll", "--no-browser", "--output", "json");
    const enrollmentPayload = JSON.parse(enrollment.stdout);
    const enrollmentUrl = new URL(enrollmentPayload.enrollment_url);
    const bootstrap = new URLSearchParams(enrollmentUrl.hash.slice(1)).get("bootstrap");
    expect(bootstrap).toMatch(/^[A-Za-z0-9_-]{43}$/);

    await page.goto(enrollmentPayload.enrollment_url);
    await expect(page).toHaveURL(`${PUBLIC_ORIGIN}/oauth/owner/enroll`);
    await page.getByRole("button", { name: "Create owner passkey" }).click();
    await expect(page.getByRole("status")).toHaveText(/Owner passkey created/);
    await expect(page.getByRole("button", { name: "Create owner passkey" })).toBeHidden();

    const registration = await registerClient(plug);
    const proof = pkce();
    const oauthState = randomBytes(16).toString("hex");
    let callback;
    await context.unroute(`${CLIENT_ORIGIN}/callback**`);
    await context.route(`${CLIENT_ORIGIN}/callback**`, async route => {
      callback = new URL(route.request().url());
      await route.fulfill({ status: 200, contentType: "text/html", body: "<h1>Connected</h1>" });
    });

    const navigation = await page.goto(authorizationUrl(registration.client_id, proof.challenge, oauthState));
    expect(navigation.status()).toBe(200);
    await expect(page.getByRole("heading", { name: "Allow Hosted browser client to use Plug?" })).toBeVisible();
    await page.getByText("Show full callback address").click();
    await expect(page.getByText(CALLBACK_URL)).toBeVisible();
    await expect(page.getByText("tools:read", { exact: true })).toBeVisible();

    await plug.stop();
    await plug.start();
    await page.getByRole("button", { name: "Allow with Touch ID or passkey" }).click();
    await page.waitForURL(`${CLIENT_ORIGIN}/callback**`);
    expect(callback.searchParams.get("state")).toBe(oauthState);
    const code = callback.searchParams.get("code");
    expect(code).toBeTruthy();

    const tokenResponse = await exchangeToken(plug, {
      grant_type: "authorization_code",
      client_id: registration.client_id,
      code,
      redirect_uri: CALLBACK_URL,
      code_verifier: proof.verifier,
      resource: RESOURCE,
    });
    expect(tokenResponse.status).toBe(200);
    expect(tokenResponse.headers.get("cache-control")).toBe("no-store");
    const tokens = await tokenResponse.json();
    expect(tokens.token_type).toBe("Bearer");
    expect(tokens.access_token).toBeTruthy();
    expect(tokens.refresh_token).toBeTruthy();

    const codeReplay = await exchangeToken(plug, {
      grant_type: "authorization_code",
      client_id: registration.client_id,
      code,
      redirect_uri: CALLBACK_URL,
      code_verifier: proof.verifier,
      resource: RESOURCE,
    });
    expect(codeReplay.status).toBe(400);
    expect((await codeReplay.json()).error).toBe("invalid_grant");

    const initialize = await mcpRequest(plug, tokens.access_token, {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "browser-e2e", version: "1.0.0" },
      },
    });
    expect(initialize.status).toBe(200);
    const sessionId = initialize.headers.get("mcp-session-id");
    expect(sessionId).toBeTruthy();
    expect((await initialize.json()).result.protocolVersion).toBe(PROTOCOL_VERSION);

    const initialized = await mcpRequest(plug, tokens.access_token, {
      jsonrpc: "2.0",
      method: "notifications/initialized",
    }, sessionId);
    expect(initialized.status).toBe(202);

    const tools = await mcpRequest(plug, tokens.access_token, {
      jsonrpc: "2.0",
      id: 2,
      method: "tools/list",
    }, sessionId);
    expect(tools.status).toBe(200);
    const toolsPayload = await tools.json();
    expect(toolsPayload.result.tools.some(tool => /echo/i.test(tool.name))).toBe(true);

    const rotation = await exchangeToken(plug, {
      grant_type: "refresh_token",
      client_id: registration.client_id,
      refresh_token: tokens.refresh_token,
      resource: RESOURCE,
    });
    expect(rotation.status).toBe(200);
    const rotated = await rotation.json();
    expect(rotated.access_token).not.toBe(tokens.access_token);
    expect(rotated.refresh_token).not.toBe(tokens.refresh_token);

    const refreshReplay = await exchangeToken(plug, {
      grant_type: "refresh_token",
      client_id: registration.client_id,
      refresh_token: tokens.refresh_token,
      resource: RESOURCE,
    });
    expect(refreshReplay.status).toBe(400);
    expect((await refreshReplay.json()).error).toBe("invalid_grant");

    await plug.cli("auth", "clients", "revoke", registration.client_id, "--yes");
    const revoked = await mcpRequest(plug, rotated.access_token, {
      jsonrpc: "2.0",
      id: 3,
      method: "tools/list",
    }, sessionId);
    expect(revoked.status).toBe(401);
    expect(revoked.headers.get("www-authenticate")).toContain("oauth-protected-resource");

    const processOutput = plug.output.join("");
    for (const secret of [bootstrap, proof.verifier, code, tokens.access_token, tokens.refresh_token, rotated.access_token, rotated.refresh_token]) {
      expect(processOutput, `process output must redact ${secret.slice(0, 8)}…`).not.toContain(secret);
    }
  });

  test("Chromium and WebKit keep denial, expiry, restart, and errors on public HTTPS", async ({ browser, context, page }) => {
    const browserRequests = [];
    page.on("request", request => browserRequests.push(request.url()));
    const callbacks = [];
    await context.unroute(`${CLIENT_ORIGIN}/callback**`);
    await context.route(`${CLIENT_ORIGIN}/callback**`, async route => {
      callbacks.push(new URL(route.request().url()));
      await route.fulfill({ status: 200, contentType: "text/html", body: "<h1>Denied</h1>" });
    });

    const registration = await registerClient(plug, "Shared browser client");
    const firstProof = pkce();
    const firstState = randomBytes(12).toString("hex");
    const response = await page.goto(authorizationUrl(registration.client_id, firstProof.challenge, firstState));
    expect(response.status()).toBe(200);
    expect(response.headers()["cache-control"]).toBe("no-store");
    expect(response.headers()["content-security-policy"]).toContain("connect-src 'self'");
    expect(response.headers()["referrer-policy"]).toBe("no-referrer");
    expect(response.headers()["x-frame-options"]).toBe("DENY");
    await expect(page.getByRole("heading", { name: "Allow Shared browser client to use Plug?" })).toBeVisible();
    await expect(page.getByText("Owner passkey required")).toBeVisible();
    await expect(page.getByRole("button", { name: /Allow with/ })).toHaveCount(0);
    await page.getByText("Show full callback address").click();
    await expect(page.getByText(CALLBACK_URL)).toBeVisible();

    const consentId = await page.locator("#consent").getAttribute("data-consent-id");
    const wrongOrigin = await plug.request("/oauth/consent/challenge", {
      method: "POST",
      headers: { "Content-Type": "application/json", Origin: "https://evil.test" },
      body: JSON.stringify({ consent_id: consentId }),
    });
    expect(wrongOrigin.status).toBe(403);
    expect(await wrongOrigin.text()).not.toContain(plug.root);

    await plug.stop();
    await plug.start();
    await page.getByRole("button", { name: "Deny" }).click();
    await page.waitForURL(`${CLIENT_ORIGIN}/callback**`);
    expect(callbacks).toHaveLength(1);
    expect(callbacks[0].searchParams.get("error")).toBe("access_denied");
    expect(callbacks[0].searchParams.get("state")).toBe(firstState);

    const secondProof = pkce();
    const secondState = randomBytes(12).toString("hex");
    await page.goto(authorizationUrl(registration.client_id, secondProof.challenge, secondState));
    await plug.stop();
    await plug.expirePendingConsents();
    await plug.start();
    await page.getByRole("button", { name: "Deny" }).click();
    await expect(page.getByRole("status")).toHaveText(/authorization request could not be completed/i);
    expect(callbacks).toHaveLength(1);

    const thirdProof = pkce();
    const thirdState = randomBytes(12).toString("hex");
    const noScript = await browser.newContext({ javaScriptEnabled: false });
    await plug.installPublicProxy(noScript);
    const noScriptPage = await noScript.newPage();
    await noScriptPage.goto(authorizationUrl(registration.client_id, thirdProof.challenge, thirdState));
    await expect(noScriptPage.locator("noscript")).toBeVisible();
    await noScriptPage.getByRole("button", { name: "Deny" }).click();
    await expect(noScriptPage).toHaveURL(/plug\.test\/oauth\/authorize/);
    await noScript.close();

    expect(browserRequests.some(url => /^http:\/\/(127\.0\.0\.1|localhost)/.test(url))).toBe(false);
    expect(browserRequests.filter(url => url.includes("/oauth/")).every(url => url.startsWith(PUBLIC_ORIGIN))).toBe(true);
  });
});
