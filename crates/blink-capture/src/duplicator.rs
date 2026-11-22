//! The Desktop Duplication session: pace, acquire, copy, recover.

use std::num::NonZeroU32;
use std::thread;
use std::time::{Duration, Instant};

use blink_ring::media::Timestamp;
use blink_sys::{Backoff, ErrorChain, QpcClock, qpc_counter};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO,
    IDXGIDevice, IDXGIOutput1, IDXGIOutput6, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::decision::{
    Acquire, Acquired, EMPTY_SESSION_WAIT, Outcome, Pacing, Staleness, next_empty_wait,
};
use crate::error::{CaptureError, Context, is_session_lost};
use crate::rotation::RoundRobin;

/// Number of textures the duplicator copies frames into, round robin.
///
/// The texture of a [`CapturedFrame`] is overwritten by the copy that produces
/// the `TEXTURE_POOL_SIZE`-th [`CaptureEvent::Frame`] after it (the frame
/// whose [`sequence`](CapturedFrame::sequence) is `TEXTURE_POOL_SIZE` higher).
/// So when calling [`DesktopDuplicator::next_frame`], at most
/// `TEXTURE_POOL_SIZE - 1` earlier frames may still be in use (for example
/// mapped as NVENC input); every older frame must be finished, which for NVENC
/// means its encode has completed and the input resource is unmapped. Cloning
/// a `CapturedFrame` keeps the texture alive but does not protect its
/// contents.
pub const TEXTURE_POOL_SIZE: usize = 3;

/// Delay before the first retry after a failed recreation.
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Longest delay between retries. Recovery never gives up, because the
/// secure desktop (UAC, lock screen) can block duplication for minutes, so the
/// cap keeps polling cheap rather than ending it.
const MAX_RETRY_DELAY: Duration = Duration::from_millis(500);

/// How long a lost session may stay unrecoverable before it is logged as a
/// warning rather than at debug level.
const LONG_OUTAGE: Duration = Duration::from_secs(3);

/// How often the frame counters are logged.
const STATS_PERIOD: Duration = Duration::from_secs(60);

/// A desktop image copied into one of the duplicator's pooled textures.
///
/// Cloning is cheap: it adds a COM reference to the same texture. Holding a
/// clone keeps the texture alive across pool recreation, but does not stop the
/// duplicator from overwriting it; see [`TEXTURE_POOL_SIZE`] for the reuse rule.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    timestamp: Timestamp,
    sequence: u64,
}

impl CapturedFrame {
    /// The `DXGI_FORMAT_B8G8R8A8_UNORM` texture holding the frame, on the
    /// duplicator's device, created with `D3D11_USAGE_DEFAULT` and
    /// `D3D11_BIND_RENDER_TARGET`.
    ///
    /// The copy into it is queued on the device's immediate context. Work that
    /// reads it through that context is ordered after the copy.
    #[must_use]
    pub const fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Presentation time on the shared QPC timeline: the start of the pacing
    /// slot nearest the moment the image was acquired (within half a target
    /// frame interval of it), so frames are a whole number of intervals
    /// apart. Strictly increasing across the frames of one duplicator.
    #[must_use]
    pub const fn timestamp(&self) -> Timestamp {
        self.timestamp
    }

    /// Frame counter, starting at 0 and increasing by one per emitted frame
    /// of this duplicator. A consumer can debug-assert the
    /// [`TEXTURE_POOL_SIZE`] reuse rule with it: every frame still in use when
    /// `next_frame` is called must have
    /// `sequence > newest - TEXTURE_POOL_SIZE + 1`.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// The result of one [`DesktopDuplicator::next_frame`] call.
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// A new desktop image, taken at a pacing slot.
    Frame(CapturedFrame),
    /// Duplication reported an update without new desktop content (only the
    /// mouse pointer moved). Nothing was copied; the previous frame still
    /// stands.
    Unchanged {
        /// When the update was acquired, on the shared QPC timeline.
        timestamp: Timestamp,
    },
    /// Nothing to report before the timeout elapsed. Also returned while the
    /// duplication session is being recreated after a mode change or desktop
    /// switch; [`DesktopDuplicator::is_recovering`] tells the two apart.
    Timeout,
    /// The output size changed (or is known for the first time; this is always
    /// the first event of a new duplicator). Frames from now on have this size.
    /// Textures from earlier frames remain valid while referenced, but the
    /// duplicator never writes them again, so an encoder can drop its
    /// registrations of them.
    ModeChanged {
        /// New width in pixels.
        width: u32,
        /// New height in pixels.
        height: u32,
    },
}

