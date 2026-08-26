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
    var isAddingServer = false

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
        case let .removeServer(name):
            if router.selectedServer == name { router.selectedServer = nil }
            perform { .removeServer(authToken: $0, name: name) }
        case let .revokeClient(id):
            perform { .revokeClient(authToken: $0, clientID: id) }
        case .addServer:
            router.section = .servers
            router.isAddingServer = true
            showWindow()
        case let .openWindow(section):
            router.section = section
            showWindow()
        case let .reveal(server):
            router.reveal(server: server)
            showWindow()
        case .checkForUpdates:
            UpdateService.shared.checkForUpdates()
        case .openSettings:
            NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
        case .quit:
            NSApp.terminate(nil)
        }
    }

    private func perform(_ request: @escaping (String) -> IPCRequest) {
        Task { await model.perform(request) }
    }
}
