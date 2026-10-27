//! A small virtual DOM. Views build a `Node` tree each render; `diff`
//! compares it with what is mounted and writes the minimal patch as a JSON
//! op list the JS bridge applies:
//!
//! - `["e", id, tag, svg]` create an element (`svg` 1 for the SVG namespace)
//! - `["t", id, text]` create a text node, `["x", id, text]` change its text
//! - `["s", id, name, value]` / `["r", id, name]` set / remove an attribute
//! - `["h", id, html]` set inner HTML (pre-escaped, used for code blocks)
//! - `["i", parent, child, before]` insert (`before` 0 appends)
//! - `["d", id]` remove a node and forget its subtree
//! - `["f", id]` focus, `["v", dom_id]` scroll the element with that id into view
//!
//! Events are delegated: an element carries `data-on-<event>="<n>"` and the
//! bridge calls back with handler `n` of the current render.

use std::rc::Rc;

pub type Handler<M> = Rc<dyn Fn(&str) -> Option<M>>;

pub enum Node<M> {
    El(El<M>),
    Text(String),
}

pub struct El<M> {
    pub tag: &'static str,
    pub svg: bool,
    pub attrs: Vec<(&'static str, String)>,
    pub events: Vec<(&'static str, Handler<M>)>,
    pub children: Vec<Node<M>>,
    pub html: Option<String>,
    /// Focus this element after the patch.
    pub focus: bool,
}

pub fn el<M>(tag: &'static str) -> El<M> {
    El {
        tag,
        svg: false,
        attrs: Vec::new(),
        events: Vec::new(),
        children: Vec::new(),
        html: None,
        focus: false,
    }
}

pub fn svg<M>(tag: &'static str) -> El<M> {
    El {
        svg: true,
        ..el(tag)
    }
}

pub fn text<M>(t: impl Into<String>) -> Node<M> {
    Node::Text(t.into())
}

impl<M: 'static> El<M> {
    pub fn attr(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.attrs.push((name, value.into()));
        self
    }

    pub fn attr_if(self, cond: bool, name: &'static str, value: impl Into<String>) -> Self {
        if cond {
            self.attr(name, value)
        } else {
            self
        }
    }

    pub fn class(self, c: impl Into<String>) -> Self {
        self.attr("class", c)
    }

    pub fn href(self, h: impl Into<String>) -> Self {
        self.attr("href", h)
    }

    pub fn child(mut self, n: impl Into<Node<M>>) -> Self {
        self.children.push(n.into());
        self
    }

    pub fn children(mut self, ns: impl IntoIterator<Item = Node<M>>) -> Self {
        self.children.extend(ns);
        self
    }

    pub fn opt(mut self, n: Option<Node<M>>) -> Self {
        if let Some(n) = n {
            self.children.push(n);
        }
        self
    }

    pub fn text(self, t: impl Into<String>) -> Self {
        self.child(Node::Text(t.into()))
    }

    /// Trusted, already-escaped markup as the element's content.
    pub fn html(mut self, h: String) -> Self {
        self.html = Some(h);
        self
    }

    pub fn focus(mut self) -> Self {
        self.focus = true;
        self
    }

    pub fn on(mut self, event: &'static str, f: impl Fn(&str) -> Option<M> + 'static) -> Self {
        self.events.push((event, Rc::new(f)));
        self
    }

    pub fn on_click(self, msg: M) -> Self
    where
        M: Clone,
    {
        self.on("click", move |_| Some(msg.clone()))
    }

    pub fn on_input(self, f: impl Fn(String) -> M + 'static) -> Self {
        self.on("input", move |v| Some(f(v.to_string())))
    }
}

impl<M> From<El<M>> for Node<M> {
    fn from(e: El<M>) -> Self {
        Node::El(e)
    }
}

/// What is mounted in the page, mirrored for diffing.
#[derive(Debug, Clone, PartialEq)]
pub enum Mounted {
    El {
        id: u32,
        tag: &'static str,
        svg: bool,
        attrs: Vec<(&'static str, String)>,
        html: Option<String>,
        children: Vec<Mounted>,
    },
    Text {
        id: u32,
        text: String,
    },
}

impl Mounted {
    fn id(&self) -> u32 {
        match self {
            Mounted::El { id, .. } | Mounted::Text { id, .. } => *id,
        }
    }
}

/// Op-list writer (a JSON array of arrays).
pub struct Ops {
    buf: String,
    next_id: u32,
}

pub enum Arg<'a> {
    S(&'a str),
    N(u32),
}

fn escape_into(buf: &mut String, s: &str) {
    buf.push('"');
    for c in s.chars() {
        match c {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                buf.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => buf.push(c),
        }
    }
    buf.push('"');
}

impl Ops {
    pub fn new(next_id: u32) -> Self {
        Ops {
            buf: String::from("["),
            next_id,
        }
    }