/// A DXGI Desktop Duplication session on one output, paced to a target frame
/// rate.
///
/// Acquisition is paced: after emitting a frame the duplicator sleeps until
/// the next slot of the target frame rate before acquiring again, and DXGI
/// accumulates the desktop updates in between, so the acquire at the slot
/// returns the latest image at once. No update is dropped, so a change made
/// just before the desktop goes quiet is still captured, and a 144 Hz desktop
/// costs one acquire per slot rather than one per refresh. When a slot finds
/// nothing new, the acquire blocks until the desktop changes and takes that
/// update straight away. Each frame is stamped with the start of the slot
/// nearest the acquire, so frames during motion are exactly one interval
/// apart; a frame that starts late (a slow encode, a preempted thread) takes
/// the slot it lands nearest, skipping the ones that passed rather than
/// bursting. After a timeout or a new session the current slot is not
/// stamped, since the encoder may have refreshed a keyframe in it.
///
/// Each emitted frame is copied with one GPU `CopyResource` into a small pool
/// of textures this duplicator owns, and the duplication frame is released
/// straight after. The copy is what lets the encoder work asynchronously: the
/// duplicated surface is a single OS-owned texture that DXGI reuses for every
/// frame, so it cannot stay mapped as encoder input across an asynchronous
/// encode or the next acquire. Its bind flags are not guaranteed, and it
/// changes identity with every session, which would churn NVENC
/// registrations. Pool textures have known flags and a stable identity until
/// the size changes. The steady state performs no heap allocation.
///
/// Only frames with new desktop content are copied; a pointer-only update's
/// surface is not guaranteed to hold the desktop (on a new session it is
/// black). A pointer-only update holds the next acquire until the following
/// slot, so a moving mouse costs at most one acquire per slot, and a content
/// change made meanwhile accumulates with it and wins. A content update that
/// comes without an image is looked for again at the next slot, and if that
/// slot brings no content the session is recreated: the first content update
/// of a new session carries the whole desktop. A new session with nothing to
/// show yet (startup, or after a resize) that gets no content update within a
/// second is re-duplicated, backing off to every 8 s on a static screen.
///
/// When a session ends (`DXGI_ERROR_ACCESS_LOST` on a mode change or alt-tab,
/// `DXGI_ERROR_INVALID_CALL` or access denied around a desktop switch), it is
/// recreated with a bounded exponential backoff, reporting
/// [`CaptureEvent::Timeout`] in the meantime and
/// [`CaptureEvent::ModeChanged`] if the size changed.
///
/// Only one duplication of an output can exist per process: drop a
/// `DesktopDuplicator` before creating another for the same output, or the
/// new one fails with `E_INVALIDARG`.
///
/// Known limitations in v1:
///
/// - The mouse cursor is not composited into frames.
/// - Rotated outputs are captured unrotated.
///
/// Drive the duplicator from the thread that also encodes on its device; see
/// the threading notes on [`CaptureDevice`](crate::CaptureDevice).
#[derive(Debug)]
pub struct DesktopDuplicator {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    output_index: u32,
    duplication: Option<IDXGIOutputDuplication>,
    pool: Option<TexturePool>,
    clock: QpcClock,
    pacing: Pacing,
    backoff: Backoff,
    /// When to next try recreating a lost session; `None` means now.
    retry_at: Option<Instant>,
    /// An event to return before acquiring again.
    pending: Option<CaptureEvent>,
    /// A session has existed at some point, so errors that are fatal on first
    /// duplication may be transient now.
    had_session: bool,
    /// When the current outage began, if the session is lost.
    lost_since: Option<Instant>,
    /// The current outage has been reported at warning level.
    warned_outage: bool,
    /// `ProtectedContentMaskedOut` has been logged.
    logged_protected: bool,
    next_sequence: u64,
    /// Sessions recreated to deliver a change that came without an image.
    resyncs: u64,
    stats_since: Instant,
    /// When the current session was created.
    session_started: Instant,
    /// How long the next session with nothing to show may wait for content.
    empty_wait: Duration,
}

