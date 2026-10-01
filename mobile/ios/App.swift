import UIKit
import Security

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Bootty", sessionRole: session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession,
               options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        window.overrideUserInterfaceStyle = .dark
        window.rootViewController = UINavigationController(rootViewController: WorkspaceController())
        window.makeKeyAndVisible()
        self.window = window
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        gpui_ios_did_become_active(nil)
        bootty_mobile_active(true)
    }

    func sceneWillResignActive(_ scene: UIScene) {
        bootty_mobile_active(false)
        gpui_ios_will_resign_active(nil)
    }
}

/// One embedded GPUI view for the process lifetime. UIKit owns native layout.
final class TerminalSurface: UIView {
    private let gpuiWindow: UnsafeMutableRawPointer
    let contentController: UIViewController
    private var displayLink: CADisplayLink?
    private var inputObservers: [NSObjectProtocol] = []

    override init(frame: CGRect) {
        gpui_ios_set_embedded()
        bootty_mobile_register()
        gpui_ios_run_demo()
        guard let window = gpui_ios_get_window(),
              let controller = gpui_ios_view_controller(window) else {
            fatalError("Couldn’t create the Bootty terminal view")
        }
        gpuiWindow = window
        contentController = Unmanaged<UIViewController>.fromOpaque(controller).takeUnretainedValue()
        super.init(frame: frame)
        clipsToBounds = true
        gpui_ios_set_frame_waker(window, { context in
            guard let context else { return }
            Unmanaged<TerminalSurface>.fromOpaque(context).takeUnretainedValue().wake()
        }, Unmanaged.passUnretained(self).toOpaque())
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is unsupported") }

    deinit {
        gpui_ios_set_frame_waker(gpuiWindow, nil, nil)
        for observer in inputObservers { NotificationCenter.default.removeObserver(observer) }
    }

    func attach(to parent: UIViewController) {
        parent.addChild(contentController)
        addSubview(contentController.view)
        contentController.didMove(toParent: parent)
        // The embedded GPUI platform uses a native composition buffer. Terminal
        // commands must preserve literal quotes/dashes until its traits API exposes them.
        for input in contentController.view.subviews.compactMap({ $0 as? UITextView }) {
            input.smartQuotesType = .no
            input.smartDashesType = .no
            input.smartInsertDeleteType = .no
            // UIKit paste bypasses the upstream insertText override. Commit its
            // buffer after native editing finishes; leave marked IME text intact.
            inputObservers.append(NotificationCenter.default.addObserver(
                forName: UITextView.textDidChangeNotification, object: input, queue: .main
            ) { [weak input] _ in
                DispatchQueue.main.async { [weak input] in
                    guard let input, input.markedTextRange == nil, !input.text.isEmpty else { return }
                    input.insertText("")
                }
            })
        }
    }

    func startFrames() {
        guard displayLink == nil else { return }
        displayLink = CADisplayLink(target: self, selector: #selector(drawFrame))
        displayLink?.add(to: .main, forMode: .common)
    }

    func stopFrames() {
        displayLink?.invalidate()
        displayLink = nil
    }

    func wake() { displayLink?.isPaused = false }

    @objc private func drawFrame() {
        displayLink?.isPaused = !gpui_ios_request_frame(gpuiWindow)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds.width > 0, bounds.height > 0,
              contentController.view.frame != bounds else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        contentController.view.frame = bounds
        gpui_ios_layout_view(gpuiWindow)
        wake()
        _ = gpui_ios_request_frame(gpuiWindow)
        CATransaction.commit()
    }
}

final class WorkspaceController: UIViewController {
    private var surface: TerminalSurface!
    private let status = UILabel()

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Bootty"
        view.backgroundColor = UIColor(red: 0.075, green: 0.082, blue: 0.094, alpha: 1)
        let appearance = UINavigationBarAppearance()
        appearance.configureWithOpaqueBackground()
        appearance.backgroundColor = view.backgroundColor
        appearance.shadowColor = UIColor(red: 0.216, green: 0.231, blue: 0.263, alpha: 1)
        appearance.titleTextAttributes = [.foregroundColor: UIColor(white: 0.94, alpha: 1),
                                         .font: UIFont.systemFont(ofSize: 17, weight: .semibold)]
        navigationController?.navigationBar.standardAppearance = appearance
        navigationController?.navigationBar.scrollEdgeAppearance = appearance
        navigationController?.navigationBar.compactAppearance = appearance
        navigationItem.rightBarButtonItem = navigationButton("Connect", symbol: "link",
                                                             action: #selector(showConnectionDialog))
        navigationItem.leftBarButtonItem = navigationButton("Disconnect", symbol: "personalhotspot.slash",
                                                            action: #selector(disconnect))
        status.text = nil
        status.font = .preferredFont(forTextStyle: .footnote)
        status.adjustsFontForContentSizeCategory = true
        status.textColor = .secondaryLabel
        status.numberOfLines = 0
        status.translatesAutoresizingMaskIntoConstraints = false
        surface = TerminalSurface(frame: .zero)
        surface.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(status)
        view.addSubview(surface)
        surface.attach(to: self)
        NSLayoutConstraint.activate([
            status.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            status.leadingAnchor.constraint(equalTo: view.layoutMarginsGuide.leadingAnchor),
            status.trailingAnchor.constraint(equalTo: view.layoutMarginsGuide.trailingAnchor),
            surface.topAnchor.constraint(equalTo: status.bottomAnchor),
            surface.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor),
            surface.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor),
            surface.bottomAnchor.constraint(equalTo: view.keyboardLayoutGuide.topAnchor),
        ])
        if let code = savedPairingCode() { connect(code) }
    }

    private func navigationButton(_ label: String, symbol: String, action: Selector) -> UIBarButtonItem {
        let button = UIButton(type: .system)
        button.setImage(UIImage(systemName: symbol), for: .normal)
        button.tintColor = UIColor(red: 0.706, green: 0.631, blue: 0.910, alpha: 1)
        button.accessibilityLabel = label
        button.addTarget(self, action: action, for: .touchUpInside)
        button.widthAnchor.constraint(equalToConstant: 44).isActive = true
        button.heightAnchor.constraint(equalToConstant: 44).isActive = true
        let item = UIBarButtonItem(customView: button)
        if #available(iOS 26.0, *) { item.hidesSharedBackground = true }
        return item
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        surface.startFrames()
        bootty_mobile_active(true)
        updateAppearance()
    }

    override func traitCollectionDidChange(_ previous: UITraitCollection?) {
        super.traitCollectionDidChange(previous)
        if surface != nil { updateAppearance() }
    }

    private func updateAppearance() {
        bootty_mobile_appearance(Float(UIFont.preferredFont(forTextStyle: .body).pointSize))
        surface.wake()
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        bootty_mobile_active(false)
        surface.stopFrames()
    }

    @objc private func showConnectionDialog() {
        let dialog = UIAlertController(title: "Connect to Bootty",
            message: "Enable remote control in Bootty’s Connections window, then paste its pairing code. This grants control of that computer’s sessions.", preferredStyle: .alert)
        dialog.addTextField { field in
            field.placeholder = "Bootty pairing code"
            field.isSecureTextEntry = true
            field.autocorrectionType = .no
            field.autocapitalizationType = .none
            field.accessibilityLabel = "Bootty pairing code"
        }
        dialog.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        dialog.addAction(UIAlertAction(title: "Connect", style: .default) { [weak self, weak dialog] _ in
            guard let code = dialog?.textFields?.first?.text, !code.isEmpty else { return }
            self?.connect(code)
        })
        present(dialog, animated: true)
    }

    private func connect(_ code: String) {
        guard code.utf8.count <= 8192 else { status.text = "Pairing code is too large"; return }
        do {
            let credential = Data(code.utf8)
            SecItemDelete(pairingQuery as CFDictionary)
            var item = pairingQuery
            item[kSecValueData as String] = credential as NSData
            item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly as NSString
            let saved = SecItemAdd(item as CFDictionary, nil)
            guard saved == errSecSuccess else { throw PairingError.keychain(saved) }
            // A bounded, protected handoff keeps the Rust export pointer-free.
            let destination = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("bootty-pairing.txt")
            try credential.write(to: destination, options: [.atomic, .completeFileProtection])
            bootty_mobile_reload()
            status.text = nil
            surface.wake()
        } catch {
            status.text = "Couldn’t connect: \(error.localizedDescription)"
        }
    }

    @objc private func disconnect() {
        SecItemDelete(pairingQuery as CFDictionary)
        let handoff = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("bootty-pairing.txt")
        try? FileManager.default.removeItem(at: handoff)
        bootty_mobile_disconnect()
        surface.wake()
    }
}

private let pairingQuery: [String: NSObject] = [
    kSecClass as String: kSecClassGenericPassword as NSString,
    kSecAttrService as String: "dev.bootty.mobile.connection" as NSString,
    kSecAttrAccount as String: "paired-desktop" as NSString,
]

private func savedPairingCode() -> String? {
    var query = pairingQuery
    query[kSecReturnData as String] = true as NSNumber
    query[kSecMatchLimit as String] = kSecMatchLimitOne as NSString
    var result: CFTypeRef?
    guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
          let bytes = result as? Data else { return nil }
    return String(data: bytes, encoding: .utf8)
}

enum PairingError: LocalizedError {
    case keychain(OSStatus)
    var errorDescription: String? { "Couldn’t save the pairing credential in Keychain" }
}
