import UIKit

/// A list row: cover (or the track number on an album), title, subtitle.
final class ItemCell: UITableViewCell {
    static let id = "item"
    let cover = CoverView()
    private let number = UILabel()
    private let title = UILabel()
    private let detail = UILabel()
    private let lit = UIView()
    private let mark = UIImageView(image: Glyph.smallHeart)
    private let bar = UIProgressView(progressViewStyle: .bar)
    private var coverWidth: NSLayoutConstraint!

    override init(style: UITableViewCell.CellStyle, reuseIdentifier: String?) {
        super.init(style: style, reuseIdentifier: reuseIdentifier)
        backgroundColor = Theme.background
        let selected = UIView()
        selected.backgroundColor = Theme.track
        selectedBackgroundView = selected
        title.font = UIFont.preferredFont(forTextStyle: .body)
        title.adjustsFontForContentSizeCategory = true
        title.textColor = Theme.label
        detail.font = UIFont.preferredFont(forTextStyle: .footnote)
        detail.adjustsFontForContentSizeCategory = true
        detail.textColor = Theme.secondary
        number.font = UIFont.monospacedDigitSystemFont(ofSize: 15, weight: .regular)
        number.textColor = Theme.secondary
        number.textAlignment = .center
        lit.backgroundColor = Theme.accent
        lit.layer.cornerRadius = 2
        let text = UIStackView(arrangedSubviews: [title, detail])
        text.axis = .vertical
        text.spacing = 2
        mark.tintColor = Theme.secondary
        mark.setContentHuggingPriority(.required, for: .horizontal)
        bar.progressTintColor = Theme.label
        bar.trackTintColor = Theme.track
        for v in [cover, number, text, lit, mark, bar] {
            v.translatesAutoresizingMaskIntoConstraints = false
            contentView.addSubview(v)
        }
        coverWidth = cover.widthAnchor.constraint(equalToConstant: 44)
        NSLayoutConstraint.activate([
            cover.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            cover.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            coverWidth,
            cover.heightAnchor.constraint(equalTo: cover.widthAnchor),
            number.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 8),
            number.widthAnchor.constraint(equalToConstant: 36),
            number.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            text.leadingAnchor.constraint(equalTo: cover.trailingAnchor, constant: 12),
            text.trailingAnchor.constraint(equalTo: mark.leadingAnchor, constant: -8),
            text.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            bar.leadingAnchor.constraint(equalTo: text.leadingAnchor),
            bar.trailingAnchor.constraint(equalTo: text.trailingAnchor),
            bar.topAnchor.constraint(equalTo: text.bottomAnchor, constant: 4),
            bar.heightAnchor.constraint(equalToConstant: 2),
            mark.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -12),
            mark.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            lit.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 4),
            lit.widthAnchor.constraint(equalToConstant: 4),
            lit.heightAnchor.constraint(equalToConstant: 20),
            lit.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            contentView.heightAnchor.constraint(greaterThanOrEqualToConstant: 60),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    /// `numbered`: the album page's rows show the track number instead of a cover.
    func show(_ item: Item, numbered: Bool, current: Bool = false) {
        let line: String
        switch item.kind {
        case "song" where item.progress != nil:
            let p = item.progress!
            line = Say.downloading(percent: p.percent, speed: p.speed, left: p.left)
        case "song":
            line = [item.subtitle, item.seconds > 0 ? Fmt.clock(ms: item.seconds * 1000) : ""].filter { !$0.isEmpty }.joined(separator: " · ")
        case "artist":
            line = item.count > 0 ? Say.albums(item.count) : ""
        case "playlist":
            line = Say.songs(item.count)
        case "genre":
            line = Say.albums(item.count)
        case "smart":
            line = ""
        default:
            line = item.subtitle
        }
        // With no second line (an album's own tracks) the cloud leads the title instead.
        let name: String = {
            if item.kind == "smart", item.title.isEmpty { return Say.smart(item.count) }
            return item.title.isEmpty ? item.id : item.title
        }()
        title.attributedText = remoteText(name, remote: item.external && line.isEmpty, font: title.font,
                                          colour: current ? Theme.accent : Theme.label)
        detail.attributedText = remoteText(line, remote: item.external && !line.isEmpty, font: detail.font, colour: Theme.secondary)
        lit.isHidden = !current
        mark.isHidden = !(item.kind == "song" && Core.shared.isFavorite(item))
        bar.isHidden = (item.progress?.percent ?? -1) < 0
        bar.progress = Float(max(0, item.progress?.percent ?? 0)) / 100
        detail.isHidden = line.isEmpty
        let bare = numbered || item.kind == "genre" || item.kind == "recent"
        cover.isHidden = bare
        number.isHidden = !numbered
        number.text = item.track > 0 ? "\(item.track)" : ""
        coverWidth.constant = bare ? 28 : 44
        cover.layer.cornerRadius = item.kind == "artist" ? 22 : 6
        if !cover.isHidden { cover.show(item.cover, points: 44) }
        accessoryType = item.kind == "song" ? .none : .disclosureIndicator
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        cover.cancel()
    }
}

/// `text`, led by the cloud when the item is a provider's (not in the library yet). The cloud comes first
/// so the text, not the mark, is what gets cut short.
func remoteText(_ text: String, remote: Bool, font: UIFont, colour: UIColor) -> NSAttributedString {
    let out = NSMutableAttributedString()
    if remote {
        let mark = NSTextAttachment()
        mark.image = Glyph.cloud
        let side = font.capHeight + 2
        mark.bounds = CGRect(x: 0, y: (font.capHeight - side) / 2, width: side * 1.25, height: side)
        out.append(NSAttributedString(attachment: mark))
        if !text.isEmpty { out.append(NSAttributedString(string: " ")) }
    }
    out.append(NSAttributedString(string: text, attributes: [.font: font, .foregroundColor: colour]))
    return out
}

/// An album card: cover, title, subtitle.
final class CardCell: UICollectionViewCell {
    static let id = "card"
    let cover = CoverView()
    private let title = UILabel()
    private let detail = UILabel()

