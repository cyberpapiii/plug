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
    let router: Router
    var scanner: ImportScanning = ImportService()
    @Environment(\.dismiss) private var dismiss

    @State private var scan: ImportScan?
    @State private var chosen: Set<String> = []
    @State private var failure: String?
    @State private var importing = false
    @State private var scanFailed = false
    /// Why each server that could not be added was refused, by server id.
    @State private var refused: [String: String] = [:]

    /// The list scrolls once it is taller than this.
    private static let listHeight: CGFloat = 240

    var body: some View {
        SheetFrame(
            title: "Import Servers",
            subtitle: "Servers already set up in other clients on this Mac. Their settings are left as they are.",
            failure: scanFailed ? nil : failure,
            busy: importing,
            // With nothing to import there is nothing to confirm, only Done.
            confirmTitle: (scan?.isEmpty == true || scanFailed) ? nil : importTitle,
            confirmDisabled: chosen.isEmpty || scan == nil,
            confirm: importChosen
        ) {
            content
        }
        .task { await load() }
    }

    private var importTitle: String {
        switch chosen.count {
        case 0: "Import Servers"
        case 1: "Import 1 Server"
        default: "Import \(chosen.count) Servers"
        }
    }

    // MARK: - What was found

    @ViewBuilder private var content: some View {
        if scanFailed {
            ProblemNote(
                title: "Plug could not read your other clients' settings.",
                reason: failure,
                actionTitle: "Try Again",
                action: { Task { await load() } }
            )
            .padding(.horizontal, Metric.roomy)
        } else if let scan {
            if scan.isEmpty {
                ContentUnavailableView(
                    "Nothing New to Import",
                    systemImage: "checkmark.circle",
                    description: Text("Every server your other clients use is already in Plug.")
                )
            } else {
                found(scan)
            }
        } else {
            SheetLoading(label: "Looking through your other clients", height: 120)
        }
    }

    private func found(_ scan: ImportScan) -> some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            // A short list takes only the room it needs; a long one scrolls.
            ViewThatFits(in: .vertical) {
                rows(scan)
                ScrollView { rows(scan) }
            }
            .frame(maxHeight: Self.listHeight)

            if !scan.unreadable.isEmpty {
                InlineWarning(
                    "Plug could not read \(scan.unreadable.joined(separator: ", ")), so anything set up there is not listed."
                )
                .font(.caption)
            }
        }
        .padding(.horizontal, Metric.roomy)
    }

    /// Every server found, under the client it came from.
    private func rows(_ scan: ImportScan) -> some View {
        let order = sources(of: scan)
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(order, id: \.self) { source in
                let name = sourceName(source, in: scan)
                HStack(spacing: Metric.tight) {
                    AppGlyph(target: source, name: name)
                    SectionLabel(text: name)
                }
                .padding(.top, source == order.first ? 0 : Metric.regular)
                .padding(.bottom, Metric.hairline)
                ForEach(scan.servers.filter { $0.source == source }) { server in
                    row(server)
                }
            }
        }
    }

    private func row(_ server: DiscoveredServer) -> some View {
        Toggle(isOn: binding(for: server)) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(server.name).font(.body)
                Text(serverDescription(server))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let reason = refused[server.id] {
                    InlineWarning("Not added: \(reason)")
                        .font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .toggleStyle(.checkbox)
        .padding(.vertical, Metric.tight)
        .help(server.detail)
    }

    private func serverDescription(_ server: DiscoveredServer) -> String {
        if let url = server.config.url,
           let host = URL(string: url)?.host
        {
            return "Over the network · \(host)"
        }
        if let command = server.config.command {
            return "On this Mac · \(URL(fileURLWithPath: command).lastPathComponent)"
        }
        return "Server"
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

    /// Reads what the other clients have. `keeping` is what stays ticked;
    /// nil ticks everything, because someone who opens this sheet wants their
    /// servers, not a checklist.
    private func load(keeping: Set<String>? = nil) async {
        scanFailed = false
        failure = nil
        scan = nil
        do {
            let result = try await scanner.scan()
            scan = result
            scanFailed = false
            let found = Set(result.servers.map(\.id))
            chosen = keeping.map { $0.intersection(found) } ?? found
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
        refused = [:]
        let wanted = scan.servers.filter { chosen.contains($0.id) }
        Task {
            var failed: [String: String] = [:]
            for server in wanted {
                do {
                    try await model.performOperation {
                        .addServer(authToken: $0, name: server.name, server: server.config)
                    }
                } catch {
                    failed[server.id] = error.localizedDescription
                }
            }
            importing = false
            guard !failed.isEmpty else {
                // One server went in: show it. Several: the list shows them.
                if wanted.count == 1, let only = wanted.first {
                    router.reveal(server: only.name)
                }
                dismiss()
                return
            }
            // The ones that went in leave the list; the rest stay ticked with
            // their reason, so trying again retries only what failed.
            await load(keeping: Set(failed.keys))
            refused = failed
            failure = Self.summary(
                failed: wanted.filter { failed[$0.id] != nil }.map(\.name),
                of: wanted.count
            )
        }
    }

    /// Names what could not be added, and says the rest went in.
    nonisolated static func summary(failed: [String], of total: Int) -> String {
        let names = failed.count <= 3
            ? failed.joined(separator: ", ")
            : failed.prefix(3).joined(separator: ", ") + ", and \(failed.count - 3) more"
        let added = total - failed.count
        let rest = added == 0 ? "" : added == 1 ? " 1 server was added." : " \(added) servers were added."
        return "Could not add \(names).\(rest) The reason is under each one."
    }
}
