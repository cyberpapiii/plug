import AppKit
import SwiftUI

/// One app, shown as itself. An app with no icon on this Mac gets a tile
/// with its symbol or its first letter, so every row has a picture of the
/// same size and weight.
struct AppGlyph: View {
    let target: String
    let name: String
    /// The app bundle behind a client with no entry of its own.
    var appPath: String? = nil
    var size: CGFloat = 18

    var body: some View {
        Group {
            if let icon = IconStore.shared.image(forClient: target, name: name, appPath: appPath) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
            } else {
                let symbol = AppIcons.symbol(target: target, name: name)
                MonogramTile(
                    name: name.isEmpty ? target : name,
                    symbol: symbol == "app" ? nil : symbol,
                    size: size
                )
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// One server, shown as the best picture Plug has for it, and as a tile
/// with its first letter when it has none.
struct ServerGlyph: View {
    let name: String
    var size: CGFloat = 18
    /// The server's state as a dot on the corner of the icon, the way an
    /// app shows a contact's presence. Nil draws no dot.
    var status: Color?

    private var dot: CGFloat { max(7, size * 0.32) }

    var body: some View {
        Group {
            if let icon = IconStore.shared.image(forServer: name) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
            } else {
                MonogramTile(name: name, size: size)
            }
        }
        .frame(width: size, height: size)
        // The gap around the dot is cut out of the icon, not painted over it,
        // so it is right on any background, glass included.
        .mask {
            Rectangle()
                .overlay(alignment: .bottomTrailing) {
                    if status != nil {
                        Circle()
                            .frame(width: dot * 1.4, height: dot * 1.4)
                            .offset(x: dot * 0.35, y: dot * 0.35)
                            .blendMode(.destinationOut)
                    }
                }
                .compositingGroup()
        }
        .overlay(alignment: .bottomTrailing) {
            if let status {
                Circle()
                    .fill(status)
                    .frame(width: dot, height: dot)
                    .offset(x: dot * 0.15, y: dot * 0.15)
            }
        }
        .accessibilityHidden(true)
    }
}

/// The stand-in for an icon: a rounded tile carrying a symbol or the first
/// letter of a name. The color comes from the name, so it stays the same
/// from one launch to the next.
struct MonogramTile: View {
    let name: String
    /// A symbol that says what kind of thing this is, shown instead of the
    /// letter on a neutral tile.
    var symbol: String?
    var size: CGFloat = 18

    private nonisolated static let tints: [Color] = [.blue, .indigo, .purple, .pink, .orange, .teal, .green, .brown]

    /// Which tint a name gets.
    ///
    /// The same name always gets the same tint.
    nonisolated static func tintIndex(for name: String) -> Int {
        let sum = name.lowercased().unicodeScalars.reduce(0) { ($0 &* 31 &+ Int($1.value)) % 1_000_003 }
        return sum % tints.count
    }

    nonisolated static func letter(for name: String) -> String {
        name.first { $0.isLetter || $0.isNumber }.map { String($0).uppercased() } ?? "?"
    }

    var body: some View {
        // App icons keep a margin inside their square; the tile keeps the
        // same one so the two sit level in a list.
        let side = size * 0.84
        RoundedRectangle(cornerRadius: side * 0.24, style: .continuous)
            .fill(symbol == nil ? AnyShapeStyle(Self.tints[Self.tintIndex(for: name)].gradient) : AnyShapeStyle(Color.gray.gradient))
            .frame(width: side, height: side)
            .overlay {
                if let symbol {
                    Image(systemName: symbol)
                        .font(.system(size: side * 0.5, weight: .semibold))
                        .foregroundStyle(.white)
                } else {
                    Text(Self.letter(for: name))
                        .font(.system(size: side * 0.58, weight: .semibold, design: .rounded))
                        .foregroundStyle(.white)
                }
            }
            .frame(width: size, height: size)
    }
}

/// The two menu items that let a person pick an icon and take it back.
struct IconMenu: View {
    /// The name the chosen icon is kept under.
    let key: String

    var body: some View {
        Button("Choose Icon…") { IconStore.shared.chooseIcon(for: key) }
        if IconStore.shared.chosen[key] != nil {
            Button("Use Default Icon") { IconStore.shared.removeChosenIcon(for: key) }
        }
    }
}
