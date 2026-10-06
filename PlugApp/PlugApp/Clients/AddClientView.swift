import AppKit
import PlugIPC
import SwiftUI

/// What the Add a Client sheet offers, apart from how it is drawn.
enum AddClient {
    /// The clients on this Mac that could use Plug and do not yet, by name.
    /// One added from this sheet stays in the list, so its row can say so.
    static func choices(_ apps: [LinkableApp], added: Set<String>) -> [LinkableApp] {
        apps
            .filter { $0.detected && (!$0.linked || added.contains($0.target)) }
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    /// True when the address reaches Plug from outside this Mac.
    static func reachesBeyondThisMac(_ address: String) -> Bool {
        guard let host = URL(string: address)?.host()?.lowercased() else { return false }
        return !["localhost", "127.0.0.1", "::1", "[::1]"].contains(host)
    }

    /// What to do with the address, for a client with no row above.
    static func addressAdvice(_ address: String) -> String {
        reachesBeyondThisMac(address)
            ? "For ChatGPT, Claude on the web, or any client not listed: add a connector or MCP server there and paste this address. Plug then opens a page asking you to approve it."
            : "For a client not listed: paste this address into its MCP settings. It works only on this Mac."
    }
}

/// How a client that lost its sign-in gets it back. Only the client can start
/// one, so the app says where.
enum ClientSignIn {
    static func steps(target: String, name: String) -> String {
        switch target {
        case "codex-cli": "In Terminal, run codex mcp login plug."
        case "claude-code": "In Claude Code, type /mcp, choose plug, then Authenticate."
        default: "Open \(name) and use Plug from there."
        }
    }

    static func about(target: String, name: String) -> String {
        "Its sign-in ended, so it cannot use Plug until it signs in again. "
            + steps(target: target, name: name)
            + " Plug then asks you to approve it. Turn this off to remove it instead."
    }
}

/// Adding a client is one button for a client on this Mac, and one address
/// to paste for any other.
struct AddClientView: View {
    let model: AppModel
    /// The clients added from this sheet, so their rows stay and say so.
    @State private var added: Set<String> = []
    @State private var copied = false

    private var choices: [LinkableApp] { AddClient.choices(model.connectableApps, added: added) }

    var body: some View {
        SheetFrame(
            title: "Add a Client",
            subtitle: "A client is an app that uses your servers, such as Claude or Cursor."
        ) {
            Form {
                Section("On This Mac") {
                    if choices.isEmpty {
                        Text(model.isLoadingConnectableApps
                            ? "Looking for clients…"
                            : "Every client Plug found on this Mac already uses it.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(choices) { app in row(app) }
                    }
                }
                if let address = model.snapshot.clientAddress {
                    Section {
                        HStack(spacing: Metric.snug) {
                            Text(address)
                                .font(.body.monospaced())
                                .textSelection(.enabled)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer(minLength: 0)
                            Button(copied ? "Copied" : "Copy") {
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(address, forType: .string)
                                copied = true
                            }
                        }
                    } header: {
                        Text("Any Other Client")
                    } footer: {
                        Text(AddClient.addressAdvice(address))
                    }
                }
            }
            .formStyle(.grouped)
            .fixedSize(horizontal: false, vertical: true)
        }
        .task { await model.loadConnectableApps() }
    }

    private func row(_ app: LinkableApp) -> some View {
        HStack(spacing: Metric.snug) {
            AppGlyph(target: app.target, name: app.name, size: 22)
            Text(app.name)
            Spacer(minLength: 0)
            if model.busyApps.contains(app.target) {
                ProgressView().controlSize(.small)
            } else if app.linked {
                Label("Added. Restart \(app.name).", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.secondary)
            } else {
                Button("Add") {
                    added.insert(app.target)
                    Task { await model.setAppLinked(app.target, true) }
                }
                .accessibilityLabel("Add \(app.name)")
            }
        }
    }
}
