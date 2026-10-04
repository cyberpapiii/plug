import Foundation
import PlugIPC

/// A server someone can add by picking its name.
///
/// Adding a server used to start from a paste box, which is a dead end for a
/// person who has nothing to paste. These are servers their makers run at a
/// public address and that let Plug sign in on its own, so picking one needs
/// no setup block, no key, and no account with the maker's developer program.
struct KnownServer: Equatable, Sendable, Identifiable {
    /// The name the server gets in Plug.
    let id: String
    let title: String
    /// What a person can do with it, in a few words.
    let summary: String
    let address: String
    /// False for the few that answer without an account.
    var needsSignIn = true

    var config: ServerConfig {
        var config = ServerConfig.remote(address)
        if needsSignIn { config.auth = "oauth" }
        return config
    }

    var draft: ServerDraft {
        ServerDraft(
            name: id,
            config: config,
            facts: [
                DraftFact(label: "Connects to", value: address),
                DraftFact(
                    label: "Sign-in",
                    value: needsSignIn ? "Plug asks you to sign in after you add it" : "None needed"
                ),
            ]
        )
    }

    /// Every address here was checked on 2026-10-04: it answers as a server,
    /// and those marked for sign-in let a new client register itself.
    static let all: [KnownServer] = [
        KnownServer(id: "notion", title: "Notion", summary: "Pages and databases", address: "https://mcp.notion.com/mcp"),
        KnownServer(id: "linear", title: "Linear", summary: "Issues and projects", address: "https://mcp.linear.app/mcp"),
        KnownServer(id: "atlassian", title: "Atlassian", summary: "Jira and Confluence", address: "https://mcp.atlassian.com/v1/mcp"),
        KnownServer(id: "asana", title: "Asana", summary: "Tasks and projects", address: "https://mcp.asana.com/v2/mcp"),
        KnownServer(id: "sentry", title: "Sentry", summary: "Errors and performance", address: "https://mcp.sentry.dev/mcp"),
        KnownServer(id: "stripe", title: "Stripe", summary: "Payments and customers", address: "https://mcp.stripe.com"),
        KnownServer(id: "vercel", title: "Vercel", summary: "Projects and deployments", address: "https://mcp.vercel.com"),
        KnownServer(id: "cloudflare", title: "Cloudflare", summary: "Workers, DNS, and storage", address: "https://mcp.cloudflare.com/mcp"),
        KnownServer(id: "canva", title: "Canva", summary: "Designs", address: "https://mcp.canva.com/mcp"),
        KnownServer(id: "intercom", title: "Intercom", summary: "Conversations and contacts", address: "https://mcp.intercom.com/mcp"),
        KnownServer(
            id: "context7",
            title: "Context7",
            summary: "Up-to-date library documentation",
            address: "https://mcp.context7.com/mcp",
            needsSignIn: false
        ),
    ]

    /// The ones Plug does not have yet. Names compare without case, so a
    /// server added as "Notion" is not offered again as "notion".
    static func notYetAdded(names: [String]) -> [KnownServer] {
        let taken = Set(names.map { $0.lowercased() })
        return all.filter { !taken.contains($0.id) }
    }
}
