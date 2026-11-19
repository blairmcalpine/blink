//! Where encoded audio packets go.

use blink_ring::media::PacketMeta;

/// Receives encoded AAC frames on the audio thread.
///
/// `blink-app` implements this by locking its shared ring (`SharedRing`) and
/// pushing the packet, which copies the payload into the ring's preallocated
/// arena. Keep the call short and never block on I/O: the audio thread drains
/// the WASAPI buffer between calls, and that buffer holds about half a second.
/// Any closure `FnMut(PacketMeta, &[u8]) + Send + 'static` is a sink.
pub trait AudioSink: Send + 'static {
    /// Called once per AAC frame, in presentation order.
    ///
    /// `meta.kind` is [`StreamKind::Audio`](blink_ring::media::StreamKind),
    /// `meta.keyframe` is `true`, and `payload` is one raw AAC-LC access unit
    /// (no ADTS header) that is only valid for the duration of the call.
    fn packet(&mut self, meta: PacketMeta, payload: &[u8]);
}

impl<F> AudioSink for F
where
    F: FnMut(PacketMeta, &[u8]) + Send + 'static,
{
    fn packet(&mut self, meta: PacketMeta, payload: &[u8]) {
        self(meta, payload);
    }
}
