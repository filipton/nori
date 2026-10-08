import UIKit

/// Home, Library, Settings and Search, as on Android, with the mini player over the tab bar.
final class ShellController: UITabBarController {
    private let mini = MiniPlayer()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Theme.background
        Theme.apply(tab: tabBar)
        viewControllers = [
            tab(Say.home, Glyph.home, PageController(kind: NORI_PAGE_HOME, title: Say.home)),
            tab(Say.library, Glyph.library, LibraryPage()),
            tab(Say.settingsTitle, Glyph.settings, SettingsPage()),
            tab(Say.search, Glyph.search, SearchPage()),
        ]
        mini.opened = { [weak self] in self?.openPlayer() }
        mini.dragged = { [weak self] in self?.dragPlayer($0) }
        view.addSubview(mini)
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        let height = MiniPlayer.height
        mini.frame = CGRect(x: 0, y: tabBar.frame.minY - height, width: view.bounds.width, height: height)
        view.bringSubviewToFront(mini)
    }

    func openPlayer() {
        guard Core.shared.now.song != nil else { return }
        present(PlayerCard(), animated: true)
    }

    /// The card's opening while a finger drags the mini player up.
    private var rising: UIPercentDrivenInteractiveTransition?

    /// Follows a drag up from the mini player: the card's top under the finger, then open or back down
    /// as far and fast as it went.
    private func dragPlayer(_ g: UIPanGestureRecognizer) {
        let height = view.bounds.height
        let lift = -g.translation(in: view).y
        switch g.state {
        case .began:
            guard Core.shared.now.song != nil, presentedViewController == nil else { return }
            let card = PlayerCard()
            let drive = UIPercentDrivenInteractiveTransition()
            card.transition.interaction = drive
            rising = drive
            present(card, animated: true)
        case .changed:
            rising?.update(min(1, max(0, lift / height)))
        case .ended, .cancelled:
            guard let drive = rising else { return }
            rising = nil
            let opens = g.state == .ended && (lift > height * 0.3 || g.velocity(in: view).y < -800)
            if opens { drive.finish() } else { drive.cancel() }
        default:
            break
        }
    }

    private func tab(_ title: String, _ image: UIImage, _ root: UIViewController) -> UINavigationController {
        root.title = title
        let nav = UINavigationController(rootViewController: root)
        nav.tabBarItem = UITabBarItem(title: title, image: image, selectedImage: nil)
        Theme.apply(nav: nav.navigationBar)
        nav.view.backgroundColor = Theme.background
        return nav
    }
}

/// Sits above the tab bar: the song, the heart, play/pause, next and a progress hairline. Tap, or drag it
/// up, for the card; swipe it left or right for the next or previous song.
final class MiniPlayer: UIView {
    static let height: CGFloat = 56

    var opened: (() -> Void)?
    var dragged: ((UIPanGestureRecognizer) -> Void)?
    private let cover = CoverView()
    private let title = UILabel()
    private let artist = UILabel()
    private let heart = UIButton(type: .system)
    private let play = UIButton(type: .system)
    private let skipButton = UIButton(type: .system)
    /// Which way the drag under way goes, decided as it starts: up for the card, sideways to skip.
    private var sideways = false
    /// The cover and the song's lines: what follows a sideways swipe.
    private var sliding: [UIView] = []
    private let progress = UIView()
    private var progressWidth: NSLayoutConstraint!
    private var ticker: Timer?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = Theme.row