impl DesktopDuplicator {
    /// Starts duplicating output `output_index` of `device`'s adapter (see
    /// [`CaptureDevice::primary_output`](crate::CaptureDevice::primary_output)),
    /// acquiring at most `target_fps` frames per second.
    ///
    /// If the output exists but cannot be duplicated right now (the secure
    /// desktop is up, say), this still succeeds and keeps retrying from
    /// [`next_frame`](Self::next_frame).
    ///
    /// # Errors
    ///
    /// [`CaptureError::OutputNotFound`] for a bad index,
    /// [`CaptureError::Windows`] or [`CaptureError::DeviceLost`] if DXGI or
    /// D3D11 fail in a way recreation cannot fix (for example
    /// `DXGI_ERROR_UNSUPPORTED` when the output is driven by another adapter,
    /// or `E_INVALIDARG` while another duplicator of this output is alive).
    pub fn new(
        device: &ID3D11Device,
        output_index: u32,
        target_fps: NonZeroU32,
    ) -> Result<Self, CaptureError> {
        // SAFETY: `device` is a live device; the call only returns a new
        // reference to its immediate context.
        let context =
            unsafe { device.GetImmediateContext() }.context("get the immediate context")?;
        let clock = QpcClock::system()?;
        let mut duplicator = Self {
            device: device.clone(),
            context,
            output_index,
            duplication: None,
            pool: None,
            clock,
            pacing: Pacing::new(target_fps, clock.frequency()),
            backoff: Backoff::new(INITIAL_RETRY_DELAY, MAX_RETRY_DELAY),
            retry_at: None,
            pending: None,
            had_session: false,
            lost_since: None,
            warned_outage: false,
            logged_protected: false,
            next_sequence: 0,
            resyncs: 0,
            stats_since: Instant::now(),
            session_started: Instant::now(),
            empty_wait: EMPTY_SESSION_WAIT,
        };
        match duplicator.recreate() {
            Ok(event) => duplicator.pending = event,
            Err(error @ CaptureError::OutputNotFound { .. }) => return Err(error),
            Err(error) if error.is_session_lost() => {
                tracing::warn!(error = %ErrorChain(&error), "desktop duplication unavailable, will retry");
                duplicator.lost_since = Some(Instant::now());
                duplicator.schedule_retry();
            }
            Err(error) => return Err(error),
        }
        Ok(duplicator)
    }

    /// The current output size, once a session has been established.
    #[must_use]
    pub fn size(&self) -> Option<(u32, u32)> {
        self.pool.as_ref().map(|pool| (pool.width, pool.height))
    }

    /// Whether the duplication session is lost and being recreated, so a
    /// [`CaptureEvent::Timeout`] means "no capture" rather than "idle desktop".
    #[must_use]
    pub const fn is_recovering(&self) -> bool {
        self.duplication.is_none()
    }

