import Foundation

/// The app's words. The library answers with codes and keys; this is where they become English,
/// following the Android app's `strings.xml`.
enum Say {
    static func shelf(_ key: String) -> String {
        switch key {
        case "mixes": return "For you"
        case "newest": return "Recently added"
        case "recent": return "Recently played"
        case "frequent": return "Most played albums"
        case "starred": return "Favorite albums"
        case "random": return "Random"
        case "artists": return "Artists"
        case "albums": return "Albums"
        case "songs": return "Songs"
        case "playlists": return "Playlists"
        case "genres": return "Genres"
        case "active": return "Downloading"
        case "queued": return "Waiting"
        case "failed": return "Failed"
        case "stored": return "Downloaded"
        case "queue": return "Up next"
        default: return ""
        }
    }

    /// A "For you" mix by its `MixName` code.
    /// A built-in smart playlist by its `SmartBuiltin` code.
    static func smart(_ code: Int) -> String {
        ["Most played", "Recently played", "Recently added", "Never played", "Top rated",
         "Forgotten favorites", "Long tracks"][safe: code] ?? "Smart playlist"
    }

    static func mix(_ code: Int) -> String {
        ["Favorites", "Quick picks", "Discover", "Discover Weekly", "Listen again", "Your top songs"][safe: code] ?? ""
    }

    /// A long list's order, by the name the core keeps it under: album orders by the server's `type`,
    /// song orders by `browse::song_sorts` name.
    static func sort(_ name: String) -> String {
        switch name {
        case "alphabeticalByName": return "A–Z"
        case "alphabeticalByArtist", "ARTIST": return "Artist"
        case "newest", "ADDED": return "Added"
        case "recent": return "Played"
        case "frequent", "PLAYS": return "Most played"
        case "starred": return "Favorites"
        case "byYear", "YEAR": return "Year"
        case "random": return "Random"
        case "TITLE": return "Title"
        case "ALBUM": return "Album"
        case "LONGEST": return "Longest"
        default: return name
        }
    }

    /// A running download's line: "45% · 1.2 MB/s · 0:30 left", each part only when known.
    static func downloading(percent: Int, speed: Int, left: Int) -> String {
        var bits: [String] = [percent >= 0 ? "\(percent)%" : "Downloading"]
        if speed > 0 { bits.append(Fmt.bytes(speed) + "/s") }
        if left >= 0 { bits.append(Fmt.clock(ms: left * 1000) + " left") }
        return bits.joined(separator: " · ")
    }

    /// A built-in equalizer curve by its `PresetKind` code.
    static func preset(_ code: Int) -> String {
        ["Flat", "Bass boost", "Bass cut", "Treble boost", "Treble cut", "Vocal boost", "Loudness",
         "Small speakers"][safe: code] ?? ""
    }

    static func songs(_ n: Int) -> String { n == 1 ? "1 song" : "\(n) songs" }
    static func albums(_ n: Int) -> String { n == 1 ? "1 album" : "\(n) albums" }

    static func note(_ code: Int32, _ count: Int) -> String? {
        switch code {
        case NORI_NOTE_QUEUED_NEXT: return "Playing next: \(songs(count))"
        case NORI_NOTE_QUEUED_LAST: return "Added to the queue: \(songs(count))"
        case NORI_NOTE_NOTHING_TO_PLAY: return "Nothing to play"
        case NORI_NOTE_SONGS_FAILED: return "Could not load the songs"
        case NORI_NOTE_NOTHING_TO_PUT_BACK: return "Nothing to put back"
        case NORI_NOTE_DOWNLOADING: return "Downloading \(songs(count))"
        case NORI_NOTE_DOWNLOAD_FAILED: return "Could not download"
        case NORI_NOTE_STARRED: return "Added to favorites"
        case NORI_NOTE_UNSTARRED: return "Removed from favorites"
        case NORI_NOTE_STAR_FAILED: return "Could not change the favorite"
        case NORI_NOTE_INDEXING: return "Filling the offline index…"
        case NORI_NOTE_INDEXED: return "Offline index: \(songs(count))"
        case NORI_NOTE_INDEX_STOPPED: return "The offline index stopped"
        default: return nil
        }
    }

    static func playbackError(_ detail: String) -> String {
        detail.isEmpty ? "This song could not be played" : "This song could not be played: \(detail)"
    }