    pub fn op(&mut self, code: &str, args: &[Arg<'_>]) {
        if self.buf.len() > 1 {
            self.buf.push(',');
        }
        self.buf.push('[');
        escape_into(&mut self.buf, code);
        for a in args {
            self.buf.push(',');
            match a {
                Arg::S(s) => escape_into(&mut self.buf, s),
                Arg::N(n) => self.buf.push_str(&n.to_string()),
            }
        }
        self.buf.push(']');
    }

    pub fn is_empty(&self) -> bool {
        self.buf.len() <= 1
    }

    pub fn finish(mut self) -> (String, u32) {
        self.buf.push(']');
        (self.buf, self.next_id)
    }

    fn fresh(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }
}

/// Handlers registered by the current render, indexed by `data-on-*`.
pub struct Handlers<M> {
    pub list: Vec<Handler<M>>,
}

impl<M> Default for Handlers<M> {
    fn default() -> Self {
        Handlers { list: Vec::new() }
    }
}

fn final_attrs<M>(e: &El<M>, handlers: &mut Handlers<M>) -> Vec<(&'static str, String)> {
    let mut attrs = e.attrs.clone();
    for (ev, h) in &e.events {
        let name: &'static str = match *ev {
            "click" => "data-on-click",
            "input" => "data-on-input",
            "keydown" => "data-on-keydown",
            "submit" => "data-on-submit",
            "change" => "data-on-change",
            "mouseover" => "data-on-mouseover",
            _ => continue,
        };
        handlers.list.push(h.clone());
        attrs.push((name, (handlers.list.len() - 1).to_string()));
    }
    attrs
}

/// Create `node` (and its subtree) and insert it under `parent`.
fn create<M>(
    node: &Node<M>,
    parent: u32,
    before: u32,
    ops: &mut Ops,
    handlers: &mut Handlers<M>,
    focus: &mut Option<u32>,
) -> Mounted {
    match node {
        Node::Text(t) => {
            let id = ops.fresh();
            ops.op("t", &[Arg::N(id), Arg::S(t)]);
            ops.op("i", &[Arg::N(parent), Arg::N(id), Arg::N(before)]);
            Mounted::Text {
                id,
                text: t.clone(),
            }
        }
        Node::El(e) => {
            let id = ops.fresh();
            ops.op("e", &[Arg::N(id), Arg::S(e.tag), Arg::N(u32::from(e.svg))]);
            let attrs = final_attrs(e, handlers);
            for (k, v) in &attrs {
                ops.op("s", &[Arg::N(id), Arg::S(k), Arg::S(v)]);
            }
            let mut children = Vec::new();
            if let Some(h) = &e.html {
                ops.op("h", &[Arg::N(id), Arg::S(h)]);
            } else {
                for c in &e.children {
                    children.push(create(c, id, 0, ops, handlers, focus));
                }
            }
            // Insert after the subtree is built: one reflow per new subtree.
            ops.op("i", &[Arg::N(parent), Arg::N(id), Arg::N(before)]);
            if e.focus {
                *focus = Some(id);
            }
            Mounted::El {
                id,
                tag: e.tag,
                svg: e.svg,
                attrs,
                html: e.html.clone(),
                children,
            }
        }
    }
}

/// Patch `old` (mounted under `parent`) into `new`.
fn patch<M>(
    old: &mut Mounted,
    new: &Node<M>,
    parent: u32,
    ops: &mut Ops,
    handlers: &mut Handlers<M>,
    focus: &mut Option<u32>,
) {
    match (&mut *old, new) {
        (Mounted::Text { id, text }, Node::Text(t)) => {
            if text != t {
                ops.op("x", &[Arg::N(*id), Arg::S(t)]);
                *text = t.clone();
            }
        }
        (
            Mounted::El {
                id,
                tag,
                svg,
                attrs,
                html,
                children,
            },
            Node::El(e),
        ) if *tag == e.tag && *svg == e.svg && html.is_some() == e.html.is_some() => {
            let id = *id;
            let new_attrs = final_attrs(e, handlers);
            for (k, v) in &new_attrs {
                if !attrs.iter().any(|(k2, v2)| k2 == k && v2 == v) {
                    ops.op("s", &[Arg::N(id), Arg::S(k), Arg::S(v)]);
                }
            }
            for (k, _) in attrs.iter() {
                if !new_attrs.iter().any(|(k2, _)| k2 == k) {
                    ops.op("r", &[Arg::N(id), Arg::S(k)]);
                }
            }
            *attrs = new_attrs;
            if let Some(h) = &e.html {
                if html.as_ref() != Some(h) {
                    ops.op("h", &[Arg::N(id), Arg::S(h)]);
                    *html = Some(h.clone());
                }
            } else {
                diff_children(children, &e.children, id, ops, handlers, focus);
            }
            if e.focus {
                *focus = Some(id);
            }
        }
        _ => {
            // Different shape: replace in place.
            let old_id = old.id();
            let fresh = create(new, parent, old_id, ops, handlers, focus);
            ops.op("d", &[Arg::N(old_id)]);
            *old = fresh;
        }
    }
}

fn diff_children<M>(
    old: &mut Vec<Mounted>,
    new: &[Node<M>],
    parent: u32,
    ops: &mut Ops,
    handlers: &mut Handlers<M>,
    focus: &mut Option<u32>,
) {
    let common = old.len().min(new.len());
    for i in 0..common {
        patch(&mut old[i], &new[i], parent, ops, handlers, focus);
    }
    for gone in old.drain(common..) {
        ops.op("d", &[Arg::N(gone.id())]);
    }
    for n in &new[common..] {
        let m = create(n, parent, 0, ops, handlers, focus);
        old.push(m);
    }
}

/// Patch the root's children (`mounted`) into `new`; returns the op list,
/// the handlers of this render, and the next free node id.
pub fn render<M>(
    mounted: &mut Vec<Mounted>,
    new: &[Node<M>],
    next_id: u32,
    scroll_to: Option<&str>,
) -> (String, Handlers<M>, u32) {
    let mut ops = Ops::new(next_id);
    let mut handlers = Handlers::default();
    let mut focus = None;
    diff_children(mounted, new, 0, &mut ops, &mut handlers, &mut focus);
    if let Some(f) = focus {
        ops.op("f", &[Arg::N(f)]);
    }
    if let Some(target) = scroll_to {
        ops.op("v", &[Arg::S(target)]);
    }
    let (s, next) = ops.finish();
    (s, handlers, next)
}

/// Escape text for HTML content and attribute values.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    enum M {
        A,
    }