        let hairline = UIView()
        hairline.backgroundColor = Theme.hairline
        title.textColor = Theme.label
        title.font = UIFont.preferredFont(forTextStyle: .subheadline)
        title.adjustsFontForContentSizeCategory = true
        artist.textColor = Theme.secondary
        artist.font = UIFont.preferredFont(forTextStyle: .caption1)
        artist.adjustsFontForContentSizeCategory = true
        let text = UIStackView(arrangedSubviews: [title, artist])
        text.axis = .vertical
        text.spacing = 1
        play.setImage(Glyph.play, for: .normal)
        play.tintColor = Theme.label
        play.addTarget(self, action: #selector(toggle), for: .touchUpInside)
        skipButton.setImage(Glyph.next, for: .normal)
        skipButton.accessibilityLabel = Say.next
        skipButton.tintColor = Theme.label
        skipButton.addTarget(self, action: #selector(skip), for: .touchUpInside)
        heart.tintColor = Theme.label
        heart.accessibilityLabel = Say.favorite
        heart.addTarget(self, action: #selector(heartTapped), for: .touchUpInside)
        progress.backgroundColor = Theme.accent

        sliding = [cover, text]
        // The song's side, clipped where the controls start: a swiped song leaves at that edge rather
        // than passing under the heart and the buttons.
        let lane = UIView()
        lane.clipsToBounds = true
        lane.isUserInteractionEnabled = false
        for v in [hairline, lane, heart, play, skipButton, progress] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        for v in [cover, text] {
            v.translatesAutoresizingMaskIntoConstraints = false
            lane.addSubview(v)
        }
        progressWidth = progress.widthAnchor.constraint(equalToConstant: 0)
        NSLayoutConstraint.activate([
            hairline.leadingAnchor.constraint(equalTo: leadingAnchor),
            hairline.trailingAnchor.constraint(equalTo: trailingAnchor),
            hairline.topAnchor.constraint(equalTo: topAnchor),
            hairline.heightAnchor.constraint(equalToConstant: 1.0 / UIScreen.main.scale),
            lane.leadingAnchor.constraint(equalTo: leadingAnchor),
            lane.trailingAnchor.constraint(equalTo: heart.leadingAnchor),
            lane.topAnchor.constraint(equalTo: topAnchor),
            lane.bottomAnchor.constraint(equalTo: bottomAnchor),
            cover.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            cover.centerYAnchor.constraint(equalTo: centerYAnchor),
            cover.widthAnchor.constraint(equalToConstant: 40),
            cover.heightAnchor.constraint(equalToConstant: 40),
            text.leadingAnchor.constraint(equalTo: cover.trailingAnchor, constant: 10),
            text.centerYAnchor.constraint(equalTo: centerYAnchor),
            text.trailingAnchor.constraint(lessThanOrEqualTo: heart.leadingAnchor, constant: -4),
            heart.trailingAnchor.constraint(equalTo: play.leadingAnchor),
            heart.centerYAnchor.constraint(equalTo: centerYAnchor),
            heart.widthAnchor.constraint(equalToConstant: 40),
            heart.heightAnchor.constraint(equalToConstant: 44),
            skipButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            skipButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            skipButton.widthAnchor.constraint(equalToConstant: 44),
            skipButton.heightAnchor.constraint(equalToConstant: 44),
            play.trailingAnchor.constraint(equalTo: skipButton.leadingAnchor),
            play.centerYAnchor.constraint(equalTo: centerYAnchor),
            play.widthAnchor.constraint(equalToConstant: 44),
            play.heightAnchor.constraint(equalToConstant: 44),
            progress.leadingAnchor.constraint(equalTo: leadingAnchor),
            progress.bottomAnchor.constraint(equalTo: bottomAnchor),
            progress.heightAnchor.constraint(equalToConstant: 2),
            progressWidth,
        ])
        addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(tapped)))
        addGestureRecognizer(UIPanGestureRecognizer(target: self, action: #selector(panned(_:))))
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(changed), name: .noriNow, object: nil)
        center.addObserver(self, selector: #selector(paintHeart), name: .noriFavorites, object: nil)
        center.addObserver(self, selector: #selector(changed), name: UIApplication.didBecomeActiveNotification, object: nil)
        center.addObserver(self, selector: #selector(stopTicking), name: UIApplication.didEnterBackgroundNotification, object: nil)
        changed()
    }

    required init?(coder: NSCoder) { fatalError() }

    @objc private func changed() {
        let now = Core.shared.now
        if let song = now.song {
            title.text = song.title
            artist.text = now.device.map(Say.playingOn) ?? song.subtitle
            artist.isHidden = false
            cover.show(song.cover, points: 40)
        } else {
            title.text = Say.nothingPlaying
            artist.isHidden = true
            cover.show("", points: 40)
        }
        play.isEnabled = now.song != nil
        skipButton.isEnabled = now.song != nil
        paintHeart()
        play.setImage(now.playing ? Glyph.pause : Glyph.play, for: .normal)
        play.accessibilityLabel = now.playing ? Say.pause : Say.play
        paintProgress()
        let active = UIApplication.shared.applicationState == .active
        if now.playing && active {
            if ticker == nil {
                ticker = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.paintProgress() }
                ticker?.tolerance = 0.3
            }
        } else {
            stopTicking()
        }
    }

    @objc private func stopTicking() {
        ticker?.invalidate()
        ticker = nil
    }

    private func paintProgress() {
        let now = Core.shared.now
        let total = (now.song?.seconds ?? 0) * 1000
        let part = total > 0 ? min(1, CGFloat(now.position) / CGFloat(total)) : 0
        progressWidth.constant = bounds.width * part
    }

    @objc private func toggle() {
        nori_ios_toggle()
    }

    @objc private func paintHeart() {
        guard let song = Core.shared.now.song, !song.external else {
            heart.isHidden = true
            return
        }
        heart.isHidden = false
        let on = Core.shared.isFavorite(song)
        heart.setImage(on ? Glyph.heartSmall : Glyph.heartSmallOutline, for: .normal)
        heart.accessibilityTraits = on ? [.button, .selected] : .button
    }

    @objc private func heartTapped() {
        guard let song = Core.shared.now.song else { return }
        Core.shared.favorite(song, !Core.shared.isFavorite(song))
        paintHeart()
    }

    @objc private func skip() {
        nori_ios_next()
    }

    @objc private func tapped() {
        opened?()
    }

    @objc private func panned(_ g: UIPanGestureRecognizer) {
        if g.state == .began {
            let v = g.velocity(in: self)
            sideways = abs(v.x) > abs(v.y) && Core.shared.now.song != nil
        }
        guard sideways else { return dragged?(g) ?? () }
        let dx = g.translation(in: self).x
        let moving = sliding
        switch g.state {
        case .changed:
            moving.forEach { $0.transform = CGAffineTransform(translationX: dx, y: 0) }
        case .ended, .cancelled:
            let vx = g.velocity(in: self).x
            let goes = g.state == .ended && (abs(dx) > bounds.width * 0.25 || abs(vx) > 700)
            guard goes else {
                UIView.animate(withDuration: 0.25) { moving.forEach { $0.transform = .identity } }
                return
            }
            // Left for the next song, right for the previous; the new one comes in from the other side.
            let left = (dx != 0 ? dx : vx) < 0
            let away = left ? -bounds.width : bounds.width
            let plain = UIAccessibility.isReduceMotionEnabled
            UIView.animate(withDuration: plain ? 0 : 0.18, animations: {
                moving.forEach { $0.transform = CGAffineTransform(translationX: away, y: 0) }
            }, completion: { _ in
                if left { nori_ios_next() } else { nori_ios_previous() }
                moving.forEach { $0.transform = CGAffineTransform(translationX: -away, y: 0) }
                UIView.animate(withDuration: plain ? 0 : 0.22) { moving.forEach { $0.transform = .identity } }
            })
        default:
            break
        }
    }
}

final class LibraryPage: UITableViewController {
    private let rows: [(String, Int32)] = [
        (Say.playlists, NORI_PAGE_PLAYLISTS),
        (Say.artists, NORI_PAGE_ARTISTS),
        (Say.albumsTitle, NORI_PAGE_ALBUMS),
        (Say.songsTitle, NORI_PAGE_SONGS),
        (Say.genres, NORI_PAGE_GENRES),
        (Say.favorites, NORI_PAGE_STARRED),
        (Say.smartPlaylists, NORI_PAGE_SMARTS),
        (Say.downloaded, NORI_PAGE_DOWNLOADS),
    ]

    init() { super.init(style: .grouped) }
    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        dress(tableView)
    }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { rows.count }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = row(tableView, id: "library")
        cell.textLabel?.text = rows[indexPath.row].0
        cell.accessoryType = .disclosureIndicator
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        let (name, kind) = rows[indexPath.row]
        let page = kind == NORI_PAGE_DOWNLOADS ? DownloadsPage() : PageController(kind: kind, title: name)
        navigationController?.pushViewController(page, animated: true)
    }
}

/// Downloads, with the line saying they run only while the app is open or playing.
final class DownloadsPage: PageController {
    init() { super.init(kind: NORI_PAGE_DOWNLOADS, title: Say.downloaded) }
    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        let note = UILabel(frame: CGRect(x: 0, y: 0, width: UIScreen.main.bounds.width, height: 44))
        note.text = Say.downloadsRun
        note.textColor = Theme.secondary
        note.font = UIFont.preferredFont(forTextStyle: .footnote)
        note.adjustsFontForContentSizeCategory = true
        note.textAlignment = .center
        tableView.tableFooterView = note
    }

