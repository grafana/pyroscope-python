//! 3.15 keeps the thread state in a thread-local variable that is not exported. [`init`]
//! locates it so that `pyroscope_python_unwind::thread_state::macos` reads it without calling
//! the variable's accessor. Nothing here is async-signal-safe.
//!
//! Compiled code reaches a thread-local variable through its descriptor in a
//! `thread_local_variables` section: it calls the descriptor's first word, dyld's
//! `_tlv_get_addr`, with the descriptor as argument. dyld gives every image one pthread key,
//! and a thread's value for that key is a block holding all of the image's thread-local
//! variables. The block is `malloc`ed on the thread's first access to any of them and freed by
//! the key's destructor at thread exit, so the accessor can allocate in a thread that has not
//! used the image's variables, or that is exiting.
//!
//! `pthread_getspecific` is a load from the thread's slot array and never allocates, so a
//! variable is read as the word at its offset in `pthread_getspecific(key)`, when that is not
//! null.
//!
//! The descriptor layout is private to dyld and changed in 2025. Only the key is taken from
//! it, from a field both layouts share; [`locate`] derives the offset from dyld's own answer
//! and accepts it only if, on two threads, it is the same and lies in the `malloc` block that
//! the key holds.
//!
//! The module is compiled for every CPython version, so that the search is tested on all of
//! them, but [`init`] is used only on 3.15.

use crate::ThreadStateError;
use core::ffi::{CStr, c_char, c_void};
use core::mem::size_of;
use core::ptr;
use pyroscope_python_unwind::Error;
use pyroscope_python_unwind::thread_state::macos::{self as reader, KEYS_END, Location, block};

/// Locates CPython's thread-local thread state and stores it in
/// `pyroscope_python_unwind::thread_state::macos`, so that the handler reads it without
/// calling into CPython or dyld. Until this succeeds the handler finds no thread state. The
/// location is cached for the process.
///
/// Briefly releases the GIL and spawns a short-lived thread.
///
/// # Safety
/// The interpreter must be initialized and kindasafe recovery must be installed.
pub unsafe fn init() -> Result<(), ThreadStateError> {
    if reader::location().is_some() {
        return Ok(());
    }
    let image = image_of(pyo3_ffi::PyGILState_GetThisThreadState as *const () as usize)
        .ok_or(ThreadStateError::NoTlsSegment)?;
    // SAFETY: the interpreter is initialized. The thread is detached during the search
    // because the attached thread state is also cached in a second thread-local variable.
    let descriptor = unsafe {
        let gil = pyo3_ffi::PyGILState_Ensure();
        let tstate = pyo3_ffi::PyGILState_GetThisThreadState() as usize;
        let saved = pyo3_ffi::PyEval_SaveThread();
        let descriptor = find_value(image, tstate);
        pyo3_ffi::PyEval_RestoreThread(saved);
        pyo3_ffi::PyGILState_Release(gil);
        descriptor
    }
    .ok_or(ThreadStateError::VariableNotFound)?;
    let location = locate(image, descriptor).ok_or(ThreadStateError::UnknownTlsLayout)?;
    // SAFETY: `locate` verified the location on two threads. A concurrent call may have
    // stored its own verified location first; either one holds.
    unsafe { reader::set_location(location) };
    Ok(())
}

const S_THREAD_LOCAL_VARIABLES: u32 = 0x13;
const SECTION_TYPE: u32 = 0xff;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_SEGMENT_64: u32 = 0x19;

/// `struct mach_header_64`.
#[repr(C)]
struct Header {
    magic: u32,
    _cpu: [u32; 3],
    ncmds: u32,
    _rest: [u32; 3],
}

/// `struct load_command`.
#[repr(C)]
struct Command {
    cmd: u32,
    cmdsize: u32,
}

/// `struct segment_command_64`.
#[repr(C)]
struct Segment {
    _command: Command,
    segname: [c_char; 16],
    vmaddr: u64,
    _map: [u64; 3],
    _protection: [u32; 2],
    nsects: u32,
    _flags: u32,
}

/// `struct section_64`.
#[repr(C)]
struct Section {
    _names: [c_char; 32],
    addr: u64,
    size: u64,
    _file: [u32; 4],
    flags: u32,
    _reserved: [u32; 3],
}

/// A thread-local variable's entry in a `thread_local_variables` section, after dyld has
/// prepared it. The words after the key differ between dyld versions.
#[repr(C)]
pub struct Descriptor {
    address: unsafe extern "C" fn(*const Descriptor) -> *mut c_void,
    key: u32,
    _private: [u32; 3],
}

