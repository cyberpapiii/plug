import SwiftUI

/// The shapes of Plug's mark, the plug with a face, in the units of
/// `docs/assets/plug-icon.svg`. The menu bar icon and the moving character
/// are both drawn from these.
enum PlugMark {
    /// How far the mark leans to the left, in degrees.
    static let lean: CGFloat = -10
    /// How tall the leaning mark is.
    static let height: CGFloat = 81.7
    static let body = CGRect(x: 24, y: 40, width: 72, height: 54)
    static let bodyCorner: CGFloat = 18

    static var silhouette: Path {
        var path = Path()
        for x in [39.0, 67.0] {
            path.addRoundedRect(in: CGRect(x: x, y: 17, width: 14, height: 37), cornerSize: CGSize(width: 7, height: 7))
        }
        path.addRoundedRect(in: body, cornerSize: CGSize(width: bodyCorner, height: bodyCorner))
        return path
    }

    /// The eyes, from wide open (1) to shut (0), looking `gaze` units to one
    /// side. Shut, each is a line `lid` units thick.
    static func eyes(open: CGFloat, gaze: CGFloat = 0, lid: CGFloat = 5) -> Path {
        let height = max(lid, 22.28 * open)
        let width = 9.24 + 3.76 * (1 - open)
        let corner = min(width, height) / 2
        var path = Path()
        for middle in [51.5, 76.5] {
            path.addRoundedRect(
                in: CGRect(x: middle + gaze - width / 2, y: 65 - height / 2, width: width, height: height),
                cornerSize: CGSize(width: corner, height: corner)
            )
        }
        return path
    }

    /// Puts the mark's middle at `centre`, `scale` points to a unit. `yUp` is
    /// for AppKit drawing, where y grows upward.
    static func placement(
        centre: CGPoint,
        scale: CGFloat,
        lean: CGFloat = lean,
        yUp: Bool = false
    ) -> CGAffineTransform {
        CGAffineTransform(translationX: centre.x - 2.04 * scale, y: centre.y + (yUp ? 0.48 : -0.48) * scale)
            .scaledBy(x: scale, y: yUp ? -scale : scale)
            .rotated(by: lean * .pi / 180)
            .translatedBy(x: -60, y: -55.5)
    }
}

/// Plug's mark, moving the way its state reads. It blinks and glances about
/// when all is well, looks from side to side while it works, shakes its head
/// once when something is wrong, hops when that passes, and dozes when Plug
/// is off. With Reduce Motion on it holds still. It takes the foreground
/// style it is given.
struct PlugCharacter: View {
    enum Mood: Equatable {
        case awake, working, troubled, asleep

        init(_ tone: Verdict.Tone) {
            switch tone {
            case .good: self = .awake
            case .quiet: self = .asleep
            case .busy: self = .working
            case .attention, .blocked: self = .troubled
            }
        }
    }

    let mood: Mood

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var open: CGFloat = 1
    @State private var gaze: CGFloat = 0
    @State private var lean = PlugMark.lean
    @State private var breathing = false
    @State private var lift: CGFloat = 0
    @State private var shown: Mood?

    var body: some View {
        ZStack {
            Part(eyes: false, open: open, gaze: gaze, lean: lean)
            Part(eyes: true, open: open, gaze: gaze, lean: lean)
                .blendMode(.destinationOut)
        }
        .compositingGroup()
        .scaleEffect(breathing ? 1.06 : 1)
        .visualEffect { [lift] content, proxy in content.offset(y: lift * proxy.size.height) }
        .task(id: Script(mood: mood, still: reduceMotion)) { try? await perform() }
        .accessibilityHidden(true)
    }

    private struct Script: Equatable {
        let mood: Mood
        let still: Bool
    }

    private func perform() async throws {
        let asleep = mood == .asleep
        // Coming out of work or trouble into all-is-well earns a small hop.
        let relieved = mood == .awake && (shown == .working || shown == .troubled)
        shown = mood
        withAnimation(reduceMotion ? nil : .smooth(duration: 0.4)) {
            open = asleep ? 0 : 1
            gaze = 0
            lean = asleep ? -18 : PlugMark.lean
            breathing = false
            lift = 0
        }
        guard !reduceMotion else { return }
        if relieved {
            withAnimation(.easeOut(duration: 0.14)) { lift = -0.12 }
            try await pause(0.14)
            withAnimation(.spring(duration: 0.4, bounce: 0.55)) { lift = 0 }
        }
        switch mood {
        case .asleep:
            withAnimation(.easeInOut(duration: 2.6).repeatForever(autoreverses: true)) { breathing = true }
        case .working:
            while true {
                for side in [3.5, -3.5] {
                    withAnimation(.easeInOut(duration: 0.5)) { gaze = side }
                    try await pause(0.8)
                }
            }
        case .troubled:
            try await pause(0.4)
            for angle in [-17.0, -3, -15, PlugMark.lean] {
                withAnimation(.easeInOut(duration: 0.11)) { lean = angle }
                try await pause(0.11)
            }
            while true {
                try await pause(.random(in: 3...6))
                try await blink()
            }
        case .awake:
            while true {
                try await pause(.random(in: 2.5...6))
                try await blink()
                guard Int.random(in: 0..<3) == 0 else { continue }
                try await pause(0.5)
                withAnimation(.smooth(duration: 0.3)) { gaze = Bool.random() ? 3.5 : -3.5 }
                try await pause(1.1)
                withAnimation(.smooth(duration: 0.3)) { gaze = 0 }
            }
        }
    }

    private func blink() async throws {
        withAnimation(.easeIn(duration: 0.07)) { open = 0 }
        try await pause(0.11)
        withAnimation(.easeOut(duration: 0.12)) { open = 1 }
    }

    private func pause(_ seconds: Double) async throws {
        try await Task.sleep(for: .seconds(seconds))
    }

    /// The silhouette or the eyes, as one shape whose face can move.
    private struct Part: Shape {
        let eyes: Bool
        var open: CGFloat
        var gaze: CGFloat
        var lean: CGFloat

        var animatableData: AnimatablePair<CGFloat, AnimatablePair<CGFloat, CGFloat>> {
            get { AnimatablePair(open, AnimatablePair(gaze, lean)) }
            set {
                open = newValue.first
                gaze = newValue.second.first
                lean = newValue.second.second
            }
        }

        func path(in rect: CGRect) -> Path {
            // A little room, so a head shake stays inside the frame.
            let scale = min(rect.width, rect.height) / (PlugMark.height + 8)
            let placement = PlugMark.placement(
                centre: CGPoint(x: rect.midX, y: rect.midY), scale: scale, lean: lean
            )
            return (eyes ? PlugMark.eyes(open: open, gaze: gaze) : PlugMark.silhouette).applying(placement)
        }
    }
}

/// The heading of a page where Plug speaks about itself: the character, then
/// the words.
struct PlugCharacterLabel: View {
    let title: String
    let mood: PlugCharacter.Mood

    var body: some View {
        Label {
            Text(title)
        } icon: {
            PlugCharacter(mood: mood).frame(width: 52, height: 52)
        }
    }
}
