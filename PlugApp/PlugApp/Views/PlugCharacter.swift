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

/// Plug's mark, with a face for each thing Plug can be: awake, working,
/// worried, asleep and the rest of `Mood`. Going from one face to the next it
/// changes shape on springs; nothing is swapped. The prongs move like ears,
/// and the two prongs and the body can pull apart into three dots, an
/// exclamation mark, or one dot. A small mark beside it, a dot, a "?" or a
/// "!", says there is something to look at.
/// Cheered, it hops and sparks. Clicked, it winks, and clicked again it goes
/// through every face it has. With Reduce Motion on it holds still in the
/// face for its state.
/// It takes the foreground style it is given. Given a `mark` colour too, the
/// mark beside it takes that colour and the "z"s go grey, so the character
/// itself can stay Plug blue while the mark says how bad things are.
struct PlugCharacter: View {
    enum Mood: Equatable, Sendable, CaseIterable {
        /// Everything is working.
        case awake
        /// Something just came right.
        case happy
        /// A first: a server, a client, a tool call, a step done.
        case cheering
        /// It was clicked.
        case wink
        /// Waiting for a first something, on a page with nothing on it yet.
        case curious
        case surprised
        /// Starting or busy.
        case working
        /// Something is taking a while.
        case thinking
        /// A page is loading: the three parts are three dots.
        case loading
        /// Sign in, or allow something.
        case needsYou
        /// Some of it works and some does not.
        case unsure
        /// Several things are wrong at once: its eyes are swirls.
        case dizzy
        /// Something stopped.
        case worried
        /// Plug cannot go on: the three parts are an exclamation mark.
        case alert
        /// Plug itself is stopped: its eyes are crosses.
        case out
        /// Plug is off.
        case asleep
        /// Out of the way: the three parts are one dot.
        case tucked

        /// The face for a tone when nothing says more.
        init(_ tone: Verdict.Tone) {
            switch tone {
            case .good: self = .awake
            case .quiet: self = .asleep
            case .busy: self = .working
            case .attention: self = .needsYou
            case .blocked: self = .alert
            }
        }

        /// Coming out of one of these into all-is-well is a relief.
        fileprivate var isTrouble: Bool {
            switch self {
            case .working, .thinking, .needsYou, .unsure, .dizzy, .worried, .alert, .out: true
            default: false
            }
        }

        fileprivate var blinks: Bool {
            switch self {
            case .awake, .happy, .curious, .thinking, .needsYou: true
            default: false
            }
        }
    }

    let mood: Mood
    /// Each time this goes up, the character cheers: it hops and sparks fly
    /// off its prongs, as when a step is done.
    var cheers = 0
    /// The colour of the mark beside the character, when it is not the
    /// character's own.
    var mark: Color?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var puppet: PlugPuppet
    /// Nothing is moving, so nothing is redrawn.
    @State private var idle = false

    init(mood: Mood, cheers: Int = 0, mark: Color? = nil) {
        self.mood = mood
        self.cheers = cheers
        self.mark = mark
        _puppet = State(initialValue: PlugPuppet(mood))
    }

    var body: some View {
        GeometryReader { proxy in
            let side = min(proxy.size.width, proxy.size.height)
            TimelineView(.animation(paused: idle || reduceMotion)) { timeline in
                let frame = puppet.frame(at: timeline.date, moving: !reduceMotion)
                Canvas { context, size in
                    PlugDrawing.draw(frame, side: side, mark: mark, in: &context, size: size)
                }
            }
            // Room around the character for a hop, the sparks and the "z"s.
            .frame(width: side * PlugDrawing.room, height: side * PlugDrawing.room)
            .position(x: proxy.size.width / 2, y: proxy.size.height / 2)
            .allowsHitTesting(false)
        }
        .task(id: reduceMotion) {
            while !Task.isCancelled {
                let resting = reduceMotion || puppet.isResting
                if resting != idle { idle = resting }
                try? await Task.sleep(for: .seconds(PlugPuppet.lookAhead * 0.8))
            }
        }
        .onChange(of: mood) { _, new in
            puppet.show(new, moving: !reduceMotion)
            idle = false
        }
        .onChange(of: cheers) { old, new in
            guard new > old, !reduceMotion else { return }
            puppet.cheer()
            idle = false
        }
        .contentShape(Rectangle())
        .onTapGesture {
            guard !reduceMotion else { return }
            puppet.tap()
            idle = false
        }
        .accessibilityHidden(true)
    }
}

