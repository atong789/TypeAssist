import Foundation

let bridge = Bridge()

guard Accessibility.isTrusted() else {
    bridge.emit(.permissionRequired)
    // Stay alive briefly so the parent process can read the event,
    // then exit non-zero so the parent can decide to relaunch after the user grants.
    Thread.sleep(forTimeInterval: 0.25)
    exit(2)
}

let tap = EventTap(bridge: bridge)
guard tap.start() else {
    FileHandle.standardError.write(Data("failed to install CGEventTap\n".utf8))
    exit(3)
}

bridge.emit(.ready)
bridge.runInputLoop(commandHandler: tap.handleCommand(_:))

RunLoop.current.run()
