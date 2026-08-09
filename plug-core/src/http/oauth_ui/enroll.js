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
    throw new Error(payload.error_description || "Plug could not complete owner passkey setup.");
  }
  return payload;
}

function creationOptions(publicKey) {
  return {
    ...publicKey,
    challenge: decodeBase64url(publicKey.challenge),
    user: { ...publicKey.user, id: decodeBase64url(publicKey.user.id) },
    excludeCredentials: (publicKey.excludeCredentials || []).map(credential => ({
      ...credential,
      id: decodeBase64url(credential.id),
    })),
    authenticatorSelection: {
      ...publicKey.authenticatorSelection,
      userVerification: "required",
    },
  };
}

function registrationResponse(credential) {
  const response = {
    id: encodeBase64url(credential.rawId),
    attestationObject: encodeBase64url(credential.response.attestationObject),
    clientDataJSON: encodeBase64url(credential.response.clientDataJSON),
  };
  if (typeof credential.response.getTransports === "function") {
    response.transports = credential.response.getTransports();
  }
  return response;
}

const enroll = document.getElementById("enroll");
const status = document.getElementById("status");
const fragment = new URLSearchParams(window.location.hash.slice(1));
const bootstrap = fragment.get("bootstrap");
history.replaceState(null, "", window.location.pathname + window.location.search);

async function enrollOwner() {
  if (!bootstrap) {
    status.textContent = "This enrollment link is missing or has already been used. Run plug auth owner enroll on the Mac running Plug.";
    return;
  }
  if (!window.PublicKeyCredential || !navigator.credentials) {
    status.textContent = "This browser or device cannot create a passkey. Open the enrollment link in a passkey-capable browser.";
    return;
  }
  enroll.disabled = true;
  status.textContent = "Waiting for Touch ID or your passkey provider…";
  try {
    const challenge = await postJson("/oauth/owner/enroll/challenge", { bootstrap });
    const credential = await navigator.credentials.create({
      publicKey: creationOptions(challenge.public_key),
    });
    await postJson("/oauth/owner/enroll/complete", {
      ceremony_id: challenge.ceremony_id,
      credential: registrationResponse(credential),
    });
    status.textContent = "Owner passkey created. You can close this window and connect to Plug.";
    enroll.hidden = true;
  } catch (error) {
    status.textContent = error && error.name === "NotAllowedError"
      ? "Passkey setup was canceled or timed out. Run plug auth owner enroll to create a fresh link."
      : (error.message || "Plug could not create the owner passkey. Run plug auth owner enroll and try again.");
  }
}

enroll.addEventListener("click", enrollOwner);
if (!bootstrap) enrollOwner();
