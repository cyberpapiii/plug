//! `plug codesign-setup` — give the binary a stable macOS code-signing identity.
//!
//! plug stores upstream OAuth credentials in the macOS login Keychain. A
//! locally-built / release binary is ad-hoc signed, and its signature changes on
//! every rebuild, so the Keychain "Always Allow" ACL never persists and macOS
//! re-prompts constantly. This command creates (once) a stable self-signed
//! code-signing identity and signs the running binary with it, so the approval
//! sticks across rebuilds.
//!
//! It is install-path-agnostic: it signs whatever `plug` binary is running, so it
//! works for `cargo install`, Homebrew, and release-download installs alike.
//!
//! Background:
//! `docs/solutions/integration-issues/local-codesigning-identity-stops-keychain-reprompts.md`

use crate::OutputFormat;

/// Stable identity name; the Keychain ACL binds to this cert, not the per-build hash.
#[cfg(target_os = "macos")]
const IDENTITY: &str = "Plug Local Signing";
/// Local-only passphrase for the on-disk PKCS#12 (stored chmod 600).
#[cfg(target_os = "macos")]
const P12_PASS: &str = "pluglocal";

pub(crate) fn cmd_codesign_setup(output: &OutputFormat) -> anyhow::Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = output;
        println!("Code-signing setup is only needed on macOS. Nothing to do on this platform.");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        run_macos(output)
    }
}

