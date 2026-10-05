import AppKit
import PlugIPC
import Observation
import SwiftUI

/// Where the window is pointed. Kept outside `AppModel` so navigation never
/// mixes with runtime state.
@MainActor @Observable
final class Router {
    var section: AppSection = .servers
    var selectedServer: String?
    /// The tool whose details are open, by its merged name.
    var selectedTool: String?
    /// The one sheet the window is showing, if any.
    var sheet: Sheet?
    /// The selected row in each of the other sections.
    var selectedClient: String?
    var selectedEvent: String?
    var selectedCall: UInt64?
    /// Whether Activity shows every call or only the failed ones.
    var activityScope: ActivityView.Scope = .everything
    /// Counts the times a checkup was asked for from outside Settings, so
    /// Settings runs one each time the number moves.
    var checkupRequests = 0

    /// Every sheet the window can show. The window shows one at a time, so
    /// asking for another replaces the one that is up.
    enum Sheet: Identifiable, Equatable, Sendable {
        case guide
        case addServer
        case importServers
        case addWatch
        /// A server's settings, open for editing.
        case editServer(String)
        /// A server getting a second account.
        case addAccount(String)

        var id: String {
            switch self {
            case .guide: "guide"
            case .addServer: "addServer"
            case .importServers: "importServers"
            case .addWatch: "addWatch"
            case let .editServer(name): "editServer:\(name)"
            case let .addAccount(server): "addAccount:\(server)"
            }
        }
    }

    func reveal(server: String) {
        section = .servers
        selectedServer = server
    }
}

/// The single place an interface action turns into work. Views name a
/// `PlugIntent`; nothing in the view layer talks to the model directly.
@MainActor
struct PlugIntentRunner {
    let model: AppModel
    let router: Router
    var showWindow: () -> Void = {}
    var showSettings: () -> Void = {}

    func run(_ intent: PlugIntent) {
        switch intent {
        case .allowBackgroundRunning:
            Task { await model.adopt() }
        case .repairInstallation:
            Task { await model.retry() }
        case .showRepairLog:
            model.openLog()
        case .reconnect:
            Task { await model.retryConnection() }
        case let .signIn(server):
            Task { await model.signIn(server: server) }
        case let .cancelSignIn(server):
            model.cancelSignIn(server: server)
        case let .restartServer(name):
            perform("restart \(name)") { .restartServer(authToken: $0, serverID: name) }
        case let .setServerEnabled(name, enabled):
            perform("turn \(name) \(enabled ? "on" : "off")") {
                .setServerEnabled(authToken: $0, name: name, enabled: enabled)
            }
        case let .editServer(name):
            router.reveal(server: name)
            router.sheet = .editServer(name)
            showWindow()
        case let .addAccount(server):
            router.reveal(server: server)
            router.sheet = .addAccount(server)
            showWindow()
        case let .setToolEnabled(tool, enabled):
            Task { await model.setToolEnabled(tool, enabled) }
        case let .linkApp(target):
            Task { await model.setAppLinked(target, true) }
        case let .unlinkApp(target):
            Task { await model.setAppLinked(target, false) }
        case let .removeServer(name):
            if router.selectedServer == name { router.selectedServer = nil }
            perform("remove \(name)") { .removeServer(authToken: $0, name: name) }
        case let .revokeClient(id):
            perform("remove that client's access") { .revokeClient(authToken: $0, clientID: id) }
        case let .renameClient(key, name):
            perform("rename the client") { .renameClient(authToken: $0, key: key, name: name) }
        case let .setClientServerBlocked(key, server, blocked):
            perform("\(blocked ? "turn off" : "turn on") \(server) for that client") {
                .setClientServerBlocked(authToken: $0, key: key, server: server, blocked: blocked)
            }
        case let .setClientToolBlocked(key, tool, blocked):
            perform("\(blocked ? "turn off" : "turn on") that tool for that client") {
                .setClientToolBlocked(authToken: $0, key: key, tool: tool, blocked: blocked)
            }
        case .addServer:
            router.section = .servers
            router.sheet = .addServer
            showWindow()
        case .addWatch:
            router.section = .events
            router.sheet = .addWatch
            showWindow()
        case let .removeWatch(event):
            perform("stop watching \(event)") { .removeWatch(authToken: $0, event: event) }
        case .importServers:
            router.section = .servers
            router.sheet = .importServers
            showWindow()
        case let .signOut(server):
            Task { await model.signOut(server: server) }
        case let .openWindow(section):
            router.section = section
            showWindow()
        case .showGuide:
            router.sheet = .guide
            showWindow()
        case .openSettings:
            showSettings()
        case .checkup:
            router.checkupRequests += 1
            showSettings()
        case .openCurrentWindow:
            showWindow()
        case let .reveal(server):
            router.reveal(server: server)
            showWindow()
        case .checkForUpdates:
            UpdateService.shared.checkForUpdates()
        case .restartService:
            Task { await model.restartService() }
        case let .setServiceEnabled(enabled):
            Task { await model.setServiceEnabled(enabled) }
        case .reloadConfiguration:
            perform("reload the settings file") { .reload(authToken: $0) }
        case .openLogs:
            NSWorkspace.shared.activateFileViewerSelecting([
                URL.homeDirectory.appending(path: "Library/Logs/plug", directoryHint: .isDirectory),
            ])
        case .dismissActionError:
            model.dismissActionError()
        case .quit:
            NSApp.terminate(nil)
        }
    }

    private func perform(_ doing: String, _ request: @escaping (String) -> IPCRequest) {
        Task { await model.perform(doing, request) }
    }
}
