import UIKit

/// The app's marks: Android's own Material icons (`Icons.Filled.*`, Apache 2.0, credited on the Licenses
/// page), drawn from their 24 × 24 path data as templates. iOS 12 has no SF Symbols.
enum Glyph {
    static let home = material(Material.home, CGSize(width: 25, height: 25))
    static let library = material(Material.libraryMusic, CGSize(width: 25, height: 25))
    static let search = material(Material.search, CGSize(width: 25, height: 25))
    static let settings = material(Material.settings, CGSize(width: 25, height: 25))
    /// The mini player's.
    static let play = material(Material.playArrow, CGSize(width: 22, height: 22))
    static let pause = material(Material.pause, CGSize(width: 22, height: 22))
    static let next = material(Material.skipNext, CGSize(width: 22, height: 22))
    /// The card's transport, as Android's: rewind and fast-forward marks for previous and next.
    static let bigPlay = material(Material.playArrow, CGSize(width: 44, height: 44))
    static let bigPause = material(Material.pause, CGSize(width: 44, height: 44))
    static let bigNext = material(Material.fastForward, CGSize(width: 40, height: 40))
    static let bigPrevious = material(Material.fastRewind, CGSize(width: 40, height: 40))
    static let shuffle = material(Material.shuffle, CGSize(width: 22, height: 22))
    static let repeatAll = material(Material.repeatAll, CGSize(width: 22, height: 22))
    static let repeatOne = material(Material.repeatOne, CGSize(width: 22, height: 22))
    static let queue = material(Material.queueMusic, CGSize(width: 24, height: 24))
    static let speaker = material(Material.speaker, CGSize(width: 24, height: 24))
    /// Another device plays: its speaker, filled while it sounds.
    static let groups = material(Material.groups, CGSize(width: 24, height: 24))
    static let groupsSmall = material(Material.groups, CGSize(width: 22, height: 22))
    static let speakerSmall = material(Material.speaker, CGSize(width: 20, height: 20))
    static let speakerSmallOutline = material(Material.speakerOutline, CGSize(width: 20, height: 20))
    /// A queue row's handle: hold and drag it to move the song.
    static let grip = material(Material.dragHandle, CGSize(width: 22, height: 22))
    static let lyrics = material(Material.lyrics, CGSize(width: 24, height: 24))
    static let heart = material(Material.favoriteBorder, CGSize(width: 24, height: 24))
    static let heartFilled = material(Material.favorite, CGSize(width: 24, height: 24))
    /// The mini player's heart, on and off.
    static let heartSmall = material(Material.favorite, CGSize(width: 20, height: 20))
    static let heartSmallOutline = material(Material.favoriteBorder, CGSize(width: 20, height: 20))
    /// The small heart a favourite row carries.
    static let smallHeart = material(Material.favorite, CGSize(width: 13, height: 13))
    static let more = material(Material.moreHoriz, CGSize(width: 24, height: 24))
    static let chevronDown = material(Material.keyboardArrowDown, CGSize(width: 28, height: 28))
    static let sort = material(Material.swapVert, CGSize(width: 24, height: 24))
    /// The empty cover's mark.
    static let disc = material(Material.musicNote, CGSize(width: 22, height: 22))

    /// The cloud on a provider's item (not in the library yet), in the secondary grey: a text attachment
    /// takes no tint.
    static var cloud: UIImage { Theme.light ? cloudOnLight : cloudOnDark }
    private static let cloudOnDark = cloud(UIColor(rgb: 0xA8A8A8))
    private static let cloudOnLight = cloud(UIColor(rgb: 0x6E6E6E))

