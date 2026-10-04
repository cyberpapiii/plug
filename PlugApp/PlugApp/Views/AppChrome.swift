import SwiftUI

/// A section's rows on the left and the selected row in full on the right.
/// Every section is laid out by this one view, so the divider, the list
/// width, and the empty right side are the same everywhere.
struct ListDetail<Rows: View, Detail: View>: View {
    @ViewBuilder var rows: Rows
    @ViewBuilder var detail: Detail

    var body: some View {
        HStack(spacing: 0) {
            rows
                .listStyle(.inset)
                .frame(width: Metric.listWidth)
            Divider()
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

/// The right side of a section: one grouped form, the system's own look for
/// "everything about this one thing".
struct DetailForm<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        Form { content }
            .formStyle(.grouped)
            .frame(maxWidth: Metric.detailMaxWidth)
            .frame(maxWidth: .infinity)
    }
}

/// The first row of every detail: a picture, the name, one line of status,
/// and the controls that act on the whole thing.
struct DetailHeader<Glyph: View, Controls: View>: View {
    let title: String
    var subtitle: String?
    var monospaced = false
    @ViewBuilder var glyph: Glyph
    @ViewBuilder var controls: Controls

    var body: some View {
        HStack(spacing: Metric.snug) {
            glyph
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(title)
                    .font(monospaced ? .headline.monospaced() : .headline)
                    .lineLimit(2)
                    .textSelection(.enabled)
                if let subtitle {
                    Text(subtitle)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            controls
        }
        .padding(.vertical, Metric.rowGap)
    }
}

/// The right side when no row is selected.
struct NoSelection: View {
    let item: String

    var body: some View {
        Text("No \(item) Selected")
            .font(.title3)
            .foregroundStyle(.tertiary)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// A problem, said the same way everywhere: what happened, why, and what to
/// do. The window, the menu bar panel, and every sheet show this one view, so
/// trouble never looks like three different things.
struct ProblemNote: View {
    let title: String
    var reason: String?
    var advice: String?
    var dismiss: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: Metric.snug) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
                .symbolRenderingMode(.hierarchical)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(title)
                    .font(.callout.weight(.medium))
                    .fixedSize(horizontal: false, vertical: true)
                if let reason, !reason.isEmpty, reason != title {
                    Text(reason)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(4)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                if let advice, !advice.isEmpty {
                    Text(advice)
                        .font(.caption)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let dismiss {
                Button(action: dismiss) {
                    Image(systemName: "xmark")
                        .font(.caption.weight(.semibold))
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .help("Dismiss")
                .accessibilityLabel("Dismiss")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Problem. \(title) \(reason ?? "") \(advice ?? "")")
    }
}

extension ProblemNote {
    init(_ error: ActionError, dismiss: (() -> Void)? = nil) {
        self.init(title: error.title, reason: error.message, advice: error.advice, dismiss: dismiss)
    }

    /// A failure inside a sheet, where the sheet's title already says what
    /// was being done: the reason leads, and the next step follows.
    init(reason: String) {
        self.init(title: reason, advice: Explain.advice(forReason: reason))
    }
}

/// A press that failed, floating over the window until it is dismissed.
/// Persistent trouble is the verdict's job; this is for one action.
struct ErrorToast: View {
    let error: ActionError
    let dismiss: () -> Void

    var body: some View {
        ProblemNote(error, dismiss: dismiss)
            .frame(maxWidth: 460)
            .padding(.horizontal, Metric.regular)
            .padding(.vertical, Metric.snug)
            .nativeGlassSurface(tint: .orange.opacity(0.08))
            .padding()
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
