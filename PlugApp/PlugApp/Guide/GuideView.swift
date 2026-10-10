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

    /// The step number or the sparkle at the start of a row.
    private static let markWidth: CGFloat = 24
    /// Every row's buttons share one column, so the text beside them lines up.
    private static let actionWidth: CGFloat = 140

    private var guide: FirstRunGuide { model.firstRunGuide }

    var body: some View {
        SheetFrame(
            title: "How Plug Works",
            subtitle: "One place on your Mac that holds every tool you have and gives it to every client you use."
        ) {
            VStack(alignment: .leading, spacing: Metric.regular) {
                sides

                VStack(spacing: Metric.snug) {
                    ForEach(Array(FirstRunGuide.Step.allCases.enumerated()), id: \.element) { index, step in
                        row(step, number: index + 1)
                    }
                    if FirstRunGuide.agentPrompt != nil {
                        setupOffer
                    }
                }
            }
            .padding(.horizontal, Metric.roomy)
        }
        .task { await model.loadConnectableApps() }
    }

    private var doneSteps: Int { FirstRunGuide.Step.allCases.filter(guide.isDone).count }

    /// Plug's two sides, as the one picture a newcomer needs.
    private var sides: some View {
        HStack(spacing: Metric.snug) {
            side("Servers", "provide tools") { sideIcon(.servers) }
            PlugIcon(.show).foregroundStyle(.tertiary)
            side("Plug", "holds them all") {
                // It hops and sparks each time a step below is done.
                PlugCharacter(mood: .awake, cheers: doneSteps).foregroundStyle(.tint).frame(width: 30)
            }
            PlugIcon(.show).foregroundStyle(.tertiary)
            side("Clients", "use tools") { sideIcon(.clients) }
        }
        .frame(maxWidth: .infinity)
        .padding(Metric.regular)
        .nativeInsetSurface(.insetFill)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Servers provide tools. Plug holds them all. Clients use tools.")
    }

    private func sideIcon(_ kind: PlugIcon.Kind) -> some View {
        PlugIcon(kind, size: 26)
            .foregroundStyle(.secondary)
    }

    private func side(_ title: String, _ detail: String, @ViewBuilder icon: () -> some View) -> some View {
        VStack(spacing: Metric.rowGap) {
            icon().frame(height: 30)
            Text(title).font(.callout.weight(.semibold))
            Text(detail).font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
    }

    private func row(_ step: FirstRunGuide.Step, number: Int) -> some View {
        let done = guide.isDone(step)
        return HStack(alignment: .top, spacing: Metric.snug) {
            Group {
                if done {
                    PlugIcon(.worked, size: 22).foregroundStyle(StatusColor.working)
                } else {
                    Image(systemName: "\(number).circle")
                        .font(.title3)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(width: Self.markWidth)
                .accessibilityLabel(done ? "Done" : "Step \(number)")
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(step.title).font(.body.weight(.medium))
                Text(detail(for: step, done: done))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.snug)
            // A done step keeps the column, so every row's text is one width.
            Group {
                if !done { actions(step) }
            }
            .frame(width: Self.actionWidth, alignment: .trailing)
        }
    }

    /// Adding a server needs Plug to be on, so the first step says so.
    private func detail(for step: FirstRunGuide.Step, done: Bool) -> String {
        if step == .server, !done, !model.canMutate { return "Turn Plug on to add servers." }
        return step.detail
    }

    @ViewBuilder private func actions(_ step: FirstRunGuide.Step) -> some View {
        switch step {
        case .server:
            VStack(spacing: Metric.rowGap) {
                action("Import Servers…", .importServers)
                action("Add Server…", .addServer)
            }
            .disabled(!model.canMutate)
        case .client:
            action("Show Clients", .openWindow(.clients))
        case .activity:
            action("Show Activity", .openWindow(.activity))
        }
    }

    /// One button, as wide as the action column. The sheet's Done is its one
    /// prominent button, so these stay plain.
    private func action(_ title: String, _ intent: PlugIntent) -> some View {
        Button { leave(intent) } label: {
            Text(title).frame(maxWidth: .infinity)
        }
    }

    /// The other way to do all three steps: hand the prompt to a client.
    private var setupOffer: some View {
        HStack(alignment: .top, spacing: Metric.snug) {
            Image(systemName: "sparkles")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(width: Self.markWidth)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Want a client to set this up for you?").font(.body.weight(.medium))
                Text("Paste the prompt into Claude, Codex, or any client that can run commands.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.snug)
            Button { copyPrompt() } label: {
                Label("Copy Setup Prompt", icon: copied ? .worked : .copy)
            }
            .contentTransition(.symbolEffect(.replace))
            .fixedSize()
        }
    }

    private func copyPrompt() {
        guard let prompt = FirstRunGuide.agentPrompt else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(prompt, forType: .string)
        copied = true
    }
}
