import AppKit
import ServiceManagement
import SwiftUI
import UserNotifications

/// Everything a person owns about Plug, in the Settings window: is Plug on
/// and healthy, how should it behave, where are its files, what version is
/// this. One page, so nothing is behind a tab.
struct SettingsView: View {
    let model: AppModel
    let router: Router
    let run: (PlugIntent) -> Void
    var checkups: any CheckupRunning = CheckupService()

    // The login item lives in macOS, not in this app's defaults, so the toggle
    // starts from what macOS last said and the task below asks again.
    @State private var loginItems = LoginItems.last
    @AppStorage(NotificationService.preferenceKey) private var notify = false
    @State private var loginItemFailed = false
    /// Goes up when the switch is used, so an answer asked for before then
    /// does not undo it.
    @State private var loginItemEdits = 0
    @State private var notificationsDenied = false
    // This page is made when the app opens, and reading this is what starts
    // the updater then. Keep it here.
    @State private var automaticUpdates = UpdateService.shared.checksAutomatically
    @State private var keepsInMenuBar = MenuBarPresence.standard.keeps
    @State private var notifications = SystemAllowance.notAsked

    @State private var checkup: CheckupReport?
    @State private var checking = false
    @State private var checkupError: String?
    @State private var showsPassingChecks = false
    @State private var configURL: URL?

    /// A closed Settings window is kept, with every view in it still
    /// running. While it is closed nothing here animates or asks macOS
    /// anything.
    @State private var windowOpen = true
    @State private var activations = 0

    /// What sends the page back to macOS for the state of its permissions.
    private struct SystemRead: Equatable {
        let windowOpen: Bool
        let serviceEnabled: Bool
        let activations: Int
    }

    var body: some View {
        Form {
            Section {
                if windowOpen {
                    PlugHeadline(model: model, run: run)
                } else {
                    Color.clear.frame(height: 40)
                }
            }
            plug
            general
            permissions
            files
            about
        }
        .formStyle(.grouped)
        .frame(width: Metric.settingsWidth)
        .frame(height: Metric.settingsHeight)
        .background {
            WindowOpenReader(isOpen: $windowOpen)
                .frame(width: 0, height: 0)
                .accessibilityHidden(true)
        }
        .task {
            if configURL == nil { configURL = await checkups.configPath() }
        }
        // Read when the window opens, when Plug is turned on or off, and when
        // the person comes back from System Settings, where the login item
        // is allowed.
        .task(id: SystemRead(
            windowOpen: windowOpen, serviceEnabled: model.serviceEnabled, activations: activations
        )) {
            guard windowOpen else { return }
            await readSystem()
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            if windowOpen { activations += 1 }
        }
        // A problem elsewhere in the app can send a person here with the
        // checkup already asked for.
        .task(id: router.checkupRequests) {
            if router.checkupRequests > 0 { await runCheckup() }
        }
    }

    // MARK: General

    private var general: some View {
        Section(heading: "General") {
            Toggle("Open at Login", isOn: Binding(
                get: { loginItems.opensAtLogin },
                set: { setLoginItem($0) }
            ))
            if loginItemFailed {
                LabeledContent {
                    Button("Open Login Items…") { DaemonServiceManager.shared.openLoginItemSettings() }
                } label: {
                    Text("macOS needs you to allow this in System Settings.")
                        .foregroundStyle(.secondary)
                }
            }
            Toggle(isOn: $keepsInMenuBar) {
                Text("Keep in Menu Bar")
                Text("While Plug is serving, its icon is in the menu bar. It comes back if something else closes it.")
            }
            .onChange(of: keepsInMenuBar) { _, keeps in
                MenuBarPresence.standard.keeps = keeps
            }
            Toggle(isOn: $notify) {
                Text("Notifications")
                Text(notificationsDenied
                    ? "Allow Plug in System Settings > Notifications."
                    : "When a server needs sign-in or a new client connects.")
            }
            .onChange(of: notify) { _, enabled in
                guard enabled else { return }
                Task {
                    let allowed = await NotificationService.shared.requestAuthorization()
                    notificationsDenied = !allowed
                    if !allowed { notify = false }
                    await readSystem()
                }
            }
            Toggle("Check for updates automatically", isOn: $automaticUpdates)
                .onChange(of: automaticUpdates) { _, enabled in
                    UpdateService.shared.checksAutomatically = enabled
                }
        }
    }

    // MARK: Plug

    private var plug: some View {
        Section {
            ServiceStatusRow(model: model, run: run, animates: windowOpen)
            checkupRows
        } header: {
            SectionLabel(text: "Plug")
        } footer: {
            Text("Plug keeps serving your clients after you close the window or quit. To stop it, turn Plug off.")
        }
    }

