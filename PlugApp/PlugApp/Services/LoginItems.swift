import ServiceManagement
import UserNotifications

/// Where one thing Plug needs from macOS stands.
enum SystemAllowance: Equatable, Sendable {
    case allowed
    /// The person turned it off in System Settings; only they can turn it on.
    case turnedOff
    /// Nothing has asked macOS for it yet.
    case notAsked

    init(_ status: SMAppService.Status) {
        switch status {
        case .enabled: self = .allowed
        case .requiresApproval: self = .turnedOff
        case .notRegistered, .notFound: self = .notAsked
        @unknown default: self = .notAsked
        }
    }

    init(_ status: UNAuthorizationStatus) {
        switch status {
        case .authorized, .provisional, .ephemeral: self = .allowed
        case .denied: self = .turnedOff
        case .notDetermined: self = .notAsked
        @unknown default: self = .notAsked
        }
    }
}

/// What macOS says about Plug opening at login and running in the
/// background. Both are switched off by the person in System Settings >
/// Login Items, and neither says so on its own.
struct LoginItems: Equatable, Sendable {
    var opensAtLogin: Bool
    var background: SystemAllowance
    var login: SystemAllowance

    /// Each answer is a round trip to another process, so callers ask off
    /// the main thread. The background item is the daemon's LaunchAgent.
    static func read() -> LoginItems {
        let app = SMAppService.mainApp.status
        return LoginItems(
            opensAtLogin: app == .enabled,
            background: SystemAllowance(SMAppService.agent(plistName: DaemonServiceManager.agentPlistName).status),
            login: SystemAllowance(app)
        )
    }
}
