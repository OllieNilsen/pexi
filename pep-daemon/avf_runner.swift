import Foundation
import Virtualization
import Darwin

struct Args {
    var kernel: URL?
    var initrd: URL?
    var disk: URL
    var seed: URL?
    var cpus: Int
    var memoryBytes: UInt64
    var vsockPort: UInt32
    var bridgePort: UInt16
    var cmdline: String
    var consoleLog: URL?
    var statusLog: URL?
    var useEfi: Bool
    var efiVars: URL?
    var sharedDir: URL?
}

func parseArgs() -> Args {
    var kernel: URL?
    var initrd: URL?
    var disk: URL?
    var seed: URL?
    var cpus = 2
    var memoryBytes: UInt64 = 1024 * 1024 * 1024
    var vsockPort: UInt32 = 4040
    var bridgePort: UInt16 = 4041
    var cmdline = "console=hvc0 ip=none root=/dev/vda1 rw"
    var consoleLog: URL?
    var statusLog: URL?
    var useEfi = false
    var efiVars: URL?
    var sharedDir: URL?

    var it = CommandLine.arguments.dropFirst().makeIterator()
    while let arg = it.next() {
        switch arg {
        case "--kernel":
            kernel = URL(fileURLWithPath: it.next() ?? "")
        case "--initrd":
            initrd = URL(fileURLWithPath: it.next() ?? "")
        case "--disk":
            disk = URL(fileURLWithPath: it.next() ?? "")
        case "--seed":
            seed = URL(fileURLWithPath: it.next() ?? "")
        case "--cpus":
            cpus = Int(it.next() ?? "2") ?? 2
        case "--memory-bytes":
            memoryBytes = UInt64(it.next() ?? "") ?? memoryBytes
        case "--vsock-port":
            vsockPort = UInt32(it.next() ?? "") ?? vsockPort
        case "--bridge-port":
            bridgePort = UInt16(it.next() ?? "") ?? bridgePort
        case "--cmdline":
            cmdline = it.next() ?? cmdline
        case "--console-log":
            consoleLog = URL(fileURLWithPath: it.next() ?? "")
        case "--status-log":
            statusLog = URL(fileURLWithPath: it.next() ?? "")
        case "--efi":
            useEfi = true
        case "--efi-vars":
            efiVars = URL(fileURLWithPath: it.next() ?? "")
        case "--shared-dir":
            sharedDir = URL(fileURLWithPath: it.next() ?? "")
        default:
            continue
        }
    }

    guard let disk else {
        fatalError("Missing required --disk")
    }
    if !useEfi {
        guard kernel != nil, initrd != nil else {
            fatalError("Missing required --kernel or --initrd (unless --efi)")
        }
    }
    return Args(
        kernel: kernel,
        initrd: initrd,
        disk: disk,
        seed: seed,
        cpus: cpus,
        memoryBytes: memoryBytes,
        vsockPort: vsockPort,
        bridgePort: bridgePort,
        cmdline: cmdline,
        consoleLog: consoleLog,
        statusLog: statusLog,
        useEfi: useEfi,
        efiVars: efiVars,
        sharedDir: sharedDir
    )
}

let args = parseArgs()

if let statusLog = args.statusLog {
    FileManager.default.createFile(atPath: statusLog.path, contents: nil)
    if let handle = try? FileHandle(forWritingTo: statusLog) {
        try? handle.truncate(atOffset: 0)
        try? handle.close()
    }
}

func logStatus(_ message: String) {
    let line = "[\(Date())] \(message)\n"
    if let statusLog = args.statusLog {
        FileManager.default.createFile(atPath: statusLog.path, contents: nil)
        if let handle = try? FileHandle(forWritingTo: statusLog) {
            handle.seekToEndOfFile()
            if let data = line.data(using: .utf8) {
                try? handle.write(contentsOf: data)
            }
            try? handle.close()
        }
    } else {
        print(message)
    }
}

final class SocketBridge: NSObject, VZVirtioSocketListenerDelegate {
    private let bridgePort: UInt16
    private var connections: [VZVirtioSocketConnection] = []

    init(bridgePort: UInt16) {
        self.bridgePort = bridgePort
    }

    func listener(_ listener: VZVirtioSocketListener,
                  shouldAcceptNewConnection connection: VZVirtioSocketConnection,
                  from socketDevice: VZVirtioSocketDevice) -> Bool {
        connections.append(connection)
        bridge(connection: connection)
        return true
    }

    private func bridge(connection: VZVirtioSocketConnection) {
        let vsockFD = connection.fileDescriptor
        guard vsockFD >= 0, let tcpFD = connectTcp(port: bridgePort) else {
            connection.close()
            return
        }

        let vsockRead = FileHandle(fileDescriptor: vsockFD, closeOnDealloc: false)
        let vsockWrite = FileHandle(fileDescriptor: vsockFD, closeOnDealloc: false)
        let tcpRead = FileHandle(fileDescriptor: tcpFD, closeOnDealloc: true)
        let tcpWrite = FileHandle(fileDescriptor: tcpFD, closeOnDealloc: true)

        func closeAll() {
            vsockRead.readabilityHandler = nil
            tcpRead.readabilityHandler = nil
            connection.close()
            try? tcpRead.close()
        }

        vsockRead.readabilityHandler = { handle in
            let data = handle.availableData
            if data.isEmpty {
                closeAll()
                return
            }
            try? tcpWrite.write(contentsOf: data)
        }

        tcpRead.readabilityHandler = { handle in
            let data = handle.availableData
            if data.isEmpty {
                closeAll()
                return
            }
            try? vsockWrite.write(contentsOf: data)
        }
    }

