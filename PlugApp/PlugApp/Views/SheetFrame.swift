import SwiftUI

/// The one frame every sheet in Plug uses: a title, a sentence under it, the
/// content, what went wrong, and a footer with Cancel and the one button that
/// finishes the sheet. A sheet with nothing to confirm shows only Done.
///
/// The frame insets its header and footer and leaves the content's sides
/// alone, so a grouped form in the content runs on its own margins. Content
/// that is not a form adds `Metric.roomy` at its sides.
///
/// Each sheet used to retype this, and they drifted: four widths, and an error
/// that was orange in one sheet and missing in another.
struct SheetFrame<Content: View, Extra: View>: View {
    static var width: CGFloat { Metric.sheetWidth }

    let title: String
    var subtitle: String?
    /// What went wrong, in a sentence. Stays until the next attempt. The
    /// sheet adds what to do about it.
    var failure: String?
    /// True while the sheet is saving; it cannot be closed half way.
    var busy = false
    var cancelTitle = "Cancel"
    /// Nil when the sheet has nothing to confirm, only something to close.
    var confirmTitle: String?
    var confirmDisabled = false
    var confirm: () -> Void = {}
    /// A quieter button at the leading edge of the footer, such as Back.
    @ViewBuilder var extra: Extra
    @ViewBuilder var content: Content
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(title)
                    .font(.title2.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                if let subtitle {
                    Text(subtitle)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.horizontal, Metric.roomy)

            content

            if let failure {
                ProblemNote(reason: failure)
                    .padding(.horizontal, Metric.roomy)
            }

            HStack(spacing: Metric.snug) {
                extra
                Spacer(minLength: 0)
                if busy {
                    WaitingDots()
                }
                if let confirmTitle {
                    Button(cancelTitle) { dismiss() }
                        .keyboardShortcut(.cancelAction)
                        .disabled(busy)
                    Button(confirmTitle, action: confirm)
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(confirmDisabled || busy)
                } else {
                    Button("Done") { dismiss() }
                        .keyboardShortcut(.defaultAction)
                        .disabled(busy)
                }
            }
            .padding(.horizontal, Metric.roomy)
        }
        .padding(.vertical, Metric.roomy)
        .frame(width: Self.width)
        .interactiveDismissDisabled(busy)
        .onExitCommand {
            if confirmTitle == nil, !busy { dismiss() }
        }
    }
}

extension SheetFrame where Extra == EmptyView {
    init(
        title: String,
        subtitle: String? = nil,
        failure: String? = nil,
        busy: Bool = false,
        cancelTitle: String = "Cancel",
        confirmTitle: String? = nil,
        confirmDisabled: Bool = false,
        confirm: @escaping () -> Void = {},
        @ViewBuilder content: () -> Content
    ) {
        self.init(
            title: title,
            subtitle: subtitle,
            failure: failure,
            busy: busy,
            cancelTitle: cancelTitle,
            confirmTitle: confirmTitle,
            confirmDisabled: confirmDisabled,
            confirm: confirm,
            extra: { EmptyView() },
            content: content
        )
    }
}
