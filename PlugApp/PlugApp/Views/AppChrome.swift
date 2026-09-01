import SwiftUI

/// One stable content header for every section. It gives controls context and
/// keeps them out of the crowded window toolbar.
struct PageHeader<Content: View>: View {
    let title: String
    var detail: String?
    @ViewBuilder let content: Content

    var body: some View {
        HStack(alignment: .center, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.title3.weight(.semibold))
                if let detail {
                    Text(detail)
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            .fixedSize(horizontal: false, vertical: true)
            .layoutPriority(1)
            Spacer(minLength: 0)
            content
        }
        .padding(.horizontal, Metric.roomy)
        .padding(.vertical, Metric.snug)
        .frame(minHeight: 56)
        .frame(maxWidth: Metric.contentMaxWidth)
        .frame(maxWidth: .infinity)
        .accessibilityAddTraits(.isHeader)
    }
}

extension PageHeader where Content == EmptyView {
    init(_ title: String, detail: String? = nil) {
        self.init(title: title, detail: detail) { EmptyView() }
    }
}

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
            .nativeGlassSurface(tint: .red.opacity(0.08))
            .padding()
            .accessibilityLabel("Error. \(message)")
    }
}

struct LoadingPage: View {
    let message: String

    var body: some View {
        VStack(spacing: Metric.snug) {
            ProgressView().controlSize(.small)
            Text(message)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}

struct UnavailablePage: View {
    let item: String
    let retry: () -> Void

    var body: some View {
        ContentUnavailableView {
            Label("\(item) unavailable", systemImage: "bolt.slash")
        } description: {
            Text("Plug could not reach its background service.")
        } actions: {
            Button("Try Again", action: retry)
                .buttonStyle(.borderedProminent)
        }
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
            VStack(spacing: Metric.snug) {
                Image(systemName: symbol)
                    .font(.system(size: 34, weight: .light))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
                Text(title).font(.title3.weight(.medium))
                Text(message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 320)
            }
            .accessibilityElement(children: .combine)
            if actionTitle != nil || secondaryTitle != nil {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Metric.snug) { actions }
                    VStack(spacing: Metric.tight) { actions }
                }
                .padding(.top, Metric.tight)
            }
        }
        .padding(.horizontal, Metric.roomy)
        .padding(.bottom, 36)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder private var actions: some View {
        if let actionTitle, let actionIntent {
            Button(actionTitle) { run(actionIntent) }
                .buttonStyle(.borderedProminent)
        }
        if let secondaryTitle, let secondaryIntent {
            Button(secondaryTitle) { run(secondaryIntent) }
        }
    }
}
