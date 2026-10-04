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
    @State private var automaticUpdates = UpdateService.shared.checksAutomatically

    @State private var checkup: Checkup?
    @State private var checking = false
    @State private var checkupError: String?
    @State private var showsPassingChecks = false

    var body: some View {
        Form {
            plug
            checkupSection
            behavior
            files
            about
        }
        .formStyle(.grouped)
        .frame(width: Metric.settingsWidth)
        .frame(height: 620)
        .task {
            launchAtLogin = DaemonServiceManager.shared.mainAppAtLoginEnabled
        }
        // A problem elsewhere in the app can send a person here with the
        // checkup already asked for.
        .task(id: router.checkupRequests) {
            if router.checkupRequests > 0 { await runCheckup() }
        }
    }

    // MARK: Plug

    private var plug: some View {
        Section {
            ServicePowerToggle(model: model, run: run)
            LabeledContent("Background service") {
                HStack(spacing: Metric.tight) {
                    Image(systemName: serviceSymbol)
                        .foregroundStyle(serviceColor)
                        .accessibilityHidden(true)
                    Text(serviceStatus)
                    if model.isRestartingService { ProgressView().controlSize(.small) }
                    Button("Restart") { run(.restartService) }
                        .disabled(model.isRestartingService || !model.serviceEnabled || model.isChangingService)
                }
            }
        } footer: {
            Text("Restarting reconnects every server. Connected clients pick Plug back up on their own.")
        }
    }

    // MARK: Checkup

    private var checkupSection: some View {
        Section("Checkup") {
            LabeledContent {
                HStack(spacing: Metric.tight) {
                    if checking { ProgressView().controlSize(.small) }
                    Button("Check Everything") { Task { await runCheckup() } }
                        .disabled(checking)
                }
            } label: {
                if let checkup {
                    Label(
                        checkup.headline,
                        systemImage: checkup.isClean
                            ? "checkmark.circle.fill"
                            : "exclamationmark.triangle.fill"
                    )
                    .foregroundStyle(checkup.isClean ? Color.green : Color.orange)
                } else {
                    Text("Look for anything wrong with Plug")
                }
            }

            if let checkupError {
                Label(checkupError, systemImage: "xmark.circle.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
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
    }

    // MARK: Behavior

    private var behavior: some View {
        Section {
            Toggle("Open Plug at login", isOn: $launchAtLogin)
                .onChange(of: launchAtLogin) { _, enabled in
                    do {
                        try DaemonServiceManager.shared.setMainAppAtLogin(enabled)
                        loginItemFailed = false
                    } catch {
                        loginItemFailed = true
                        DaemonServiceManager.shared.openLoginItemSettings()
                    }
                }
            if loginItemFailed {
                Label(
                    "macOS wants to confirm this in System Settings.",
                    systemImage: "exclamationmark.triangle"
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }
            Toggle(isOn: $notify) {
                Text("Notifications")
                Text("When a server needs sign-in or a new client connects.")
            }
            .onChange(of: notify) { _, enabled in
                if enabled { NotificationService.shared.requestAuthorization() }
            }
        } header: {
            Text("General")
        } footer: {
            Text("Plug and its servers keep running after you close its windows or quit the menu bar icon.")
        }
    }

    // MARK: Files

    private var files: some View {
        Section {
            LabeledContent("Settings file") {
                HStack(spacing: Metric.tight) {
                    Button("Read Again") { run(.reloadConfiguration) }
                        .help("Use this after you change the settings file by hand")
                        .disabled(!model.canMutate)
                    Button("Show in Finder") {
                        Task {
                            if let path = await checkups.configPath() {
                                NSWorkspace.shared.activateFileViewerSelecting([path])
                            }
                        }
                    }
                }
            }
            LabeledContent("Logs") {
                Button("Show in Finder") { run(.openLogs) }
            }
        } header: {
            Text("Files")
        } footer: {
            Text("Plug keeps every server and client choice in one settings file. Changes made in the app are saved there for you.")
        }
    }

    // MARK: About

    private var about: some View {
        Section("Updates") {
            Toggle("Check for updates automatically", isOn: $automaticUpdates)
                .onChange(of: automaticUpdates) { _, enabled in
                    UpdateService.shared.checksAutomatically = enabled
                }
            LabeledContent("Version") {
                HStack(spacing: Metric.tight) {
                    Text(model.displayVersion)
                        .monospacedDigit()
                        .textSelection(.enabled)
                    Button("Check Now…") { run(.checkForUpdates) }
                        .disabled(!UpdateService.shared.canCheckForUpdates)
                }
            }
        }
    }

    private var serviceStatus: String {
        if !model.serviceEnabled { return "Off" }
        if model.isRestartingService { return "Restarting" }
        switch model.connectionState {
        case .ready: return "Running"
        case .connecting: return "Connecting"
        case .reconnecting: return "Reconnecting"
        case .incompatible: return "Restart required to finish update"
        case .disconnected: return "Not running"
        }
    }

    private var serviceSymbol: String {
        if !model.serviceEnabled { return "bolt.slash" }
        if model.isRestartingService { return "circle.dotted" }
        switch model.connectionState {
        case .ready: return "checkmark.circle.fill"
        case .connecting, .reconnecting: return "circle.dotted"
        case .incompatible: return "arrow.triangle.2.circlepath"
        case .disconnected: return "xmark.circle.fill"
        }
    }

    private var serviceColor: Color {
        if !model.serviceEnabled { return .secondary }
        if model.isRestartingService { return .secondary }
        switch model.connectionState {
        case .ready: return .green
        case .connecting, .reconnecting: return .secondary
        case .incompatible: return .orange
        case .disconnected: return .red
        }
    }

    private func runCheckup() async {
        checking = true
        checkupError = nil
        showsPassingChecks = false
        do {
            checkup = try await checkups.run()
        } catch {
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
        let count = passingChecks(in: checkup).count
        return checkup.isClean
            ? "Show \(count) checked \(count == 1 ? "item" : "items")"
            : "Show \(count) passed \(count == 1 ? "check" : "checks")"
    }
}

/// One checked thing: a glyph for the outcome, a plain title, the detail.
private struct CheckRow: View {
    let check: Check

    var body: some View {
        HStack(alignment: .top, spacing: Metric.snug) {
            Image(systemName: symbol)
                .foregroundStyle(color)
                .symbolRenderingMode(.hierarchical)
                .frame(width: 16)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(check.title).font(.callout)
                Text(check.message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let fix = check.fix, check.result != .pass {
                    Label(fix, systemImage: "wrench.adjustable")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(check.title), \(spokenResult). \(check.message)")
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
