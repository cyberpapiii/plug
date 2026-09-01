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
    @State private var clearAuthToken = false
    @State private var environment = ""
    @State private var saving = false
    @State private var failure: String?
    @State private var loaded = false
    @State private var loadedConfig: ServerConfig?

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Edit \(name)").font(.title2.weight(.semibold))
                Text(model.canReadServerConfig
                    ? "Changes take effect as soon as you save."
                    : AppModel.serverConfigReadRequiredCopy)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            if loaded {
                Picker("Kind", selection: $isRemote) {
                    Label("Runs on this Mac", systemImage: "desktopcomputer").tag(false)
                    Label("Remote server", systemImage: "globe").tag(true)
                }
                .pickerStyle(.segmented)
                .labelsHidden()

                Form {
                    Section("Connection") {
                        if isRemote {
                            TextField("Address", text: $url, prompt: Text("https://example.com/mcp"))
                                .listRowSeparator(.hidden)
                            SecureField("New access token", text: $authToken, prompt: Text("Keep current token"))
                                .listRowSeparator(.hidden)
                                .onChange(of: authToken) { _, value in
                                    if !value.isEmpty { clearAuthToken = false }
                                }
                            if loadedConfig?.authToken != nil {
                                Toggle("Remove saved access token", isOn: $clearAuthToken)
                                    .listRowSeparator(.hidden)
                                    .disabled(!authToken.isEmpty)
                            }
                        } else {
                            TextField("Command", text: $command, prompt: Text("npx"))
                                .listRowSeparator(.hidden)
                            TextField("Arguments", text: $arguments, prompt: Text("-y linear-mcp"))
                                .listRowSeparator(.hidden)
                        }
                    }
                    .listSectionSeparator(.hidden)

                    Section("Environment variables") {
                        TextField(
                            "Variables",
                            text: $environment,
                            prompt: Text("KEY=value, one per line"),
                            axis: .vertical
                        )
                        .lineLimit(2...5)
                        .listRowSeparator(.hidden)
                    }
                    .listSectionSeparator(.hidden)
                }
                .formStyle(.grouped)
                .frame(height: 260)
            } else if failure == nil {
                HStack(spacing: Metric.snug) {
                    ProgressView().controlSize(.small)
                    Text("Loading server settings…").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 220, alignment: .center)
            }

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
                    .disabled(!Self.canSave(
                        canReadServerConfig: model.canReadServerConfig,
                        loaded: loaded,
                        isComplete: isComplete,
                        saving: saving
                    ))
            }
        }
        .padding(Metric.roomy)
        .frame(width: 520)
        .interactiveDismissDisabled(saving)
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

    private var isComplete: Bool {
        isRemote
            ? !url.trimmingCharacters(in: .whitespaces).isEmpty
            : !command.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// Load the daemon's complete definition once. Saving starts from this
    /// value, so advanced settings the compact form does not show stay intact.
    private func load() async {
        guard !loaded else { return }
        guard model.canReadServerConfig else {
            failure = AppModel.serverConfigReadRequiredCopy
            return
        }
        do {
            let config = try await model.serverConfig(name: name)
            loadedConfig = config
            isRemote = config.transport.lowercased() != "stdio"
            command = config.command ?? ""
            arguments = Self.renderArguments(config.args)
            url = config.url ?? ""
            authToken = ""
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
        guard model.canReadServerConfig else {
            failure = AppModel.serverConfigReadRequiredCopy
            saving = false
            return
        }
        guard var config = loadedConfig else {
            failure = "The server settings could not be loaded."
            saving = false
            return
        }
        if isRemote {
            config.transport = "http"
            config.command = nil
            config.args = []
            config.url = url.trimmingCharacters(in: .whitespaces)
            let token = authToken.trimmingCharacters(in: .whitespaces)
            if !token.isEmpty {
                config.authToken = token
            } else if clearAuthToken {
                config.authToken = nil
            }
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
