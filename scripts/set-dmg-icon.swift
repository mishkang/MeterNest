import AppKit

// 为 DMG 设置自定义文件图标（与 Electron 版观感一致）。
// 用法: swift scripts/set-dmg-icon.swift <dmg 路径> <icns 路径>
let arguments = CommandLine.arguments
guard arguments.count >= 3 else {
    FileHandle.standardError.write("用法: swift scripts/set-dmg-icon.swift <dmg> <icns>\n".data(using: .utf8)!)
    exit(1)
}
let dmgPath = arguments[1]
let iconPath = arguments[2]
guard FileManager.default.fileExists(atPath: dmgPath) else {
    FileHandle.standardError.write("文件不存在: \(dmgPath)\n".data(using: .utf8)!)
    exit(1)
}
guard let icon = NSImage(contentsOfFile: iconPath) else {
    FileHandle.standardError.write("图标无法读取: \(iconPath)\n".data(using: .utf8)!)
    exit(1)
}
if NSWorkspace.shared.setIcon(icon, forFile: dmgPath) {
    print("已设置 DMG 图标: \(dmgPath)")
} else {
    FileHandle.standardError.write("设置图标失败: \(dmgPath)\n".data(using: .utf8)!)
    exit(1)
}
