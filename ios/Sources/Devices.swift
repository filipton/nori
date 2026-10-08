import MediaPlayer
import UIKit

/// Remote control's mDNS through the system's Bonjour: this iPod's door announced while remote control
/// is on, the account's other doors looked for while asked. The library asks from its own
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

/// The system volume, set when another device asks: through a volume view's slider, the one way iOS 12
/// lets an app move it. The view is in the window only for the move.
enum SystemVolume {
    static func set(_ fraction: Float) {
        guard let window = UIApplication.shared.keyWindow else { return }
        let view = MPVolumeView(frame: CGRect(x: -200, y: -200, width: 100, height: 40))
        window.addSubview(view)
        guard let slider = view.subviews.lazy.compactMap({ $0 as? UISlider }).first else {
            return view.removeFromSuperview()
        }
        slider.setValue(min(1, max(0, fraction)), animated: false)
        slider.sendActions(for: .valueChanged)
        // The slider hands the value to the system on its own turn; the view goes after it.
        DispatchQueue.main.async { view.removeFromSuperview() }
    }
}
