// Theme toggle, copy buttons, mobile navigation, table-of-contents
// highlighting and client-side search for the docs.
"use strict";

(function () {
  const root = document.documentElement;

  // Theme: an explicit choice is remembered; otherwise the system decides.
  const themeBtn = document.getElementById("theme");
  if (themeBtn) {
    themeBtn.addEventListener("click", () => {
      const dark = root.dataset.theme
        ? root.dataset.theme === "dark"
        : matchMedia("(prefers-color-scheme: dark)").matches;
      root.dataset.theme = dark ? "light" : "dark";
      try { localStorage.setItem("cg-theme", root.dataset.theme); } catch (_) {}
    });
  }

  // Copy buttons on code blocks.
  document.addEventListener("click", async (e) => {
    const btn = e.target instanceof Element ? e.target.closest(".copy") : null;
    if (!btn) return;
    const code = btn.parentElement.querySelector("code");
    try {
      await navigator.clipboard.writeText(code ? code.innerText : "");
      btn.textContent = "Copied";
    } catch (_) {
      btn.textContent = "Select and copy";
    }
    setTimeout(() => { btn.textContent = "Copy"; }, 1500);
  });

  // Mobile navigation.
  const menu = document.getElementById("menu");
  if (menu) {
    menu.addEventListener("click", () => {
      const open = document.body.classList.toggle("nav-open");
      menu.setAttribute("aria-expanded", String(open));
    });
  }

  // Table of contents: mark the section being read.
  const tocLinks = [...document.querySelectorAll(".toc a")];
  if (tocLinks.length && "IntersectionObserver" in window) {
    const byId = new Map(tocLinks.map((a) => [a.getAttribute("href").slice(1), a]));
    const io = new IntersectionObserver((entries) => {
      for (const en of entries) {
        if (!en.isIntersecting) continue;
        tocLinks.forEach((a) => a.classList.remove("active"));
        const a = byId.get(en.target.id);
        if (a) a.classList.add("active");
      }
    }, { rootMargin: "-70px 0px -70% 0px" });
    byId.forEach((_, id) => { const h = document.getElementById(id); if (h) io.observe(h); });
  }

  // Search over search.json, loaded on first use.
  const input = document.getElementById("search");
  const box = document.getElementById("search-results");
  if (!input || !box) return;
  const base = input.dataset.base || "";
  let index = null;
  let selected = 0;
  const esc = (s) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

  async function load() {
    if (index) return index;
    try {
      index = await (await fetch(base + "/search.json")).json();
    } catch (_) {
      index = [];
    }
    return index;
  }

  function score(page, terms) {
    let total = 0;
    const title = page.title.toLowerCase();
    const heads = page.headings.map((h) => h.text.toLowerCase());
    const text = page.text.toLowerCase();
    for (const t of terms) {
      let s = 0;
      if (title.includes(t)) s += 10;
      if (heads.some((h) => h.includes(t))) s += 5;
      if (text.includes(t)) s += 1;
      if (!s) return 0;
      total += s;
    }
    return total;
  }

  function snippet(page, terms) {
    const text = page.text;
    const i = text.toLowerCase().indexOf(terms[0]);
    if (i < 0) return text.slice(0, 110);
    const start = Math.max(0, i - 40);
    return (start ? "…" : "") + text.slice(start, start + 120);
  }

  function anchor(page, terms) {
    const h = page.headings.find((h) => terms.every((t) => h.text.toLowerCase().includes(t)));
    return h ? page.url + "#" + h.id : page.url;
  }

  async function run() {
    const q = input.value.trim().toLowerCase();
    if (!q) { box.hidden = true; return; }
    const terms = q.split(/\s+/).filter(Boolean);
    const pages = await load();
    const hits = pages
      .map((p) => [score(p, terms), p])
      .filter(([s]) => s > 0)
      .sort((a, b) => b[0] - a[0])
      .slice(0, 8);
    selected = 0;
    box.innerHTML = hits.length
      ? hits.map(([, p], i) =>
          `<a href="${esc(anchor(p, terms))}" role="option" aria-selected="${i === 0}"><span class="t">${esc(p.title)}</span><span class="s">${esc(p.section ? p.section + " · " : "")}${esc(snippet(p, terms))}</span></a>`
        ).join("")
      : `<div class="none">No pages match “${esc(q)}”.</div>`;
    box.hidden = false;
  }

  input.addEventListener("input", run);
  input.addEventListener("focus", () => { load(); if (input.value) run(); });
  input.addEventListener("keydown", (e) => {
    const items = [...box.querySelectorAll("a")];
    if (e.key === "Escape") { box.hidden = true; input.blur(); return; }
    if (!items.length) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      selected = (selected + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
      items.forEach((a, i) => a.setAttribute("aria-selected", String(i === selected)));
      items[selected].scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter") {
      e.preventDefault();
      location.href = items[selected].href;
    }
  });
  document.addEventListener("click", (e) => {
    if (!(e.target instanceof Node) || !input.parentElement.contains(e.target)) box.hidden = true;
  });
  document.addEventListener("keydown", (e) => {
    const t = e.target;
    const typing = t instanceof HTMLElement && (t.isContentEditable || ["INPUT", "TEXTAREA"].includes(t.tagName));
    if (e.key === "/" && !typing) { e.preventDefault(); input.focus(); }
  });
})();
