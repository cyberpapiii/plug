import SwiftUI

struct SettingsView: View {
    let model: AppModel
    @AppStorage("launchAtLogin") private var launchAtLogin = true
    @State private var loginItemFailed = false

    var body: some View {
        Form {
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
                    Text("macOS wants to confirm this in System Settings.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } footer: {
                Text("Your servers keep running when this window is closed, and when Plug's icon isn't in the menu bar.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("About") {
                LabeledContent("Version", value: model.situation.version.isEmpty ? "—" : model.situation.version)
                LabeledContent("Servers", value: "\(model.situation.activeServers.count) on")
                HStack {
                    Button("Check for Updates…") { UpdateService.shared.checkForUpdates() }
                        .disabled(!UpdateService.shared.canCheckForUpdates)
                    Spacer()
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .fixedSize(horizontal: false, vertical: true)
    }
}
