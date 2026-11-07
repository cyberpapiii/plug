/// What the window is for. Signing in to a server used to be its own section,
/// and so did tools; both now live on the server they belong to, because a
/// server's account and its tools are found and fixed where the server is.
enum AppSection: String, CaseIterable, Identifiable, Sendable {
    case servers = "Servers"
    case clients = "Clients"
    case events = "Events"
    case activity = "Activity"

    var id: Self { self }
}
