@preconcurrency import Foundation

struct AuthFlowService: Sendable {
    func signIn(server: String) async throws {
        guard let executable = Bundle.main.url(forResource: "plug", withExtension: nil) else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSLocalizedDescriptionKey: "The bundled Plug service is missing."])
        }
        try await Task.detached {
            let process = Process()
            process.executableURL = executable
            process.arguments = ["auth", "login", "--server", server]
            let output = Pipe()
            process.standardOutput = output
            process.standardError = output
            try process.run()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else {
                let data = output.fileHandleForReading.readDataToEndOfFile()
                let message = String(data: data, encoding: .utf8)?
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                let detail = message.flatMap { $0.isEmpty ? nil : $0 } ?? "Sign-in did not complete."
                throw NSError(
                    domain: "Plug.Auth",
                    code: Int(process.terminationStatus),
                    userInfo: [NSLocalizedDescriptionKey: detail]
                )
            }
        }.value
    }
}
