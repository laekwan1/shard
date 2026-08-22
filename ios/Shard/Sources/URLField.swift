import SwiftUI

/// The address field. A UITextField rather than SwiftUI's TextField so it can
/// select all its text the moment it is focused — you open it to type a new
/// address, and clearing the old one character by character is slow.
struct URLField: UIViewRepresentable {
    @Binding var text: String
    var onSubmit: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UITextField {
        let field = UITextField()
        field.delegate = context.coordinator
        field.placeholder = "주소 또는 검색"
        field.autocapitalizationType = .none
        field.autocorrectionType = .no
        field.keyboardType = .webSearch
        field.returnKeyType = .go
        field.clearButtonMode = .whileEditing
        field.textColor = UIColor(Color.onSurface)
        field.tintColor = UIColor(Color.accent)
        field.font = .systemFont(ofSize: 15)
        return field
    }

    func updateUIView(_ field: UITextField, context: Context) {
        if !field.isFirstResponder { field.text = text }
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
