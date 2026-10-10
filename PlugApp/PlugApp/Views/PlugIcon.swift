import SwiftUI

/// Plug's own icons. Each is drawn here with one pen, in one weight with
/// round ends, and takes the colour of the text around it.
struct PlugIcon: View {
    enum Kind: Equatable, Sendable {
        // Pages
        /// A socket.
        case servers
        /// A window.
        case clients
        /// A bell, leaning the way Plug leans.
        case events
        /// A pulse.
        case activity
        /// Two sliders.
        case settings

        // Status
        case working
        /// Plug's three dots.
        case starting
        case needsYou
        /// A key.
        case signIn
        case stopped
        /// A moon: asleep.
        case off
        /// A padlock, for a tool a rule turned off.
        case locked
        /// A call that worked.
        case worked
        /// A call the client gave up on.
        case skipped

        // Actions
        case add
        case dismiss
        case edit
        case more
        case show
        case copy
        case quit
        /// A magnifying glass.
        case search

        // Beside a line that offers a button
        /// A stethoscope, for a checkup.
        case checkup
        /// A plug with its cord loose, for Plug not running.
        case plug
        /// A server's tile with a plus, for adding one.
        case addServer
    }

    let kind: Kind
    var size: CGFloat = 18

    nonisolated init(_ kind: Kind, size: CGFloat = 18) {
        self.kind = kind
        self.size = size
    }

    var body: some View {
        let pen = StrokeStyle(lineWidth: 1.6 * max(1, size / 18), lineCap: .round, lineJoin: .round)
        ZStack {
            Mark(kind: kind, part: .faint).stroke(style: pen).opacity(0.28)
            Mark(kind: kind, part: .lines).stroke(style: pen)
            Mark(kind: kind, part: .dots).fill()
        }
        .frame(width: size, height: size)
    }

    /// Drawn on a 16 by 16 grid.
    private struct Mark: Shape {
        enum Part { case lines, dots, faint }

        let kind: Kind
        let part: Part

        func path(in rect: CGRect) -> Path {
            var path = Path()
            switch part {
            case .lines: lines(&path)
            case .dots: dots(&path)
            case .faint: if kind == .working { path.circle(8, 8, 6) }
            }
            return path.applying(CGAffineTransform(scaleX: rect.width / 16, y: rect.height / 16))
        }

        private func dots(_ path: inout Path) {
            switch kind {
            case .settings:
                path.circle(10.2, 5.3, 2)
                path.circle(5.8, 10.7, 2)
            case .working:
                path.circle(8, 8, 3.4)
            case .starting:
                for x in [3.6, 8, 12.4] { path.circle(x, 8, 1.3) }
            case .needsYou:
                path.circle(8, 10.9, 0.9)
            case .more:
                for x in [3.6, 8, 12.4] { path.circle(x, 8, 1.2) }
            default:
                break
            }
        }

