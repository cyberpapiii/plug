import PlugIPC
import SwiftUI

/// Changing a server that already exists.
///
/// Adding is a paste; editing is not — the settings are already correct except
/// for the one thing that is wrong, and retyping a command to change a token is
/// how mistakes get made. So this shows the real fields, prefilled, and sends
/// only what the daemon needs to keep the rest intact.
struct EditServerView: View {
    let model: AppModel
    let name: String
    @Environment(\.dismiss) private var dismiss

    @State private var isRemote = false
    @State private var command = ""
    @State private var arguments = ""
    @State private var url = ""
    @State private var authToken = ""
    @State private var environment = ""
    @State private var saving = false
    @State private var failure: String?
    @State private var loaded = false

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Edit \(name)").font(.title2.weight(.semibold))
                Text("Changes take effect as soon as you save.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Picker("Kind", selection: $isRemote) {
                Text("Runs on this Mac").tag(false)
                Text("Remote server").tag(true)
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            Form {
                if isRemote {
                    TextField("Address", text: $url, prompt: Text("https://example.com/mcp"))
                    TextField("Access token", text: $authToken, prompt: Text("Optional"))
                } else {
                    TextField("Command", text: $command, prompt: Text("npx"))
                    TextField("Arguments", text: $arguments, prompt: Text("-y linear-mcp"))
                }
                TextField(
                    "Environment",
                    text: $environment,
                    prompt: Text("KEY=value, one per line"),
                    axis: .vertical
                )
                .lineLimit(2...5)
            }
            .formStyle(.grouped)
            .frame(height: 168)

            HStack(spacing: Metric.snug) {
                if let failure {
                    Label(failure, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(saving ? "Saving…" : "Save") { save() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving || !isComplete)
            }
        }
        .padding(Metric.roomy)
        .frame(width: 520)
        .task { load() }
    }

    private var isComplete: Bool {
        isRemote
            ? !url.trimmingCharacters(in: .whitespaces).isEmpty
            : !command.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// Prefill from what the snapshot already knows. The daemon does not hand
    /// back the full server definition, so anything it does not report is left
    /// blank and only sent when it is filled in.
    private func load() {
        guard !loaded else { return }
        loaded = true
        guard let server = model.snapshot.configuredServers.first(where: { $0.name == name })
        else { return }
        isRemote = server.transport.lowercased() != "stdio"
    }

    private func save() {
        saving = true
        failure = nil
        var config = isRemote
            ? ServerConfig.remote(url.trimmingCharacters(in: .whitespaces))
            : ServerConfig.command("", args: [])
        if isRemote {
            let token = authToken.trimmingCharacters(in: .whitespaces)
            config.authToken = token.isEmpty ? nil : token
        } else {
            config.command = command.trimmingCharacters(in: .whitespaces)
            config.args = ServerDraftParser.tokenize(arguments)
        }
        config.env = Self.parseEnvironment(environment)
        let saved = config

        Task {
            await model.perform { .validateServer(authToken: $0, name: name, server: saved) }
            if let error = model.lastError {
                failure = error
                saving = false
                return
            }
            await model.updateServer(name: name, config: saved)
            saving = false
            if let error = model.lastError {
                failure = error
            } else {
                dismiss()
            }
        }
    }

    static func parseEnvironment(_ text: String) -> [String: String] {
        var result: [String: String] = [:]
        for line in text.split(whereSeparator: { $0 == "\n" || $0 == "," }) {
            let entry = line.trimmingCharacters(in: .whitespaces)
            guard let split = entry.firstIndex(of: "=") else { continue }
            let key = String(entry[entry.startIndex..<split]).trimmingCharacters(in: .whitespaces)
            let value = String(entry[entry.index(after: split)...])
                .trimmingCharacters(in: .whitespaces)
            guard !key.isEmpty else { continue }
            result[key] = value
        }
        return result
    }
}