    /// Waits up to `timeout` for the next event.
    ///
    /// Sleeps until the next pacing slot, then blocks inside
    /// `AcquireNextFrame` until the desktop changes; or sleeps while waiting
    /// to retry a lost session. It never waits on I/O.
    ///
    /// # Errors
    ///
    /// [`CaptureError::DeviceLost`] when the device is removed; any other error
    /// is a DXGI or D3D11 failure that recreating the session cannot fix.
    /// Either way the session has been dropped: the duplicator delivers no
    /// frames until a later call recreates it, and since recreation is not
    /// expected to succeed, callers should rebuild the device and duplicator.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<CaptureEvent, CaptureError> {
        let now = Instant::now();
        if now.saturating_duration_since(self.stats_since) >= STATS_PERIOD {
            self.log_stats(now);
        }
        let deadline = now.checked_add(timeout);
        loop {
            if let Some(event) = self.pending.take() {
                return Ok(event);
            }
            let remaining = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if self.duplication.is_some() {
                let now = qpc_counter()?;
                if let Some(slot) = self.pacing.before_acquire(now) {
                    let wait = self.clock.to_duration(slot.saturating_sub(now));
                    if wait > remaining {
                        thread::sleep(remaining);
                        return Ok(CaptureEvent::Timeout);
                    }
                    thread::sleep(wait);
                    continue;
                }
            }
            // Moved out, not cloned: `DuplicateOutput` fails with
            // `E_INVALIDARG` while any reference to this output's previous
            // session is alive, so a session that ends here must be dropped
            // before `recreate` runs.
            let Some(duplication) = self.duplication.take() else {
                if !self.wait_for_retry(remaining) {
                    return Ok(CaptureEvent::Timeout);
                }
                self.try_recover(Recovery::Lost)?;
                continue;
            };
            let step = self.acquire(&duplication, remaining)?;
            if matches!(step, Step::Lost | Step::Resync) {
                drop(duplication);
            } else {
                self.duplication = Some(duplication);
            }
            match step {
                Step::Event(event) => {
                    if self.empty_session_overdue() {
                        let stats = &mut self.pacing.stats;
                        stats.empty_resyncs = stats.empty_resyncs.saturating_add(1);
                        stats.record_contentless_session(self.session_started.elapsed());
                        tracing::debug!(
                            waited = ?self.session_started.elapsed(),
                            "no desktop content on a session with nothing to show; re-duplicating"
                        );
                        self.empty_wait = next_empty_wait(self.empty_wait);
                        self.duplication = None;
                        self.try_recover(Recovery::Empty)?;
                    }
                    return Ok(event);
                }
                Step::Missed if remaining.is_zero() => return Ok(CaptureEvent::Timeout),
                Step::Missed => {}
                Step::Lost => {
                    if self.pacing.staleness == Staleness::NewSession {
                        self.pacing
                            .stats
                            .record_contentless_session(self.session_started.elapsed());
                    }
                    self.lost_since = Some(Instant::now());
                    self.schedule_retry();
                }
                Step::Resync => {
                    self.resyncs = self.resyncs.saturating_add(1);
                    let started = Instant::now();
                    self.try_recover(Recovery::Resync)?;
                    let elapsed = started.elapsed();
                    self.pacing.stats.record_resync(elapsed);
                    tracing::debug!(
                        ?elapsed,
                        resyncs = self.resyncs,
                        recovering = self.is_recovering(),
                        "resynced the session to deliver a change that came without an image"
                    );
                }
            }
        }
    }

    /// Acquires one duplication frame and decides what it becomes.
    fn acquire(
        &mut self,
        duplication: &IDXGIOutputDuplication,
        remaining: Duration,
    ) -> Result<Step, CaptureError> {
        let timeout_ms = self.pacing.acquire_timeout_ms(remaining);
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: `duplication` is live, no frame is currently held (every
        // successful acquire is paired with a `ReleaseGuard` that is dropped
        // before this function returns), and both out-pointers are valid for
        // the call.
        let acquired =
            unsafe { duplication.AcquireNextFrame(timeout_ms, &raw mut info, &raw mut resource) };
        match acquired {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                let outcome = self.pacing.after_acquire(Acquire::Timeout, qpc_counter()?);
                return Ok(match outcome {
                    Outcome::Resync => Step::Resync,
                    _ => Step::Event(CaptureEvent::Timeout),
                });
            }
            Err(error) if is_session_lost(error.code()) => {
                tracing::info!(error = %ErrorChain(&error), "desktop duplication session lost, recreating");
                return Ok(Step::Lost);
            }
            Err(error) => return Err(error).context("acquire the next desktop frame"),
        }
        // Declared before the resource binding below so the resource reference
        // is dropped first, then the frame released.
        let _release = ReleaseGuard(duplication);
        let resource = resource.take();
        let qpc = qpc_counter()?;
        self.backoff.reset();
        if info.ProtectedContentMaskedOut.as_bool() && !self.logged_protected {
            self.logged_protected = true;
            tracing::info!("protected content is masked out of the captured desktop");
        }

        let facts = Acquired {
            content_changed: info.LastPresentTime != 0,
            has_image: resource.is_some(),
        };
        let acquired = Acquire::Frame {
            facts,
            accumulated: info.AccumulatedFrames,
        };
        match self.pacing.after_acquire(acquired, qpc) {
            Outcome::Copy {
                timestamp,
                first_of_session,
            } => {
                if first_of_session {
                    self.pacing
                        .stats
                        .record_first_content(self.session_started.elapsed());
                }
                let Some(resource) = resource else {
                    return Err(CaptureError::Invariant("copying a frame without an image"));
                };
                self.copy_frame(&resource, timestamp).map(Step::Event)
            }
            Outcome::Unchanged => Ok(Step::Event(CaptureEvent::Unchanged {
                timestamp: self.clock.to_ticks(qpc),
            })),
            Outcome::Timeout => Ok(Step::Event(CaptureEvent::Timeout)),
            Outcome::Missed => Ok(Step::Missed),
            Outcome::Resync => Ok(Step::Resync),
        }
    }

    /// Copies the desktop image into the next pool texture, stamped with
    /// `timestamp`.
    fn copy_frame(
        &mut self,
        resource: &IDXGIResource,
        timestamp: Timestamp,
    ) -> Result<CaptureEvent, CaptureError> {
        let source: ID3D11Texture2D = resource.cast().context("query the desktop texture")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `source` is live and `desc` is a valid, writable struct.
        unsafe { source.GetDesc(&raw mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(CaptureError::UnsupportedFormat(desc.Format));
        }
        let (pool, resized) = pool_for(&mut self.pool, &self.device, desc.Width, desc.Height)?;
        pool.filled = true;
        let Some(texture) = pool.textures.next().cloned() else {
            return Err(CaptureError::Invariant("the capture texture pool is empty"));
        };
        // SAFETY: both textures belong to `self.device` and share size and
        // format (checked above) and sample count; the duplicator never maps
        // either. Whether the destination is still mapped as encoder input
        // depends on the caller honoring the `TEXTURE_POOL_SIZE` reuse rule.
        // Mip and array counts are not checked; a mismatch would make the copy
        // a no-op, not undefined behavior. The source stays alive until the
        // frame is released, after this call is queued.
        unsafe { self.context.CopyResource(&texture, &source) };
        let sequence = self.next_sequence;
        self.next_sequence = sequence.saturating_add(1);
        self.empty_wait = EMPTY_SESSION_WAIT;
        let frame = CaptureEvent::Frame(CapturedFrame {
            texture,
            width: desc.Width,
            height: desc.Height,
            timestamp,
            sequence,
        });
        if resized {
            self.pending = Some(frame);
            return Ok(CaptureEvent::ModeChanged {
                width: desc.Width,
                height: desc.Height,
            });
        }
        Ok(frame)
    }

    /// Whether the session has gone `empty_wait` without the content update
    /// that would give it something to show (see
    /// [`Pacing::empty_session_overdue`]); nothing of the current size has
    /// been copied yet on startup or after a resize. Re-duplicating is not
    /// documented to produce content, but the first acquire of a fresh
    /// duplication is expected to carry the full desktop, as it usually does
    /// in practice.
    fn empty_session_overdue(&self) -> bool {
        self.duplication.is_some()
            && self.pacing.empty_session_overdue(
                self.pool.as_ref().is_some_and(|pool| pool.filled),
                self.session_started.elapsed(),
                self.empty_wait,
            )
    }

    /// Logs and restarts the periodic frame counters.
    fn log_stats(&mut self, now: Instant) {
        let stats = std::mem::take(&mut self.pacing.stats);
        let pending_first_content = (self.pacing.staleness == Staleness::NewSession
            && self.duplication.is_some())
        .then(|| now.saturating_duration_since(self.session_started));
        tracing::info!(
            copied = stats.copied,
            coalesced = stats.coalesced,
            unchanged_slots = stats.unchanged_slots,
            pointer_only = stats.pointer_only,
            new_session_pointer_only = stats.new_session_pointer_only,
            late_slots = stats.late_slots,
            no_image = stats.no_image,
            resyncs = stats.resyncs,
            slowest_resync = ?stats.slowest_resync,
            slowest_first_content = ?stats.slowest_first_content,
            empty_resyncs = stats.empty_resyncs,
            contentless_sessions = stats.contentless_sessions,
            pending_first_content = ?pending_first_content,
            elapsed_s = now.saturating_duration_since(self.stats_since).as_secs(),
            "capture stats"
        );
        self.stats_since = now;
    }

    /// Sleeps until the next retry is due. Returns `false` if that is later
    /// than `remaining`, after sleeping for `remaining`.
    fn wait_for_retry(&self, remaining: Duration) -> bool {
        let Some(retry_at) = self.retry_at else {
            return true;
        };
        let wait = retry_at.saturating_duration_since(Instant::now());
        if wait > remaining {
            thread::sleep(remaining);
            return false;
        }
        thread::sleep(wait);
        true
    }

    fn try_recover(&mut self, cause: Recovery) -> Result<(), CaptureError> {
        match self.recreate() {
            Ok(event) => {
                if let Some(since) = self.lost_since.take() {
                    tracing::info!(
                        size = ?self.size(),
                        outage = ?since.elapsed(),
                        "desktop duplication session recreated"
                    );
                }
                self.warned_outage = false;
                self.pending = event;
                Ok(())
            }
            Err(error) if error.is_session_lost() => {
                self.note_outage(&error);
                self.schedule_retry();
                Ok(())
            }
            Err(error) if self.had_session && error.is_unsupported() => {
                // Mid-session this can be a mode transition; keep polling at
                // the slowest rate rather than ending capture.
                self.note_outage(&error);
                self.retry_at = Instant::now().checked_add(MAX_RETRY_DELAY);
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    error = %ErrorChain(&error),
                    cause = cause.as_str(),
                    frames = self.next_sequence,
                    resyncs = self.resyncs,
                    "could not recreate the desktop duplication session"
                );
                Err(error)
            }
        }
    }

    /// Logs a failed recreation: debug at first, one warning once the outage
    /// has lasted [`LONG_OUTAGE`].
    fn note_outage(&mut self, error: &CaptureError) {
        let since = *self.lost_since.get_or_insert_with(Instant::now);
        if !self.warned_outage && since.elapsed() >= LONG_OUTAGE {
            self.warned_outage = true;
            tracing::warn!(
                error = %ErrorChain(error),
                outage = ?since.elapsed(),
                "desktop capture interrupted"
            );
        } else {
            tracing::debug!(error = %ErrorChain(error), "desktop duplication still unavailable");
        }
    }

    fn schedule_retry(&mut self) {
        self.retry_at = Instant::now().checked_add(self.backoff.next_delay());
    }

    /// Creates a fresh duplication session, resizing the pool if the output
    /// size changed. Returns [`CaptureEvent::ModeChanged`] in that case.
    fn recreate(&mut self) -> Result<Option<CaptureEvent>, CaptureError> {
        // Release the old session first: an output allows one duplication per
        // process.
        self.duplication = None;
        let dxgi_device: IDXGIDevice = self.device.cast().context("query IDXGIDevice")?;
        // SAFETY: `dxgi_device` is live; the call returns a new reference to
        // the adapter the device was created on.
        let adapter = unsafe { dxgi_device.GetAdapter() }.context("get the device's adapter")?;
        // SAFETY: `adapter` is live; out-of-range indices return
        // DXGI_ERROR_NOT_FOUND.
        let output = match unsafe { adapter.EnumOutputs(self.output_index) } {
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
                return Err(CaptureError::OutputNotFound {
                    index: self.output_index,
                });
            }
            result => result.context("enumerate the capture output")?,
        };
        let output: IDXGIOutput1 = output.cast().context("query IDXGIOutput1")?;
        // SAFETY: `output` is live and `self.device` is a live D3D11 device on
        // the adapter that owns it.
        let duplication =
            unsafe { output.DuplicateOutput(&self.device) }.context("duplicate the output")?;
        // SAFETY: `duplication` is live; GetDesc only writes the returned struct.
        let desc = unsafe { duplication.GetDesc() };
        let (width, height) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
        let first = !self.had_session;
        self.duplication = Some(duplication);
        self.had_session = true;
        // Time spent without a session was not a slot missed by running late.
        self.pacing.new_session(qpc_counter()?);
        self.retry_at = None;
        self.session_started = Instant::now();
        let (_, resized) = pool_for(&mut self.pool, &self.device, width, height)?;
        if first || resized {
            log_session(&output, &desc);
        }
        Ok(resized.then_some(CaptureEvent::ModeChanged { width, height }))
    }
}

