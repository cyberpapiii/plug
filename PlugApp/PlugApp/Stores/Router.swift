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
    var isAddingServer = false
    var isImportingServers = false
    /// The server whose settings are open for editing, if any.
    var editingServer: ServerName?

    /// A server name that a sheet can be presented from.
    struct ServerName: Identifiable, Equatable, Sendable {
        let id: String
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
        case let .restartServer(name):
            perform { .restartServer(authToken: $0, serverID: name) }
        case let .setServerEnabled(name, enabled):
            perform { .setServerEnabled(authToken: $0, name: name, enabled: enabled) }
        case let .editServer(name):
            router.section = .servers
            router.editingServer = Router.ServerName(id: name)
            showWindow()
        case let .setToolEnabled(tool, enabled):
            Task { await model.setToolEnabled(tool, enabled) }
        case let .linkApp(target):
            Task { await model.setAppLinked(target, true) }
        case let .unlinkApp(target):
            Task { await model.setAppLinked(target, false) }
        case let .removeServer(name):
            if router.selectedServer == name { router.selectedServer = nil }
            perform { .removeServer(authToken: $0, name: name) }
        case let .revokeClient(id):
            perform { .revokeClient(authToken: $0, clientID: id) }
        case .addServer:
            router.section = .servers
            router.isAddingServer = true
            showWindow()
        case .importServers:
            router.section = .servers
            router.isImportingServers = true
            showWindow()
        case let .signOut(server):
            Task { await model.signOut(server: server) }
        case let .openWindow(section):
            router.section = section
            showWindow()
        case .openCurrentWindow:
            showWindow()
        case let .reveal(server):
            router.reveal(server: server)
            showWindow()
        case .checkForUpdates:
            UpdateService.shared.checkForUpdates()
        case .restartService:
            guard model.beginServiceRestart() else { return }
            Task {
                do {
                    try await DaemonServiceManager.shared.restart()
                    model.finishServiceRestart()
                    await model.refresh()
                } catch {
                    model.finishServiceRestart(error: error)
                    await model.retryConnection()
                }
            }
        case .reloadConfiguration:
            perform { .reload(authToken: $0) }
        case .openLogs:
            NSWorkspace.shared.open(
                URL.homeDirectory.appending(path: "Library/Logs/plug", directoryHint: .isDirectory)
            )
        case .quit:
            NSApp.terminate(nil)
        }
    }

    private func perform(_ request: @escaping (String) -> IPCRequest) {
        Task { await model.perform(request) }
    }
}
