"use strict";

function decodeBase64url(value) {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((value.length + 3) % 4);
  return Uint8Array.from(atob(base64), character => character.charCodeAt(0));
}

function encodeBase64url(value) {
  const bytes = new Uint8Array(value);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function postJson(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "same-origin",
    body: JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(payload.error_description || "Plug could not complete this request.");
    error.code = payload.error;
    throw error;
  }
  return payload;
}

function requestOptions(publicKey) {
  return {
    ...publicKey,
    challenge: decodeBase64url(publicKey.challenge),
    allowCredentials: (publicKey.allowCredentials || []).map(credential => ({
      ...credential,
      id: decodeBase64url(credential.id),
    })),
    userVerification: "required",
  };
}

function authenticationResponse(credential) {
  return {
    id: encodeBase64url(credential.rawId),
    authenticatorData: encodeBase64url(credential.response.authenticatorData),
    signature: encodeBase64url(credential.response.signature),
    clientDataJSON: encodeBase64url(credential.response.clientDataJSON),
    userHandle: credential.response.userHandle
      ? encodeBase64url(credential.response.userHandle)
      : null,
  };
}

const page = document.getElementById("consent");
const allow = document.getElementById("allow");
const deny = document.getElementById("deny");
const status = document.getElementById("status");

function showError(error) {
  if (error && error.name === "NotAllowedError") {
    status.textContent = "Passkey verification was canceled or timed out. You can try again while this request is open.";
  } else {
    status.textContent = error && error.message
      ? error.message
      : "Plug could not complete this request. Try again.";
  }
}

async function approve() {
  if (!window.PublicKeyCredential || !navigator.credentials) {
    status.textContent = "This browser or device cannot use passkeys. Open this page in a passkey-capable browser on your device.";
    return;
  }
  allow.disabled = true;
  deny.disabled = true;
  status.textContent = "Waiting for Touch ID or your passkey…";
  try {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const challenge = await postJson("/oauth/consent/challenge", {
        consent_id: page.dataset.consentId,
      });
      const credential = await navigator.credentials.get({
        publicKey: requestOptions(challenge.public_key),
      });
      try {
        const decision = await postJson("/oauth/consent/decision", {
          decision: "approve",
          ceremony_id: challenge.ceremony_id,
          credential: authenticationResponse(credential),
        });
        window.location.assign(decision.redirect_uri);
        return;
      } catch (error) {
        if (attempt === 0 && error.code === "owner_challenge_expired") continue;
        throw error;
      }
    }
  } catch (error) {
    showError(error);
    allow.disabled = false;
    deny.disabled = false;
  }
}

async function denyAccess() {
  if (allow) allow.disabled = true;
  deny.disabled = true;
  status.textContent = "Denying this request…";
  try {
    const decision = await postJson("/oauth/consent/decision", {
      decision: "deny",
      consent_id: page.dataset.consentId,
      csrf_token: page.dataset.csrfToken,
    });
    window.location.assign(decision.redirect_uri);
  } catch (error) {
    showError(error);
    if (allow) allow.disabled = false;
    deny.disabled = false;
  }
}

if (allow) allow.addEventListener("click", approve);
deny.addEventListener("click", denyAccess);
