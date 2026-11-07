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

const REQUEST_FAILED = "Plug could not complete this request.";
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
    status.textContent = "Passkeys are not available in this browser or device. Open this page in a browser that supports passkeys. No access was granted.";
    return;
  }
  allow.disabled = true;
  deny.disabled = true;
  status.textContent = "Waiting for Touch ID or your passkey…";
  try {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const challenge = await postJson(page.dataset.challengeEndpoint, {
        consent_id: page.dataset.consentId,
      }, REQUEST_FAILED);
      if (challenge.redirect_uri) {
        window.location.assign(challenge.redirect_uri);
        return;
      }
      const credential = await navigator.credentials.get({
        publicKey: requestOptions(challenge.public_key),
      });
      try {
        const decision = await postJson(page.dataset.decisionEndpoint, {
          decision: "approve",
          ceremony_id: challenge.ceremony_id,
          credential: authenticationResponse(credential),
        }, REQUEST_FAILED);
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
    const decision = await postJson(page.dataset.decisionEndpoint, {
      decision: "deny",
      consent_id: page.dataset.consentId,
      csrf_token: page.dataset.csrfToken,
    }, REQUEST_FAILED);
    window.location.assign(decision.redirect_uri);
  } catch (error) {
    showError(error);
    if (allow) allow.disabled = false;
    deny.disabled = false;
  }
}

if (allow) allow.addEventListener("click", approve);
deny.addEventListener("click", denyAccess);
