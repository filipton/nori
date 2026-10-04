import UIKit

/// Settings → Servers: the saved ones, the active one ticked. A tap switches to it at once; a swipe
/// forgets it; the last row adds one.
final class ServersPage: UITableViewController {
    private struct Saved {
        let id: String
        let label: String
        let url: String
        let active: Bool
    }

    private var servers: [Saved] = []
    private var switching = false

    override func viewDidLoad() {
        super.viewDidLoad()
        title = Say.servers
        navigationItem.largeTitleDisplayMode = .never
        dress(tableView)
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        load()
    }

    private func load() {
        DispatchQueue.global(qos: .userInitiated).async {
            let rows = dataDirectory().path.withCString { takenJSON(nori_ios_servers($0)) } as? [[String: Any]] ?? []
            let saved = rows.map {
                Saved(id: $0["id"] as? String ?? "", label: $0["label"] as? String ?? "",
                      url: $0["url"] as? String ?? "", active: $0["active"] as? Bool ?? false)
            }
            DispatchQueue.main.async {
                self.servers = saved
                self.tableView.reloadData()
            }
        }
    }

    override func numberOfSections(in tableView: UITableView) -> Int { 2 }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        section == 0 ? servers.count : 1
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = row(tableView, id: "server", style: .subtitle)
        if indexPath.section == 1 {
            cell.textLabel?.text = Say.addServer
            cell.textLabel?.textColor = Theme.accent
            cell.detailTextLabel?.text = nil
            cell.accessoryType = .disclosureIndicator
            return cell
        }
        let s = servers[indexPath.row]
        cell.textLabel?.text = s.label
        cell.detailTextLabel?.text = s.url
        cell.accessoryType = s.active ? .checkmark : .none
        cell.tintColor = Theme.accent
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        if indexPath.section == 1 {
            navigationController?.pushViewController(LoginPage(), animated: true)
            return
        }
        let s = servers[indexPath.row]
        guard !s.active, !switching else { return }
        switchTo { dataDirectory().path.withCString { d in s.id.withCString { _ = nori_ios_pick_server(d, $0) } } }
    }

    override func tableView(_ tableView: UITableView, canEditRowAt indexPath: IndexPath) -> Bool {
        indexPath.section == 0
    }

    override func tableView(_ tableView: UITableView, commit editingStyle: UITableViewCell.EditingStyle, forRowAt indexPath: IndexPath) {
        guard editingStyle == .delete, !switching else { return }
        let s = servers[indexPath.row]
        let sheet = UIAlertController.sheet(Say.forgetServer(s.label), Say.forgetServerNote)
        sheet.add(Say.forget, destructive: true) { [weak self] in
            let forget = { dataDirectory().path.withCString { d in s.id.withCString { nori_ios_remove_server(d, $0) } } }
            if s.active {
                self?.switchTo(forget)
            } else {
                DispatchQueue.global(qos: .userInitiated).async {
                    forget()
                    DispatchQueue.main.async { self?.load() }
                }
            }
        }
        sheet.show(from: self)
    }

    /// Runs `change` on the saved list, then reopens on whichever server is active after it.
    private func switchTo(_ change: @escaping () -> Void) {
        switching = true
        let spinner = UIActivityIndicatorView(style: Theme.light ? .gray : .white)
        spinner.startAnimating()
        navigationItem.rightBarButtonItem = UIBarButtonItem(customView: spinner)
        DispatchQueue.global(qos: .userInitiated).async {
            change()
            let left = (dataDirectory().path.withCString { takenJSON(nori_ios_servers($0)) } as? [Any])?.count ?? 0
            let failed = Core.reopen()
            DispatchQueue.main.async {
                self.switching = false
                self.navigationItem.rightBarButtonItem = nil
                Core.shared.opened()
                if let failed, left > 0 { Toast.show(failed) }
                self.load()
            }
        }
    }
}
