import Foundation

// MARK: - Server health

/// Health of one upstream server, normalized out of the daemon's string form so
/// the interface never compares raw protocol vocabulary.
enum ServerHealth: Equatable, Sendable {
    case working
    case starting
    case signInNeeded
    case down
    case off
    case unknown

    init(daemonValue: String?, enabled: Bool) {
        guard enabled else {
            self = .off
            return
        }
        switch daemonValue {
        case "Healthy": self = .working
        case "Starting", nil: self = .starting
        case "AuthRequired": self = .signInNeeded
        case "Failed": self = .down
        case "Disabled": self = .off
        default: self = .unknown
        }
    }

    /// Words a person reads. Never protocol vocabulary.
    var label: String {
        switch self {
        case .working: "Running"
        case .starting: "Starting"
        case .signInNeeded: "Sign-in needed"
        case .down: "Down"
        case .off: "Off"
        case .unknown: "Unknown"
        }
    }

    /// True when this server is the reason someone opened Plug.
    var needsAttention: Bool {
        switch self {
        case .signInNeeded, .down, .unknown: true
        case .working, .starting, .off: false
        }
    }

    var isSettling: Bool { self == .starting }
}

// MARK: - Facts

/// Everything the interface knows about one server, in one flat value.
struct ServerFacts: Identifiable, Equatable, Sendable {
    var id: String { name }
    let name: String
    let enabled: Bool
    let transport: String
    let usesOAuth: Bool
    let health: ServerHealth
    let toolCount: Int
    let error: String?
    let isSigningIn: Bool
    let tokenExpiresInSecs: UInt64?
    let authWarnings: [String]

    init(
        name: String,
        enabled: Bool,
        transport: String,
        usesOAuth: Bool = false,
        health: ServerHealth,
        toolCount: Int = 0,
        error: String? = nil,
        isSigningIn: Bool = false,
        tokenExpiresInSecs: UInt64? = nil,
        authWarnings: [String] = []
    ) {
        self.name = name
        self.enabled = enabled
        self.transport = transport
        self.usesOAuth = usesOAuth
        self.health = health
        self.toolCount = toolCount
        self.error = error
        self.isSigningIn = isSigningIn
        self.tokenExpiresInSecs = tokenExpiresInSecs
        self.authWarnings = authWarnings
    }

    /// Where the server runs, as a picture. A person recognizes the difference
    /// between "on this Mac" and "somewhere on the internet" faster from a
    /// screen and a globe than from either sentence.
    var transportSymbol: String {
        switch transport.lowercased() {
        case "stdio": "desktopcomputer"
        case "http", "sse", "streamable_http": "globe"
        default: "questionmark.square.dashed"
        }
    }

    /// What the row's second line is about, so the icon matches the words.
    var subtitleSymbol: String {
        if health.needsAttention, let error, !error.isEmpty { return "exclamationmark.triangle" }
        if !enabled { return "circle.slash" }
        return transportSymbol
    }

    var transportLabel: String {
        switch transport.lowercased() {
        case "stdio": "Runs on this Mac"
        case "http", "sse", "streamable_http": "Remote server"
        default: transport.capitalized
        }
    }
}

/// The complete input to every status decision the interface makes. Keeping it
/// a plain value means the verdict below is a pure function that tests can pin.
struct PlugSituation: Equatable, Sendable {
    /// Whether Plug itself is installed and permitted to run in the background.
    enum Setup: Equatable, Sendable {
        case ready
        case needsPermission
        case settingUp
        case needsRepair(detail: String)
        case blocked(detail: String, hasLog: Bool)
    }

    /// Whether the background runtime is reachable and speaking our version.
    enum Runtime: Equatable, Sendable {
        case running
        case starting
        case stopped
        case versionMismatch
    }

    let setup: Setup
    let runtime: Runtime
    let servers: [ServerFacts]
    let connectedApps: Int
    let version: String

    init(
        setup: Setup = .ready,
        runtime: Runtime = .stopped,
        servers: [ServerFacts] = [],
        connectedApps: Int = 0,
        version: String = ""
    ) {
        self.setup = setup
        self.runtime = runtime
        self.servers = servers
        self.connectedApps = connectedApps
        self.version = version
    }

    var activeServers: [ServerFacts] { servers.filter(\.enabled) }
    var troubledServers: [ServerFacts] { activeServers.filter(\.health.needsAttention) }
    var workingServers: [ServerFacts] { activeServers.filter { $0.health == .working } }
    var totalTools: Int { activeServers.reduce(0) { $0 + $1.toolCount } }
}

// MARK: - Intents

