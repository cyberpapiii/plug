import Foundation

/// Where a setting is kept: `UserDefaults` in the app, memory in tests. A
/// `UserDefaults` suite writes a plist under the real `~/Library/Preferences`,
/// and removing its domain afterwards leaves the file.
protocol PreferenceStore: AnyObject {
    func object(forKey key: String) -> Any?
    func set(_ value: Any?, forKey key: String)
}

extension UserDefaults: PreferenceStore {}
