use crate::encode::pprof::PprofBuilderType;
use crate::encode::pprof::ffi::{FFIFrame, FFISampleValues};

#[unsafe(no_mangle)]
pub extern "C" fn pyroscope_push_sample(
    builder_type: PprofBuilderType,
    frames: *const FFIFrame,
    len: usize,
    values: *const FFISampleValues,
) {
    if frames.is_null() || len == 0 || values.is_null() {
        return;
    }
    let frames = unsafe { std::slice::from_raw_parts(frames, len) };
    let values = unsafe { &*values };
    match builder_type {
        PprofBuilderType::Memory => crate::memory::push_sample(frames, values),
        PprofBuilderType::CpuWall => crate::stack::push_sample(frames, values),
        PprofBuilderType::OnCpu => crate::stack::push_oncpu_sample(frames, values),
    }
}