/// Every action the interface can start, in one list. Views name an intent;
/// a single dispatcher performs it. Nothing else calls the model directly.
enum PlugIntent: Equatable, Sendable {
    case allowBackgroundRunning
    case repairInstallation
    case showRepairLog
    case reconnect
    case signIn(server: String)
    case restartServer(String)
    case setServerEnabled(String, Bool)
    case editServer(String)
    case removeServer(String)
    case setToolEnabled(String, Bool)
    case linkApp(String)
    case unlinkApp(String)
    case revokeClient(id: String)
    case addServer
    case openWindow(AppSection)
    case openCurrentWindow
    case reveal(server: String)
    case checkForUpdates
    /// Restart the background service through the same path the installer
    /// uses. Nothing in the app ever kills it directly.
    case restartService
    case reloadConfiguration
    /// Copy servers over from the other AI apps on this Mac.
    case importServers
    /// Forget a server's stored account.
    case signOut(server: String)
    case openLogs
    case quit
}

// MARK: - Verdict

/// The one sentence Plug says about itself, everywhere it speaks. The menu bar
/// icon, the popover headline, and the window banner all render this value, so
/// the app can never contradict itself.
struct Verdict: Equatable, Sendable {
    enum Tone: Equatable, Sendable {
        case good
        case busy
        case attention
        case blocked
    }

    struct Button: Equatable, Sendable {
        let title: String
        let intent: PlugIntent

        init(_ title: String, _ intent: PlugIntent) {
            self.title = title
            self.intent = intent
        }
    }

    let tone: Tone
    let symbol: String
    let title: String
    let detail: String?
    let primary: Button?
    let secondary: Button?

    init(
        tone: Tone,
        symbol: String,
        title: String,
        detail: String? = nil,
        primary: Button? = nil,
        secondary: Button? = nil
    ) {
        self.tone = tone
        self.symbol = symbol
        self.title = title
        self.detail = detail
        self.primary = primary
        self.secondary = secondary
    }
}

/// One fixable problem, shown with the button that fixes it. The rule this
/// encodes: you never see a problem in a place where you cannot act on it.
struct AttentionItem: Identifiable, Equatable, Sendable {
    let id: String
    let symbol: String
    let title: String
    let detail: String
    let button: Verdict.Button?
    let isWorking: Bool

    init(
        id: String,
        symbol: String,
        title: String,
        detail: String,
        button: Verdict.Button? = nil,
        isWorking: Bool = false
    ) {
        self.id = id
        self.symbol = symbol
        self.title = title
        self.detail = detail
        self.button = button
        self.isWorking = isWorking
    }
}

// MARK: - Builder

/// Turns a situation into the single verdict and the list of fixable problems.
/// The order of the checks below is the product: the most blocking, most
/// specific, most actionable thing wins, and only one thing ever speaks.
enum PlugVerdict {
    static func verdict(for situation: PlugSituation) -> Verdict {
        if let setupVerdict = setupVerdict(for: situation.setup) { return setupVerdict }
        if let runtimeVerdict = runtimeVerdict(for: situation.runtime) { return runtimeVerdict }
        return serverVerdict(for: situation)
    }

    private static func setupVerdict(for setup: PlugSituation.Setup) -> Verdict? {
        switch setup {
        case .ready:
            return nil
        case .settingUp:
            return Verdict(
                tone: .busy,
                symbol: "bolt.horizontal.circle",
                title: "Setting up…",
                detail: "Finishing installation."
            )
        case .needsPermission:
            // Leftover launchd still lands here. CLI leftover copy is
            // LEFTOVER_LAUNCHD_ADOPT_SENTENCE, not this verdict.
            return Verdict(
                tone: .attention,
                symbol: "bolt.badge.checkmark",
                title: "Background running is off",
                detail: "Plug serves connected apps only while it runs in the background.",
                primary: .init("Turn On", .allowBackgroundRunning)
            )
        case let .needsRepair(detail):
            return Verdict(
                tone: .attention,
                symbol: "bolt.trianglebadge.exclamationmark",
                title: "Plug needs a repair",
                detail: detail,
                primary: .init("Repair", .repairInstallation)
            )
        case let .blocked(detail, hasLog):
            return Verdict(
                tone: .blocked,
                symbol: "bolt.trianglebadge.exclamationmark",
                title: "Setup incomplete",
                detail: detail,
                primary: .init("Try Again", .repairInstallation),
                secondary: hasLog ? .init("Show Log", .showRepairLog) : nil
            )
        }
    }

