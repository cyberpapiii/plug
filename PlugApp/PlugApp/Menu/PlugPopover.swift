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
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openSettings) private var openSettings

    private var situation: PlugSituation { model.situation }
    private var attention: [AttentionItem] { model.attentionItems }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VerdictView(verdict: model.verdict, run: send)
                .popoverInset()
                .padding(.top, Metric.regular)
                .padding(.bottom, Metric.regular)

            if attention.count > 1 {
                attentionContent
            }

            if !listedServers.isEmpty {
                serverList
            }

            if situation.connectedApps > 0 {
                Button { send(.openWindow(.connections)) } label: {
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
                .popoverInset()
                .padding(.vertical, Metric.tight)
            }

            footer
        }
        .frame(width: Metric.popoverWidth)
        .animation(reduceMotion ? nil : .snappy(duration: 0.18), value: model.verdict)
        .animation(reduceMotion ? nil : .snappy(duration: 0.18), value: attention)
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
    }

    // MARK: - Servers

    @ViewBuilder private var attentionContent: some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            GlassEffectContainer(spacing: Metric.tight) { attentionRows }
        } else {
            attentionRows
        }
#else
        attentionRows
#endif
    }

    private var attentionRows: some View {
        VStack(spacing: Metric.tight) {
            ForEach(attention.prefix(3)) { item in
                AttentionRow(item: item, run: send)
            }
            if attention.count > 3 {
                Button("Show all \(attention.count) issues") { send(.openWindow(.servers)) }
                    .buttonStyle(.link)
                    .font(.caption)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .popoverInset()
        .padding(.bottom, Metric.regular)
    }

    private var listedServers: [ServerFacts] {
        situation.activeServers.filter { !situation.troubledServers.contains($0) }
    }

    private var serverList: some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            HStack(spacing: Metric.tight) {
                Text(attention.isEmpty ? "Servers" : "Other servers")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
                if attention.isEmpty {
                    Label(serversSummary, systemImage: serversSummarySymbol)
                        .font(.caption)
                        .foregroundStyle(serversSummaryColor)
                }
            }
            .padding(.horizontal, Metric.tight)

            ScrollView {
                LazyVStack(spacing: Metric.hairline) {
                    ForEach(listedServers) { server in
                        Button { send(.reveal(server: server.name)) } label: {
                            ServerRow(server: server)
                                .padding(.vertical, Metric.hairline)
                        }
                        .buttonStyle(QuietRowButtonStyle())
                    }
                }
                .padding(Metric.hairline)
            }
            .frame(height: listHeight)
            .scrollBounceBehavior(.basedOnSize)
        }
        .padding(.horizontal, Metric.regular)
        .padding(.vertical, Metric.snug)
        .padding(.bottom, Metric.tight)
    }

    private var listHeight: CGFloat {
        min(CGFloat(listedServers.count) * Metric.popoverRowHeight, Metric.popoverMaxListHeight)
    }

    private var serversSummary: String {
        let working = situation.workingServers.count
        return working == listedServers.count ? "All working" : "\(working) of \(listedServers.count)"
    }

    private var serversSummarySymbol: String {
        situation.workingServers.count == listedServers.count
            ? "checkmark.circle.fill"
            : "exclamationmark.circle.fill"
    }

    private var serversSummaryColor: Color {
        situation.workingServers.count == listedServers.count ? .secondary : .orange
    }

    private var connectedAppsText: String {
        situation.connectedApps == 1 ? "1 connected app" : "\(situation.connectedApps) connected apps"
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
            Button { send(.addServer) } label: {
                Label("Add", systemImage: "plus")
                    .font(.callout)
            }
            .nativeGlassButton()
            .controlSize(.small)
            .fixedSize()
            .help("Add a server")

            Button { send(.openCurrentWindow) } label: {
                Label("Open", systemImage: "macwindow")
                    .font(.callout)
            }
            .nativeGlassButton()
            .controlSize(.small)
            .fixedSize()
            .help("Open the Plug window")

            Spacer(minLength: 0)

            // Settings and Quit are what people look for in a menu bar app, so
            // they are visible controls rather than entries inside a menu. Both
            // are icon-only: the picture is the label, and the tooltip and the
            // accessibility label carry the words.
            Button {
                dismiss()
                openSettings()
            } label: {
                Image(systemName: "gearshape")
                    .font(.callout)
            }
            .nativeGlassButton()
            .controlSize(.small)
            .fixedSize()
            .help("Settings")
            .accessibilityLabel("Settings")

            Button { run(.quit) } label: {
                Image(systemName: "power")
                    .font(.callout)
            }
            .nativeGlassButton()
            .controlSize(.small)
            .fixedSize()
            .help("Quit Plug. Your servers keep running.")
            .accessibilityLabel("Quit Plug")
        }
        .popoverInset()
        .padding(.vertical, Metric.tight)
    }

    /// Window-opening actions close the menu panel first. Otherwise its
    /// floating window can remain above the sheet or inspector it opened.
    private func send(_ intent: PlugIntent) {
        switch intent {
        case .addServer, .importServers, .editServer, .openWindow,
             .openCurrentWindow, .reveal, .showRepairLog, .signIn:
            dismiss()
        default:
            break
        }
        run(intent)
    }
}
