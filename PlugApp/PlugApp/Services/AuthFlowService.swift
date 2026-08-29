import Foundation

struct AuthFlowService: Sendable {
    /// `plug auth login` blocks while the browser round-trips, and the CLI
    /// bounds that wait itself: the localhost callback expires after 120
    /// seconds. This cap sits above that, so it can only fire once the CLI has
    /// stopped honouring its own bound, and a wedged sign-in is torn down
    /// instead of holding a child process open for the life of the app.
    private static let timeout = Duration.seconds(180)

    private let runner: any ProcessRunning
    private let executable: URL?

    init(runner: any ProcessRunning = ProcessRunner(), executable: URL? = BundledPlug.executable) {
        self.runner = runner
        self.executable = executable
    }

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
        guard let executable else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSLocalizedDescriptionKey: "The bundled Plug service is missing."])
        }
        let result = try await runner.run(
            executable: executable,
            arguments: arguments,
            timeout: Self.timeout
        )
        guard result.status == 0 else {
            throw NSError(
                domain: "Plug.Auth",
                code: Int(result.status),
                userInfo: [NSLocalizedDescriptionKey: detail(from: result, failure: failure)]
            )
        }
    }

    /// The CLI reports failures on standard error, but a few paths explain
    /// themselves on standard output and still exit non-zero, so read both
    /// before settling for the generic message.
    private func detail(from result: ProcessResult, failure: String) -> String {
        let stderr = result.stderrText
        if !stderr.isEmpty { return stderr }
        let stdout = String(decoding: result.stdout, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return stdout.isEmpty ? failure : stdout
    }
}
