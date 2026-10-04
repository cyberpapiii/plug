import PlugIPC
import SwiftUI

/// Changing a server that already exists.
///
/// The same form Add Server ends on, filled in from the daemon's complete
/// definition, so the one thing that is wrong can be changed without retyping
/// the rest.
struct EditServerView: View {
    let model: AppModel
    let name: String
    @Environment(\.dismiss) private var dismiss

    @State private var form: ServerForm?
    @State private var saving = false
    @State private var failure: String?

    var body: some View {
        SheetFrame(
            title: "Edit \(name)",
            subtitle: model.canReadServerConfig
                ? "Changes take effect as soon as you save."
                : AppModel.serverConfigReadRequiredCopy,
            failure: form == nil ? nil : failure,
            busy: saving,
            confirmTitle: saving ? "Saving…" : "Save",
            confirmDisabled: !Self.canSave(
                canReadServerConfig: model.canReadServerConfig,
                loaded: form != nil,
                isComplete: form?.isComplete ?? false,
                saving: saving
            ),
            confirm: save
        ) {
            if let form {
                ServerFormFields(form: Binding(
                    get: { self.form ?? form },
                    set: { self.form = $0 }
                ))
            } else if let failure {
                // The settings could not be read, so there is nothing to
                // edit. Say why and offer the one thing that can help.
                VStack(alignment: .leading, spacing: Metric.snug) {
                    Label("Plug could not read this server's settings.", systemImage: "exclamationmark.triangle")
                        .font(.callout)
                    Text(failure)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                    if model.canReadServerConfig {
                        Button("Try Again") { Task { await load() } }
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 160, alignment: .leading)
            } else {
                HStack(spacing: Metric.snug) {
                    ProgressView().controlSize(.small)
                    Text("Loading server settings…").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 160, alignment: .center)
            }
        }
        .task { await load() }
    }

    nonisolated static func canSave(
        canReadServerConfig: Bool,
        loaded: Bool,
        isComplete: Bool,
        saving: Bool
    ) -> Bool {
        canReadServerConfig && loaded && isComplete && !saving
    }

    /// Load the daemon's complete definition once. Saving starts from this
    /// value, so settings the form does not show stay intact.
    private func load() async {
        guard form == nil else { return }
        guard model.canReadServerConfig else {
            failure = AppModel.serverConfigReadRequiredCopy
            return
        }
        failure = nil
        do {
            form = ServerForm(config: try await model.serverConfig(name: name))
        } catch {
            failure = error.localizedDescription
        }
    }

    private func save() {
        guard model.canReadServerConfig else {
            failure = AppModel.serverConfigReadRequiredCopy
            return
        }
        guard let saved = form?.config else { return }
        saving = true
        failure = nil
        Task {
            do {
                try await model.performOperation {
                    .validateServer(authToken: $0, name: name, server: saved)
                }
                try await model.performOperation {
                    .updateServer(authToken: $0, name: name, server: saved)
                }
                saving = false
                dismiss()
            } catch {
                failure = error.localizedDescription
                saving = false
            }
        }
    }
}