    private static func cloud(_ colour: UIColor) -> UIImage {
        UIGraphicsBeginImageContextWithOptions(CGSize(width: 15, height: 12), false, 0)
        colour.set()
        let p = UIBezierPath()
        p.append(UIBezierPath(ovalIn: CGRect(x: 0.5, y: 5, width: 6.5, height: 6.5)))
        p.append(UIBezierPath(ovalIn: CGRect(x: 3.5, y: 1, width: 8, height: 8)))
        p.append(UIBezierPath(ovalIn: CGRect(x: 8, y: 4, width: 6.5, height: 6.5)))
        p.append(UIBezierPath(rect: CGRect(x: 3.5, y: 7, width: 8, height: 4.5)))
        p.fill()
        let image = UIGraphicsGetImageFromCurrentImageContext() ?? UIImage()
        UIGraphicsEndImageContext()
        return image.withRenderingMode(.alwaysOriginal)
    }

    /// Icon `path` (SVG path data on a 24 × 24 grid) filled, scaled to fit `size`, as a template.
    private static func material(_ path: String, _ size: CGSize) -> UIImage {
        let scale = min(size.width, size.height) / 24
        let bezier = MaterialPath.parse(path)
        bezier.apply(CGAffineTransform(translationX: (size.width - 24 * scale) / 2, y: (size.height - 24 * scale) / 2)
            .scaledBy(x: scale, y: scale))
        UIGraphicsBeginImageContextWithOptions(size, false, 0)
        UIColor.black.setFill()
        bezier.fill()
        let image = UIGraphicsGetImageFromCurrentImageContext() ?? UIImage()
        UIGraphicsEndImageContext()
        return image.withRenderingMode(.alwaysTemplate)
    }
}

