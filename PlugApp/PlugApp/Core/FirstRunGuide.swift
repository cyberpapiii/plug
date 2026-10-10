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

        /// What Plug says the first time this step is done.
        var firstTitle: String {
            switch self {
            case .server: "Your first server is in"
            case .client: "Your first client is connected"
            case .activity: "That was your first tool call"
            }
        }

        var firstDetail: String {
            switch self {
            case .server: "Connect a client and it can use these tools."
            case .client: "It can use every server in Plug."
            case .activity: "Plug is all set. You will find it in the menu bar."
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

/// The first time each step is done on a new setup, Plug marks it, once.
/// This remembers which steps have had their moment.
struct FirstMoments: Equatable, Sendable {
    /// The steps already marked or passed over. Nil until the first look.
    private(set) var marked: Set<FirstRunGuide.Step>?

    init(stored: String?) {
        marked = stored.map { Set($0.split(separator: ",").compactMap { FirstRunGuide.Step(rawValue: String($0)) }) }
    }

    /// What to keep between launches.
    var stored: String? {
        marked.map { steps in FirstRunGuide.Step.allCases.filter(steps.contains).map(\.rawValue).joined(separator: ",") }
    }

    /// Looks at where the setup is now and returns the step to mark, if one
    /// was just done. A Mac that already had servers at the first look is
    /// not new, so nothing is ever marked there.
    mutating func observe(_ guide: FirstRunGuide) -> FirstRunGuide.Step? {
        let done = Set(FirstRunGuide.Step.allCases.filter(guide.isDone))
        guard let marked else {
            self.marked = guide.serverCount > 0 ? Set(FirstRunGuide.Step.allCases) : done
            return nil
        }
        self.marked = marked.union(done)
        // Two at once say the later one.
        return FirstRunGuide.Step.allCases.last { done.contains($0) && !marked.contains($0) }
    }
}

extension AppModel {
    /// Where this Mac is in the three steps.
    var firstRunGuide: FirstRunGuide {
        FirstRunGuide(
            serverCount: snapshot.configuredServers.count,
            clientCount: connectableApps.filter { $0.linked || $0.live }.count + snapshot.downstreamClients.count,
            hasActivity: !activities.isEmpty
        )
    }
}
