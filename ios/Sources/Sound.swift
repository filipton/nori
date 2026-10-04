import AVFoundation
import UIKit

/// One AutoEQ curve as the library names it.
struct Curve {
    let name: String
    let source: String
    let form: String
    let target: String
    let path: String

    init(_ d: [String: Any]) {
        name = d["name"] as? String ?? ""
        source = d["source"] as? String ?? ""
        form = d["form"] as? String ?? ""
        target = d["target"] as? String ?? ""
        path = d["path"] as? String ?? ""
    }

    /// Calls `f` with the five fields as C strings, valid for the call.
    func withC<R>(_ f: ([UnsafeMutablePointer<CChar>?]) -> R) -> R {
        let c = [name, source, form, target, path].map { s in s.withCString { strdup($0) } }
        defer { c.forEach { free($0) } }
        return f(c)
    }
}

/// One output in the device list, as `nori_ios_devices` names it.
struct Output {
    let key: String
    let port: Int
    let name: String?
    let current: Bool
    let choice: Int
    let profile: String?

    init(_ d: [String: Any]) {
        key = d["output"] as? String ?? ""
        port = d["port"] as? Int ?? 4
        name = (d["name"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        current = d["current"] as? Bool ?? false
        choice = d["choice"] as? Int ?? 0
        profile = d["profile"] as? String
    }

    var label: String { Say.outputLabel(port: port, name: name) }
}

/// A spinner in the bar while a request runs, `after` there once it is done; the table takes no taps
/// meanwhile.
private func busy(_ page: UITableViewController, _ on: Bool, after: UIBarButtonItem? = nil) {
    page.tableView.isUserInteractionEnabled = !on
    guard on else {
        page.navigationItem.rightBarButtonItem = after
        return
    }
    let spinner = UIActivityIndicatorView(style: Theme.light ? .gray : .white)
    spinner.startAnimating()
    page.navigationItem.rightBarButtonItem = UIBarButtonItem(customView: spinner)
}

/// Settings → Sound: each output with the sound it gets, and the AutoEQ headphone presets.
final class SoundPage: UITableViewController {
    private var outputs: [Output] = []

    init() { super.init(style: .grouped) }
    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = Say.sound
        navigationItem.largeTitleDisplayMode = .never
        dress(tableView)
        tableView.estimatedRowHeight = 52
        // Headphones in or out while the list is open: "Playing now" moves with them.
        NotificationCenter.default.addObserver(self, selector: #selector(routeChanged),
                                               name: AVAudioSession.routeChangeNotification, object: nil)
    }

    @objc private func routeChanged() {
        DispatchQueue.main.async { self.load() }
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        load()
    }

    private func load() {
        DispatchQueue.global(qos: .userInitiated).async {
            let d = takenJSON(nori_ios_devices()) as? [String: Any] ?? [:]
            let rows = (d["rows"] as? [[String: Any]] ?? []).map(Output.init)
            DispatchQueue.main.async {
                self.outputs = rows
                self.tableView.reloadData()
            }
        }
    }

    override func numberOfSections(in tableView: UITableView) -> Int { 2 }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        section == 0 ? Say.devices : nil
    }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        section == 0 ? outputs.count : 1
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = row(tableView, id: "output", style: .subtitle)
        cell.accessoryType = .disclosureIndicator
        if indexPath.section == 1 {
            cell.textLabel?.text = Say.headphonePresets
            cell.detailTextLabel?.text = nil
            return cell
        }
        let o = outputs[indexPath.row]
        cell.textLabel?.text = o.label
        let sound = Say.deviceSound(choice: o.choice, profile: o.profile)
        cell.detailTextLabel?.text = o.current ? sound + " · " + Say.playingNow : sound
        cell.detailTextLabel?.font = UIFont.preferredFont(forTextStyle: .footnote)
        cell.detailTextLabel?.adjustsFontForContentSizeCategory = true
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        let page = indexPath.section == 0 ? DevicePage(outputs[indexPath.row]) : AutoEqPage(for: nil)
        navigationController?.pushViewController(page, animated: true)
    }
}

/// One output's sound: automatic, flat, no processing, left as is, a saved profile or an AutoEQ curve.
/// A choice made goes back to the list.
final class DevicePage: UITableViewController {
    private let output: Output
    private var profiles: [String] = []
    private var curves: [Curve] = []
    private var forget = false
    /// Headphones in the AutoEQ list; 0 before it is downloaded.
    private var listed = 0
    private var autoApply = false

    /// The fixed choices, by `nori_ios_device_assign` code, in Android's order.
    private let fixed: [(code: Int32, title: String)] = [
        (0, Say.automatic), (2, Say.flat), (4, Say.noProcessing), (1, Say.leaveAsIs),
    ]