/// The path data of the icons in use, as `androidx.compose.material.icons` builds them.
private enum Material {
    static let home = "M10 20v-6h4v6h5v-8h3L12 3L2 12h3v8Z"
    static let libraryMusic = "M20 2L8 2c-1.1 0 -2 0.9 -2 2v12c0 1.1 0.9 2 2 2h12c1.1 0 2 -0.9 2 -2L22 4c0 -1.1 -0.9 -2 -2 -2ZM18 7h-3v5.5c0 1.38 -1.12 2.5 -2.5 2.5S10 13.88 10 12.5s1.12 -2.5 2.5 -2.5c0.57 0 1.08 0.19 1.5 0.51L14 5h4v2ZM4 6L2 6v14c0 1.1 0.9 2 2 2h14v-2L4 20L4 6Z"
    static let search = "M15.5 14h-0.79l-0.28 -0.27C15.41 12.59 16 11.11 16 9.5C16 5.91 13.09 3 9.5 3S3 5.91 3 9.5S5.91 16 9.5 16c1.61 0 3.09 -0.59 4.23 -1.57l0.27 0.28v0.79l5 4.99L20.49 19l-4.99 -5ZM9.5 14C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5S14 7.01 14 9.5S11.99 14 9.5 14Z"
    static let settings = "M19.14 12.94c0.04 -0.3 0.06 -0.61 0.06 -0.94c0 -0.32 -0.02 -0.64 -0.07 -0.94l2.03 -1.58c0.18 -0.14 0.23 -0.41 0.12 -0.61l-1.92 -3.32c-0.12 -0.22 -0.37 -0.29 -0.59 -0.22l-2.39 0.96c-0.5 -0.38 -1.03 -0.7 -1.62 -0.94L14.4 2.81c-0.04 -0.24 -0.24 -0.41 -0.48 -0.41h-3.84c-0.24 0 -0.43 0.17 -0.47 0.41L9.25 5.35C8.66 5.59 8.12 5.92 7.63 6.29L5.24 5.33c-0.22 -0.08 -0.47 0 -0.59 0.22L2.74 8.87C2.62 9.08 2.66 9.34 2.86 9.48l2.03 1.58C4.84 11.36 4.8 11.69 4.8 12s0.02 0.64 0.07 0.94l-2.03 1.58c-0.18 0.14 -0.23 0.41 -0.12 0.61l1.92 3.32c0.12 0.22 0.37 0.29 0.59 0.22l2.39 -0.96c0.5 0.38 1.03 0.7 1.62 0.94l0.36 2.54c0.05 0.24 0.24 0.41 0.48 0.41h3.84c0.24 0 0.44 -0.17 0.47 -0.41l0.36 -2.54c0.59 -0.24 1.13 -0.56 1.62 -0.94l2.39 0.96c0.22 0.08 0.47 0 0.59 -0.22l1.92 -3.32c0.12 -0.22 0.07 -0.47 -0.12 -0.61L19.14 12.94ZM12 15.6c-1.98 0 -3.6 -1.62 -3.6 -3.6s1.62 -3.6 3.6 -3.6s3.6 1.62 3.6 3.6S13.98 15.6 12 15.6Z"
    static let playArrow = "M8 5v14l11 -7Z"
    static let pause = "M6 19h4L10 5L6 5v14ZM14 5v14h4L18 5h-4Z"
    static let skipNext = "M6 18l8.5 -6L6 6v12ZM16 6v12h2V6h-2Z"
    static let fastForward = "M4 18l8.5 -6L4 6v12ZM13 6v12l8.5 -6L13 6Z"
    static let fastRewind = "M11 18L11 6l-8.5 6l8.5 6ZM11.5 12l8.5 6L20 6l-8.5 6Z"
    static let shuffle = "M10.59 9.17L5.41 4L4 5.41l5.17 5.17l1.42 -1.41ZM14.5 4l2.04 2.04L4 18.59L5.41 20L17.96 7.46L20 9.5L20 4h-5.5ZM14.83 13.41l-1.41 1.41l3.13 3.13L14.5 20L20 20v-5.5l-2.04 2.04l-3.13 -3.13Z"
    static let repeatAll = "M7 7h10v3l4 -4l-4 -4v3L5 5v6h2L7 7ZM17 17L7 17v-3l-4 4l4 4v-3h12v-6h-2v4Z"
    static let repeatOne = "M7 7h10v3l4 -4l-4 -4v3L5 5v6h2L7 7ZM17 17L7 17v-3l-4 4l4 4v-3h12v-6h-2v4ZM13 15L13 9h-1l-2 1v1h1.5v4L13 15Z"
    static let queueMusic = "M15 6H3v2h12V6ZM15 10H3v2h12V10ZM3 16h8v-2H3V16ZM17 6v8.18C16.69 14.07 16.35 14 16 14c-1.66 0 -3 1.34 -3 3s1.34 3 3 3s3 -1.34 3 -3V8h3V6H17Z"
    /// The outlined speaker, drawn here (the filled one is `speaker`): the same body, tweeter and woofer.
    static let speakerOutline = "M7 2H17C18.1 2 19 2.9 19 4V20C19 21.1 18.1 22 17 22H7C5.9 22 5 21.1 5 20V4C5 2.9 5.9 2 7 2ZM7 4V20H17V4ZM12 5.4C12.88 5.4 13.6 6.12 13.6 7C13.6 7.88 12.88 8.6 12 8.6C11.12 8.6 10.4 7.88 10.4 7C10.4 6.12 11.12 5.4 12 5.4ZM12 10.7C14.1 10.7 15.8 12.4 15.8 14.5C15.8 16.6 14.1 18.3 12 18.3C9.9 18.3 8.2 16.6 8.2 14.5C8.2 12.4 9.9 10.7 12 10.7ZM12 12.1C10.67 12.1 9.6 13.17 9.6 14.5C9.6 15.83 10.67 16.9 12 16.9C13.33 16.9 14.4 15.83 14.4 14.5C14.4 13.17 13.33 12.1 12 12.1Z"
    static let groups = "M12 12.75c1.63 0 3.07 0.39 4.24 0.9c1.08 0.48 1.76 1.56 1.76 2.73V18H6v-1.61c0 -1.18 0.68 -2.26 1.76 -2.73c1.17 -0.52 2.61 -0.91 4.24 -0.91ZM4 13c1.1 0 2 -0.9 2 -2s-0.9 -2 -2 -2s-2 0.9 -2 2s0.9 2 2 2ZM5.13 14.1c-0.37 -0.06 -0.74 -0.1 -1.13 -0.1c-0.99 0 -1.93 0.21 -2.78 0.58C0.48 14.9 0 15.62 0 16.43V18h4.5v-1.61c0 -0.83 0.23 -1.61 0.63 -2.29ZM20 13c1.1 0 2 -0.9 2 -2s-0.9 -2 -2 -2s-2 0.9 -2 2s0.9 2 2 2ZM24 16.43c0 -0.81 -0.48 -1.53 -1.22 -1.85c-0.85 -0.37 -1.79 -0.58 -2.78 -0.58c-0.39 0 -0.76 0.04 -1.13 0.1c0.4 0.68 0.63 1.46 0.63 2.29V18H24v-1.57ZM12 6c1.66 0 3 1.34 3 3s-1.34 3 -3 3s-3 -1.34 -3 -3s1.34 -3 3 -3Z"
    static let speaker = "M17 2H7c-1.1 0 -1.99 0.9 -1.99 2L5 20c0 1.1 0.9 2 2 2h10c1.1 0 2 -0.9 2 -2V4c0 -1.1 -0.9 -2 -2 -2ZM12 4c1.1 0 2 0.9 2 2s-0.9 2 -2 2c-1.11 0 -2 -0.9 -2 -2s0.89 -2 2 -2ZM12 20c-2.76 0 -5 -2.24 -5 -5s2.24 -5 5 -5s5 2.24 5 5s-2.24 5 -5 5ZM12 12c-1.66 0 -3 1.34 -3 3s1.34 3 3 3s3 -1.34 3 -3s-1.34 -3 -3 -3Z"
    static let dragHandle = "M20 9H4v2h16V9ZM4 15h16v-2H4V15Z"
    static let lyrics = "M14 9c0 -2.04 1.24 -3.79 3 -4.57V4c0 -1.1 -0.9 -2 -2 -2H4C2.9 2 2.01 2.9 2.01 4L2 22l4 -4h9c1.1 0 2 -0.9 2 -2v-2.42C15.24 12.8 14 11.05 14 9ZM10 14H6v-2h4V14ZM13 11H6V9h7V11ZM13 8H6V6h7V8ZM20 6.18C19.69 6.07 19.35 6 19 6c-1.66 0 -3 1.34 -3 3c0 1.66 1.34 3 3 3s3 -1.34 3 -3V3h2V1h-4V6.18Z"
    static let favoriteBorder = "M16.5 3c-1.74 0 -3.41 0.81 -4.5 2.09C10.91 3.81 9.24 3 7.5 3C4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45 -1.32C18.6 15.36 22 12.28 22 8.5C22 5.42 19.58 3 16.5 3ZM12.1 18.55l-0.1 0.1l-0.1 -0.1C7.14 14.24 4 11.39 4 8.5C4 6.5 5.5 5 7.5 5c1.54 0 3.04 0.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5c0 2.89 -3.14 5.74 -7.9 10.05Z"
    static let favorite = "M12 21.35l-1.45 -1.32C5.4 15.36 2 12.28 2 8.5C2 5.42 4.42 3 7.5 3c1.74 0 3.41 0.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3C19.58 3 22 5.42 22 8.5c0 3.78 -3.4 6.86 -8.55 11.54L12 21.35Z"
    static let moreHoriz = "M6 10c-1.1 0 -2 0.9 -2 2s0.9 2 2 2s2 -0.9 2 -2s-0.9 -2 -2 -2ZM18 10c-1.1 0 -2 0.9 -2 2s0.9 2 2 2s2 -0.9 2 -2s-0.9 -2 -2 -2ZM12 10c-1.1 0 -2 0.9 -2 2s0.9 2 2 2s2 -0.9 2 -2s-0.9 -2 -2 -2Z"
    static let keyboardArrowDown = "M7.41 8.59L12 13.17l4.59 -4.58L18 10l-6 6l-6 -6l1.41 -1.41Z"
    static let swapVert = "M16 17.01V10h-2v7.01h-3L15 21l4 -3.99h-3ZM9 3L5 6.99h3V14h2V6.99h3L9 3Z"
    static let musicNote = "M12 3v10.55c-0.59 -0.34 -1.27 -0.55 -2 -0.55c-2.21 0 -4 1.79 -4 4s1.79 4 4 4s4 -1.79 4 -4V7h4V3h-6Z"
}

