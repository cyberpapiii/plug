import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    var showWindow: (() -> Void)?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        NotificationService.shared.install()
        installSnapshotHook()
    }

    /// A developer aid, off unless `defaults write com.cyberpapiii.plug
    /// snapshotDirectory <path>` names a folder. Posting the distributed
    /// notification `com.cyberpapiii.plug.snapshot` then makes Plug draw each
    /// of its own windows into a PNG there, so a screen can be reviewed
    /// without screen recording. Plug only ever draws its own views.
    private func installSnapshotHook() {
        guard UserDefaults.standard.string(forKey: "snapshotDirectory") != nil else { return }
        DistributedNotificationCenter.default().addObserver(
            forName: Notification.Name("com.cyberpapiii.plug.snapshot"), object: nil, queue: .main
        ) { _ in
            MainActor.assumeIsolated {
                guard let path = UserDefaults.standard.string(forKey: "snapshotDirectory") else { return }
                let directory = URL(fileURLWithPath: path, isDirectory: true)
                for (index, window) in NSApp.windows.filter(\.isVisible).enumerated() {
                    // The frame view, so the toolbar and sidebar are in the picture.
                    guard let view = window.contentView?.superview ?? window.contentView,
                          let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { continue }
                    view.cacheDisplay(in: view.bounds, to: bitmap)
                    let name = "window-\(index)-\(Int(view.bounds.width))x\(Int(view.bounds.height)).png"
                    try? bitmap.representation(using: .png, properties: [:])?
                        .write(to: directory.appending(path: name))
                }
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationShouldHandleReopen(
        _ sender: NSApplication,
        hasVisibleWindows flag: Bool
    ) -> Bool {
        if !flag { showWindow?() }
        return true
    }
}

@main
struct PlugApplication: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var model = AppModel()
    @State private var router = Router()
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings

    var body: some Scene {
        // The menu bar panel is the app. It answers "is Plug working?" and
        // carries the fix for whatever it just said, so most visits never open
        // a window at all.
        MenuBarExtra {
            PlugPopover(model: model, run: runner.run)
        } label: {
            // The label is the one view that exists from launch, so the
            // runtime connection starts here rather than in a window that may
            // never be opened.
            Image(systemName: model.menuBarSymbol)
                .accessibilityLabel("Plug: \(model.verdict.title)")
                .task {
                    appDelegate.showWindow = { runner.run(.openCurrentWindow) }
                    NotificationService.shared.perform = { runner.run($0) }
                    await model.start()
                }
        }
        .menuBarExtraStyle(.window)

        // The window is for the rare, deliberate work: adding a server,
        // auditing who is connected, reading history.
        Window("Plug", id: Self.windowID) {
            RootView(model: model, router: router, run: runner.run)
                .frame(minWidth: 860, minHeight: 520)
                .task { await model.start() }
        }
        .defaultSize(width: 980, height: 640)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(after: .appInfo) {
                Button("Check for Updates…") { runner.run(.checkForUpdates) }
            }
            CommandGroup(replacing: .newItem) {
                // One sheet at a time: these wait while another is open.
                Button("Add Server…") { runner.run(.addServer) }
                    .keyboardShortcut("n", modifiers: .command)
                    .disabled(!model.canMutate || router.sheet != nil)
                Button("Import Servers…") { runner.run(.importServers) }
                    .keyboardShortcut("i", modifiers: [.command, .shift])
                    .disabled(!model.canMutate || router.sheet != nil)
                Button("Watch a Tool…") { runner.run(.addWatch) }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
                    .disabled(!model.canMutate || router.sheet != nil)
            }
            SidebarCommands()
            CommandGroup(after: .sidebar) {
                Divider()
                ForEach(Array(AppSection.allCases.enumerated()), id: \.element) { index, section in
                    Button(section.rawValue) { runner.run(.openWindow(section)) }
                        .keyboardShortcut(KeyEquivalent(Character("\(index + 1)")), modifiers: .command)
                }
                Divider()
                // Plug refreshes itself, so a refresh button would be visual
                // weight for something the app already does. The shortcut
                // people reach for out of habit still works.
                Button("Refresh") { Task { await model.refresh(forceCatalog: true) } }
                    .keyboardShortcut("r", modifiers: .command)
            }
            CommandGroup(replacing: .help) {
                Button("How Plug Works") { runner.run(.showGuide) }
                Button("Run Checkup") { runner.run(.checkup) }
                Button("Show Logs in Finder") { runner.run(.openLogs) }
            }
        }

        // Settings is its own window, on Command-comma, like every Mac app.
        Settings {
            SettingsView(model: model, router: router, run: runner.run)
        }
    }

    private static let windowID = "main"

    private var runner: PlugIntentRunner {
        PlugIntentRunner(
            model: model,
            router: router,
            showWindow: {
                openWindow(id: Self.windowID)
                NSApp.activate(ignoringOtherApps: true)
            },
            showSettings: {
                openSettings()
                NSApp.activate(ignoringOtherApps: true)
            }
        )
    }
}
