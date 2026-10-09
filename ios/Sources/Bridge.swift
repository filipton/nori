import UIKit

/// One row of a page answer: a song, album, artist, playlist or genre.
struct Item {
    let kind: String
    let id: String
    let title: String
    let subtitle: String
    let cover: String
    let seconds: Int
    let index: Int
    let count: Int
    let year: Int
    let external: Bool
    let starred: Bool
    let track: Int
    let disc: Int
    let album: String
    let albumId: String
    let artistId: String
    /// A mix tile's covers, up to four.
    let covers: [String]
    /// The A–Z index letter, when the list runs in the order of a name.
    let letter: String
    /// A running download: whole percent (-1 unknown), bytes a second (0 unknown), seconds left (-1).
    let progress: (percent: Int, speed: Int, left: Int)?
    /// In a jam's queue: who asked for the song, where someone did.
    let by: String

    init(_ d: [String: Any]) {
        by = d["by"] as? String ?? ""
        progress = (d["pct"] as? Int).map { ($0, d["bps"] as? Int ?? 0, d["eta"] as? Int ?? -1) }
        letter = d["l"] as? String ?? ""
        covers = d["covers"] as? [String] ?? []
        kind = d["k"] as? String ?? ""
        id = d["id"] as? String ?? ""
        title = d["t"] as? String ?? ""
        cover = d["c"] as? String ?? ""
        seconds = d["d"] as? Int ?? 0
        index = d["i"] as? Int ?? 0
        count = d["n"] as? Int ?? 0
        year = d["y"] as? Int ?? 0
        external = d["x"] as? Bool ?? false
        starred = d["st"] as? Bool ?? false
        track = d["n"] as? Int ?? 0
        disc = d["disc"] as? Int ?? 0
        album = d["a"] as? String ?? ""
        albumId = d["albumId"] as? String ?? ""
        artistId = d["artistId"] as? String ?? ""
        if let s = d["s"] as? String {
            subtitle = s
        } else {
            subtitle = ""
        }
    }
}

struct Section {
    let key: String
    let grid: Bool
    let items: [Item]
}

/// A page answer: sections, an optional header, or an error code.
struct PageAnswer {
    let sections: [Section]
    let head: [String: Any]
    let error: Int32?
    let detail: String
    let raw: [String: Any]

    init(_ d: [String: Any]) {
        raw = d
        head = d["head"] as? [String: Any] ?? [:]
        error = (d["error"] as? Int).map { Int32($0) }
        detail = d["detail"] as? String ?? ""
        sections = (d["sections"] as? [[String: Any]] ?? []).map {
            Section(
                key: $0["key"] as? String ?? "",
                grid: $0["grid"] as? Bool ?? false,
                items: ($0["items"] as? [[String: Any]] ?? []).map(Item.init)
            )
        }
    }
}

/// What plays now.
struct Now {
    var state: Int = 0
    var index: Int = -1
    var ms: Int = 0
    var pace: Double = 1
    var repeatMode: Int = 0
    var shuffle = false
    var length = 0
    var song: Item?
    var suffix = ""
    var kbps = 0
    var hz = 0
    var bits = 0
    var stamp = Date()
    /// Waiting for bytes after a seek or open: the place stands still until sound is heard again.
    var buffering = false
    /// Another device plays, and this is its music: its name.
    var device: String?
    /// Remote control is on: the music can move to the account's other devices.
    var remote = false
    /// The other device's volume in percent, when music plays there and it can be set.
    var volume: Int?
    /// The jam's music, this iPod a guest in it: the host's, which nothing here controls.
    var jam = false

    var playing: Bool { state == 1 }

    /// The position extrapolated from the last read while playing and not buffering.
    var position: Int {
        guard playing, !buffering else { return ms }
        return ms + Int(Date().timeIntervalSince(stamp) * 1000 * pace)
    }
}

/// What the open profile offers (`nori_ios_rules`): a jam guest's asks the host for what it plays, and has
/// nothing of the account's (hearts, playlists, downloads, its settings).
struct Rules {
    var asks = false
    var account = true
    /// The library's sections, as the core's LibrarySection numbers.
    var sections: Set<Int> = Set(0...11)
    /// The parts of the settings it opens, as the core's SettingsPart numbers (1: the server's own options).
    var settings: Set<Int> = Set(0...10)

    init() {}

    init(_ d: [String: Any]) {
        asks = d["asks"] as? Bool ?? false
        account = d["account"] as? Bool ?? true
        sections = Set(d["sections"] as? [Int] ?? [])
        settings = Set(d["settings"] as? [Int] ?? [])
    }
}