        private func lines(_ path: inout Path) {
            switch kind {
            case .servers:
                path.tile(2.5, 2.5, 11, 11, 3.4)
                path.line(6.2, 6.3, 6.2, 9.7)
                path.line(9.8, 6.3, 9.8, 9.7)
            case .clients:
                path.tile(2, 3, 12, 10, 3)
                path.line(4.8, 6, 7.4, 6)
            case .events:
                var bell = Path()
                bell.move(to: CGPoint(x: 4.4, y: 11))
                bell.addLine(to: CGPoint(x: 4.4, y: 7.4))
                bell.addRelativeArc(center: CGPoint(x: 8, y: 7.4), radius: 3.6, startAngle: .degrees(180), delta: .degrees(180))
                bell.addLine(to: CGPoint(x: 11.6, y: 11))
                bell.addLine(to: CGPoint(x: 12.6, y: 12.3))
                bell.addLine(to: CGPoint(x: 3.4, y: 12.3))
                bell.closeSubpath()
                bell.line(7, 14.4, 9, 14.4)
                let lean = CGAffineTransform(translationX: 8, y: 8)
                    .rotated(by: -10 * .pi / 180)
                    .translatedBy(x: -8, y: -8)
                path.addPath(bell.applying(lean))
            case .activity:
                path.addLines([
                    CGPoint(x: 2, y: 8.4), CGPoint(x: 4.8, y: 8.4), CGPoint(x: 6.4, y: 4.4),
                    CGPoint(x: 9.6, y: 12), CGPoint(x: 11.2, y: 8.4), CGPoint(x: 14, y: 8.4),
                ])
            case .settings:
                path.line(2.5, 5.3, 13.5, 5.3)
                path.line(2.5, 10.7, 13.5, 10.7)
            case .working, .starting, .more:
                break
            case .needsYou:
                path.circle(8, 8, 5.6)
                path.line(8, 5, 8, 8.4)
            case .signIn:
                path.circle(5.4, 8, 2.7)
                path.line(8.1, 8, 13.6, 8)
                path.line(11.4, 8, 11.4, 10.3)
                path.line(13.6, 8, 13.6, 9.7)
            case .stopped:
                path.circle(8, 8, 5.6)
                path.line(6, 6, 10, 10)
                path.line(10, 6, 6, 10)
            case .off:
                path.addRelativeArc(center: CGPoint(x: 7.52, y: 8.48), radius: 5.2, startAngle: .degrees(12.46), delta: .degrees(245.08))
                path.addRelativeArc(center: CGPoint(x: 9.5, y: 6.5), radius: 4.38, startAngle: .degrees(225), delta: .degrees(-180))
                path.closeSubpath()
            case .locked:
                path.tile(3.8, 7.2, 8.4, 6.2, 2)
                path.move(to: CGPoint(x: 5.6, y: 7.2))
                path.addLine(to: CGPoint(x: 5.6, y: 5.4))
                path.addRelativeArc(center: CGPoint(x: 8, y: 5.4), radius: 2.4, startAngle: .degrees(180), delta: .degrees(180))
                path.addLine(to: CGPoint(x: 10.4, y: 7.2))
            case .worked:
                path.circle(8, 8, 5.6)
                path.addLines([CGPoint(x: 5.4, y: 8.2), CGPoint(x: 7.2, y: 10), CGPoint(x: 10.6, y: 6.2)])
            case .skipped:
                path.circle(8, 8, 5.6)
                path.line(5.8, 8, 10.2, 8)
            case .add:
                path.line(8, 3.5, 8, 12.5)
                path.line(3.5, 8, 12.5, 8)
            case .dismiss:
                path.line(4.6, 4.6, 11.4, 11.4)
                path.line(11.4, 4.6, 4.6, 11.4)
            case .edit:
                path.move(to: CGPoint(x: 3.2, y: 12.8))
                path.addLine(to: CGPoint(x: 3.9, y: 9.8))
                path.addLine(to: CGPoint(x: 10.4, y: 3.3))
                path.addRelativeArc(center: CGPoint(x: 11.55, y: 4.45), radius: 1.63, startAngle: .degrees(225), delta: .degrees(180))
                path.addLine(to: CGPoint(x: 6.2, y: 12.1))
                path.closeSubpath()
            case .show:
                path.line(3.5, 8, 12.5, 8)
                path.addLines([CGPoint(x: 9, y: 4.5), CGPoint(x: 12.5, y: 8), CGPoint(x: 9, y: 11.5)])
            case .copy:
                path.tile(5.6, 5.6, 7.9, 7.9, 2.3)
                path.move(to: CGPoint(x: 2.8, y: 10.2))
                path.addLine(to: CGPoint(x: 2.8, y: 5))
                path.addRelativeArc(center: CGPoint(x: 5, y: 5), radius: 2.2, startAngle: .degrees(180), delta: .degrees(90))
                path.addLine(to: CGPoint(x: 10.2, y: 2.8))
            case .quit:
                path.addRelativeArc(center: CGPoint(x: 8, y: 8.33), radius: 5.2, startAngle: .degrees(229.17), delta: .degrees(-278.34))
                path.line(8, 2.4, 8, 7.4)
            case .search:
                path.circle(7, 7, 4.3)
                path.line(10.2, 10.2, 13.4, 13.4)
            case .checkup:
                path.move(to: CGPoint(x: 3.5, y: 2.5))
                path.addLine(to: CGPoint(x: 3.5, y: 6.5))
                path.addRelativeArc(center: CGPoint(x: 6, y: 6.5), radius: 2.5, startAngle: .degrees(180), delta: .degrees(-180))
                path.addLine(to: CGPoint(x: 8.5, y: 2.5))
                path.move(to: CGPoint(x: 6, y: 9))
                path.addLine(to: CGPoint(x: 6, y: 10.5))
                path.addRelativeArc(center: CGPoint(x: 9.25, y: 10.5), radius: 3.25, startAngle: .degrees(180), delta: .degrees(-180))
                path.addLine(to: CGPoint(x: 12.5, y: 9.6))
                path.circle(12.5, 7.7, 1.8)
            case .plug:
                path.line(6.2, 2.5, 6.2, 5.5)
                path.line(9.8, 2.5, 9.8, 5.5)
                path.move(to: CGPoint(x: 4.5, y: 5.5))
                path.addLine(to: CGPoint(x: 11.5, y: 5.5))
                path.addLine(to: CGPoint(x: 11.5, y: 7.5))
                path.addRelativeArc(center: CGPoint(x: 8, y: 7.5), radius: 3.5, startAngle: .degrees(0), delta: .degrees(180))
                path.closeSubpath()
                path.move(to: CGPoint(x: 8, y: 11))
                path.addLine(to: CGPoint(x: 8, y: 11.8))
                path.addCurve(
                    to: CGPoint(x: 4.8, y: 14),
                    control1: CGPoint(x: 8, y: 13.4), control2: CGPoint(x: 4.8, y: 12.2)
                )
            case .addServer:
                path.tile(2.5, 2.5, 11, 11, 3.4)
                path.line(8, 5.8, 8, 10.2)
                path.line(5.8, 8, 10.2, 8)
            }
        }
    }
}

private extension Path {
    mutating func line(_ x1: CGFloat, _ y1: CGFloat, _ x2: CGFloat, _ y2: CGFloat) {
        move(to: CGPoint(x: x1, y: y1))
        addLine(to: CGPoint(x: x2, y: y2))
    }

    mutating func circle(_ x: CGFloat, _ y: CGFloat, _ radius: CGFloat) {
        addEllipse(in: CGRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2))
    }

    mutating func tile(_ x: CGFloat, _ y: CGFloat, _ width: CGFloat, _ height: CGFloat, _ corner: CGFloat) {
        addRoundedRect(in: CGRect(x: x, y: y, width: width, height: height), cornerSize: CGSize(width: corner, height: corner))
    }
}

extension Label where Title == Text, Icon == PlugIcon {
    /// A label with one of Plug's own icons.
    init(_ title: String, icon: PlugIcon.Kind) {
        self.init { Text(title) } icon: { PlugIcon(icon) }
    }
}
