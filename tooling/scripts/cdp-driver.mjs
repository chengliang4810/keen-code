// KeenCode E2E CDP 驱动：连接 WebView2 远程调试端口，对真实桌面窗口的 UI 执行
// 可复核的交互与断言。零依赖（node >= 22 内置 WebSocket）。
//
// 用法：
//   node out/cdp-driver.mjs ax                     列出可见可交互元素（role/name/enabled/rect/selector 提示）
//   node out/cdp-driver.mjs click "<css>"          真实鼠标点击匹配元素的中心
//   node out/cdp-driver.mjs clicktext button "新建对话"   按角色+可见文本点击
//   node out/cdp-driver.mjs type "<css>" "<text>"  点击后输入文本（insertText）
//   node out/cdp-driver.mjs keys "<text>"          逐键发送（如 "Control_L+a"，键名用 X keysym 风格）
//   node out/cdp-driver.mjs eval "<js>"            执行 JS 并返回 JSON 结果
//   node out/cdp-driver.mjs shot "<file.png>"      截图当前窗口
//   node out/cdp-driver.mjs seq <plan.json>        批量执行动作+断言，输出逐条结果
//
// seq 计划格式：{"steps":[
//   {"do":"clicktext","role":"button","text":"设置"},
//   {"do":"ax","save":"after-open"},
//   {"do":"assert","js":"!!document.querySelector('[role=dialog]')","desc":"弹窗出现"},
//   {"do":"type","css":"textarea","text":"你好"},
//   {"do":"keys","text":"Escape"},
//   {"do":"shot","file":"out/shots/x.png"},
//   {"do":"eval","js":"1+1","save":"n"}
// ]}
// 输出：一行 JSON {"ok":bool,"results":[{step,ok,...}]}（整体 ok=所有步骤成功；断言失败 ok=false 但继续执行）。

import { writeFileSync } from "node:fs";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";

const DEBUG_HOST = "127.0.0.1";
const DEBUG_PORT = process.env.KEENCODE_CDP_PORT ?? "9222";

function die(msg) {
  console.log(JSON.stringify({ ok: false, error: msg }));
  process.exit(1);
}

async function getPageWs() {
  const res = await fetch(`http://${DEBUG_HOST}:${DEBUG_PORT}/json/list`).catch(() => null);
  if (!res || !res.ok) die(`无法连接 CDP http://${DEBUG_HOST}:${DEBUG_PORT}（应用需以 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=${DEBUG_PORT} 启动）`);
  const targets = await res.json();
  const page = targets.find((t) => t.type === "page" && !/devtools/i.test(t.url)) ?? targets.find((t) => t.type === "page");
  if (!page) die("未找到页面目标");
  return page.webSocketDebuggerUrl;
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`CDP 超时: ${method}`));
        }
      }, 20000);
    });
  }
}

const KEY_MAP = { Escape: "Escape", Esc: "Escape", Enter: "Enter", Return: "Enter", Tab: "Tab", Up: "ArrowUp", Down: "ArrowDown", Left: "ArrowLeft", Right: "ArrowRight", BackSpace: "Backspace" };
const MODS = { Control_L: "ctrl", Control_R: "ctrl", ctrl: "ctrl", Alt_L: "alt", alt: "alt", Shift_L: "shift", shift: "shift", super: "meta", Meta_L: "meta" };

