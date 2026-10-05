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
            subtitle: form != nil ? "Changes take effect as soon as you save." : nil,
            failure: form == nil ? nil : failure,
            busy: saving,
            // With no settings to edit there is nothing to save, only Done.
            confirmTitle: form == nil ? nil : "Save",
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
                // edit. Say why and offer the one thing that can help. When
                // Plug has to restart first, the reason is already the advice.
                ProblemNote(
                    title: "Plug could not read this server's settings.",
                    reason: failure,
                    advice: model.canReadServerConfig ? Explain.advice(forReason: failure) : nil,
                    actionTitle: model.canReadServerConfig ? "Try Again" : nil,
                    action: { Task { await load() } }
                )
                .padding(.horizontal, Metric.roomy)
            } else {
                SheetLoading(label: "Loading server settings")
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
        guard let form else { return }
        let saved = form.config
        let store = form.keyStore.sent
        saving = true
        failure = nil
        Task {
            do {
                try await model.performOperation {
                    .validateServer(authToken: $0, name: name, server: saved)
                }
                try await model.performOperation {
                    .updateServer(authToken: $0, name: name, server: saved, secretStore: store)
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
