import AVKit
import MediaPlayer
import UIKit

/// The system's output picker, which has no call of its own: its button is pressed on the caller's behalf.
final class OutputPicker {
    private let picker = AVRoutePickerView()

    /// Puts the picker into `host`, where it must be for its button to answer, out of sight.
    func attach(to host: UIView) {
        picker.alpha = 0.011
        // Out of sight and out of the way: the host's own touches are its own.
        picker.isUserInteractionEnabled = false
        picker.frame = CGRect(x: 0, y: 0, width: 44, height: 44)
        host.addSubview(picker)
    }

    func show() {
        picker.subviews.compactMap { $0 as? UIButton }.first?.sendActions(for: .touchUpInside)
    }
}

/// The player's output button: this iPod and the account's other devices with nori, each with what it
/// plays, the one playing ticked, then the iPod's own outputs (the system's picker) and joining someone's
/// jam. A tap on a device moves the music there. Open, the other devices are followed.
final class DevicesSheet: UIViewController, UITableViewDataSource, UITableViewDelegate {
    let transition = CardTransition()
    private var closer: DragToClose?
    private let table = UITableView(frame: .zero, style: .plain)
    private let outputs = OutputPicker()
    private var here = true
    private var devices: [[String: Any]] = []

    init() {
        super.init(nibName: nil, bundle: nil)
        modalPresentationStyle = .overFullScreen
        modalPresentationCapturesStatusBarAppearance = true
        transitioningDelegate = transition
    }

    required init?(coder: NSCoder) { fatalError() }

    override var preferredStatusBarStyle: UIStatusBarStyle { Theme.statusBar }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Theme.Card.background
        let grabber = UIButton(type: .system)
        grabber.setImage(Glyph.chevronDown, for: .normal)
        grabber.tintColor = Theme.Card.secondary
        grabber.accessibilityLabel = Say.close
        grabber.addTarget(self, action: #selector(close), for: .touchUpInside)
        let title = UILabel()
        title.text = Say.playOn
        title.font = UIFont.systemFont(ofSize: 22, weight: .bold)
        title.textColor = Theme.Card.label
        table.backgroundColor = Theme.Card.background
        table.separatorColor = Theme.Card.track
        table.rowHeight = 60
        table.dataSource = self
        table.delegate = self
        table.tableFooterView = UIView()
        outputs.attach(to: view)
        for v in [grabber, title, table] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        NSLayoutConstraint.activate([
            grabber.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            grabber.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            grabber.widthAnchor.constraint(equalToConstant: 60),
            grabber.heightAnchor.constraint(equalToConstant: 24),
            title.topAnchor.constraint(equalTo: grabber.bottomAnchor, constant: 4),
            title.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
            title.heightAnchor.constraint(equalToConstant: 44),
            table.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 4),
            table.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            table.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            table.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        closer = DragToClose(self, transition) { [weak self] pan in
            guard let self else { return false }
            return pan.location(in: self.view).y < self.table.frame.minY
        }
        NotificationCenter.default.addObserver(self, selector: #selector(read), name: .noriDevices, object: nil)
        read()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        nori_ios_remote_watch(1)
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        nori_ios_remote_watch(0)
    }

    @objc private func close() { dismiss(animated: true) }

    @objc private func read() {
        let d = takenJSON(nori_ios_remote_devices()) as? [String: Any] ?? [:]
        here = d["here"] as? Bool ?? true
        devices = d["devices"] as? [[String: Any]] ?? []
        table.tableFooterView = devices.isEmpty ? footer(Say.devicesNone) : UIView()
        table.reloadData()
    }

    /// A line of quiet text under the rows.
    private func footer(_ text: String) -> UIView {
        let label = UILabel(frame: CGRect(x: 16, y: 12, width: view.bounds.width - 32, height: 0))
        label.text = text
        label.font = UIFont.preferredFont(forTextStyle: .footnote)
        label.textColor = Theme.Card.secondary
        label.numberOfLines = 0
        label.sizeToFit()
        let box = UIView(frame: CGRect(x: 0, y: 0, width: view.bounds.width, height: label.frame.height + 24))
        box.addSubview(label)
        return box
    }

    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { devices.count + 3 }

    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = tableView.dequeueReusableCell(withIdentifier: "device")
            ?? UITableViewCell(style: .subtitle, reuseIdentifier: "device")
        cell.backgroundColor = Theme.Card.background
        let selected = UIView()
        selected.backgroundColor = Theme.Card.track
        cell.selectedBackgroundView = selected
        cell.tintColor = Theme.Card.label
        cell.textLabel?.textColor = Theme.Card.label
        cell.detailTextLabel?.textColor = Theme.Card.secondary
        cell.detailTextLabel?.font = UIFont.preferredFont(forTextStyle: .footnote)
        guard indexPath.row > 0 else {
            cell.textLabel?.text = Say.thisIPod
            cell.detailTextLabel?.text = nil
            cell.accessoryType = here ? .checkmark : .none
            return cell
        }
        guard indexPath.row <= devices.count else {
            if indexPath.row == devices.count + 1 {
                cell.textLabel?.text = Say.output
                cell.detailTextLabel?.text = nil
                cell.accessoryType = .none
                return cell
            }
            cell.textLabel?.text = Say.joinJam
            cell.detailTextLabel?.text = nil
            cell.accessoryType = .disclosureIndicator
            return cell
        }
        let d = devices[indexPath.row - 1]
        cell.textLabel?.text = d["name"] as? String
        let song = [d["title"] as? String, d["artist"] as? String].compactMap { $0 }.filter { !$0.isEmpty }
        let playing = d["playing"] as? Bool ?? false
        cell.detailTextLabel?.text = Say.refused(d["refused"] as? Int ?? 0)
            ?? (playing && !song.isEmpty ? song.joined(separator: " · ") : Say.deviceIdle)
        cell.accessoryType = d["active"] as? Bool == true ? .checkmark : .none
        return cell
    }

    func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        if indexPath.row == devices.count + 1 { return outputs.show() }
        guard indexPath.row <= devices.count else { return JamJoin.ask(from: self) }
        let id = indexPath.row == 0 ? "" : devices[indexPath.row - 1]["id"] as? String ?? ""
        id.withCString { nori_ios_remote_pick($0) }
        dismiss(animated: true)
    }
}

