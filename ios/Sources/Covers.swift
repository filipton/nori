import UIKit

/// Decoded covers by id and pixel size; the system empties it under memory pressure.
enum CoverCache {
    static let images: NSCache<NSString, UIImage> = {
        let c = NSCache<NSString, UIImage>()
        c.totalCostLimit = 16 * 1024 * 1024
        return c
    }()

    static func key(_ id: String, _ px: Int) -> NSString { "\(id)@\(px)" as NSString }

    /// The pixel sizes decoded so far, largest first: the app draws covers at a handful of them.
    private static var sizes: [Int] = []

    static func put(_ image: UIImage, _ id: String, _ px: Int) {
        let cost = Int(image.size.width * image.scale * image.size.height * image.scale * 4)
        images.setObject(image, forKey: key(id, px), cost: cost)
        if !sizes.contains(px) {
            sizes.append(px)
            sizes.sort(by: >)
        }
    }

    /// The sharpest picture of `id` decoded at any size, to show while the size asked for comes.
    static func any(_ id: String) -> UIImage? {
        for px in sizes {
            if let image = images.object(forKey: key(id, px)) { return image }
        }
        return nil
    }

    static func trim() {
        images.removeAllObjects()
    }
}

/// A square cover on a plate, the disc mark until the picture comes. A picture of it already decoded at
/// another size stands in at once and the sharp one replaces it; with none, a sheen crosses the plate
/// and the picture fades in over 260 ms. Both are layer animations the render server runs, and a picture
/// cached at its own size shows at once with neither.
final class CoverView: UIView {
    private let picture = UIImageView()
    private let mark = UIImageView(image: Glyph.disc)
    private let sheen = CAGradientLayer()
    private var token: UInt64?
    private var shown: String?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = Theme.track
        layer.cornerRadius = 6
        clipsToBounds = true
        mark.tintColor = Theme.secondary
        mark.contentMode = .center
        picture.contentMode = .scaleAspectFill
        sheen.startPoint = CGPoint(x: 0, y: 0.5)
        sheen.endPoint = CGPoint(x: 1, y: 0.5)
        let light = Theme.label.withAlphaComponent(0.08).cgColor, clear = Theme.label.withAlphaComponent(0).cgColor
        sheen.colors = [clear, light, clear]
        sheen.locations = [0, 0.15, 0.3]
        sheen.isHidden = true
        layer.addSublayer(sheen)
        for v in [mark, picture] {
            // The plate's size is set from outside: the picture or the mark arriving never moves it.
            for axis in [NSLayoutConstraint.Axis.horizontal, .vertical] {
                v.setContentHuggingPriority(UILayoutPriority(1), for: axis)
                v.setContentCompressionResistancePriority(UILayoutPriority(1), for: axis)
            }
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
            NSLayoutConstraint.activate([
                v.leadingAnchor.constraint(equalTo: leadingAnchor),
                v.trailingAnchor.constraint(equalTo: trailingAnchor),
                v.topAnchor.constraint(equalTo: topAnchor),
                v.bottomAnchor.constraint(equalTo: bottomAnchor),
            ])
        }
    }

    required init?(coder: NSCoder) { fatalError() }

    override func layoutSubviews() {
        super.layoutSubviews()
        sheen.frame = bounds
    }

    private func waiting(_ on: Bool) {
        sheen.removeAllAnimations()
        sheen.isHidden = !on
        guard on, !UIAccessibility.isReduceMotionEnabled else { return }
        let sweep = CABasicAnimation(keyPath: "locations")
        sweep.fromValue = [-0.3, -0.15, 0]
        sweep.toValue = [1, 1.15, 1.3]
        sweep.duration = 1.2
        // A cover that never comes (failed, or none) does not keep the plate moving.
        sweep.repeatCount = 3
        sheen.add(sweep, forKey: "sweep")
    }

    /// Shows cover `id` at `points` wide; a cell reused for another row lets go of the old request.
    func show(_ id: String, points: CGFloat) {
        let px = Int(points * UIScreen.main.scale)
        let key = "\(id)@\(px)"
        if shown == key { return }
        cancel()
        shown = key
        picture.image = nil
        mark.isHidden = false
        waiting(false)
        guard !id.isEmpty else { return }
        if let cached = CoverCache.images.object(forKey: key as NSString) {
            picture.image = cached
            mark.isHidden = true
            return
        }
        let standIn = CoverCache.any(id)
        if let standIn {
            picture.image = standIn
            mark.isHidden = true
        } else {
            waiting(true)
        }
        token = Core.shared.cover(id, px: px) { [weak self] image in
            CoverCache.put(image, id, px)
            guard let self, self.shown == key else { return }
            self.token = nil
            self.waiting(false)
            self.mark.isHidden = true
            // Sharpening a stand-in is a short cross-fade; a first picture fades in.
            let fade = UIAccessibility.isReduceMotionEnabled ? 0 : standIn == nil ? 0.26 : 0.15
            UIView.transition(with: self.picture, duration: fade, options: .transitionCrossDissolve, animations: { self.picture.image = image })
        }
    }

    func cancel() {
        if let t = token {
            Core.shared.cancelCover(t)
            token = nil
        }
        waiting(false)
        shown = nil
    }
}
