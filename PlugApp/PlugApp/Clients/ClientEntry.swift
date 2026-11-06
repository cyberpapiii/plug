import PlugIPC
import SwiftUI

/// The one line under a client's name in its detail.
struct ClientStatus: Equatable, Sendable {
    let text: String

    static func hasNetworkRepresentation(_ app: LinkableApp, grantNames: [String]) -> Bool {
        app.linked && app.transport?.lowercased() == "http"
            && grantNames.contains { AppIcons.target(forClientType: $0) == app.target }
    }

    /// A state, followed by what is off for the client when something is.
    init(state: String, limit: String?) {
        text = limit.map { "\(state) · \($0)" } ?? state
    }

    /// The state of a client with this many open connections.
    static func connected(_ connections: Int) -> String {
        connections == 1 ? "Connected" : "\(connections) connections"
    }

    /// A client allowed in over the network. One with no sign-in left says
    /// so, because nothing else about it looks different.
    static func grant(connections: Int, needsSignIn: Bool, limit: String?) -> ClientStatus {
        let state = if connections > 0 {
            connected(connections)
        } else if needsSignIn {
            "Needs sign-in"
        } else {
            "Allowed over the network"
        }
        return ClientStatus(state: state, limit: limit)
    }

    /// A client on this Mac: connected, ready, or not using Plug.
    static func app(_ app: LinkableApp, connections: Int, limit: String?) -> ClientStatus {
        let state: String
        if connections > 0 {
            state = connected(connections)
        } else if !app.detected {
            state = "Not found on this Mac"
        } else if !app.linked {
            state = "No local Plug configuration"
        } else {
            state = app.transport?.lowercased() == "http" ? "Uses Plug over the network" : "Not open"
        }
        return ClientStatus(state: state, limit: limit)
    }
}

/// One client as the list and the detail show it.
struct ClientEntry: Identifiable {
    /// Where a client is. The title is also what a connection row says.
    enum Group: String {
        case onThisMac = "On This Mac"
        case network = "Over the Network"
        case notUsing = "No Local Plug Configuration"
        case unfinished = "Unfinished Sign-Ins"
    }

    enum Glyph: Equatable, Sendable {
        case app(target: String, name: String, appPath: String?)
        /// A client allowed in over the network, by the name it gave.
        case grant(name: String)

        /// The name a chosen icon for this client is kept under.
        var iconKey: String {
            switch self {
            case let .app(target, _, _): IconStore.key(client: target)
            case let .grant(name): IconStore.key(client: AppIcons.target(forClientType: name))
            }
        }
    }

    /// A new sign-in that gave a name earlier ones gave, and the question
    /// that settles which client it is.
    struct Asking {
        struct Choice: Identifiable {
            /// The earlier sign-in's client id.
            let id: String
            let name: String
        }

        /// The day it signed in, as the row says it.
        let when: String
        let choices: [Choice]
        /// It is this earlier client, signed in again.
        let replace: (String) -> Void
        /// It is a client of its own.
        let keep: () -> Void
    }

    let id: String
    let group: Group
    /// Where it runs, which is the section it shows under.
    let place: String
    let name: String
    /// What the client is called when the owner has not named it.
    let originalName: String
    let status: ClientStatus
    let isLive: Bool
    let dimmed: Bool
    let glyph: Glyph
    let isBusy: Bool
    let switchLabel: String
    /// Nil when this client has nothing to switch.
    let isOn: Bool?
    let setOn: (Bool) -> Void
    let about: String
    /// Nil when there are no server choices to offer for this client.
    let access: ClientAccess?
    /// Nil when Plug has nothing to store a name under.
    let renameKey: String?
    /// The key its place is stored under. An app set up on this Mac has one
    /// even when it reaches Plug over the network and cannot be renamed.
    let placeKey: String?
    /// The owner has said where this client runs.
    var hasOwnPlace: Bool { place != group.rawValue }
    /// A few words that tell this client from another with the same name.
    /// Nil when its name is the only one like it.
    var disambiguator: String?
    /// Set for a client allowed in over the network.
    var grantID: String?
    /// Its sign-in ended and it has to sign in again.
    var needsSignIn = false
    /// What Plug runs to sign it in again, when the client has a command.
    var signInCommand: String?
    /// Set while a new sign-in has not been told apart from earlier ones.
    var asking: Asking?
    /// A few words a row says in place of a switch.
    var note: String?
    /// Forgets an unfinished sign-in now.
    var forget: (() -> Void)?

    /// Everything a row in the list shows, as a value that can be compared.
    /// An entry's actions are closures and cannot be; each is made from
    /// what is here, so an equal face means the same actions.
    struct Face: Equatable, Sendable {
        let id: String
        let name: String
        let status: ClientStatus
        let isLive: Bool
        let dimmed: Bool
        let glyph: Glyph
        let isBusy: Bool
        let switchLabel: String
        let isOn: Bool?
        let disambiguator: String?
        let grantID: String?
        let needsSignIn: Bool
        let isAsking: Bool
        let note: String?
        let canForget: Bool
    }

    var face: Face {
        Face(
            id: id,
            name: name,
            status: status,
            isLive: isLive,
            dimmed: dimmed,
            glyph: glyph,
            isBusy: isBusy,
            switchLabel: switchLabel,
            isOn: isOn,
            disambiguator: disambiguator,
            grantID: grantID,
            needsSignIn: needsSignIn,
            isAsking: asking != nil,
            note: note,
            canForget: forget != nil
        )
    }
}
