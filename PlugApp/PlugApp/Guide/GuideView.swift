import AppKit
import SwiftUI

/// The first run. It says what Plug is in one picture and walks the three
/// things a newcomer does, each with the button that does it. It reads live
/// state, so a step ticks itself off when it is done.
struct GuideView: View {
    let model: AppModel
    /// Close the guide, then do something in the window behind it.
    let leave: (PlugIntent?) -> Void
    @State private var copied = false

    private var guide: FirstRunGuide {
        FirstRunGuide(
            serverCount: model.snapshot.configuredServers.count,
            clientCount: model.connectableApps.filter { $0.linked || $0.live }.count
                + model.snapshot.downstreamClients.count,
            hasActivity: !model.activities.isEmpty
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("How Plug works").font(.title2.weight(.semibold))
                Text("One place on your Mac that holds every tool you have and gives it to every client you use.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            sides

            VStack(spacing: Metric.snug) {
                ForEach(Array(FirstRunGuide.Step.allCases.enumerated()), id: \.element) { index, step in
                    row(step, number: index + 1)
                }
            }

            Divider()

            HStack(spacing: Metric.snug) {
                if FirstRunGuide.agentPrompt != nil {
                    VStack(alignment: .leading, spacing: Metric.hairline) {
                        Text("Would you like your agent to do this?").font(.callout)
                        Text("Paste the prompt into Claude, Codex, or any agent with a terminal.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Button(copied ? "Copied" : "Copy Setup Prompt") { copyPrompt() }
                }
                Spacer(minLength: 0)
                Button("Done") { leave(nil) }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(Metric.roomy)
        .frame(width: SheetFrame<EmptyView, EmptyView>.width)
        .task { await model.loadConnectableApps() }
    }

    /// Plug's two sides, as the one picture a newcomer needs.
    private var sides: some View {
        HStack(spacing: Metric.snug) {
            side("Servers", "provide tools", symbol: "shippingbox")
            Image(systemName: "arrow.right").foregroundStyle(.tertiary)
            side("Plug", "holds them all", symbol: "powerplug")
            Image(systemName: "arrow.right").foregroundStyle(.tertiary)
            side("Clients", "use tools", symbol: "app.connected.to.app.below.fill")
        }
        .frame(maxWidth: .infinity)
        .padding(Metric.regular)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: Metric.corner))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Servers provide tools. Plug holds them all. Clients use tools.")
    }

    private func side(_ title: String, _ detail: String, symbol: String) -> some View {
        VStack(spacing: Metric.rowGap) {
            Image(systemName: symbol)
                .font(.title2)
                .foregroundStyle(.secondary)
                .frame(height: 26)
            Text(title).font(.callout.weight(.semibold))
            Text(detail).font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
    }

    private func row(_ step: FirstRunGuide.Step, number: Int) -> some View {
        let done = guide.isDone(step)
        let isNext = guide.next == step
        return HStack(alignment: .top, spacing: Metric.snug) {
            Image(systemName: done ? "checkmark.circle.fill" : "\(number).circle")
                .font(.title3)
                .foregroundStyle(done ? AnyShapeStyle(.green) : AnyShapeStyle(.secondary))
                .frame(width: 24)
                .accessibilityLabel(done ? "Done" : "Step \(number)")
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(step.title).font(.body.weight(.medium))
                Text(step.detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.snug)
            actions(step, prominent: isNext)
        }
    }

    @ViewBuilder private func actions(_ step: FirstRunGuide.Step, prominent: Bool) -> some View {
        switch step {
        case .server:
            VStack(alignment: .trailing, spacing: Metric.rowGap) {
                action("Import Servers…", .importServers, prominent: prominent)
                action("Add Server…", .addServer, prominent: false)
            }
            .disabled(!model.canMutate)
        case .client:
            action("Show Clients", .openWindow(.clients), prominent: prominent)
        case .activity:
            action("Show Activity", .openWindow(.activity), prominent: prominent)
        }
    }

    @ViewBuilder private func action(_ title: String, _ intent: PlugIntent, prominent: Bool) -> some View {
        if prominent {
            Button(title) { leave(intent) }.buttonStyle(.borderedProminent)
        } else {
            Button(title) { leave(intent) }
        }
    }

    private func copyPrompt() {
        guard let prompt = FirstRunGuide.agentPrompt else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(prompt, forType: .string)
        copied = true
    }
}
