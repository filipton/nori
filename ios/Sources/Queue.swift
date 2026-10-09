import UIKit

/// The queue, from the player: the song playing at the top as it opens, what has played above it under
/// "History", what plays next under "Playing next". Shuffle and repeat stay pinned beside the title.
/// Hold a row and drag to move it (when the play order is the list's), swipe to remove, tap to play.
/// The rows, their order and what may move or go are the core's (`queue_rows`). A jam guest's is the
/// host's queue, read only, under the jam: its host and listeners, Listen Here, Leave, and the songs it
/// asked for.
final class QueueSheet: UIViewController, UITableViewDataSource, UITableViewDelegate,
    UITableViewDragDelegate, UITableViewDropDelegate {
    let transition = CardTransition()
    private var closer: DragToClose?
    private let table = UITableView(frame: .zero, style: .plain)
    private let shuffle = UIButton(type: .system)
    private let repeatButton = UIButton(type: .system)
    /// After a swipe: the song's name and Undo, for a few seconds.
    private let undoBar = UIView()
    private let undoText = UILabel()
    private var undoID = ""
    private var undoTimer: Timer?
    private var token: UInt64 = 0
    private var history: [Item] = []
    private var now: [Item] = []
    private var upcoming: [Item] = []
    private var reorderable = false
    private var kept: Set<Int> = []
    /// Another device's queue, mirrored: a tap plays there, nothing is edited from here.
    private var mirrored = false
    /// The jam this iPod is a guest in, over its host's queue.
    private var jam: Jam?
    private var opened = false
    /// What the queue was when last read: a new read only when it changes, not at every position tick.
    private var seen = ""

    private enum Part: Int, CaseIterable { case jam, asked, history, now, next }

    /// The jam part's rows.
    private enum JamRow: Int, CaseIterable { case people, listen, leave }

    init() {
        super.init(nibName: nil, bundle: nil)
        modalPresentationStyle = .overFullScreen
        modalPresentationCapturesStatusBarAppearance = true
        transitioningDelegate = transition
    }

    required init?(coder: NSCoder) { fatalError() }

    deinit { if token != 0 { Core.shared.forget(token) } }

    override var preferredStatusBarStyle: UIStatusBarStyle { .lightContent }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Theme.Card.background

        let grabber = UIButton(type: .system)
        grabber.setImage(Glyph.chevronDown, for: .normal)
        grabber.tintColor = Theme.Card.secondary
        grabber.addTarget(self, action: #selector(close), for: .touchUpInside)
        grabber.accessibilityLabel = Say.close
        let title = UILabel()
        title.text = Say.queue
        title.font = UIFont.systemFont(ofSize: 22, weight: .bold)
        title.textColor = Theme.Card.label
        shuffle.setImage(Glyph.shuffle, for: .normal)
        shuffle.accessibilityLabel = Say.shuffle
        repeatButton.accessibilityLabel = Say.repeatMode
        shuffle.addTarget(self, action: #selector(shuffleTapped), for: .touchUpInside)
        repeatButton.addTarget(self, action: #selector(repeatTapped), for: .touchUpInside)
        let bar = UIStackView(arrangedSubviews: [title, UIView(), shuffle, repeatButton])
        bar.spacing = 4
        bar.alignment = .center

        table.backgroundColor = Theme.Card.background
        table.separatorStyle = .none
        table.rowHeight = 60
        table.dataSource = self
        table.delegate = self
        table.dragDelegate = self
        table.dropDelegate = self
        table.dragInteractionEnabled = true
        table.register(QueueCell.self, forCellReuseIdentifier: QueueCell.id)
        table.contentInset.bottom = 24

        for v in [grabber, bar, table] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        NSLayoutConstraint.activate([
            grabber.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            grabber.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            grabber.widthAnchor.constraint(equalToConstant: 60),
            grabber.heightAnchor.constraint(equalToConstant: 24),
            bar.topAnchor.constraint(equalTo: grabber.bottomAnchor, constant: 4),
            bar.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
            bar.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -8),
            bar.heightAnchor.constraint(equalToConstant: 44),
            shuffle.widthAnchor.constraint(equalToConstant: 44),
            repeatButton.widthAnchor.constraint(equalToConstant: 44),
            table.topAnchor.constraint(equalTo: bar.bottomAnchor, constant: 4),
            table.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            table.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            table.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        undoBar.backgroundColor = Theme.Card.track
        undoBar.layer.cornerRadius = 10
        undoBar.isHidden = true
        undoText.font = UIFont.preferredFont(forTextStyle: .footnote)
        undoText.textColor = Theme.Card.label
        let undo = UIButton(type: .system)
        undo.setTitle(Say.undo, for: .normal)
        undo.tintColor = Theme.Card.label
        undo.titleLabel?.font = UIFont.systemFont(ofSize: 15, weight: .semibold)
        undo.addTarget(self, action: #selector(undoTapped), for: .touchUpInside)
        undo.setContentHuggingPriority(.required, for: .horizontal)
        let undoRow = UIStackView(arrangedSubviews: [undoText, undo])
        undoRow.spacing = 12
        undoRow.alignment = .center
        undoRow.translatesAutoresizingMaskIntoConstraints = false
        undoBar.addSubview(undoRow)
        undoBar.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(undoBar)
        NSLayoutConstraint.activate([
            undoRow.leadingAnchor.constraint(equalTo: undoBar.leadingAnchor, constant: 14),
            undoRow.trailingAnchor.constraint(equalTo: undoBar.trailingAnchor, constant: -10),
            undoRow.topAnchor.constraint(equalTo: undoBar.topAnchor, constant: 4),
            undoRow.bottomAnchor.constraint(equalTo: undoBar.bottomAnchor, constant: -4),
            undoBar.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            undoBar.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
            undoBar.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -12),
            undoBar.heightAnchor.constraint(greaterThanOrEqualToConstant: 44),
        ])

        // A drag down above the list closes the sheet under the finger, as the card does.
        closer = DragToClose(self, transition) { [weak self] pan in
            guard let self else { return false }
            return pan.location(in: self.view).y < self.table.frame.minY
        }
        NotificationCenter.default.addObserver(self, selector: #selector(changed), name: .noriNow, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(jamChanged), name: .noriJam, object: nil)
        paintToggles()
        read()
    }

    /// The jam changed, and with what its host took, its queue.
    @objc private func jamChanged() {
        read()
    }

    @objc private func close() { dismiss(animated: true) }

    /// Pulled down past the first row and let go: the queue closes.
    func scrollViewWillEndDragging(_ scrollView: UIScrollView, withVelocity velocity: CGPoint,
                                   targetContentOffset: UnsafeMutablePointer<CGPoint>) {
        if scrollView.contentOffset.y + scrollView.adjustedContentInset.top < -DragToClose.pull {
            dismiss(animated: true)
        }
    }

    @objc private func changed() {
        paintToggles()
        let n = Core.shared.now
        let key = "\(n.index) \(n.length) \(n.shuffle) \(n.repeatMode) \(n.song?.id ?? "")"
        guard key != seen, !table.hasActiveDrag else { return }
        read()
    }

    private func paintToggles() {
        let n = Core.shared.now
        // A jam's play order is its host's.
        shuffle.isHidden = n.jam
        repeatButton.isHidden = n.jam
        shuffle.tintColor = n.shuffle ? Theme.Card.label : Theme.Card.dim
        repeatButton.tintColor = n.repeatMode == 0 ? Theme.Card.dim : Theme.Card.label
        repeatButton.setImage(n.repeatMode == 1 ? Glyph.repeatOne : Glyph.repeatAll, for: .normal)
        shuffle.accessibilityTraits = n.shuffle ? [.button, .selected] : .button
        repeatButton.accessibilityTraits = n.repeatMode == 0 ? .button : [.button, .selected]
    }

    private func read() {
        let n = Core.shared.now
        seen = "\(n.index) \(n.length) \(n.shuffle) \(n.repeatMode) \(n.song?.id ?? "")"
        if token != 0 { Core.shared.forget(token) }
        token = Core.shared.read(NORI_PAGE_QUEUE) { [weak self] a in self?.took(a) }
    }

    private func took(_ a: PageAnswer) {
        let part = { (key: String) in a.sections.first { $0.key == key }?.items ?? [] }
        history = part("history")
        now = part("now")
        upcoming = part("next")
        reorderable = a.head["reorderable"] as? Bool ?? false
        kept = Set(a.head["kept"] as? [Int] ?? [])
        mirrored = a.head["remote"] as? Bool ?? false
        jam = a.head["jam"] as? Bool == true ? Core.shared.jam : nil
        table.reloadData()
        // Opens on the song playing, what has played above it out of sight.
        if !opened, !now.isEmpty {
            opened = true
            table.scrollToRow(at: IndexPath(row: 0, section: Part.now.rawValue), at: .top, animated: false)
        }
    }

    private func items(_ section: Int) -> [Item] {
        switch Part(rawValue: section) {
        case .jam: return []
        case .asked: return jam?.asks ?? []
        case .history: return history
        case .now: return now
        default: return upcoming
        }
    }

    @objc private func shuffleTapped() {
        nori_ios_shuffle(Core.shared.now.shuffle ? 0 : 1)
        Core.shared.refresh()
    }

    @objc private func repeatTapped() {
        let next: Int32 = [0: 2, 2: 1, 1: 0][Core.shared.now.repeatMode] ?? 0
        nori_ios_set_repeat(next)
        Core.shared.refresh()
    }

    @objc private func clearTapped() {
        nori_ios_clear_upcoming()
        read()
    }

    // MARK: the list

    func numberOfSections(in tableView: UITableView) -> Int { Part.allCases.count }

    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        Part(rawValue: section) == .jam ? (jam == nil ? 0 : JamRow.allCases.count) : items(section).count
    }

    func tableView(_ tableView: UITableView, heightForHeaderInSection section: Int) -> CGFloat {
        switch Part(rawValue: section) {
        case .asked: return items(section).isEmpty ? 0 : 44
        case .history: return history.isEmpty ? 0 : 36
        case .next: return upcoming.isEmpty ? 0 : 44
        default: return 0
        }
    }

    func tableView(_ tableView: UITableView, viewForHeaderInSection section: Int) -> UIView? {
        let part = Part(rawValue: section)
        guard part != .now, part != .jam, !items(section).isEmpty else { return nil }
        let header = UIView()
        header.backgroundColor = Theme.Card.background
        let label = UILabel()
        label.text = part == .asked ? Say.youAskedFor : part == .history ? Say.history : Say.upNext
        label.font = UIFont.systemFont(ofSize: 17, weight: .semibold)
        label.textColor = Theme.Card.label
        label.translatesAutoresizingMaskIntoConstraints = false
        header.addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: header.leadingAnchor, constant: 16),
            label.bottomAnchor.constraint(equalTo: header.bottomAnchor, constant: -6),
        ])
        if part == .next && !mirrored {
            let clear = UIButton(type: .system)
            clear.setTitle(Say.clear, for: .normal)
            clear.tintColor = Theme.Card.secondary
            clear.addTarget(self, action: #selector(clearTapped), for: .touchUpInside)
            clear.translatesAutoresizingMaskIntoConstraints = false
            header.addSubview(clear)
            NSLayoutConstraint.activate([
                clear.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -16),
                clear.centerYAnchor.constraint(equalTo: label.centerYAnchor),
            ])
        }
        return header
    }

    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let part = Part(rawValue: indexPath.section)
        if part == .jam, let jam {
            return jamCell(tableView, JamRow(rawValue: indexPath.row) ?? .people, jam)
        }
        let cell = tableView.dequeueReusableCell(withIdentifier: QueueCell.id, for: indexPath) as! QueueCell
        let item = items(indexPath.section)[indexPath.row]
        if part == .asked {
            cell.show(item, played: false, playing: false, movable: false, note: Say.waitingFor(jam?.host ?? ""))
        } else {
            cell.show(item, played: part == .history, playing: part == .now, movable: part == .next && reorderable)
        }
        return cell
    }

    /// The jam's rows: its host and who listens, Listen Here (and why it does not play), Leave.
    private func jamCell(_ tableView: UITableView, _ row: JamRow, _ jam: Jam) -> UITableViewCell {
        let cell = tableView.dequeueReusableCell(withIdentifier: "jam") ?? UITableViewCell(style: .subtitle, reuseIdentifier: "jam")
        cell.backgroundColor = Theme.Card.background
        cell.selectionStyle = .none
        cell.accessoryView = nil
        cell.textLabel?.textColor = Theme.Card.label
        cell.textLabel?.font = UIFont.systemFont(ofSize: 17, weight: .regular)
        cell.detailTextLabel?.textColor = Theme.Card.secondary
        cell.detailTextLabel?.font = UIFont.preferredFont(forTextStyle: .footnote)
        cell.detailTextLabel?.numberOfLines = 0
        switch row {
        case .people:
            cell.textLabel?.text = Say.jamOf(jam.host)
            cell.textLabel?.font = UIFont.systemFont(ofSize: 17, weight: .semibold)
            cell.detailTextLabel?.text = ([Say.listening(jam.listeners.count)] + jam.listeners).joined(separator: " · ")
        case .listen:
            cell.textLabel?.text = jam.listening == 1 ? Say.playingHere : Say.listenHere
            cell.detailTextLabel?.text = Say.jamAlong(jam.listening)
            let toggle = UISwitch()
            toggle.isOn = jam.listening != 0
            toggle.onTintColor = Theme.switchOn
            toggle.addTarget(self, action: #selector(listenSwitched(_:)), for: .valueChanged)
            cell.accessoryView = toggle
        case .leave:
            cell.textLabel?.text = Say.leaveJam
            cell.textLabel?.textColor = .systemRed
            cell.detailTextLabel?.text = nil
            cell.selectionStyle = .default
        }
        return cell
    }

    @objc private func listenSwitched(_ s: UISwitch) {
        nori_ios_jam_listen(s.isOn ? 1 : 0)
    }

    func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        if Part(rawValue: indexPath.section) == .jam, JamRow(rawValue: indexPath.row) == .leave {
            nori_ios_jam_leave()
            return dismiss(animated: true)
        }
        // The jam's rows, its requests and its host's queue: nothing a guest plays.
        guard jam == nil else { return }
        _ = nori_ios_play_at(Int32(items(indexPath.section)[indexPath.row].index), 0)
    }

    func tableView(_ tableView: UITableView, trailingSwipeActionsConfigurationForRowAt indexPath: IndexPath) -> UISwipeActionsConfiguration? {
        guard let part = Part(rawValue: indexPath.section), [.history, .now, .next].contains(part), jam == nil else { return nil }
        let item = items(indexPath.section)[indexPath.row]
        guard !kept.contains(item.index) else { return nil }
        let remove = UIContextualAction(style: .destructive, title: Say.remove) { [weak self] _, _, done in
            nori_ios_remove(Int32(item.index))
            done(true)
            self?.read()
            self?.offerUndo(item)
        }
        return UISwipeActionsConfiguration(actions: [remove])
    }

    // MARK: taking a removal back

    private func offerUndo(_ item: Item) {
        undoID = item.id
        undoText.text = Say.queueRemoved(item.title.isEmpty ? item.id : item.title)
        undoBar.isHidden = false
        undoTimer?.invalidate()
        undoTimer = Timer.scheduledTimer(withTimeInterval: 4, repeats: false) { [weak self] _ in self?.undoBar.isHidden = true }
    }

    /// The core puts the song back where it was (`put_back`).
    @objc private func undoTapped() {
        undoTimer?.invalidate()
        undoBar.isHidden = true
        undoID.withCString { nori_ios_put_back($0) }
        read()
    }

    // MARK: moving a song: held and dragged, dropped only among those still to come

    func tableView(_ tableView: UITableView, itemsForBeginning session: UIDragSession, at indexPath: IndexPath) -> [UIDragItem] {
        guard reorderable, Part(rawValue: indexPath.section) == .next else { return [] }
        return [UIDragItem(itemProvider: NSItemProvider())]
    }

    func tableView(_ tableView: UITableView, dropSessionDidUpdate session: UIDropSession, withDestinationIndexPath destination: IndexPath?) -> UITableViewDropProposal {
        guard session.localDragSession != nil, let d = destination, Part(rawValue: d.section) == .next else {
            return UITableViewDropProposal(operation: .forbidden)
        }
        return UITableViewDropProposal(operation: .move, intent: .insertAtDestinationIndexPath)
    }

    func tableView(_ tableView: UITableView, performDropWith coordinator: UITableViewDropCoordinator) {}

    func tableView(_ tableView: UITableView, canMoveRowAt indexPath: IndexPath) -> Bool {
        reorderable && Part(rawValue: indexPath.section) == .next
    }

    func tableView(_ tableView: UITableView, targetIndexPathForMoveFromRowAt source: IndexPath, toProposedIndexPath proposed: IndexPath) -> IndexPath {
        Part(rawValue: proposed.section) == .next ? proposed : IndexPath(row: 0, section: Part.next.rawValue)
    }

    func tableView(_ tableView: UITableView, moveRowAt source: IndexPath, to destination: IndexPath) {
        guard source != destination else { return }
        // In list order (the play order, when moving is offered): the song lands where the row it
        // displaced was.
        let from = upcoming[source.row].index
        let to = upcoming[destination.row].index
        upcoming.insert(upcoming.remove(at: source.row), at: destination.row)
        nori_ios_move(Int32(from), Int32(to))
        read()
    }
}

