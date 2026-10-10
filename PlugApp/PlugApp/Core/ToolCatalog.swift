import Foundation
import PlugIPC

/// One tool a connected app can call, in the terms the interface needs.
///
/// Tools arrive from the daemon under their merged name (`figma__get_file`),
/// which is what a client actually calls. People read them per server, so the
/// server prefix is stripped for display and kept for every action.
struct ToolFacts: Identifiable, Equatable, Sendable {
    var id: String { name }
    let name: String
    let server: String
    let summary: String?
    let isOn: Bool
    /// The wildcard hiding this tool, when it is not switched off by name.
    /// A tool inside a wildcard cannot be switched back on by itself.
    let lockedByPattern: String?
    /// The name the server gives the tool. A rename or a group prefix can
    /// make the merged name unrelated to it.
    let ownName: String?
    /// The server says calling this tool changes nothing.
    let isReadOnly: Bool
    /// The name without its server prefix, which the surrounding group already
    /// states. The full name when there is no prefix to drop.
    let shortName: String

    init(
        name: String,
        server: String,
        summary: String? = nil,
        isOn: Bool = true,
        lockedByPattern: String? = nil,
        ownName: String? = nil,
        isReadOnly: Bool = false
    ) {
        self.name = name
        self.server = server
        self.summary = summary
        self.isOn = isOn
        self.lockedByPattern = lockedByPattern
        self.ownName = ownName
        self.isReadOnly = isReadOnly
        let prefix = "\(server)__"
        if name.count > prefix.count, name.lowercased().hasPrefix(prefix.lowercased()) {
            shortName = String(name.dropFirst(prefix.count))
        } else {
            shortName = name
        }
    }

    var canToggle: Bool { lockedByPattern == nil }
}

extension ToolFacts {
    init(_ info: ToolInfo) {
        self.init(
            name: info.name,
            server: info.serverId,
            summary: info.description ?? info.title,
            isOn: !info.disabled,
            lockedByPattern: info.disabledByPattern,
            ownName: info.ownName,
            isReadOnly: info.readOnly
        )
    }
}

/// Tools of one server, kept together because that is how people look for them.
struct ToolGroup: Identifiable, Equatable, Sendable {
    var id: String { server }
    let server: String
    let tools: [ToolFacts]

    var onCount: Int { tools.filter(\.isOn).count }
    var isFullyOff: Bool { !tools.isEmpty && onCount == 0 }
}

/// The full tool list, grouped and searchable. Pure value, so the rules for
/// what a search matches and how groups are ordered can be tested directly.
struct ToolCatalog: Equatable, Sendable {
    let tools: [ToolFacts]
    /// Each server's tools in the order they are shown. Every row of a server
    /// asks for these, so they are sorted once, when the list arrives.
    private let byServer: [String: [ToolFacts]]

    init(_ tools: [ToolFacts] = []) {
        self.tools = tools
        byServer = Dictionary(grouping: tools, by: \.server).mapValues { $0.sorted(by: ToolFacts.inNameOrder) }
    }

    static func == (lhs: ToolCatalog, rhs: ToolCatalog) -> Bool { lhs.tools == rhs.tools }

    var isEmpty: Bool { tools.isEmpty }

    /// The names a server's tools are filed under for clients, in order.
    /// More than one means Plug splits the server into groups.
    static func groups(ofNames names: [String]) -> [String] {
        Set(names.compactMap { name in
            name.range(of: "__").map { String(name[..<$0.lowerBound]) }
        })
        .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
    }

    var onCount: Int { tools.filter(\.isOn).count }
    var offCount: Int { tools.count - onCount }

    /// Groups matching a query, servers alphabetical, tools alphabetical.
    ///
    /// A query matches a tool's name or its description, and also matches every
    /// tool of a server whose name matches — searching "figma" should show what
    /// Figma can do, not nothing.
    func groups(matching query: String = "") -> [ToolGroup] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let matched = needle.isEmpty ? tools : tools.filter { $0.matches(needle) }
        return Dictionary(grouping: matched, by: \.server)
            .map { server, tools in ToolGroup(server: server, tools: tools.sorted(by: ToolFacts.inNameOrder)) }
            .sorted { $0.server.localizedStandardCompare($1.server) == .orderedAscending }
    }

    /// Every tool a wildcard holds off. A pattern is the only way a tool can be
    /// off without being named, so the panel that explains one has to be able
    /// to list what else it took.
    func tools(coveredBy pattern: String) -> [ToolFacts] {
        tools.filter { $0.lockedByPattern == pattern }.sorted(by: ToolFacts.inNameOrder)
    }

    func tools(for server: String) -> [ToolFacts] { byServer[server] ?? [] }

    /// One server's tools that match a query, by the same rule as `groups`:
    /// a query naming the server keeps all of its tools.
    func tools(for server: String, matching query: String) -> [ToolFacts] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let own = tools(for: server)
        return needle.isEmpty ? own : own.filter { $0.matches(needle) }
    }

    /// The servers with at least one tool matching a query. Empty for an
    /// empty query, which matches nothing in particular.
    func servers(withToolsMatching query: String) -> Set<String> {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !needle.isEmpty else { return [] }
        return Set(tools.filter { $0.matches(needle) }.map(\.server))
    }
}

private extension ToolFacts {
    static func inNameOrder(_ one: ToolFacts, _ other: ToolFacts) -> Bool {
        one.shortName.localizedStandardCompare(other.shortName) == .orderedAscending
    }

    func matches(_ needle: String) -> Bool {
        name.lowercased().contains(needle)
            || server.lowercased().contains(needle)
            || (summary?.lowercased().contains(needle) ?? false)
    }
}
