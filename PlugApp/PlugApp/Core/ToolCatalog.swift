import Foundation
import PlugIPC

/// One tool a connected app can call, in the terms the interface needs.
///
/// Tools arrive from the daemon under their merged name (`figma__get_file`),
/// which is what a client actually calls. People read them per server, so the
/// server prefix is stripped for display and kept for every action. The
/// delimiter between the two is the daemon's (`OperatorSnapshot.prefixDelimiter`).
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
        delimiter: String,
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
        shortName = Self.shortName(of: name, server: server, delimiter: delimiter)
    }

    /// A merged tool name without its server's own prefix. Any other prefix,
    /// such as a group's, stays.
    static func shortName(of name: String, server: String, delimiter: String) -> String {
        let prefix = "\(server)\(delimiter)"
        guard !delimiter.isEmpty, name.count > prefix.count, name.lowercased().hasPrefix(prefix.lowercased())
        else { return name }
        return String(name.dropFirst(prefix.count))
    }
}

extension ToolFacts {
    init(_ info: ToolInfo, delimiter: String) {
        self.init(
            delimiter: delimiter,
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

/// The full tool list, grouped and searchable.
struct ToolCatalog: Equatable, Sendable {
    let tools: [ToolFacts]
    /// The snapshot's `tool_catalog_revision` when this list was read.
    let revision: UInt64?
    /// Each server's tools in the order they are shown. Every row of a server
    /// asks for these, so they are sorted once, when the list arrives.
    private let byServer: [String: [ToolFacts]]

    init(_ tools: [ToolFacts] = [], revision: UInt64? = nil) {
        self.tools = tools
        self.revision = revision
        byServer = Dictionary(grouping: tools, by: \.server).mapValues { $0.sorted(by: ToolFacts.inNameOrder) }
    }

    static func == (lhs: ToolCatalog, rhs: ToolCatalog) -> Bool { lhs.tools == rhs.tools }

    var isEmpty: Bool { tools.isEmpty }

    /// Whether a daemon reporting `revision` would answer with this list.
    func isCurrent(at revision: UInt64?) -> Bool { !isEmpty && self.revision == revision }

    /// The names a server's tools are filed under for clients, in order.
    /// More than one means Plug splits the server into groups.
    static func groups(ofNames names: [String], delimiter: String) -> [String] {
        guard !delimiter.isEmpty else { return [] }
        return Set(names.compactMap { name in
            name.range(of: delimiter).map { String(name[..<$0.lowerBound]) }
        })
        .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
    }

    /// Every tool a wildcard holds off. A pattern is the only way a tool can be
    /// off without being named, so the panel that explains one has to be able
    /// to list what else it took.
    func tools(coveredBy pattern: String) -> [ToolFacts] {
        tools.filter { $0.lockedByPattern == pattern }.sorted(by: ToolFacts.inNameOrder)
    }

    func tools(for server: String) -> [ToolFacts] { byServer[server] ?? [] }

    /// One server's tools that match a query, alphabetical. A query matches a
    /// tool's name or its description, and a query naming the server keeps
    /// all of its tools: searching "figma" should show what Figma can do, not
    /// nothing.
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
