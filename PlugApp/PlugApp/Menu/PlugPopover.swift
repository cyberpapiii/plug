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
    let section: AppSection
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
                attentionList
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
    /// so it is not news and does not belong in a status panel. Servers already
    /// shown as problems above are not repeated.
    private var listedServers: [ServerFacts] {
        situation.activeServers.filter { !$0.health.needsAttention }
    }

    @ViewBuilder private var attentionList: some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            GlassEffectContainer(spacing: Metric.tight) { attentionContent }
        } else {
            attentionContent
        }
#else
        attentionContent
#endif
    }

    private var attentionContent: some View {
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

    @ViewBuilder private var footer: some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            GlassEffectContainer(spacing: Metric.tight) { footerControls }
        } else {
            footerControls
        }
#else
        footerControls
#endif
    }

    private var footerControls: some View {
        HStack(spacing: Metric.tight) {
            Button { run(.addServer) } label: {
                Label("Add Server", systemImage: "plus")
                    .font(.callout)
            }
            .nativeGlassButton()
            .controlSize(.small)
            .fixedSize()
            .help("Add a server")

            Button { run(.openWindow(section)) } label: {
                Label("Open Plug", systemImage: "macwindow")
                    .font(.callout)
            }
            .nativeGlassButton()
            .controlSize(.small)
            .fixedSize()
            .help("Open the Plug window")

            Spacer(minLength: 0)

            Menu {
                SettingsLink {
                    Label("Settings…", systemImage: "gearshape")
                }
                Divider()
                Button { run(.quit) } label: {
                    Label("Quit Plug", systemImage: "power")
                }
            } label: {
                Image(systemName: "ellipsis")
                    .font(.callout.weight(.semibold))
            }
            .nativeGlassButton()
            .controlSize(.small)
            .fixedSize()
            .help("More")
            .accessibilityLabel("More")
        }
        .padding(.horizontal, Metric.snug)
        .padding(.vertical, Metric.tight)
    }
}