    private func connectTcp(port: UInt16) -> Int32? {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        if fd < 0 {
            return nil
        }

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))

        let result = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        if result != 0 {
            close(fd)
            return nil
        }
        return fd
    }
}

let config = VZVirtualMachineConfiguration()
if args.useEfi {
    let platform = VZGenericPlatformConfiguration()
    config.platform = platform
    let efi = VZEFIBootLoader()
    let varsURL: URL = {
        if let efiVars = args.efiVars {
            return efiVars
        }
        let base = args.disk.deletingPathExtension().appendingPathExtension("efi-vars.fd")
        return base
    }()
    if FileManager.default.fileExists(atPath: varsURL.path) {
        efi.variableStore = VZEFIVariableStore(url: varsURL)
    } else {
        efi.variableStore = try VZEFIVariableStore(creatingVariableStoreAt: varsURL, options: [.allowOverwrite])
    }
    config.bootLoader = efi
    if #available(macOS 13.0, *) {
        let graphics = VZVirtioGraphicsDeviceConfiguration()
        let scanout = VZVirtioGraphicsScanoutConfiguration(widthInPixels: 1024, heightInPixels: 768)
        graphics.scanouts = [scanout]
        config.graphicsDevices = [graphics]
    }
} else {
    guard let kernel = args.kernel, let initrd = args.initrd else {
        fatalError("kernel/initrd required for Linux boot loader")
    }
    let bootLoader = VZLinuxBootLoader(kernelURL: kernel)
    bootLoader.initialRamdiskURL = initrd
    bootLoader.commandLine = args.cmdline
    config.bootLoader = bootLoader
}
config.cpuCount = args.cpus
config.memorySize = args.memoryBytes

var storageDevices: [VZStorageDeviceConfiguration] = []
let diskAttachment = try VZDiskImageStorageDeviceAttachment(url: args.disk, readOnly: false)
let diskDevice = VZVirtioBlockDeviceConfiguration(attachment: diskAttachment)
storageDevices.append(diskDevice)
if let seed = args.seed {
    let seedAttachment = try VZDiskImageStorageDeviceAttachment(url: seed, readOnly: true)
    let seedDevice = VZVirtioBlockDeviceConfiguration(attachment: seedAttachment)
    storageDevices.append(seedDevice)
}
config.storageDevices = storageDevices

let entropyDevice = VZVirtioEntropyDeviceConfiguration()
config.entropyDevices = [entropyDevice]

let console = VZVirtioConsoleDeviceSerialPortConfiguration()
let consoleAttachment: VZSerialPortAttachment
if let consoleLog = args.consoleLog {
    let attachment = try VZFileSerialPortAttachment(url: consoleLog, append: false)
    consoleAttachment = attachment
} else {
    consoleAttachment = VZFileHandleSerialPortAttachment(
        fileHandleForReading: FileHandle.standardInput,
        fileHandleForWriting: FileHandle.standardOutput
    )
}
console.attachment = consoleAttachment
config.serialPorts = [console]

let socketDevice = VZVirtioSocketDeviceConfiguration()
config.socketDevices = [socketDevice]

if let sharedDir = args.sharedDir {
    let share = VZSharedDirectory(url: sharedDir, readOnly: false)
    let fs = VZVirtioFileSystemDeviceConfiguration(tag: "workspace")
    fs.share = VZSingleDirectoryShare(directory: share)
    config.directorySharingDevices = [fs]
}

try config.validate()

let vm = VZVirtualMachine(configuration: config)
var socketBridge: SocketBridge?
var socketListener: VZVirtioSocketListener?
logStatus("Starting VM...")
vm.start { result in
    switch result {
    case .success:
        logStatus("VM started.")
        if let device = vm.socketDevices.first as? VZVirtioSocketDevice {
            let bridge = SocketBridge(bridgePort: args.bridgePort)
            let runtimeListener = VZVirtioSocketListener()
            runtimeListener.delegate = bridge
            device.setSocketListener(runtimeListener, forPort: args.vsockPort)
            socketBridge = bridge
            socketListener = runtimeListener
            logStatus("Vsock bridge listening on port \(args.vsockPort) -> 127.0.0.1:\(args.bridgePort)")
        } else {
            logStatus("WARN: no virtio socket device available")
        }
    case .failure(let error):
        let nsError = error as NSError
        logStatus("VM failed to start: \(nsError)")
        logStatus("VM error userInfo: \(nsError.userInfo)")
        exit(1)
    }
}
let timer = DispatchSource.makeTimerSource()
timer.schedule(deadline: .now() + 2, repeating: 5)
timer.setEventHandler {
    logStatus("VM state: \(vm.state.rawValue)")
}
timer.resume()

dispatchMain()
