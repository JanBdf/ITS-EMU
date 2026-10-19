//! Display rendering on its own thread: the emulator thread only copies the
//! panel's frame buffer ([`DisplayFrame`]); the colours of the 480 × 320
//! pixels are computed here, so that the emulator keeps running meanwhile.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::thread::JoinHandle;

use itsboard::{DisplayFrame, SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::emu::SharedSnapshot;

struct Job {
    frame: DisplayFrame,
    /// Ideal colours instead of the panel's look.
    ideal: bool,
}

pub struct Renderer {
    jobs: Option<SyncSender<Job>>,
    /// Frames the thread has rendered, for reuse.
    done: Receiver<DisplayFrame>,
    spare: Option<DisplayFrame>,
    thread: Option<JoinHandle<()>>,
}

impl Renderer {
    /// Starts the thread. It puts each frame into `snapshot` and calls `repaint`.
    pub fn spawn(snapshot: SharedSnapshot, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        // One frame waiting while one is rendered; newer ones wait in the board.
        let (jobs, rx) = sync_channel::<Job>(1);
        let (done_tx, done) = channel();
        let thread = std::thread::Builder::new()
            .name("display".into())
            .spawn(move || render_loop(rx, done_tx, snapshot, repaint))
            .expect("spawn display thread");
        Self { jobs: Some(jobs), done, spare: None, thread: Some(thread) }
    }

    /// A frame to capture the display into (reused when possible).
    pub fn frame(&mut self) -> DisplayFrame {
        self.spare.take().or_else(|| self.done.try_recv().ok()).unwrap_or_default()
    }

    /// Hands a captured frame to the thread. False if it is still busy with
    /// the previous ones: the frame is dropped, capture again later.
    pub fn submit(&mut self, frame: DisplayFrame, ideal: bool) -> bool {
        let Some(jobs) = &self.jobs else { return false };
        match jobs.try_send(Job { frame, ideal }) {
            Ok(()) => true,
            Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => {
                self.spare = Some(job.frame);
                false
            }
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // closing the channel ends the thread
        self.jobs = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn render_loop(
    jobs: Receiver<Job>,
    done: Sender<DisplayFrame>,
    snapshot: SharedSnapshot,
    repaint: Arc<dyn Fn() + Send + Sync>,
) {
    let mut rgba = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
    for job in jobs {
        if job.ideal {
            job.frame.render(&mut rgba);
        } else {
            job.frame.render_as_seen(&mut rgba);
        }
        {
            let mut snap = snapshot.lock().unwrap();
            std::mem::swap(&mut snap.display, &mut rgba);
            snap.display_generation = snap.display_generation.wrapping_add(1);
        }
        repaint();
        let _ = done.send(job.frame);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn frames_rendered_on_the_thread_match_the_board() {
        let fw = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/ac6/TestNewFont.axf"))
            .expect("fixture");
        let mut sim = itsboard::Simulator::new(&fw, Some(itsboard::sim::DEFAULT_FONTS)).unwrap();
        sim.run_for_ns(3_000_000_000);
        let snapshot = SharedSnapshot::default();
        let repaints = Arc::new(AtomicUsize::new(0));
        let counter = repaints.clone();
        let mut renderer = Renderer::spawn(
            snapshot.clone(),
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        for (round, ideal) in [false, true].into_iter().enumerate() {
            let mut frame = renderer.frame();
            sim.board.capture_display(&mut frame);
            assert!(renderer.submit(frame, ideal));
            let start = Instant::now();
            while repaints.load(Ordering::SeqCst) <= round {
                assert!(start.elapsed() < Duration::from_secs(10), "no frame");
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut want = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
            if ideal {
                sim.board.render_display(&mut want);
            } else {
                sim.board.render_display_as_seen(&mut want);
            }
            let snap: &Mutex<_> = &snapshot;
            let snap = snap.lock().unwrap();
            assert_eq!(snap.display_generation, round as u64 + 1);
            assert!(snap.display == want, "frame {round} differs");
            assert!(want.chunks(4).any(|p| p[..3] != want[..3]), "blank display");
        }
    }
}