    // MARK: Permissions

    /// What Plug itself needs from macOS, and whether it has it. A row with
    /// nothing to fix says so and offers nothing.
    private var permissions: some View {
        Section {
            BackgroundItemRow(model: model, allowance: loginItems.background)
            PermissionRow(
                title: "Open at Login",
                detail: "Puts Plug in the menu bar when you log in.",
                allowance: loginItems.login == .turnedOff || loginItems.opensAtLogin ? loginItems.login : nil,
                idle: "Off",
                fix: "Open Login Items…"
            ) { DaemonServiceManager.shared.openLoginItemSettings() }
            PermissionRow(
                title: "Notifications",
                detail: "Tells you when a server needs sign-in or a new client connects.",
                allowance: notifications == .turnedOff || notify ? notifications : nil,
                idle: "Off",
                fix: "Open Notifications…"
            ) {
                if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") {
                    NSWorkspace.shared.open(url)
                }
            }
        } header: {
            SectionLabel(text: "Permissions")
        } footer: {
            Text("Plug keeps sign-ins and keys in your Keychain. When macOS asks, choose Always Allow. A server that reads your Mac's data has its own permissions under Privacy & Security.")
        }
    }

    // MARK: Checkup

    @ViewBuilder private var checkupRows: some View {
        LabeledContent {
            HStack(spacing: Metric.tight) {
                if checking, windowOpen { WaitingDots() }
                Button("Run Checkup") { Task { await runCheckup() } }
                    .disabled(checking)
            }
        } label: {
            if let checkup = checkup?.checkup {
                Label {
                    Text(checkup.headline)
                } icon: {
                    PlugIcon(headlineIcon(for: checkup))
                        .foregroundStyle(headlineColor(for: checkup))
                }
            } else {
                Text("Checkup")
            }
        }

        if let checkupError {
            ProblemNote(title: "The checkup could not run", reason: checkupError, failed: true)
        }

        if let checkup {
            ForEach(checkup.troubled) { check in
                CheckRow(check: check)
            }
            if !checkup.passed.isEmpty {
                DisclosureGroup(
                    passingChecksTitle(in: checkup),
                    isExpanded: $showsPassingChecks
                ) {
                    ForEach(checkup.passed) { check in
                        CheckRow(check: check)
                    }
                }
            }
        }
    }

    // MARK: Files

    private var files: some View {
        Section {
            LabeledContent("Settings file") {
                HStack(spacing: Metric.tight) {
                    ReloadButton(model: model, run: run)
                    Button("Show in Finder") {
                        if let configURL {
                            NSWorkspace.shared.activateFileViewerSelecting([configURL])
                        }
                    }
                    .disabled(configURL == nil)
                }
            }
            LabeledContent("Logs") {
                Button("Show in Finder") { run(.openLogs) }
            }
        } header: {
            SectionLabel(text: "Files")
        } footer: {
            Text("Plug saves your servers and clients here. Reload after editing the file by hand.")
        }
    }

    // MARK: About

    private var about: some View {
        Section(heading: "About") {
            HStack(spacing: Metric.snug) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 48, height: 48)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: Metric.hairline) {
                    Text("Plug").font(.headline)
                    Text("One place on your Mac that holds every tool and gives it to every client.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            LabeledContent("Version") {
                HStack(spacing: Metric.tight) {
                    VersionText(model: model)
                    Button("Check for Updates…") { run(.checkForUpdates) }
                        .disabled(!UpdateService.shared.canCheckForUpdates)
                }
            }
        }
    }

    /// Asks macOS where the login items and notifications stand. Each answer
    /// is a round trip to another process, so none of it waits on the main
    /// thread.
    private func readSystem() async {
        let edits = loginItemEdits
        let items = await Task.detached(priority: .userInitiated) { LoginItems.read() }.value
        // The settings object cannot cross to the main actor; only what it
        // says does.
        let allowance = await withCheckedContinuation { done in
            UNUserNotificationCenter.current().getNotificationSettings {
                done.resume(returning: SystemAllowance($0.authorizationStatus))
            }
        }
        // Something newer has been asked for, and its answer is on the way.
        guard !Task.isCancelled else { return }
        if edits == loginItemEdits {
            LoginItems.last = items
            if items != loginItems { loginItems = items }
            if items.opensAtLogin, loginItemFailed { loginItemFailed = false }
        }
        if allowance != notifications { notifications = allowance }
    }

    // MARK: Login item

    /// The switch only moves when macOS agrees. When it does not, the row
    /// under it says where to allow it; System Settings is not opened for the
    /// person.
    private func setLoginItem(_ enabled: Bool) {
        do {
            try DaemonServiceManager.shared.setMainAppAtLogin(enabled)
            loginItems.opensAtLogin = enabled
            loginItemEdits += 1
            loginItemFailed = false
        } catch {
            loginItemFailed = true
        }
    }

    // MARK: Checkup results

    private func headlineIcon(for checkup: Checkup) -> PlugIcon.Kind {
        if !checkup.problems.isEmpty { return .stopped }
        return checkup.isClean ? .worked : .needsYou
    }

    private func headlineColor(for checkup: Checkup) -> Color {
        if !checkup.problems.isEmpty { return StatusColor.stopped }
        return checkup.isClean ? StatusColor.working : StatusColor.needsYou
    }

    private func runCheckup() async {
        checking = true
        checkupError = nil
        showsPassingChecks = false
        do {
            let result = try await checkups.run()
            // A checkup that checked nothing did not run.
            if result.checks.isEmpty { throw CheckupError.unreadable }
            checkup = CheckupReport(result)
        } catch {
            checkup = nil
            checkupError = error.localizedDescription
        }
        checking = false
    }

    private func passingChecksTitle(in report: CheckupReport) -> String {
        report.checkup.isClean ? "Details" : "\(report.passed.count) passed"
    }
}

