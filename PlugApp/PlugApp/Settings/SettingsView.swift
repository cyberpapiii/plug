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
        .frame(width: 540, height: 420)
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
                    Label("Show Plug in the menu bar at login", systemImage: "power")
                }
                .listRowSeparator(.hidden)
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
                    .listRowSeparator(.hidden)
                }
                Toggle(isOn: $notify) {
                    Label("Tell me when a server needs attention", systemImage: "bell")
                }
                .listRowSeparator(.hidden)
                .onChange(of: notify) { _, enabled in
                    if enabled { NotificationService.shared.requestAuthorization() }
                }
                Toggle(isOn: $automaticUpdates) {
                    Label("Check for updates automatically", systemImage: "arrow.down.circle")
                }
                .listRowSeparator(.hidden)
                .onChange(of: automaticUpdates) { _, enabled in
                    UpdateService.shared.checksAutomatically = enabled
                }
            } footer: {
                Label(
                    "Plug and its servers keep running after you close this window or hide the menu bar icon.",
                    systemImage: "info.circle"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            .listSectionSeparator(.hidden)
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
    @State private var showsPassingChecks = false
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
                .listRowSeparator(.hidden)

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
                        Label("Reload Configuration", systemImage: "arrow.triangle.2.circlepath")
                    }
                    .help("Reload server settings without restarting Plug")

                    if model.isRestartingService { ProgressView().controlSize(.small) }
                    Spacer()
                }
                .listRowSeparator(.hidden)
            } footer: {
                Text("Restarting reconnects every server. Connected apps pick Plug back up on their own.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .listSectionSeparator(.hidden)

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
                .listRowSeparator(.hidden)

                if let checkupError {
                    Label(checkupError, systemImage: "xmark.circle.fill")
                        .font(.caption)
                        .foregroundStyle(.red)
                        .listRowSeparator(.hidden)
                }

                if let checkup {
                    ForEach(problemChecks(in: checkup)) { check in
                        CheckRow(check: check)
                            .listRowSeparator(.hidden)
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
                        .listRowSeparator(.hidden)
                    }
                }
            }
            .listSectionSeparator(.hidden)

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
                .listRowSeparator(.hidden)
            }
            .listSectionSeparator(.hidden)
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

// MARK: - About

private struct AboutSettings: View {
    let model: AppModel
    let run: (PlugIntent) -> Void

    var body: some View {
        VStack(spacing: Metric.regular) {
            VStack(spacing: Metric.tight) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 64, height: 64)
                    .accessibilityHidden(true)
                Text("Plug")
                    .font(.title2.weight(.semibold))
                Text(versionText)
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            .accessibilityElement(children: .combine)

            Button {
                run(.checkForUpdates)
            } label: {
                Label("Check for Updates…", systemImage: "arrow.down.circle")
            }
            .disabled(!UpdateService.shared.canCheckForUpdates)

            Text("Plug keeps your MCP servers available to every connected AI app.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 320)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Metric.roomy)
    }

    private var versionText: String {
        let version = model.situation.version
        return version.isEmpty ? "Version unavailable" : "Version \(version)"
    }
}
