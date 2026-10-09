import UIKit

/// Monochrome: greys, black and white, no hue. iOS 12 has no system dark mode or dynamic colours, so
/// each colour is read from the mode when a view is built, and a change of mode builds the views again.
enum Theme {
    /// The `theme` setting as last seen, kept for the launch before the settings are open.
    private static let kept = "nori.theme.light"
    static var light = UserDefaults.standard.bool(forKey: kept)

    /// The `theme` setting's value: the system choice is dark, iOS 12 having no system mode.
    static func isLight(_ setting: String) -> Bool { setting == "LIGHT" }

    static func keep(light on: Bool) {
        light = on
        UserDefaults.standard.set(on, forKey: kept)
    }

    private static func pick(_ dark: UInt32, _ bright: UInt32) -> UIColor {
        UIColor(rgb: light ? bright : dark)
    }

    static var background: UIColor { pick(0x111111, 0xFFFFFF) }
    static var row: UIColor { pick(0x1C1C1C, 0xF2F2F2) }
    static var label: UIColor { pick(0xF5F5F5, 0x111111) }
    static var secondary: UIColor { pick(0xA8A8A8, 0x6E6E6E) }
    /// What is on or chosen: the labels' colour.
    static var accent: UIColor { label }
    /// A control that is off (shuffle, repeat) beside one that is on.
    static var dim: UIColor { pick(0x666666, 0xB4B4B4) }
    /// A switch's track when on: grey, so its white thumb stands out.
    static var switchOn: UIColor { pick(0x8C8C8C, 0x555555) }
    static var track: UIColor { pick(0x363636, 0xDCDCDC) }
    static var hairline: UIColor { pick(0x2A2A2A, 0xE2E2E2) }
    static var bar: UIBarStyle { light ? .default : .black }
    static var keyboard: UIKeyboardAppearance { light ? .light : .dark }
    static var statusBar: UIStatusBarStyle { light ? .default : .lightContent }

    /// The player card, its queue and its lyrics: black, or white in the light mode, whatever the cover.
    enum Card {
        static var background: UIColor { pick(0x000000, 0xFFFFFF) }
        static var label: UIColor { pick(0xF5F5F5, 0x111111) }
        static var secondary: UIColor { pick(0xA8A8A8, 0x6E6E6E) }
        static var dim: UIColor { pick(0x666666, 0xB4B4B4) }
        static var track: UIColor { pick(0x363636, 0xDCDCDC) }
    }

    static func apply(tab: UITabBar) {
        tab.barStyle = bar
        tab.barTintColor = background
        tab.isTranslucent = false
        tab.tintColor = accent
        tab.unselectedItemTintColor = secondary
    }

    static func apply(nav: UINavigationBar) {
        nav.barStyle = bar
        nav.barTintColor = background
        nav.isTranslucent = false
        nav.tintColor = accent
        nav.titleTextAttributes = [.foregroundColor: label]
        nav.largeTitleTextAttributes = [.foregroundColor: label]
        nav.prefersLargeTitles = true
    }
}

extension UIAlertController {
    /// iOS 12's sheet is always light; the window's accent tint reads poorly on it, so its text is black.
    static func sheet(_ title: String?, _ message: String? = nil) -> UIAlertController {
        let sheet = UIAlertController(title: title, message: message, preferredStyle: .actionSheet)
        sheet.view.tintColor = .black
        if let title {
            let bold = UIFont.systemFont(ofSize: 13, weight: .semibold)
            sheet.setValue(NSAttributedString(string: title, attributes: [.foregroundColor: UIColor.black, .font: bold]), forKey: "attributedTitle")
        }
        if let message {
            let plain = UIFont.systemFont(ofSize: 13)
            sheet.setValue(NSAttributedString(string: message, attributes: [.foregroundColor: UIColor.black, .font: plain]), forKey: "attributedMessage")
        }
        return sheet
    }

    func add(_ title: String, checked: Bool = false, destructive: Bool = false, run: @escaping () -> Void) {
        let action = UIAlertAction(title: title, style: destructive ? .destructive : .default) { _ in run() }
        if checked { action.setValue(true, forKey: "checked") }
        addAction(action)
    }

    func show(from host: UIViewController) {
        addAction(UIAlertAction(title: Say.cancel, style: .cancel))
        host.present(self, animated: true)
    }
}

extension UIColor {
    convenience init(rgb: UInt32) {
        self.init(
            red: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1
        )
    }
}
