import AppKit
import SwiftUI

/// Everything a person owns about Plug, in one window with three tabs.
///
/// The old settings window held one switch and a version number, so the
/// ordinary things a Mac app is expected to do — check for updates, restart
/// the background service, look at the log, find out whether anything is wrong
/// — had no home. Each tab answers one question: how should Plug behave, is it
/// healthy, and what is this.
struct SettingsView: View {
    let model: AppModel
    let run: (PlugIntent) -> Void

    var body: some View {
        Group {
            if #available(macOS 15.0, *) {
                TabView {
                    Tab("General", systemImage: "gearshape") { GeneralSettings() }
                    Tab("Service", systemImage: "bolt.horizontal.circle") {
                        ServiceSettings(model: model, run: run)
                    }
                    Tab("About", systemImage: "info.circle") {
                        AboutSettings(model: model, run: run)
                    }
                }
            } else {
                legacyTabs
            }
        }
        .frame(width: 520, height: 460)
    }

    private var legacyTabs: some View {
        TabView {
            GeneralSettings()
                .tabItem { Label("General", systemImage: "gearshape") }
            ServiceSettings(model: model, run: run)
                .tabItem { Label("Service", systemImage: "bolt.horizontal.circle") }
            AboutSettings(model: model, run: run)
                .tabItem { Label("About", systemImage: "info.circle") }
        }
    }
}

// MARK: - General

private struct GeneralSettings: View {
    @AppStorage("launchAtLogin") private var launchAtLogin = true
    @AppStorage(NotificationService.preferenceKey) private var notify = false
    @State private var loginItemFailed = false
    @State private var automaticUpdates = UpdateService.shared.checksAutomatically

    var body: some View {
        Form {
            Section {
                Toggle(isOn: $launchAtLogin) {
                    Label("Open Plug at login", systemImage: "power")
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
                    Label("Tell me when a server needs attention", systemImage: "bell")
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
            } footer: {
                Label(
                    "Your servers keep running when this window is closed, and when Plug's icon isn't in the menu bar.",
                    systemImage: "info.circle"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .task {
            launchAtLogin = DaemonServiceManager.shared.mainAppAtLoginEnabled
        }
    }
}

// MARK: - Service

private struct ServiceSettings: View {
    let model: AppModel
    let run: (PlugIntent) -> Void
    var checkups: any CheckupRunning = CheckupService()

    @State private var checkup: Checkup?
    @State private var checking = false
    @State private var checkupError: String?
    var body: some View {
        Form {
            Section {
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
                        Label("Restart", systemImage: "arrow.clockwise")
                    }
                    .disabled(model.isRestartingService)

                    Button {
                        run(.reloadConfiguration)
                    } label: {
                        Label("Reload settings", systemImage: "arrow.triangle.2.circlepath")
                    }

                    if model.isRestartingService { ProgressView().controlSize(.small) }
                    Spacer()
                }
            } footer: {
                Text("Restarting reconnects every server. Connected apps pick Plug back up on their own.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("Checkup") {
                HStack {
                    Button {
                        Task { await runCheckup() }
                    } label: {
                        Label("Check everything", systemImage: "stethoscope")
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
                    ScrollView {
                        VStack(alignment: .leading, spacing: Metric.tight) {
                            ForEach(checkup.ordered) { check in
                                CheckRow(check: check)
                            }
                        }
                    }
                    .frame(height: 168)
                }
            }

            Section("Files") {
                HStack {
                    Button {
                        Task {
                            if let path = await checkups.configPath() {
                                NSWorkspace.shared.activateFileViewerSelecting([path])
                            }
                        }
                    } label: {
                        Label("Show settings file", systemImage: "doc.text")
                    }
                    Button {
                        run(.openLogs)
                    } label: {
                        Label("Show logs", systemImage: "list.bullet.rectangle")
                    }
                    Spacer()
                }
            }
        }
        .formStyle(.grouped)
    }

    private var serviceStatus: String {
        switch model.connectionState {
        case .ready: "Running"
        case .connecting: "Connecting"
        case .incompatible: "Update required"
        case .disconnected: "Not running"
        }
    }

    private var serviceSymbol: String {
        switch model.connectionState {
        case .ready: "checkmark.circle.fill"
        case .connecting: "circle.dotted"
        case .incompatible: "arrow.triangle.2.circlepath"
        case .disconnected: "xmark.circle.fill"
        }
    }

    private var serviceColor: Color {
        switch model.connectionState {
        case .ready: .green
        case .connecting: .secondary
        case .incompatible: .orange
        case .disconnected: .red
        }
    }

    private func runCheckup() async {
        checking = true
        checkupError = nil
        do {
            checkup = try await checkups.run()
        } catch {
            checkupError = error.localizedDescription
        }
        checking = false
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

// MARK: - About

private struct AboutSettings: View {
    let model: AppModel
    let run: (PlugIntent) -> Void

    var body: some View {
        Form {
            Section {
                LabeledContent {
                    Text(model.situation.version.isEmpty ? "—" : model.situation.version)
                } label: {
                    Label("Version", systemImage: "number")
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
            } footer: {
                Text("Plug keeps your MCP servers available to every connected AI app.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}
