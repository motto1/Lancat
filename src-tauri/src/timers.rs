//! One thread for every delayed action of the shell (hover delay, collapse fallback, suspend,
//! destroy after the keep-alive, reveal timeout, fade steps …). Each of them used to spawn its own
//! OS thread that slept, some of them for 30 minutes (one per collapse), and the window fades one
//! thread per fade. Jobs run on the timer thread and must stay short: they post to the main
//! thread (`run_on_main_thread`) or flip a flag.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Entry {
    at: Instant,
    seq: u64,
    job: Job,
}

impl PartialEq for Entry {
    fn eq(&self, o: &Self) -> bool {
        self.at == o.at && self.seq == o.seq
    }
}
impl Eq for Entry {}
impl PartialOrd for Entry {
    fn partial_cmp(&self, o: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(o))
    }
}
impl Ord for Entry {
    // BinaryHeap is a max-heap: the earliest deadline (then the oldest entry) is the "largest"
    fn cmp(&self, o: &Self) -> CmpOrdering {
        o.at.cmp(&self.at).then(o.seq.cmp(&self.seq))
    }
}

struct Queue {
    heap: Mutex<(BinaryHeap<Entry>, u64)>,
    cv: Condvar,
}

fn queue() -> &'static Queue {
    static Q: OnceLock<Queue> = OnceLock::new();
    Q.get_or_init(|| {
        std::thread::Builder::new()
            .name("lancat-timers".into())
            .spawn(run)
            .expect("spawn the timer thread");
        Queue { heap: Mutex::new((BinaryHeap::new(), 0)), cv: Condvar::new() }
    })
}

fn run() {
    let q = queue();
    let mut g = q.heap.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let now = Instant::now();
        match g.0.peek().map(|e| e.at) {
            Some(at) if at <= now => {
                let e = g.0.pop().expect("peeked");
                drop(g);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(e.job)).is_err() {
                    log::warn!("a timer job panicked");
                }
                g = q.heap.lock().unwrap_or_else(|e| e.into_inner());
            }
            Some(at) => g = q.cv.wait_timeout(g, at - now).unwrap_or_else(|e| e.into_inner()).0,
            None => g = q.cv.wait(g).unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// Runs `f` on the timer thread after `d`.
pub fn after(d: Duration, f: impl FnOnce() + Send + 'static) {
    let q = queue();
    let mut g = q.heap.lock().unwrap_or_else(|e| e.into_inner());
    g.1 += 1;
    let seq = g.1;
    g.0.push(Entry { at: Instant::now() + d, seq, job: Box::new(f) });
    q.cv.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn runs_in_deadline_order() {
        let (tx, rx) = mpsc::channel();
        for (ms, n) in [(60u64, 3), (20, 1), (40, 2), (20, 11)] {
            let tx = tx.clone();
            after(Duration::from_millis(ms), move || tx.send(n).unwrap());
        }
        let got: Vec<i32> = (0..4).map(|_| rx.recv_timeout(Duration::from_secs(2)).unwrap()).collect();
        assert_eq!(got, vec![1, 11, 2, 3]);
    }
}
