import AppKit
import SwiftUI
import UserNotifications

/// Everything a person owns about Plug, in the Settings window: is Plug on
/// and healthy, how should it behave, where are its files, what version is
/// this. One page, so nothing is behind a tab.
struct SettingsView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void
    var checkups: any CheckupRunning = CheckupService()

    // The login item lives in macOS, not in this app's defaults, so the toggle
    // starts from what SMAppService reports and the task below re-reads it.
    @State private var launchAtLogin = DaemonServiceManager.shared.mainAppAtLoginEnabled
    @AppStorage(NotificationService.preferenceKey) private var notify = false
    @State private var loginItemFailed = false
    @State private var notificationsDenied = false
    @State private var automaticUpdates = UpdateService.shared.checksAutomatically
    @State private var keepsInMenuBar = MenuBarPresence.standard.keeps
    @State private var backgroundItem = DaemonServiceManager.shared.backgroundItemAllowance
    @State private var loginItem = DaemonServiceManager.shared.loginItemAllowance
    @State private var notifications = SystemAllowance.notAsked

    @State private var checkup: Checkup?
    @State private var checking = false
    @State private var checkupError: String?
    @State private var showsPassingChecks = false
    @State private var configURL: URL?

    var body: some View {
        Form {
            general
            plug
            permissions
            files
            about
        }
        .formStyle(.grouped)
        .frame(width: Metric.settingsWidth)
        .frame(height: Metric.settingsHeight)
        .task {
            readLoginItem()
            await readPermissions()
            configURL = await checkups.configPath()
        }
        // Allowing the login item happens in System Settings, so look again
        // when the person comes back.
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            readLoginItem()
            Task { await readPermissions() }
        }
        .task(id: model.serviceEnabled) { await readPermissions() }
        // A problem elsewhere in the app can send a person here with the
        // checkup already asked for.
        .task(id: router.checkupRequests) {
            if router.checkupRequests > 0 { await runCheckup() }
        }
    }

    // MARK: General

    private var general: some View {
        Section("General") {
            Toggle("Open at Login", isOn: Binding(
                get: { launchAtLogin },
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
                    await readPermissions()
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
            ServicePowerToggle(model: model, run: run)
            if model.serviceEnabled {
                LabeledContent("Status") {
                    HStack(spacing: Metric.tight) {
                        if serviceIsSettling {
                            ProgressView().controlSize(.small)
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
            checkupRows
        } header: {
            Text("Plug")
        } footer: {
            Text("Plug keeps serving your clients after you close the window or quit. To stop it, turn Plug off.")
        }
    }

    // MARK: Permissions

    /// What Plug itself needs from macOS, and whether it has it. A row with
    /// nothing to fix says so and offers nothing.
    private var permissions: some View {
        Section {
            PermissionRow(
                title: "Run in the Background",
                detail: "Keeps Plug serving when its window is closed.",
                allowance: model.serviceEnabled ? backgroundItem : nil,
                idle: "Not needed while Plug is off",
                fix: "Open Login Items…"
            ) { DaemonServiceManager.shared.openLoginItemSettings() }
            PermissionRow(
                title: "Open at Login",
                detail: "Puts Plug in the menu bar when you log in.",
                allowance: loginItem == .turnedOff || launchAtLogin ? loginItem : nil,
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
            Text("Permissions")
        } footer: {
            Text("Plug keeps sign-ins and keys in your Keychain; when macOS asks, choose Always Allow. A server that reads your Mac's data, such as messages or contacts, has permissions of its own under Privacy & Security, which Plug cannot see or grant.")
        }
    }

    // MARK: Checkup

    @ViewBuilder private var checkupRows: some View {
        LabeledContent {
            HStack(spacing: Metric.tight) {
                if checking { ProgressView().controlSize(.small) }
                Button("Run Checkup") { Task { await runCheckup() } }
                    .disabled(checking)
            }
        } label: {
            if let checkup {
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
            ProblemNote(title: "The checkup could not run", reason: checkupError)
        }

        if let checkup {
            ForEach(problemChecks(in: checkup)) { check in
                CheckRow(check: check)
            }
            if !passingChecks(in: checkup).isEmpty {
                DisclosureGroup(
                    passingChecksTitle(in: checkup),
                    isExpanded: $showsPassingChecks
                ) {
                    ForEach(passingChecks(in: checkup)) { check in
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
                    Button("Reload") { run(.reloadConfiguration) }
                        .help("Reload after you edit the settings file by hand")
                        .disabled(!model.canMutate)
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
            Text("Files")
        } footer: {
            Text("Plug saves your servers and clients here. Reload after editing the file by hand.")
        }
    }

    // MARK: About

    private var about: some View {
        Section("About") {
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
                    Text(model.displayVersion)
                        .textSelection(.enabled)
                    Button("Check for Updates…") { run(.checkForUpdates) }
                        .disabled(!UpdateService.shared.canCheckForUpdates)
                }
            }
        }
    }

    private func readPermissions() async {
        backgroundItem = DaemonServiceManager.shared.backgroundItemAllowance
        loginItem = DaemonServiceManager.shared.loginItemAllowance
        // The settings object cannot cross to the main actor; only what it
        // says does.
        notifications = await withCheckedContinuation { done in
            UNUserNotificationCenter.current().getNotificationSettings {
                done.resume(returning: SystemAllowance($0.authorizationStatus))
            }
        }
    }

    // MARK: Login item

    private func readLoginItem() {
        launchAtLogin = DaemonServiceManager.shared.mainAppAtLoginEnabled
        if launchAtLogin { loginItemFailed = false }
    }

    /// The switch only moves when macOS agrees. When it does not, the row
    /// under it says where to allow it; System Settings is not opened for the
    /// person.
    private func setLoginItem(_ enabled: Bool) {
        do {
            try DaemonServiceManager.shared.setMainAppAtLogin(enabled)
            launchAtLogin = enabled
            loginItemFailed = false
        } catch {
            loginItemFailed = true
        }
    }

    // MARK: Status

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
            checkup = result
        } catch {
            checkup = nil
            checkupError = error.localizedDescription
        }
        checking = false
    }

    private func problemChecks(in checkup: Checkup) -> [Check] {
        checkup.ordered.filter { $0.result != .pass }
    }

    private func passingChecks(in checkup: Checkup) -> [Check] {
        checkup.ordered.filter { $0.result == .pass }
    }

    private func passingChecksTitle(in checkup: Checkup) -> String {
        checkup.isClean ? "Details" : "\(passingChecks(in: checkup).count) passed"
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
