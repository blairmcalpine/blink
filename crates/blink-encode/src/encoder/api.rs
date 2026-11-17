//! Loading `nvEncodeAPI64.dll` and resolving the NVENC function table.

use core::ffi::{CStr, c_void};

use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{PCSTR, s, w};

use crate::EncodeError;
use crate::ffi::{
    NV_ENCODE_API_FUNCTION_LIST, NV_ENCODE_API_FUNCTION_LIST_VER, NVENCAPI_MAJOR_VERSION,
    NVENCAPI_MINOR_VERSION, NVENCSTATUS, PNVENCCREATEBITSTREAMBUFFER, PNVENCDESTROYBITSTREAMBUFFER,
    PNVENCDESTROYENCODER, PNVENCENCODEPICTURE, PNVENCGETENCODEPRESETCONFIGEX, PNVENCGETLASTERROR,
    PNVENCGETSEQUENCEPARAMS, PNVENCINITIALIZEENCODER, PNVENCLOCKBITSTREAM, PNVENCMAPINPUTRESOURCE,
    PNVENCODEAPICREATEINSTANCE, PNVENCODEAPIGETMAXSUPPORTEDVERSION, PNVENCOPENENCODESESSIONEX,
    PNVENCRECONFIGUREENCODER, PNVENCREGISTERRESOURCE, PNVENCUNLOCKBITSTREAM,
    PNVENCUNMAPINPUTRESOURCE, PNVENCUNREGISTERRESOURCE, zeroed,
};
use crate::status::{ApiVersion, NvencStatus};

/// The API version of the transcribed header; older drivers reject its
/// struct versions.
const REQUIRED_VERSION: ApiVersion = ApiVersion {
    major: NVENCAPI_MAJOR_VERSION,
    minor: NVENCAPI_MINOR_VERSION,
};

/// What `GetProcAddress` returns for a found export.
type RawProc = unsafe extern "system" fn() -> isize;

/// An owned `LoadLibraryExW` handle, freed on drop.
struct Library(HMODULE);