/// Logs what a new duplication session delivers and the output's color
/// setup, so a capture problem report shows HDR, refresh, and format.
fn log_session(output: &IDXGIOutput1, desc: &DXGI_OUTDUPL_DESC) {
    let mode = &desc.ModeDesc;
    let output_desc = output.cast::<IDXGIOutput6>().ok().and_then(|output| {
        // SAFETY: `output` is live; GetDesc1 only fills the returned struct.
        unsafe { output.GetDesc1() }.ok()
    });
    let bits_per_color = output_desc.map(|desc| desc.BitsPerColor);
    let color_space = output_desc.map(|desc| desc.ColorSpace.0);
    tracing::info!(
        width = mode.Width,
        height = mode.Height,
        format = mode.Format.0,
        refresh_numerator = mode.RefreshRate.Numerator,
        refresh_denominator = mode.RefreshRate.Denominator,
        rotation = desc.Rotation.0,
        in_system_memory = desc.DesktopImageInSystemMemory.as_bool(),
        bits_per_color,
        color_space,
        "desktop duplication session"
    );
    if desc.DesktopImageInSystemMemory.as_bool() {
        tracing::warn!("desktop image is in system memory; GPU copy path is unverified here");
    }
    if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY
        && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED
    {
        tracing::warn!(
            rotation = desc.Rotation.0,
            "rotated output is captured unrotated"
        );
    }
}

