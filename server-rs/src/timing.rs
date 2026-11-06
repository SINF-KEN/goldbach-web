//! Per request thread: time waiting for the store lock, in SQL, rebuilding,
//! serialising (the Server-Timing header). Off the request path (the snapshot
//! thread) the timings go nowhere.
use std::cell::RefCell;
use std::time::Instant;

thread_local! {
    static T: RefCell<Option<(Instant, Vec<(&'static str, f64)>)>> = const { RefCell::new(None) };
}

pub fn timed(key: &'static str, dt: f64) {
    T.with(|t| {
        if let Some((_, d)) = t.borrow_mut().as_mut() {
            match d.iter_mut().find(|e| e.0 == key) {
                Some(e) => e.1 += dt,
                None => d.push((key, dt)),
            }
        }
    })
}

pub fn start() {
    T.with(|t| *t.borrow_mut() = Some((Instant::now(), Vec::new())));
}

/// The header value (with the total so far), or None outside a request.
pub fn header() -> Option<String> {
    T.with(|t| {
        t.borrow().as_ref().map(|(t0, d)| {
            let mut parts: Vec<String> = d.iter().map(|(k, v)| format!("{k};dur={:.2}", v * 1000.0)).collect();
            parts.push(format!("total;dur={:.2}", t0.elapsed().as_secs_f64() * 1000.0));
            parts.join(", ")
        })
    })
}

pub fn stop() {
    T.with(|t| *t.borrow_mut() = None);
}
