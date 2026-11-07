//! Where an older Plug, installed before the app owned the daemon, could be.

use std::path::{Path, PathBuf};

/// The leftover-path classify table, shared with `LegacyPlugProgram` in
/// Plug.app. `testdata/legacy_plug_programs.json` pins the table itself as well
/// as the cases, so a shape added to one language without the other fails a
/// test on both sides.
pub(crate) const LEGACY_EXACT_PROGRAMS: [&str; 4] = [
    "/opt/homebrew/bin/plug",
    "/usr/local/bin/plug",
    "/opt/homebrew/opt/plug/bin/plug",
    "/usr/local/opt/plug/bin/plug",
];
pub(crate) const LEGACY_HOME_RELATIVE_PROGRAMS: [&str; 2] =
    [".cargo/bin/plug", plug_core::paths::COMMAND_LINK];
pub(crate) const LEGACY_CELLAR_ROOTS: [&str; 2] =
    ["/opt/homebrew/Cellar/plug", "/usr/local/Cellar/plug"];

/// Every place a recognized older Plug could be installed, each Cellar
/// version included, whether or not anything is there.
pub(crate) fn legacy_program_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = LEGACY_EXACT_PROGRAMS.iter().map(PathBuf::from).collect();
    if let Some(home) = dirs::home_dir() {
        candidates.extend(
            LEGACY_HOME_RELATIVE_PROGRAMS
                .iter()
                .map(|suffix| home.join(suffix)),
        );
    }
    for root in LEGACY_CELLAR_ROOTS {
        if let Ok(versions) = std::fs::read_dir(root) {
            candidates.extend(
                versions
                    .flatten()
                    .map(|version| version.path().join("bin/plug")),
            );
        }
    }
    candidates
}

/// Keep in lockstep with `LegacyPlugProgram.isRecognized` in Plug.app.
pub(crate) fn is_recognized_legacy_program(program: &Path) -> bool {
    if program.file_name().and_then(|name| name.to_str()) != Some("plug") {
        return false;
    }
    if LEGACY_EXACT_PROGRAMS
        .iter()
        .any(|candidate| program == Path::new(candidate))
    {
        return true;
    }
    if dirs::home_dir().is_some_and(|home| {
        LEGACY_HOME_RELATIVE_PROGRAMS
            .iter()
            .any(|suffix| program == home.join(suffix))
    }) {
        return true;
    }
    // A Cellar binary is a shape, not a path: `<root>/<version>/bin/plug`.
    LEGACY_CELLAR_ROOTS.iter().any(|root| {
        program.strip_prefix(root).is_ok_and(|relative| {
            let parts = relative.components().collect::<Vec<_>>();
            parts.len() == 3 && parts[1].as_os_str() == "bin" && parts[2].as_os_str() == "plug"
        })
    })
}
