import Foundation
import PlugIPC

/// One call, in the words every surface uses for it.
///
/// The Activity list, the menu bar panel, and a server's recent calls all show
/// the same call. They read it from here, so the name, the caller, the time it
/// took, and the result never differ between them.
struct CallFacts: Equatable {
    let event: ActivityEvent
    /// What the owner called this client, when they named it.
    let ownerName: String?

    init(_ event: ActivityEvent, ownerName: String? = nil) {
        self.event = event
        self.ownerName = ownerName
    }

    var succeeded: Bool { event.outcome == "success" }
    /// The client stopped the call. Nothing went wrong, so it is not a problem.
    var cancelled: Bool { event.outcome == "cancelled" }
    var failed: Bool { !succeeded && !cancelled }

    /// "Worked", "Failed", or "Canceled".
    var result: String {
        switch event.outcome {
        case "success": "Worked"
        case "cancelled": "Canceled"
        default: "Failed"
        }
    }

    /// Tool names arrive as `Server__tool`. The server half is shown once, as
    /// a word; the tool half stands alone.
    private var parts: (server: String?, tool: String) {
        guard let tool = event.tool, !tool.isEmpty else { return (event.server, event.method) }
        guard let range = tool.range(of: "__"), range.lowerBound > tool.startIndex,
              range.upperBound < tool.endIndex
        else { return (event.server, tool) }
        return (String(tool[..<range.lowerBound]), String(tool[range.upperBound...]))
    }

    var tool: String { parts.tool }
    var server: String? { parts.server }

    /// The icon to show for whoever called.
    var callerTarget: String {
        if let ownerName {
            let named = AppIcons.target(forClientType: ownerName)
            if AppIcons.displayName(forTarget: named) != nil { return named }
        }
        return AppIcons.target(forClientType: event.clientType ?? "")
    }

    /// The product name when Plug recognises the client; the client's own
    /// label otherwise. A raw client type is the last resort.
    var caller: String {
        if let ownerName, !ownerName.isEmpty { return ownerName }
        if let name = AppIcons.displayName(forTarget: callerTarget) { return name }
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
