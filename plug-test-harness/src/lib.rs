#![forbid(unsafe_code)]

//! Test utilities for plug integration tests.

/// Build the `mock-mcp-server` binary once and return its path.
///
/// `mock-mcp-server` is a `[[bin]]` of this dev-dependency crate, so Cargo does
/// not build it automatically for a downstream crate's tests. This builds it on
/// first use, in the profile of the running test binary, and returns its path
/// beside that binary, so tests exec it directly instead of each running
/// `cargo run` and contending on Cargo's `target/` lock.
///
/// Parallel tests within a process share one build behind a `OnceLock`;
/// across test-binary processes Cargo's own lock serializes the builds.
pub fn mock_server_bin() -> std::path::PathBuf {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        // The test binary lives in `<target>/<profile>/deps/`.
        let mut profile_dir = std::env::current_exe().expect("failed to get current exe path");
        profile_dir.pop();
        profile_dir.pop();
        let release = profile_dir
            .file_name()
            .is_some_and(|name| name == "release");

        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("plug-test-harness should live under the workspace root");
        let mut build = std::process::Command::new("cargo");
        build.current_dir(workspace_root).args([
            "build",
            "--quiet",
            "-p",
            "plug-test-harness",
            "--bin",
            "mock-mcp-server",
        ]);
        if release {
            build.arg("--release");
        }
        let status = build.status().expect("build mock-mcp-server");
        assert!(status.success(), "mock-mcp-server build failed");
        profile_dir.join("mock-mcp-server")
    })
    .clone()
}