impl Library {
    fn load() -> Result<Self, EncodeError> {
        // SAFETY: the name is a static NUL-terminated wide string, no file
        // handle is passed (it must be null), and the flag restricts the search
        // to System32 so a DLL planted next to the executable is never loaded.
        let module =
            unsafe { LoadLibraryExW(w!("nvEncodeAPI64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(EncodeError::NoNvidiaDriver)?;
        Ok(Self(module))
    }

    fn export(&self, name: PCSTR, display: &'static str) -> Result<RawProc, EncodeError> {
        // SAFETY: `self.0` is a module loaded by `load` and not yet freed, and
        // `name` is a static NUL-terminated ANSI string.
        unsafe { GetProcAddress(self.0, name) }.ok_or(EncodeError::MissingExport(display))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `LoadLibraryExW` and is freed exactly once,
        // here. `Api` declares its library last, so no function pointer into
        // the DLL outlives this.
        if let Err(error) = unsafe { FreeLibrary(self.0) } {
            tracing::warn!(%error, "FreeLibrary(nvEncodeAPI64.dll) failed");
        }
    }
}

/// The NVENC entry points blink calls, each checked non-null at load.
pub(super) struct Api {
    pub(super) open_encode_session_ex: PNVENCOPENENCODESESSIONEX,
    pub(super) get_encode_preset_config_ex: PNVENCGETENCODEPRESETCONFIGEX,
    pub(super) initialize_encoder: PNVENCINITIALIZEENCODER,
    pub(super) reconfigure_encoder: PNVENCRECONFIGUREENCODER,
    pub(super) create_bitstream_buffer: PNVENCCREATEBITSTREAMBUFFER,
    pub(super) destroy_bitstream_buffer: PNVENCDESTROYBITSTREAMBUFFER,
    pub(super) register_resource: PNVENCREGISTERRESOURCE,
    pub(super) unregister_resource: PNVENCUNREGISTERRESOURCE,
    pub(super) map_input_resource: PNVENCMAPINPUTRESOURCE,
    pub(super) unmap_input_resource: PNVENCUNMAPINPUTRESOURCE,
    pub(super) encode_picture: PNVENCENCODEPICTURE,
    pub(super) lock_bitstream: PNVENCLOCKBITSTREAM,
    pub(super) unlock_bitstream: PNVENCUNLOCKBITSTREAM,
    pub(super) get_sequence_params: PNVENCGETSEQUENCEPARAMS,
    pub(super) destroy_encoder: PNVENCDESTROYENCODER,
    /// Optional: only used to enrich error messages.
    get_last_error_string: Option<PNVENCGETLASTERROR>,
    /// Declared last so it drops after every other field; the function
    /// pointers above point into this DLL.
    _library: Library,
}

impl Api {
    /// Loads the DLL, checks the driver's API version, and fills the function
    /// table.
    pub(super) fn load() -> Result<Self, EncodeError> {
        let library = Library::load()?;

        let get_max_version = library.export(
            s!("NvEncodeAPIGetMaxSupportedVersion"),
            "NvEncodeAPIGetMaxSupportedVersion",
        )?;
        // SAFETY: nvEncodeAPI.h declares this export as
        // `NVENCSTATUS NVENCAPI NvEncodeAPIGetMaxSupportedVersion(uint32_t*)`,
        // which is exactly `PNVENCODEAPIGETMAXSUPPORTEDVERSION`; both are
        // function pointers of the same size.
        let get_max_version = unsafe {
            core::mem::transmute::<RawProc, PNVENCODEAPIGETMAXSUPPORTEDVERSION>(get_max_version)
        };
        let mut packed = 0_u32;
        // SAFETY: the out pointer refers to a live, writable `u32`.
        let status = unsafe { get_max_version(&raw mut packed) };
        check_status("NvEncodeAPIGetMaxSupportedVersion", status, None)?;
        let driver = ApiVersion::from_driver(packed);
        if driver < REQUIRED_VERSION {
            return Err(EncodeError::DriverTooOld {
                driver_major: driver.major,
                driver_minor: driver.minor,
                required_major: REQUIRED_VERSION.major,
                required_minor: REQUIRED_VERSION.minor,
            });
        }
        tracing::debug!(
            major = driver.major,
            minor = driver.minor,
            "NVENC driver API version"
        );

        let create_instance =
            library.export(s!("NvEncodeAPICreateInstance"), "NvEncodeAPICreateInstance")?;
        // SAFETY: nvEncodeAPI.h declares this export as
        // `NVENCSTATUS NVENCAPI NvEncodeAPICreateInstance(NV_ENCODE_API_FUNCTION_LIST*)`,
        // which is exactly `PNVENCODEAPICREATEINSTANCE`.
        let create_instance =
            unsafe { core::mem::transmute::<RawProc, PNVENCODEAPICREATEINSTANCE>(create_instance) };
        let mut list: NV_ENCODE_API_FUNCTION_LIST = zeroed();
        list.version = NV_ENCODE_API_FUNCTION_LIST_VER;
        // SAFETY: `list` is a zeroed function list with its version set, as
        // the API requires; the driver only writes within its 2552 bytes.
        let status = unsafe { create_instance(&raw mut list) };
        check_status("NvEncodeAPICreateInstance", status, None)?;

        Ok(Self {
            open_encode_session_ex: required(
                list.nvEncOpenEncodeSessionEx,
                "nvEncOpenEncodeSessionEx",
            )?,
            get_encode_preset_config_ex: required(
                list.nvEncGetEncodePresetConfigEx,
                "nvEncGetEncodePresetConfigEx",
            )?,
            initialize_encoder: required(list.nvEncInitializeEncoder, "nvEncInitializeEncoder")?,
            reconfigure_encoder: required(list.nvEncReconfigureEncoder, "nvEncReconfigureEncoder")?,
            create_bitstream_buffer: required(
                list.nvEncCreateBitstreamBuffer,
                "nvEncCreateBitstreamBuffer",
            )?,
            destroy_bitstream_buffer: required(
                list.nvEncDestroyBitstreamBuffer,
                "nvEncDestroyBitstreamBuffer",
            )?,
            register_resource: required(list.nvEncRegisterResource, "nvEncRegisterResource")?,
            unregister_resource: required(list.nvEncUnregisterResource, "nvEncUnregisterResource")?,
            map_input_resource: required(list.nvEncMapInputResource, "nvEncMapInputResource")?,
            unmap_input_resource: required(
                list.nvEncUnmapInputResource,
                "nvEncUnmapInputResource",
            )?,
            encode_picture: required(list.nvEncEncodePicture, "nvEncEncodePicture")?,
            lock_bitstream: required(list.nvEncLockBitstream, "nvEncLockBitstream")?,
            unlock_bitstream: required(list.nvEncUnlockBitstream, "nvEncUnlockBitstream")?,
            get_sequence_params: required(list.nvEncGetSequenceParams, "nvEncGetSequenceParams")?,
            destroy_encoder: required(list.nvEncDestroyEncoder, "nvEncDestroyEncoder")?,
            get_last_error_string: list.nvEncGetLastErrorString,
            _library: library,
        })
    }

    /// Turns an NVENC status into a `Result`, attaching the session's last
    /// error string on failure.
    pub(super) fn check(
        &self,
        encoder: *mut c_void,
        call: &'static str,
        status: NVENCSTATUS,
    ) -> Result<(), EncodeError> {
        if status == crate::ffi::NV_ENC_SUCCESS {
            return Ok(());
        }
        check_status(call, status, self.last_error(encoder))
    }

    /// `nvEncGetLastErrorString` for a live session, if the driver has one.
    fn last_error(&self, encoder: *mut c_void) -> Option<String> {
        let get = self.get_last_error_string?;
        if encoder.is_null() {
            return None;
        }
        // SAFETY: `encoder` is a live session handle from
        // `nvEncOpenEncodeSessionEx` (callers pass only those).
        let message = unsafe { get(encoder) };
        if message.is_null() {
            return None;
        }
        // SAFETY: the driver returns a NUL-terminated string that stays valid
        // until the next call on this session; it is copied out immediately,
        // on the same thread.
        let message = unsafe { CStr::from_ptr(message) };
        let message = message.to_string_lossy();
        let message = message.trim();
        (!message.is_empty()).then(|| message.to_owned())
    }
}

fn required<F>(function: Option<F>, name: &'static str) -> Result<F, EncodeError> {
    function.ok_or(EncodeError::MissingFunction(name))
}

fn check_status(
    call: &'static str,
    status: NVENCSTATUS,
    detail: Option<String>,
) -> Result<(), EncodeError> {
    let status = NvencStatus::from_raw(status);
    if status.is_success() {
        Ok(())
    } else {
        Err(EncodeError::Nvenc {
            call,
            status,
            detail,
        })
    }
}
