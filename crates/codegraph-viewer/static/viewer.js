// The bridge between the page and viewer.wasm. Rust owns the app; this file
// only applies its DOM patches, forwards events, and performs fetches.
"use strict";

const enc = new TextEncoder();
const dec = new TextDecoder();
const SVG_NS = "http://www.w3.org/2000/svg";
const nodes = new Map();
let wasm;

function readStr(ptr, len) {
  return dec.decode(new Uint8Array(wasm.memory.buffer, ptr, len));
}

/** Copy `s` into wasm memory; the Rust side takes ownership. */
function passStr(s) {
  const bytes = enc.encode(s);
  const ptr = wasm.cg_alloc(bytes.length);
  new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
  return [ptr, bytes.length];
}

function forget(node) {
  if (node.__cg !== undefined) nodes.delete(node.__cg);
  for (const c of node.childNodes) forget(c);
}

function applyOps(ops) {
  for (const op of ops) {
    switch (op[0]) {
      case "e": {
        const n = op[3] ? document.createElementNS(SVG_NS, op[2]) : document.createElement(op[2]);
        n.__cg = op[1];
        nodes.set(op[1], n);
        break;
      }
      case "t": {
        const n = document.createTextNode(op[2]);
        n.__cg = op[1];
        nodes.set(op[1], n);
        break;
      }
      case "x": nodes.get(op[1]).nodeValue = op[2]; break;
      case "s": {
        const n = nodes.get(op[1]);
        if (op[2] === "value" && "value" in n) { if (n.value !== op[3]) n.value = op[3]; }
        n.setAttribute(op[2], op[3]);
        break;
      }
      case "r": nodes.get(op[1]).removeAttribute(op[2]); break;
      case "h": nodes.get(op[1]).innerHTML = op[2]; break;
      case "i": {
        const parent = nodes.get(op[1]);
        const before = op[3] ? nodes.get(op[3]) : null;
        parent.insertBefore(nodes.get(op[2]), before);
        break;
      }
      case "d": {
        const n = nodes.get(op[1]);
        if (n) { n.remove(); forget(n); }
        break;
      }
      case "f": {
        const n = nodes.get(op[1]);
        if (n && document.activeElement !== n) n.focus();
        break;
      }
      case "v": {
        if (op[1] === "top") { window.scrollTo(0, 0); break; }
        const n = document.getElementById(op[1]);
        if (n) n.scrollIntoView({ block: "center" });
        break;
      }
    }
  }
}

function keyName(e) {
  const mods = (e.metaKey ? "Meta+" : "") + (e.ctrlKey ? "Ctrl+" : "") + (e.altKey ? "Alt+" : "");
  return mods + e.key;
}

function delegate(type, valueOf) {
  document.addEventListener(type, (e) => {
    const attr = "data-on-" + type;
    const target = e.target instanceof Element ? e.target.closest("[" + attr + "]") : null;
    if (!target) return;
    // Links keep their default navigation; everything else is the app's.
    if (type === "click" && target.closest("a[href]") && !target.matches("[data-on-click]")) return;
    if (type === "submit" || (type === "keydown" && ["ArrowDown", "ArrowUp", "Enter", "Escape"].includes(e.key))) {
      e.preventDefault();
    }
    if (type === "click") e.stopPropagation();
    const [ptr, len] = passStr(valueOf(e, target));
    wasm.cg_event(Number(target.getAttribute(attr)), ptr, len);
  }, type === "mouseover" ? { passive: true } : undefined);
}

const host = {
  apply(ptr, len) { applyOps(JSON.parse(readStr(ptr, len))); },
  fetch(id, mPtr, mLen, uPtr, uLen, bPtr, bLen) {
    const method = readStr(mPtr, mLen);
    const url = readStr(uPtr, uLen);
    const body = bLen ? readStr(bPtr, bLen) : undefined;
    const headers = method === "GET" ? {} : { "x-codegraph-ui": "1", "content-type": "application/json" };
    fetch(url, { method, headers, body: method === "GET" ? undefined : body || "{}" })
      .then(async (r) => [r.status, await r.text()])
      .catch((err) => [0, String(err)])
      .then(([status, text]) => {
        const [ptr, len] = passStr(text);
        wasm.cg_fetched(id, status, ptr, len);
      });
  },
  set_hash(ptr, len) { location.hash = readStr(ptr, len); },
  set_title(ptr, len) { document.title = readStr(ptr, len); },
  storage_set(kPtr, kLen, vPtr, vLen) {
    try { localStorage.setItem(readStr(kPtr, kLen), readStr(vPtr, vLen)); } catch (_) {}
  },
  timer(id, ms) { setTimeout(() => wasm.cg_timer(id), ms); },
  log(ptr, len) { console.error(readStr(ptr, len)); },
  export_image(ptr, len) {
    exportImage(JSON.parse(readStr(ptr, len))).then(
      (msg) => notice(msg),
      (err) => notice("Export failed: " + err.message),
    );
  },
};