/// The jam this iPod is a guest in (`nori_ios_jam`).
struct Jam {
    let host: String
    let listeners: [String]
    /// The songs it asked for that wait for the host, by id.
    let asked: Set<String>
    let asks: [Item]
    /// 0 only shown, 1 playing here, 2 asked but the host lets no one, 3 asked but the server lets no guest.
    let listening: Int
    /// What its play, skip and seek controls reach by its role: 0 offered not, 1 this iPod's own
    /// listening, 2 the host's playback.
    let play: Int
    let skip: Int
    let seek: Int
    /// What the play button shows.
    let playing: Bool
    /// Paused here while the jam plays on: play joins it again.
    let pausedHere: Bool

    init(_ d: [String: Any]) {
        host = d["host"] as? String ?? ""
        listeners = d["listeners"] as? [String] ?? []
        asked = Set(d["asked"] as? [String] ?? [])
        asks = (d["asks"] as? [[String: Any]] ?? []).map { Item($0.merging(["k": "song"]) { a, _ in a }) }
        listening = d["listening"] as? Int ?? 0
        play = d["play"] as? Int ?? 0
        skip = d["skip"] as? Int ?? 0
        seek = d["seek"] as? Int ?? 0
        playing = d["playing"] as? Bool ?? false
        pausedHere = d["pausedHere"] as? Bool ?? false
    }

    var strip: String { pausedHere ? Say.jamPausedHere : Say.jamStrip(host, listeners.count) }
}

private func json(_ text: UnsafePointer<CChar>) -> [String: Any] {
    let data = Data(bytes: text, count: strlen(text))
    return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
}

/// Reads a string this library hands out and frees it.
func taken(_ p: UnsafeMutablePointer<CChar>?) -> String? {
    guard let p else { return nil }
    let s = String(cString: p)
    nori_ios_free(p)
    return s
}

func takenJSON(_ p: UnsafeMutablePointer<CChar>?) -> Any? {
    guard let text = taken(p), let data = text.data(using: .utf8) else { return nil }
    return try? JSONSerialization.jsonObject(with: data)
}

extension Notification.Name {
    /// The playing song, state or place changed: read `Core.shared.now`.
    static let noriNow = Notification.Name("noriNow")
    /// A heart was set or cleared: every heart on screen paints again.
    static let noriFavorites = Notification.Name("noriFavorites")
    /// Downloads were removed: pages showing them read again.
    static let noriDownloads = Notification.Name("noriDownloads")
    /// A session opened.
    static let noriOpened = Notification.Name("noriOpened")
    /// The account's other devices changed (remote control): read them again.
    static let noriDevices = Notification.Name("noriDevices")
    /// Lyrics came for the song in `object`.
    static let noriLyrics = Notification.Name("noriLyrics")
    /// The jam this iPod is a guest in changed: read `Core.shared.jam`.
    static let noriJam = Notification.Name("noriJam")
}

/// The library's callbacks, delivered on the main thread, and the requests that wait for them.
final class Core {
    static let shared = Core()

    private(set) var now = Now()
    private var nextToken: UInt64 = 1
    private var pages: [UInt64: (PageAnswer) -> Void] = [:]
    private var covers: [UInt64: (UIImage) -> Void] = [:]
    private let lock = NSLock()

    var isOpen: Bool { nori_ios_is_open() != 0 }

    /// What the open profile offers, read as it opens.
    private(set) var rules = Rules()
    /// The jam this iPod is a guest in, read as it changes.
    private(set) var jam: Jam?

    /// Whether the song is one this guest asked for and the host has yet to take.
    func isAsked(_ item: Item) -> Bool { item.kind == "song" && jam?.asked.contains(item.id) == true }

    private func readJam() {
        jam = (takenJSON(nori_ios_jam()) as? [String: Any]).map(Jam.init)
        NotificationCenter.default.post(name: .noriJam, object: nil)
    }

    /// Hearts changed in this session, by item id, until a page read brings the server's word.
    private var marks: [String: Bool] = [:]

    func isFavorite(_ item: Item) -> Bool { marks[item.id] ?? item.starred }

    /// Hearts (or un-hearts) a song, album or artist.
    func favorite(_ item: Item, _ on: Bool) {
        let kind: Int32
        switch item.kind {
        case "album": kind = 2
        case "artist": kind = 3
        default: kind = 1
        }
        marks[item.id] = on
        item.id.withCString { nori_ios_star(kind, $0, on ? 1 : 0) }
        NotificationCenter.default.post(name: .noriFavorites, object: nil)
    }

