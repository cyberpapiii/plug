/// The face Plug's character wears.
enum PlugMood: Equatable, Sendable, CaseIterable {
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
}
