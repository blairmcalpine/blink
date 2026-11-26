//! D3D11 device creation on the adapter that drives the primary output.

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_DEBUG, D3D11_CREATE_DEVICE_FLAG,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_SDK_COMPONENT_MISSING, IDXGIAdapter1,
    IDXGIFactory1,
};
use windows::core::Interface;

use crate::error::{CaptureError, Context};

/// PCI vendor id of NVIDIA, the only vendor with NVENC.
const NVIDIA_VENDOR_ID: u32 = 0x10DE;

/// The adapter a [`CaptureDevice`] was created on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    /// Driver-reported adapter name, such as `NVIDIA GeForce RTX 4070`.
    pub description: String,
    /// PCI vendor id (`0x10DE` NVIDIA, `0x1002` AMD, `0x8086` Intel).
    pub vendor_id: u32,
}

impl AdapterInfo {
    /// Whether the adapter is an NVIDIA GPU. When it is not (an integrated
    /// GPU driving the primary display on a hybrid laptop, say), NVENC is
    /// unavailable on this device.
    #[must_use]
    pub const fn is_nvidia(&self) -> bool {
        self.vendor_id == NVIDIA_VENDOR_ID
    }
}

/// A hardware D3D11 device for capture and encode, with its immediate context.
///
/// The encoder must use this same device: duplication textures live on it, and
/// NVENC only registers resources of its own device.
///
/// # Threading
///
/// The device is free-threaded. The immediate context is not, so it is put
/// in D3D11 multithread-protected mode, which serializes each context call on
/// an internal lock (an uncontended lock is effectively free). That makes any
/// single call safe from any thread, but not a sequence of calls: the
/// intended model is that the one capture-plus-encode thread that drives
/// [`DesktopDuplicator`](crate::DesktopDuplicator) is also the only thread
/// that uses `context`. Encode work queued for a frame after
/// [`next_frame`](crate::DesktopDuplicator::next_frame) returned it is
/// ordered after that frame's copy, because both go through this one context.
#[derive(Debug, Clone)]
pub struct CaptureDevice {
    /// The D3D11 device, created with `D3D11_CREATE_DEVICE_BGRA_SUPPORT` and
    /// `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`.
    pub device: ID3D11Device,
    /// The device's immediate context, multithread-protected. Use it only from
    /// the capture-plus-encode thread; see the type-level threading notes.
    pub context: ID3D11DeviceContext,
    /// Index of the primary output on this device's adapter, for
    /// [`DesktopDuplicator::new`](crate::DesktopDuplicator::new). Fixed when
    /// the device is created: after a monitor hot-plug or a change of primary
    /// display it may point at another output, or none, until the device is
    /// recreated.
    pub primary_output: u32,
    /// The adapter the device runs on.
    pub adapter: AdapterInfo,
}

/// BGRA for desktop images; video for the NV12 conversion.
const FLAGS: D3D11_CREATE_DEVICE_FLAG = D3D11_CREATE_DEVICE_FLAG(
    D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0,
);

const FEATURE_LEVELS: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];

/// Creates a hardware D3D11 device on the adapter that owns the primary output
/// (the one whose desktop rectangle starts at (0, 0)).
///
/// Desktop Duplication only works on the adapter that scans out the output, so
/// on hybrid-GPU laptops this may be the integrated GPU, where NVENC is
/// unavailable; check [`AdapterInfo::is_nvidia`].
///
/// Debug builds enable the D3D11 debug layer when the Graphics Tools optional
/// feature is installed, and fall back to a plain device when it is not.
///
/// # Errors
///
/// [`CaptureError::NoPrimaryOutput`] if no attached output sits at the desktop
/// origin, or [`CaptureError::Windows`] if DXGI or D3D11 calls fail.
pub fn create_device() -> Result<CaptureDevice, CaptureError> {
    let (adapter, primary_output) = find_primary_output()?;
    let adapter_info = describe(&adapter)?;
    let (device, context) = if cfg!(debug_assertions) {
        match create_on(&adapter, FLAGS | D3D11_CREATE_DEVICE_DEBUG) {
            Err(CaptureError::Windows { source, .. })
                if source.code() == DXGI_ERROR_SDK_COMPONENT_MISSING =>
            {
                tracing::debug!("D3D11 debug layer not installed; creating a plain device");
                create_on(&adapter, FLAGS)?
            }
            result => result?,
        }
    } else {
        create_on(&adapter, FLAGS)?
    };
    let multithread: ID3D11Multithread = context
        .cast()
        .context("query ID3D11Multithread on the immediate context")?;
    // SAFETY: `multithread` is a live interface on this device's immediate
    // context. The call only toggles D3D's internal context lock; its return
    // value is the previous setting, which is irrelevant here.
    let _previous = unsafe { multithread.SetMultithreadProtected(true) };
    tracing::info!(
        primary_output,
        adapter = %adapter_info.description,
        vendor_id = format_args!("{:#06x}", adapter_info.vendor_id),
        "created D3D11 capture device"
    );
    if !adapter_info.is_nvidia() {
        tracing::warn!(
            adapter = %adapter_info.description,
            "primary display is not driven by an NVIDIA GPU; NVENC is unavailable on it"
        );
    }
    Ok(CaptureDevice {
        device,
        context,
        primary_output,
        adapter: adapter_info,
    })
}

fn create_on(
    adapter: &IDXGIAdapter1,
    flags: D3D11_CREATE_DEVICE_FLAG,
) -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device = None;
    let mut context = None;
    // SAFETY: `adapter` is a live adapter. With an explicit adapter the driver
    // type must be UNKNOWN and the software module null. The feature-level
    // slice and the two out-pointers are valid for the duration of the call.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            flags,
            Some(&FEATURE_LEVELS),
            D3D11_SDK_VERSION,
            Some(&raw mut device),
            None,
            Some(&raw mut context),
        )
    }
    .context("create the D3D11 device")?;
    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(CaptureError::NullOutput {
            call: "D3D11CreateDevice",
            what: "device or context",
        }),
    }
}

fn describe(adapter: &IDXGIAdapter1) -> Result<AdapterInfo, CaptureError> {
    // SAFETY: `adapter` is live; GetDesc1 only writes the returned struct.
    let desc = unsafe { adapter.GetDesc1() }.context("describe the capture adapter")?;
    let name = desc
        .Description
        .split(|&unit| unit == 0)
        .next()
        .unwrap_or(&[]);
    Ok(AdapterInfo {
        description: String::from_utf16_lossy(name),
        vendor_id: desc.VendorId,
    })
}

fn find_primary_output() -> Result<(IDXGIAdapter1, u32), CaptureError> {
    // SAFETY: CreateDXGIFactory1 has no preconditions; the generic parameter
    // selects the IID that the returned interface is queried for.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.context("create the DXGI factory")?;
    for adapter_index in 0.. {
        // SAFETY: `factory` is live; out-of-range indices return
        // DXGI_ERROR_NOT_FOUND rather than misbehaving.
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(error).context("enumerate DXGI adapters"),
        };
        for output_index in 0.. {
            // SAFETY: `adapter` is live; out-of-range indices return
            // DXGI_ERROR_NOT_FOUND.
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(error).context("enumerate adapter outputs"),
            };
            // SAFETY: `output` is live; GetDesc only writes the returned struct.
            let desc = unsafe { output.GetDesc() }.context("describe an adapter output")?;
            let origin = desc.DesktopCoordinates;
            if desc.AttachedToDesktop.as_bool() && origin.left == 0 && origin.top == 0 {
                return Ok((adapter, output_index));
            }
        }
    }
    Err(CaptureError::NoPrimaryOutput)
}
