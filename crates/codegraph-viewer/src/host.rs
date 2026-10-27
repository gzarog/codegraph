//! The bridge to the page. On wasm32 these are imports provided by
//! `static/viewer.js`; elsewhere they record calls so the app logic can be
//! tested natively.

#[cfg(target_arch = "wasm32")]
mod imp {
    #[link(wasm_import_module = "host")]
    extern "C" {
        fn apply(ptr: *const u8, len: usize);
        fn fetch(
            id: u32,
            method: *const u8,
            method_len: usize,
            url: *const u8,
            url_len: usize,
            body: *const u8,
            body_len: usize,
        );
        fn set_hash(ptr: *const u8, len: usize);
        fn set_title(ptr: *const u8, len: usize);
        fn storage_set(key: *const u8, key_len: usize, value: *const u8, value_len: usize);
        fn timer(id: u32, ms: u32);
        fn log(ptr: *const u8, len: usize);
    }

    pub fn apply_ops(ops: &str) {
        unsafe { apply(ops.as_ptr(), ops.len()) }
    }
    pub fn start_fetch(id: u32, method: &str, url: &str, body: &str) {
        unsafe {
            fetch(
                id,
                method.as_ptr(),
                method.len(),
                url.as_ptr(),
                url.len(),
                body.as_ptr(),
                body.len(),
            )
        }
    }
    pub fn go(hash: &str) {
        unsafe { set_hash(hash.as_ptr(), hash.len()) }
    }
    pub fn title(t: &str) {
        unsafe { set_title(t.as_ptr(), t.len()) }
    }
    pub fn store(key: &str, value: &str) {
        unsafe { storage_set(key.as_ptr(), key.len(), value.as_ptr(), value.len()) }
    }
    pub fn after(id: u32, ms: u32) {
        unsafe { timer(id, ms) }
    }
    pub fn console(msg: &str) {
        unsafe { log(msg.as_ptr(), msg.len()) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::cell::RefCell;

    thread_local! {
        /// Every host call, for tests: `("apply", ops)`, `("fetch", "GET url")`, …
        pub static CALLS: RefCell<Vec<(&'static str, String)>> = const { RefCell::new(Vec::new()) };
    }

    fn record(kind: &'static str, value: String) {
        CALLS.with(|c| c.borrow_mut().push((kind, value)));
    }

    pub fn apply_ops(ops: &str) {
        record("apply", ops.to_string())
    }
    pub fn start_fetch(id: u32, method: &str, url: &str, _body: &str) {
        record("fetch", format!("{id} {method} {url}"))
    }
    pub fn go(hash: &str) {
        record("hash", hash.to_string())
    }
    pub fn title(t: &str) {
        record("title", t.to_string())
    }
    pub fn store(key: &str, value: &str) {
        record("store", format!("{key}={value}"))
    }
    pub fn after(id: u32, ms: u32) {
        record("timer", format!("{id} {ms}"))
    }
    pub fn console(msg: &str) {
        record("log", msg.to_string())
    }

    /// Take the recorded calls.
    pub fn take() -> Vec<(&'static str, String)> {
        CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }
}

pub use imp::*;
