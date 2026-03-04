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