/// Where every part of the character is, for one face at one moment.
struct PlugPose: Equatable, Sendable {
    enum Part: Int, CaseIterable, Sendable {
        case lean, lift, stretchX, stretchY
        case leftProngX, leftProngY, leftProngWidth, leftProngHeight, leftProngTurn, leftProngShown
        case rightProngX, rightProngY, rightProngWidth, rightProngHeight, rightProngTurn, rightProngShown
        case bodyX, bodyY, bodyWidth, bodyHeight, bodyCorner, bodyShown
        /// The eyes shrink to nothing when the parts stop being a face.
        case eyeScale, gazeX, gazeY
        case leftEyeWidth, leftEyeHeight, leftLid, leftLidTurn, leftCheek
        case rightEyeWidth, rightEyeHeight, rightLid, rightLidTurn, rightCheek
        case badge, snore
        /// How far each eye has crossed into an "x", and how much of a swirl
        /// has drawn itself around each.
        case cross, swirl
        /// The "?" and the "!" beside the character.
        case question, bang
        /// How far down the lean turns from: the middle, or the feet.
        case pivot
    }

    fileprivate var values: [CGFloat]

    subscript(part: Part) -> CGFloat {
        get { values[part.rawValue] }
        set { values[part.rawValue] = newValue }
    }

    static let still = PlugPose(values: Array(repeating: 0, count: Part.allCases.count))

    /// The mark as `docs/assets/plug-icon.svg` draws it.
    static let rest = still.with([
        .lean: PlugMark.lean, .stretchX: 1, .stretchY: 1,
        .leftProngX: 46, .leftProngY: 35.5, .leftProngWidth: 14, .leftProngHeight: 37, .leftProngShown: 1,
        .rightProngX: 74, .rightProngY: 35.5, .rightProngWidth: 14, .rightProngHeight: 37, .rightProngShown: 1,
        .bodyX: 60, .bodyY: 67, .bodyWidth: 72, .bodyHeight: 54, .bodyCorner: PlugMark.bodyCorner, .bodyShown: 1,
        .eyeScale: 1, .pivot: 55.5,
        .leftEyeWidth: 9.24, .leftEyeHeight: 22.28, .rightEyeWidth: 9.24, .rightEyeHeight: 22.28,
    ])

    func with(_ changes: [Part: CGFloat]) -> PlugPose {
        var pose = self
        for (part, value) in changes { pose[part] = value }
        return pose
    }

    /// Prongs of these lengths, standing on the body and leaning apart.
    func prongs(_ left: CGFloat, _ right: CGFloat, apart: CGFloat = 0) -> PlugPose {
        with([
            .leftProngHeight: left, .leftProngY: 54 - left / 2, .leftProngTurn: -apart,
            .rightProngHeight: right, .rightProngY: 54 - right / 2, .rightProngTurn: apart,
        ])
    }

    /// Both eyes the same.
    func eyes(
        width: CGFloat? = nil, height: CGFloat? = nil, lid: CGFloat? = nil, cheek: CGFloat? = nil
    ) -> PlugPose {
        var pose = self
        if let width { pose[.leftEyeWidth] = width; pose[.rightEyeWidth] = width }
        if let height { pose[.leftEyeHeight] = height; pose[.rightEyeHeight] = height }
        if let lid { pose[.leftLid] = lid; pose[.rightLid] = lid }
        if let cheek { pose[.leftCheek] = cheek; pose[.rightCheek] = cheek }
        return pose
    }

    /// The three parts as three round things of one size.
    func dots(size: CGFloat, left: CGPoint, body: CGPoint, right: CGPoint, stem: CGFloat? = nil) -> PlugPose {
        with([
            .eyeScale: 0,
            .leftProngX: left.x, .leftProngY: left.y, .leftProngWidth: size, .leftProngHeight: stem ?? size,
            .rightProngX: right.x, .rightProngY: right.y, .rightProngWidth: size, .rightProngHeight: stem ?? size,
            .bodyX: body.x, .bodyY: body.y, .bodyWidth: size, .bodyHeight: size, .bodyCorner: size / 2,
        ])
    }

