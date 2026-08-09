import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";

const consentSource = fs.readFileSync(
  new URL("../plug-core/src/http/oauth_ui/consent.js", import.meta.url),
  "utf8",
);
const enrollmentSource = fs.readFileSync(
  new URL("../plug-core/src/http/oauth_ui/enroll.js", import.meta.url),
  "utf8",
);

function element(extra = {}) {
  return {
    disabled: false,
    hidden: false,
    textContent: "",
    listeners: {},
    addEventListener(name, listener) {
      this.listeners[name] = listener;
    },
    ...extra,
  };
}

function browserGlobals(overrides = {}) {
  return {
    Uint8Array,
    URLSearchParams,
    Error,
    atob: value => Buffer.from(value, "base64").toString("binary"),
    btoa: value => Buffer.from(value, "binary").toString("base64"),
    ...overrides,
  };
}

async function unsupportedConsentExplainsRecovery() {
  const page = element({ dataset: { consentId: "consent", csrfToken: "csrf" } });
  const allow = element();
  const deny = element();
  const status = element();
  const elements = { consent: page, allow, deny, status };
  const context = browserGlobals({
    document: { getElementById: id => elements[id] },
    navigator: {},
    window: { location: { assign() {} } },
    fetch: () => {
      throw new Error("unsupported browser must not call Plug");
    },
  });

  vm.runInNewContext(consentSource, context, { filename: "consent.js" });
  await allow.listeners.click();

  assert.equal(
    status.textContent,
    "Passkeys are not available in this browser or device. Open this page in a browser that supports passkeys. No access was granted.",
  );
}

async function expiredAssertionRetriesOnce() {
  const page = element({ dataset: { consentId: "consent", csrfToken: "csrf" } });
  const allow = element();
  const deny = element();
  const status = element();
  const elements = { consent: page, allow, deny, status };
  const calls = [];
  const assigned = [];
  let decisionAttempts = 0;
  const credential = {
    rawId: new Uint8Array([1]).buffer,
    response: {
      authenticatorData: new Uint8Array([2]).buffer,
      signature: new Uint8Array([3]).buffer,
      clientDataJSON: new Uint8Array([4]).buffer,
      userHandle: null,
    },
  };
  const fetch = async path => {
    calls.push(path);
    if (path === "/oauth/consent/challenge") {
      return {
        ok: true,
        async json() {
          return {
            ceremony_id: `ceremony-${calls.length}`,
            public_key: { challenge: "AQ", allowCredentials: [] },
          };
        },
      };
    }
    decisionAttempts += 1;
    if (decisionAttempts === 1) {
      return {
        ok: false,
        async json() {
          return {
            error: "owner_challenge_expired",
            error_description: "Approval expired. Select Allow again.",
          };
        },
      };
    }
    return {
      ok: true,
      async json() {
        return { redirect_uri: "https://client.example/callback" };
      },
    };
  };
  const context = browserGlobals({
    document: { getElementById: id => elements[id] },
    navigator: { credentials: { get: async () => credential } },
    window: {
      PublicKeyCredential: function PublicKeyCredential() {},
      location: { assign: value => assigned.push(value) },
    },
    fetch,
  });

  vm.runInNewContext(consentSource, context, { filename: "consent.js" });
  await allow.listeners.click();

  assert.deepEqual(calls, [
    "/oauth/consent/challenge",
    "/oauth/consent/decision",
    "/oauth/consent/challenge",
    "/oauth/consent/decision",
  ]);
  assert.deepEqual(assigned, ["https://client.example/callback"]);
}

async function unsupportedEnrollmentExplainsRecovery() {
  const enroll = element();
  const status = element();
  const elements = { enroll, status };
  const context = browserGlobals({
    document: { getElementById: id => elements[id] },
    navigator: {},
    history: { replaceState() {} },
    window: {
      location: {
        hash: "#bootstrap=bootstrap-secret",
        pathname: "/oauth/owner/enroll",
        search: "",
      },
    },
    fetch: () => {
      throw new Error("unsupported browser must not call Plug");
    },
  });

  vm.runInNewContext(enrollmentSource, context, { filename: "enroll.js" });
  await enroll.listeners.click();

  assert.equal(
    status.textContent,
    "Passkeys are not available in this browser or device. Open this enrollment link in a browser that supports passkeys. No owner passkey was created.",
  );
}

await unsupportedConsentExplainsRecovery();
await expiredAssertionRetriesOnce();
await unsupportedEnrollmentExplainsRecovery();