/// Joining someone's jam with the invite link its host sent: the guest profile opens once joined
/// (`Core`'s report 20).
enum JamJoin {
    static func ask(from host: UIViewController) {
        let ask = UIAlertController(title: Say.joinJam, message: Say.joinJamHow, preferredStyle: .alert)
        ask.view.tintColor = .black
        ask.addTextField { field in
            field.placeholder = Say.inviteLink
            field.keyboardType = .URL
            field.autocapitalizationType = .none
            field.autocorrectionType = .no
            field.returnKeyType = .join
        }
        ask.addAction(UIAlertAction(title: Say.cancel, style: .cancel))
        ask.addAction(UIAlertAction(title: Say.join, style: .default) { _ in
            join(ask.textFields?.first?.text ?? "")
        })
        host.present(ask, animated: true)
    }

    /// An invite opened before a session was: joined once one is.
    static var waiting: String?

    /// Joins the jam `link` invites to (an https invite, or the app's own nori:// one).
    static func join(_ link: String) {
        switch link.withCString({ l in Say.jamGuest.withCString { nori_ios_jam_join(l, $0) } }) {
        case NORI_JOIN_STARTED: Toast.show(Say.joining)
        case NORI_JOIN_NOT_AN_INVITE: Toast.show(Say.notAnInvite)
        case NORI_JOIN_OWN: Toast.show(Say.ownJam)
        case NORI_JOIN_ENDED: Toast.show(Say.jamInviteEnded)
        default: waiting = link
        }
    }
}