/// A queue row: cover, title, artist; quieter once played, lit while playing, a handle when it can move.
final class QueueCell: UITableViewCell {
    static let id = "queue"
    private let cover = CoverView()
    private let title = UILabel()
    private let detail = UILabel()
    private let grip = UIImageView(image: Glyph.grip)

    override init(style: UITableViewCell.CellStyle, reuseIdentifier: String?) {
        super.init(style: style, reuseIdentifier: reuseIdentifier)
        backgroundColor = Theme.Card.background
        let selected = UIView()
        selected.backgroundColor = Theme.Card.track
        selectedBackgroundView = selected
        title.font = UIFont.preferredFont(forTextStyle: .body)
        title.adjustsFontForContentSizeCategory = true
        detail.font = UIFont.preferredFont(forTextStyle: .footnote)
        detail.adjustsFontForContentSizeCategory = true
        grip.tintColor = Theme.Card.dim
        grip.setContentHuggingPriority(.required, for: .horizontal)
        let text = UIStackView(arrangedSubviews: [title, detail])
        text.axis = .vertical
        text.spacing = 2
        for v in [cover, text, grip] {
            v.translatesAutoresizingMaskIntoConstraints = false
            contentView.addSubview(v)
        }
        NSLayoutConstraint.activate([
            cover.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            cover.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            cover.widthAnchor.constraint(equalToConstant: 44),
            cover.heightAnchor.constraint(equalToConstant: 44),
            text.leadingAnchor.constraint(equalTo: cover.trailingAnchor, constant: 12),
            text.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            text.trailingAnchor.constraint(equalTo: grip.leadingAnchor, constant: -8),
            grip.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -16),
            grip.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    /// `note` in place of the artist; who asked for a jam's song after it.
    func show(_ item: Item, played: Bool, playing: Bool, movable: Bool, note: String? = nil) {
        cover.show(item.cover, points: 44)
        title.text = item.title.isEmpty ? item.id : item.title
        detail.text = note ?? ([item.subtitle, item.by].filter { !$0.isEmpty }.joined(separator: " · "))
        title.font = UIFont.systemFont(ofSize: 17, weight: playing ? .semibold : .regular)
        title.textColor = Theme.Card.label.withAlphaComponent(played ? 0.45 : 1)
        detail.textColor = Theme.Card.secondary.withAlphaComponent(played ? 0.6 : 1)
        cover.alpha = played ? 0.5 : 1
        grip.isHidden = !movable
    }
}
