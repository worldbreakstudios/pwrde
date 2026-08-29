//! Off-thread work behind one seam. Native spawns a std thread; wasm32 has no
//! threads (`std::thread::spawn` panics there), so the job is dropped and the
//! `TermEvent` it would have sent never arrives — every caller already
//! tolerates a worker that has not answered yet (cards stay bare, a panel
//! stays on its loading state).
//!
//! On wasm32 a fixture can stand in for the workers: [`answer_requests_with`]
//! registers the events the shell-outs would have produced (a PR list, a
//! cleanup scan), and every dropped job re-sends them — so a page that asks
//! again (entering Cleanup rescans, Refresh re-lists) gets its answer again.
//! The events are idempotent state updates, so answering every request with
//! all of them is harmless.

use std::sync::mpsc::Sender;

use crate::term::TermEvent;

#[cfg(target_family = "wasm")]
thread_local! {
    static CANNED: std::cell::RefCell<Option<(Sender<TermEvent>, Vec<TermEvent>)>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `job` off the UI thread (native) or answer it from the canned events
/// instead (wasm32).
pub fn spawn<F>(job: F)
where
    F: FnOnce() + Send + 'static,
{
    #[cfg(not(target_family = "wasm"))]
    {
        std::thread::spawn(job);
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = job;
        CANNED.with(|canned| {
            if let Some((tx, events)) = &*canned.borrow() {
                for event in events {
                    let _ = tx.send(event.clone());
                }
            }
        });
    }
}

/// wasm32: from now on every request answers with `events`, sent over `tx`
/// (the app's wakeup channel). Also sends them once right away, so surfaces
/// that never ask — the sidebar's PR tool, say — are populated too.
#[allow(unused_variables)]
pub fn answer_requests_with(tx: Sender<TermEvent>, events: Vec<TermEvent>) {
    #[cfg(target_family = "wasm")]
    {
        for event in &events {
            let _ = tx.send(event.clone());
        }
        CANNED.with(|canned| *canned.borrow_mut() = Some((tx, events)));
    }
}
