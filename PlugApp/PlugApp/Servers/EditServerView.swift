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
    @State private var loadedConfig: ServerConfig?

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Edit \(name)").font(.title2.weight(.semibold))
                Text("Changes take effect as soon as you save.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Picker("Kind", selection: $isRemote) {
                Label("Runs on this Mac", systemImage: "desktopcomputer").tag(false)
                Label("Remote server", systemImage: "globe").tag(true)
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            Form {
                if isRemote {
                    TextField("Address", text: $url, prompt: Text("https://example.com/mcp"))
                    SecureField("Access token", text: $authToken, prompt: Text("Optional"))
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
                    .disabled(saving || !loaded || !isComplete)
            }
        }
        .padding(Metric.roomy)
        .frame(width: 520)
        .task { await load() }
    }

    private var isComplete: Bool {
        isRemote
            ? !url.trimmingCharacters(in: .whitespaces).isEmpty
            : !command.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// Load the daemon's complete definition once. Saving starts from this
    /// value, so advanced settings the compact form does not show stay intact.
    private func load() async {
        guard !loaded else { return }
        do {
            let config = try await model.serverConfig(name: name)
            loadedConfig = config
            isRemote = config.transport.lowercased() != "stdio"
            command = config.command ?? ""
            arguments = Self.renderArguments(config.args)
            url = config.url ?? ""
            authToken = config.authToken ?? ""
            environment = config.env
                .sorted { $0.key.localizedStandardCompare($1.key) == .orderedAscending }
                .map { "\($0.key)=\($0.value)" }
                .joined(separator: "\n")
            loaded = true
        } catch {
            failure = error.localizedDescription
        }
    }

    private func save() {
        saving = true
        failure = nil
        guard var config = loadedConfig else {
            failure = "The server settings could not be loaded."
            return
        }
        if isRemote {
            config.transport = "http"
            config.command = nil
            config.args = []
            config.url = url.trimmingCharacters(in: .whitespaces)
            let token = authToken.trimmingCharacters(in: .whitespaces)
            config.authToken = token.isEmpty ? nil : token
        } else {
            config.transport = "stdio"
            config.command = command.trimmingCharacters(in: .whitespaces)
            config.args = ServerDraftParser.tokenize(arguments)
            config.url = nil
            config.authToken = nil
            config.auth = nil
            config.oauthClientID = nil
            config.oauthScopes = nil
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

    nonisolated static func parseEnvironment(_ text: String) -> [String: String] {
        var result: [String: String] = [:]
        for line in text.split(whereSeparator: { $0 == "\n" }) {
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

    nonisolated static func renderArguments(_ arguments: [String]) -> String {
        arguments.map { argument in
            guard argument.isEmpty || argument.contains(where: { $0.isWhitespace || "'\"\\".contains($0) }) else {
                return argument
            }
            return "'\(argument.replacingOccurrences(of: "'", with: "'\\''"))'"
        }.joined(separator: " ")
    }
}
