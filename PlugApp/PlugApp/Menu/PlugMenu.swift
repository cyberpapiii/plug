import AppKit
import SwiftUI

struct PlugMenu: View {
    let model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Text(model.connectionState == .ready ? (model.isHealthy ? "Everything looks good" : "Plug needs attention") : "Plug is not connected")
        ForEach(model.visibleServers.prefix(6)) { server in
            Label(server.serverID, systemImage: server.health == "Healthy" ? "circle.fill" : "exclamationmark.circle.fill")
        }
        Divider()
        Button("Open Plug") { openWindow(id: "main"); NSApp.activate(ignoringOtherApps: true) }
        Button("Refresh") { Task { await model.refresh() } }
        Divider()
        Button("Quit Plug") { NSApp.terminate(nil) }
    }
}