    init(_ output: Output) {
        self.output = output
        super.init(style: .grouped)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = output.label
        navigationItem.largeTitleDisplayMode = .never
        dress(tableView)
        tableView.estimatedRowHeight = 52
        let intro = UILabel()
        intro.text = Say.deviceIntro(kind: Say.outputKind(port: output.port))
        intro.numberOfLines = 0
        intro.font = UIFont.preferredFont(forTextStyle: .footnote)
        intro.adjustsFontForContentSizeCategory = true
        intro.textColor = Theme.secondary
        let width = UIScreen.main.bounds.width - 32
        let height = intro.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        let head = UIView(frame: CGRect(x: 0, y: 0, width: width + 32, height: height + 24))
        intro.frame = CGRect(x: 16, y: 16, width: width, height: height)
        head.addSubview(intro)
        tableView.tableHeaderView = head
        load()
    }

    private func load() {
        let key = output.key
        DispatchQueue.global(qos: .userInitiated).async {
            let d = key.withCString { takenJSON(nori_ios_device_sheet($0)) } as? [String: Any] ?? [:]
            let listed = (takenJSON(nori_ios_autoeq_browse("")) as? [String: Any])?["count"] as? Int ?? 0
            let auto = SettingsPage.value("autoEqAuto") == "true"
            DispatchQueue.main.async {
                self.profiles = d["profiles"] as? [String] ?? []
                self.curves = (d["curves"] as? [[String: Any]] ?? []).map(Curve.init)
                self.forget = d["forget"] as? Bool ?? false
                self.listed = listed
                self.autoApply = auto
                self.tableView.reloadData()
            }
        }
    }

    // 0: the choices and saved profiles. 1: AutoEQ curves. 2: forget, when it can be.
    override func numberOfSections(in tableView: UITableView) -> Int { forget ? 3 : 2 }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        section == 1 ? Say.autoeqCurves : nil
    }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        switch section {
        case 0: return fixed.count + profiles.count
        case 1: return curves.count + 1
        default: return 1
        }
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = row(tableView, id: "choice", style: .subtitle)
        cell.accessoryType = .none
        cell.textLabel?.textColor = Theme.label
        cell.detailTextLabel?.font = UIFont.preferredFont(forTextStyle: .footnote)
        cell.detailTextLabel?.adjustsFontForContentSizeCategory = true
        switch indexPath.section {
        case 0:
            if indexPath.row < fixed.count {
                let (code, title) = fixed[indexPath.row]
                cell.textLabel?.text = title
                cell.detailTextLabel?.text = Say.choiceDetail(code: Int(code), autoApply: autoApply)
                cell.accessoryType = output.choice == Int(code) ? .checkmark : .none
            } else {
                let name = profiles[indexPath.row - fixed.count]
                cell.textLabel?.text = name
                cell.detailTextLabel?.text = Say.savedProfile
                cell.accessoryType = output.choice == 3 && output.profile == name ? .checkmark : .none
            }
        case 1:
            if indexPath.row < curves.count {
                let c = curves[indexPath.row]
                cell.textLabel?.text = c.name
                cell.detailTextLabel?.text = Say.autoeqShort(c)
            } else {
                cell.textLabel?.text = listed > 0 ? Say.autoeqSearch(listed) : Say.downloadTheList
                cell.detailTextLabel?.text = nil
                cell.accessoryType = listed > 0 ? .disclosureIndicator : .none
            }
        default:
            cell.textLabel?.text = Say.forgetDevice
            cell.detailTextLabel?.text = nil
        }
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        let key = output.key
        switch indexPath.section {
        case 0:
            let (code, profile): (Int32, String) = indexPath.row < fixed.count
                ? (fixed[indexPath.row].code, "")
                : (3, profiles[indexPath.row - fixed.count])
            run(curve: false) { key.withCString { k in profile.withCString { p in nori_ios_device_assign(k, code, p) } } }
        case 1 where indexPath.row < curves.count:
            let c = curves[indexPath.row]
            run(curve: true) { key.withCString { k in c.withC { f in nori_ios_device_adopt(k, f[0], f[1], f[2], f[3], f[4]) } } }
        case 1 where listed > 0:
            navigationController?.pushViewController(AutoEqPage(for: output), animated: true)
        case 1:
            busy(self, true)
            DispatchQueue.global(qos: .userInitiated).async {
                let n = nori_ios_autoeq_update()
                DispatchQueue.main.async {
                    busy(self, false)
                    if n < 0 { Toast.show(Say.failed) }
                    self.load()
                }
            }
        default:
            key.withCString { nori_ios_device_forget($0) }
            navigationController?.popViewController(animated: true)
        }
    }

    /// Runs a choice off the main thread, then goes back to the list. `curve`: the door answers 0 when
    /// AutoEQ has no curve, rather than for a choice not kept.
    private func run(curve: Bool, _ choose: @escaping () -> Int32) {
        busy(self, true)
        DispatchQueue.global(qos: .userInitiated).async {
            let kept = choose()
            DispatchQueue.main.async {
                busy(self, false)
                switch kept {
                case 1: self.navigationController?.popViewController(animated: true)
                case 0 where curve: Toast.show(Say.autoeqNoCurve); self.load()
                default: Toast.show(Say.failed)
                }
            }
        }
    }
}

