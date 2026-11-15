import AppKit
import Combine
import SwiftUI

struct WindowVisibilityObserver: NSViewRepresentable {
    let onShow: @MainActor () -> Void

    func makeNSView(context: Context) -> VisibilityView {
        let view = VisibilityView()
        view.onShow = onShow
        return view
    }

    func updateNSView(_ view: VisibilityView, context: Context) {
        view.onShow = onShow
    }

    final class VisibilityView: NSView {
        var onShow: (@MainActor () -> Void)?
        private var subscription: AnyCancellable?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            subscription = nil
            guard let window else { return }
            // 系统保留窗口内容时，重新显示仍需更新状态。
            subscription = window.publisher(for: \.isVisible, options: [.initial, .new])
                .removeDuplicates()
                .filter { $0 }
                .sink { [weak self] _ in
                    MainActor.assumeIsolated { self?.onShow?() }
                }
        }
    }
}
