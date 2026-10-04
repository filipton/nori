import UIKit

/// The song menu the core lays out (`nori_ios_song_menu`) and what each line does. Lines that reach the
/// server run off the main thread and say how it went.
enum SongMenu {
    /// Shows song `index` of list `token`'s menu over `host`. `player`: opened from the player card;
    /// `message` under the title; `go` opens a page (album, artist).
    static func show(_ song: Item, token: UInt64, index: Int, from host: UIViewController, player: Bool = false,
                     message: String? = nil, go: @escaping (UIViewController) -> Void) {
        let starred: Int32 = Core.shared.isFavorite(song) ? 1 : 0
        DispatchQueue.global(qos: .userInitiated).async {
            let d = takenJSON(nori_ios_song_menu(token, Int32(index), starred, player ? 1 : 0)) as? [String: Any]
            DispatchQueue.main.async {
                guard let d else { return }
                let items = d["items"] as? [[String: Any]] ?? []
                let details = d["details"] as? [String: Any] ?? [:]
                let sheet = UIAlertController.sheet(song.title, message)
                for item in items {
                    guard let a = item["a"] as? Int else { continue }
                    sheet.add(Say.songAction(a, item)) {
                        act(a, item, song: song, token: token, index: index, details: details, host: host, go: go)
                    }
                }
                sheet.show(from: host)
            }
        }
    }

    private static func act(_ a: Int, _ item: [String: Any], song: Item, token: UInt64, index: Int,
                            details: [String: Any], host: UIViewController, go: @escaping (UIViewController) -> Void) {
        let i = Int32(index)
        switch a {
        case 0:
            Core.shared.favorite(song, item["on"] as? Bool ?? true)
        case 3:
            pickPlaylist(token: token, index: index, from: host)
        case 7:
            guard let id = item["id"] as? String else { return }
            go(PageController(kind: NORI_PAGE_ALBUM, arg: id, title: song.album))
        case 8:
            guard let id = item["id"] as? String else { return }
            let name = item["name"] as? String ?? ""
            go(PageController(kind: NORI_PAGE_ARTIST, arg: id, title: name.isEmpty ? song.subtitle : name))
        case 9:
            _ = nori_ios_song_act(token, i, 9, 1)
            Toast.show(Say.serverDownloading)
        case 10:
            SleepTimer.shared.pick(from: host)
        case 11, 12:
            off({ nori_ios_song_act(token, i, Int32(a), 0) }) { n in
                if n < 0 { Toast.show(Say.failed) } else if n == 0, let words = Say.note(NORI_NOTE_NOTHING_TO_PLAY, 0) { Toast.show(words) }
            }
        case 13:
            off({ nori_ios_song_act(token, i, 13, 0) }) { done in Toast.show(done == 1 ? Say.excludedFromMixes : Say.failed) }
        case 14:
            share(token: token, index: index, from: host)
        case 15:
            showDetails(details, from: host)
        default:
            // Play next, add to queue and the downloads: the library says what it did in a note.
            _ = nori_ios_song_act(token, i, Int32(a), 0)
        }
    }

    /// Runs `work` off the main thread and hands its answer to `done` on it.
    private static func off(_ work: @escaping () -> Int32, _ done: @escaping (Int32) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let answer = work()
            DispatchQueue.main.async { done(answer) }
        }
    }

    /// The playlists, a new one first; the song goes into the one picked.
    static func pickPlaylist(token: UInt64, index: Int, from host: UIViewController) {
        var asked: UInt64 = 0
        asked = Core.shared.read(NORI_PAGE_PLAYLISTS) { a in
            // The stored copy is enough to pick from; the server's later answer is not waited for.
            Core.shared.forget(asked)
            let playlists = a.sections.flatMap { $0.items }.filter { $0.kind == "playlist" }
            let sheet = UIAlertController.sheet(Say.addToPlaylist)
            sheet.add(Say.newPlaylist) { newPlaylist(token: token, index: index, from: host) }
            for p in playlists {
                sheet.add(p.title) {
                    off({ p.id.withCString { nori_ios_playlist_add(token, Int32(index), $0) } }) { done in
                        Toast.show(done == 1 ? Say.addedToPlaylist(p.title) : Say.failed)
                    }
                }
            }
            sheet.show(from: host)
        }
    }

    private static func newPlaylist(token: UInt64, index: Int, from host: UIViewController) {
        let ask = UIAlertController(title: Say.newPlaylist, message: nil, preferredStyle: .alert)
        ask.view.tintColor = .black
        ask.addTextField { field in
            field.autocapitalizationType = .sentences
            field.returnKeyType = .done
        }
        ask.addAction(UIAlertAction(title: Say.cancel, style: .cancel))
        ask.addAction(UIAlertAction(title: Say.create, style: .default) { _ in
            let name = ask.textFields?.first?.text?.trimmingCharacters(in: .whitespaces) ?? ""
            guard !name.isEmpty else { return }
            off({ name.withCString { nori_ios_playlist_create(token, Int32(index), $0) } }) { done in
                Toast.show(done == 1 ? Say.playlistCreated(name) : Say.failed)
            }
        })
        host.present(ask, animated: true)
    }

    private static func share(token: UInt64, index: Int, from host: UIViewController) {
        DispatchQueue.global(qos: .userInitiated).async {
            let link = taken(nori_ios_song_share(token, Int32(index)))
            DispatchQueue.main.async {
                guard let link, let url = URL(string: link) else { return Toast.show(Say.failed) }
                host.present(UIActivityViewController(activityItems: [url], applicationActivities: nil), animated: true)
            }
        }
    }

    private static func showDetails(_ d: [String: Any], from host: UIViewController) {
        let text = Say.details(d)
        let alert = UIAlertController(title: Say.detailsTitle, message: text, preferredStyle: .alert)
        alert.view.tintColor = .black
        alert.addAction(UIAlertAction(title: "OK", style: .cancel))
        host.present(alert, animated: true)
    }
}

/// The sleep timer: after some minutes (a one-shot timer here, its length the core's), or after songs or
/// at the end of this one (the core and the engine count those).
final class SleepTimer {
    static let shared = SleepTimer()
    private var timer: Timer?
    private var running = false

    func pick(from host: UIViewController) {
        guard let choices = takenJSON(nori_ios_sleep_choices(running ? 1 : 0)) as? [[String: Any]] else { return }
        let sheet = UIAlertController.sheet(Say.sleepTimer)
        for c in choices {
            let minutes = c["minutes"] as? Int ?? 0
            let end = c["end"] as? Bool ?? false
            let songs = c["songs"] as? Int ?? 0
            sheet.add(Say.sleepChoice(minutes: minutes, end: end, songs: songs)) {
                self.set(minutes: minutes, end: end, songs: songs)
            }
        }
        sheet.show(from: host)
    }

    private func set(minutes: Int, end: Bool, songs: Int) {
        timer?.invalidate()
        timer = nil
        nori_ios_sleep_set(UInt32(songs), end ? 1 : 0)
        running = minutes > 0 || end || songs > 0
        guard minutes > 0, let delay = takenJSON(nori_ios_sleep_delay(UInt32(minutes))) as? [Int], delay.count == 2 else { return }
        let t = Timer(timeInterval: Double(delay[0]) / 1000, repeats: false) { [weak self] _ in
            nori_ios_sleep_now()
            self?.running = false
            self?.timer = nil
        }
        t.tolerance = Double(delay[1]) / 1000
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }
}