    /// A failure code from login or a page read.
    static func failure(_ code: Int32, _ detail: String?) -> String {
        switch code {
        case NORI_LOGIN_INCOMPLETE: return "The server's address and a user name, please."
        case NORI_LOGIN_NOT_FOUND: return "Server not found. Check the address."
        case NORI_LOGIN_UNREACHABLE: return "Nothing is answering at that address. Is the port right, and is the server running?"
        case NORI_LOGIN_TIMEOUT: return "The server did not answer in time."
        case NORI_LOGIN_CERTIFICATE: return "The server's certificate was not accepted."
        case NORI_LOGIN_HTTP: return "HTTP \(detail ?? "")"
        case NORI_LOGIN_PASSWORD: return "Wrong user name or password."
        case NORI_LOGIN_FORBIDDEN: return "This user is not allowed to do that."
        case NORI_LOGIN_NOT_SUBSONIC: return "That address answered, but not like a Subsonic server. Check the URL."
        case NORI_LOGIN_DATABASE: return detail.map { "The database: \($0)" } ?? "The database would not open."
        case NORI_LOGIN_CLEARTEXT: return "Cleartext HTTP was refused; use https://"
        case NORI_LOGIN_METERED: return "This server is set to Wi-Fi only."
        default:
            if let detail, !detail.isEmpty { return detail }
            return "The server did not answer."
        }
    }

    /// Setting names this client shows, in its order, with their labels.
    static let settings: [(section: String, rows: [(name: String, label: String)])] = [
        ("Playback", [
            ("crossfadeSec", "Crossfade (seconds)"),
            ("crossfadeKeepAlbums", "Keep albums gapless"),
            ("autoMix", "AutoMix"),
            ("autoMixMaxS", "AutoMix longest mix (seconds)"),
            ("autoMixBeatMatch", "Match beats"),
            ("skipSilence", "Skip silence"),
            ("previousAlwaysSkips", "Previous always skips"),
        ]),
        ("Queue", [
            ("skipExplicit", "Skip explicit songs"),
            ("autoFill", "Keep playing when the queue ends"),
            ("autoFillKind", "Carry on with"),
            ("autoFillBasis", "Chosen by"),
            ("autoFillRemote", "Include remote songs"),
        ]),
        ("When something goes wrong", [
            ("skipOnError", "Skip songs that won't play"),
            ("bridgeOffline", "Play downloads when offline"),
        ]),
        ("Sound", [
            ("replayGain", "ReplayGain"),
            ("loudnessTarget", "Loudness target (LUFS)"),
            ("gainBoostDb", "Gain boost (dB)"),
            ("loudness", "Loudness compensation"),
            ("crossfeedDb", "Crossfeed (dB)"),
            ("mono", "Mono"),
            ("limiter", "Limiter"),
            ("compressor", "Compressor"),
            ("profilePerOutput", "Sound profile per output"),
        ]),
        ("Lyrics", [
            ("lyricsTranslation", "Show translations"),
            ("lyricsPreferWords", "Prefer word-timed lyrics"),
        ]),
        ("Song rows", [
            ("swipeRight", "Swipe right"),
            ("swipeLeft", "Swipe left"),
        ]),
        ("Messages", [
            ("favouriteNotice", "Confirm favorites"),
        ]),
        ("Remote control", [
            ("remoteControl", "Control from other devices"),
        ]),
        ("Server", [
            ("scrobble", "Scrobble plays"),
            ("scrobblePercent", "Scrobble after (percent)"),
            ("liveSearchDelayMs", "Search the server after (ms)"),
            ("thirdPartyLookups", "Look up artist info and lyrics online"),
        ]),
    ]

    // Screens. Android's strings.xml where it has the phrase; the rest are this client's own (servers,
    // the offline index, the downloads line).
    static let home = "Home"
    static let library = "Library"
    static let search = "Search"
    static let settingsTitle = "Settings"
    static let playlists = "Playlists"
    static let artists = "Artists"
    static let albumsTitle = "Albums"
    static let songsTitle = "Songs"
    static let genres = "Genres"
    static let favorites = "Favorites"
    static let smartPlaylists = "Smart playlists"
    static let downloaded = "Downloaded"
    static let downloadsRun = "Downloads run while nori is open or playing."
    static let searchHint = "Songs, albums, artists"
    static let nothingPlaying = "Nothing playing"
    static let lookingForLyrics = "Looking for lyrics…"
    static let noLyrics = "No lyrics for this song"
    static let sortBy = "Sort by"
    static let noServer = "No server yet"
    static let noServerDetail = "Add your server in Settings → Server."
    static let couldNotLoad = "Could not load this page"
    static let offline = "You're offline"
    static let offlineDetail = "Connect to the internet to see this page. Downloaded songs still play."

