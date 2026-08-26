import AppKit
import SwiftUI

/// The app.
///
/// Plug is background infrastructure, so almost every visit is one of two
/// questions: "is it working?" and "what do I press to fix it?". This panel
/// answers both without opening a window. The window exists for the rare work —
/// adding a server, auditing connections, reading history — and nothing that
/// belongs here has been moved there.
struct PlugPopover: View {
    let model: AppModel
    let run: (PlugIntent) -> Void

    private var situation: PlugSituation { model.situation }
    private var attention: [AttentionItem] { model.attentionItems }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VerdictView(verdict: model.verdict, run: run)
                .popoverInset()
                .padding(.top, Metric.regular)
                .padding(.bottom, Metric.regular)

            if !attention.isEmpty {
                VStack(spacing: Metric.tight) {
                    ForEach(attention.prefix(3)) { item in
                        AttentionRow(item: item, run: run)
                    }
                    if attention.count > 3 {
                        Button("Show all \(attention.count)") { run(.openWindow(.servers)) }
                            .buttonStyle(.link)
                            .font(.caption)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .popoverInset()
                .padding(.bottom, Metric.regular)
            }

            if !listedServers.isEmpty {
                Divider()
                serverList
            }

            if situation.connectedApps > 0 {
                Divider()
                Button { run(.openWindow(.connections)) } label: {
                    DisclosureRow(
                        symbol: "app.connected.to.app.below.fill",
                        title: connectedAppsText,
                        detail: nil
                    ) {
                        Image(systemName: "chevron.right")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                }
                .buttonStyle(QuietRowButtonStyle())
                .padding(.horizontal, Metric.tight)
                .padding(.vertical, Metric.tight)
            }

            Divider()
            footer
        }
        .frame(width: Metric.popoverWidth)
        .animation(.snappy(duration: 0.18), value: model.verdict)
        .animation(.snappy(duration: 0.18), value: attention)
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
    }

    // MARK: - Servers

    /// Only servers meant to be running. Anything switched off is deliberate,
    /// so it is not news and does not belong in a status panel.
    private var listedServers: [ServerFacts] {
        situation.activeServers
    }

    private var serverList: some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            SectionLabel(text: "Servers", trailing: serversSummary)
                .popoverInset()
                .padding(.top, Metric.snug)

            ScrollView {
                VStack(spacing: 0) {
                    ForEach(listedServers) { server in
                        Button { run(.reveal(server: server.name)) } label: {
                            ServerRow(server: server)
                        }
                        .buttonStyle(QuietRowButtonStyle())
                    }
                }
                .padding(.horizontal, Metric.tight)
            }
            .frame(height: listHeight)
            .scrollBounceBehavior(.basedOnSize)
            .padding(.bottom, Metric.tight)
        }
    }

    private var listHeight: CGFloat {
        min(CGFloat(listedServers.count) * 27, Metric.popoverMaxListHeight)
    }

    private var serversSummary: String {
        let working = situation.workingServers.count
        return working == listedServers.count ? "All working" : "\(working) of \(listedServers.count) working"
    }

    private var connectedAppsText: String {
        situation.connectedApps == 1 ? "1 app connected" : "\(situation.connectedApps) apps connected"
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: Metric.tight) {
            Button { run(.addServer) } label: {
                Label("Add Server", systemImage: "plus.circle")
                    .font(.callout)
            }
            .buttonStyle(QuietRowButtonStyle())
            .fixedSize()
            .help("Add a server")

            Button { run(.openWindow(.servers)) } label: {
                Label("Open Plug", systemImage: "macwindow")
                    .font(.callout)
            }
            .buttonStyle(QuietRowButtonStyle())
            .fixedSize()
            .help("Open the Plug window")

            Spacer(minLength: 0)

            // Settings and Quit are what people look for in a menu bar app, so
            // they are visible controls rather than entries inside a menu. Both
            // are icon-only: the picture is the label, and the tooltip and the
            // accessibility label carry the words.
            SettingsLink {
                Image(systemName: "gearshape")
                    .font(.callout)
            }
            .buttonStyle(QuietIconButtonStyle())
            .fixedSize()
            .help("Settings")
            .accessibilityLabel("Settings")

            Button { run(.quit) } label: {
                Image(systemName: "power")
                    .font(.callout)
            }
            .buttonStyle(QuietIconButtonStyle())
            .fixedSize()
            .help("Quit Plug. Your servers keep running.")
            .accessibilityLabel("Quit Plug")
        }
        .padding(.horizontal, Metric.snug)
        .padding(.vertical, Metric.tight)
    }
}
