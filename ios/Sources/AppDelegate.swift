import UIKit

/// The app's data directory: `Library/Application Support/nori`, excluded from backups (the stream cache
/// and downloads live under it).
func dataDirectory() -> URL {
    let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    var dir = support.appendingPathComponent("nori", isDirectory: true)
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try? dir.setResourceValues(values)
    return dir
}

final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = ShellController()
        window.backgroundColor = Theme.background
        window.tintColor = Theme.accent
        window.makeKeyAndVisible()
        self.window = window
        Core.shared.start()
        NowPlaying.shared.start()
        let dir = dataDirectory().path
        nori_ios_keep_log(dir, Int32(TimeZone.current.secondsFromGMT() / 60))
        DispatchQueue.global(qos: .userInitiated).async {
            let failed = dir.withCString { d in taken(nori_ios_open(d, nil)) }
            guard failed == nil else { return }
            let light = SettingsPage.value("theme").map(Theme.isLight) ?? Theme.light
            DispatchQueue.main.async {
                if light != Theme.light { self.restyle(light: light) }
                Core.shared.opened()
            }
        }
        return true
    }

    /// Builds the interface again in `light` or dark, on the tab it was on.
    func restyle(light: Bool) {
        guard let window else { return }
        Theme.keep(light: light)
        let tab = (window.rootViewController as? UITabBarController)?.selectedIndex ?? 0
        let shell = ShellController()
        shell.loadViewIfNeeded()
        shell.selectedIndex = tab
        window.backgroundColor = Theme.background
        window.tintColor = Theme.accent
        UIView.transition(with: window, duration: 0.25, options: .transitionCrossDissolve, animations: {
            window.rootViewController = shell
        })
    }

    func applicationDidEnterBackground(_ application: UIApplication) {
        nori_ios_background()
    }

    func applicationWillEnterForeground(_ application: UIApplication) {
        Core.shared.refresh()
    }

    func applicationDidReceiveMemoryWarning(_ application: UIApplication) {
        CoverCache.trim()
        nori_ios_memory_warning()
    }
}