    /// Read again a second on while something downloads and the page is in sight.
    private var again: Timer?
    /// A row is swiped open: reading again would reload the table and close it, so it waits.
    private var swiped = false

    override func took(_ a: PageAnswer, appending: Bool) {
        super.took(a, appending: appending)
        again?.invalidate()
        again = nil
        guard a.raw["running"] as? Bool == true, view.window != nil else { return }
        again = Timer.scheduledTimer(withTimeInterval: 1, repeats: false) { [weak self] _ in
            guard let self, !self.swiped else { return }
            self.reload()
        }
        again?.tolerance = 0.2
    }

    override func tableView(_ tableView: UITableView, willBeginEditingRowAt indexPath: IndexPath) {
        swiped = true
    }

    override func tableView(_ tableView: UITableView, didEndEditingRowAt indexPath: IndexPath?) {
        swiped = false
        if answer?.raw["running"] as? Bool == true, again?.isValid != true { reload() }
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        if answer != nil { reload() }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        again?.invalidate()
        again = nil
    }
}

/// The index at every key, the server after a typing pause.
final class SearchPage: PageController, UISearchResultsUpdating, UISearchBarDelegate, UISearchControllerDelegate {
    private let search = UISearchController(searchResultsController: nil)
    private var pending: DispatchWorkItem?
    private var typed = ""
    /// The pause before the server is asked (the `liveSearchDelayMs` setting), seconds.
    private var delay = 0.35

