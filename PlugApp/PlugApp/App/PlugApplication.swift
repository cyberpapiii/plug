import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        NotificationService.shared.requestAuthorization()
    }
}

@main
struct PlugApplication: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup("Plug", id: "main") {
            RootView(model: model)
                .frame(minWidth: 760, minHeight: 520)
                .task { await model.startMonitoring() }
        }
        .defaultSize(width: 940, height: 640)

        MenuBarExtra("Plug", systemImage: model.menuBarSymbol) {
            PlugMenu(model: model)
        }
        .menuBarExtraStyle(.menu)

        Settings { SettingsView(model: model) }
    }
}
