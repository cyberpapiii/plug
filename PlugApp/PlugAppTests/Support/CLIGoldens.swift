import Foundation
@testable import Plug

/// A report as the CLI prints it, from `testdata/cli`. `plug/src/cli_goldens.rs`
/// writes these files from the types the commands print.
func cliGolden(_ name: String, filePath: String = #filePath) throws -> Data {
    try Data(contentsOf: testdata(filePath).appending(path: "cli/\(name)"))
}

/// The daemon's replies from `testdata/ipc/responses.json`, by the name
/// `plug-core/tests/ipc_goldens.rs` files each under.
func ipcResponseGoldens(filePath: String = #filePath) throws -> [String: [String: Any]] {
    let data = try Data(contentsOf: testdata(filePath).appending(path: "ipc/responses.json"))
    guard let replies = try JSONSerialization.jsonObject(with: data) as? [String: [String: Any]] else {
        throw CocoaError(.fileReadCorruptFile)
    }
    return replies
}

private func testdata(_ filePath: String) throws -> URL {
    var root = URL(fileURLWithPath: filePath)
    while !FileManager.default.fileExists(atPath: root.appending(path: "testdata/cli").path) {
        let parent = root.deletingLastPathComponent()
        guard parent != root else { throw CocoaError(.fileNoSuchFile) }
        root = parent
    }
    return root.appending(path: "testdata")
}

extension ProcessResult {
    /// The command exited with `status` and printed the golden `name`.
    static func golden(_ name: String, status: Int32 = 0) throws -> ProcessResult {
        ProcessResult(status: status, stdout: try cliGolden(name), stderr: Data())
    }
}