    /// What an empty page says for a failed read: the network being gone is not a page that is broken.
    static func pageFailure(_ code: Int32, _ detail: String?) -> (title: String, detail: String) {
        switch code {
        case NORI_LOGIN_NOT_FOUND, NORI_LOGIN_UNREACHABLE, NORI_LOGIN_TIMEOUT: return (offline, offlineDetail)
        default: return (couldNotLoad, failure(code, detail))
        }
    }
    static let nothingHere = "Nothing here yet"
    static let playNext = "Play next"
    static let addToQueue = "Add to queue"
    static let favorite = "Favorite"
    static let unfavorite = "Unfavorite"
    static let download = "Download"
    static func downloadOther(_ n: Int) -> String { "Download the other \(n)" }
    static let removeDownloads = "Remove downloads"
    static func removeDownloaded(_ n: Int) -> String { n == 1 ? "Remove the 1 downloaded" : "Remove the \(n) downloaded" }
    static func downloadsRemoved(_ n: Int) -> String { n == 1 ? "Removed 1 download" : "Removed \(n) downloads" }
    static let addToFavorites = "Add to favorites"
    static let removeFromFavorites = "Remove from favorites"
    static let play = "Play"
    static let shuffle = "Shuffle"
    static let server = "Server"
    static let servers = "Servers"
    static let none = "None"
    static let addServer = "Add a server"
    static func forgetServer(_ name: String) -> String { "Forget \(name)?" }
    static let forgetServerNote = "Its downloads stay on this iPod until you add it again."
    static let forget = "Forget"
    static let user = "User"
    static let password = "Password"
    static let connect = "Connect"
    static let connecting = "Connecting…"
    static let serverKinds = "Navidrome, octo-fiesta or any Subsonic server"
    static let serverUrl = "Server URL"
    static let advanced = "Advanced"
    static let hideAdvanced = "Hide advanced"
    static let nameOptional = "Name (optional)"
    static let secondAddress = "Second address (e.g. public URL)"
    static let apiKey = "API key instead of password (OpenSubsonic)"
    static let equalizer = "Equalizer"
    static let presets = "Presets"
    static let on = "On"
    static let off = "Off"
    static let refreshIndex = "Refresh the offline index"
    static let about = "About"
    static let version = "Version"
    static let streamed = "Streamed music"
    static let theme = "Theme"
    static let light = "Light"
    static let dark = "Dark"

    // What controls drawn as glyphs say to VoiceOver, from Android's strings.xml.
    static let pause = "Pause"
    static let next = "Next"
    static let previous = "Previous"
    static let close = "Close"
    static let more = "More"
    static let lyrics = "Lyrics"
    static let repeatMode = "Repeat"
    static let sort = "Sort"
    static let output = "Output"

    // The queue, from Android's strings.xml.
    static let queue = "Queue"
    static let history = "History"
    static let upNext = "Playing next"
    static let clear = "Clear"
    static let remove = "Remove"
    static let undo = "Undo"
    static func queueRemoved(_ title: String) -> String { "Removed “\(title)”" }

    // About, from Android's strings.xml.
    static let licences = "Licenses"
    static let licencesDetail = "The libraries, fonts and data this app is made of, and their terms"
    static let freeSoftware = "nori is free software"
    static let rustCore = "Rust core"
    static let appCredits = "iPod app"
    static let fontsAndData = "Fonts and data"
    static let noLicenceText = "No license text to reproduce."
    static let licenceUnreadable = "The license text could not be read."

    // The song menu, from Android's strings.xml (menu_*, said_*, sleep_choice_*).
    static let addToPlaylist = "Add to playlist…"
    static let newPlaylist = "New playlist"
    static let create = "Create"
    static let cancel = "Cancel"
    static let sleepTimer = "Sleep timer"
    static let detailsTitle = "Details"
    static let excludedFromMixes = "Excluded from mixes"
    static let serverDownloading = "The server is downloading it into your library"
    static let removeFromPlaylist = "Remove"
    static func addedToPlaylist(_ name: String) -> String { "Added to \(name)" }
    static func playlistCreated(_ name: String) -> String { "Created \(name)" }
    static func deleteNamed(_ name: String) -> String { "Delete \(name)" }

