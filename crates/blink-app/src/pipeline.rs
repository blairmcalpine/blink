//! The save pipeline: a cutter thread that copies each clip out of the ring
//! as soon as its settle delay has passed, and a writer thread that writes
//! the finished buffers.
//!
//! Splitting the stages keeps copying from ever waiting on the disk. The ring
//! keeps only a few seconds beyond the clip length, so a save queued behind a
//! slow write would otherwise lose the start of its clip to eviction.
//!
//! Memory: one [`ClipBuf`] (about one clip of encoded media) is kept between
//! saves, in a single spare slot. A second exists only while one save is
//! being written and the next is cut; the writer frees whichever buffer finds
//! the slot full, so a burst of saves costs a transient allocation rather
//! than doubling the resident size for good.

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use blink_ring::ClipBuf;
use blink_ring::media::Timestamp;
use parking_lot::Mutex;

use crate::cut::{CutError, cut};
use crate::shared::SharedRing;

/// One save: the clip ending at `now`, about `duration` long, and what the
/// writer needs to know about it.
pub(crate) struct Job<P> {
    /// The save time on the shared clock; the clip ends here.
    pub(crate) now: Timestamp,
    /// Requested clip length.
    pub(crate) duration: Timestamp,
    /// Passed through to the writer.
    pub(crate) payload: P,
}

/// A job once cut, on its way to the writer.
struct Cut<P> {
    job: Job<P>,
    clip: ClipBuf,
    result: Result<(), CutError>,
}

/// The one buffer kept between saves.
type Spare = Arc<Mutex<Option<ClipBuf>>>;

/// The two save threads. Dropping it lets queued jobs finish, then joins.
pub(crate) struct SavePipeline<P> {
    jobs: Option<Sender<Job<P>>>,
    #[cfg(test)]
    spare: Spare,
    cutter: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl<P: Send + 'static> SavePipeline<P> {
    /// Starts the cutter, which runs `cutter_init` and then for each job
    /// calls `settle(now)` (to wait for the encoder to drain) and cuts the
    /// clip; and the writer, which keeps the value `writer_init` returns
    /// (such as a thread-priority guard) for its lifetime and calls `write`
    /// for every job in order, with the clip or the reason it could not be
    /// cut.
    pub(crate) fn spawn<S, C, I, G, W>(
        ring: Arc<SharedRing>,
        settle: S,
        cutter_init: C,
        writer_init: I,
        mut write: W,
    ) -> io::Result<Self>
    where
        S: Fn(Timestamp) + Send + 'static,
        C: FnOnce() + Send + 'static,
        I: FnOnce() -> G + Send + 'static,
        W: FnMut(Job<P>, Result<&ClipBuf, CutError>) + Send + 'static,
    {
        let (jobs, job_queue) = mpsc::channel::<Job<P>>();
        let (cuts, cut_queue) = mpsc::channel::<Cut<P>>();
        let spare = Spare::default();
        let writer_spare = Arc::clone(&spare);
        let cutter_spare = Arc::clone(&spare);
        let writer = thread::Builder::new()
            .name("blink-write".to_owned())
            .spawn(move || {
                let _guard = writer_init();
                for Cut { job, clip, result } in cut_queue {
                    write(job, result.map(|()| &clip));
                    give_back(&writer_spare, clip);
                }
            })?;
        let cutter = thread::Builder::new()
            .name("blink-save".to_owned())
            .spawn(move || {
                cutter_init();
                run_cutter(&ring, &settle, &job_queue, &cutter_spare, &cuts);
            })?;
        Ok(Self {
            jobs: Some(jobs),
            #[cfg(test)]
            spare,
            cutter: Some(cutter),
            writer: Some(writer),
        })
    }