/// A loaded image that defines thread-local variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    header: *const Header,
    /// The pthread key of the image's block.
    pub key: u32,
}

fn commands(header: *const Header) -> impl Iterator<Item = *const Command> {
    // SAFETY: `header` is the Mach-O header of a loaded image, which its load commands follow.
    let (count, first) = unsafe { ((*header).ncmds, header.add(1).cast::<Command>()) };
    core::iter::successors(Some(first), |&command| {
        // SAFETY: dyld accepted the image, so every command is `cmdsize` bytes long.
        Some(unsafe { command.byte_add((*command).cmdsize as usize) })
    })
    .take(count as usize)
}

fn segments(header: *const Header) -> impl Iterator<Item = *const Segment> {
    commands(header)
        // SAFETY: `commands` yields the image's load commands.
        .filter(|&command| unsafe { (*command).cmd } == LC_SEGMENT_64)
        .map(<*const Command>::cast)
}

fn descriptors(header: *const Header) -> impl Iterator<Item = *const Descriptor> {
    let slide = segments(header)
        // SAFETY: `segname` is NUL-padded and "__TEXT" is shorter than the field.
        .find(|&segment| unsafe { CStr::from_ptr((*segment).segname.as_ptr()) } == c"__TEXT")
        // SAFETY: `segments` yields the image's segment commands.
        .map(|segment| (header as usize).wrapping_sub(unsafe { (*segment).vmaddr } as usize));
    segments(header)
        .flat_map(|segment| {
            // SAFETY: a segment command is followed by its `nsects` sections.
            unsafe {
                core::slice::from_raw_parts(
                    segment.add(1).cast::<Section>(),
                    (*segment).nsects as usize,
                )
            }
        })
        .filter(|section| section.flags & SECTION_TYPE == S_THREAD_LOCAL_VARIABLES)
        .flat_map(move |section| {
            let first = slide.map(|slide| slide.wrapping_add(section.addr as usize));
            let count = section.size as usize / size_of::<Descriptor>();
            (0..count)
                .filter_map(move |index| Some((first? + index * size_of::<Descriptor>()) as _))
        })
}

/// The image mapped at `address`, if it defines thread-local variables that all use one valid
/// pthread key. Not async-signal-safe.
pub fn image_of(address: usize) -> Option<Image> {
    // SAFETY: dladdr accepts any address and fills `info` when it returns non-zero.
    let header = unsafe {
        let mut info: libc::Dl_info = core::mem::zeroed();
        if libc::dladdr(address as *const c_void, &mut info) == 0 {
            return None;
        }
        info.dli_fbase as *const Header
    };
    // SAFETY: dladdr returned the header of a loaded image.
    if header.is_null() || unsafe { (*header).magic } != MH_MAGIC_64 {
        return None;
    }
    // SAFETY: dyld prepared the descriptors when it loaded the image.
    let mut keys = descriptors(header).map(|descriptor| unsafe { (*descriptor).key });
    let key = keys.next()?;
    (key != 0 && key < KEYS_END && keys.all(|other| other == key)).then_some(Image { header, key })
}

#[inline(always)]
fn read(address: usize) -> Result<usize, Error> {
    // SAFETY: a pointer-sized load under kindasafe's recovery preconditions, documented at
    // crate level. No Rust reference to the source memory is created.
    unsafe { kindasafe::u64(address as u64) }
        .map(|word| word as usize)
        .map_err(|_| Error::Read)
}

/// The variable's address in the calling thread, allocating the image's block as a first
/// access would. Not async-signal-safe.
fn address(descriptor: *const Descriptor) -> usize {
    // SAFETY: `descriptor` is a prepared descriptor of a loaded image; this is the call
    // compiled code makes to reach the variable.
    unsafe { ((*descriptor).address)(descriptor) as usize }
}

/// The only thread-local variable of `image` that holds `value` in the calling thread. `None`
/// if no variable or several variables hold it. Not async-signal-safe.
pub fn find_value(image: Image, value: usize) -> Option<*const Descriptor> {
    let mut holders =
        descriptors(image.header).filter(|&descriptor| read(address(descriptor)) == Ok(value));
    let descriptor = holders.next()?;
    holders.next().is_none().then_some(descriptor)
}

struct Probe {
    image: Image,
    descriptor: *const Descriptor,
    /// The variable's offset in the `malloc` block the pthread key holds, if it is inside one.
    offset: Option<usize>,
}

