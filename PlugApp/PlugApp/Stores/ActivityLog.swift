import PlugIPC

/// The calls Activity lists: the newest `limit` the daemon has sent, read
/// after the last one already held.
struct ActivityLog: Equatable, Sendable {
    static let limit = 200

    private(set) var events: [ActivityEvent] = []
    /// True only after older rows were actually discarded. Holding exactly
    /// `limit` calls is not proof that one more exists.
    private(set) var wasTruncated = false

    /// Where the next read starts. A daemon that restarted numbers its calls
    /// from the beginning again, so the read starts over too.
    func cursor(daemonRestarted: Bool) -> UInt64 {
        daemonRestarted ? 0 : (events.last?.sequence ?? 0)
    }

    /// Takes the calls read after `cursor`, asked for `limit + 1` at a time so
    /// a read from the start can tell whether older ones exist.
    mutating func take(_ page: [ActivityEvent], after cursor: UInt64) {
        if cursor == 0 {
            wasTruncated = page.count > Self.limit
            events = Array(page.suffix(Self.limit))
        } else if !page.isEmpty {
            let merged = events + page
            wasTruncated = wasTruncated || merged.count > Self.limit
            events = Array(merged.suffix(Self.limit))
        }
    }

    /// The one line Activity shows about its list: how many calls, how many
    /// of them failed, and whether older ones were dropped. Nil with no calls.
    func summary(stale: Bool) -> String? {
        guard !events.isEmpty else { return nil }
        let count = events.count
        let failed = events.filter { CallFacts($0).failed }.count
        var summary = wasTruncated
            ? "Latest \(Self.limit) calls"
            : "\(count) recent \(count == 1 ? "call" : "calls")"
        if failed > 0 { summary += ", \(failed) failed" }
        return lastKnown(summary, stale: stale)
    }
}
