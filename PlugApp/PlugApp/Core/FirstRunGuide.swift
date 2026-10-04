import Foundation

/// The three things a newcomer does, and how far along they are. Pure value,
/// so what counts as done can be tested without a daemon.
struct FirstRunGuide: Equatable, Sendable {
    enum Step: String, CaseIterable, Identifiable, Sendable {
        case server, client, activity

        var id: Self { self }

        var title: String {
            switch self {
            case .server: "Add a server"
            case .client: "Connect a client"
            case .activity: "Use a tool"
            }
        }

        var detail: String {
            switch self {
            case .server:
                "Import the ones your other apps already use, or add a new one."
            case .client:
                "Connect Claude, Codex, or Cursor and it gets every server at once."
            case .activity:
                "Ask your client to do something. The call shows up in Activity."
            }
        }
    }

    let serverCount: Int
    let clientCount: Int
    let hasActivity: Bool

    func isDone(_ step: Step) -> Bool {
        switch step {
        case .server: serverCount > 0
        case .client: clientCount > 0
        case .activity: hasActivity
        }
    }

    /// The first step not done yet, which is the one the guide points at.
    var next: Step? { Step.allCases.first { !isDone($0) } }

    var isComplete: Bool { next == nil }

    /// The guide opens by itself once: on a Mac where Plug has loaded and has
    /// no servers, and only until it has been seen.
    static func opensByItself(seen: Bool, loaded: Bool, serverCount: Int) -> Bool {
        !seen && loaded && serverCount == 0
    }

    /// What an agent is told so it can set Plug up for its person. Shipped as
    /// a file so the repository and the app say the same thing.
    static var agentPrompt: String? {
        guard let url = Bundle.main.url(forResource: "agent-setup", withExtension: "md") else { return nil }
        return try? String(contentsOf: url, encoding: .utf8)
    }
}