impl Probe {
    fn run(&mut self) {
        let address = address(self.descriptor);
        self.offset = block(self.image.key)
            .ok()
            .map(|block| {
                // SAFETY: malloc_size accepts any address and returns 0 for one that does not
                // start an allocation.
                let size = unsafe { libc::malloc_size(block as *const c_void) };
                (address.wrapping_sub(block), size)
            })
            .filter(|&(offset, size)| {
                offset
                    .checked_add(size_of::<usize>())
                    .is_some_and(|end| end <= size)
            })
            .map(|(offset, _)| offset);
    }

    fn on_new_thread(&mut self) -> Option<()> {
        extern "C" fn start(probe: *mut c_void) -> *mut c_void {
            // SAFETY: the spawning thread keeps the probe alive until it has joined this one.
            unsafe { &mut *(probe as *mut Probe) }.run();
            ptr::null_mut()
        }
        // SAFETY: `start` only uses `self`, which outlives the joined thread.
        unsafe {
            let mut thread: libc::pthread_t = core::mem::zeroed();
            let probe = ptr::from_mut(self).cast();
            if libc::pthread_create(&mut thread, ptr::null(), start, probe) != 0 {
                return None;
            }
            (libc::pthread_join(thread, ptr::null_mut()) == 0).then_some(())
        }
    }
}

