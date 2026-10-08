#!/usr/bin/env python3
"""macOS native acceptance; all model/tool/session data uses a temporary root.

Run from the repository root. Requires Accessibility and Screen Recording
permissions for the launching terminal. Reads the supplied providers file;
never writes to the original config or prints its contents.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--providers", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("output/acceptance/native"))
    parser.add_argument("--ime-source", help="Optional installed macOS input source ID for composition acceptance")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    binary = out / "keencode-native-acceptance"
    subprocess.run(["go", "build", "-o", str(binary), "./cmd/keencode"], check=True)
    helper = out / "window-id.swift"
    helper.write_text('''import CoreGraphics
import Foundation
let pid = Int(CommandLine.arguments[1])!
let windows = CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID) as! [[String:Any]]
for w in windows where (w[kCGWindowOwnerPID as String] as? Int) == pid && (w[kCGWindowLayer as String] as? Int) == 0 && (w[kCGWindowIsOnscreen as String] as? Bool) == true && (w[kCGWindowName as String] as? String) == "KeenCode" {
 if CommandLine.arguments.count > 2 {
  let bounds = w[kCGWindowBounds as String] as! [String:NSNumber]
  print(bounds["Width"]!, bounds["Height"]!)
 } else { print(w[kCGWindowNumber as String]!) }
 break
}
''')
    window_helper = out / "window-id"
    subprocess.run(["swiftc", str(helper), "-o", str(window_helper)], check=True)
    ime_file = out / "input-source.swift"
    ime_file.write_text('''import Carbon
import Foundation
func sourceID(_ source: TISInputSource) -> String {
 return Unmanaged<CFString>.fromOpaque(TISGetInputSourceProperty(source, kTISPropertyInputSourceID)!).takeUnretainedValue() as String
}
if CommandLine.arguments.count == 1 {
 print(sourceID(TISCopyCurrentKeyboardInputSource().takeRetainedValue()))
} else {
 let sources = TISCreateInputSourceList(nil, false).takeRetainedValue() as NSArray
 for entry in sources {
  let source = entry as! TISInputSource
  if sourceID(source) == CommandLine.arguments[1] {
   let status = TISSelectInputSource(source)
   if status != noErr { exit(1) }
   exit(0)
  }
 }
 exit(2)
}
''')
    ime_helper = out / "input-source"
    subprocess.run(["swiftc", str(ime_file), "-o", str(ime_helper)], check=True)
    saved_input_source = None
    toggled_ime = False
    with tempfile.TemporaryDirectory(prefix="keencode-native-") as tmp:
        root = Path(tmp)
        source = args.providers.resolve()
        shutil.copyfile(source, root / "providers.json")
        os.chmod(root / "providers.json", 0o600)
        work = root / "workspace"
        work.mkdir()
        (root / "settings.json").write_text(json.dumps({
            "schema": "keencode/app-settings", "version": 1, "theme": "dark",
            "workingDirectory": str(work), "toolPermissionPolicy": "ask",
        }))
        log = (out / "native.log").open("w")
        process = None

        def launch():
            return subprocess.Popen([str(binary)], env={**os.environ, "KEENCODE_GO_HOME": str(root)}, stdout=log, stderr=log)

        def osa(body):
            script = f'''tell application "System Events"
tell (first application process whose unix id is {process.pid})
set frontmost to true
{body}
end tell
end tell'''
            return subprocess.check_output(["osascript", "-e", script], text=True).strip()

        def wait(predicate, label, timeout=90):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if process.poll() is not None:
                    raise RuntimeError("native application exited; inspect native.log")
                if predicate():
                    return
                time.sleep(0.2)
            raise RuntimeError(f"timeout: {label}")

        captured_sizes = {}

        def bounds():
            raw = subprocess.check_output([str(window_helper), str(process.pid), "bounds"], text=True).split()
            return [int(float(n)) for n in raw]

        def resize(width, height):
            osa(f'''try
if value of attribute "AXFullScreen" of window "KeenCode" then
set value of attribute "AXFullScreen" of window "KeenCode" to false
delay 1
end if
end try
set size of window "KeenCode" to {{{width},{height}}}
set position of window "KeenCode" to {{80,70}}''')
            wait(lambda: len(bounds()) == 2 and bounds()[0] == width and height <= bounds()[1] <= height + 20, "actual window size", 10)

        def screenshot(name):
            def capture_window():
                return subprocess.check_output([str(window_helper), str(process.pid)], text=True).strip()
            wait(lambda: bool(capture_window()), "visible capture window", 10)
            win = capture_window()
            if not win:
                raise RuntimeError("no native window")
            captured_sizes[name] = bounds()
            subprocess.run(["screencapture", "-x", "-o", f"-l{win}", str(out / name)], check=True)

        def events():
            result = []
            for path in root.rglob("journal.jsonl"):
                for line in path.read_text().splitlines():
                    try:
                        result.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass  # writer may be finishing the tail
            return result

        def send(prompt, middle=False, submit=True):
            wait(lambda: osa('return exists text area 1 of group 1 of window "KeenCode"') == "true", "editor before input", 10)
            # Clipboard paste is independent of the user's current IME.
            # Restore it even if automation raises an AppleScript error.
            keys = 'key code 123\nkey code 123' if middle else ''
            enter = 'key code 36' if submit else ''
            osa(f'''set savedClipboard to the clipboard
try
set the clipboard to {json.dumps(prompt, ensure_ascii=False)}
set value of attribute "AXFocused" of text area 1 of group 1 of window "KeenCode" to true
key code 53
keystroke "a" using command down
keystroke "v" using command down
delay 0.2
{keys}
{enter}
set the clipboard to savedClipboard
on error messageText number errorNumber
set the clipboard to savedClipboard
error messageText number errorNumber
end try''')

        try:
            process = launch()
            wait(lambda: bool(subprocess.check_output([str(window_helper), str(process.pid)], text=True).strip()), "window", 20)
            wait(lambda: osa('return exists window "KeenCode"') == "true", "window accessibility", 20)
            resize(1280, 800)
            (out / "initial-accessibility.txt").write_text(osa('get entire contents of window "KeenCode"'))
            wait(lambda: osa('return exists text area 1 of group 1 of window "KeenCode"') == "true", "editor accessibility", 10)
            time.sleep(0.5)
            (out / "initial-accessibility.txt").write_text(osa('get entire contents of window "KeenCode"'))
            screenshot("01-draft-dark.png")
            osa('click (pop up button 1 of group 1 of window "KeenCode")')
            time.sleep(0.2)
            screenshot("02-model-menu.png")
            osa('key code 53')
            send("第一行", submit=False)
            osa('key code 36 using shift down')
            time.sleep(0.2)
            assert osa('get value of text area 1 of group 1 of window "KeenCode"') == "第一行"
            # AppleScript strips the trailing newline from command output;
            # compare an explicit marker to preserve it.
            assert osa('return (value of text area 1 of group 1 of window "KeenCode") & "END"') == "第一行\nEND"
            assert not any(e["type"] == "user_message" for e in events())
            screenshot("02-shift-enter.png")
            if args.ime_source:
                saved_input_source = subprocess.check_output([str(ime_helper)], text=True).strip()
                subprocess.run([str(ime_helper), args.ime_source], check=True)
                osa('keystroke "a" using command down\nkey code 51')
                # Physical keys n/i/h/a/o go through the active IME.
                osa('key code 45\nkey code 34\nkey code 4\nkey code 0\nkey code 31')
                time.sleep(0.5)
                if osa('get value of text area 1 of group 1 of window "KeenCode"') == "nihao":
                    # Sogou remembers English mode per application. Switch
                    # once only when the actual buffer proves English mode.
                    osa('keystroke "a" using command down\nkey code 51\nkey code 56')
                    toggled_ime = True
                    osa('key code 45\nkey code 34\nkey code 4\nkey code 0\nkey code 31')
                    time.sleep(0.5)
                assert osa('get value of text area 1 of group 1 of window "KeenCode"') == "", "IME is not composing"
                screenshot("02-ime-marked.png")
                osa('key code 36')
                time.sleep(0.3)
                committed = osa('get value of text area 1 of group 1 of window "KeenCode"')
                (out / "ime-buffer.json").write_text(json.dumps({"afterEnter": committed}, ensure_ascii=False))
                assert committed.replace("\'", "") == "nihao", "IME Enter did not commit the marked test text"
                assert not any(e["type"] == "user_message" for e in events()), "IME commit incorrectly sent a message"
                screenshot("02-ime-committed.png")
                osa('keystroke "a" using command down\nkey code 51\nkey code 45\nkey code 34\nkey code 4\nkey code 0\nkey code 31\nkey code 49')
                time.sleep(0.3)
                chinese = osa('get value of text area 1 of group 1 of window "KeenCode"')
                assert chinese and any("\u4e00" <= char <= "\u9fff" for char in chinese), "IME Space did not commit Chinese text"
                assert not any(e["type"] == "user_message" for e in events()), "Chinese composition incorrectly sent a message"
                screenshot("02-ime-chinese.png")
                if toggled_ime:
                    osa('key code 56')
                    toggled_ime = False
                subprocess.run([str(ime_helper), saved_input_source], check=True)
                saved_input_source = None
            prompt = "Reply with exactly: NATIVE_OK"
            send(prompt, middle=True)
            wait(lambda: any(e["type"] == "turn_completed" for e in events()), "live reply")
            first = events()
            assert any(e["type"] == "user_message" and e["payload"].get("text", "").rstrip("\n") == prompt for e in first)
            assert "NATIVE_OK" in "".join(e["payload"].get("text", "") for e in first if e["type"] == "text_delta")
            screenshot("03-reply-dark.png")
            send("Use Write to create native-success.txt containing exactly GO_NATIVE_TOOL_OK. Only reply TOOL_OK after the tool succeeds.")

            def approve():
                if osa('return exists sheet 1 of window "KeenCode"') != "true":
                    return False
                details = osa('get value of every static text of sheet 1 of window "KeenCode"')
                if "native-success.txt" not in details:
                    raise RuntimeError("unexpected permission request")
                screenshot("04-write-permission.png")
                osa('click button "允许一次" of sheet 1 of window "KeenCode"')
                return True

            wait(approve, "Write permission")
            wait(lambda: sum(e["type"] == "turn_completed" for e in events()) >= 2, "tool reply")
            assert (work / "native-success.txt").read_text() == "GO_NATIVE_TOOL_OK"
            assert any(e["type"] == "tool_end" and e["payload"]["tool"]["status"] == "completed" for e in events())
            screenshot("05-tool-success.png")
            # Restart the executable and verify disk recovery on the UI.
            process.terminate(); process.wait(timeout=10)
            process = launch()
            wait(lambda: bool(subprocess.check_output([str(window_helper), str(process.pid)], text=True).strip()), "restored window", 20)
            wait(lambda: osa('return exists window "KeenCode"') == "true", "restored accessibility", 20)
            resize(1280, 800)
            wait(lambda: "TOOL_OK" in osa('get entire contents of window "KeenCode"'), "restored reply")
            screenshot("06-restored-chat.png")
            osa('click button "新建任务" of window "KeenCode"')
            wait(lambda: osa('return exists text area 1 of group 1 of window "KeenCode"') == "true", "new draft", 10)
            osa('click button "设置" of window "KeenCode"')
            wait(lambda: osa('return exists scroll area 1 of window "KeenCode"') == "true", "settings", 10)
            resize(1280, 800)
            time.sleep(0.2)
            screenshot("07-settings-dark.png")
            (out / "settings-accessibility.txt").write_text(osa('get entire contents of window "KeenCode"'))
            osa('set value of attribute "AXFocused" of pop up button 1 of scroll area 1 of window "KeenCode" to true\nclick pop up button 1 of scroll area 1 of window "KeenCode"\ndelay 0.3\nkey code 126\nkey code 36')
            wait(lambda: json.loads((root / "settings.json").read_text())["theme"] == "light", "light theme", 5)
            screenshot("08-settings-light.png")
            osa('click button "返回" of window "KeenCode"')
            time.sleep(0.2)
            screenshot("09-draft-light.png")
            resize(720, 480)
            time.sleep(0.2)
            screenshot("10-draft-light-small.png")
            report = {"status":"PASS", "platform":"macOS", "viewport":[1280,800],
                      "checks":["Shift+Enter newline without sending", "model selector", "Enter at middle", "live reply", "native permission",
                                "actual Write contents", "journal recovery", "theme persistence", "small window"],
                      "events":[{"type":e["type"], "sequence":e["sequence"], "toolStatus":e["payload"].get("tool",{}).get("status")} for e in events()]}
            report["capturedWindowSizesDIP"] = captured_sizes
            report["imeSourceTested"] = args.ime_source
            if args.ime_source:
                report["checks"].append("Chinese IME Enter commits without sending")
            (out / "result.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
            print(f"PASS: native acceptance; artifacts in {out}")
        except Exception as error:
            (out / "result.json").write_text(json.dumps({"status": "FAIL", "error": str(error)}, ensure_ascii=False, indent=2))
            raise
        finally:
            if toggled_ime and process and process.poll() is None:
                osa('key code 56')
            if saved_input_source:
                subprocess.run([str(ime_helper), saved_input_source], check=True)
            if process and process.poll() is None:
                process.terminate(); process.wait(timeout=10)
            log.close()


if __name__ == "__main__":
    main()
