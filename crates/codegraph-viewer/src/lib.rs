//! The `codegraph ui` viewer, compiled to WebAssembly. The page loads
//! `viewer.wasm` through a ~150-line JS bridge (`static/viewer.js`) that
//! applies DOM patches, delegates events, and performs fetches; everything
//! else (routing, state, rendering, diffing, highlighting, graph layout)
//! is Rust.

pub mod app;
pub mod flowviews;
pub mod highlight;
pub mod host;
pub mod layout;
pub mod vdom;
pub mod views;

use app::{App, Msg};
use std::cell::RefCell;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the app, then re-render.
fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().as_mut() {
            f(app);
            app.render();
        }
    });
}

fn dispatch(msg: Msg) {
    with_app(|app| app.update(msg));
}

/// Take ownership of a string the bridge wrote into a buffer from `cg_alloc`.
///
/// # Safety
/// `ptr`/`len` must come from `cg_alloc(len)` and be filled with UTF-8.
unsafe fn take_string(ptr: *mut u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    let bytes = Vec::from_raw_parts(ptr, len, len);
    String::from_utf8(bytes).unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn cg_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Start the app. `ptr` holds `{"hash": "...", "storage": {...}}`.
///
/// # Safety
/// See `take_string`.
#[no_mangle]
pub unsafe extern "C" fn cg_start(ptr: *mut u8, len: usize) {
    std::panic::set_hook(Box::new(|info| {
        host::console(&format!("viewer panic: {info}"))
    }));
    let init: serde_json::Value = serde_json::from_str(&take_string(ptr, len)).unwrap_or_default();
    let mut app = App::new(&init["storage"]);
    app.start(app::s(&init, "hash"));
    app.render();
    APP.with(|cell| *cell.borrow_mut() = Some(app));
}

/// A delegated DOM event for handler `handler` of the current render.
///
/// # Safety
/// See `take_string`.
#[no_mangle]
pub unsafe extern "C" fn cg_event(handler: u32, ptr: *mut u8, len: usize) {
    let value = take_string(ptr, len);
    let h = APP.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|a| a.handlers.list.get(handler as usize).cloned())
    });
    if let Some(msg) = h.and_then(|h| h(&value)) {
        dispatch(msg);
    }
}

/// A key pressed outside any text field (`"/"`, `"Meta+k"`, `"Escape"`, …).
///
/// # Safety
/// See `take_string`.
#[no_mangle]
pub unsafe extern "C" fn cg_key(ptr: *mut u8, len: usize) {
    dispatch(Msg::GlobalKey(take_string(ptr, len)));
}

/// The URL hash changed.
///
/// # Safety
/// See `take_string`.
#[no_mangle]
pub unsafe extern "C" fn cg_route(ptr: *mut u8, len: usize) {
    dispatch(Msg::Route(take_string(ptr, len)));
}

/// A fetch finished (`status` 0 when the request itself failed).
///
/// # Safety
/// See `take_string`.
#[no_mangle]
pub unsafe extern "C" fn cg_fetched(id: u32, status: u32, ptr: *mut u8, len: usize) {
    let body = take_string(ptr, len);
    with_app(|app| {
        if let Some(msg) = app.fetched(id, status, &body) {
            app.update(msg);
        }
    });
}

#[no_mangle]
pub extern "C" fn cg_timer(id: u32) {
    dispatch(Msg::Timer(id));
}

/// A message from the page for the user (export results).
///
/// # Safety
/// See `take_string`.
#[no_mangle]
pub unsafe extern "C" fn cg_notice(ptr: *mut u8, len: usize) {
    dispatch(Msg::Notice(take_string(ptr, len)));
}
