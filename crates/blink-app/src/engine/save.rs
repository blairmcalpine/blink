//! Saving clips: cut out of the ring, then written as MP4s.
//!
//! Two threads ([`crate::pipeline`]): the cutter copies each clip at below
//! normal priority as soon as its settle delay has passed, so a game's
//! threads preempt it but it is not starved while it holds the ring lock (see
//! [`crate::cut`]). The writer, the only thread that touches the disk, runs
//! in Windows background mode (lowest CPU, I/O, and memory priority) and
//! never holds the lock.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Instant;

use anyhow::{Context as _, bail};
use blink_ring::ClipBuf;
use blink_ring::media::Timestamp;
use blink_sys::{BackgroundMode, ErrorChain, ThreadPriority};
use time::OffsetDateTime;

use super::EngineEvent;
use crate::clip::{LocalTime, Seconds, file_name, is_temp_name, temp_name};
use crate::cut::CutError;
use crate::pipeline::{Job, SavePipeline};
use crate::shared::SharedRing;
use crate::status::SaveResult;

/// How long after the save time the copy starts, so frames the encoder (and
/// audio, in 20 ms packets) was still finishing at that moment are included.
const SETTLE: Timestamp = Timestamp::from_millis(200);

/// Clips saved within one second get a counter; give up after this many.
const NAME_ATTEMPTS: u32 = 100;

/// Requests accepted at once: one being written plus one being cut or
/// queued, which bounds memory to two clip buffers.
const MAX_OUTSTANDING: u32 = 2;

/// One save, captured when the hotkey or menu item fired.
pub(super) struct SaveRequest {
    /// The save time on the shared QPC clock; the clip ends here.
    pub(super) now: Timestamp,
    /// The local time of the press, which names the file.
    pub(super) time: LocalTime,
    /// Requested clip length.
    pub(super) clip_seconds: u32,
    /// Folder to write the clip to; created if missing.
    pub(super) output_dir: PathBuf,
}

/// The local time now, to the second, or UTC if the local offset is unknown.
pub(super) fn local_time() -> LocalTime {
    let now = OffsetDateTime::now_local().unwrap_or_else(|error| {
        tracing::warn!(error = %ErrorChain(&error), "local time offset unavailable; naming clips in UTC");
        OffsetDateTime::now_utc()
    });
    LocalTime {
        year: now.year(),
        month: u32::from(u8::from(now.month())),
        day: u32::from(now.day()),
        hour: u32::from(now.hour()),
        minute: u32::from(now.minute()),
        second: u32::from(now.second()),
    }
}

/// Hands requests to the save pipeline: one being written and one more
/// being cut or queued; presses beyond that are ignored.
pub(super) struct Saver {
    pipeline: SavePipeline<Written>,
    outstanding: Arc<AtomicU32>,
}

/// What the writer needs besides the clip.
struct Written {
    /// The local time of the press, which names the file.
    time: LocalTime,
    clip_seconds: u32,
    output_dir: PathBuf,
    pressed: Instant,
}

impl Saver {
    /// Starts the cutter and writer threads. The writer first deletes temp
    /// files a crash mid-save left in `output_dir`.
    pub(super) fn spawn(
        ring: Arc<SharedRing>,
        events: Sender<EngineEvent>,
        output_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        let outstanding = Arc::new(AtomicU32::new(0));
        let mut writer = Writer {
            events,
            outstanding: Arc::clone(&outstanding),
            cleaned: Some(output_dir.clone()),
        };
        let pipeline = SavePipeline::spawn(
            ring,
            settle,
            || {
                if let Err(error) =
                    blink_sys::set_current_thread_priority(ThreadPriority::BelowNormal)
                {
                    tracing::warn!(error = %ErrorChain(&error), "could not lower the save thread priority");
                }
            },
            move || {
                remove_temp_files(&output_dir);
                BackgroundMode::enter()
                    .inspect_err(|error| tracing::warn!(error = %ErrorChain(error), "writing at normal I/O priority"))
                    .ok()
            },
            move |job: Job<Written>, clip: Result<&ClipBuf, CutError>| writer.finish(&job, clip),
        )
        .context("failed to spawn the save threads")?;
        Ok(Self {
            pipeline,
            outstanding,
        })
    }

    /// Queues a save unless one is being written and another already queued.
    pub(super) fn request(&self, request: SaveRequest) {
        if !self.pipeline.is_running() {
            tracing::error!("the save threads are gone; cannot save clips");
            return;
        }
        let accepted = self
            .outstanding
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_OUTSTANDING).then_some(count + 1)
            });
        if accepted.is_err() {
            tracing::info!("a clip is saving and another is queued; ignoring this request");
            return;
        }
        let sent = self.pipeline.submit(Job {
            now: request.now,
            duration: Timestamp::from_secs(i64::from(request.clip_seconds)),
            payload: Written {
                time: request.time,
                clip_seconds: request.clip_seconds,
                output_dir: request.output_dir,
                pressed: Instant::now(),
            },
        });
        if !sent {
            self.outstanding.fetch_sub(1, Ordering::AcqRel);
            tracing::error!("the save threads are gone; cannot save clips");
        }
    }

    /// Lets queued saves finish, then joins the threads.
    pub(super) fn shutdown(&mut self) {
        self.pipeline.shutdown();
    }
}

