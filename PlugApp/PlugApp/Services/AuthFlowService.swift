@preconcurrency import Foundation

struct AuthFlowService: Sendable {
    func signIn(server: String) async throws {
        try await run(["auth", "login", "--server", server], failure: "Sign-in did not complete.")
    }

    /// Forgets the stored account for one server. The daemon reloads it, so the
    /// server goes back to asking for a sign-in without anything being
    /// restarted by hand.
    func signOut(server: String) async throws {
        try await run(["auth", "logout", "--server", server], failure: "Signing out did not complete.")
    }

    private func run(_ arguments: [String], failure: String) async throws {
        guard let executable = Bundle.main.url(forResource: "plug", withExtension: nil) else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSLocalizedDescriptionKey: "The bundled Plug service is missing."])
        }
        try await Task.detached {
            let process = Process()
            process.executableURL = executable
            process.arguments = arguments
            let output = Pipe()
            process.standardOutput = output
            process.standardError = output
            try process.run()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else {
                let data = output.fileHandleForReading.readDataToEndOfFile()
                let message = String(data: data, encoding: .utf8)?
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                let detail = message.flatMap { $0.isEmpty ? nil : $0 } ?? failure
                throw NSError(
                    domain: "Plug.Auth",
                    code: Int(process.terminationStatus),
                    userInfo: [NSLocalizedDescriptionKey: detail]
                )
            }
        }.value
    }
}