    /// The face for `mood`, `time` seconds after it began. `moving` is false
    /// for the one pose a face holds when nothing may move.
    static func pose(for mood: PlugCharacter.Mood, at time: TimeInterval, moving: Bool) -> PlugPose {
        let t = moving ? CGFloat(time) : 0
        /// True for `lasts` seconds out of every `every`.
        func pulse(_ shift: CGFloat, every: CGFloat, lasts: CGFloat) -> Bool {
            moving && (t + shift).truncatingRemainder(dividingBy: every) < lasts
        }
        switch mood {
        case .awake:
            guard pulse(2, every: 7, lasts: 1.2) else { return rest }
            return rest.with([.gazeX: Int(t / 7) % 2 == 0 ? -3.5 : 3.5])
        case .happy:
            return rest.eyes(height: 20, cheek: 0.6).prongs(41, 41)
                .with([.lift: moving ? -0.025 * max(0, sin(t * 5)) : 0])
        case .cheering:
            return rest.eyes(height: 20, cheek: 0.7).prongs(44, 44, apart: 5)
        case .wink:
            return rest.prongs(40, 36)
                .with([.rightEyeHeight: 5, .rightEyeWidth: 13, .leftCheek: 0.35, .lean: -6])
        case .curious:
            // Still, it holds the tilt: that is the face.
            guard !moving || pulse(5.5, every: 6, lasts: 1.8) else { return rest }
            return rest.prongs(44, 32).with([.lean: 6, .gazeX: 2])
        case .surprised:
            return rest.eyes(width: 13, height: 24).prongs(46, 46, apart: 4).with([.gazeY: -1])
        case .working:
            // The prongs take turns, and the eyes follow.
            let right = moving && Int(t / 0.8) % 2 == 1
            return rest.prongs(right ? 42 : 33, right ? 33 : 42).with([.gazeX: right ? 3.5 : -3.5])
        case .thinking:
            return rest.eyes(lid: 0.3).prongs(34, 43)
                .with([.lean: -15, .gazeX: 4 + (moving ? sin(t * 1.3) * 1.5 : 0), .gazeY: -4])
        case .loading:
            /// Each dot in turn, a beat after the one before.
            func beat(_ index: CGFloat) -> CGFloat { moving ? max(0, sin(t * 4.2 - index * 0.9)) : 1 }
            return rest.dots(
                size: 17,
                left: CGPoint(x: 36, y: 60 - 5 * beat(0) * beat(0)),
                body: CGPoint(x: 60, y: 60 - 5 * beat(1) * beat(1)),
                right: CGPoint(x: 84, y: 60 - 5 * beat(2) * beat(2))
            ).with([
                .lean: 0,
                .leftProngShown: 0.4 + 0.6 * beat(0), .bodyShown: 0.4 + 0.6 * beat(1),
                .rightProngShown: 0.4 + 0.6 * beat(2),
            ])
        case .needsYou:
            return rest.eyes(width: 14, height: 17).prongs(43, 43).with([.lean: -6, .badge: 1])
        case .unsure:
            return rest.prongs(42, 31)
                .with([
                    .lean: -4, .rightEyeHeight: 5, .rightEyeWidth: 15, .leftEyeHeight: 21, .gazeX: 2, .question: 1,
                ])
        case .dizzy:
            // It sways from its feet, so the whole head goes round.
            return rest.eyes(width: 5, height: 5).prongs(40, 33, apart: 6)
                .with([.swirl: 1, .question: 1, .pivot: 94, .lean: moving ? 7 * sin(t * 1.7) : 0])
        case .worried:
            return rest.eyes(height: 21, lid: 0.34).prongs(31, 31, apart: 9)
                .with([.leftLidTurn: -18, .rightLidTurn: 18, .gazeY: 2, .lean: -13, .badge: 1])
        case .alert:
            let stem = CGPoint(x: 60, y: 42)
            return rest.dots(size: 17, left: stem, body: CGPoint(x: 60, y: 87), right: stem, stem: 50)
                .with([.lean: 7, .lift: pulse(0.3, every: 2.6, lasts: 0.14) ? -0.09 : 0])
        case .out:
            return rest.eyes(width: 6.5, height: 21).prongs(30, 28, apart: 10)
                .with([.cross: 1, .lean: -15, .gazeY: 1, .bang: 1])
        case .asleep:
            let breath = moving ? sin(t * 1.2) : 0
            return rest.eyes(width: 13, height: 5).prongs(31, 29, apart: 3)
                .with([.lean: -18, .snore: 1, .stretchY: 1 + 0.035 * breath, .stretchX: 1 - 0.02 * breath])
        case .tucked:
            let middle = CGPoint(x: 60, y: 60)
            let swell = 1 + (moving ? 0.06 * sin(t * 2.4) : 0)
            return rest.dots(size: 24, left: middle, body: middle, right: middle)
                .with([.lean: 0, .stretchX: swell, .stretchY: swell])
        }
    }
}

