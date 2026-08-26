import SwiftUI

/// The address field. A UITextField rather than SwiftUI's TextField so it can
/// select all its text the moment it is focused — you open it to type a new
/// address, and clearing the old one character by character is slow.
struct URLField: UIViewRepresentable {
    @Binding var text: String
    /// Grab focus (and select all — see the delegate) as soon as it appears. Used by
    /// the homepage editor so the whole address is ready to be replaced at once.
    var autofocus = false
    var onSubmit: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UITextField {
        let field = UITextField()
        field.delegate = context.coordinator
        if autofocus { DispatchQueue.main.async { field.becomeFirstResponder() } }
        field.placeholder = "주소 또는 검색"
        field.autocapitalizationType = .none
        field.autocorrectionType = .no
        field.keyboardType = .webSearch
        field.returnKeyType = .go
        field.clearButtonMode = .whileEditing
        field.textColor = UIColor(Color.onSurface)
        field.tintColor = UIColor(Color.accent)
        field.font = .systemFont(ofSize: 15)
        // Do NOT let the field's text drive the layout width. A UITextField's
        // intrinsic width grows with its text, and on iOS 16+ SwiftUI reads that —
        // a long URL (a YouTube watch page) made this field, then the address panel,
        // then the whole ZStack (web view included) wider than the screen, cutting the
        // right. Low hugging/compression + a proposal-capped size keep it flexible.
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return field
    }

    func updateUIView(_ field: UITextField, context: Context) {
        if !field.isFirstResponder { field.text = text }
    }

    @available(iOS 16.0, *)
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextField, context: Context) -> CGSize? {
        // Take the width the row offers instead of the text's own (unbounded) width.
        CGSize(width: proposal.width ?? 40, height: 22)
    }

    final class Coordinator: NSObject, UITextFieldDelegate {
        let parent: URLField
        init(_ parent: URLField) { self.parent = parent }

        func textFieldDidBeginEditing(_ field: UITextField) {
            // Select everything so the next keystroke replaces the whole address.
            DispatchQueue.main.async { field.selectAll(nil) }
        }
        func textField(_ f: UITextField, shouldChangeCharactersIn r: NSRange, replacementString s: String) -> Bool {
            if let text = (f.text as NSString?)?.replacingCharacters(in: r, with: s) { parent.text = text }
            return true
        }
        func textFieldShouldReturn(_ field: UITextField) -> Bool {
            parent.text = field.text ?? ""
            field.resignFirstResponder()
            parent.onSubmit()
            return true
        }
    }
}