    func start() {
        nori_ios_on_report { report in
            guard let r = report?.pointee else { return }
            let kind = r.kind
            let text = r.text.map { String(cString: $0) } ?? ""
            let id = r.id.map { String(cString: $0) } ?? ""
            let flag = r.flag
            let count = r.index
            let ms = r.ms
            DispatchQueue.main.async { Core.shared.reported(kind: kind, id: id, text: text, flag: flag, count: count, ms: ms) }
        }
        nori_ios_on_page { token, text in
            guard let text else { return }
            let answer = PageAnswer(json(text))
            DispatchQueue.main.async { Core.shared.answered(token, answer) }
        }
        nori_ios_on_cover { token, width, height, rgba, len, owner in
            guard let image = Core.image(width: Int(width), height: Int(height), rgba: rgba, len: len, owner: owner) else { return }
            DispatchQueue.main.async { Core.shared.painted(token, image) }
        }
    }

    func token() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        nextToken += 1
        return nextToken
    }

    /// Reads a page; `answer` runs on the main thread for each answer that comes.
    @discardableResult
    func read(_ kind: Int32, _ arg: String = "", token: UInt64? = nil, answer: @escaping (PageAnswer) -> Void) -> UInt64 {
        let t = token ?? self.token()
        pages[t] = answer
        arg.withCString { nori_ios_read(t, kind, $0) }
        return t
    }

    /// Stops listening for a page's answers.
    func forget(_ token: UInt64) {
        pages[token] = nil
    }

    func cover(_ id: String, px: Int, done: @escaping (UIImage) -> Void) -> UInt64? {
        guard !id.isEmpty else { return nil }
        let t = token()
        covers[t] = done
        id.withCString { nori_ios_cover(t, $0, UInt32(px)) }
        return t
    }

    func cancelCover(_ token: UInt64) {
        covers[token] = nil
        nori_ios_cover_cancel(token)
    }

    /// Reads what plays now and tells the screens.
    func refresh() {
        // Buffering is only told on its own report: keep it across a place or state read.
        let wasBuffering = now.buffering
        var n = Now()
        guard let d = takenJSON(nori_ios_now()) as? [String: Any] else {
            now = n
            NotificationCenter.default.post(name: .noriNow, object: nil)
            return
        }
        n.state = d["state"] as? Int ?? 0
        n.index = d["index"] as? Int ?? -1
        n.ms = d["ms"] as? Int ?? 0
        n.pace = d["pace"] as? Double ?? 1
        n.repeatMode = d["repeat"] as? Int ?? 0
        n.shuffle = d["shuffle"] as? Bool ?? false
        n.length = d["len"] as? Int ?? 0
        n.song = (d["song"] as? [String: Any]).map(Item.init)
        n.suffix = d["suffix"] as? String ?? ""
        n.kbps = d["kbps"] as? Int ?? 0
        n.hz = d["hz"] as? Int ?? 0
        n.bits = d["bits"] as? Int ?? 0
        n.device = d["device"] as? String
        n.remote = d["remote"] as? Bool ?? false
        n.volume = d["volume"] as? Int
        n.jam = d["jam"] as? Bool ?? false
        if n.device != nil || n.jam {
            // The device says its own waits.
            n.buffering = d["buffering"] as? Bool ?? false
        } else {
            // Buffering is per song and only while Playing; a new song or a pause lets the clock go again.
            let same = n.song?.id == now.song?.id
            n.buffering = wasBuffering && same && n.state == 1
        }
        n.stamp = Date()
        now = n
        NotificationCenter.default.post(name: .noriNow, object: nil)
    }

    /// The engine is waiting for bytes (`on`) or sound is moving again.
    func setBuffering(_ on: Bool) {
        if on == now.buffering { return }
        if on {
            // Freeze at the place last heard: a refresh while starved can still extrapolate.
            now.ms = now.position
        }
        now.buffering = on
        now.stamp = Date()
        NotificationCenter.default.post(name: .noriNow, object: nil)
    }

    /// Closes the open session and opens the active saved server; nil when it opened, otherwise why
    /// not. Blocks: call it off the main thread.
    static func reopen() -> String? {
        dataDirectory().path.withCString { taken(nori_ios_reopen($0)) }
    }

    func opened() {
        rules = (takenJSON(nori_ios_rules()) as? [String: Any]).map(Rules.init) ?? Rules()
        refresh()
        readJam()
        NotificationCenter.default.post(name: .noriOpened, object: nil)
        if let link = JamJoin.waiting {
            JamJoin.waiting = nil
            JamJoin.join(link)
        }
    }

    /// Opens the active profile in place of the open one, a jam joined or left, and says `note` once open.
    private func switchProfile(_ note: String?) {
        DispatchQueue.global(qos: .userInitiated).async {
            let failed = Core.reopen()
            DispatchQueue.main.async {
                Core.shared.opened()
                if let words = failed ?? note { Toast.show(words) }
            }
        }
    }

    private func reported(kind: Int32, id: String, text: String, flag: Int32, count: Int32, ms: Int64) {
        switch kind {
        case 1, 2, 3, 4, 8, 14, 16:
            refresh()
        case 18:
            refresh()
            readJam()
            NotificationCenter.default.post(name: .noriDevices, object: nil)
        case 20 where flag == 2:
            Toast.show(Say.jamInviteEnded)
        case 20 where flag != 0:
            switchProfile(nil)
        case 20:
            Toast.show(Say.jamJoinFailed(Say.failure(count, text)))
        case 21:
            switchProfile(count == 0 ? Say.jamLeft : Say.jamEnded(text))
        case 19:
            SystemVolume.set(Float(ms) / 1000)
        case 5:
            Toast.show(Say.playbackError(text))
        case 7 where now.device == nil:
            setBuffering(flag != 0)
        case 9:
            if let words = Say.note(flag, Int(count)) {
                Toast.show(words)
            }
        case 11:
            NotificationCenter.default.post(name: .noriLyrics, object: id)
        case 22:
            askCurve(text, offered: flag != 0)
        default:
            break
        }
    }

    /// An AutoEQ curve for the output just attached: offered (Apply answers it) or already applied (Undo
    /// answers it). Either way `nori_ios_curve_answer` is the answer.
    private func askCurve(_ curve: String, offered: Bool) {
        var top = UIApplication.shared.keyWindow?.rootViewController
        while let next = top?.presentedViewController { top = next }
        guard let shown = top else { return }
        let ask = UIAlertController(title: offered ? Say.curveOffered : Say.curveApplied, message: curve, preferredStyle: .alert)
        ask.addAction(UIAlertAction(title: offered ? Say.curveApply : Say.curveUndo, style: .default) { _ in nori_ios_curve_answer() })
        ask.addAction(UIAlertAction(title: offered ? Say.curveNotNow : Say.curveKeep, style: .cancel))
        shown.present(ask, animated: true)
    }

    private func answered(_ token: UInt64, _ answer: PageAnswer) {
        pages[token]?(answer)
    }

    private func painted(_ token: UInt64, _ image: UIImage) {
        guard let done = covers.removeValue(forKey: token) else { return }
        done(image)
    }

    /// A picture drawn straight from the library's pixels; `owner` is let go when the picture is.
    private static func image(width: Int, height: Int, rgba: UnsafePointer<UInt8>?, len: Int, owner: UnsafeMutableRawPointer?) -> UIImage? {
        guard let rgba, width > 0, height > 0, len >= width * height * 4,
              let provider = CGDataProvider(dataInfo: owner, data: rgba, size: width * height * 4,
                                            releaseData: { info, _, _ in nori_ios_cover_release(info) }) else {
            nori_ios_cover_release(owner)
            return nil
        }
        // From here the provider owns `owner`, failed picture or not.
        guard let cg = CGImage(
                width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
                provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent
              ) else { return nil }
        return UIImage(cgImage: cg, scale: UIScreen.main.scale, orientation: .up)
    }
}

