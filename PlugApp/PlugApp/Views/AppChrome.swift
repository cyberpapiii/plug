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
    @ViewBuilder var glyph: Glyph
    @ViewBuilder var controls: Controls

    var body: some View {
        HStack(spacing: Metric.snug) {
            glyph
                .frame(width: Metric.glyphSlot, height: Metric.glyphSlot)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(title)
                    .font(.title3.weight(.semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .help(title)
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
    }
}

/// The right side when no row is selected.
struct NoSelection: View {
    let item: String
    let symbol: String

    var body: some View {
        ContentUnavailableView("No \(item) Selected", systemImage: symbol)
    }
}

/// A problem, said the same way everywhere: what happened, why, and what to
/// do. The window, the menu bar panel, and every sheet show this one view, so
/// trouble never looks like three different things.
struct ProblemNote: View {
    let title: String
    var reason: String?
    var advice: String?
    /// The one thing to press about it, under the words.
    var actionTitle: String?
    var action: (() -> Void)?
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
                if let actionTitle, let action {
                    Button(actionTitle, action: action)
                        .controlSize(.small)
                        .padding(.top, Metric.rowGap)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let dismiss {
                Button(action: dismiss) {
                    Image(systemName: "xmark")
                        .imageScale(.small)
                        .frame(width: 20, height: 20)
                        .contentShape(Rectangle())
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
            .padding(Metric.regular)
    }
}

/// A page that is still loading. The words are for VoiceOver only.
struct LoadingPage: View {
    let message: String

    var body: some View {
        ProgressView()
            .controlSize(.small)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityLabel(message)
    }
}

/// The same wait inside a sheet, holding the height the content will take.
struct SheetLoading: View {
    let label: String
    var height: CGFloat = 160

    var body: some View {
        ProgressView()
            .controlSize(.small)
            .frame(maxWidth: .infinity, minHeight: height)
            .accessibilityLabel(label)
    }
}

/// A page Plug cannot fill right now. It shows the verdict, so the page and
/// the banner never say two different things.
struct UnavailablePage: View {
    let verdict: Verdict
    let run: (PlugIntent) -> Void

    var body: some View {
        ContentUnavailableView {
            Label(verdict.title, systemImage: verdict.symbol)
        } description: {
            if let detail = verdict.detail {
                Text(detail)
            }
        } actions: {
            if let primary = verdict.primary {
                Button(primary.title) { run(primary.intent) }
                    .buttonStyle(.borderedProminent)
            }
            if let secondary = verdict.secondary {
                Button(secondary.title) { run(secondary.intent) }
            }
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
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            Text(message)
        } actions: {
            if let actionTitle, let actionIntent {
                Button(actionTitle) { run(actionIntent) }
                    .buttonStyle(.borderedProminent)
            }
            if let secondaryTitle, let secondaryIntent {
                Button(secondaryTitle) { run(secondaryIntent) }
            }
        }
    }
}
