import AppKit
import SwiftUI

/// Everything a person owns about Plug, as one page of the main window.
///
/// Settings used to be its own window with three tabs, which put the switch
/// that turns Plug off, Restart, and Checkup one window away from the place a
/// problem is shown. It is now the fifth section, one page that scrolls: is
/// Plug on and healthy, how should it behave, where are its files, what
/// version is this.
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
        .frame(maxWidth: Metric.settingsMaxWidth)
        .frame(maxWidth: .infinity)
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
            LabeledContent {
                Text(serviceStatus)
                    .foregroundStyle(model.connectionState == .ready ? .primary : .secondary)
            } label: {
                Label("Background service", systemImage: serviceSymbol)
                    .foregroundStyle(serviceColor)
            }
            HStack {
                Button {
                    run(.restartService)
                } label: {
                    Label("Restart Plug", systemImage: "arrow.clockwise")
                }
                .disabled(model.isRestartingService || !model.serviceEnabled || model.isChangingService)
                if model.isRestartingService { ProgressView().controlSize(.small) }
                Spacer()
            }
        } header: {
            Text("Plug")
        } footer: {
            Text("Restarting reconnects every server. Connected clients pick Plug back up on their own.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: Checkup

    private var checkupSection: some View {
        Section {
            HStack {
                Button {
                    Task { await runCheckup() }
                } label: {
                    Label("Check Everything", systemImage: "stethoscope")
                }
                .disabled(checking)
                if checking { ProgressView().controlSize(.small) }
                Spacer()
                if let checkup {
                    Label(
                        checkup.headline,
                        systemImage: checkup.isClean
                            ? "checkmark.circle.fill"
                            : "exclamationmark.triangle.fill"
                    )
                    .font(.callout)
                    .foregroundStyle(checkup.isClean ? Color.green : Color.orange)
                }
            }

            if let checkupError {
                Label(checkupError, systemImage: "xmark.circle.fill")
                    .font(.caption)
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
                                .padding(.top, Metric.tight)
                        }
                    }
                }
            }
        } header: {
            Text("Checkup")
        }
    }

    // MARK: Behavior

    private var behavior: some View {
        Section {
            Toggle(isOn: $launchAtLogin) {
                Label("Show Plug in the menu bar at login", systemImage: "power")
            }
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
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Toggle(isOn: $notify) {
                Label("Tell me when a server needs sign-in or a new client connects", systemImage: "bell")
            }
            .onChange(of: notify) { _, enabled in
                if enabled { NotificationService.shared.requestAuthorization() }
            }
            Toggle(isOn: $automaticUpdates) {
                Label("Check for updates automatically", systemImage: "arrow.down.circle")
            }
            .onChange(of: automaticUpdates) { _, enabled in
                UpdateService.shared.checksAutomatically = enabled
            }
        } header: {
            Text("General")
        } footer: {
            Text("Plug and its servers keep running after you close this window or quit the menu bar icon.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: Files

    private var files: some View {
        Section {
            HStack {
                Button {
                    Task {
                        if let path = await checkups.configPath() {
                            NSWorkspace.shared.activateFileViewerSelecting([path])
                        }
                    }
                } label: {
                    Label("Show Settings File", systemImage: "doc.text")
                }
                Button {
                    run(.openLogs)
                } label: {
                    Label("Show Logs", systemImage: "list.bullet.rectangle")
                }
                Button {
                    run(.reloadConfiguration)
                } label: {
                    Label("Read Settings File Again", systemImage: "arrow.triangle.2.circlepath")
                }
                .help("Use this after you change the settings file by hand")
                .disabled(!model.canMutate)
                Spacer()
            }
        } header: {
            Text("Files")
        } footer: {
            Text("Plug keeps every server and client choice in one settings file. Changes made in this window are saved there for you.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: About

    private var about: some View {
        Section {
            LabeledContent("Version") {
                Text(model.displayVersion)
                    .monospacedDigit()
                    .textSelection(.enabled)
            }
            HStack {
                Button {
                    run(.checkForUpdates)
                } label: {
                    Label("Check for Updates…", systemImage: "arrow.down.circle")
                }
                .disabled(!UpdateService.shared.canCheckForUpdates)
                Spacer()
            }
        } header: {
            Text("About")
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
