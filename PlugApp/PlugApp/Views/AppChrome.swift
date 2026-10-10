import SwiftUI

/// Which column of the window a section is drawing. The window shows each
/// section twice, once per column, and the section's own views pick the half
/// that belongs there.
enum SplitPane {
    /// Not in the window's columns: rows and detail side by side.
    case whole
    case list
    case detail
}

extension EnvironmentValues {
    @Entry var splitPane: SplitPane = .whole
}

/// A section's rows and the selected row in full. Every section is laid out
/// by this one view. In the window the rows go in the middle column and the
/// detail in the last, so the system draws the bars and the dividers the way
/// it does in Mail.
struct ListDetail<Rows: View, Detail: View>: View {
    @ViewBuilder var rows: Rows
    @ViewBuilder var detail: Detail
    @Environment(\.splitPane) private var pane

    var body: some View {
        switch pane {
        case .list:
            rows
        case .detail:
            detail
        case .whole:
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
}

/// The name of a group of rows in a section's list. It is a row of its own
/// and scrolls with the rest: a pinned header draws a line across the column
/// under the bar, and the bar should read as one piece.
struct ListGroupHeader: View {
    let title: String

    init(_ title: String) { self.title = title }

    var body: some View {
        Text(title)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.secondary)
            .padding(.top, Metric.snug)
            .selectionDisabled()
            .listRowSeparator(.hidden)
            .accessibilityAddTraits(.isHeader)
    }
}

/// Something that fills a section instead of its rows: a wait, an empty
/// state, a failure. It belongs to the detail column, and the list column
/// stays empty beside it.
struct PagePane<Content: View>: View {
    @ViewBuilder var content: Content
    @Environment(\.splitPane) private var pane

    var body: some View {
        if pane == .list {
            Color.clear
        } else {
            content
        }
    }
}

/// A search that matched nothing. It is the list that came up empty, so the
/// list column says so.
struct NoSearchResults: View {
    let text: String
    @Environment(\.splitPane) private var pane

    var body: some View {
        if pane == .detail {
            Color.clear
        } else {
            ContentUnavailableView.search(text: text)
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
        PagePane {
            PlugCharacter(mood: .working)
                .foregroundStyle(.secondary)
                .frame(width: 44, height: 44)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityElement()
                .accessibilityLabel(message)
        }
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
        PagePane {
            ContentUnavailableView {
                PlugCharacterLabel(title: verdict.title, mood: PlugCharacter.Mood(verdict.tone))
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
        PagePane {
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
}
