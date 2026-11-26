//! BGRA to NV12 on the D3D11 video processor.
//!
//! The conversion reads the duplicated desktop surface directly and writes
//! the encoder's NV12 input, so one pass replaces both a BGRA copy and
//! NVENC's own RGB conversion (which runs on CUDA cores), and the texture the
//! encoder reads is 1.5 bytes per pixel instead of 4.

use core::mem::ManuallyDrop;

use windows::Win32::Graphics::Direct3D11::{
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL,
};
use windows::core::Interface;

use crate::error::{CaptureError, Context};

/// Converts full-range BGRA desktop images into one NV12 texture: BT.709
/// studio range with left-sited chroma, which is what the encoder labels
/// the stream as.
#[derive(Debug)]
pub(crate) struct Nv12Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    output: ID3D11VideoProcessorOutputView,
    /// Views of the last desktop surfaces, newest first. DXGI hands back the
    /// same surface for every frame of a session in practice (not by
    /// contract, so two are cached in case a driver alternates), so views
    /// are made about once per session. Each surface is held so its address
    /// cannot be reused; holding it across `ReleaseFrame` only keeps the
    /// object alive and never touches its contents outside an acquired
    /// frame. The references must be gone before the next `DuplicateOutput`,
    /// see [`forget_input`](Self::forget_input).
    inputs: [Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>; 2],
}

impl Nv12Converter {
    /// Sets up conversion of `width` x `height` BGRA images into `target`,
    /// an NV12 render-target texture of the same size on `device`.
    pub(crate) fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        target: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Self, CaptureError> {
        let video_device: ID3D11VideoDevice = device.cast().context("query the video device")?;
        let video_context: ID3D11VideoContext1 =
            context.cast().context("query the video context")?;
        // The rates only guide the driver; there is no rate conversion.
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: `video_device` is live and `content_desc` is a valid descriptor
        // that outlives the call.
        let enumerator =
            unsafe { video_device.CreateVideoProcessorEnumerator(&raw const content_desc) }
                .context("create a video processor enumerator")?;
        require(&enumerator, DXGI_FORMAT_B8G8R8A8_UNORM, true)?;
        require(&enumerator, DXGI_FORMAT_NV12, false)?;
        // SAFETY: `enumerator` is live and came from `video_device`; index 0
        // always exists.
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }
            .context("create a video processor")?;

        let view = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output = None;
        // SAFETY: `target` is a live single-mip render-target texture on this
        // device, `enumerator` is live, and `view` and `output` are valid for
        // the call.
        unsafe {
            video_device.CreateVideoProcessorOutputView(
                target,
                &enumerator,
                &raw const view,
                Some(&raw mut output),
            )
        }
        .context("create the NV12 output view")?;
        let output = output.ok_or(CaptureError::NullOutput {
            call: "CreateVideoProcessorOutputView",
            what: "view",
        })?;

        // SAFETY: `processor` is live and stream 0 exists.
        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
        };
        // Desktop pixels are converted as they are: no driver denoising,
        // edge enhancement, or color "improvements".
        // SAFETY: as above.
        unsafe { video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false) };
        // SAFETY: as above.
        unsafe {
            video_context.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
        };
        // SAFETY: `processor` is live.
        unsafe {
            video_context.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
        };
        Ok(Self {
            video_device,
            video_context,
            enumerator,
            processor,
            output,
            inputs: [None, None],
        })
    }

    /// Queues the conversion of `source`, a BGRA texture of the converter's
    /// size on its device, into the target texture.
    pub(crate) fn convert(&mut self, source: &ID3D11Texture2D) -> Result<(), CaptureError> {
        let input = self.input_view(source)?.clone();
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input)),
            ..D3D11_VIDEO_PROCESSOR_STREAM::default()
        };
        let streams = [stream];
        // SAFETY: the processor, the output view, and the input view in the
        // one enabled stream are live and belong to this device; the source
        // stays alive until the duplication frame is released, after this
        // call is queued.
        let blt = unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, &self.output, 0, &streams)
        };
        let [stream] = streams;
        drop(ManuallyDrop::into_inner(stream.pInputSurface));
        blt.context("convert the desktop image to NV12")
    }

    /// Drops the cached view of the desktop surface. Call it before a new
    /// duplication session is created, which needs every reference to the
    /// old session's surfaces gone.
    pub(crate) fn forget_input(&mut self) {
        self.inputs = [None, None];
    }

    fn input_view(
        &mut self,
        source: &ID3D11Texture2D,
    ) -> Result<&ID3D11VideoProcessorInputView, CaptureError> {
        let is_source = |slot: &Option<(ID3D11Texture2D, _)>| {
            slot.as_ref()
                .is_some_and(|(surface, _)| surface.as_raw() == source.as_raw())
        };
        if !is_source(&self.inputs[0]) {
            if is_source(&self.inputs[1]) {
                self.inputs.swap(0, 1);
            } else {
                let view = self.create_input_view(source)?;
                self.inputs.swap(0, 1);
                self.inputs[0] = Some((source.clone(), view));
            }
        }
        self.inputs[0]
            .as_ref()
            .map(|(_, view)| view)
            .ok_or(CaptureError::Invariant("the desktop input view is missing"))
    }

    fn create_input_view(
        &self,
        source: &ID3D11Texture2D,
    ) -> Result<ID3D11VideoProcessorInputView, CaptureError> {
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut view = None;
        // SAFETY: `source` is a live BGRA texture on this device,
        // `enumerator` is live, and `desc` and `view` are valid for the call.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                source,
                &self.enumerator,
                &raw const desc,
                Some(&raw mut view),
            )
        }
        .context("create the desktop input view")?;
        tracing::debug!("created a video processor view of the desktop surface");
        view.ok_or(CaptureError::NullOutput {
            call: "CreateVideoProcessorInputView",
            what: "view",
        })
    }
}

/// Fails unless the video processor takes `format` as input (or produces it
/// as output).
fn require(
    enumerator: &ID3D11VideoProcessorEnumerator,
    format: DXGI_FORMAT,
    input: bool,
) -> Result<(), CaptureError> {
    let flag = if input {
        D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT
    } else {
        D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT
    };
    // SAFETY: `enumerator` is live; the call only reports support flags.
    let support = unsafe { enumerator.CheckVideoProcessorFormat(format) }
        .context("query video processor format support")?;
    if support & flag.0.cast_unsigned() == 0 {
        return Err(CaptureError::VideoProcessorFormat { format, input });
    }
    Ok(())
}