    private static func runtimeVerdict(for runtime: PlugSituation.Runtime) -> Verdict? {
        switch runtime {
        case .running:
            return nil
        case .starting:
            return Verdict(
                tone: .busy,
                symbol: "bolt.horizontal.circle",
                title: "Starting…",
                detail: "Connecting to servers."
            )
        case .stopped:
            return Verdict(
                tone: .blocked,
                symbol: "bolt.slash",
                title: "Plug is not running",
                detail: "Connected apps cannot reach any servers.",
                primary: .init("Start Plug", .reconnect)
            )
        case .versionMismatch:
            return Verdict(
                tone: .attention,
                symbol: "bolt.badge.clock",
                title: "Restart required to finish update",
                detail: "The new version is installed.",
                primary: .init("Restart Plug", .reconnect)
            )
        }
    }

    private static func serverVerdict(for situation: PlugSituation) -> Verdict {
        let troubled = situation.troubledServers
        let active = situation.activeServers

        if active.isEmpty {
            return Verdict(
                tone: .attention,
                symbol: "bolt.horizontal.circle",
                title: "No servers configured",
                detail: "Add a server to make tools available.",
                primary: .init("Add Server", .addServer)
            )
        }

        if troubled.count == 1, let only = troubled.first {
            switch only.health {
            case .signInNeeded:
                return Verdict(
                    tone: .attention,
                    symbol: "bolt.badge.checkmark",
                    title: "\(only.name) needs sign-in",
                    detail: only.isSigningIn
                        ? "Sign-in is open in the browser."
                        : "All other servers are running.",
                    primary: only.isSigningIn ? nil : .init("Sign In", .signIn(server: only.name))
                )
            default:
                return Verdict(
                    tone: .attention,
                    symbol: "bolt.trianglebadge.exclamationmark",
                    title: "\(only.name) is \(only.health.label.lowercased())",
                    detail: only.error ?? "All other servers are running.",
                    primary: .init("Restart", .restartServer(only.name))
                )
            }
        }

        if troubled.count > 1 {
            let signIns = troubled.filter { $0.health == .signInNeeded }.count
            let detail: String = signIns == troubled.count
                ? "All are waiting for sign-in."
                : "\(situation.workingServers.count) of \(active.count) servers running."
            return Verdict(
                tone: .attention,
                symbol: "bolt.trianglebadge.exclamationmark",
                title: "\(troubled.count) servers need attention",
                detail: detail
            )
        }

        if active.contains(where: \.health.isSettling) {
            return Verdict(
                tone: .busy,
                symbol: "bolt.horizontal.circle",
                title: "Starting servers…",
                detail: "\(situation.workingServers.count) of \(active.count) ready."
            )
        }

        return Verdict(
            tone: .good,
            symbol: "bolt.fill",
            title: "All servers running",
            detail: readyDetail(for: situation)
        )
    }

    private static func readyDetail(for situation: PlugSituation) -> String {
        let servers = situation.activeServers.count
        let serverWord = servers == 1 ? "server" : "servers"
        let tools = situation.totalTools
        guard tools > 0 else { return "\(servers) \(serverWord)" }
        let toolWord = tools == 1 ? "tool" : "tools"
        return "\(servers) \(serverWord) · \(tools) \(toolWord)"
    }

    /// Problems worth listing, each paired with the button that resolves it.
    static func attention(for situation: PlugSituation) -> [AttentionItem] {
        situation.troubledServers.map { server in
            switch server.health {
            case .signInNeeded:
                return AttentionItem(
                    id: server.name,
                    symbol: "person.badge.key",
                    title: server.name,
                    detail: server.isSigningIn ? "Sign-in open in browser" : "Sign-in needed",
                    button: server.isSigningIn ? nil : .init("Sign In", .signIn(server: server.name)),
                    isWorking: server.isSigningIn
                )
            default:
                return AttentionItem(
                    id: server.name,
                    symbol: "exclamationmark.triangle",
                    title: server.name,
                    detail: server.error.map(firstLine) ?? server.health.label,
                    button: .init("Restart", .restartServer(server.name))
                )
            }
        }
    }

    private static func firstLine(_ text: String) -> String {
        let line = text.split(separator: "\n").first.map(String.init) ?? text
        return line.count > 120 ? String(line.prefix(119)) + "…" : line
    }

    /// The menu bar icon. Shape carries the state, not colour, so it stays
    /// readable in a monochrome menu bar and for colour-blind eyes.
    static func menuBarSymbol(for verdict: Verdict) -> String {
        switch verdict.tone {
        case .good: "bolt.fill"
        case .busy: "bolt.horizontal.circle"
        case .attention: "bolt.trianglebadge.exclamationmark.fill"
        case .blocked: "bolt.slash.fill"
        }
    }
}