/// SVG path data as the Material icons use it: M L H V C S Q T and Z, absolute and relative.
private enum MaterialPath {
    static func parse(_ d: String) -> UIBezierPath {
        let p = UIBezierPath()
        var at = CGPoint.zero, start = CGPoint.zero
        var control: CGPoint?  // the last curve's second control point, for S and T
        var command: Character = "M"
        var numbers: [CGFloat] = []
        var token = ""
        func flushNumber() {
            if let v = Double(token) { numbers.append(CGFloat(v)) }
            token = ""
        }
        func run(_ c: Character, _ n: [CGFloat]) {
            let rel = c.isLowercase
            let o = rel ? at : .zero
            switch c.uppercased().first! {
            case "M":
                at = CGPoint(x: o.x + n[0], y: o.y + n[1]); start = at; p.move(to: at); control = nil
            case "L":
                at = CGPoint(x: o.x + n[0], y: o.y + n[1]); p.addLine(to: at); control = nil
            case "H":
                at = CGPoint(x: (rel ? at.x : 0) + n[0], y: at.y); p.addLine(to: at); control = nil
            case "V":
                at = CGPoint(x: at.x, y: (rel ? at.y : 0) + n[0]); p.addLine(to: at); control = nil
            case "C":
                let c1 = CGPoint(x: o.x + n[0], y: o.y + n[1]), c2 = CGPoint(x: o.x + n[2], y: o.y + n[3])
                at = CGPoint(x: o.x + n[4], y: o.y + n[5]); p.addCurve(to: at, controlPoint1: c1, controlPoint2: c2); control = c2
            case "S":
                let c1 = control.map { CGPoint(x: 2 * at.x - $0.x, y: 2 * at.y - $0.y) } ?? at
                let c2 = CGPoint(x: o.x + n[0], y: o.y + n[1])
                at = CGPoint(x: o.x + n[2], y: o.y + n[3]); p.addCurve(to: at, controlPoint1: c1, controlPoint2: c2); control = c2
            case "Q":
                let c1 = CGPoint(x: o.x + n[0], y: o.y + n[1])
                at = CGPoint(x: o.x + n[2], y: o.y + n[3]); p.addQuadCurve(to: at, controlPoint: c1); control = c1
            case "T":
                let c1 = control.map { CGPoint(x: 2 * at.x - $0.x, y: 2 * at.y - $0.y) } ?? at
                at = CGPoint(x: o.x + n[0], y: o.y + n[1]); p.addQuadCurve(to: at, controlPoint: c1); control = c1
            default:
                p.close(); at = start; control = nil
            }
        }
        let arity: [Character: Int] = ["M": 2, "L": 2, "H": 1, "V": 1, "C": 6, "S": 4, "Q": 4, "T": 2, "Z": 0]
        func drain() {
            let n = arity[command.uppercased().first!] ?? 0
            if n == 0 { run(command, []); return }
            while numbers.count >= n {
                run(command, Array(numbers.prefix(n)))
                numbers.removeFirst(n)
                // Pairs after a move are lines.
                if command == "M" { command = "L" } else if command == "m" { command = "l" }
            }
        }
        for ch in d {
            if ch.isLetter {
                flushNumber(); drain(); command = ch; numbers = []
                if arity[ch.uppercased().first!] == 0 { drain() }
            } else if ch == " " || ch == "," {
                flushNumber()
            } else if ch == "-" && !token.isEmpty && token.last != "e" {
                flushNumber(); token = "-"
            } else {
                token.append(ch)
            }
        }
        flushNumber(); drain()
        return p
    }
}
