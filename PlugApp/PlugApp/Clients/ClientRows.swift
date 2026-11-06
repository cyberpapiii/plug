import PlugIPC
import SwiftUI

struct ClientGlyph: View {
    let glyph: ClientEntry.Glyph
    /// The large one fills a detail header's glyph slot.
    var large = false

    var body: some View {
        let size = large ? Metric.glyphSlot : 18
        switch glyph {
        case let .app(target, name, appPath):
            AppGlyph(target: target, name: name, appPath: appPath, size: size)
        case let .grant(name):
            let target = AppIcons.target(forClientType: name)
            AppGlyph(target: target, name: target == "python" ? "Python" : name, size: size)
        }
    }
}

/// One client: its icon, its name, and one switch. The switch works without
/// selecting the row. Equal while what it shows is the same, so one client
/// changing does not redraw every row.
struct ClientRow: View, Equatable {
    let entry: ClientEntry
    /// What the row shows, kept beside the entry so two rows can be compared.
    let face: ClientEntry.Face
    let canMutate: Bool

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.face == rhs.face && lhs.canMutate == rhs.canMutate
    }

    private static let liveDot: CGFloat = 7

    var body: some View {
        HStack(spacing: Metric.tight) {
            ClientGlyph(glyph: entry.glyph)
                .opacity(entry.dimmed ? 0.4 : 1)
            Text(entry.name)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
                .foregroundStyle(entry.dimmed ? .secondary : .primary)
                .layoutPriority(1)
                .accessibilityLabel("\(entry.name), \(entry.status.text)")
            if let disambiguator = entry.disambiguator {
                Text(disambiguator)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: Metric.tight)
            if entry.asking != nil {
                Text("New")
                    .font(PanelType.small.weight(.medium))
                    .padding(.horizontal, Metric.tight)
                    .background(.tint.opacity(0.2), in: Capsule())
                    .accessibilityLabel("New sign-in")
            }
            if let note = entry.note {
                Text(note)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            if entry.needsSignIn {
                PlugIcon(.needsYou)
                    .foregroundStyle(StatusColor.needsYou)
                    .help("Needs sign-in")
                    .accessibilityLabel("Needs sign-in")
            }
            if entry.isLive {
                Circle()
                    .fill(StatusColor.working)
                    .frame(width: Self.liveDot, height: Self.liveDot)
                    .accessibilityLabel("Connected")
            }
            ClientSwitch(entry: entry, canMutate: canMutate, size: .mini)
        }
        .contextMenu {
            if entry.grantID == nil, let isOn = entry.isOn, !entry.isBusy {
                Button(isOn ? "Turn Off" : "Turn On") { entry.setOn(!isOn) }
                Divider()
            }
            IconMenu(key: entry.glyph.iconKey)
            if let forget = entry.forget, canMutate {
                Divider()
                Button("Forget Now", role: .destructive, action: forget)
            } else if entry.grantID != nil, canMutate {
                Divider()
                Button("Remove Access…", role: .destructive) { entry.setOn(false) }
            }
        }
    }
}

/// A client's one switch. A switch that adds Plug to another app's settings
/// works while Plug is off; one that takes away access needs Plug running.
struct ClientSwitch: View {
    let entry: ClientEntry
    let canMutate: Bool
    var size: ControlSize = .regular

    private var isGrant: Bool { entry.grantID != nil }

    var body: some View {
        if let isOn = entry.isOn {
            Toggle(entry.switchLabel, isOn: Binding(get: { isOn }, set: entry.setOn))
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(size)
                .disabled(entry.isBusy || (isGrant && !canMutate))
                .help(isGrant ? "Remove Access…" : "")
                .accessibilityLabel("\(entry.switchLabel), \(entry.name)")
        }
    }
}
