import AppKit
import SwiftUI

struct PlugMenu: View {
    let model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Label(
            model.connectionState == .ready
                ? (model.isHealthy ? "Running normally" : "Needs attention")
                : "Not connected",
            systemImage: model.menuBarSymbol
        )
        .disabled(true)
        Text("\(model.visibleServers.filter(\.configured.enabled).count) servers · \(model.snapshot.liveSessions.count) clients")
            .disabled(true)
        if !attentionServers.isEmpty {
            Divider()
            ForEach(attentionServers.prefix(4)) { server in
                Label(server.configured.name, systemImage: "exclamationmark.circle.fill")
            }
        }
        Divider()
        Button("Open Plug") { openWindow(id: "main"); NSApp.activate(ignoringOtherApps: true) }
        Button("Refresh") { Task { await model.refresh() } }
        Button("Check for Updates…") { UpdateService.shared.checkForUpdates() }
        if model.adoptionIsRequired {
            Button("Use Plug") { Task { await model.adopt() } }
        } else if model.connectionRecoveryIsRequired {
            Button("Retry") { Task { await model.retryConnection() } }
        } else if model.installationFailure != nil {
            Button("Retry") { Task { await model.retry() } }
            if model.installationFailure?.logURL != nil {
                Button("View Log") { model.openLog() }
            }
        }
        Divider()
        Button("Quit Plug") { NSApp.terminate(nil) }
    }

    private var attentionServers: [AppModel.ServerPresentation] {
        model.visibleServers.filter { $0.configured.enabled && $0.health != "Healthy" }
    }
}
