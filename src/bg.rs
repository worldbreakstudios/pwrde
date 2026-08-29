//! Off-thread work behind one seam. Native spawns a std thread; wasm32 has no
//! threads (`std::thread::spawn` panics there), so the job is dropped and the
//! `TermEvent` it would have sent never arrives — every caller already
//! tolerates a worker that has not answered yet (cards stay bare, a panel
//! stays on its loading state), which is the right shape for a web build fed
//! by fixtures rather than shell-outs.

/// Run `job` off the UI thread (native) or not at all (wasm32).
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
    }
}
