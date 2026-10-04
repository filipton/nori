import UIKit

/// Settings → Licenses: the core's credits (`credits::core_credits`, `data_credits`), each opening its
/// licence text from the bundle.
final class CreditsPage: UITableViewController {
    private struct Credit {
        let name: String
        let what: String
        let copyright: String
        let licence: String
        let file: String?
    }

    private var sections: [(title: String, credits: [Credit])] = []

    init() { super.init(style: .grouped) }
    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = Say.licences
        navigationItem.largeTitleDisplayMode = .never
        dress(tableView)
        tableView.estimatedRowHeight = 60
        let d = takenJSON(nori_ios_credits()) as? [String: Any] ?? [:]
        let read = { (key: String) -> [Credit] in
            (d[key] as? [[String: Any]] ?? []).map {
                Credit(name: $0["name"] as? String ?? "", what: $0["what"] as? String ?? "",
                       copyright: $0["copyright"] as? String ?? "", licence: $0["licence"] as? String ?? "",
                       file: $0["file"] as? String)
            }
        }
        sections = [(Say.rustCore, read("core")), (Say.appCredits, read("app")), (Say.fontsAndData, read("data"))]
            .filter { !$0.credits.isEmpty }
    }

    override func numberOfSections(in tableView: UITableView) -> Int { sections.count }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? { sections[section].title }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { sections[section].credits.count }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let c = sections[indexPath.section].credits[indexPath.row]
        let cell = row(tableView, id: "credit", style: .subtitle)
        cell.textLabel?.text = c.name + "  ·  " + c.licence
        cell.detailTextLabel?.text = c.what
        cell.detailTextLabel?.numberOfLines = 0
        cell.detailTextLabel?.font = UIFont.preferredFont(forTextStyle: .footnote)
        cell.detailTextLabel?.adjustsFontForContentSizeCategory = true
        cell.accessoryType = .disclosureIndicator
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        let c = sections[indexPath.section].credits[indexPath.row]
        let text: String
        if let file = c.file {
            let path = Bundle.main.path(forResource: file, ofType: "txt", inDirectory: "licences")
            text = path.flatMap { try? String(contentsOfFile: $0) } ?? Say.licenceUnreadable
        } else {
            text = Say.noLicenceText
        }
        navigationController?.pushViewController(LicencePage(title: c.name, text: c.copyright + "\n\n" + text), animated: true)
    }
}

/// One licence, as text.
final class LicencePage: UIViewController {
    private let text: String

    init(title: String, text: String) {
        self.text = text
        super.init(nibName: nil, bundle: nil)
        self.title = title
    }

    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        navigationItem.largeTitleDisplayMode = .never
        let view = UITextView(frame: self.view.bounds)
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.isEditable = false
        view.backgroundColor = Theme.background
        view.textColor = Theme.label
        view.font = UIFont.monospacedDigitSystemFont(ofSize: 13, weight: .regular)
        view.textContainerInset = UIEdgeInsets(top: 16, left: 12, bottom: 16 + MiniPlayer.height, right: 12)
        view.text = text
        self.view.addSubview(view)
    }
}