/// Remote control's mDNS through the system's Bonjour: this iPod's door announced while remote control
/// is on, the account's other doors looked for while "Play on" is open. The library asks from its own
/// threads; NetService runs on the main run loop.
final class Bonjour: NSObject, NetServiceDelegate, NetServiceBrowserDelegate {
    static let shared = Bonjour()
    private static let type = "_nori._tcp."
    private var announced: NetService?
    private var browser: NetServiceBrowser?
    /// Found and being resolved or resolved, by name: kept so their answers arrive.
    private var found: [String: NetService] = [:]

    func start() {
        nori_ios_on_bonjour({ name, port, txt in
            let name = name.map { String(cString: $0) }
            let txt = txt.map { String(cString: $0) } ?? "{}"
            DispatchQueue.main.async { Bonjour.shared.announce(name, port, txt) }
        }, { on in
            DispatchQueue.main.async { Bonjour.shared.browse(on != 0) }
        })
    }

    private func announce(_ name: String?, _ port: UInt16, _ txt: String) {
        announced?.stop()
        announced = nil
        guard let name else { return }
        let service = NetService(domain: "local.", type: Bonjour.type, name: name, port: Int32(port))
        let fields = (try? JSONSerialization.jsonObject(with: Data(txt.utf8))) as? [String: String] ?? [:]
        service.setTXTRecord(NetService.data(fromTXTRecord: fields.mapValues { Data($0.utf8) }))
        service.publish()
        announced = service
    }

    private func browse(_ on: Bool) {
        browser?.stop()
        browser = nil
        found.values.forEach { $0.stop() }
        found = [:]
        guard on else { return }
        let b = NetServiceBrowser()
        b.delegate = self
        b.searchForServices(ofType: Bonjour.type, inDomain: "local.")
        browser = b
    }

    func netServiceBrowser(_ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool) {
        found[service.name] = service
        service.delegate = self
        service.resolve(withTimeout: 5)
    }

    func netServiceBrowser(_ browser: NetServiceBrowser, didRemove service: NetService, moreComing: Bool) {
        found[service.name] = nil
        service.name.withCString { nori_ios_lan_lost($0) }
    }

    func netServiceDidResolveAddress(_ sender: NetService) {
        guard let host = (sender.addresses ?? []).lazy.compactMap(Bonjour.ipv4).first else { return }
        let fields = NetService.dictionary(fromTXTRecord: sender.txtRecordData() ?? Data())
            .compactMapValues { String(data: $0, encoding: .utf8) }
        let json = (try? JSONSerialization.data(withJSONObject: fields)).flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        sender.name.withCString { name in
            host.withCString { h in json.withCString { nori_ios_lan_found(name, h, UInt16(sender.port), $0) } }
        }
    }

    /// An IPv4 socket address as text.
    private static func ipv4(_ address: Data) -> String? {
        guard address.count >= MemoryLayout<sockaddr_in>.size else { return nil }
        var sin = sockaddr_in()
        _ = withUnsafeMutableBytes(of: &sin) { address.copyBytes(to: $0, count: MemoryLayout<sockaddr_in>.size) }
        guard sin.sin_family == sa_family_t(AF_INET) else { return nil }
        var text = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
        guard inet_ntop(AF_INET, &sin.sin_addr, &text, socklen_t(INET_ADDRSTRLEN)) != nil else { return nil }
        return String(cString: text)
    }
}

/// The system volume slider, kept in the window so it stays connected to the output route.
final class SystemVolume {
    private let view = MPVolumeView(frame: CGRect(x: -200, y: -200, width: 100, height: 40))

    init(in window: UIWindow) {
        view.isUserInteractionEnabled = false
        window.addSubview(view)
        view.layoutIfNeeded()
    }

    func set(_ fraction: Float) {
        guard let slider = view.subviews.lazy.compactMap({ $0 as? UISlider }).first else { return }
        slider.setValue(min(1, max(0, fraction)), animated: false)
        slider.sendActions(for: .valueChanged)
    }
}