    /// A song menu line by its code (`nori_ios_song_menu`'s `a`), with the fields it carries.
    static func songAction(_ code: Int, _ item: [String: Any]) -> String {
        switch code {
        case 0: return item["on"] as? Bool ?? true ? "Add to favorites" : "Remove from favorites"
        case 1: return "Play next"
        case 2: return "Add to queue"
        case 3: return addToPlaylist
        case 4: return "Remove download"
        case 5: return "Stop download"
        case 6: return "Download"
        case 7: return "Go to album"
        case 8:
            guard item["named"] as? Bool == true, let name = item["name"] as? String else { return "Go to artist" }
            return "Go to \(name)"
        case 9: return "Add to library"
        case 10: return "Sleep timer…"
        case 11: return "Start radio from this song"
        case 12: return "Instant mix"
        case 13: return "Exclude from mixes"
        case 14: return "Share link"
        default: return detailsTitle
        }
    }

    /// "30 minutes", "End of track", "After 3 songs", or "Off" (all zeros).
    static func sleepChoice(minutes: Int, end: Bool, songs: Int) -> String {
        if end { return "End of track" }
        if songs > 0 { return "After \(songs) songs" }
        if minutes > 0 { return "\(minutes) minutes" }
        return "Off"
    }

    /// The details sheet's rows as "Label: value", each only when known (Android's `Say.trackInfo`).
    static func details(_ d: [String: Any]) -> String {
        let text = { (k: String) -> String? in (d[k] as? String).flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 } }
        let number = { (k: String) -> Int? in (d[k] as? Int).flatMap { $0 > 0 ? $0 : nil } }
        let fixed = { (k: String, places: Int, sign: Bool) -> String? in
            (d[k] as? Double).map { String(format: sign ? "%+.\(places)f" : "%.\(places)f", $0) }
        }
        let artists = (d["artists"] as? [String] ?? []).joined(separator: ", ")
        let track = [number("disc").map { "disc \($0)" }, number("track").map { "track \($0)" }].compactMap { $0 }.joined(separator: ", ")
        let format = [text("suffix")?.uppercased(), text("type")].compactMap { $0 }.joined(separator: " · ")
        let quality = Fmt.quality(suffix: "", kbps: number("kbps") ?? 0, hz: number("hz") ?? 0, bits: number("bits") ?? 0)
        let gain = [fixed("trackGain", 2, true).map { "track \($0) dB" }, fixed("albumGain", 2, true).map { "album \($0) dB" },
                    fixed("peak", 3, false).map { "peak \($0)" }].compactMap { $0 }.joined(separator: " · ")
        let rows: [(String, String?)] = [
            ("Title", text("title")),
            ("Artist", artists.isEmpty ? text("artist") : artists),
            ("Album", text("album")),
            ("Track", track),
            ("Year", number("year").map(String.init)),
            ("Genre", text("genre")),
            ("Duration", number("seconds").map { Fmt.clock(ms: $0 * 1000) }),
            ("Format", format),
            ("Quality", quality),
            ("Size", number("bytes").map(Fmt.bytes)),
            ("ReplayGain", gain),
            ("BPM", number("bpm").map(String.init)),
            ("Plays (server)", number("plays").map(String.init)),
            ("Last played", text("played").map { String($0.prefix(16)).replacingOccurrences(of: "T", with: " ") }),
            ("Added", text("added").map { String($0.prefix(10)) }),
            ("Path", text("path")),
            ("MusicBrainz", text("mbid")),
            ("Comment", text("comment")),
            ("Id", text("id")),
        ]
        return rows.compactMap { label, v in v.flatMap { $0.isEmpty ? nil : "\(label): \($0)" } }.joined(separator: "\n")
    }

    // Remote control, from Android's strings_ui.xml (devices_*).
    static let playOn = "Play on"
    static let thisIPod = "This iPod"
    static let volume = "Volume"
    static let devicesNone = "No other devices yet. Turn on “Control from other devices” in nori on them."
    static let deviceIdle = "Not playing"
    static func playingOn(_ device: String) -> String { "Playing on \(device)" }

    // Jams, from the desktop's words.rs (jam_*).
    static let jamGuest = "Jam"
    static let joinJam = "Join a Jam"
    static let joinJamHow = "Paste the invite link the host sent you."
    static let inviteLink = "Invite link"
    static let join = "Join"
    static let joining = "Joining…"
    static let notAnInvite = "That is not a jam invite. Paste the whole link the host sent you."
    static let ownJam = "That's your own jam"
    static func jamJoinFailed(_ why: String) -> String { "Couldn't join the jam (\(why))" }
    static let leaveJam = "Leave"
    static let jamLeft = "You left the jam"
    static func jamEnded(_ host: String) -> String { host.isEmpty ? "The jam ended" : "\(host) ended the jam" }
    static let listenHere = "Listen Here"
    static let playingHere = "Playing Here"
    static let youAskedFor = "You Asked For"
    static let asked = "Asked"
    static func jamOf(_ host: String) -> String { "\(host)’s Jam" }
    static func listening(_ n: Int) -> String { n == 0 ? "no one yet" : "\(n) listening" }
    /// The jam on the player: "Jam · Desk · 2 listening".
    static func jamStrip(_ host: String, _ n: Int) -> String { "Jam · \(host) · \(listening(n))" }
    /// A jam guest paused its own listening; play joins the jam again.
    static let jamPausedHere = "Paused here · Jam still playing"
    static func waitingFor(_ host: String) -> String { "Waiting for \(host)" }
    /// Why a guest who asked to listen along does not, by `nori_ios_jam` listening code.
    static func jamAlong(_ code: Int) -> String? {
        switch code {
        case 2: return "The host doesn't let guests listen along right now. You can still ask for songs."
        case 3: return "This server doesn't let guests listen along. You can still ask for songs."
        default: return nil
        }
    }

    /// A device's answer to the last thing asked of it, by `nori_ios_remote_devices` refusal code.
    static func refused(_ code: Int) -> String? {
        switch code {
        case 1: return "The queue changed there. Try again."
        case 2: return "That device said no."
        case 3: return "That is no longer there."
        case 4: return "Too many songs waiting."
        default: return nil
        }
    }

    // The Sound page, from Android's strings.xml (devices, output_*, device_*, autoeq_*).
    static let sound = "Sound"
    static let devices = "Devices"
    static let playingNow = "Playing now"
    static let automatic = "Automatic"
    static let flat = "Flat"
    static let noProcessing = "No processing"
    static let leaveAsIs = "Leave as is"
    static let savedProfile = "Saved profile"
    static let forgetDevice = "Forget this device"
    static let headphonePresets = "Headphone presets"
    static let autoeqCurves = "AutoEQ curves"
    static let autoeqAbout = "AutoEQ measures headphones and publishes a correction curve for each: 850 kB from github.com, then searched on this iPod."
    static let downloadTheList = "Download the list"
    static let refreshList = "Refresh list"
    static let autoeqNoCurve = "AutoEQ has no curve for that one, so it's left out of the list from now on."
    static let nothingMatches = "Nothing matches"
    static let failed = "Failed"

    static func autoeqSearch(_ n: Int) -> String { "Search \(n) headphones" }
    static func autoeqApplied(_ name: String) -> String { "Applied \(name). The equalizer screen now holds that curve." }
    /// Who measured it, the form and the target, whichever are known.
    static func autoeqCaption(_ c: Curve) -> String { [c.source, c.form, c.target].filter { !$0.isEmpty }.joined(separator: " · ") }
    static func autoeqShort(_ c: Curve) -> String { "\(c.source) · \(c.form)" }

    /// Where a device is plugged in, when worth saying, by `nori_ios_devices` port code.
    static func outputKind(port: Int) -> String? {
        switch port {
        case 2: return "USB"
        case 3: return "Bluetooth"
        default: return nil
        }
    }

    /// A device's name: its own, or the app's for one without. The iPod has no phone speaker.
    static func outputName(port: Int, name: String?) -> String {
        switch port {
        case 0: return "Speaker"
        case 1: return "Wired headphones"
        case 2: return name ?? "DAC"
        case 3: return name ?? "device"
        default: return name ?? "Other output"
        }
    }

    /// "Bluetooth: Buds", "Wired headphones".
    static func outputLabel(port: Int, name: String?) -> String {
        let named = outputName(port: port, name: name)
        return outputKind(port: port).map { "\($0): \(named)" } ?? named
    }

    /// The sound a device gets, by `nori_ios_devices` choice code.
    static func deviceSound(choice: Int, profile: String?) -> String {
        switch choice {
        case 1: return leaveAsIs
        case 2: return flat
        case 3: return profile ?? ""
        case 4: return noProcessing
        default: return automatic
        }
    }

    /// The line under a fixed choice in a device's page.
    static func choiceDetail(code: Int, autoApply: Bool) -> String {
        switch code {
        case 0: return autoApply ? "Uses a matching AutoEQ curve when one is known" : "Offers a matching AutoEQ curve when one is known"
        case 1: return "Nothing switches and nothing is offered"
        case 2: return "The equalizer off on this device"
        default: return "No equalizer or effects on this device"
        }
    }

    static func deviceIntro(kind: String?) -> String {
        "What music played through \(kind.map { "this \($0) device" } ?? "this") sounds like. It switches by itself whenever the device connects."
    }

    /// Rows shown only while the setting they refine is on, as on Android: what keeps playing, and how
    /// it is chosen, only once the queue keeps playing.
    static let shownWhen: [String: String] = [
        "autoFillKind": "autoFill", "autoFillBasis": "autoFill", "autoFillRemote": "autoFill",
    ]

    /// A setting's value in words; the few whose names say too little have their own.
    static func choice(_ name: String, _ value: String) -> String {
        switch (name, value) {
        case ("autoFillBasis", "Similar"): return "Similar music"
        case ("autoFillBasis", "Artist"): return "The same artist"
        case ("autoFillBasis", "Genre"): return "The same genre"
        case ("autoFillBasis", "Era"): return "The same era"
        case ("swipeRight", "NONE"), ("swipeLeft", "NONE"): return "Nothing"
        case ("swipeRight", "QUEUE"), ("swipeLeft", "QUEUE"): return addToQueue
        case ("swipeRight", "PLAY_NEXT"), ("swipeLeft", "PLAY_NEXT"): return playNext
        case ("swipeRight", "FAVOURITE"), ("swipeLeft", "FAVOURITE"): return favorite
        case ("swipeRight", "DOWNLOAD"), ("swipeLeft", "DOWNLOAD"): return download
        default: return choice(value)
        }
    }

    static func choice(_ value: String) -> String {
        switch value {
        case "true": return "On"
        case "false": return "Off"
        default:
            // Enum names come camel-cased or in capitals: "EqualPower" → "Equal power", "OFF" → "Off".
            // A word starts only where a lower-case letter meets a capital.
            var out = ""
            var previous: Character?
            for ch in value {
                if let p = previous, p.isLowercase, ch.isUppercase { out += " " }
                out += out.isEmpty ? String(ch).uppercased() : String(ch).lowercased()
                previous = ch
            }
            return out
        }
    }
}

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

