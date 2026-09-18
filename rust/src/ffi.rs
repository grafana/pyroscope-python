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
        // py-spy samples never cross the FFI boundary.
        PprofBuilderType::Cpu => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::pprof::StringID;

    fn frames() -> Vec<FFIFrame> {
        vec![FFIFrame {
            function_name: StringID::empty_ffi_string(),
            file_name: StringID::empty_ffi_string(),
            line: 1,
        }]
    }

    fn values() -> FFISampleValues {
        FFISampleValues {
            cpu_time: 7,
            wall_time: 9,
            alloc_space: 300,
            alloc_count: 2,
            heap_space: 100,
            heap_count: 1,
        }
    }

    #[test]
    fn push_sample_boundary_is_a_noop() {
        let frames = frames();
        let values = values();

        pyroscope_push_sample(
            PprofBuilderType::Memory,
            std::ptr::null(),
            frames.len(),
            &values,
        );
        pyroscope_push_sample(
            PprofBuilderType::Memory,
            frames.as_ptr(),
            frames.len(),
            std::ptr::null(),
        );
        pyroscope_push_sample(PprofBuilderType::Memory, frames.as_ptr(), 0, &values);

        // py-spy has no accumulator behind this boundary; a valid push must
        // still be a no-op. The CpuWall route is covered in crate::stack.
        pyroscope_push_sample(
            PprofBuilderType::Cpu,
            frames.as_ptr(),
            frames.len(),
            &values,
        );
    }
}
