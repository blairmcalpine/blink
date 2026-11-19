//! The Desktop Duplication session: acquire, decimate, copy, recover.

use std::num::NonZeroU32;
use std::thread;
use std::time::{Duration, Instant};

use blink_sys::{QpcClock, qpc_counter};
use windows::Win32::Foundation::{E_POINTER, E_UNEXPECTED};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIDevice,
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::backoff::{Backoff, MAX_RETRY_DELAY};
use crate::decision::{
    Acquired, FrameAction, Staleness, TimeoutAction, acquire_timeout_ms, decide, monotonic,
    on_timeout,
};
use crate::error::{CaptureError, Context, is_session_lost};
use crate::rotation::RoundRobin;
use crate::schedule::FrameScheduler;

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

/// How long a lost session may stay unrecoverable before it is logged as a
/// warning rather than at debug level.
const LONG_OUTAGE: Duration = Duration::from_secs(3);

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
    timestamp: i64,
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

    /// Presentation time in 100 ns ticks on the QPC timeline (the
    /// `blink_ring` `Timestamp` unit). Strictly increasing across the frames
    /// of one duplicator.
    #[must_use]
    pub const fn timestamp(&self) -> i64 {
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
    /// A new desktop image, admitted by the frame-rate decimator.
    Frame(CapturedFrame),
    /// Duplication reported an update without new desktop content (only the
    /// mouse pointer moved). Nothing was copied; the previous frame still
    /// stands.
    Unchanged {
        /// When the update happened, in 100 ns QPC ticks: the pointer update
        /// time if DXGI reported one, otherwise the time it was observed.
        timestamp: i64,
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

/// A DXGI Desktop Duplication session on one output, decimated to a target
/// frame rate.
///
/// Each admitted frame is copied with one GPU `CopyResource` into a small pool
/// of textures this duplicator owns, and the duplication frame is released
/// straight after. The copy is what lets the encoder work asynchronously: the
/// duplicated surface is a single OS-owned texture that DXGI reuses for every
/// frame, so it cannot stay mapped as encoder input across an asynchronous
/// encode or the next acquire. Its bind flags are not guaranteed, and it
/// changes identity with every session, which would churn NVENC
/// registrations. Pool textures have known flags and a stable identity until
/// the size changes. Frames the decimator drops, and updates that only move
/// the pointer, are released without a copy. The steady state performs no
/// heap allocation.
///
/// If the last content change before the desktop goes idle was dropped by the
/// decimator, the session is recreated once that change's slot is due; the
/// first acquire of a new session returns the whole desktop, so the change is
/// still captured. That costs one `DuplicateOutput` per such idle transition
/// and nothing in the steady state.
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
    scheduler: FrameScheduler,
    backoff: Backoff,
    /// When to next try recreating a lost session; `None` means now.
    retry_at: Option<Instant>,
    staleness: Staleness,
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
    last_timestamp: Option<i64>,
    next_sequence: u64,
}

impl DesktopDuplicator {
    /// Starts duplicating output `output_index` of `device`'s adapter (see
    /// [`CaptureDevice::primary_output`](crate::CaptureDevice::primary_output)),
    /// keeping at most `target_fps` frames per second.
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
            scheduler: FrameScheduler::new(target_fps, clock.frequency()),
            backoff: Backoff::default(),
            retry_at: None,
            staleness: Staleness::NewSession,
            pending: None,
            had_session: false,
            lost_since: None,
            warned_outage: false,
            logged_protected: false,
            last_timestamp: None,
            next_sequence: 0,
        };
        match duplicator.recreate() {
            Ok(event) => duplicator.pending = event,
            Err(error @ CaptureError::OutputNotFound { .. }) => return Err(error),
            Err(error) if error.is_session_lost() => {
                tracing::warn!(%error, "desktop duplication unavailable, will retry");
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
    /// Blocks inside `AcquireNextFrame`, or sleeps while waiting to retry a
    /// lost session; it never waits on I/O. Frames dropped by the decimator do
    /// not end the wait.
    ///
    /// # Errors
    ///
    /// [`CaptureError::DeviceLost`] when the device is removed; any other error
    /// is a DXGI or D3D11 failure that recreating the session cannot fix.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<CaptureEvent, CaptureError> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if let Some(event) = self.pending.take() {
                return Ok(event);
            }
            let remaining = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            let Some(duplication) = self.duplication.clone() else {
                if !self.wait_for_retry(remaining) {
                    return Ok(CaptureEvent::Timeout);
                }
                self.try_recover()?;
                continue;
            };
            match self.acquire(&duplication, remaining)? {
                Step::Event(event) => return Ok(event),
                Step::Dropped if remaining.is_zero() => return Ok(CaptureEvent::Timeout),
                Step::Dropped => {}
                Step::Lost => {
                    self.duplication = None;
                    self.lost_since = Some(Instant::now());
                    self.schedule_retry();
                }
                Step::Resync => {
                    tracing::trace!("desktop idle after a dropped change; resyncing session");
                    self.try_recover()?;
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
        let until_slot = if self.staleness == Staleness::DroppedChange {
            self.time_until_slot()?
        } else {
            None
        };
        let timeout_ms = acquire_timeout_ms(remaining, until_slot);
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
                let slot_due = self.scheduler.is_due(qpc_counter()?);
                return Ok(match on_timeout(self.staleness, slot_due) {
                    TimeoutAction::Resync => Step::Resync,
                    TimeoutAction::Report => Step::Event(CaptureEvent::Timeout),
                });
            }
            Err(error) if is_session_lost(error.code()) => {
                tracing::info!(%error, "desktop duplication session lost, recreating");
                return Ok(Step::Lost);
            }
            Err(error) => return Err(error).context("acquire the next desktop frame"),
        }
        // Declared before the resource binding below so the resource reference
        // is dropped first, then the frame released.
        let _release = ReleaseGuard(duplication);
        let resource = resource.take();
        self.backoff.reset();
        if info.ProtectedContentMaskedOut.as_bool() && !self.logged_protected {
            self.logged_protected = true;
            tracing::info!("protected content is masked out of the captured desktop");
        }

        let content_changed = info.LastPresentTime != 0;
        let qpc = if content_changed {
            info.LastPresentTime
        } else if info.LastMouseUpdateTime != 0 {
            info.LastMouseUpdateTime
        } else {
            qpc_counter()?
        };
        let facts = Acquired {
            content_changed,
            has_image: resource.is_some(),
            qpc,
        };
        match decide(facts, &mut self.staleness, &mut self.scheduler) {
            FrameAction::Drop => {
                if !facts.has_image {
                    tracing::debug!("admitted frame had no desktop resource; will resync");
                }
                Ok(Step::Dropped)
            }
            FrameAction::Unchanged => Ok(Step::Event(CaptureEvent::Unchanged {
                timestamp: self.clock.to_ticks(qpc).ticks(),
            })),
            FrameAction::Copy => resource.map_or(Ok(Step::Dropped), |resource| {
                self.copy_frame(&resource, qpc).map(Step::Event)
            }),
        }
    }

    /// Time from now until the decimator's next slot opens.
    fn time_until_slot(&self) -> Result<Option<Duration>, CaptureError> {
        let Some(slot) = self.scheduler.next_admit_qpc() else {
            return Ok(None);
        };
        let now = qpc_counter()?;
        Ok(Some(self.clock.to_duration(slot.saturating_sub(now))))
    }

    /// Copies the acquired desktop image into the next pool texture.
    fn copy_frame(
        &mut self,
        resource: &IDXGIResource,
        qpc: i64,
    ) -> Result<CaptureEvent, CaptureError> {
        let source: ID3D11Texture2D = resource.cast().context("query the desktop texture")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `source` is live and `desc` is a valid, writable struct.
        unsafe { source.GetDesc(&raw mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(CaptureError::UnsupportedFormat(desc.Format));
        }
        let (pool, resized) = pool_for(&mut self.pool, &self.device, desc.Width, desc.Height)?;
        let Some(texture) = pool.textures.next().cloned() else {
            return Err(CaptureError::Windows {
                context: "pick a capture texture",
                source: windows::core::Error::from(E_UNEXPECTED),
            });
        };
        // SAFETY: both textures belong to `self.device` and share size and
        // format (checked above) and sample count; the duplicator never maps
        // either. Whether the destination is still mapped as encoder input
        // depends on the caller honoring the `TEXTURE_POOL_SIZE` reuse rule.
        // Mip and array counts are not checked; a mismatch would make the copy
        // a no-op, not undefined behavior. The source stays alive until the
        // frame is released, after this call is queued.
        unsafe { self.context.CopyResource(&texture, &source) };
        let timestamp = monotonic(self.last_timestamp, self.clock.to_ticks(qpc).ticks());
        self.last_timestamp = Some(timestamp);
        let sequence = self.next_sequence;
        self.next_sequence = sequence.saturating_add(1);
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

    fn try_recover(&mut self) -> Result<(), CaptureError> {
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
            Err(error) => Err(error),
        }
    }

    /// Logs a failed recreation: debug at first, one warning once the outage
    /// has lasted [`LONG_OUTAGE`].
    fn note_outage(&mut self, error: &CaptureError) {
        let since = *self.lost_since.get_or_insert_with(Instant::now);
        if !self.warned_outage && since.elapsed() >= LONG_OUTAGE {
            self.warned_outage = true;
            tracing::warn!(%error, outage = ?since.elapsed(), "desktop capture interrupted");
        } else {
            tracing::debug!(%error, "desktop duplication still unavailable");
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
        let (width, height) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
        self.duplication = Some(duplication);
        self.had_session = true;
        self.scheduler.reset();
        self.retry_at = None;
        self.staleness = Staleness::NewSession;
        let (_, resized) = pool_for(&mut self.pool, &self.device, width, height)?;
        Ok(resized.then_some(CaptureEvent::ModeChanged { width, height }))
    }
}

enum Step {
    Event(CaptureEvent),
    /// A content update that was not copied; keep waiting.
    Dropped,
    /// The session ended; recreate it.
    Lost,
    /// The desktop went idle after a dropped change; recreate the session to
    /// get the full desktop.
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
            tracing::debug!(%error, "ReleaseFrame failed");
        }
    }
}

#[derive(Debug)]
struct TexturePool {
    textures: RoundRobin<ID3D11Texture2D, TEXTURE_POOL_SIZE>,
    width: u32,
    height: u32,
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
    texture.ok_or_else(|| CaptureError::Windows {
        context: "create a capture texture",
        source: windows::core::Error::from(E_POINTER),
    })
}