/// Counts a request as done when dropped, so a panicking write still frees
/// its slot.
struct Finished(Arc<AtomicU32>);

impl Drop for Finished {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The writer thread's state.
struct Writer {
    events: Sender<EngineEvent>,
    outstanding: Arc<AtomicU32>,
    /// The folder whose stale temp files were last deleted.
    cleaned: Option<PathBuf>,
}

impl Writer {
    fn finish(&mut self, job: &Job<Written>, clip: Result<&ClipBuf, CutError>) {
        let _finished = Finished(Arc::clone(&self.outstanding));
        let request = &job.payload;
        let result = match clip
            .map_err(anyhow::Error::from)
            .and_then(|clip| self.write(request, clip).map(|path| (path, clip)))
        {
            Ok((path, clip)) => {
                saved(request, &path, clip);
                SaveResult::Saved(Seconds(clip.duration()))
            }
            Err(error) => {
                let error = ErrorChain(error.as_ref()).to_string();
                tracing::error!(error, "failed to save a clip");
                SaveResult::Failed(error)
            }
        };
        let _ = self.events.send(EngineEvent::Saved(result));
    }

    fn write(&mut self, request: &Written, clip: &ClipBuf) -> anyhow::Result<PathBuf> {
        let dir = &request.output_dir;
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create the clip folder {}", dir.display()))?;
        self.clean(dir);
        let (attempt, name) = unused_name(dir, request.time)?;
        let temp = dir.join(temp_name(&name));
        if let Err(error) = write(&temp, clip) {
            // Best effort: the write error matters more than a stray temp file.
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        rename_to_unused(&temp, dir, request.time, attempt).inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
    }

    /// Deletes stale temp files in `dir` the first time it is written to.
    fn clean(&mut self, dir: &Path) {
        if self.cleaned.as_deref() != Some(dir) {
            self.cleaned = Some(dir.to_owned());
            remove_temp_files(dir);
        }
    }
}

/// Deletes the temp files of saves that never finished in `dir`.
fn remove_temp_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_str().is_some_and(is_temp_name) {
            let path = entry.path();
            match fs::remove_file(&path) {
                Ok(()) => tracing::info!(path = %path.display(), "deleted an unfinished clip"),
                Err(error) => {
                    tracing::warn!(error = %ErrorChain(&error), path = %path.display(), "cannot delete a temp file");
                }
            }
        }
    }
}

fn saved(request: &Written, path: &Path, clip: &ClipBuf) {
    let duration = clip.duration();
    let requested = Timestamp::from_secs(i64::from(request.clip_seconds));
    tracing::info!(
        path = %path.display(),
        duration = %Seconds(duration),
        bytes = clip.data_len(),
        elapsed_ms = request.pressed.elapsed().as_millis(),
        "clip saved"
    );
    if duration < requested {
        tracing::info!(
            duration = %Seconds(duration),
            requested = %Seconds(requested),
            "clip is shorter than requested: only that much has been buffered since \
             capture started, the display mode changed (alt-tab, game launch), \
             recording was paused for an unsupported resolution, or a settings change \
             restarted the recorder"
        );
    }
}

/// Waits until [`SETTLE`] after `now` on the QPC clock.
fn settle(now: Timestamp) {
    let target = now.saturating_add(SETTLE);
    let remaining = blink_sys::qpc_now().map_or(SETTLE, |current| target.saturating_sub(current));
    thread::sleep(remaining.to_duration());
}

fn write(temp: &Path, clip: &ClipBuf) -> anyhow::Result<()> {
    let file =
        File::create(temp).with_context(|| format!("failed to create {}", temp.display()))?;
    blink_mux::write_mp4(&file, clip).context("failed to write the MP4")?;
    file.sync_all()
        .with_context(|| format!("failed to flush {}", temp.display()))
}

/// The first clip name for `time` that is free in `dir`, and its attempt
/// number.
fn unused_name(dir: &Path, time: LocalTime) -> anyhow::Result<(u32, String)> {
    for attempt in 0..NAME_ATTEMPTS {
        let name = file_name(time, attempt);
        if !dir.join(&name).try_exists().unwrap_or(true) {
            return Ok((attempt, name));
        }
    }
    bail!(
        "{NAME_ATTEMPTS} clips already exist for this second in {}",
        dir.display()
    )
}

/// Moves `temp` to the first free clip name for `time` from attempt `first`
/// on. The rename never replaces a file, so a clip that took the name since
/// [`unused_name`] looked moves this one on to the next name.
fn rename_to_unused(
    temp: &Path,
    dir: &Path,
    time: LocalTime,
    first: u32,
) -> anyhow::Result<PathBuf> {
    for attempt in first..NAME_ATTEMPTS {
        let path = dir.join(file_name(time, attempt));
        match blink_sys::rename_no_replace(temp, &path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to rename the clip to {}", path.display()));
            }
        }
    }
    bail!(
        "{NAME_ATTEMPTS} clips already exist for this second in {}",
        dir.display()
    )
}