/// Finds where the variable described by `descriptor` in `image` lives in every thread.
/// Spawns a short-lived thread to compare with the calling one. `None` if the pthread key
/// does not hold a `malloc` block with the variable at the same offset on both threads. Not
/// async-signal-safe.
pub fn locate(image: Image, descriptor: *const Descriptor) -> Option<Location> {
    let mut probe = Probe {
        image,
        descriptor,
        offset: None,
    };
    probe.run();
    let here = probe.offset.take()?;
    probe.on_new_thread()?;
    (probe.offset == Some(here)).then_some(Location {
        key: image.key,
        offset: here,
    })
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::cell::Cell;
    use std::ffi::CString;
    use std::format;
    use std::process;
    use std::sync::mpsc;
    use std::thread;
    use std::thread_local;

    fn init() {
        // SAFETY: tests preserve kindasafe's handlers and do not install other handlers.
        unsafe { kindasafe_init::init() }.unwrap();
    }

    thread_local! {
        static SLOT: Cell<usize> = const { Cell::new(0) };
    }

    #[test]
    fn variable_of_the_executable_is_read_from_its_block() {
        init();
        let image = image_of(init as fn() as usize).unwrap();
        let marker = 0x2390_0000_0000_0239;
        SLOT.set(marker);
        let descriptor = find_value(image, marker).unwrap();
        assert_eq!(address(descriptor), SLOT.with(Cell::as_ptr) as usize);
        let location = locate(image, descriptor).unwrap();
        assert_eq!(location.read(), Ok(marker));
        let read = thread::spawn(move || {
            SLOT.set(239);
            location.read()
        });
        assert_eq!(read.join().unwrap(), Ok(239));
    }

    #[test]
    fn duplicated_and_missing_values_are_not_located() {
        init();
        thread_local! {
            static FIRST: Cell<usize> = const { Cell::new(0) };
            static SECOND: Cell<usize> = const { Cell::new(0) };
        }
        let image = image_of(init as fn() as usize).unwrap();
        let marker = 0x2391_0000_0000_0239;
        assert_eq!(find_value(image, marker), None);
        FIRST.set(marker);
        let descriptor = find_value(image, marker).unwrap();
        assert_eq!(address(descriptor), FIRST.with(Cell::as_ptr) as usize);
        SECOND.set(marker);
        assert_eq!(find_value(image, marker), None);
    }

    const LIBRARY: &str = "
        __thread unsigned long padding[3];
        __thread unsigned long slot;
        __thread unsigned long initialized = 0x2393000000000239;
        void set(unsigned long value) { slot = value; }
        unsigned long address(void) { return (unsigned long)&slot; }
        unsigned long get_initialized(void) { return initialized; }
    ";

    #[derive(Clone, Copy)]
    struct Library {
        handle: usize,
    }

    impl Library {
        fn load(name: &str, source: &str) -> Library {
            let dir = std::env::temp_dir();
            let source_path = dir.join(format!("pyroscope_tlv_{}_{name}.c", process::id()));
            let library = source_path.with_extension("dylib");
            std::fs::write(&source_path, source).unwrap();
            let arch = if cfg!(target_arch = "x86_64") {
                "x86_64"
            } else {
                "arm64"
            };
            let output = process::Command::new("cc")
                .args(["-arch", arch, "-dynamiclib", "-O1", "-o"])
                .args([&library, &source_path])
                .output()
                .expect("the test needs a C compiler named cc");
            assert!(output.status.success(), "{output:?}");
            let path = CString::new(library.to_str().unwrap()).unwrap();
            // SAFETY: loads the library built above; it has no constructors.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW) };
            assert!(!handle.is_null(), "dlopen failed for {library:?}");
            std::fs::remove_file(&source_path).unwrap();
            std::fs::remove_file(&library).unwrap();
            Library {
                handle: handle as usize,
            }
        }

        fn symbol(&self, name: &str) -> usize {
            let name = CString::new(name).unwrap();
            // SAFETY: `handle` comes from a successful dlopen.
            let symbol = unsafe { libc::dlsym(self.handle as *mut c_void, name.as_ptr()) };
            assert!(!symbol.is_null());
            symbol as usize
        }

        fn image(&self) -> Option<Image> {
            image_of(self.symbol("set"))
        }

        fn set(&self, value: usize) {
            // SAFETY: `set` has this signature in `LIBRARY`.
            let set: extern "C" fn(usize) = unsafe { core::mem::transmute(self.symbol("set")) };
            set(value);
        }

        fn call(&self, name: &str) -> usize {
            // SAFETY: `address` and `get_initialized` have this signature in `LIBRARY`.
            let function: extern "C" fn() -> usize =
                unsafe { core::mem::transmute(self.symbol(name)) };
            function()
        }
    }

    /// A thread that keeps running across the `dlopen` of a library.
    struct Worker {
        calls: mpsc::Sender<Box<dyn FnOnce() -> Result<usize, Error> + Send>>,
        results: mpsc::Receiver<Result<usize, Error>>,
    }

    impl Worker {
        fn spawn() -> Worker {
            let (calls, pending) =
                mpsc::channel::<Box<dyn FnOnce() -> Result<usize, Error> + Send>>();
            let (done, results) = mpsc::channel();
            thread::spawn(move || {
                for call in pending {
                    done.send(call()).unwrap();
                }
            });
            Worker { calls, results }
        }

        fn run(
            &self,
            call: impl FnOnce() -> Result<usize, Error> + Send + 'static,
        ) -> Result<usize, Error> {
            self.calls.send(Box::new(call)).unwrap();
            self.results.recv().unwrap()
        }
    }

    #[test]
    fn dlopened_image_is_read_only_on_threads_that_have_its_block() {
        init();
        let before = Worker::spawn();
        let library = Library::load("variables", LIBRARY);
        let image = library.image().unwrap();
        let marker = 0x2392_0000_0000_0239;
        library.set(marker);
        let descriptor = find_value(image, marker).unwrap();
        assert_eq!(address(descriptor), library.call("address"));
        let location = locate(image, descriptor).unwrap();
        assert_eq!(location.read(), Ok(marker));

        // Threads that never accessed the image's variables have no block; reading must not
        // allocate one.
        for _ in 0..2 {
            assert_eq!(
                before.run(move || location.read()),
                Err(Error::NoThreadState)
            );
            assert_eq!(
                before.run(move || block(image.key)),
                Err(Error::NoThreadState)
            );
        }
        assert_eq!(
            thread::spawn(move || location.read()).join().unwrap(),
            Err(Error::NoThreadState)
        );
        for (worker, value) in [(&before, 239), (&Worker::spawn(), 2390)] {
            let read = worker.run(move || {
                library.set(value);
                location.read()
            });
            assert_eq!(read, Ok(value));
        }
    }

    #[test]
    fn initialized_variable_is_located_by_its_initial_value() {
        init();
        let library = Library::load("initialized", LIBRARY);
        let image = library.image().unwrap();
        let initial = 0x2393_0000_0000_0239;
        assert_eq!(library.call("get_initialized"), initial);
        let descriptor = find_value(image, initial).unwrap();
        let location = locate(image, descriptor).unwrap();
        assert_eq!(location.read(), Ok(initial));
        // The block of a new thread starts as a copy of the image's initial content.
        let read = thread::spawn(move || {
            let before = location.read();
            library.call("get_initialized");
            (before, location.read())
        });
        assert_eq!(
            read.join().unwrap(),
            (Err(Error::NoThreadState), Ok(initial))
        );
    }

    #[test]
    fn images_without_thread_local_variables_are_rejected() {
        assert_eq!(image_of(0), None);
        let library = Library::load("plain", "int set(void) { return 239; }");
        assert_eq!(library.image(), None);
    }
}
