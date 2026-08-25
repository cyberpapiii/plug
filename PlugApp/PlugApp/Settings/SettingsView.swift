import SwiftUI

struct SettingsView: View {
    let model: AppModel
    @AppStorage("launchAtLogin") private var launchAtLogin = true
    var body: some View {
        Form {
            Toggle("Open Plug at login", isOn: $launchAtLogin)
            LabeledContent("Daemon", value: model.snapshot.ownership.replacingOccurrences(of: "_", with: " ").capitalized)
            LabeledContent("Version", value: model.snapshot.runtimeVersion.isEmpty ? "—" : model.snapshot.runtimeVersion)
            Text("Plug keeps running when this window closes.").font(.caption).foregroundStyle(.secondary)
        }.formStyle(.grouped).frame(width: 440, height: 220).padding()
    }
}