/// What to draw this instant.
struct PlugFrame: Sendable {
    var pose: PlugPose
    /// From open (0) to shut (1).
    var blink: CGFloat
    /// From just leaving (0) to gone (1), or nil when there are none.
    var sparks: CGFloat?
    var time: TimeInterval
}

/// Moves the character: each part is on a spring toward where the face of
/// the moment puts it, so a change of face is a change of shape.
@MainActor
final class PlugPuppet {
    /// How far ahead `isResting` looks for the next movement, in seconds.
    static let lookAhead: TimeInterval = 0.25

    private var mood: PlugCharacter.Mood
    private var since: TimeInterval
    /// A face put on for a moment, over the one for the state.
    private var act: (mood: PlugCharacter.Mood, since: TimeInterval, until: TimeInterval)?
    private var pose: PlugPose
    private var speed = PlugPose.still
    private var last: TimeInterval?
    private var sparkedAt = -TimeInterval.infinity
    private var blinkedAt = -TimeInterval.infinity
    private var nextBlink: TimeInterval
    private var tappedAt = -TimeInterval.infinity

    init(_ mood: PlugCharacter.Mood) {
        let now = Self.now
        self.mood = mood
        since = now
        nextBlink = now + 2
        pose = PlugPose.pose(for: mood, at: 0, moving: false)
    }

    private static var now: TimeInterval { Date.now.timeIntervalSinceReferenceDate }
    private static let sparkTime: TimeInterval = 0.6
    private static let blinkTime: TimeInterval = 0.22

    private var showing: PlugCharacter.Mood { act?.mood ?? mood }

    private func goal(at time: TimeInterval, moving: Bool = true) -> PlugPose {
        PlugPose.pose(for: showing, at: time - (act?.since ?? since), moving: moving)
    }

    func show(_ new: PlugCharacter.Mood, moving: Bool) {
        guard new != mood else { return }
        let relieved = new == .awake && mood.isTrouble
        mood = new
        since = Self.now
        act = nil
        guard moving else { return }
        enter(new)
        if relieved {
            perform(.happy, for: 2.4)
            hop(0.8)
        }
    }

    func cheer() { perform(.cheering, for: 1.6) }

    /// A click is a wink. Keep clicking and it goes through every face.
    func tap() {
        let now = Self.now
        let faces = PlugCharacter.Mood.allCases
        if now - tappedAt < 3, let act, let index = faces.firstIndex(of: act.mood) {
            perform(faces[(index + 1) % faces.count], for: 3)
        } else {
            perform(.wink, for: 0.9)
        }
        tappedAt = now
    }

    private func perform(_ mood: PlugCharacter.Mood, for seconds: TimeInterval) {
        let now = Self.now
        act = (mood, now, now + seconds)
        enter(mood)
    }

    /// How a face arrives, beyond changing shape.
    private func enter(_ mood: PlugCharacter.Mood) {
        switch mood {
        case .cheering:
            hop(1.3)
            sparkedAt = Self.now
        case .wink: hop(0.6)
        case .surprised:
            speed[.stretchY] = 1.6
            speed[.stretchX] = -1
            hop(0.5)
        case .needsYou: hop(0.7)
        // A shake of the head.
        case .worried: speed[.lean] = 260
        case .alert: hop(0.9)
        // It keels over.
        case .out:
            speed[.lean] = -200
            speed[.stretchY] = -0.8
        default: break
        }
    }

