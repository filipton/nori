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

    init(_ d: [String: Any]) {
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

    var playing: Bool { state == 1 }

    /// The position extrapolated from the last read while playing and not buffering.
    var position: Int {
        guard playing, !buffering else { return ms }
        return ms + Int(Date().timeIntervalSince(stamp) * 1000 * pace)
    }
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
    /// Lyrics came for the song in `object`.
    static let noriLyrics = Notification.Name("noriLyrics")
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
        // Buffering is per song and only while Playing; a new song or a pause lets the clock go again.
        let same = n.song?.id == now.song?.id
        n.buffering = wasBuffering && same && n.state == 1
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
        refresh()
        NotificationCenter.default.post(name: .noriOpened, object: nil)
    }

    private func reported(kind: Int32, id: String, text: String, flag: Int32, count: Int32, ms: Int64) {
        switch kind {
        case 1, 2, 3, 4, 8, 14, 16:
            refresh()
        case 19:
            SystemVolume.set(Float(ms) / 1000)
        case 5:
            Toast.show(Say.playbackError(text))
        case 7:
            setBuffering(flag != 0)
        case 9:
            if let words = Say.note(flag, Int(count)) {
                Toast.show(words)
            }
        case 11:
            NotificationCenter.default.post(name: .noriLyrics, object: id)
        default:
            break
        }
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