/// Numbers and times as the Android app writes them (`core/text/Fmt.kt`).
enum Fmt {
    /// "3:07", "1:02:03".
    static func clock(ms: Int) -> String {
        let s = max(0, ms / 1000)
        let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, sec) : String(format: "%d:%02d", m, sec)
    }

    /// "42 min", "1 hr 5 min".
    static func length(seconds: Int) -> String {
        let m = (seconds + 30) / 60
        if m < 60 { return "\(m) min" }
        let (h, rest) = (m / 60, m % 60)
        return rest == 0 ? "\(h) hr" : "\(h) hr \(rest) min"
    }

    static func bytes(_ n: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .file)
    }

    /// "FLAC · 24-bit · 96 kHz", "MP3 · 320 kbps".
    static func quality(suffix: String, kbps: Int, hz: Int, bits: Int) -> String {
        var parts: [String] = []
        if !suffix.isEmpty { parts.append(suffix.uppercased()) }
        if bits > 0 { parts.append("\(bits)-bit") }
        if hz > 0 {
            let k = Double(hz) / 1000
            parts.append(k == k.rounded() ? "\(Int(k)) kHz" : String(format: "%.1f kHz", k))
        }
        if bits == 0 && kbps > 0 { parts.append("\(kbps) kbps") }
        return parts.joined(separator: " · ")
    }
}
