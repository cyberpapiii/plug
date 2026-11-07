import Foundation
import PlugIPC

/// One call, in the words every surface uses for it.
///
/// The Activity list, the menu bar panel, and a server's recent calls all show
/// the same call. They read it from here, so the name, the caller, the time it
/// took, and the result never differ between them.
struct CallFacts: Equatable, Sendable {
    let event: ActivityEvent
    /// What the owner called this client, when they named it.
    let ownerName: String?
    /// The name on the client's sign-in, when it reached Plug over the
    /// network. The owner approved it, so it outranks what the client says
    /// about itself.
    let grantName: String?
    /// Where the owner says this client runs.
    let place: String?
    /// What joins a server's prefix to a tool's own name. Empty leaves the
    /// name whole, for a reader that only asks how a call went.
    let delimiter: String

    init(
        _ event: ActivityEvent,
        delimiter: String = "",
        ownerName: String? = nil,
        grantName: String? = nil,
        place: String? = nil
    ) {
        self.event = event
        self.delimiter = delimiter
        self.ownerName = ownerName
        self.grantName = grantName
        self.place = place
    }

    /// The caller, with where it runs when the owner has said.
    var callerAndPlace: String {
        guard let place, !place.isEmpty else { return caller }
        return "\(caller) (\(place))"
    }

    var succeeded: Bool { event.outcome == .success }
    /// The client stopped the call. Nothing went wrong, so it is not a problem.
    var cancelled: Bool { event.outcome == .cancelled }
    var failed: Bool { !succeeded && !cancelled }

    /// "Worked", "Failed", or "Canceled".
    var result: String {
        switch event.outcome {
        case .success: "Worked"
        case .cancelled: "Canceled"
        case .error: "Failed"
        }
    }

    /// The tool as the tool list names it. A tool a group rule files under
    /// another name, such as `Gmail__send` on `workspace`, keeps that name.
    var tool: String {
        guard let tool = event.tool, !tool.isEmpty else { return event.method }
        guard let server = event.server else { return tool }
        return ToolFacts.shortName(of: tool, server: server, delimiter: delimiter)
    }

    /// The server that answered, as the daemon routed the call.
    var server: String? { event.server }

    /// A search finds a call by the names its row shows, and by the ones
    /// the call arrived with.
    func matches(_ query: String) -> Bool {
        [caller, place, tool, server, event.tool, event.method, event.clientLabel]
            .contains { $0?.localizedCaseInsensitiveContains(query) ?? false }
    }

    /// The client whose icon the caller shows.
    var callerTarget: String {
        ClientIdentity.resolve(
            ownerName: ownerName, daemonTarget: event.target,
            reported: event.clientType, grantName: grantName
        )
    }

    /// The product name when Plug recognises the client; the client's own
    /// label otherwise. A raw client type is the last resort.
    var caller: String {
        if let ownerName, !ownerName.isEmpty { return ownerName }
        if let name = ClientIdentity.displayName(forTarget: callerTarget) { return name }
        if let grantName, !grantName.isEmpty { return grantName }
        if let label = event.clientLabel, !label.isEmpty { return label }
        guard let type = event.clientType, !type.isEmpty, type.lowercased() != "unknown" else {
            return "Unknown client"
        }
        return type
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .capitalized
    }

    var date: Date { Date(timeIntervalSince1970: Double(event.occurredAtMs) / 1_000) }

    /// Milliseconds under a second, seconds above, the same everywhere.
    var duration: String {
        event.latencyMs >= 1_000
            ? String(format: "%.1f s", Double(event.latencyMs) / 1_000)
            : "\(event.latencyMs) ms"
    }

    var spokenDuration: String {
        event.latencyMs >= 1_000
            ? String(format: "%.1f seconds", Double(event.latencyMs) / 1_000)
            : "\(event.latencyMs) milliseconds"
    }

    /// Why it failed, when the service said. Nil for a call that worked.
    var reason: String? {
        guard !succeeded else { return nil }
        if let reason = event.reason, !reason.isEmpty { return reason }
        return nil
    }

    /// What to do about a failure. Nil for a call that worked.
    var advice: String? {
        guard !succeeded else { return nil }
        if cancelled { return "The client stopped this call before it finished." }
        guard let reason else { return "Try the call again to see why it fails." }
        return Explain.advice(forReason: reason)
    }
}