#[cfg(target_os = "macos")]
fn run_macos(output: &OutputFormat) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let json = matches!(output, OutputFormat::Json);

    // Sign the binary that is actually running (symlinks resolved), so this works
    // regardless of how plug was installed.
    let exe = std::env::current_exe().context("could not resolve the running executable")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

    let created = if identity_valid()? {
        if !json {
            println!("✓ '{IDENTITY}' code-signing identity already present.");
        }
        false
    } else {
        if !json {
            println!("==> Creating a stable self-signed code-signing identity: '{IDENTITY}'");
            eprintln!(
                "   (a macOS dialog will ask for your login password to trust the cert — this is expected)"
            );
        }
        create_identity()?;
        true
    };

    if !json {
        println!("==> Signing {}", exe.display());
    }
    let status = std::process::Command::new("codesign")
        .args(["--force", "-s", IDENTITY])
        .arg(&exe)
        .status()
        .context("failed to run codesign")?;
    anyhow::ensure!(status.success(), "codesign failed for {}", exe.display());

    let signed = matches!(adhoc_or_unsigned(&exe), Some(false));

    if json {
        let report = serde_json::json!({
            "platform": "macos",
            "identity": IDENTITY,
            "identity_created": created,
            "binary": exe.display().to_string(),
            "signed": signed,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if !signed {
            println!(
                "!  Signed, but verification did not confirm a stable authority. Inspect with: codesign -dvv {}",
                exe.display()
            );
        }
        println!();
        println!("Done. Next:");
        println!("  1. Restart plug in your login session:  plug stop && plug start");
        println!(
            "  2. Click \"Always Allow\" on the one final round of Keychain prompts (one per OAuth"
        );
        println!(
            "     upstream). They now bind to '{IDENTITY}' and won't recur on future rebuilds."
        );
    }

    Ok(())
}

/// True when `security` lists `IDENTITY` as a *valid* code-signing identity.
#[cfg(target_os = "macos")]
fn identity_valid() -> anyhow::Result<bool> {
    use anyhow::Context as _;
    let out = std::process::Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .context("failed to run `security find-identity`")?;
    Ok(String::from_utf8_lossy(&out.stdout).contains(IDENTITY))
}

/// Generate a self-signed code-signing cert, import it into the login keychain,
/// and trust it for the codeSign policy. Only called when no valid identity exists.
#[cfg(target_os = "macos")]
fn create_identity() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::process::Command;

    let home = std::env::var("HOME").context("HOME is not set")?;
    let home = std::path::PathBuf::from(home);
    let dir = home.join(".config/plug-signing");
    std::fs::create_dir_all(&dir).context("could not create ~/.config/plug-signing")?;
    set_mode(&dir, 0o700);

    let key = dir.join("key.pem");
    let cert = dir.join("cert.pem");
    let p12 = dir.join("plug-signing.p12");
    let keychain = home.join("Library/Keychains/login.keychain-db");

    // 1. Self-signed cert. BOTH basic Key Usage (digitalSignature) AND Extended
    //    Key Usage (codeSigning) are required by macOS's code-signing policy.
    let mut c = Command::new("openssl");
    c.arg("req")
        .arg("-x509")
        .arg("-newkey")
        .arg("rsa:2048")
        .arg("-nodes")
        .arg("-days")
        .arg("3650")
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .arg("-subj")
        .arg(format!("/CN={IDENTITY}"))
        .arg("-addext")
        .arg("keyUsage=critical,digitalSignature")
        .arg("-addext")
        .arg("extendedKeyUsage=critical,codeSigning")
        .arg("-addext")
        .arg("basicConstraints=critical,CA:false");
    run(&mut c, "openssl req (generate cert)")?;

    // 2. Bundle into PKCS#12 with LEGACY algorithms — OpenSSL 3 defaults fail
    //    macOS `security import` with "MAC verification failed".
    let mut c = Command::new("openssl");
    c.arg("pkcs12")
        .arg("-export")
        .arg("-inkey")
        .arg(&key)
        .arg("-in")
        .arg(&cert)
        .arg("-out")
        .arg(&p12)
        .arg("-name")
        .arg(IDENTITY)
        .arg("-passout")
        .arg(format!("pass:{P12_PASS}"))
        .arg("-legacy");
    run(&mut c, "openssl pkcs12 (bundle p12)")?;

    set_mode(&key, 0o600);
    set_mode(&p12, 0o600);

    // Clear any stale same-named (invalid/untrusted) identity so re-runs after a
    // failed attempt don't stack duplicates. Best-effort.
    let _ = Command::new("security")
        .args(["delete-identity", "-c", IDENTITY])
        .output();

    // 3. Import cert+key, granting codesign access to the key.
    let mut c = Command::new("security");
    c.arg("import")
        .arg(&p12)
        .arg("-k")
        .arg(&keychain)
        .arg("-P")
        .arg(P12_PASS)
        .arg("-T")
        .arg("/usr/bin/codesign");
    run(&mut c, "security import")?;

    // 4. Trust it FOR CODE SIGNING ONLY. Pops a login-password dialog.
    let mut c = Command::new("security");
    c.arg("add-trusted-cert")
        .arg("-r")
        .arg("trustRoot")
        .arg("-p")
        .arg("codeSign")
        .arg(&cert);
    run(&mut c, "security add-trusted-cert")?;

    anyhow::ensure!(
        identity_valid()?,
        "'{IDENTITY}' did not become a valid code-signing identity after trust (check: security find-identity -v -p codesigning)"
    );
    Ok(())
}

/// Run a command, mapping a non-zero exit to an error that surfaces stderr.
#[cfg(target_os = "macos")]
fn run(cmd: &mut std::process::Command, ctx: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn: {ctx}"))?;
    anyhow::ensure!(
        out.status.success(),
        "{ctx} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// `Some(true)` if ad-hoc/unsigned, `Some(false)` if it has a real authority,
/// `None` if codesign output was unrecognized.
#[cfg(target_os = "macos")]
fn adhoc_or_unsigned(exe: &std::path::Path) -> Option<bool> {
    // codesign writes the display info to stderr.
    let out = std::process::Command::new("codesign")
        .arg("-dvv")
        .arg(exe)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    if text.contains("adhoc") || text.contains("not signed") {
        Some(true)
    } else if text.contains("Authority=") {
        Some(false)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}
