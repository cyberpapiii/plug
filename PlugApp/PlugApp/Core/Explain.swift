import Foundation
import PlugIPC

/// An error of the app's own. Its message is the app's, so its advice is
/// chosen with it, not guessed from its words: a message that says
/// "permission" or "expired" about Plug itself is not about a server.
/// Text from outside the app that one carries is still read for its words.
protocol AdvisedError: Error {
    var advice: String { get }
}

extension AdvisedError {
    var advice: String { Explain.fallback }
}

/// What to do about a failure, in plain words.
///
/// Errors reach the app as text from a server, the system, or Plug's own
/// service. That text says what went wrong in its author's words and almost
/// never says what to do. This reads the reason and names the next step, so no
/// raw error is ever the only thing on screen.
enum Explain {
    /// The fallback when Plug cannot tell what went wrong.
    static let fallback = "Try again. If it keeps happening, run a checkup in Settings."

    static let starting = "Plug is starting. Wait a moment, then try again."

    static func advice(for error: any Error) -> String {
        if let advised = error as? any AdvisedError { return advised.advice }
        return advice(forReason: error.localizedDescription)
    }

    /// The advice for text from outside the app inside one of its errors,
    /// the fallback when there is none.
    static func advice(forOutside text: String) -> String {
        text.isEmpty ? fallback : advice(forReason: text)
    }

    static func advice(forReason reason: String) -> String {
        let text = reason.lowercased()
        func has(_ words: String...) -> Bool { words.contains { text.contains($0) } }
        // A status code counts only as a whole number, so "1500 ms" is not a 500.
        func code(_ codes: Int...) -> Bool {
            codes.contains { text.range(of: "\\b\($0)\\b", options: .regularExpression) != nil }
        }

        if has("reconnecting", "plug is not running", "plug is starting", "background service") {
            return starting
        }
        if code(401) || has("unauthorized", "invalid_token", "invalid token", "sign-in", "sign in", "authorization", "expired") {
            return "Sign in to the server again, then try again."
        }
        if code(403) || has("forbidden", "not allowed", "permission") {
            return "The account does not have access to this. Check it with whoever runs the server."
        }
        if has("timed out", "timeout", "deadline") {
            return "The server took too long to answer. Try again, or restart the server."
        }
        if has("already exists", "already a server", "name is taken") {
            return "Choose another name."
        }
        if has("no such file", "command not found", "not found in path", "failed to spawn", "os error 2") {
            return "Check that the command is installed on this Mac and spelled correctly."
        }
        if has("connection refused", "could not connect", "dns", "unreachable", "connection reset", "failed to connect") {
            return "Check that the address is right and that the server is running."
        }
        if code(404) || has("not found", "unknown tool", "no such tool") {
            return "The server no longer offers this. Restart the server to refresh its tools."
        }
        if code(429) || has("rate limit", "too many requests") {
            return "The server is asking for fewer requests. Wait a minute, then try again."
        }
        if code(500, 502, 503, 504) || has("internal error", "bad gateway", "unavailable") {
            return "The server had a problem of its own. Try again in a moment."
        }
        if has("keychain") {
            return "Unlock the Keychain, then try again."
        }
        if has("invalid", "missing", "required", "parse", "expected") {
            return "Check what was entered, then try again."
        }
        return fallback
    }
}

/// The daemon's own refusal is text from outside the app; the rest is the
/// connection to it.
extension PlugIPCError: AdvisedError {
    var advice: String {
        switch self {
        case let .daemon(_, message): Explain.advice(forOutside: message)
        case .disconnected: Explain.starting
        default: Explain.fallback
        }
    }
}