/// What macOS says about Plug opening at login and running in the
/// background. Asking is a round trip to another process each time, so the
/// last answer is kept and a new one is asked for off the main thread.
private struct LoginItems: Equatable, Sendable {
    var opensAtLogin: Bool
    var background: SystemAllowance
    var login: SystemAllowance

    /// The last answer. The first is read as the app opens, so the page
    /// never opens showing a guess.
    @MainActor static var last = read()

    /// The background item is the daemon's job, under the name
    /// `DaemonServiceManager` registers it with.
    static func read() -> LoginItems {
        let app = SMAppService.mainApp.status
        return LoginItems(
            opensAtLogin: app == .enabled,
            background: SystemAllowance(SMAppService.agent(plistName: "com.plug.daemon.plist").status),
            login: SystemAllowance(app)
        )
    }
}

/// A finished checkup with its rows already split into what needs a person
/// and what passed, so drawing the page does not sort them again.
private struct CheckupReport {
    let checkup: Checkup
    /// Problems, then warnings.
    let troubled: [Check]
    let passed: [Check]

    init(_ checkup: Checkup) {
        let ordered = checkup.ordered
        self.checkup = checkup
        troubled = ordered.filter { $0.result != .pass }
        passed = ordered.filter { $0.result == .pass }
    }
}

/// Tells the page whether its window is open. Closing says no; coming back
/// on screen says yes.
private struct WindowOpenReader: NSViewRepresentable {
    @Binding var isOpen: Bool

    func makeNSView(context: Context) -> WindowProbe {
        let probe = WindowProbe()
        let binding = $isOpen
        probe.report = { open in
            if binding.wrappedValue != open { binding.wrappedValue = open }
        }
        return probe
    }

    func updateNSView(_ probe: WindowProbe, context: Context) {}
}

private final class WindowProbe: NSView {
    var report: ((Bool) -> Void)?

    private static let watched = [
        NSWindow.willCloseNotification,
        NSWindow.didBecomeKeyNotification,
        NSWindow.didChangeOcclusionStateNotification,
    ]

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        let center = NotificationCenter.default
        for name in Self.watched { center.removeObserver(self, name: name, object: nil) }
        guard let window else { return }
        center.addObserver(self, selector: #selector(closed(_:)), name: NSWindow.willCloseNotification, object: window)
        center.addObserver(self, selector: #selector(shown(_:)), name: NSWindow.didBecomeKeyNotification, object: window)
        center.addObserver(
            self, selector: #selector(occlusionChanged(_:)),
            name: NSWindow.didChangeOcclusionStateNotification, object: window
        )
    }

    @objc private func closed(_ note: Notification) { report?(false) }

    @objc private func shown(_ note: Notification) { report?(true) }

    @objc private func occlusionChanged(_ note: Notification) {
        if window?.occlusionState.contains(.visible) == true { report?(true) }
    }
}

/// Whether Plug is running, and the button that restarts it. Shown only
/// while Plug is on. It reads the model itself, so a change in Plug's state
/// redraws this row and not the whole page.
private struct ServiceStatusRow: View {
    let model: AppModel
    let run: (PlugIntent) -> Void
    /// False while the window is closed.
    let animates: Bool