    private func hop(_ strength: CGFloat) {
        speed[.lift] = -2.1 * strength
        speed[.stretchY] += 1.2 * strength
    }

    /// Stiffness and damping. The lean and the hop are loose, the rest firm.
    private static func spring(_ part: PlugPose.Part) -> (CGFloat, CGFloat) {
        switch part {
        case .lean: (210, 15)
        case .lift: (300, 13)
        case .stretchX, .stretchY: (300, 16)
        default: (190, 23)
        }
    }

    func frame(at date: Date, moving: Bool) -> PlugFrame {
        let now = date.timeIntervalSinceReferenceDate
        // A long gap is a pause, not a leap.
        let step = CGFloat(min(1.0 / 30, max(0, now - (last ?? now))))
        last = now
        if let act, now >= act.until { self.act = nil }
        guard moving else {
            pose = goal(at: now, moving: false)
            speed = .still
            return PlugFrame(pose: pose, blink: 0, sparks: nil, time: now)
        }
        let goal = goal(at: now)
        for part in PlugPose.Part.allCases {
            let (stiffness, damping) = Self.spring(part)
            speed[part] += (stiffness * (goal[part] - pose[part]) - damping * speed[part]) * step
            pose[part] += speed[part] * step
        }
        if showing.blinks, now > nextBlink {
            blinkedAt = now
            nextBlink = now + .random(in: 2.5...6)
        }
        let blink = max(0, 1 - abs((now - blinkedAt) / (Self.blinkTime / 2) - 1))
        let sparks = (now - sparkedAt) / Self.sparkTime
        return PlugFrame(
            pose: pose, blink: blink, sparks: sparks > 0 && sparks < 1 ? sparks : nil, time: now
        )
    }

    /// Nothing is moving and nothing is about to.
    var isResting: Bool {
        let now = Self.now
        guard act == nil, now - sparkedAt > Self.sparkTime, now - blinkedAt > Self.blinkTime,
              !(showing.blinks && now + Self.lookAhead > nextBlink), pose[.snore] < 0.02
        else { return false }
        let goals = [goal(at: now), goal(at: now + Self.lookAhead)]
        return PlugPose.Part.allCases.allSatisfy { part in
            abs(speed[part]) < 0.02 && goals.allSatisfy { abs($0[part] - pose[part]) < 0.005 }
        }
    }
}

/// Draws one frame of the character.
enum PlugDrawing {
    /// How many times the character's own frame the drawing gets.
    static let room: CGFloat = 3

    static func draw(
        _ frame: PlugFrame, side: CGFloat, mark: Color? = nil, in context: inout GraphicsContext, size: CGSize
    ) {
        let pose = frame.pose
        let markShading: GraphicsContext.Shading = mark.map { .color($0) } ?? .foreground
        // The same fit as the still mark, with a little room around it.
        let scale = side / (PlugMark.height + 8)
        var grid = context
        grid.translateBy(x: size.width / 2 - 2.04 * scale, y: size.height / 2 - 0.48 * scale)
        grid.scaleBy(x: scale, y: scale)
        grid.translateBy(x: -60, y: -55.5)

        var whole = grid
        whole.translateBy(x: 0, y: pose[.lift] * 120)
        // It stretches and squashes from its feet.
        whole.translateBy(x: 60, y: 94)
        whole.scaleBy(x: pose[.stretchX], y: pose[.stretchY])
        whole.translateBy(x: -60, y: -94)
        whole.translateBy(x: 60, y: pose[.pivot])
        whole.rotate(by: .degrees(pose[.lean]))
        whole.translateBy(x: -60, y: -pose[.pivot])

        let badge = CGPoint(x: 98, y: 38)
        let badgeSize = max(0, 10 * pose[.badge])
        whole.drawLayer { layer in
            for part in parts(of: pose) {
                layer.opacity = min(1, max(0, part.shown))
                layer.fill(part.path, with: .foreground)
            }
            layer.opacity = 1
            layer.blendMode = .destinationOut
            // One at a time, so shapes that overlap never cancel out.
            for eye in eyes(of: pose, blink: frame.blink, time: frame.time) {
                layer.fill(eye, with: .color(.black))
            }
        }
        // The dot that says "look here" sits a little apart from the
        // character.
        if badgeSize > 0.5 { whole.fill(circle(at: badge, radius: badgeSize - 2), with: markShading) }
        let sign = max(pose[.question], pose[.bang])
        if sign > 0.05 {
            whole.drawLayer { layer in
                layer.draw(
                    Text(pose[.question] >= pose[.bang] ? "?" : "!")
                        .font(.system(size: 40 * sign, weight: .black, design: .rounded)),
                    at: CGPoint(x: 99, y: 26)
                )
                layer.blendMode = .sourceIn
                layer.fill(Path(CGRect(x: 60, y: -20, width: 80, height: 90)), with: markShading)
            }
        }

        if let sparks = frame.sparks { grid.fill(Self.sparks(sparks), with: .foreground) }
        if pose[.snore] > 0.02 {
            for index in [0.0, 1] {
                // Each "z" drifts up and away, growing, and fades at both ends.
                let drift = CGFloat((frame.time * 0.35 + index * 0.5).truncatingRemainder(dividingBy: 1))
                grid.opacity = pose[.snore] * sin(drift * .pi)
                grid.fill(
                    letterZ(at: CGPoint(x: 92 + drift * 14 + index * 4, y: 30 - drift * 26), size: 7 + drift * 6),
                    with: mark == nil ? .foreground : .color(.secondary)
                )
            }
        }
    }

