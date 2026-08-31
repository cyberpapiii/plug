import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

@main
struct PlugApplication: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var model = AppModel()
    @State private var router = Router()
    @Environment(\.openWindow) private var openWindow

    var body: some Scene {
        // The menu bar panel is the app. It answers "is Plug working?" and
        // carries the fix for whatever it just said, so most visits never open
        // a window at all.
        MenuBarExtra {
            PlugPopover(model: model, section: router.section, run: runner.run)
        } label: {
            // The label is the one view that exists from launch, so the
            // runtime connection starts here rather than in a window that may
            // never be opened.
            Image(systemName: model.menuBarSymbol)
                .accessibilityLabel("Plug: \(model.verdict.title)")
                .task { await model.start() }
        }
        .menuBarExtraStyle(.window)

        // The window is for the rare, deliberate work: adding a server,
        // auditing who is connected, reading history.
        Window("Plug", id: Self.windowID) {
            RootView(model: model, router: router, run: runner.run)
                .frame(minWidth: 760, minHeight: 500)
                .task { await model.start() }
        }
        .defaultSize(width: 920, height: 600)
        .windowResizability(.contentMinSize)
        .windowToolbarStyle(.unified)
        .commands {
            // Plug refreshes itself, so a refresh button would be visual weight
            // for something the app already does. The shortcut people reach for
            // out of habit still works.
            CommandGroup(after: .toolbar) {
                Button("Refresh") { Task { await model.refresh(forceCatalog: true) } }
                    .keyboardShortcut("r", modifiers: .command)
            }
        }

        Settings { SettingsView(model: model, run: runner.run) }
    }

    private static let windowID = "main"

    private var runner: PlugIntentRunner {
        PlugIntentRunner(model: model, router: router) {
            openWindow(id: Self.windowID)
            NSApp.activate(ignoringOtherApps: true)
        }
    }
}