    fn tree(label: &str, items: usize) -> Vec<Node<M>> {
        vec![el("div")
            .class("box")
            .child(el("h1").text(label))
            .children((0..items).map(|i| el("p").on_click(M::A).text(format!("item {i}")).into()))
            .into()]
    }

    #[test]
    fn diff_emits_minimal_patches() {
        let mut mounted = Vec::new();
        let (ops, h, next) = render(&mut mounted, &tree("one", 2), 0, None);
        assert!(
            ops.starts_with(r#"[["e",1,"div",0],["s",1,"class","box"]"#),
            "{ops}"
        );
        assert_eq!(h.list.len(), 2);
        // Same tree: nothing to do.
        let (ops, _, next) = render(&mut mounted, &tree("one", 2), next, None);
        assert_eq!(ops, "[]");
        // Text change and one item fewer.
        let (ops, _, _) = render(&mut mounted, &tree("two", 1), next, None);
        assert_eq!(ops, r#"[["x",3,"two"],["d",6]]"#);
    }

    #[test]
    fn json_escaping() {
        let mut o = Ops::new(0);
        o.op("t", &[Arg::N(1), Arg::S("a\"b\\c\n\u{1}")]);
        assert_eq!(o.finish().0, r#"[["t",1,"a\"b\\c\n\u0001"]]"#);
        assert_eq!(
            escape_html("<a href=\"x\">&"),
            "&lt;a href=&quot;x&quot;&gt;&amp;"
        );
    }
}