    private static func circle(at centre: CGPoint, radius: CGFloat) -> Path {
        Path(ellipseIn: CGRect(x: centre.x - radius, y: centre.y - radius, width: radius * 2, height: radius * 2))
    }

    /// The two prongs and the body, each a rounded bar.
    private static func parts(of pose: PlugPose) -> [(path: Path, shown: CGFloat)] {
        func bar(
            _ x: PlugPose.Part, _ y: PlugPose.Part, _ width: PlugPose.Part, _ height: PlugPose.Part,
            corner: CGFloat, turn: CGFloat
        ) -> Path {
            let width = max(0, pose[width]), height = max(0, pose[height])
            let radius = min(corner, width / 2, height / 2)
            // A prong turns where it meets the body.
            let foot = CGPoint(x: pose[x], y: pose[y] + height / 2)
            return Path(
                roundedRect: CGRect(x: pose[x] - width / 2, y: pose[y] - height / 2, width: width, height: height),
                cornerRadius: radius
            )
            .applying(
                CGAffineTransform(translationX: foot.x, y: foot.y)
                    .rotated(by: turn * .pi / 180)
                    .translatedBy(x: -foot.x, y: -foot.y)
            )
        }
        return [
            (bar(.leftProngX, .leftProngY, .leftProngWidth, .leftProngHeight,
                 corner: .infinity, turn: pose[.leftProngTurn]), pose[.leftProngShown]),
            (bar(.rightProngX, .rightProngY, .rightProngWidth, .rightProngHeight,
                 corner: .infinity, turn: pose[.rightProngTurn]), pose[.rightProngShown]),
            (bar(.bodyX, .bodyY, .bodyWidth, .bodyHeight, corner: pose[.bodyCorner], turn: 0), pose[.bodyShown]),
        ]
    }

