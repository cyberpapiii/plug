import SwiftUI

/// The one frame every sheet in Plug uses: a title, a sentence under it, the
/// content, and a footer that shows what went wrong beside Cancel and the one
/// button that finishes the sheet.
///
/// Each sheet used to retype this, and they drifted: four widths, and an error
/// that was orange in one sheet and missing in another.
struct SheetFrame<Content: View, Extra: View>: View {
    static var width: CGFloat { 520 }

    let title: String
    var subtitle: String?
    /// What went wrong, in a sentence. Stays until the next attempt.
    var failure: String?
    /// True while the sheet is saving; it cannot be closed half way.
    var busy = false
    var cancelTitle = "Cancel"
    /// Nil when the sheet has nothing to confirm, only something to close.
    var confirmTitle: String?
    var confirmDisabled = false
    var confirm: () -> Void = {}
    /// A quieter button left of Cancel, such as Back.
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

            content

            HStack(spacing: Metric.snug) {
                if let failure {
                    Label(failure, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                Spacer(minLength: 0)
                extra
                Button(cancelTitle) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(busy)
                if let confirmTitle {
                    Button(confirmTitle, action: confirm)
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(confirmDisabled || busy)
                }
            }
        }
        .padding(Metric.roomy)
        .frame(width: Self.width)
        .interactiveDismissDisabled(busy)
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
