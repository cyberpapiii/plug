import Foundation
@testable import Plug

/// A report as the CLI prints it, from `testdata/cli`. `plug/src/cli_goldens.rs`
/// writes these files from the types the commands print.
func cliGolden(_ name: String, filePath: String = #filePath) throws -> Data {
    var root = URL(fileURLWithPath: filePath)
    while !FileManager.default.fileExists(atPath: root.appending(path: "testdata/cli").path) {
        let parent = root.deletingLastPathComponent()
        guard parent != root else { throw CocoaError(.fileNoSuchFile) }
        root = parent
    }
    return try Data(contentsOf: root.appending(path: "testdata/cli/\(name)"))
}

extension ProcessResult {
    /// The command exited with `status` and printed the golden `name`.
    static func golden(_ name: String, status: Int32 = 0) throws -> ProcessResult {
        ProcessResult(status: status, stdout: try cliGolden(name), stderr: Data())
    }
}
