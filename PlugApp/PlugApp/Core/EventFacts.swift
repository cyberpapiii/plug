import Foundation
import PlugIPC

/// One event as the interface reads it. Pure value, so the wording can be
/// tested without a daemon.
struct EventFacts: Identifiable, Equatable, Sendable {
    enum Health: Equatable, Sendable {
        case waiting, watching, toolMissing, notReadOnly, callFailed, tooLarge

        init(state: String) {
            switch state {
            case "watching": self = .watching
            case "tool_missing": self = .toolMissing
            case "not_read_only": self = .notReadOnly
            case "call_failed": self = .callFailed
            case "too_large": self = .tooLarge
            default: self = .waiting
            }
        }

        /// The watch cannot do its job until something changes.
        var needsAttention: Bool {
            switch self {
            case .waiting, .watching: false
            case .toolMissing, .notReadOnly, .callFailed, .tooLarge: true
            }
        }
    }

    var id: String { name }
    /// `<server>.<name>`, which is what a client subscribes to.
    let name: String
    let server: String
    /// Nil for an event Plug does not make by watching a tool.
    let tool: String?
    let everySecs: UInt64?
    let health: Health
    let lastChecked: UInt64?
    let lastChanged: UInt64?
    let listeners: Int

    init(_ status: EventStatus) {
        name = status.name
        server = status.server
        tool = status.tool
        everySecs = status.everySecs
        health = Health(state: status.state)
        lastChecked = status.lastChecked
        lastChanged = status.lastChanged
        listeners = status.subscribers
    }

    /// Only a watch can be stopped from here.
    var canRemove: Bool { tool != nil }

    /// Where the event comes from, as one line under its name.
    var source: String {
        guard let tool else { return "Sent by \(server)" }
        guard let everySecs else { return "\(tool) on \(server)" }
        return "\(tool) on \(server), \(Self.interval(everySecs))"
    }

    var listenerLine: String {
        switch listeners {
        case 0: "Nobody"
        case 1: "1 client"
        default: "\(listeners) clients"
        }
    }

    /// How the watch is doing, in a word or two for a row.
    var stateWord: String {
        guard tool != nil else { return "Sent by \(server)" }
        switch health {
        case .watching: return "Watching"
        case .waiting: return "Waiting for the first check"
        case .toolMissing, .notReadOnly, .callFailed, .tooLarge: return "Needs attention"
        }
    }

    /// One plain sentence for how the watch is doing.
    func healthLine(now: UInt64) -> String {
        switch health {
        case .waiting:
            "Waiting for the first check."
        case .watching:
            lastChanged == nil
                ? "Checked \(Self.ago(lastChecked, now: now)). No change yet."
                : "Checked \(Self.ago(lastChecked, now: now)). Last changed \(Self.ago(lastChanged, now: now))."
        case .toolMissing:
            "Plug cannot find this tool. Check that the server is running."
        case .notReadOnly:
            "Not watching. This tool can change things."
        case .callFailed:
            "The last check failed (\(Self.ago(lastChecked, now: now))). Plug keeps trying."
        case .tooLarge:
            "The result is too large to send."
        }
    }

    static func interval(_ secs: UInt64) -> String {
        switch secs {
        case 60: "every minute"
        case 3600: "every hour"
        case let secs where secs % 3600 == 0: "every \(secs / 3600) hours"
        case let secs where secs % 60 == 0: "every \(secs / 60) minutes"
        default: "every \(secs) seconds"
        }
    }

    static func ago(_ timestamp: UInt64?, now: UInt64) -> String {
        guard let timestamp else { return "never" }
        let elapsed = now > timestamp ? now - timestamp : 0
        switch elapsed {
        case ..<60: return "just now"
        case ..<3600: return "\(elapsed / 60) min ago"
        case ..<86400: return "\(elapsed / 3600) hr ago"
        default:
            let days = elapsed / 86400
            return days == 1 ? "1 day ago" : "\(days) days ago"
        }
    }
}

/// What the add-a-watch sheet has gathered so far, and whether it is enough.
struct WatchDraft: Equatable, Sendable {
    /// How often a watch may check. The daemon refuses anything under 30
    /// seconds; a minute is the shortest worth offering.
    static let intervals: [UInt64] = [60, 300, 900, 3600]

    enum ArgumentsError: Error, Equatable {
        case notAnObject
    }

    /// A name for the event taken from the tool: lower case, with anything
    /// that is not a letter, digit, or underscore turned into an underscore.
    static func name(fromTool tool: String) -> String {
        var name = ""
        for scalar in tool.lowercased().unicodeScalars {
            let keep = ("a"..."z").contains(scalar) || ("0"..."9").contains(scalar) || scalar == "_"
            if keep {
                name.unicodeScalars.append(scalar)
            } else if !name.hasSuffix("_") {
                name.append("_")
            }
        }
        return name.trimmingCharacters(in: CharacterSet(charactersIn: "_"))
    }

    /// The arguments typed into the sheet. Empty text means none; anything
    /// else has to be one JSON object.
    static func arguments(from text: String) -> Result<[String: JSONValue], ArgumentsError> {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .success([:]) }
        guard let object = try? JSONSerialization.jsonObject(with: Data(trimmed.utf8)) as? [String: Any],
              case let .object(values)? = JSONValue(object)
        else { return .failure(.notAnObject) }
        return .success(values)
    }
}

extension String {
    /// A sentence fragment such as "every 5 minutes", made fit to stand alone
    /// as a value.
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}
