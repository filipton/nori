import UIKit

/// Adds a server: checks the address, saves the profile as the active one and opens it.
final class LoginPage: UIViewController, UITextFieldDelegate {
    private let address = UITextField()
    private let user = UITextField()
    private let password = UITextField()
    private let name = UITextField()
    private let second = UITextField()
    private let key = UITextField()
    private let more = UIButton(type: .system)
    private let scroll = UIScrollView()
    private let connect = UIButton(type: .system)
    private let status = UILabel()
    private let spinner = UIActivityIndicatorView()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Theme.background
        title = Say.addServer
        navigationItem.largeTitleDisplayMode = .never
        additionalSafeAreaInsets.bottom = MiniPlayer.height

        field(address, Say.serverUrl, .URL, false)
        field(user, Say.user, .default, false)
        field(password, Say.password, .default, true)
        address.returnKeyType = .next
        user.returnKeyType = .next
        password.returnKeyType = .go

        connect.setTitle(Say.connect, for: .normal)
        connect.setTitleColor(Theme.background, for: .normal)
        connect.titleLabel?.font = UIFont.preferredFont(forTextStyle: .headline)
        connect.titleLabel?.adjustsFontForContentSizeCategory = true
        connect.backgroundColor = Theme.accent
        connect.layer.cornerRadius = 10
        connect.addTarget(self, action: #selector(submit), for: .touchUpInside)

        status.textColor = UIColor(rgb: 0xFFB4AB)
        status.font = UIFont.preferredFont(forTextStyle: .footnote)
        status.adjustsFontForContentSizeCategory = true
        status.numberOfLines = 0
        status.textAlignment = .center

        spinner.hidesWhenStopped = true
        spinner.color = Theme.label

        let hint = UILabel()
        hint.text = Say.serverKinds
        hint.textColor = Theme.secondary
        hint.font = UIFont.preferredFont(forTextStyle: .footnote)
        hint.adjustsFontForContentSizeCategory = true
        hint.numberOfLines = 0
        hint.textAlignment = .center

        field(name, Say.nameOptional, .default, false)
        name.autocapitalizationType = .words
        field(second, Say.secondAddress, .URL, false)
        field(key, Say.apiKey, .default, true)
        more.setTitle(Say.advanced, for: .normal)
        more.tintColor = Theme.secondary
        more.contentHorizontalAlignment = .leading
        more.addTarget(self, action: #selector(moreTapped), for: .touchUpInside)
        for f in [name, second, key] {
            f.isHidden = true
            f.heightAnchor.constraint(equalToConstant: 44).isActive = true
        }
        let stack = UIStackView(arrangedSubviews: [address, user, password, more, name, second, key, connect, spinner, status, hint])
        stack.axis = .vertical
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        // Scrolls, so the advanced fields and the button stay in reach above the keyboard on 568 pt.
        scroll.keyboardDismissMode = .onDrag
        scroll.alwaysBounceVertical = true
        scroll.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scroll)
        scroll.addSubview(stack)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboard(_:)), name: UIResponder.keyboardWillChangeFrameNotification, object: nil)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: scroll.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: scroll.trailingAnchor, constant: -20),
            stack.widthAnchor.constraint(equalTo: scroll.widthAnchor, constant: -40),
            stack.topAnchor.constraint(equalTo: scroll.topAnchor, constant: 16),
            stack.bottomAnchor.constraint(equalTo: scroll.bottomAnchor, constant: -16),
            address.heightAnchor.constraint(equalToConstant: 44),
            user.heightAnchor.constraint(equalToConstant: 44),
            password.heightAnchor.constraint(equalToConstant: 44),
            connect.heightAnchor.constraint(equalToConstant: 44),
        ])
    }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        if textField === address {
            user.becomeFirstResponder()
        } else if textField === user {
            password.becomeFirstResponder()
        } else if textField === name {
            second.becomeFirstResponder()
        } else if textField === second {
            key.becomeFirstResponder()
        } else {
            textField.resignFirstResponder()
            submit()
        }
        return true
    }

    /// Room under the form for the keyboard, as far as it covers the scroll view.
    @objc private func keyboard(_ n: Notification) {
        guard let frame = (n.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue else { return }
        let covered = max(0, scroll.convert(scroll.bounds, to: nil).maxY - frame.minY)
        scroll.contentInset.bottom = covered
        scroll.scrollIndicatorInsets.bottom = covered
    }

    /// The name, the second address and the API key: shown on asking.
    @objc private func moreTapped() {
        let show = name.isHidden
        UIView.animate(withDuration: UIAccessibility.isReduceMotionEnabled ? 0 : 0.25) {
            for f in [self.name, self.second, self.key] { f.isHidden = !show }
        }
        more.setTitle(show ? Say.hideAdvanced : Say.advanced, for: .normal)
    }

    @objc private func submit() {
        view.endEditing(true)
        let fields: [String: String] = [
            "url": address.text ?? "", "user": user.text ?? "", "password": password.text ?? "",
            "name": name.text ?? "", "alt": second.text ?? "", "key": key.text ?? "",
        ]
        let form = (try? JSONSerialization.data(withJSONObject: fields)).flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        connect.isEnabled = false
        connect.setTitle(Say.connecting, for: .normal)
        status.text = ""
        spinner.startAnimating()
        let dir = dataDirectory().path
        DispatchQueue.global(qos: .userInitiated).async {
            var detail: UnsafeMutablePointer<CChar>?
            let code = dir.withCString { d in
                form.withCString { f in
                    withUnsafeMutablePointer(to: &detail) { nori_ios_login(d, f, $0) }
                }
            }
            let extra = taken(detail)
            let opened = code == NORI_LOGIN_OK ? Core.reopen() : nil
            DispatchQueue.main.async {
                self.spinner.stopAnimating()
                if code == NORI_LOGIN_OK {
                    Core.shared.opened()
                    if opened == nil {
                        self.navigationController?.popViewController(animated: true)
                        return
                    }
                }
                self.status.text = opened ?? Say.failure(code, extra)
                self.connect.setTitle(Say.connect, for: .normal)
                self.connect.isEnabled = true
            }
        }
    }

    private func field(_ box: UITextField, _ placeholder: String, _ keyboard: UIKeyboardType, _ secure: Bool) {
        box.placeholder = placeholder
        box.keyboardType = keyboard
        box.isSecureTextEntry = secure
        box.autocapitalizationType = .none
        box.autocorrectionType = .no
        box.keyboardAppearance = Theme.keyboard
        box.textColor = Theme.label
        box.tintColor = Theme.accent
        box.backgroundColor = Theme.row
        box.layer.cornerRadius = 10
        box.font = UIFont.preferredFont(forTextStyle: .body)
        box.adjustsFontForContentSizeCategory = true
        box.delegate = self
        box.leftView = UIView(frame: CGRect(x: 0, y: 0, width: 12, height: 44))
        box.leftViewMode = .always
        box.attributedPlaceholder = NSAttributedString(
            string: placeholder,
            attributes: [.foregroundColor: Theme.secondary]
        )
    }
}