    var body: some View {
        if model.serviceEnabled {
            LabeledContent("Status") {
                HStack(spacing: Metric.tight) {
                    if serviceIsSettling, animates {
                        WaitingDots()
                    } else {
                        PlugIcon(serviceIcon)
                            .foregroundStyle(serviceColor)
                    }
                    Text(serviceStatus)
                    Button("Restart") { run(.restartService) }
                        .help("Reconnects every server. Clients pick Plug back up on their own.")
                        .disabled(model.isRestartingService || model.isChangingService)
                }
            }
        }
    }

    /// Only read while Plug is on; the Status row is hidden when it is off.
    private var serviceIsSettling: Bool {
        if model.isRestartingService { return true }
        switch model.connectionState {
        case .connecting, .reconnecting: return true
        case .ready, .incompatible, .disconnected: return false
        }
    }

    private var serviceStatus: String {
        if model.isRestartingService { return "Restarting" }
        switch model.connectionState {
        case .ready: return "Running"
        case .connecting: return "Connecting"
        case .reconnecting: return "Reconnecting"
        case .incompatible: return "Restart needed"
        case .disconnected: return "Not running"
        }
    }

    private var serviceIcon: PlugIcon.Kind {
        switch model.connectionState {
        case .incompatible: .needsYou
        case .disconnected: .stopped
        case .ready, .connecting, .reconnecting: .worked
        }
    }

    private var serviceColor: Color {
        switch model.connectionState {
        case .incompatible: StatusColor.needsYou
        case .disconnected: StatusColor.stopped
        case .ready, .connecting, .reconnecting: StatusColor.working
        }
    }
}

/// The first permission row, which is idle while Plug is off.
private struct BackgroundItemRow: View {
    let model: AppModel
    let allowance: SystemAllowance

    var body: some View {
        PermissionRow(
            title: "Run in the Background",
            detail: "Keeps Plug serving when its window is closed.",
            allowance: model.serviceEnabled ? allowance : nil,
            idle: "Not needed while Plug is off",
            fix: "Open Login Items…"
        ) { DaemonServiceManager.shared.openLoginItemSettings() }
    }
}

private struct ReloadButton: View {
    let model: AppModel
    let run: (PlugIntent) -> Void

    var body: some View {
        Button("Reload") { run(.reloadConfiguration) }
            .help("Reload after you edit the settings file by hand")
            .disabled(!model.canMutate)
    }
}

private struct VersionText: View {
    let model: AppModel

    var body: some View {
        Text(model.displayVersion)
            .textSelection(.enabled)
    }
}

/// One thing Plug needs from macOS: what it is for, where it stands, and the
/// way to System Settings when the person has turned it off there.
private struct PermissionRow: View {
    let title: String
    let detail: String
    /// Nil while Plug is not using it.
    let allowance: SystemAllowance?
    let idle: String
    let fix: String
    let open: () -> Void

    var body: some View {
        LabeledContent {
            HStack(spacing: Metric.tight) {
                switch allowance {
                case .allowed:
                    PlugIcon(.worked)
                        .foregroundStyle(StatusColor.working)
                        .accessibilityHidden(true)
                    Text("Allowed")
                case .turnedOff:
                    PlugIcon(.needsYou)
                        .foregroundStyle(StatusColor.needsYou)
                        .accessibilityHidden(true)
                    Text("Turned off")
                    Button(fix, action: open)
                case .notAsked:
                    Text("Not asked yet").foregroundStyle(.secondary)
                case nil:
                    Text(idle).foregroundStyle(.secondary)
                }
            }
        } label: {
            Text(title)
            Text(detail)
        }
    }
}

/// One checked thing: a glyph for the outcome, a plain title, the detail,
/// and what to do when it did not pass.
private struct CheckRow: View {
    let check: Check

    var body: some View {
        Label {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(check.title)
                Text(check.message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let fix {
                    // The fix can carry a command in backticks.
                    Text((try? AttributedString(markdown: fix)) ?? AttributedString(fix))
                        .font(.callout)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
            }
        } icon: {
            PlugIcon(icon)
                .foregroundStyle(color)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(check.title), \(spokenResult). \(check.message)\(fix.map { " \($0)" } ?? "")")
    }

    /// What to do about it, for a check that did not pass.
    private var fix: String? {
        check.result == .pass ? nil : check.fix
    }

    private var icon: PlugIcon.Kind {
        switch check.result {
        case .pass: .worked
        case .warn: .needsYou
        case .fail: .stopped
        }
    }

    private var color: Color {
        switch check.result {
        case .pass: StatusColor.working
        case .warn: StatusColor.needsYou
        case .fail: StatusColor.stopped
        }
    }

    private var spokenResult: String {
        switch check.result {
        case .pass: "passed"
        case .warn: "warning"
        case .fail: "problem"
        }
    }
}