    /// Queues a job. Returns `false` if the threads are gone.
    pub(crate) fn submit(&self, job: Job<P>) -> bool {
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok())
    }

    /// Whether both threads are still running; `false` once either has
    /// panicked or exited.
    pub(crate) fn is_running(&self) -> bool {
        [&self.cutter, &self.writer]
            .into_iter()
            .all(|thread| thread.as_ref().is_some_and(|thread| !thread.is_finished()))
    }
}

/// Keeps `clip` as the spare if the slot is empty; otherwise frees it,
/// outside the lock.
fn give_back(spare: &Mutex<Option<ClipBuf>>, clip: ClipBuf) {
    let mut slot = spare.lock();
    let extra = if slot.is_none() {
        *slot = Some(clip);
        None
    } else {
        Some(clip)
    };
    drop(slot);
    drop(extra);
}

impl<P> SavePipeline<P> {
    /// Lets queued jobs finish, then joins both threads.
    pub(crate) fn shutdown(&mut self) {
        self.jobs = None;
        for thread in [self.cutter.take(), self.writer.take()]
            .into_iter()
            .flatten()
        {
            if thread.join().is_err() {
                tracing::error!("a save thread panicked");
            }
        }
    }
}

impl<P> Drop for SavePipeline<P> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_cutter<P>(
    ring: &SharedRing,
    settle: &dyn Fn(Timestamp),
    jobs: &Receiver<Job<P>>,
    spare: &Mutex<Option<ClipBuf>>,
    cuts: &Sender<Cut<P>>,
) {
    for job in jobs {
        // Reused across saves, so after the largest clip it stops allocating.
        let mut clip = spare.lock().take().unwrap_or_default();
        settle(job.now);
        let result = cut(ring, &mut clip, job.now, job.duration);
        if cuts.send(Cut { job, clip, result }).is_err() {
            tracing::error!("the clip writer is gone; cannot save clips");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cut::tests::{CLIP_SECS, NOW_MS, Source, config};
    use crate::mapping::RingSize;
    use crate::shared::tests::MS;

    /// Two presses 100 ms apart, and a first write that is slow while
    /// capture keeps pushing: the second clip must still be cut in full,
    /// because cutting does not wait for the first write.
    ///
    /// A third job's settle callback runs only once the second clip is cut,
    /// and the slow writer waits for it before pushing more media. If cutting
    /// waited on writes, that signal would never come in time: the writer
    /// gives up waiting, pushes, and the second clip is cut short.
    #[test]
    fn a_queued_save_is_cut_before_a_slow_write_finishes() {
        const SENTINEL: Timestamp = Timestamp::from_ticks(i64::MAX);
        let ring = Arc::new(SharedRing::new(RingSize::for_config(&config())));
        let mut source = Source::new();
        source.run_until(&ring, NOW_MS + 400);
        let (results, received) = mpsc::channel();
        let (second_cut, second_was_cut) = mpsc::channel();
        let writer_ring = Arc::clone(&ring);
        let mut pipeline = SavePipeline::spawn(
            Arc::clone(&ring),
            move |now| {
                if now == SENTINEL {
                    let _ = second_cut.send(());
                }
            },
            || {},
            || (),
            move |job: Job<u32>, clip: Result<&ClipBuf, CutError>| {
                results
                    .send((job.payload, clip.map(ClipBuf::duration)))
                    .unwrap();
                if job.payload == 1 {
                    // A slow disk: 6 s of media arrive while writing, which
                    // evicts the start of a clip that has not been cut yet.
                    let _ = second_was_cut.recv_timeout(Duration::from_secs(5));
                    source.run_until(&writer_ring, NOW_MS + 6_000);
                }
            },
        )
        .unwrap();
        let clip = Timestamp::from_secs(CLIP_SECS.into());
        for (payload, now) in [
            (1, Timestamp::from_ticks(NOW_MS * MS)),
            (2, Timestamp::from_ticks((NOW_MS + 100) * MS)),
            (3, SENTINEL),
        ] {
            assert!(pipeline.submit(Job {
                now,
                duration: clip,
                payload,
            }));
        }
        pipeline.shutdown();
        let results: Vec<_> = received.iter().collect();
        assert_eq!(results.len(), 3);
        for (payload, duration) in results.into_iter().take(2) {
            let duration = duration.unwrap();
            assert!(duration >= clip, "clip {payload} is only {duration:?}");
        }
        // Up to two buffers were in flight; one is kept.
        assert!(
            pipeline
                .spare
                .lock()
                .as_ref()
                .is_some_and(|clip| clip.data_capacity() > 0)
        );
    }

    #[test]
    fn failed_cuts_reach_the_writer() {
        let ring = Arc::new(SharedRing::new(RingSize::for_config(&config())));
        let (results, received) = mpsc::channel();
        let mut pipeline = SavePipeline::spawn(
            ring,
            |_now| {},
            || {},
            || (),
            move |job: Job<u32>, clip: Result<&ClipBuf, CutError>| {
                results.send((job.payload, clip.err())).unwrap();
            },
        )
        .unwrap();
        for payload in 0..3 {
            assert!(pipeline.submit(Job {
                now: Timestamp::from_ticks(MS),
                duration: Timestamp::from_secs(5),
                payload,
            }));
        }
        assert!(pipeline.is_running());
        pipeline.shutdown();
        assert!(!pipeline.is_running());
        let results: Vec<_> = received.iter().collect();
        assert_eq!(
            results,
            [0, 1, 2].map(|payload| (payload, Some(CutError::NothingYet)))
        );
        assert!(!pipeline.submit(Job {
            now: Timestamp::from_ticks(MS),
            duration: Timestamp::from_secs(5),
            payload: 3,
        }));
    }

    #[test]
    fn the_spare_buffer_is_reused_and_kept() {
        // Larger than these cuts ask for, so capacity shows reuse.
        const MARK: usize = 8 << 20;
        let ring = Arc::new(SharedRing::new(RingSize::for_config(&config())));
        Source::new().run_until(&ring, NOW_MS);
        let (results, received) = mpsc::channel();
        let (written, wait_for_write) = mpsc::channel();
        let mut pipeline = SavePipeline::spawn(
            ring,
            |_now| {},
            || {},
            || (),
            move |job: Job<u32>, clip: Result<&ClipBuf, CutError>| {
                let capacity = clip.map(ClipBuf::data_capacity);
                results.send((job.payload, capacity)).unwrap();
                written.send(()).unwrap();
            },
        )
        .unwrap();
        *pipeline.spare.lock() = Some(ClipBuf::with_capacity(MARK, 0));
        for payload in 0..2 {
            assert!(pipeline.submit(Job {
                now: Timestamp::from_ticks(NOW_MS * MS),
                duration: Timestamp::from_secs(CLIP_SECS.into()),
                payload,
            }));
            // One at a time, so each cut finds the buffer the last write
            // gave back.
            wait_for_write.recv().unwrap();
            let returned = (0..500).any(|_| {
                thread::sleep(Duration::from_millis(10));
                pipeline.spare.lock().is_some()
            });
            assert!(returned, "the writer gave the buffer back");
        }
        pipeline.shutdown();
        let results: Vec<_> = received.iter().collect();
        for (payload, capacity) in results {
            assert!(
                capacity.unwrap() >= MARK,
                "save {payload} got a fresh buffer: {capacity:?}"
            );
        }
        assert!(
            pipeline
                .spare
                .lock()
                .as_ref()
                .is_some_and(|clip| clip.data_capacity() >= MARK)
        );
    }

    #[test]
    fn only_one_spare_is_kept() {
        let spare = Mutex::new(None);
        give_back(&spare, ClipBuf::with_capacity(1 << 10, 0));
        give_back(&spare, ClipBuf::with_capacity(1 << 20, 0));
        assert_eq!(
            spare.lock().take().map(|clip| clip.data_capacity()),
            Some(1 << 10),
            "the second buffer was freed, not swapped in"
        );
    }
}
