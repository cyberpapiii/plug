/// A second account is the same server under `<server>-<account>`. The daemon
/// has the last word on the name; this says the same thing before the trip.
enum AccountDraft {
    static let longest = 24

    /// What the owner typed, as the daemon will take it, or nil when it cannot
    /// be an account name.
    static func label(from typed: String) -> String? {
        let label = typed.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard let first = label.unicodeScalars.first,
              ("a"..."z").contains(first),
              label.count <= longest,
              label.unicodeScalars.allSatisfy({ ("a"..."z").contains($0) || ("0"..."9").contains($0) })
        else { return nil }
        return label
    }

    static func serverName(server: String, label: String) -> String { "\(server)-\(label)" }
}
