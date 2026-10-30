/// Field offsets of one CPython minor version, default (GIL) build, 64-bit.
pub trait Layout: Sync + 'static {
    /// Offset in `PyThreadState` of the current frame, or of `cframe` if [`Layout::CFRAME_FRAME`] is set.
    const THREAD_FRAME: usize;
    /// Offset in `_PyCFrame` of the current frame.
    const CFRAME_FRAME: Option<usize>;
    const EXECUTABLE: usize;
    const EXECUTABLE_TAG_BITS: usize;
    const PREVIOUS: usize;
    const OWNER: Option<usize>;
    /// Bit `1 << owner` is set for owners that are not Python frames.
    const SKIP_OWNERS: u8;
}

const FRAME_OWNED_BY_CSTACK_3_12: u8 = 3;
const FRAME_OWNED_BY_INTERPRETER: u8 = 3;
const FRAME_OWNED_BY_CSTACK: u8 = 4;

pub struct V3_11;

impl Layout for V3_11 {
    const THREAD_FRAME: usize = 56;
    const CFRAME_FRAME: Option<usize> = Some(8);
    const EXECUTABLE: usize = 32;
    const EXECUTABLE_TAG_BITS: usize = 0;
    const PREVIOUS: usize = 48;
    const OWNER: Option<usize> = None;
    const SKIP_OWNERS: u8 = 0;
}

pub struct V3_12;

impl Layout for V3_12 {
    const THREAD_FRAME: usize = 56;
    const CFRAME_FRAME: Option<usize> = Some(0);
    const EXECUTABLE: usize = 0;
    const EXECUTABLE_TAG_BITS: usize = 0;
    const PREVIOUS: usize = 8;
    const OWNER: Option<usize> = Some(70);
    const SKIP_OWNERS: u8 = 1 << FRAME_OWNED_BY_CSTACK_3_12;
}

pub struct V3_13;

impl Layout for V3_13 {
    const THREAD_FRAME: usize = 72;
    const CFRAME_FRAME: Option<usize> = None;
    const EXECUTABLE: usize = 0;
    const EXECUTABLE_TAG_BITS: usize = 0;
    const PREVIOUS: usize = 8;
    const OWNER: Option<usize> = Some(70);
    const SKIP_OWNERS: u8 = 1 << FRAME_OWNED_BY_CSTACK_3_12;
}

pub struct V3_14;

impl Layout for V3_14 {
    const THREAD_FRAME: usize = 72;
    const CFRAME_FRAME: Option<usize> = None;
    const EXECUTABLE: usize = 0;
    const EXECUTABLE_TAG_BITS: usize = 3;
    const PREVIOUS: usize = 8;
    const OWNER: Option<usize> = Some(74);
    const SKIP_OWNERS: u8 = (1 << FRAME_OWNED_BY_INTERPRETER) | (1 << FRAME_OWNED_BY_CSTACK);
}
