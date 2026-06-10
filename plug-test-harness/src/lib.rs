#![forbid(unsafe_code)]

//! Test utilities for plug integration tests.

/// Returns the path to the mock-mcp-server binary.
///
/// Locates the binary in the cargo target directory relative to the
/// current test executable.
pub fn mock_server_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("failed to get current exe path");
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("mock-mcp-server");
    path
}

/// Build the `mock-mcp-server` binary once and return its path.
///
/// `mock-mcp-server` is a `[[bin]]` of this dev-dependency crate, so Cargo does
/// not build it automatically for a downstream crate's tests. This builds it on
/// first use (memoized behind a `OnceLock`) and returns the prebuilt path, so
/// tests can exec the binary directly instead of paying `cargo run` overhead on
/// every mock spawn — which, under parallel test threads, would otherwise have
/// many concurrent `cargo run` processes contend on Cargo's `target/` lock.
///
/// Safe to call from multiple parallel tests within a process (the `OnceLock`
/// serializes the build); across test-binary processes the build is idempotent
/// and Cargo's own lock serializes concurrent builds.
pub fn mock_server_bin() -> std::path::PathBuf {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("plug-test-harness should live under the workspace root");
        let status = std::process::Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "build",
                "--quiet",
                "-p",
                "plug-test-harness",
                "--bin",
                "mock-mcp-server",
            ])
            .status()
            .expect("build mock-mcp-server");
        assert!(status.success(), "mock-mcp-server build failed");
        mock_server_path()
    })
    .clone()
}
