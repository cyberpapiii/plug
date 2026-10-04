import AppKit
import SwiftUI

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

    @State private var checkup: Checkup?
    @State private var checking = false
    @State private var checkupError: String?
    @State private var showsPassingChecks = false
    @State private var configURL: URL?

    var body: some View {
        Form {
            general
            plug
            files
            about
        }
        .formStyle(.grouped)
        .frame(width: Metric.settingsWidth)
        .frame(height: Metric.settingsHeight)
        .task {
            readLoginItem()
            configURL = await checkups.configPath()
        }
        // Allowing the login item happens in System Settings, so look again
        // when the person comes back.
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            readLoginItem()
        }
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
                            Image(systemName: serviceSymbol)
                                .foregroundStyle(serviceColor)
                                .accessibilityHidden(true)
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
                    Image(systemName: headlineSymbol(for: checkup))
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

    private var serviceSymbol: String {
        switch model.connectionState {
        case .incompatible: "exclamationmark.triangle.fill"
        case .disconnected: "xmark.circle.fill"
        case .ready, .connecting, .reconnecting: "checkmark.circle.fill"
        }
    }

    private var serviceColor: Color {
        switch model.connectionState {
        case .incompatible: .orange
        case .disconnected: .red
        case .ready, .connecting, .reconnecting: .green
        }
    }

    // MARK: Checkup results

    private func headlineSymbol(for checkup: Checkup) -> String {
        if !checkup.problems.isEmpty { return "xmark.circle.fill" }
        return checkup.isClean ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
    }

    private func headlineColor(for checkup: Checkup) -> Color {
        if !checkup.problems.isEmpty { return .red }
        return checkup.isClean ? .green : .orange
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
            Image(systemName: symbol)
                .foregroundStyle(color)
                .symbolRenderingMode(.hierarchical)
                .accessibilityHidden(true)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(check.title), \(spokenResult). \(check.message)\(fix.map { " \($0)" } ?? "")")
    }

    /// What to do about it, for a check that did not pass.
    private var fix: String? {
        check.result == .pass ? nil : check.fix
    }

    private var symbol: String {
        switch check.result {
        case .pass: "checkmark.circle.fill"
        case .warn: "exclamationmark.triangle.fill"
        case .fail: "xmark.circle.fill"
        }
    }

    private var color: Color {
        switch check.result {
        case .pass: .green
        case .warn: .orange
        case .fail: .red
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