    override init(frame: CGRect) {
        super.init(frame: frame)
        title.font = UIFont.preferredFont(forTextStyle: .footnote)
        title.adjustsFontForContentSizeCategory = true
        title.textColor = Theme.label
        detail.font = UIFont.preferredFont(forTextStyle: .caption1)
        detail.adjustsFontForContentSizeCategory = true
        detail.textColor = Theme.secondary
        for v in [cover, title, detail] {
            v.translatesAutoresizingMaskIntoConstraints = false
            contentView.addSubview(v)
        }
        NSLayoutConstraint.activate([
            cover.topAnchor.constraint(equalTo: contentView.topAnchor),
            cover.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            cover.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            cover.heightAnchor.constraint(equalTo: cover.widthAnchor),
            title.topAnchor.constraint(equalTo: cover.bottomAnchor, constant: 6),
            title.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            title.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            detail.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 1),
            detail.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            detail.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func show(_ item: Item, width: CGFloat) {
        mix?.removeFromSuperview()
        mix = nil
        if item.kind == "mix" {
            let tile = MixTile(item, side: width)
            tile.translatesAutoresizingMaskIntoConstraints = false
            contentView.addSubview(tile)
            NSLayoutConstraint.activate([
                tile.leadingAnchor.constraint(equalTo: cover.leadingAnchor),
                tile.trailingAnchor.constraint(equalTo: cover.trailingAnchor),
                tile.topAnchor.constraint(equalTo: cover.topAnchor),
                tile.bottomAnchor.constraint(equalTo: cover.bottomAnchor),
            ])
            mix = tile
            // The tile carries its name: no line under it.
            title.text = nil
            detail.text = nil
            return
        }
        title.text = item.title
        let line: String
        switch item.kind {
        case "artist": line = Say.albums(item.count)
        case "playlist": line = Say.songs(item.count)
        default: line = item.subtitle
        }
        detail.attributedText = remoteText(line, remote: item.external, font: detail.font, colour: Theme.secondary)
        cover.layer.cornerRadius = item.kind == "artist" ? width / 2 : 6
        cover.show(item.cover, points: width)
    }

    private var mix: MixTile?

    override func prepareForReuse() {
        super.prepareForReuse()
        cover.cancel()
    }
}

/// A mix's art: its four covers in a square, or its one when it has fewer.
final class Collage: UIView {
    private var views: [CoverView] = []

    init(covers all: [String], side: CGFloat) {
        super.init(frame: .zero)
        backgroundColor = Theme.track
        layer.cornerRadius = 6
        clipsToBounds = true
        let covers = Array(all.prefix(all.count >= 4 ? 4 : 1))
        let half = covers.count == 4
        for (i, id) in covers.enumerated() {
            let v = CoverView()
            v.layer.cornerRadius = 0
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
            let f: CGFloat = half ? 0.5 : 1
            NSLayoutConstraint.activate([
                v.widthAnchor.constraint(equalTo: widthAnchor, multiplier: f),
                v.heightAnchor.constraint(equalTo: heightAnchor, multiplier: f),
                half && i % 2 == 1 ? v.trailingAnchor.constraint(equalTo: trailingAnchor) : v.leadingAnchor.constraint(equalTo: leadingAnchor),
                half && i >= 2 ? v.bottomAnchor.constraint(equalTo: bottomAnchor) : v.topAnchor.constraint(equalTo: topAnchor),
            ])
            v.show(id, points: half ? side / 2 : side)
            views.append(v)
        }
    }

    required init?(coder: NSCoder) { fatalError() }

    override func removeFromSuperview() {
        views.forEach { $0.cancel() }
        super.removeFromSuperview()
    }
}

/// A "For you" tile: the mix's collage and its name over a dark band.
final class MixTile: UIView {
    init(_ item: Item, side: CGFloat) {
        super.init(frame: .zero)
        layer.cornerRadius = 6
        clipsToBounds = true
        let art = Collage(covers: item.covers, side: side)
        let band = GradientView(colours: [UIColor.black.withAlphaComponent(0), UIColor.black.withAlphaComponent(0.94)])
        let name = UILabel()
        name.text = Say.mix(item.count)
        name.font = UIFont.systemFont(ofSize: 17, weight: .bold)
        name.textColor = .white
        name.numberOfLines = 2
        for v in [art, band, name] as [UIView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        NSLayoutConstraint.activate([
            art.leadingAnchor.constraint(equalTo: leadingAnchor),
            art.trailingAnchor.constraint(equalTo: trailingAnchor),
            art.topAnchor.constraint(equalTo: topAnchor),
            art.bottomAnchor.constraint(equalTo: bottomAnchor),
            band.leadingAnchor.constraint(equalTo: leadingAnchor),
            band.trailingAnchor.constraint(equalTo: trailingAnchor),
            band.bottomAnchor.constraint(equalTo: bottomAnchor),
            band.heightAnchor.constraint(equalTo: heightAnchor, multiplier: 0.55),
            name.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            name.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            name.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -10),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }
}

final class GradientView: UIView {
    override class var layerClass: AnyClass { CAGradientLayer.self }

    init(colours: [UIColor]) {
        super.init(frame: .zero)
        (layer as! CAGradientLayer).colors = colours.map { $0.cgColor }
        isUserInteractionEnabled = false
    }

    required init?(coder: NSCoder) { fatalError() }
}

/// A horizontal shelf of cards in one table row.
final class ShelfCell: UITableViewCell, UICollectionViewDataSource, UICollectionViewDelegateFlowLayout {
    static let id = "shelf"
    static let card: CGFloat = 132
    static let height: CGFloat = card + 52
    /// A shelf of mix tiles: they carry their names, so no lines under them.
    static let bare: CGFloat = card + 8

    /// The row's height for `items`.
    static func height(_ items: [Item]) -> CGFloat {
        items.allSatisfy { $0.kind == "mix" } && !items.isEmpty ? bare : height
    }

    private let strip: UICollectionView
    private let layout: UICollectionViewFlowLayout
    private var stripHeight: NSLayoutConstraint!
    private var items: [Item] = []
    var picked: ((Item) -> Void)?
    var held: ((Item) -> Void)?

    override init(style: UITableViewCell.CellStyle, reuseIdentifier: String?) {
        let layout = UICollectionViewFlowLayout()
        self.layout = layout
        layout.scrollDirection = .horizontal
        layout.itemSize = CGSize(width: ShelfCell.card, height: ShelfCell.height - 8)
        layout.minimumLineSpacing = 12
        layout.sectionInset = UIEdgeInsets(top: 0, left: 16, bottom: 0, right: 16)
        strip = UICollectionView(frame: .zero, collectionViewLayout: layout)
        super.init(style: style, reuseIdentifier: reuseIdentifier)
        backgroundColor = Theme.background
        selectionStyle = .none
        strip.backgroundColor = Theme.background
        strip.showsHorizontalScrollIndicator = false
        strip.register(CardCell.self, forCellWithReuseIdentifier: CardCell.id)
        strip.dataSource = self
        strip.delegate = self
        strip.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(strip)
        stripHeight = strip.heightAnchor.constraint(equalToConstant: ShelfCell.height)
        NSLayoutConstraint.activate([
            strip.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            strip.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            strip.topAnchor.constraint(equalTo: contentView.topAnchor),
            stripHeight,
            contentView.bottomAnchor.constraint(equalTo: strip.bottomAnchor),
        ])
        let press = UILongPressGestureRecognizer(target: self, action: #selector(pressed(_:)))
        strip.addGestureRecognizer(press)
    }

    required init?(coder: NSCoder) { fatalError() }

    func show(_ items: [Item]) {
        self.items = items
        let height = ShelfCell.height(items)
        stripHeight.constant = height
        layout.itemSize = CGSize(width: ShelfCell.card, height: height - 8)
        strip.reloadData()
        strip.setContentOffset(.zero, animated: false)
    }

    func collectionView(_ collectionView: UICollectionView, numberOfItemsInSection section: Int) -> Int { items.count }

    func collectionView(_ collectionView: UICollectionView, cellForItemAt indexPath: IndexPath) -> UICollectionViewCell {
        let cell = collectionView.dequeueReusableCell(withReuseIdentifier: CardCell.id, for: indexPath) as! CardCell
        cell.show(items[indexPath.item], width: ShelfCell.card)
        return cell
    }

    func collectionView(_ collectionView: UICollectionView, didSelectItemAt indexPath: IndexPath) {
        picked?(items[indexPath.item])
    }

    @objc private func pressed(_ g: UILongPressGestureRecognizer) {
        guard g.state == .began, let path = strip.indexPathForItem(at: g.location(in: strip)) else { return }
        held?(items[path.item])
    }
}

/// A section's title, where the covers under it start. The table's own header label follows its
/// separator inset, which the list rows set past their covers.
final class SectionHeading: UITableViewHeaderFooterView {
    static let id = "heading"
    static let height: CGFloat = 40
    let title = UILabel()

    override init(reuseIdentifier: String?) {
        super.init(reuseIdentifier: reuseIdentifier)
        backgroundView = UIView()
        backgroundView?.backgroundColor = Theme.background
        title.textColor = Theme.label
        title.font = UIFont.preferredFont(forTextStyle: .headline)
        title.adjustsFontForContentSizeCategory = true
        title.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(title)
        NSLayoutConstraint.activate([
            title.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            title.trailingAnchor.constraint(lessThanOrEqualTo: contentView.trailingAnchor, constant: -16),
            title.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -6),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }
}

/// Two cards side by side, for album grids.
final class PairCell: UITableViewCell {
    static let id = "pair"
    static let gap: CGFloat = 16
    private let left = CardButton()
    private let right = CardButton()
    var picked: ((Item) -> Void)?
    var held: ((Item) -> Void)?

    override init(style: UITableViewCell.CellStyle, reuseIdentifier: String?) {
        super.init(style: style, reuseIdentifier: reuseIdentifier)
        backgroundColor = Theme.background
        selectionStyle = .none
        let row = UIStackView(arrangedSubviews: [left, right])
        row.axis = .horizontal
        row.distribution = .fillEqually
        row.spacing = PairCell.gap
        row.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: PairCell.gap),
            row.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -PairCell.gap),
            row.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 6),
            row.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -10),
        ])
        for b in [left, right] {
            b.addTarget(self, action: #selector(tapped(_:)), for: .touchUpInside)
            b.addGestureRecognizer(UILongPressGestureRecognizer(target: self, action: #selector(pressed(_:))))
        }
    }

    required init?(coder: NSCoder) { fatalError() }

    static var width: CGFloat { (UIScreen.main.bounds.width - gap * 3) / 2 }

    func show(_ a: Item, _ b: Item?) {
        left.show(a, width: PairCell.width)
        right.isHidden = b == nil
        if let b { right.show(b, width: PairCell.width) }
    }

    @objc private func tapped(_ b: CardButton) {
        if let item = b.item { picked?(item) }
    }

    @objc private func pressed(_ g: UILongPressGestureRecognizer) {
        guard g.state == .began, let item = (g.view as? CardButton)?.item else { return }
        held?(item)
    }
}

final class CardButton: UIControl {
    private let cover = CoverView()
    private let title = UILabel()
    private let detail = UILabel()
    private(set) var item: Item?

    override init(frame: CGRect) {
        super.init(frame: frame)
        title.font = UIFont.preferredFont(forTextStyle: .footnote)
        title.adjustsFontForContentSizeCategory = true
        title.textColor = Theme.label
        detail.font = UIFont.preferredFont(forTextStyle: .caption1)
        detail.adjustsFontForContentSizeCategory = true
        detail.textColor = Theme.secondary
        for v in [cover, title, detail] {
            v.isUserInteractionEnabled = false
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        NSLayoutConstraint.activate([
            cover.topAnchor.constraint(equalTo: topAnchor),
            cover.leadingAnchor.constraint(equalTo: leadingAnchor),
            cover.trailingAnchor.constraint(equalTo: trailingAnchor),
            cover.heightAnchor.constraint(equalTo: cover.widthAnchor),
            title.topAnchor.constraint(equalTo: cover.bottomAnchor, constant: 6),
            title.leadingAnchor.constraint(equalTo: leadingAnchor),
            title.trailingAnchor.constraint(equalTo: trailingAnchor),
            detail.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 1),
            detail.leadingAnchor.constraint(equalTo: leadingAnchor),
            detail.trailingAnchor.constraint(equalTo: trailingAnchor),
            detail.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    override var isHighlighted: Bool {
        didSet { alpha = isHighlighted ? 0.6 : 1 }
    }

    func show(_ item: Item, width: CGFloat) {
        self.item = item
        title.text = item.title
        detail.attributedText = remoteText(item.subtitle, remote: item.external, font: detail.font, colour: Theme.secondary)
        cover.show(item.cover, points: width)
    }
}

/// The big cover and title over an album, artist or playlist, then Android's row: shuffle in a circle,
/// one wide Play, the heart (albums and artists) and ⋯ in circles.
final class PageHeader: UIView {
    var play: (() -> Void)?
    var shuffle: (() -> Void)?
    var more: (() -> Void)?
    private let heart = UIButton(type: .system)
    private let playButton = PageHeader.pill(Say.play, Glyph.play, filled: true)
    private let shuffleButton = PageHeader.circle(Glyph.shuffle, Say.shuffle)
    private let item: Item
    /// An album's artist line was tapped: its artist id.
    var artist: ((String) -> Void)?
    private let artistId: String

    init(head: [String: Any]) {
        let kind = head["k"] as? String ?? ""
        artistId = kind == "album" ? head["artistId"] as? String ?? "" : ""
        item = Item(head)
        super.init(frame: CGRect(x: 0, y: 0, width: UIScreen.main.bounds.width, height: 10))
        let side: CGFloat = 220
        let cover: UIView
        if kind == "mix" {
            cover = Collage(covers: head["covers"] as? [String] ?? [], side: side)
        } else {
            let one = CoverView()
            if kind == "artist" { one.layer.cornerRadius = side / 2 }
            one.show(head["c"] as? String ?? "", points: side)
            cover = one
        }
        let title = UILabel()
        if kind == "mix" {
            title.text = Say.mix(head["n"] as? Int ?? -1)
        } else if kind == "smart" {
            let named = head["t"] as? String ?? ""
            title.text = named.isEmpty ? Say.smart(head["n"] as? Int ?? -1) : named
        } else {
            title.text = head["t"] as? String
        }
        title.font = UIFont.preferredFont(forTextStyle: .title2)
        title.adjustsFontForContentSizeCategory = true
        title.textColor = Theme.label
        title.textAlignment = .center
        title.numberOfLines = 2
        let sub = UILabel()
        sub.text = head["s"] as? String
        sub.font = UIFont.preferredFont(forTextStyle: .title3)
        sub.adjustsFontForContentSizeCategory = true
        sub.textColor = Theme.label.withAlphaComponent(0.6)
        sub.textAlignment = .center
        if !artistId.isEmpty {
            sub.isUserInteractionEnabled = true
            sub.addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(artistTapped)))
        }
        let meta = UILabel()
        var bits: [String] = []
        if let g = head["g"] as? String, !g.isEmpty { bits.append(g) }
        if let y = head["y"] as? Int, y > 0 { bits.append("\(y)") }
        if kind == "artist", let n = head["n"] as? Int { bits.append(Say.albums(n)) }
        if kind != "artist", let n = head[kind == "mix" || kind == "smart" ? "count" : "n"] as? Int { bits.append(Say.songs(n)) }
        if let sec = head["sec"] as? Int, sec > 0 { bits.append(Fmt.length(seconds: sec)) }
        meta.text = bits.joined(separator: " · ")
        meta.font = UIFont.preferredFont(forTextStyle: .footnote)
        meta.adjustsFontForContentSizeCategory = true
        meta.textColor = Theme.secondary
        meta.textAlignment = .center
        playButton.addTarget(self, action: #selector(playTapped), for: .touchUpInside)
        shuffleButton.addTarget(self, action: #selector(shuffleTapped), for: .touchUpInside)
        let moreButton = PageHeader.circle(Glyph.more, Say.more)
        moreButton.addTarget(self, action: #selector(moreTapped), for: .touchUpInside)
        var row: [UIView] = [shuffleButton, playButton]
        if (kind == "album" || kind == "artist") && !item.external {
            PageHeader.round(heart)
            heart.accessibilityLabel = Say.favorite
            heart.addTarget(self, action: #selector(heartTapped), for: .touchUpInside)
            NotificationCenter.default.addObserver(self, selector: #selector(paintHeart), name: .noriFavorites, object: nil)
            paintHeart()
            row.append(heart)
        }
        row.append(moreButton)
        let buttons = UIStackView(arrangedSubviews: row)
        buttons.axis = .horizontal
        buttons.alignment = .center
        buttons.spacing = 12
        let stack = UIStackView(arrangedSubviews: [cover, title, sub, meta, buttons])
        stack.axis = .vertical
        stack.alignment = .center
        stack.spacing = 6
        stack.setCustomSpacing(16, after: cover)
        stack.setCustomSpacing(1, after: title)
        stack.setCustomSpacing(16, after: meta)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            cover.widthAnchor.constraint(equalToConstant: side),
            cover.heightAnchor.constraint(equalToConstant: side),
            buttons.widthAnchor.constraint(equalTo: stack.widthAnchor),
            playButton.heightAnchor.constraint(equalToConstant: 44),
            buttons.heightAnchor.constraint(equalToConstant: 44),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 12),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -16),
        ])
        let size = systemLayoutSizeFitting(
            CGSize(width: UIScreen.main.bounds.width, height: UIView.layoutFittingCompressedSize.height),
            withHorizontalFittingPriority: .required, verticalFittingPriority: .fittingSizeLevel
        )
        frame.size.height = size.height
    }

    required init?(coder: NSCoder) { fatalError() }

    @objc private func artistTapped() { artist?(artistId) }

    @objc private func paintHeart() {
        let on = Core.shared.isFavorite(item)
        heart.setImage(on ? Glyph.heartFilled : Glyph.heart, for: .normal)
        heart.accessibilityTraits = on ? [.button, .selected] : .button
    }

    @objc private func heartTapped() {
        Core.shared.favorite(item, !Core.shared.isFavorite(item))
        paintHeart()
    }

    @objc private func moreTapped() { more?() }

    /// A 44 pt round button: the plate behind a glyph.
    static func circle(_ image: UIImage, _ label: String) -> UIButton {
        let b = UIButton(type: .system)
        b.setImage(image, for: .normal)
        b.accessibilityLabel = label
        round(b)
        return b
    }

    private static func round(_ b: UIButton) {
        b.backgroundColor = Theme.row
        b.tintColor = Theme.accent
        b.layer.cornerRadius = 22
        b.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([b.widthAnchor.constraint(equalToConstant: 44), b.heightAnchor.constraint(equalToConstant: 44)])
    }

    static func pill(_ title: String, _ image: UIImage, filled: Bool) -> UIButton {
        let b = UIButton(type: .system)
        b.setTitle(" " + title, for: .normal)
        b.setImage(image, for: .normal)
        b.titleLabel?.font = UIFont.preferredFont(forTextStyle: .headline)
        b.titleLabel?.adjustsFontForContentSizeCategory = true
        b.backgroundColor = filled ? Theme.accent : Theme.row
        b.tintColor = filled ? Theme.background : Theme.accent
        b.layer.cornerRadius = 10
        return b
    }

    /// The buttons as the core says they stand (`nori_ios_hero`): Play is Pause while this page's queue
    /// sounds, and Shuffle lights while that queue shuffles.
    func paint(hero bits: Int32) {
        let pausing = bits & 4 != 0
        playButton.setTitle(" " + (pausing ? Say.pause : Say.play), for: .normal)
        playButton.setImage(pausing ? Glyph.pause : Glyph.play, for: .normal)
        let lit = bits & 1 != 0
        shuffleButton.backgroundColor = lit ? Theme.accent : Theme.row
        shuffleButton.tintColor = lit ? Theme.background : Theme.accent
        shuffleButton.accessibilityTraits = lit ? [.button, .selected] : .button
    }

    @objc private func playTapped() { play?() }
    @objc private func shuffleTapped() { shuffle?() }
}

/// Any page the library reads: a list, shelves, a grid of albums, or a collection under its header.
class PageController: UITableViewController {
    let kind: Int32
    let arg: String
    private(set) var token: UInt64 = 0
    private(set) var answer: PageAnswer?
    /// Each table section: a list, a shelf (one row), or a grid (two cards a row).
    private var rows: [(section: Section, layout: Layout)] = []
    private var more = false
    private var nextPage = 0
    private var loadingMore = false
    /// The list runs in the order of a name: the table shows the A–Z index.
    private var letters = false
    /// An index letter not loaded yet: pages load until it is.
    private var wantedLetter: String?
    private static let index = ["#"] + "ABCDEFGHIJKLMNOPQRSTUVWXYZ".map(String.init)
    private let empty = EmptyState(title: "", detail: "")

    enum Layout { case list, shelf, grid }

    /// Pages drawn under a header with their cover and name.
    private static let headed: Set<Int32> = [NORI_PAGE_ALBUM, NORI_PAGE_ARTIST, NORI_PAGE_PLAYLIST, NORI_PAGE_MIX, NORI_PAGE_SMART]

    init(kind: Int32, arg: String = "", title: String) {
        self.kind = kind
        self.arg = arg
        super.init(style: .plain)
        self.title = title
    }

    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        tableView.backgroundColor = Theme.background
        tableView.separatorColor = Theme.hairline
        tableView.separatorInset = UIEdgeInsets(top: 0, left: 72, bottom: 0, right: 0)
        tableView.register(ItemCell.self, forCellReuseIdentifier: ItemCell.id)
        tableView.register(ShelfCell.self, forCellReuseIdentifier: ShelfCell.id)
        tableView.register(PairCell.self, forCellReuseIdentifier: PairCell.id)
        tableView.estimatedRowHeight = 60
        tableView.rowHeight = UITableView.automaticDimension
        tableView.tableFooterView = UIView()
        additionalSafeAreaInsets.bottom = MiniPlayer.height
        if navigationController?.viewControllers.first !== self {
            navigationItem.largeTitleDisplayMode = .never
        }
        // A page under its own header carries its name there; the bar above stays empty from the
        // start, as Apple Music's does, rather than losing the name once the page is read.
        if PageController.headed.contains(kind) {
            navigationItem.title = ""
        }
        let refresh = UIRefreshControl()
        refresh.tintColor = Theme.secondary
        refresh.addTarget(self, action: #selector(reload), for: .valueChanged)
        refreshControl = refresh
        empty.translatesAutoresizingMaskIntoConstraints = false
        empty.isHidden = true
        view.addSubview(empty)
        // The view is the table, a scroll view: its own edges are the content's, which has no width.
        let frame = tableView.frameLayoutGuide
        NSLayoutConstraint.activate([
            empty.leadingAnchor.constraint(equalTo: frame.leadingAnchor, constant: 32),
            empty.trailingAnchor.constraint(equalTo: frame.trailingAnchor, constant: -32),
            empty.centerYAnchor.constraint(equalTo: view.safeAreaLayoutGuide.centerYAnchor, constant: -40),
        ])
        NotificationCenter.default.addObserver(self, selector: #selector(reload), name: .noriOpened, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(nowChanged), name: .noriNow, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(paintVisibleRows), name: .noriFavorites, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(reload), name: .noriDownloads, object: nil)
        tableView.sectionIndexColor = Theme.secondary
        tableView.sectionIndexBackgroundColor = .clear
        tableView.addGestureRecognizer(UILongPressGestureRecognizer(target: self, action: #selector(held(_:))))
        reload()
    }

    /// A long press on a list row: the song menu, or the album's, artist's or playlist's.
    @objc private func held(_ g: UILongPressGestureRecognizer) {
        guard g.state == .began, let path = tableView.indexPathForRow(at: g.location(in: tableView)),
              rows[path.section].layout == .list else { return }
        let item = rows[path.section].section.items[path.row]
        guard item.kind == "song" else { return menu(for: item) }
        SongMenu.show(item, token: token, index: item.index, from: self) { [weak self] page in
            self?.navigationController?.pushViewController(page, animated: true)
        }
    }

    @objc private func sortTapped() {
        guard let raw = nori_ios_sorts(kind) else { return }
        let text = String(cString: raw)
        nori_ios_free(raw)
        guard let d = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any],
              let all = d["all"] as? [String] else { return }
        let now = d["now"] as? String
        let sheet = UIAlertController.sheet(Say.sortBy)
        for name in all {
            sheet.add(Say.sort(name), checked: name == now) { [weak self] in
                guard let self, name != now else { return }
                name.withCString { nori_ios_sort(self.kind, $0) }
                self.tableView.setContentOffset(CGPoint(x: 0, y: -self.tableView.adjustedContentInset.top), animated: false)
                self.reload()
            }
        }
        sheet.show(from: self)
    }

    deinit {
        if token != 0 { Core.shared.forget(token) }
    }

    /// An answer came while the list was pulled down to refresh.
    private var releaseEndsRefresh = false

    /// Ends the pull-to-refresh, but only once the finger has let go: ended mid-drag (the stored copy
    /// answers at once), UIKit leaves the list held down by the control's height.
    private func finishRefresh() {
        guard let control = refreshControl, control.isRefreshing else { return }
        if tableView.isDragging {
            releaseEndsRefresh = true
        } else {
            control.endRefreshing()
        }
    }

    override func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
        guard releaseEndsRefresh else { return }
        releaseEndsRefresh = false
        refreshControl?.endRefreshing()
    }

    @objc func reload() {
        guard Core.shared.isOpen else {
            finishRefresh()
            showEmpty(Say.noServer, Say.noServerDetail)
            return
        }
        if token != 0 { Core.shared.forget(token) }
        loadingMore = false
        wantedLetter = nil
        if navigationItem.rightBarButtonItem == nil, let raw = nori_ios_sorts(kind) {
            nori_ios_free(raw)
            let sort = UIBarButtonItem(image: Glyph.sort, style: .plain, target: self, action: #selector(sortTapped))
            sort.accessibilityLabel = Say.sort
            navigationItem.rightBarButtonItem = sort
        }
        token = Core.shared.read(kind, arg) { [weak self] a in self?.took(a, appending: false) }
    }

    func took(_ a: PageAnswer, appending: Bool) {
        finishRefresh()
        loadingMore = false
        if a.raw["closed"] != nil {
            showEmpty(Say.noServer, Say.noServerDetail)
            return
        }
        if let code = a.error {
            if rows.isEmpty { showEmpty(Say.couldNotLoad, Say.failure(code, a.detail)) }
            return
        }
        more = a.raw["more"] as? Bool ?? false
        nextPage = a.raw["next"] as? Int ?? 0
        letters = a.raw["letters"] as? Bool ?? false
        if appending, let extra = a.sections.first, let last = rows.indices.last {
            // A page can answer twice (cached, then the server's): it replaces what it sent before.
            let had = rows[last].section
            let from = min(a.raw["from"] as? Int ?? had.items.count, had.items.count)
            rows[last] = (Section(key: had.key, grid: had.grid, items: Array(had.items.prefix(from)) + extra.items), rows[last].layout)
        } else {
            answer = a
            rows = a.sections.filter { !$0.items.isEmpty }.map { ($0, layout(for: $0)) }
            if !a.head.isEmpty && tableView.tableHeaderView == nil {
                let header = PageHeader(head: a.head)
                header.play = { [weak self] in self?.heroPressed(shuffle: false) }
                header.shuffle = { [weak self] in self?.heroPressed(shuffle: true) }
                header.more = { [weak self] in self?.collectionMenu(external: a.head["x"] as? Bool ?? false) }
                header.artist = { [weak self] id in
                    let name = a.head["s"] as? String ?? ""
                    self?.navigationController?.pushViewController(PageController(kind: NORI_PAGE_ARTIST, arg: id, title: name), animated: true)
                }
                tableView.tableHeaderView = header
                header.paint(hero: heroBits())
            }
        }
        if rows.isEmpty && a.head.isEmpty {
            showEmpty(Say.nothingHere, "")
        } else {
            empty.isHidden = true
        }
        paintedCurrent = Core.shared.now.song?.id
        tableView.reloadData()
        if let wanted = wantedLetter {
            wantedLetter = nil
            jump(to: wanted)
        }
    }

    override func sectionIndexTitles(for tableView: UITableView) -> [String]? {
        letters && rows.count == 1 ? PageController.index : nil
    }

    override func tableView(_ tableView: UITableView, sectionForSectionIndexTitle title: String, at index: Int) -> Int {
        // The table first scrolls to the section's top; the row is found after it.
        DispatchQueue.main.async { self.jump(to: title) }
        return 0
    }

    /// Scrolls to the first row at or past `letter`, loading pages while it is not there yet.
    private func jump(to letter: String) {
        guard let r = rows.first else { return }
        let rank = { (l: String) in PageController.index.firstIndex(of: l) ?? 0 }
        let target = rank(letter)
        guard let at = r.section.items.firstIndex(where: { rank($0.letter) >= target }) else {
            if more {
                wantedLetter = letter
                loadMore()
            } else if let last = r.section.items.indices.last {
                scroll(toItem: last)
            }
            return
        }
        scroll(toItem: at)
    }

    private func scroll(toItem i: Int) {
        let row = rows[0].layout == .grid ? i / 2 : i
        tableView.scrollToRow(at: IndexPath(row: row, section: 0), at: .top, animated: false)
    }

    private func loadMore() {
        guard more, !loadingMore else { return }
        loadingMore = true
        let page = kind == NORI_PAGE_GENRE ? "\(arg)\n\(nextPage)" : "\(nextPage)"
        Core.shared.read(kind, page, token: token) { [weak self] a in self?.took(a, appending: true) }
    }

    private func layout(for s: Section) -> Layout {
        guard s.grid else { return .list }
        return (kind == NORI_PAGE_ALBUMS || kind == NORI_PAGE_GENRE) ? .grid : .shelf
    }

    private func showEmpty(_ title: String, _ detail: String) {
        rows = []
        tableView.reloadData()
        empty.set(title: title, detail: detail)
        empty.isHidden = false
    }

    /// The header's play and shuffle.
    /// The header's ⋯: the whole page to the queue, or downloaded.
    /// The header's ⋯: the page to the queue, and its download entries as the core lays them out (all,
    /// the rest of a partly downloaded page, removal).
    private func collectionMenu(external: Bool) {
        let (kind, arg, token) = (self.kind, self.arg, self.token)
        let whole = kind == NORI_PAGE_ARTIST
        DispatchQueue.global(qos: .userInitiated).async {
            let raw = external ? nil : whole ? arg.withCString { nori_ios_collection_download_entries(kind, $0) } : nori_ios_download_entries(token)
            let entries = takenJSON(raw) as? [[String: Int]] ?? []
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                let sheet = UIAlertController.sheet(self.title)
                sheet.add(Say.addToQueue) {
                    if whole { arg.withCString { nori_ios_enqueue_collection(kind, $0, 0) } } else { nori_ios_enqueue_list(token, -1, 0) }
                }
                PageController.add(entries, to: sheet) { act in
                    whole ? arg.withCString { nori_ios_collection_download_act(kind, $0, act) } : nori_ios_download_act(token, act)
                }
                sheet.show(from: self)
            }
        }
    }

    /// Download entries (`nori_ios_download_entries`) as lines of `sheet`; `run` does one off the main
    /// thread and answers the songs it touched. A removal says how many and has pages read again.
    static func add(_ entries: [[String: Int]], to sheet: UIAlertController, run: @escaping (Int32) -> Int32) {
        // Two entries: partly downloaded, so removal names how many it gives back.
        let partly = entries.count > 1
        for e in entries {
            guard let act = e["act"], let n = e["n"] else { continue }
            let label = act == 0 ? Say.download : act == 1 ? Say.downloadOther(n) : (partly ? Say.removeDownloaded(n) : Say.removeDownloads)
            sheet.add(label, destructive: act == 2) {
                DispatchQueue.global(qos: .userInitiated).async {
                    let done = run(Int32(act))
                    guard act == 2 else { return }
                    DispatchQueue.main.async {
                        Toast.show(Say.downloadsRemoved(Int(done)))
                        NotificationCenter.default.post(name: .noriDownloads, object: nil)
                    }
                }
            }
        }
    }

    /// The core's word on this page's Play and Shuffle against the queue now (`nori_ios_hero`).
    private func heroBits() -> Int32 {
        let now = Core.shared.now
        return arg.withCString { nori_ios_hero(kind, $0, now.playing ? 1 : 0, now.buffering ? 1 : 0) }
    }

    /// Play or Shuffle pressed: start the page's songs, or (its queue playing) pause and resume it or turn
    /// its shuffle off, as the core says.
    private func heroPressed(shuffle: Bool) {
        let press = (heroBits() >> (shuffle ? 4 : 6)) & 3
        switch press {
        case 1: nori_ios_toggle()
        case 2: nori_ios_shuffle(0)
        default: play(shuffle: shuffle)
        }
    }

    /// The song the rows were last painted with as playing.
    private var paintedCurrent: String?

    @objc private func nowChanged() {
        (tableView.tableHeaderView as? PageHeader)?.paint(hero: heroBits())
        let id = Core.shared.now.song?.id
        guard id != paintedCurrent else { return }
        paintedCurrent = id
        paintVisibleRows()
    }

    /// The visible song rows painted again in place (their playing mark and heart); a reload would close
    /// a row swiped open.
    @objc private func paintVisibleRows() {
        for path in tableView.indexPathsForVisibleRows ?? [] {
            guard rows.indices.contains(path.section), rows[path.section].layout == .list,
                  let cell = tableView.cellForRow(at: path) as? ItemCell else { continue }
            let item = rows[path.section].section.items[path.row]
            cell.show(item, numbered: kind == NORI_PAGE_ALBUM, current: isCurrent(item))
        }
    }

    func play(shuffle: Bool) {
        if kind == NORI_PAGE_ARTIST {
            arg.withCString { nori_ios_play_collection(kind, $0, shuffle ? 1 : 0) }
        } else {
            nori_ios_play_list(token, 0, shuffle ? 1 : 0)
        }
    }

    override func numberOfSections(in tableView: UITableView) -> Int { rows.count }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        let r = rows[section]
        switch r.layout {
        case .list: return r.section.items.count
        case .shelf: return 1
        case .grid: return (r.section.items.count + 1) / 2
        }
    }

    private func heading(_ section: Int) -> String? {
        let key = rows[section].section.key
        if rows.count == 1 && answer?.head.isEmpty == false { return nil }
        if rows.count == 1 && kind != NORI_PAGE_HOME && kind != NORI_PAGE_DOWNLOADS { return nil }
        let words = Say.shelf(key)
        return words.isEmpty ? nil : words
    }

    override func tableView(_ tableView: UITableView, viewForHeaderInSection section: Int) -> UIView? {
        guard let words = heading(section) else { return nil }
        let header = tableView.dequeueReusableHeaderFooterView(withIdentifier: SectionHeading.id) as? SectionHeading
            ?? SectionHeading(reuseIdentifier: SectionHeading.id)
        header.title.text = words
        return header
    }

    override func tableView(_ tableView: UITableView, heightForHeaderInSection section: Int) -> CGFloat {
        heading(section) == nil ? 0 : SectionHeading.height
    }

    override func tableView(_ tableView: UITableView, heightForRowAt indexPath: IndexPath) -> CGFloat {
        switch rows[indexPath.section].layout {
        case .list: return UITableView.automaticDimension
        case .shelf: return ShelfCell.height(rows[indexPath.section].section.items)
        case .grid: return PairCell.width + 60
        }
    }

    func isCurrent(_ item: Item) -> Bool {
        guard let song = Core.shared.now.song else { return false }
        return item.kind == "song" && item.id == song.id
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let r = rows[indexPath.section]
        switch r.layout {
        case .shelf:
            let cell = tableView.dequeueReusableCell(withIdentifier: ShelfCell.id, for: indexPath) as! ShelfCell
            cell.show(r.section.items)
            cell.picked = { [weak self] in self?.open($0) }
            cell.held = { [weak self] in self?.menu(for: $0) }
            return cell
        case .grid:
            let cell = tableView.dequeueReusableCell(withIdentifier: PairCell.id, for: indexPath) as! PairCell
            let i = indexPath.row * 2
            cell.show(r.section.items[i], i + 1 < r.section.items.count ? r.section.items[i + 1] : nil)
            cell.picked = { [weak self] in self?.open($0) }
            cell.held = { [weak self] in self?.menu(for: $0) }
            return cell
        case .list:
            let cell = tableView.dequeueReusableCell(withIdentifier: ItemCell.id, for: indexPath) as! ItemCell
            let item = r.section.items[indexPath.row]
            cell.show(item, numbered: kind == NORI_PAGE_ALBUM, current: isCurrent(item))
            return cell
        }
    }

    override func tableView(_ tableView: UITableView, willDisplay cell: UITableViewCell, forRowAt indexPath: IndexPath) {
        guard more, indexPath.section == rows.count - 1,
              indexPath.row >= tableView.numberOfRows(inSection: indexPath.section) - 10 else { return }
        loadMore()
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        let item = rows[indexPath.section].section.items[indexPath.row]
        open(item)
    }

    func open(_ item: Item) {
        switch item.kind {
        case "song":
            if nori_ios_tap_song(token, Int32(item.index)) == 1 {
                (view.window?.rootViewController as? ShellController)?.openPlayer()
            }
        case "album":
            navigationController?.pushViewController(PageController(kind: NORI_PAGE_ALBUM, arg: item.id, title: item.title), animated: true)
        case "artist":
            navigationController?.pushViewController(PageController(kind: NORI_PAGE_ARTIST, arg: item.id, title: item.title), animated: true)
        case "playlist":
            navigationController?.pushViewController(PageController(kind: NORI_PAGE_PLAYLIST, arg: item.id, title: item.title), animated: true)
        case "genre":
            navigationController?.pushViewController(PageController(kind: NORI_PAGE_GENRE, arg: item.id, title: item.title), animated: true)
        case "mix":
            navigationController?.pushViewController(PageController(kind: NORI_PAGE_MIX, arg: item.id, title: Say.mix(item.count)), animated: true)
        case "smart":
            let name = item.title.isEmpty ? Say.smart(item.count) : item.title
            navigationController?.pushViewController(PageController(kind: NORI_PAGE_SMART, arg: item.id, title: name), animated: true)
        default:
            break
        }
    }

    override func tableView(_ tableView: UITableView, trailingSwipeActionsConfigurationForRowAt indexPath: IndexPath) -> UISwipeActionsConfiguration? {
        swipe(left: true, at: indexPath)
    }

    override func tableView(_ tableView: UITableView, leadingSwipeActionsConfigurationForRowAt indexPath: IndexPath) -> UISwipeActionsConfiguration? {
        swipe(left: false, at: indexPath)
    }

    /// The one action a sideways swipe on a song row offers: the swipeLeft or swipeRight setting's, or on a
    /// playlist's own page Remove to the left. Never nil, which would give UIKit's own Delete.
    private func swipe(left: Bool, at indexPath: IndexPath) -> UISwipeActionsConfiguration {
        let none = UISwipeActionsConfiguration(actions: [])
        let r = rows[indexPath.section]
        guard r.layout == .list else { return none }
        let item = r.section.items[indexPath.row]
        guard item.kind == "song" else { return none }
        let index = Int32(item.index)
        let action: UIContextualAction
        if left && kind == NORI_PAGE_PLAYLIST {
            let playlist = arg
            action = UIContextualAction(style: .destructive, title: Say.removeFromPlaylist) { [weak self] _, _, done in
                DispatchQueue.global(qos: .userInitiated).async {
                    let ok = playlist.withCString { nori_ios_playlist_remove($0, index) } == 1
                    DispatchQueue.main.async {
                        done(ok)
                        if ok { self?.reload() } else { Toast.show(Say.failed) }
                    }
                }
            }
        } else {
            let code = nori_ios_row_swipe(left ? 1 : 0, Core.shared.isFavorite(item) ? 1 : 0)
            let starring = code == NORI_SWIPE_FAVORITE || code == NORI_SWIPE_UNFAVORITE
            guard code >= 0, !(item.external && (starring || code == NORI_SWIPE_DOWNLOAD)) else { return none }
            let title: String
            switch code {
            case NORI_SWIPE_QUEUE: title = Say.addToQueue
            case NORI_SWIPE_PLAY_NEXT: title = Say.playNext
            case NORI_SWIPE_FAVORITE: title = Say.favorite
            case NORI_SWIPE_UNFAVORITE: title = Say.unfavorite
            default: title = Say.download
            }
            action = UIContextualAction(style: .normal, title: title) { [weak self] _, _, done in
                guard let self else { return done(false) }
                switch code {
                case NORI_SWIPE_QUEUE, NORI_SWIPE_PLAY_NEXT:
                    nori_ios_enqueue_list(self.token, index, code == NORI_SWIPE_PLAY_NEXT ? 1 : 0)
                case NORI_SWIPE_FAVORITE, NORI_SWIPE_UNFAVORITE:
                    Core.shared.favorite(item, code == NORI_SWIPE_FAVORITE)
                    self.tableView.reloadRows(at: [indexPath], with: .none)
                default:
                    nori_ios_download_list(self.token, index)
                }
                done(true)
            }
            action.backgroundColor = Theme.track
        }
        return UISwipeActionsConfiguration(actions: [action])
    }

    /// A long press on a card: what to do with the whole album, artist or playlist.
    func menu(for item: Item) {
        let kind: Int32
        switch item.kind {
        case "album": kind = NORI_PAGE_ALBUM
        case "artist": kind = NORI_PAGE_ARTIST
        case "playlist": kind = NORI_PAGE_PLAYLIST
        default: return
        }
        // The download entries need the songs (stored, else the server's): read off the main thread.
        DispatchQueue.global(qos: .userInitiated).async {
            let raw = item.external ? nil : item.id.withCString { nori_ios_collection_download_entries(kind, $0) }
            let entries = takenJSON(raw) as? [[String: Int]] ?? []
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                let sheet = UIAlertController.sheet(item.title, item.subtitle.isEmpty ? nil : item.subtitle)
                sheet.add(Say.play) { item.id.withCString { nori_ios_play_collection(kind, $0, 0) } }
                sheet.add(Say.shuffle) { item.id.withCString { nori_ios_play_collection(kind, $0, 1) } }
                sheet.add(Say.playNext) { item.id.withCString { nori_ios_enqueue_collection(kind, $0, 1) } }
                sheet.add(Say.addToQueue) { item.id.withCString { nori_ios_enqueue_collection(kind, $0, 0) } }
                PageController.add(entries, to: sheet) { act in
                    item.id.withCString { nori_ios_collection_download_act(kind, $0, act) }
                }
                if kind != NORI_PAGE_PLAYLIST && !item.external {
                    let on = Core.shared.isFavorite(item)
                    sheet.add(on ? Say.removeFromFavorites : Say.addToFavorites) {
                        Core.shared.favorite(item, !on)
                    }
                }
                if kind == NORI_PAGE_PLAYLIST {
                    sheet.add(Say.deleteNamed(item.title), destructive: true) { [weak self] in self?.delete(playlist: item) }
                }
                sheet.show(from: self)
            }
        }
    }
}

extension PageController {
    /// Asks, then deletes `playlist` on the server and reads this page again.
    fileprivate func delete(playlist: Item) {
        let ask = UIAlertController.sheet(Say.deleteNamed(playlist.title) + "?")
        ask.add(Say.deleteNamed(playlist.title), destructive: true) { [weak self] in
            DispatchQueue.global(qos: .userInitiated).async {
                let ok = playlist.id.withCString { nori_ios_playlist_delete($0) } == 1
                DispatchQueue.main.async {
                    if ok { self?.reload() } else { Toast.show(Say.failed) }
                }
            }
        }
        ask.show(from: self)
    }
}