    init() { super.init(kind: NORI_PAGE_SEARCH, title: Say.search) }
    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        refreshControl = nil
        search.obscuresBackgroundDuringPresentation = false
        search.searchBar.placeholder = Say.searchHint
        search.searchBar.barStyle = Theme.bar
        search.searchBar.keyboardAppearance = Theme.keyboard
        search.searchBar.tintColor = Theme.accent
        search.searchResultsUpdater = self
        search.searchBar.delegate = self
        search.delegate = self
        navigationItem.searchController = search
        navigationItem.hidesSearchBarWhenScrolling = false
        definesPresentationContext = true
        DispatchQueue.global(qos: .utility).async {
            guard let ms = SettingsPage.number("liveSearchDelayMs") else { return }
            DispatchQueue.main.async { self.delay = Double(ms) / 1000 }
        }
    }

    /// Opened with nothing typed (the tab tapped): the field takes the keyboard at once. iOS 12 ignores
    /// both steps until the bar is in place, so each waits a turn of the run loop.
    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        guard (search.searchBar.text ?? "").isEmpty else { return }
        DispatchQueue.main.async { self.search.isActive = true }
    }

    func didPresentSearchController(_ searchController: UISearchController) {
        DispatchQueue.main.async { searchController.searchBar.becomeFirstResponder() }
    }

    override func reload() {
        guard Core.shared.isOpen else { return super.reload() }
        query(typed)
    }

    private func query(_ text: String) {
        if token != 0 { Core.shared.forget(token) }
        Core.shared.read(NORI_PAGE_SEARCH, text, token: token == 0 ? nil : token) { [weak self] a in
            self?.took(a, appending: false)
        }
    }

    override func took(_ a: PageAnswer, appending: Bool) {
        if let recent = a.raw["recent"] as? [String], typed.isEmpty {
            let items = recent.map { ["k": "recent", "id": $0, "t": $0] }
            super.took(PageAnswer(["sections": items.isEmpty ? [] : [["key": "", "items": items]]]), appending: false)
            return
        }
        super.took(a, appending: appending)
    }

    func updateSearchResults(for searchController: UISearchController) {
        let text = searchController.searchBar.text ?? ""
        guard text != typed else { return }
        typed = text
        pending?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.query(text) }
        pending = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
    }

    func searchBarSearchButtonClicked(_ searchBar: UISearchBar) {
        let text = searchBar.text ?? ""
        text.withCString { nori_ios_remember($0) }
        pending?.cancel()
        query(text)
    }

    override func open(_ item: Item) {
        if item.kind == "recent" {
            search.searchBar.text = item.id
            search.isActive = true
            return
        }
        if !typed.isEmpty { typed.withCString { nori_ios_remember($0) } }
        super.open(item)
    }
}

