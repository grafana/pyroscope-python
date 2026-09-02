// Does #[repr(C)] stop rustc from using a bool's spare bit patterns as the
// niche discriminant of an enclosing Result? (No.)
use std::mem::size_of;

// ~ remoteprocess::Error: 32 bytes, several variants, one owning a String.
#[allow(dead_code)]
enum Err32 { A(String), B(std::io::Error), C(u64), D(i32) }

// ~ py-spy's v3_11_0::_PyInterpreterFrame: repr(C), 80 bytes, with a bool.
#[repr(C)]
#[derive(Copy, Clone)]
#[allow(dead_code)]
struct FrameWithBool {
    p: [*mut u8; 8],   // 64
    stacktop: i32,     // 64..68
    is_entry: bool,    // 68        <- 254 spare bit patterns
    owner: i8,         // 69
    // 2 bytes padding
    localsplus: u64,   // 72..80
}

// Identical layout, bool -> u8 (what 3.12+ has as c_char in that slot).
#[repr(C)]
#[derive(Copy, Clone)]
#[allow(dead_code)]
struct FrameNoBool {
    p: [*mut u8; 8],
    stacktop: i32,
    is_entry: u8,      // 68        <- no spare bit patterns
    owner: i8,
    localsplus: u64,
}

fn main() {
    println!("size_of::<Err32>() = {} (fits inside the Ok payload)\n", size_of::<Err32>());
    println!("repr(C), field offsets identical in both structs:");
    println!("  size_of::<FrameWithBool>()               = {}", size_of::<FrameWithBool>());
    println!("  size_of::<FrameNoBool>()                 = {}", size_of::<FrameNoBool>());
    println!();
    println!("but the enum wrapping them is laid out differently:");
    println!("  size_of::<Result<FrameWithBool, Err32>>() = {}  <- niche: tag hidden in the bool",
             size_of::<Result<FrameWithBool, Err32>>());
    println!("  size_of::<Result<FrameNoBool,   Err32>>() = {}  <- dedicated tag word",
             size_of::<Result<FrameNoBool, Err32>>());
    println!();

    // And the concrete consequence: an Ok whose bool byte is the niche value
    // reads back as Err.
    let mut bytes = [0u8; size_of::<FrameWithBool>()];
    bytes[68] = 2;                       // what a stale read of the target gives
    let ok: Result<FrameWithBool, Err32> =
        Ok(unsafe { std::ptr::read(bytes.as_ptr() as *const FrameWithBool) });
    println!("Ok(frame) with byte 68 == 2 is observed as: {}",
             if ok.is_err() { "Err  <-- the bug" } else { "Ok" });
    std::mem::forget(ok);                // do not let it drop the fake Err
}
