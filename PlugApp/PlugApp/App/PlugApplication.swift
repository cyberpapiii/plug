import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    var showWindow: (() -> Void)?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        // The daemon opens Plug hidden so no window comes up. A hidden app
        // cannot show its menu bar panel either, so it is hidden only until
        // it has finished opening.
        if NSApp.isHidden { NSApp.unhide(nil) }
        NotificationService.shared.install()
        MenuBarPresence.standard.appDidOpen()
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
            MenuBarLabel(mark: model.menuBarMark)
                .accessibilityLabel("Plug: \(model.verdict.title)")
                .task {
                    appDelegate.showWindow = { runner.run(.openCurrentWindow) }
                    NotificationService.shared.perform = { runner.run($0) }
                    IconStore.shared.attach(to: model)
                    await model.start()
                }
        }
        .menuBarExtraStyle(.window)

        // The window is for the rare, deliberate work: adding a server,
        // auditing who is connected, reading history.
        Window("Plug", id: Self.windowID) {
            RootView(model: model, router: router, run: runner.run)
                .frame(minWidth: 860, minHeight: 520)
                .task {
                    IconStore.shared.attach(to: model)
                    await model.start()
                }
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
                Button("Add Client…") { runner.run(.addClient) }
                    .disabled(router.sheet != nil)
                Button("Import Servers…") { runner.run(.importServers) }
                    .keyboardShortcut("i", modifiers: [.command, .shift])
                    .disabled(!model.canMutate || router.sheet != nil)
                Button("Watch a Tool…") { runner.run(.addWatch) }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
                    .disabled(!model.canMutate || router.sheet != nil)
            }
            CommandGroup(replacing: .appTermination) {
                Button("Quit Plug") { runner.run(.quit) }
                    .keyboardShortcut("q", modifiers: .command)
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

/// The menu bar icon. Awake, it blinks now and then; while Plug is busy it
/// looks from side to side. With Reduce Motion on it holds still.
private struct MenuBarLabel: View {
    private enum Motion { case still, blinking, looking }

    let mark: MenuBarMark
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var blinking = false
    @State private var gaze = 0

    private var motion: Motion {
        guard mark.awake, !reduceMotion else { return .still }
        return mark.working ? .looking : .blinking
    }

    var body: some View {
        Image(nsImage: MenuBarIcon.image(for: mark, blinking: blinking, gaze: gaze))
            .task(id: motion) {
                blinking = false
                gaze = 0
                do {
                    switch motion {
                    case .still:
                        return
                    case .blinking:
                        while true {
                            try await Task.sleep(for: .seconds(.random(in: 6...14)))
                            blinking = true
                            try await Task.sleep(for: .seconds(0.13))
                            blinking = false
                        }
                    case .looking:
                        while true {
                            for side in [1, -1] {
                                gaze = side
                                try await Task.sleep(for: .seconds(0.8))
                            }
                        }
                    }
                } catch {
                    blinking = false
                    gaze = 0
                }
            }
    }
}

/// Draws the menu bar icon: Plug's own mark, the plug with a face. A gap is
/// cut from it around the badge, the way the system's badged symbols are made.
enum MenuBarIcon {
    private struct Drawing: Hashable {
        let mark: MenuBarMark
        let blinking: Bool
        let gaze: Int
    }

    @MainActor private static var drawn: [Drawing: NSImage] = [:]

    @MainActor
    /// `gaze` is -1, 0 or 1: looking left, ahead, or right.
    static func image(for mark: MenuBarMark, blinking: Bool = false, gaze: Int = 0) -> NSImage {
        let drawing = Drawing(mark: mark, blinking: blinking, gaze: gaze)
        if let image = drawn[drawing] { return image }
        // A plain plug takes no more room than it needs.
        let width: CGFloat = mark.badge == nil ? 16 : 25
        let image = NSImage(size: NSSize(width: width, height: 18), flipped: false) { rect in
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            // A badged plug sits at the left, so the badge clears its eyes.
            let middle: CGFloat = mark.badge == nil ? rect.midX : 8
            drawPlug(
                awake: mark.awake, eyesOpen: mark.awake && !blinking, gaze: gaze,
                centre: CGPoint(x: middle, y: rect.midY), in: context
            )
            guard let name = mark.badge, let badge = symbol(name, points: 8.5, weight: .bold) else { return true }
            let size = badge.size
            let origin = NSPoint(x: rect.width - 5.5 - size.width / 2, y: 5 - size.height / 2)
            context.setBlendMode(.destinationOut)
            context.fillEllipse(in: CGRect(origin: origin, size: size).insetBy(dx: -1.2, dy: -1.2))
            context.setBlendMode(.normal)
            badge.draw(at: origin, from: .zero, operation: .sourceOver, fraction: 1)
            return true
        }
        // A template takes the menu bar's own colour, light or dark.
        image.isTemplate = true
        drawn[drawing] = image
        return image
    }

    /// Awake the mark is solid; asleep it is an outline with its eyes shut.
    private static func drawPlug(
        awake: Bool, eyesOpen: Bool, gaze: Int, centre: CGPoint, in context: CGContext
    ) {
        let scale = 15 / PlugMark.height
        var placement = PlugMark.placement(centre: centre, scale: scale, yUp: true)
        func add(_ path: Path) {
            if let placed = path.cgPath.copy(using: &placement) { context.addPath(placed) }
        }
        context.setFillColor(.black)
        add(PlugMark.silhouette)
        context.fillPath()

        let line = 1.3 / scale
        context.setBlendMode(.destinationOut)
        if !awake {
            add(Path(
                roundedRect: PlugMark.body.insetBy(dx: line, dy: line),
                cornerRadius: PlugMark.bodyCorner - line
            ))
            context.fillPath()
            context.setBlendMode(.normal)
        }
        add(PlugMark.eyes(open: eyesOpen ? 1 : 0, gaze: CGFloat(gaze) * 5, lid: line))
        context.fillPath()
        context.setBlendMode(.normal)
    }

    private static func symbol(_ name: String, points: CGFloat, weight: NSFont.Weight) -> NSImage? {
        NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: points, weight: weight))
    }
}