/// Why a session is being recreated, for diagnostics.
#[derive(Clone, Copy)]
enum Recovery {
    /// The previous session ended or could not be created.
    Lost,
    /// A change came without an image and no content update followed.
    Resync,
    /// A session with nothing to show got no content update.
    Empty,
}

impl Recovery {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Lost => "session lost",
            Self::Resync => "resync after a change without an image",
            Self::Empty => "no content on a new session",
        }
    }
}

enum Step {
    Event(CaptureEvent),
    /// A content update without an image; look again at the next slot.
    Missed,
    /// The session ended; recreate it.
    Lost,
    /// A change came without an image and no content update followed;
    /// recreate the session to get the full desktop.
    Resync,
}

/// Releases the held duplication frame when dropped.
struct ReleaseGuard<'a>(&'a IDXGIOutputDuplication);

impl Drop for ReleaseGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: a guard is only constructed right after AcquireNextFrame
        // succeeded on this duplication, and is dropped once, so exactly one
        // frame is held when this runs.
        if let Err(error) = unsafe { self.0.ReleaseFrame() } {
            // The session is gone; the next acquire reports it and recovers.
            tracing::debug!(error = %ErrorChain(&error), "ReleaseFrame failed");
        }
    }
}

#[derive(Debug)]
struct TexturePool {
    textures: RoundRobin<ID3D11Texture2D, TEXTURE_POOL_SIZE>,
    width: u32,
    height: u32,
    /// A frame has been copied into the pool.
    filled: bool,
}