final class SettingsPage: UITableViewController {
    private var server = Say.none
    private var values: [String: [String: Any]] = [:]
    private var facts: [String: Any] = [:]
    private var sections: [(title: String, rows: [(name: String, label: String)])] = []

    init() { super.init(style: .grouped) }
    required init?(coder: NSCoder) { fatalError() }

    /// A setting's value, from the open session.
    static func value(_ name: String) -> String? {
        guard let rows = takenJSON(nori_ios_settings()) as? [[String: Any]],
              let row = rows.first(where: { $0["name"] as? String == name }) else { return nil }
        return row["value"] as? String
    }

    static func number(_ name: String) -> Int? { value(name).flatMap { Int($0) } }

    override func viewDidLoad() {
        super.viewDidLoad()
        dress(tableView)
        // Two-line rows measured from a closer guess, and room under the last group, so the end of the
        // list scrolls clear of the mini player.
        tableView.estimatedRowHeight = 52
        tableView.tableFooterView = UIView(frame: CGRect(x: 0, y: 0, width: 0, height: 24))
        NotificationCenter.default.addObserver(self, selector: #selector(load), name: .noriOpened, object: nil)
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        load()
    }

    @objc private func load() {
        let dir = dataDirectory().path
        DispatchQueue.global(qos: .userInitiated).async {
            let label = dir.withCString { taken(nori_ios_active($0)) } ?? ""
            let rows = takenJSON(nori_ios_settings()) as? [[String: Any]] ?? []
            let facts = takenJSON(nori_ios_facts()) as? [String: Any] ?? [:]
            DispatchQueue.main.async {
                self.server = label.isEmpty ? Say.none : label
                self.values = Dictionary(rows.compactMap { r in (r["name"] as? String).map { ($0, r) } }, uniquingKeysWith: { a, _ in a })
                self.facts = facts
                let shown = { (name: String) in
                    self.values[name] != nil && Say.shownWhen[name].map { self.values[$0]?["value"] as? String == "true" } ?? true
                }
                self.sections = Say.settings.map { s in (s.section, s.rows.filter { shown($0.name) }) }.filter { !$0.rows.isEmpty }
                self.tableView.reloadData()
            }
        }
    }

    // Section 0: server, equalizer, sound, sync. Then the curated settings. Last: about.
    override func numberOfSections(in tableView: UITableView) -> Int { sections.count + 2 }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        if section == 0 { return nil }
        if section == sections.count + 1 { return Say.about }
        return sections[section - 1].title
    }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        if section == 0 { return Core.shared.isOpen ? 4 : 1 }
        if section == sections.count + 1 { return 5 }
        return sections[section - 1].rows.count
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = row(tableView, id: "value", style: .value1)
        cell.accessoryView = nil
        cell.accessoryType = .none
        cell.selectionStyle = .default
        if indexPath.section == 0 {
            switch indexPath.row {
            case 0:
                cell.textLabel?.text = Say.server
                cell.detailTextLabel?.text = server
                cell.accessoryType = .disclosureIndicator
            case 1:
                cell.textLabel?.text = Say.equalizer
                cell.detailTextLabel?.text = (values["eq"]?["value"] as? String) == "true" ? Say.on : Say.off
                cell.accessoryType = .disclosureIndicator
            case 2:
                cell.textLabel?.text = Say.sound
                cell.detailTextLabel?.text = nil
                cell.accessoryType = .disclosureIndicator
            default:
                cell.textLabel?.text = Say.refreshIndex
                let songs = facts["songs"] as? Int ?? 0
                cell.detailTextLabel?.text = Say.songs(songs)
            }
            return cell
        }
        if indexPath.section == sections.count + 1 {
            cell.selectionStyle = .none
            switch indexPath.row {
            case 0:
                cell.textLabel?.text = Say.version
                cell.detailTextLabel?.text = String(cString: nori_ios_version())
            case 1:
                cell.textLabel?.text = Say.streamed
                cell.detailTextLabel?.text = Fmt.bytes(facts["stream"] as? Int ?? 0)
            case 3:
                cell.textLabel?.text = Say.freeSoftware
                cell.detailTextLabel?.text = "MIT"
            case 4:
                cell.textLabel?.text = Say.licences
                cell.detailTextLabel?.text = nil
                cell.selectionStyle = .default
                cell.accessoryType = .disclosureIndicator
            default:
                cell.textLabel?.text = Say.theme
                cell.detailTextLabel?.text = Theme.light ? Say.light : Say.dark
                cell.selectionStyle = .default
                cell.accessoryType = .disclosureIndicator
            }
            return cell
        }
        let (name, label) = sections[indexPath.section - 1].rows[indexPath.row]
        let spec = values[name] ?? [:]
        let value = spec["value"] as? String ?? ""
        cell.textLabel?.text = label
        cell.textLabel?.numberOfLines = 2
        if spec["kind"] as? String == "switch" {
            let toggle = UISwitch()
            toggle.isOn = value == "true"
            toggle.onTintColor = Theme.switchOn
            toggle.accessibilityIdentifier = name
            toggle.addTarget(self, action: #selector(switched(_:)), for: .valueChanged)
            cell.accessoryView = toggle
            cell.detailTextLabel?.text = nil
            cell.selectionStyle = .none
        } else {
            cell.detailTextLabel?.text = Say.choice(name, value)
            cell.accessoryType = .disclosureIndicator
        }
        return cell
    }