/// A one-line message over the tab bar for two seconds.
enum Toast {
    private static weak var shown: UILabel?

    static func show(_ text: String) {
        guard let window = UIApplication.shared.keyWindow else { return }
        shown?.removeFromSuperview()
        let label = PaddedLabel()
        label.text = text
        label.textColor = Theme.label
        label.backgroundColor = Theme.track
        label.font = UIFont.preferredFont(forTextStyle: .footnote)
        label.adjustsFontForContentSizeCategory = true
        label.numberOfLines = 2
        label.textAlignment = .center
        label.layer.cornerRadius = 10
        label.clipsToBounds = true
        label.translatesAutoresizingMaskIntoConstraints = false
        window.addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: window.centerXAnchor),
            label.widthAnchor.constraint(lessThanOrEqualTo: window.widthAnchor, constant: -40),
            label.bottomAnchor.constraint(equalTo: window.bottomAnchor, constant: -(49 + MiniPlayer.height + 12)),
        ])
        shown = label
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak label] in
            label?.removeFromSuperview()
        }
    }
}

final class PaddedLabel: UILabel {
    override func drawText(in rect: CGRect) {
        super.drawText(in: rect.insetBy(dx: 12, dy: 8))
    }

    override var intrinsicContentSize: CGSize {
        let s = super.intrinsicContentSize
        return CGSize(width: s.width + 24, height: s.height + 16)
    }
}