/// The AutoEQ list: search it, and tap a curve to make it the current sound (`output` nil, as Android's
/// browser) or the sound of `output`.
final class AutoEqPage: UITableViewController, UISearchBarDelegate {
    private let output: Output?
    private let search = UISearchBar()
    private var count = -1
    private var short = true
    private var hits: [Curve] = []
    private var asked = ""
    private var refresh: UIBarButtonItem?

    init(for output: Output?) {
        self.output = output
        super.init(style: .plain)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = Say.headphonePresets
        navigationItem.largeTitleDisplayMode = .never
        dress(tableView)
        tableView.estimatedRowHeight = 52
        tableView.keyboardDismissMode = .onDrag
        search.barStyle = Theme.bar
        search.keyboardAppearance = Theme.keyboard
        search.tintColor = Theme.accent
        search.delegate = self
        search.sizeToFit()
        query("")
    }

    func searchBar(_ searchBar: UISearchBar, textDidChange searchText: String) { query(searchText) }

    func searchBarSearchButtonClicked(_ searchBar: UISearchBar) { searchBar.resignFirstResponder() }

    /// The list is local: each key asks it, and only the last answer is shown.
    private func query(_ text: String) {
        asked = text
        DispatchQueue.global(qos: .userInitiated).async {
            let d = text.withCString { takenJSON(nori_ios_autoeq_browse($0)) } as? [String: Any] ?? [:]
            DispatchQueue.main.async {
                guard self.asked == text else { return }
                self.count = d["count"] as? Int ?? 0
                self.short = d["short"] as? Bool ?? true
                self.hits = (d["hits"] as? [[String: Any]] ?? []).map(Curve.init)
                self.paint()
            }
        }
    }

    private func paint() {
        let listed = count > 0
        search.placeholder = Say.autoeqSearch(count)
        tableView.tableHeaderView = listed ? search : nil
        if listed && output == nil && refresh == nil {
            refresh = UIBarButtonItem(title: Say.refreshList, style: .plain, target: self, action: #selector(download))
            navigationItem.rightBarButtonItem = refresh
        }
        tableView.reloadData()
    }

    @objc private func download() {
        busy(self, true)
        DispatchQueue.global(qos: .userInitiated).async {
            let n = nori_ios_autoeq_update()
            DispatchQueue.main.async {
                busy(self, false, after: self.refresh)
                if n < 0 { Toast.show(Say.failed) }
                self.query(self.asked)
            }
        }
    }

    // Unlisted: the download, with what AutoEQ is under it. Listed: the hits, or "Nothing matches".
    override func numberOfSections(in tableView: UITableView) -> Int { 1 }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        if count == 0 { return 1 }
        return hits.isEmpty && !short ? 1 : hits.count
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = row(tableView, id: "curve", style: .subtitle)
        cell.backgroundColor = Theme.background
        cell.detailTextLabel?.font = UIFont.preferredFont(forTextStyle: .footnote)
        cell.detailTextLabel?.adjustsFontForContentSizeCategory = true
        cell.selectionStyle = .default
        if count == 0 {
            // What the list is, under the one thing to do with it.
            cell.textLabel?.text = Say.downloadTheList
            cell.detailTextLabel?.text = Say.autoeqAbout
            cell.detailTextLabel?.numberOfLines = 0
        } else if hits.isEmpty {
            cell.textLabel?.text = Say.nothingMatches
            cell.detailTextLabel?.text = nil
            cell.selectionStyle = .none
        } else {
            let c = hits[indexPath.row]
            cell.textLabel?.text = c.name
            cell.detailTextLabel?.text = Say.autoeqCaption(c)
        }
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        if count == 0 { return download() }
        guard indexPath.row < hits.count else { return }
        search.resignFirstResponder()
        let c = hits[indexPath.row]
        let key = output?.key
        busy(self, true)
        DispatchQueue.global(qos: .userInitiated).async {
            let kept: Int32 = c.withC { f in
                guard let key else { return nori_ios_autoeq_apply(f[0], f[1], f[2], f[3], f[4]) }
                return key.withCString { nori_ios_device_adopt($0, f[0], f[1], f[2], f[3], f[4]) }
            }
            DispatchQueue.main.async {
                busy(self, false, after: self.refresh)
                switch kept {
                case 1 where key == nil:
                    Toast.show(Say.autoeqApplied(c.name))
                case 1:
                    // Back past the device page to the list, which shows the new choice.
                    if let list = self.navigationController?.viewControllers.first(where: { $0 is SoundPage }) {
                        self.navigationController?.popToViewController(list, animated: true)
                    }
                case 0:
                    Toast.show(Say.autoeqNoCurve)
                    self.query(self.asked)
                default:
                    Toast.show(Say.failed)
                }
            }
        }
    }
}