    @objc private func switched(_ s: UISwitch) {
        guard let name = s.accessibilityIdentifier else { return }
        set(name, s.isOn ? "true" : "false")
    }

    private func set(_ name: String, _ value: String) {
        _ = name.withCString { n in value.withCString { v in nori_ios_set(n, v) } }
        load()
        // The player offers the devices only while remote control is on.
        if name == "remoteControl" { Core.shared.refresh() }
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        if indexPath.section == 0 {
            switch indexPath.row {
            case 0: navigationController?.pushViewController(ServersPage(), animated: true)
            case 1: navigationController?.pushViewController(EqualizerPage(), animated: true)
            case 2: navigationController?.pushViewController(SoundPage(), animated: true)
            default: nori_ios_sync()
            }
            return
        }
        if indexPath.section == sections.count + 1 {
            if indexPath.row == 2 { pickTheme() }
            if indexPath.row == 4 { navigationController?.pushViewController(CreditsPage(), animated: true) }
            return
        }
        let (name, label) = sections[indexPath.section - 1].rows[indexPath.row]
        guard let spec = values[name], let kind = spec["kind"] as? String, kind != "switch" else { return }
        let current = spec["value"] as? String ?? ""
        let options: [String]
        if kind == "choice" {
            options = spec["options"] as? [String] ?? []
        } else {
            let lo = spec["min"] as? Double ?? 0, hi = spec["max"] as? Double ?? 0
            let step = max(1, ((hi - lo) / 8).rounded())
            options = stride(from: lo, through: hi, by: step).map { $0 == $0.rounded() ? "\(Int($0))" : "\($0)" }
        }
        let sheet = UIAlertController.sheet(label)
        for o in options {
            sheet.add(Say.choice(name, o), checked: o == current) { [weak self] in self?.set(name, o) }
        }
        sheet.show(from: self)
    }