impl TexturePool {
    fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self, CaptureError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0.cast_unsigned(),
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let textures = RoundRobin::new([
            create_texture(device, &desc)?,
            create_texture(device, &desc)?,
            create_texture(device, &desc)?,
        ]);
        tracing::debug!(width, height, "created capture texture pool");
        Ok(Self {
            textures,
            width,
            height,
            filled: false,
        })
    }
}

/// Returns the pool for `width` x `height`, recreating it if the size differs,
/// and whether it was recreated.
fn pool_for<'a>(
    pool: &'a mut Option<TexturePool>,
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(&'a mut TexturePool, bool), CaptureError> {
    let current = pool
        .take()
        .filter(|pool| pool.width == width && pool.height == height);
    let resized = current.is_none();
    let fresh = match current {
        Some(current) => current,
        None => TexturePool::new(device, width, height)?,
    };
    Ok((pool.insert(fresh), resized))
}

fn create_texture(
    device: &ID3D11Device,
    desc: &D3D11_TEXTURE2D_DESC,
) -> Result<ID3D11Texture2D, CaptureError> {
    let mut texture = None;
    // SAFETY: `desc` describes a valid single-mip, non-multisampled 2D texture,
    // no initial data is supplied, and `texture` is a valid out-pointer.
    unsafe { device.CreateTexture2D(desc, None, Some(&raw mut texture)) }
        .context("create a capture texture")?;
    texture.ok_or(CaptureError::NullOutput {
        call: "CreateTexture2D",
        what: "texture",
    })
}