    /// The eyes, cut out of the body. A lid comes down from above and a cheek
    /// pushes up from below, which is all the expression they have.
    /// Crossed, each eye is itself twice, turned one way and the other.
    private static func eyes(of pose: PlugPose, blink: CGFloat, time: TimeInterval) -> [Path] {
        // They shrink with the body, so they are gone before it is a dot.
        let scale = max(0, pose[.eyeScale]) * min(1, pose[.bodyWidth] / 72)
        var paths: [Path] = []
        guard scale > 0.02 else { return paths }
        let cross = pose[.cross] * .pi / 4
        let drawn = min(1, pose[.swirl])
        let spin = time.truncatingRemainder(dividingBy: 2.4) * 150
        let sides: [(CGFloat, PlugPose.Part, PlugPose.Part, PlugPose.Part, PlugPose.Part, PlugPose.Part)] = [
            (-8.5, .leftEyeWidth, .leftEyeHeight, .leftLid, .leftLidTurn, .leftCheek),
            (16.5, .rightEyeWidth, .rightEyeHeight, .rightLid, .rightLidTurn, .rightCheek),
        ]
        for (offset, width, height, lid, lidTurn, cheek) in sides {
            let width = max(0, pose[width] * scale)
            let height = max(4 * scale, pose[height] * scale * (1 - 0.85 * blink))
            var eye = Path(
                roundedRect: CGRect(x: -width / 2, y: -height / 2, width: width, height: height),
                cornerRadius: min(width, height) / 2
            )
            if pose[lid] > 0.001 {
                let edge = -height / 2 - 2 + pose[lid] * (height + 2)
                eye = eye.subtracting(
                    Path(CGRect(x: -40, y: edge - 60, width: 80, height: 60)).applying(
                        CGAffineTransform(translationX: 0, y: edge)
                            .rotated(by: pose[lidTurn] * .pi / 180)
                            .translatedBy(x: 0, y: -edge)
                    )
                )
            }
            if pose[cheek] > 0.001 {
                let radius = width * 1.4
                let middle = height / 2 + radius + 2 - pose[cheek] * (height * 0.55 + 2)
                eye = eye.subtracting(circle(at: CGPoint(x: 0, y: middle), radius: radius))
            }
            let place = CGAffineTransform(
                translationX: pose[.bodyX] + (offset + pose[.gazeX]) * scale,
                y: pose[.bodyY] - 2 + pose[.gazeY] * scale
            )
            if abs(cross) > 0.01 {
                paths.append(eye.applying(place.rotated(by: cross)))
                paths.append(eye.applying(place.rotated(by: -cross)))
            } else {
                paths.append(eye.applying(place))
            }
            if drawn > 0.02 {
                // Both swirls turn the same way at the same speed, the right
                // one a little ahead.
                let turn = ((offset < 0 ? 0 : 160) - spin) * .pi / 180
                paths.append(
                    swirl.trimmedPath(from: 0, to: drawn)
                        .strokedPath(StrokeStyle(lineWidth: 4.4, lineCap: .round, lineJoin: .round))
                        .applying(place.scaledBy(x: scale, y: scale).rotated(by: turn))
                )
            }
        }
        return paths
    }

    /// One and a bit turns out from the middle, ending on a round edge.
    private static let swirl: Path = {
        var path = Path()
        path.move(to: .zero)
        for step in 1...40 {
            let angle = CGFloat(step) / 40 * .pi * 3.4
            let radius = 10 * min(1, angle / (.pi * 2.9))
            path.addLine(to: CGPoint(x: radius * cos(angle), y: radius * sin(angle)))
        }
        return path
    }()

    /// What flies off the prongs when the character is cheered: five short
    /// bars that leave, stretch, and are gone.
    private static func sparks(_ progress: CGFloat) -> Path {
        let eased = 1 - (1 - progress) * (1 - progress)
        let size = PlugMark.height + 8
        let near = size * (0.5 + 0.28 * eased)
        let far = near + size * 0.16 * sin(eased * .pi)
        let centre = CGPoint(x: 62.04, y: 55.98)
        var path = Path()
        // Fanned around the way the prongs point.
        for degrees in [-60.0, -30, 0, 30, 60] {
            let angle = (degrees + PlugMark.lean - 90) * .pi / 180
            path.move(to: CGPoint(x: centre.x + cos(angle) * near, y: centre.y + sin(angle) * near))
            path.addLine(to: CGPoint(x: centre.x + cos(angle) * far, y: centre.y + sin(angle) * far))
        }
        return path.strokedPath(StrokeStyle(lineWidth: size * 0.07 * (1 - 0.5 * eased), lineCap: .round))
    }

    private static func letterZ(at corner: CGPoint, size: CGFloat) -> Path {
        var path = Path()
        path.move(to: CGPoint(x: corner.x, y: corner.y - size))
        path.addLine(to: CGPoint(x: corner.x + size, y: corner.y - size))
        path.addLine(to: CGPoint(x: corner.x, y: corner.y))
        path.addLine(to: CGPoint(x: corner.x + size, y: corner.y))
        return path.strokedPath(StrokeStyle(lineWidth: size * 0.26, lineCap: .round, lineJoin: .round))
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
