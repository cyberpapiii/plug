import PlugIPC
import SwiftUI

/// Bringing over the servers already set up in other apps.
///
/// Most people arrive at Plug with servers configured in Claude Desktop or
/// Cursor, and the point of Plug is that they only have to be configured once.
/// The sheet reads those settings, shows what it found grouped by the app it
/// came from, and copies over exactly what is ticked. Nothing in another app's
/// settings is changed.
struct ImportServersView: View {
    let model: AppModel
    var scanner: ImportScanning = ImportService()
    @Environment(\.dismiss) private var dismiss

    @State private var scan: ImportScan?
    @State private var chosen: Set<String> = []
    @State private var failure: String?
    @State private var importing = false
    @State private var scanFailed = false

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Import servers").font(.title2.weight(.semibold))
                Text("Servers already set up in your other AI apps. Their settings are left as they are.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            content

            HStack(spacing: Metric.snug) {
                if let failure, !scanFailed {
                    Label(failure, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                if let scan, !scan.isEmpty {
                    Button(importTitle) { importChosen() }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(chosen.isEmpty || importing)
                }
            }
        }
        .padding(Metric.roomy)
        .frame(width: 520)
        .interactiveDismissDisabled(importing)
        .task { await load() }
    }

    private var importTitle: String {
        if importing { return "Importing…" }
        return chosen.count == 1 ? "Import 1 Server" : "Import \(chosen.count) Servers"
    }

    // MARK: - What was found

    @ViewBuilder private var content: some View {
        if scanFailed {
            VStack(alignment: .leading, spacing: Metric.snug) {
                Label(failure ?? "Plug could not scan the other apps.", systemImage: "exclamationmark.triangle")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Button("Try Again") { Task { await load() } }
            }
            .frame(maxWidth: .infinity, minHeight: 120, alignment: .leading)
        } else if let scan {
            if scan.isEmpty {
                VStack(alignment: .leading, spacing: Metric.tight) {
                    Label("Nothing new to import", systemImage: "checkmark.circle")
                        .font(.callout)
                    Text("Every server your other apps use is already in Plug.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(Metric.regular)
                .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: Metric.corner))
            } else {
                found(scan)
            }
        } else {
            HStack(spacing: Metric.snug) {
                ProgressView().controlSize(.small)
                Text("Looking through your other apps…")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, minHeight: 120, alignment: .leading)
        }
    }

    private func found(_ scan: ImportScan) -> some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            SectionLabel(
                text: "Found in other apps",
                trailing: scan.servers.count == 1 ? "1 server" : "\(scan.servers.count) servers"
            )
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(sources(of: scan), id: \.self) { source in
                        SectionLabel(text: sourceName(source, in: scan))
                            .padding(.top, Metric.regular)
                            .padding(.bottom, Metric.hairline)
                        ForEach(scan.servers.filter { $0.source == source }) { server in
                            row(server)
                        }
                    }
                }
            }
            .frame(height: 240)
            .scrollBounceBehavior(.basedOnSize)

            if !scan.unreadable.isEmpty {
                Label(
                    "Plug couldn't read \(scan.unreadable.joined(separator: ", ")), so anything set up there isn't listed.",
                    systemImage: "exclamationmark.circle"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func row(_ server: DiscoveredServer) -> some View {
        Toggle(isOn: binding(for: server)) {
            HStack(spacing: Metric.snug) {
                AppGlyph(target: server.source, name: server.sourceName, size: 18)
                VStack(alignment: .leading, spacing: 0) {
                    Text(server.name).font(.body)
                    Text(serverDescription(server))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 0)
            }
        }
        .toggleStyle(.checkbox)
        .padding(.vertical, Metric.snug - 2)
        .help(server.detail)
    }

    private func serverDescription(_ server: DiscoveredServer) -> String {
        if let url = server.config.url,
           let host = URL(string: url)?.host
        {
            return "Remote server · \(host)"
        }
        if let command = server.config.command {
            return "Runs on this Mac · \(URL(fileURLWithPath: command).lastPathComponent)"
        }
        return "Server configuration"
    }

    private func binding(for server: DiscoveredServer) -> Binding<Bool> {
        Binding(
            get: { chosen.contains(server.id) },
            set: { on in
                if on { chosen.insert(server.id) } else { chosen.remove(server.id) }
            }
        )
    }

    private func sources(of scan: ImportScan) -> [String] {
        var seen: [String] = []
        for server in scan.servers where !seen.contains(server.source) {
            seen.append(server.source)
        }
        return seen
    }

    private func sourceName(_ source: String, in scan: ImportScan) -> String {
        scan.servers.first { $0.source == source }?.sourceName ?? source
    }

    // MARK: - Work

    private func load() async {
        scanFailed = false
        failure = nil
        scan = nil
        do {
            let result = try await scanner.scan()
            scan = result
            scanFailed = false
            // Everything is ticked to begin with: someone who opens this sheet
            // wants their servers, not a checklist.
            chosen = Set(result.servers.map(\.id))
        } catch {
            scan = nil
            scanFailed = true
            failure = error.localizedDescription
        }
    }

    private func importChosen() {
        guard let scan else { return }
        importing = true
        failure = nil
        let wanted = scan.servers.filter { chosen.contains($0.id) }
        Task {
            var failed: [String] = []
            for server in wanted {
                do {
                    try await model.performOperation {
                        .addServer(authToken: $0, name: server.name, server: server.config)
                    }
                } catch {
                    failed.append(server.name)
                }
            }
            importing = false
            if failed.isEmpty {
                dismiss()
            } else {
                failure = failed.count == 1
                    ? "\(failed[0]) could not be added."
                    : "\(failed.count) servers could not be added."
            }
        }
    }
}
