import Foundation
import Security

guard CommandLine.arguments.count == 3, !CommandLine.arguments[2].isEmpty else { exit(64) }
let service = CommandLine.arguments[2]
let account = "mac-bridge-state-v1"
let maximumBytes = 64 * 1024
let base: [String: Any] = [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: service,
    kSecAttrAccount as String: account,
]


switch CommandLine.arguments[1] {
case "read":
    var query = base
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var output: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &output)
    if status == errSecItemNotFound { exit(44) }
    guard status == errSecSuccess,
          let data = output as? Data,
          !data.isEmpty,
          data.count <= maximumBytes else { exit(74) }
    FileHandle.standardOutput.write(data)
case "write":
    let data = FileHandle.standardInput.readDataToEndOfFile()
    guard !data.isEmpty,
          data.count <= maximumBytes,
          (try? JSONSerialization.jsonObject(with: data)) is [String: Any] else { exit(65) }
    let status = SecItemUpdate(
        base as CFDictionary,
        [kSecValueData as String: data] as CFDictionary
    )
    if status == errSecSuccess { exit(0) }
    guard status == errSecItemNotFound else { exit(74) }
    var add = base
    add[kSecValueData as String] = data
    add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
    guard SecItemAdd(add as CFDictionary, nil) == errSecSuccess else { exit(74) }
default:
    exit(64)
}