    private func pickTheme() {
        let sheet = UIAlertController.sheet(Say.theme)
        for (title, light) in [(Say.light, true), (Say.dark, false)] {
            sheet.add(title, checked: light == Theme.light) {
                guard light != Theme.light else { return }
                _ = "theme".withCString { n in (light ? "LIGHT" : "DARK").withCString { v in nori_ios_set(n, v) } }
                (UIApplication.shared.delegate as? AppDelegate)?.restyle(light: light)
            }
        }
        sheet.show(from: self)
    }
}

/// The graphic equalizer: one switch and a slider per band. While it is in sight and touched, the
/// output runs its short buffer so a move is heard at once.
final class EqualizerPage: UIViewController {
    private let toggle = UISwitch()
    private var sliders: [UISlider] = []
    private let stack = UIStackView()

    override func viewDidLoad() {
        super.viewDidLoad()
        title = Say.equalizer
        navigationItem.largeTitleDisplayMode = .never
        view.backgroundColor = Theme.background
        additionalSafeAreaInsets.bottom = MiniPlayer.height
        toggle.onTintColor = Theme.switchOn
        toggle.addTarget(self, action: #selector(switched), for: .valueChanged)
        navigationItem.rightBarButtonItem = UIBarButtonItem(customView: toggle)
        stack.axis = .vertical
        stack.spacing = 4
        stack.translatesAutoresizingMaskIntoConstraints = false
        let scroll = UIScrollView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scroll)
        scroll.addSubview(stack)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: scroll.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: scroll.trailingAnchor, constant: -16),
            stack.topAnchor.constraint(equalTo: scroll.topAnchor, constant: 16),
            stack.bottomAnchor.constraint(equalTo: scroll.bottomAnchor, constant: -16),
            stack.widthAnchor.constraint(equalTo: scroll.widthAnchor, constant: -32),
        ])
        load()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        nori_ios_tuning(1, 0)
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        nori_ios_tuning(0, 0)
    }

    private func load() {
        guard let eq = takenJSON(nori_ios_equalizer()) as? [String: Any] else {
            let empty = EmptyState(title: Say.noServer, detail: Say.noServerDetail)
            stack.addArrangedSubview(empty)
            toggle.isEnabled = false
            return
        }
        toggle.isOn = eq["on"] as? Bool ?? false
        let presets = PageHeader.pill(Say.presets, Glyph.sort, filled: false)
        presets.heightAnchor.constraint(equalToConstant: 40).isActive = true
        presets.addTarget(self, action: #selector(presetsTapped), for: .touchUpInside)
        stack.addArrangedSubview(presets)
        stack.setCustomSpacing(12, after: presets)
        let gains = eq["gains"] as? [Double] ?? []
        let hz = eq["hz"] as? [Double] ?? []
        for (i, g) in gains.enumerated() {
            let label = UILabel()
            let f = i < hz.count ? hz[i] : 0
            label.text = f >= 1000 ? String(format: "%g kHz", f / 1000) : String(format: "%g Hz", f)
            label.font = UIFont.preferredFont(forTextStyle: .caption1)
            label.adjustsFontForContentSizeCategory = true
            label.textColor = Theme.secondary
            label.widthAnchor.constraint(equalToConstant: 64).isActive = true
            let slider = UISlider()
            slider.minimumValue = -12
            slider.maximumValue = 12
            slider.value = Float(g)
            slider.tag = i
            slider.minimumTrackTintColor = Theme.label
            slider.maximumTrackTintColor = Theme.track
            slider.isEnabled = toggle.isOn
            slider.addTarget(self, action: #selector(moved(_:)), for: .valueChanged)
            sliders.append(slider)
            let row = UIStackView(arrangedSubviews: [label, slider])
            row.spacing = 8
            row.heightAnchor.constraint(equalToConstant: 40).isActive = true
            stack.addArrangedSubview(row)
        }
    }

    @objc private func switched() {
        _ = "eq".withCString { n in (toggle.isOn ? "true" : "false").withCString { v in nori_ios_set(n, v) } }
        sliders.forEach { $0.isEnabled = toggle.isOn }
        nori_ios_tuning(1, 1)
    }

    @objc private func presetsTapped() {
        guard let kinds = takenJSON(nori_ios_presets()) as? [Int] else { return }
        let sheet = UIAlertController.sheet(Say.presets)
        for (i, kind) in kinds.enumerated() {
            sheet.add(Say.preset(kind)) { [weak self] in
                nori_ios_tuning(1, 1)
                if nori_ios_preset(UInt32(i)) == 1 { self?.paint() }
            }
        }
        sheet.show(from: self)
    }

    /// The switch and sliders as the settings now hold them.
    private func paint() {
        guard let eq = takenJSON(nori_ios_equalizer()) as? [String: Any] else { return }
        toggle.isOn = eq["on"] as? Bool ?? false
        let gains = eq["gains"] as? [Double] ?? []
        for (slider, g) in zip(sliders, gains) {
            slider.isEnabled = toggle.isOn
            slider.setValue(Float(g), animated: true)
        }
    }

    @objc private func moved(_ s: UISlider) {
        nori_ios_tuning(1, 1)
        let rounded = (s.value * 2).rounded() / 2
        s.value = nori_ios_equalizer_band(UInt32(s.tag), rounded)
    }
}

final class EmptyState: UIView {
    private let heading = UILabel()
    private let line = UILabel()

    init(title: String, detail: String) {
        super.init(frame: .zero)
        heading.textColor = Theme.label
        heading.font = UIFont.preferredFont(forTextStyle: .title2)
        heading.adjustsFontForContentSizeCategory = true
        heading.textAlignment = .center
        heading.numberOfLines = 0
        line.textColor = Theme.secondary
        line.font = UIFont.preferredFont(forTextStyle: .body)
        line.adjustsFontForContentSizeCategory = true
        line.textAlignment = .center
        line.numberOfLines = 0
        let stack = UIStackView(arrangedSubviews: [heading, line])
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 6
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        set(title: title, detail: detail)
    }

    required init?(coder: NSCoder) { fatalError() }

    func set(title: String, detail: String) {
        heading.text = title
        line.text = detail
        line.isHidden = detail.isEmpty
    }
}

func dress(_ table: UITableView) {
    table.backgroundColor = Theme.background
    table.separatorColor = Theme.hairline
    table.cellLayoutMarginsFollowReadableWidth = false
    // Room under the last row for the mini player, which sits over the tab bar.
    table.contentInset.bottom = MiniPlayer.height
    table.scrollIndicatorInsets.bottom = MiniPlayer.height
}

func row(_ table: UITableView, id: String, style: UITableViewCell.CellStyle = .default) -> UITableViewCell {
    let cell = table.dequeueReusableCell(withIdentifier: id) ?? UITableViewCell(style: style, reuseIdentifier: id)
    cell.backgroundColor = Theme.row
    cell.textLabel?.textColor = Theme.label
    cell.textLabel?.font = UIFont.preferredFont(forTextStyle: .body)
    cell.textLabel?.adjustsFontForContentSizeCategory = true
    cell.detailTextLabel?.textColor = Theme.secondary
    cell.detailTextLabel?.font = UIFont.preferredFont(forTextStyle: .body)
    cell.detailTextLabel?.adjustsFontForContentSizeCategory = true
    let selected = UIView()
    selected.backgroundColor = Theme.track
    cell.selectedBackgroundView = selected
    return cell
}