async function main() {
  const [, , cmd, ...args] = process.argv;
  if (!cmd) die("缺少命令");
  const wsUrl = await getPageWs();
  const ws = new WebSocket(wsUrl);
  await new Promise((r, j) => { ws.addEventListener("open", r); ws.addEventListener("error", () => j(new Error("ws 连接失败"))); });
  const cdp = new Cdp(ws);
  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");

  const evalJs = async (expr) => {
    const r = await cdp.send("Runtime.evaluate", { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error("页面 JS 异常: " + (r.exceptionDetails.exception?.description ?? r.exceptionDetails.text));
    return r.result?.value;
  };

  // 在页面里定位元素并取视口坐标（含滚动到可见）
  const locateExpr = (findJs) => `(() => {
    const el = (${findJs});
    if (!el) return null;
    el.scrollIntoView({ block: "center", inline: "center" });
    const r = el.getBoundingClientRect();
    return { x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width, h: r.height, tag: el.tagName, role: el.getAttribute("role"), disabled: el.disabled === true || el.getAttribute("aria-disabled") === "true", text: (el.innerText || el.value || el.getAttribute("aria-label") || "").trim().slice(0, 80) };
  })()`;

  const finders = {
    css: (sel) => `document.querySelector(${JSON.stringify(sel)})`,
    roletext: (role, text) => `(() => {
      const want = ${JSON.stringify(text)};
      const roleWant = ${JSON.stringify((role || "").toLowerCase())};
      const roleOf = (e) => e.getAttribute("role") || ({BUTTON:"button",A:"link",INPUT:"textbox",TEXTAREA:"textbox",SELECT:"combobox"}[e.tagName]);
      const pick = (root) => {
        const cands = Array.from(root.querySelectorAll("*")).filter(e => {
          if ((roleOf(e)||"").toLowerCase() !== roleWant) return false;
          const texts = [e.innerText, e.value, e.getAttribute("aria-label"), e.title, e.placeholder].map(t => (t || "").trim()).filter(Boolean);
          if (!texts.length) return false;
          return texts.some(t => t === want || t.includes(want));
        });
        cands.sort((a,b)=>(a.innerText||a.value||"").length-(b.innerText||b.value||"").length);
        return cands[0] || null;
      };
      // 最上层弹窗存在时优先在其中查找（真实坐标点击会命中遮罩导致弹窗关闭）
      const dialogs = document.querySelectorAll('[role="dialog"],[data-state="open"][role="alertdialog"]');
      if (dialogs.length) {
        for (let i = dialogs.length - 1; i >= 0; i--) {
          const hit = pick(dialogs[i]);
          if (hit) return hit;
        }
      }
      return pick(document);
    })()`,
  };

  const realClick = async (pos) => {
    const move = (x, y, type) => cdp.send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1, pointerType: "mouse" });
    await move(pos.x, pos.y, "mouseMoved");
    await move(pos.x, pos.y, "mousePressed");
    await new Promise((r) => setTimeout(r, 60));
    await move(pos.x, pos.y, "mouseReleased");
  };

  const typeText = async (text) => {
    await cdp.send("Input.insertText", { text });
  };

  const pressKeys = async (combo) => {
    const parts = combo.split("+").map((s) => s.trim());
    let mods = [];
    let key = parts[parts.length - 1];
    for (const p of parts.slice(0, -1)) {
      const m = MODS[p];
      if (!m) die(`未知修饰键: ${p}`);
      mods.push(m);
    }
    const textKey = KEY_MAP[key] ?? key;
    const dispatch = (type, params) => cdp.send("Input.dispatchKeyEvent", { type, ...params });
    const base = { key: textKey, code: textKey, windowsVirtualKeyCode: 0, modifiers: mods.map((m) => ({ ctrl: 2, alt: 1, shift: 8, meta: 4 }[m])).reduce((a, b) => a | b, 0) };
    for (const m of mods) await dispatch("rawKeyDown", { ...base, key: m === "ctrl" ? "Control" : m === "alt" ? "Alt" : m === "shift" ? "Shift" : "Meta", code: m === "ctrl" ? "ControlLeft" : m === "alt" ? "AltLeft" : m === "shift" ? "ShiftLeft" : "MetaLeft", windowsVirtualKeyCode: { ctrl: 17, alt: 18, shift: 16, meta: 91 }[m] });
    await dispatch("rawKeyDown", { ...base });
    if (textKey.length === 1) await dispatch("keyDown", { ...base, text: textKey });
    await dispatch("keyUp", { ...base });
    for (const m of mods.reverse()) await dispatch("keyUp", { key: m === "ctrl" ? "Control" : m === "alt" ? "Alt" : m === "shift" ? "Shift" : "Meta", code: m === "ctrl" ? "ControlLeft" : m === "alt" ? "AltLeft" : m === "shift" ? "ShiftLeft" : "MetaLeft", windowsVirtualKeyCode: { ctrl: 17, alt: 18, shift: 16, meta: 91 }[m], modifiers: 0 });
  };

  const axSnapshot = async () => evalJs(`(() => {
    const interactive = 'button,[role],[contenteditable],input,textarea,select,a[href],summary,option';
    const out = [];
    const seen = new Set();
    for (const el of document.querySelectorAll(interactive)) {
      if (seen.has(el)) continue;
      seen.add(el);
      const r = el.getBoundingClientRect();
      const style = getComputedStyle(el);
      if (r.width < 2 || r.height < 2 || style.visibility === "hidden" || style.display === "none") continue;
      let p = el, win = true;
      while (p && p !== document.body) { const s = getComputedStyle(p); if (s.display === "none" || s.visibility === "hidden") { win = false; break; } p = p.parentElement; }
      if (!win) continue;
      const role = el.getAttribute("role") || ({ BUTTON: "button", A: "link", INPUT: el.type === "checkbox" ? "checkbox" : "textbox", TEXTAREA: "textbox", SELECT: "combobox" }[el.tagName]) || el.tagName.toLowerCase();
      out.push({ role, name: (el.getAttribute("aria-label") || el.innerText || el.value || el.placeholder || el.title || "").trim().replace(/\\s+/g, " ").slice(0, 60), tag: el.tagName.toLowerCase(), checked: el.getAttribute("aria-checked") ?? undefined, disabled: el.disabled === true || el.getAttribute("aria-disabled") === "true" || el.getAttribute("data-disabled") === "" || undefined, x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height), id: el.id || undefined });
    }
    out.sort((a, b) => a.y - b.y || a.x - b.x);
    return { url: location.href, title: document.title, count: out.length, elements: out };
  })()`);

  const runStep = async (step, index) => {
    const entry = { step: index, do: step.do };
    try {
      if (step.do === "ax") {
        const snap = await axSnapshot();
        entry.ok = true;
        entry.snapshot = snap;
      } else if (step.do === "click" || step.do === "type") {
        const find = step.css ? finders.css(step.css) : finders.roletext(step.role, step.text);
        const pos = await evalJs(locateExpr(find));
        if (!pos) { entry.ok = false; entry.error = "未找到元素 " + (step.css ?? `${step.role}:${step.text}`); return entry; }
        entry.found = pos;
        if (pos.disabled) { entry.ok = false; entry.error = "元素处于禁用状态"; entry.found = pos; return entry; }
        await realClick(pos);
        entry.ok = true;
        if (step.do === "type") {
          await new Promise((r) => setTimeout(r, 120));
          await typeText(step.text);
          // 校验文本已落位；未落位则用原生 setter 直接赋值并派发 input 事件
          const landed = await evalJs(`(() => {
            const el = (${finders.css(step.css)});
            if (!el) return null;
            if ((el.value ?? el.innerText ?? "") === ${JSON.stringify(step.text)}) return true;
            const proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : el.getAttribute("contenteditable") ? null : HTMLInputElement.prototype;
            if (el.getAttribute("contenteditable") || el.tagName === "DIV") { el.innerText = ${JSON.stringify(step.text)}; el.dispatchEvent(new InputEvent("input", { bubbles: true })); return (el.innerText || "") === ${JSON.stringify(step.text)}; }
            if (proto) { const d = Object.getOwnPropertyDescriptor(proto, "value"); d && d.set && d.set.call(el, ${JSON.stringify(step.text)}); el.dispatchEvent(new Event("input", { bubbles: true })); return el.value === ${JSON.stringify(step.text)}; }
            return false;
          })()`);
          entry.landed = landed;
          if (landed === false) { entry.ok = false; entry.error = "文本未能写入目标"; }
        }
      } else if (step.do === "clicktext") {
        const find = finders.roletext(step.role ?? "button", step.text);
        const pos = await evalJs(locateExpr(find));
        if (!pos) { entry.ok = false; entry.error = `未找到 ${step.role ?? "button"}:${step.text}`; return entry; }
        if (pos.disabled) { entry.ok = false; entry.error = "元素禁用"; entry.found = pos; return entry; }
        await realClick(pos);
        entry.ok = true;
      } else if (step.do === "keys") {
        await pressKeys(step.text);
        entry.ok = true;
      } else if (step.do === "eval") {
        entry.value = await evalJs(step.js);
        entry.ok = true;
      } else if (step.do === "assert") {
        const v = await evalJs(step.js);
        entry.ok = v === true;
        entry.value = v;
        entry.desc = step.desc ?? "";
      } else if (step.do === "wait") {
        await new Promise((r) => setTimeout(r, Number(step.ms ?? 500)));
        entry.ok = true;
      } else if (step.do === "shot") {
        mkdirSync(dirname(step.file), { recursive: true });
        const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
        writeFileSync(step.file, Buffer.from(shot.data, "base64"));
        entry.ok = true;
        entry.file = step.file;
      } else {
        entry.ok = false;
        entry.error = "未知动作 " + step.do;
      }
      if (step.save) entry.saved = step.save;
    } catch (e) {
      entry.ok = false;
      entry.error = String(e.message ?? e).slice(0, 300);
    }
    return entry;
  };

  let result;
  if (cmd === "ax") {
    result = { ok: true, snapshot: await axSnapshot() };
  } else if (cmd === "eval") {
    try { result = { ok: true, value: await evalJs(args[0]) }; }
    catch (e) { result = { ok: false, error: String(e.message ?? e).slice(0, 500) }; }
  } else if (cmd === "click") {
    result = await runStep({ do: "click", css: args[0] }, 1);
  } else if (cmd === "clicktext") {
    result = await runStep({ do: "clicktext", role: args[0], text: args[1] }, 1);
  } else if (cmd === "type") {
    result = await runStep({ do: "type", css: args[0], text: args[1] }, 1);
  } else if (cmd === "keys") {
    result = await runStep({ do: "keys", text: args[0] }, 1);
  } else if (cmd === "shot") {
    result = await runStep({ do: "shot", file: args[0] }, 1);
  } else if (cmd === "seq") {
    const plan = JSON.parse(await (await import("node:fs/promises")).readFile(args[0], "utf8"));
    const results = [];
    let allOk = true;
    for (let i = 0; i < plan.steps.length; i++) {
      const r = await runStep(plan.steps[i], i + 1);
      if (plan.steps[i].do === "assert" && r.ok === false) allOk = false;
      if (r.ok === false && plan.steps[i].do !== "assert") allOk = false;
      results.push(r);
    }
    result = { ok: allOk, results };
  } else {
    die("未知命令 " + cmd);
  }
  console.log(JSON.stringify(result));
  ws.close();
  process.exit(0);
}

main().catch((e) => die(String(e.message ?? e)));
