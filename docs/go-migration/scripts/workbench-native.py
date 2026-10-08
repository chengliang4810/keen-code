#!/usr/bin/env python3
"""Isolated macOS acceptance of the workbench; no model keys.
Seeds explicit conversation fixtures, then operates the real native UI, Git
repository and PTY. Requires Accessibility and Screen Recording.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=Path("output/acceptance/workbench-native"))
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    binary = out / "keencode-native"
    subprocess.run(["go", "build", "-o", str(binary), "./cmd/keencode"], check=True)
    swift = out / "access.swift"
    swift.write_text('''import ApplicationServices
import CoreGraphics
import Foundation
let pid = Int32(CommandLine.arguments[1])!
let command = CommandLine.arguments[2]
let wanted = CommandLine.arguments.count > 3 ? CommandLine.arguments[3] : ""
func attr(_ el: AXUIElement, _ name: String) -> AnyObject? {
 var value: CFTypeRef?
 if AXUIElementCopyAttributeValue(el, name as CFString, &value) == .success { return value }
 return nil
}
func name(_ el: AXUIElement) -> String {
 return (attr(el,"AXDescription") as? String) ?? (attr(el,"AXTitle") as? String) ?? ""
}
func find(_ el: AXUIElement) -> AXUIElement? {
 if name(el) == wanted { return el }
 for child in (attr(el,"AXChildren") as? [AXUIElement]) ?? [] {
  if let hit = find(child) { return hit }
 }
 return nil
}
let app = AXUIElementCreateApplication(pid)
if command == "window" {
 let windows = CGWindowListCopyWindowInfo([.optionAll,.excludeDesktopElements],kCGNullWindowID) as! [[String:Any]]
 for w in windows where (w[kCGWindowOwnerPID as String] as? Int32) == pid && (w[kCGWindowLayer as String] as? Int) == 0 && (w[kCGWindowIsOnscreen as String] as? Bool) == true && (w[kCGWindowName as String] as? String) == "KeenCode" {
  let b = w[kCGWindowBounds as String] as! [String:NSNumber]
  print(w[kCGWindowNumber as String]!,b["Width"]!,b["Height"]!)
  exit(0)
 }
 exit(2)
}
if command == "dump" {
 func dump(_ el: AXUIElement,_ depth: Int) {
  print(String(repeating:" ",count:depth)+(attr(el,"AXRole") as? String ?? "")+" "+name(el))
  for child in (attr(el,"AXChildren") as? [AXUIElement]) ?? [] { dump(child,depth+1) }
 }
 dump(app,0); exit(0)
}
guard let el = find(app) else { print("missing: "+wanted); exit(2) }
if command == "exists" {print("true");exit(0)}
if command == "click" {
 let err = AXUIElementPerformAction(el,kAXPressAction as CFString)
 if err != .success { print("press: "+String(err.rawValue));exit(3) }
} else if command == "focus" {
 let err = AXUIElementSetAttributeValue(el,kAXFocusedAttribute as CFString,kCFBooleanTrue)
 if err != .success { print("focus: "+String(err.rawValue));exit(3) }
}
''')
    helper = out / "access"
    subprocess.run(["swiftc", str(swift), "-o", str(helper)], check=True)
    screenshots, checks = {}, []
    with tempfile.TemporaryDirectory(prefix="keencode-workbench-") as tmp:
        root = Path(tmp).resolve()
        work = root / "project"
        work.mkdir()
        def git(*args):
            subprocess.run(["git", "-C", str(work), *args], check=True, capture_output=True)
        git("init", "-q")
        (work / "note.txt").write_text("ORIGINAL\n")
        git("add", ".")
        git("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "initial")
        (work / "note.txt").write_text("NATIVE_DIFF_CONTENT\n")
        (work / "src").mkdir()
        (work / "src" / "hello.go").write_text("package nativefixture\n")
        settings = {"schema": "keencode/app-settings", "version": 1, "theme": "dark",
                    "workingDirectory": str(work), "toolPermissionPolicy": "ask"}
        (root / "settings.json").write_text(json.dumps(settings))
        (root / "workspaces.json").write_text(json.dumps({"schema": "keencode/workspaces", "version": 1, "projects": [str(work)]}))
        now = int(time.time() * 1000)
        for i, (title, pinned) in enumerate([("已置顶的对话", True), ("项目中的对话", False)], 1):
            sid = "0" * 25 + str(i)
            session = root / "sessions" / sid
            session.mkdir(parents=True)
            (session / "meta.json").write_text(json.dumps({"schema": "keencode/session-meta", "version": 1,
                "id": sid, "title": title, "pinned": pinned, "projectDir": str(work),
                "createdAtUnixMs": now + i, "updatedAtUnixMs": now + i}))
            events = []
            for seq, (kind, text) in enumerate([("user_message", "验证三栏工作台"),
                  ("text_delta", "当前对话位于中间。右侧可打开终端、Git 差异和项目文件。"),
                  ("turn_completed", "")], 1):
                events.append({"schema": "keencode/session-event", "version": 1, "eventId": f"fixture-{i}-{seq}",
                    "session": sid, "sequence": seq, "timeUnixMs": now + i, "type": kind,
                    "payload": {"turnId": "fixture-turn", "text": text}})
            (session / "journal.jsonl").write_text("".join(json.dumps(e, ensure_ascii=False) + "\n" for e in events))
        log = (out / "native.log").open("w")
        process = None
        def launch():
            return subprocess.Popen([str(binary)], env={**os.environ, "KEENCODE_GO_HOME": str(root)}, stdout=log, stderr=log)
        def ax(command, label="", check=True):
            result = subprocess.run([str(helper), str(process.pid), command, label], capture_output=True, text=True)
            if check and result.returncode:
                raise RuntimeError(result.stdout.strip() or result.stderr.strip())
            return result.stdout.strip() if result.returncode == 0 else ""
        def osa(body):
            script = f'''tell application "System Events"
tell (first application process whose unix id is {process.pid})
set frontmost to true
{body}
end tell
end tell'''
            return subprocess.check_output(["osascript", "-e", script], text=True).strip()
        def wait(predicate, label, timeout=30):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if process.poll() is not None:
                    raise RuntimeError("native process exited; inspect native.log")
                if predicate():
                    return
                time.sleep(0.15)
            raise RuntimeError("timeout: " + label)
        def click(label):
            wait(lambda: ax("exists", label, False) == "true", label)
            ax("click", label)
            time.sleep(0.2)
        def screenshot(name):
            info = ax("window").split()
            screenshots[name] = [int(float(n)) for n in info[1:]]
            subprocess.run(["screencapture", "-x", "-o", "-l" + info[0], str(out / name)], check=True)
        def resize(w, h):
            osa(f'''try
if value of attribute "AXFullScreen" of window "KeenCode" then
set value of attribute "AXFullScreen" of window "KeenCode" to false
delay 1
end if
end try
set size of window "KeenCode" to {{{w},{h}}}''')
            wait(lambda: ax("window").split()[1:] == [str(w), str(h)], "actual window size")
        def stop():
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        try:
            process = launch()
            wait(lambda: bool(ax("window", check=False)), "window")
            resize(1280, 800)
            osa("set frontmost to true")
            wait(lambda: ax("exists", "打开标签页", False) == "true", "three columns")
            (out / "accessibility.txt").write_text(ax("dump"))
            for label in ("置顶", "项目", "对话", "设置", "已置顶的对话", "项目中的对话"):
                assert ax("exists", label, False) == "true", label
            for label in ("搜索", "自动化", "分组"):
                assert not ax("exists", label, False), label
            screenshot("01-three-columns-dark.png")
            click("置顶当前对话")
            current_meta = root / "sessions" / ("0" * 25 + "2") / "meta.json"
            wait(lambda: json.loads(current_meta.read_text())["pinned"], "persisted pin")
            checks.append("native navigation and persisted pin")
            click("面板：差异")
            wait(lambda: ax("exists", "+NATIVE_DIFF_CONTENT", False) == "true", "actual git diff")
            screenshot("02-git-diff-dark.png")
            checks.append("actual git diff")
            click("面板：文件")
            click("文件：note.txt")
            wait(lambda: "NATIVE_DIFF_CONTENT" in ax("dump"), "file contents")
            screenshot("03-file-preview-dark.png")
            click("返回文件列表")
            click("目录：src")
            click("文件：src/hello.go")
            wait(lambda: "nativefixture" in ax("dump"), "nested file contents")
            checks.append("native file tree and preview")
            click("面板：终端")
            wait(lambda: ax("exists", "Terminal", False) == "true", "native PTY", 60)
            ax("focus", "Terminal")
            command = "printf WORKBENCH_PTY_OK > terminal-proof.txt; echo WORKBENCH_PTY_OK; printf '%s' $$ > terminal-pid.txt"
            osa(f'''set savedClipboard to the clipboard
try
set the clipboard to {json.dumps(command)}
keystroke "v" using command down
delay 0.3
key code 36
set the clipboard to savedClipboard
on error messageText number errorNumber
set the clipboard to savedClipboard
error messageText number errorNumber
end try''')
            wait(lambda: (work / "terminal-proof.txt").exists(), "actual shell command")
            assert (work / "terminal-proof.txt").read_text() == "WORKBENCH_PTY_OK"
            wait(lambda: (work / "terminal-pid.txt").exists(), "shell pid")
            shell_pid = int((work / "terminal-pid.txt").read_text())
            screenshot("04-terminal-dark.png")
            checks.append("real PTY executes in conversation directory")
            click("关闭右侧面板")
            def exited():
                try:
                    os.kill(shell_pid, 0)
                    return False
                except ProcessLookupError:
                    return True
            wait(exited, "PTY released after panel close", 10)
            checks.append("panel close releases shell process")
            click("展开右侧面板")
            resize(720, 490)
            assert not ax("exists", "左侧导航", False)
            screenshot("05-narrow-side-pane.png")
            click("关闭右侧面板")
            assert ax("exists", "左侧导航", False) == "true"
            checks.append("720x490 side pane and navigation toggles")
            resize(1280, 800)
            click("设置")
            wait(lambda: ax("exists", "主题", False) == "true", "settings entry")
            checks.append("bottom settings entry")
            stop()
            settings = json.loads((root / "settings.json").read_text())
            settings["theme"] = "light"
            (root / "settings.json").write_text(json.dumps(settings))
            process = launch()
            wait(lambda: bool(ax("window", check=False)), "light restart")
            resize(1280, 800)
            wait(lambda: ax("exists", "打开标签页", False) == "true", "restored workbench")
            assert json.loads(current_meta.read_text())["pinned"]
            screenshot("06-three-columns-light.png")
            checks.append("restart restores pin and project navigation, light theme")
            click("面板：差异")
            wait(lambda: ax("exists", "+NATIVE_DIFF_CONTENT", False) == "true", "light diff")
            screenshot("07-git-diff-light.png")
            (out / "result.json").write_text(json.dumps({"status": "PASS", "platform": "macOS",
                "fixtureConversations": True, "checks": checks, "capturedWindowSizesDIP": screenshots}, indent=2))
            print("PASS: " + str(out / "result.json"))
        except Exception:
            if process and process.poll() is None:
                (out / "failure-accessibility.txt").write_text(ax("dump", check=False))
            raise
        finally:
            stop()
            log.close()


if __name__ == "__main__":
    main()