function notice(text) {
  const [ptr, len] = passStr(text);
  wasm.cg_notice(ptr, len);
}

const STYLE_PROPS = ["fill", "stroke", "stroke-width", "stroke-dasharray", "opacity", "font-family", "font-size", "font-weight", "text-anchor", "paint-order", "stroke-linejoin"];

/** A standalone copy of an on-page SVG, in the light theme with styles inlined. */
function standaloneSvg(src) {
  const root = document.documentElement;
  const prev = root.getAttribute("data-theme");
  root.setAttribute("data-theme", "light");
  try {
    const copy = src.cloneNode(true);
    const a = [src, ...src.querySelectorAll("*")];
    const b = [copy, ...copy.querySelectorAll("*")];
    a.forEach((el, i) => {
      const cs = getComputedStyle(el);
      const style = STYLE_PROPS.map((p) => `${p}:${cs.getPropertyValue(p)}`).join(";");
      b[i].setAttribute("style", style);
      b[i].removeAttribute("class");
      for (const attr of [...b[i].attributes]) if (attr.name.startsWith("data-")) b[i].removeAttribute(attr.name);
    });
    const vb = src.viewBox.baseVal;
    copy.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    copy.setAttribute("width", String(vb.width));
    copy.setAttribute("height", String(vb.height));
    const bg = document.createElementNS("http://www.w3.org/2000/svg", "rect");
    bg.setAttribute("width", "100%");
    bg.setAttribute("height", "100%");
    bg.setAttribute("fill", "#ffffff");
    copy.insertBefore(bg, copy.firstChild);
    return { text: new XMLSerializer().serializeToString(copy), width: vb.width, height: vb.height };
  } finally {
    if (prev === null) root.removeAttribute("data-theme"); else root.setAttribute("data-theme", prev);
  }
}

function download(blob, name) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

async function exportImage({ selector, filename, format }) {
  const src = document.querySelector(selector);
  if (!(src instanceof SVGSVGElement)) throw new Error("nothing to export");
  const svg = standaloneSvg(src);
  const svgBlob = new Blob([svg.text], { type: "image/svg+xml" });
  if (format === "svg") {
    download(svgBlob, filename + ".svg");
    return "Saved " + filename + ".svg";
  }
  const scale = 2;
  const img = new Image();
  const url = URL.createObjectURL(svgBlob);
  try {
    img.src = url;
    await img.decode();
  } finally {
    URL.revokeObjectURL(url);
  }
  const canvas = document.createElement("canvas");
  canvas.width = Math.ceil(svg.width * scale);
  canvas.height = Math.ceil(svg.height * scale);
  const ctx = canvas.getContext("2d");
  ctx.scale(scale, scale);
  ctx.drawImage(img, 0, 0);
  const png = await new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
  if (!png) throw new Error("could not render the image");
  try {
    await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
    return "Image copied to the clipboard";
  } catch (_) {
    download(png, filename + ".png");
    return "Saved " + filename + ".png";
  }
}

async function main() {
  const root = document.getElementById("app");
  nodes.set(0, root);
  let module;
  try {
    const resp = await fetch("viewer.wasm");
    if (!resp.ok) throw new Error(await resp.text());
    module = await WebAssembly.instantiate(await resp.arrayBuffer(), { host });
  } catch (err) {
    root.textContent = "The viewer could not start: " + err.message;
    return;
  }
  wasm = module.instance.exports;
  root.textContent = "";
  root.className = "";
  delegate("click", () => "");
  delegate("input", (e) => e.target.value ?? "");
  delegate("change", (e) => e.target.value ?? "");
  delegate("keydown", (e) => e.key);
  delegate("submit", () => "");
  delegate("mouseover", () => "");
  document.addEventListener("keydown", (e) => {
    const t = e.target;
    const typing = t instanceof HTMLElement && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName));
    const combo = (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k";
    if (typing && !combo && e.key !== "Escape") return;
    if (!typing && !combo && !["/", "Escape", "g", "m"].includes(e.key)) return;
    if (e.metaKey || e.ctrlKey ? !combo : e.altKey) return;
    if (combo || e.key === "/") e.preventDefault();
    const [ptr, len] = passStr(combo ? (e.metaKey ? "Meta+k" : "Ctrl+k") : keyName(e));
    wasm.cg_key(ptr, len);
  });
  // Keyboard activation for SVG nodes that act as buttons.
  document.addEventListener("keydown", (e) => {
    if ((e.key === "Enter" || e.key === " ") && e.target instanceof SVGElement && e.target.matches("[role=button][data-on-click]")) {
      e.preventDefault();
      e.target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    }
  });
  window.addEventListener("hashchange", () => {
    const [ptr, len] = passStr(location.hash);
    wasm.cg_route(ptr, len);
  });
  const storage = {};
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && k.startsWith("cg-")) storage[k] = localStorage.getItem(k);
    }
  } catch (_) {}
  const [ptr, len] = passStr(JSON.stringify({ hash: location.hash, storage }));
  wasm.cg_start(ptr, len);
}

main();
