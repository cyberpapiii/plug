import SwiftUI

/// A brief report of something that just failed. Persistent trouble is the
/// verdict's job; this is only for one-off action failures.
struct ErrorToast: View {
    let message: String

    var body: some View {
        Label(message, systemImage: "exclamationmark.triangle.fill")
            .font(.callout)
            .lineLimit(2)
            .padding(.horizontal, Metric.regular)
            .padding(.vertical, Metric.snug)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: Metric.corner))
            .shadow(radius: 8, y: 2)
            .padding()
    }
}

/// The empty state for a whole page: says what would be here and how to get it.
struct EmptyPage: View {
    let title: String
    let message: String
    let symbol: String
    var actionTitle: String?
    var actionIntent: PlugIntent?
    /// A quieter second way out of an empty page, when there is more than one.
    var secondaryTitle: String?
    var secondaryIntent: PlugIntent?
    var run: (PlugIntent) -> Void = { _ in }

    var body: some View {
        VStack(spacing: Metric.snug) {
            Image(systemName: symbol)
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(.tertiary)
            Text(title).font(.title3.weight(.medium))
            Text(message)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 320)
            if actionTitle != nil || secondaryTitle != nil {
                HStack(spacing: Metric.snug) {
                    if let actionTitle, let actionIntent {
                        Button(actionTitle) { run(actionIntent) }
                            .buttonStyle(.borderedProminent)
                    }
                    if let secondaryTitle, let secondaryIntent {
                        Button(secondaryTitle) { run(secondaryIntent) }
                    }
                }
                .padding(.top, Metric.tight)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}
