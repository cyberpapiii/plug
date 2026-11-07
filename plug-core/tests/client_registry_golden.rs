//! The client registry as the app sees it, pinned in `testdata/clients.json`.
//!
//! The daemon names a client by its slug; the app shows that slug's name and
//! icon. `PlugApp/PlugAppTests/KnownClientsTests.swift` holds the app's table
//! to the same file. Rerun with `PLUG_REGENERATE_GOLDENS=1` after a registry
//! change, and change the app to match.

use std::path::PathBuf;

use plug_core::clients::ClientTarget;

#[derive(serde::Serialize)]
struct Entry {
    slug: &'static str,
    name: &'static str,
    aliases: &'static [&'static str],
    listed: bool,
}

#[test]
fn the_client_golden_is_the_registry() {
    let entries: Vec<Entry> = ClientTarget::all()
        .map(|target| {
            let info = target.info();
            Entry {
                slug: info.slug,
                name: info.name,
                aliases: info.aliases,
                listed: info.listed,
            }
        })
        .collect();
    let contents = format!(
        "{}\n",
        serde_json::to_string_pretty(&entries).expect("registry serializes")
    );
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata/clients.json");
    if std::env::var_os("PLUG_REGENERATE_GOLDENS").is_some() {
        std::fs::write(&path, contents).expect("write testdata/clients.json");
        return;
    }
    let golden = std::fs::read_to_string(&path).expect("read testdata/clients.json");
    assert!(
        golden == contents,
        "testdata/clients.json is not the client registry. If the registry is meant to \
         change, rerun with PLUG_REGENERATE_GOLDENS=1 and change the app to match."
    );
}
