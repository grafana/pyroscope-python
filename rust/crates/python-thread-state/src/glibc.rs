//! 3.15 keeps the thread state in a thread-local variable that is not exported. [`init`]
//! locates it so that `pyroscope_python_unwind::thread_state::glibc` reads it with plain
//! memory loads, without entering the dynamic loader. Nothing here is async-signal-safe.
//!
//! The TCB and DTV layouts are private to glibc. [`locate`] accepts a location only after
//! checking it on two threads against the loader's own answer.
//!
//! The module is compiled for every CPython version, so that the search is tested on all of
//! them, but [`init`] is used only on 3.15.

use crate::{ThreadStateAccess, ThreadStateError};
use core::ffi::{c_int, c_void};
use core::mem::size_of;
use core::ptr;
use pyroscope_python_unwind::thread_state::glibc::{
    self as reader, Location, dtv_block, thread_pointer,
};

fn access(location: Location) -> ThreadStateAccess {
    match location {
        Location::Fixed { .. } => ThreadStateAccess::FixedOffset,
        Location::Dtv { .. } => ThreadStateAccess::Dtv,
    }
}

/// Locates CPython's thread-local thread state and stores it in
/// `pyroscope_python_unwind::thread_state::glibc`, so that the handler reads it without
/// calling into CPython or the dynamic loader. Until this succeeds the handler finds no thread
/// state. The location is cached for the process.
///
/// Briefly releases the GIL and spawns a short-lived thread.
///
/// # Safety
/// The interpreter must be initialized and kindasafe recovery must be installed.
pub unsafe fn init() -> Result<ThreadStateAccess, ThreadStateError> {
    if let Some(location) = reader::location() {
        return Ok(access(location));
    }
    let module = module_of(pyo3_ffi::PyGILState_GetThisThreadState as *const () as usize)
        .ok_or(ThreadStateError::NoTlsSegment)?;
    // SAFETY: the interpreter is initialized. The thread is detached during the search
    // because the attached thread state is also cached in a second thread-local variable.
    let offset = unsafe {
        let gil = pyo3_ffi::PyGILState_Ensure();
        let tstate = pyo3_ffi::PyGILState_GetThisThreadState() as usize;
        let saved = pyo3_ffi::PyEval_SaveThread();
        let offset = find_value(module, tstate);
        pyo3_ffi::PyEval_RestoreThread(saved);
        pyo3_ffi::PyGILState_Release(gil);
        offset
    }
    .ok_or(ThreadStateError::VariableNotFound)?;
    let location = locate(module, offset).ok_or(ThreadStateError::UnknownTlsLayout)?;
    // SAFETY: `locate` verified the location on two threads. A concurrent call may have
    // stored its own verified location first; either one holds.
    unsafe { reader::set_location(location) };
    Ok(access(location))
}

#[repr(C)]
struct TlsIndex {
    module: usize,
    offset: usize,
}

unsafe extern "C" {
    fn __tls_get_addr(index: *const TlsIndex) -> *mut c_void;
}

/// A loaded module with a TLS segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Module {
    pub id: usize,
    /// Size of the module's TLS block.
    pub size: usize,
}

struct Search {
    address: usize,
    found: Option<Module>,
}

unsafe extern "C" fn visit(info: *mut libc::dl_phdr_info, _: usize, data: *mut c_void) -> c_int {
    // SAFETY: dl_iterate_phdr passes a valid info and the `Search` given to it.
    let (info, search) = unsafe { (&*info, &mut *(data as *mut Search)) };
    if info.dlpi_phdr.is_null() {
        return 0;
    }
    // SAFETY: the loader describes `dlpi_phnum` program headers at `dlpi_phdr`.
    let headers = unsafe { core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
    let base = info.dlpi_addr as usize;
    let contains = headers.iter().any(|header| {
        header.p_type == libc::PT_LOAD
            && search
                .address
                .wrapping_sub(base.wrapping_add(header.p_vaddr as usize))
                < header.p_memsz as usize
    });
    if !contains {
        return 0;
    }
    search.found = headers
        .iter()
        .find(|header| header.p_type == libc::PT_TLS)
        .map(|header| Module {
            id: info.dlpi_tls_modid,
            size: header.p_memsz as usize,
        })
        .filter(|module| module.id != 0);
    1
}

/// The module mapped at `address`, if it has a TLS segment. Not async-signal-safe.
pub fn module_of(address: usize) -> Option<Module> {
    let mut search = Search {
        address,
        found: None,
    };
    // SAFETY: `visit` only reads the loader's info and writes to `search`.
    unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut search).cast()) };
    search.found
}

#[inline(always)]
fn read(address: usize) -> Option<usize> {
    // SAFETY: a pointer-sized load under kindasafe's recovery preconditions, documented at
    // crate level. No Rust reference to the source memory is created.
    unsafe { kindasafe::u64(address as u64) }
        .ok()
        .map(|word| word as usize)
}

/// The calling thread's block of `module`, allocated as a first access would. Not
/// async-signal-safe.
fn block(module: usize) -> usize {
    let index = TlsIndex { module, offset: 0 };
    // SAFETY: `module` is the id of a loaded module, from `module_of`.
    unsafe { __tls_get_addr(&index) as usize }
}

/// The offset in `module`'s block of the only pointer-sized slot of the calling thread that
/// holds `value`. `None` if no slot or several slots hold it. Not async-signal-safe.
pub fn find_value(module: Module, value: usize) -> Option<usize> {
    let block = block(module.id);
    let mut offsets = (0..=module.size.checked_sub(size_of::<usize>())?)
        .step_by(size_of::<usize>())
        .filter(|offset| read(block + offset) == Some(value));
    let offset = offsets.next()?;
    offsets.next().is_none().then_some(offset)
}

#[derive(Clone, Copy)]
struct Probe {
    /// Block address relative to the thread pointer.
    delta: isize,
    /// Whether the DTV records the block the loader returned.
    in_dtv: bool,
}

impl Probe {
    fn current(module: usize) -> Probe {
        let block = block(module);
        Probe {
            delta: block.wrapping_sub(thread_pointer()) as isize,
            in_dtv: dtv_block(module) == Some(block),
        }
    }

    fn on_new_thread(module: usize) -> Option<Probe> {
        struct Call {
            module: usize,
            probe: Option<Probe>,
        }
        extern "C" fn start(call: *mut c_void) -> *mut c_void {
            // SAFETY: the spawning thread keeps `Call` alive until it has joined this one.
            let call = unsafe { &mut *(call as *mut Call) };
            call.probe = Some(Probe::current(call.module));
            ptr::null_mut()
        }
        let mut call = Call {
            module,
            probe: None,
        };
        // SAFETY: `start` only uses `call`, which outlives the joined thread.
        unsafe {
            let mut thread: libc::pthread_t = 0;
            if libc::pthread_create(&mut thread, ptr::null(), start, (&raw mut call).cast()) != 0 {
                return None;
            }
            if libc::pthread_join(thread, ptr::null_mut()) != 0 {
                return None;
            }
        }
        call.probe
    }
}

/// Finds where the variable at `offset` in `module`'s block lives in every thread. Spawns a
/// short-lived thread to compare with the calling one. `None` if neither location holds on
/// both threads. Not async-signal-safe.
pub fn locate(module: Module, offset: usize) -> Option<Location> {
    let here = Probe::current(module.id);
    let there = Probe::on_new_thread(module.id)?;
    if here.delta == there.delta {
        Some(Location::Fixed {
            offset: here.delta.wrapping_add_unsigned(offset),
        })
    } else if here.in_dtv && there.in_dtv {
        Some(Location::Dtv {
            module: module.id,
            offset,
        })
    } else {
        None
    }
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::cell::Cell;
    use std::ffi::CString;
    use std::format;
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;
    use std::thread_local;

    fn init_recovery() {
        // SAFETY: tests preserve kindasafe's handlers and do not install other handlers.
        unsafe { kindasafe_init::init() }.unwrap();
    }

    thread_local! {
        static SLOT: Cell<usize> = const { Cell::new(0) };
    }

    #[test]
    fn startup_module_variable_is_at_a_fixed_offset() {
        init_recovery();
        let module = module_of(init_recovery as fn() as usize).unwrap();
        let marker = 0x2390_0000_0000_0239;
        SLOT.set(marker);
        let offset = find_value(module, marker).unwrap();
        assert_eq!(block(module.id) + offset, SLOT.with(Cell::as_ptr) as usize);
        let location = locate(module, offset).unwrap();
        assert!(matches!(location, Location::Fixed { .. }), "{location:?}");
        assert_eq!(location.read(), Some(marker));
        for value in [0, 239] {
            let read = thread::spawn(move || {
                if value != 0 {
                    SLOT.set(value);
                }
                location.read()
            });
            assert_eq!(read.join().unwrap(), Some(value));
        }
    }

    #[test]
    fn unmapped_addresses_and_short_blocks_are_rejected() {
        assert_eq!(module_of(0), None);
        let module = module_of(init_recovery as fn() as usize).unwrap();
        for size in [0, 7] {
            assert_eq!(find_value(Module { size, ..module }, 0), None);
        }
    }

    #[test]
    fn duplicated_and_missing_values_are_not_located() {
        init_recovery();
        thread_local! {
            static PAIR: Cell<[usize; 2]> = const { Cell::new([0; 2]) };
        }
        let module = module_of(init_recovery as fn() as usize).unwrap();
        let marker = 0x2391_0000_0000_0239;
        assert_eq!(find_value(module, marker), None);
        PAIR.set([marker, 0]);
        assert!(find_value(module, marker).is_some());
        PAIR.set([marker; 2]);
        assert_eq!(find_value(module, marker), None);
    }

    const LIBRARY: &str = "
        __thread unsigned long slots[3];
        void set(unsigned long value) { slots[1] = value; }
        unsigned long *address(void) { return &slots[1]; }
    ";

    #[derive(Clone, Copy)]
    struct Library {
        handle: usize,
        module: Module,
    }

    impl Library {
        /// Builds and `dlopen`s a library whose TLS accesses use `dialect`.
        fn load(name: &str, dialect: &str) -> Library {
            let dir = std::env::temp_dir();
            let source = dir.join(format!("pyroscope_tls_{}_{name}.c", std::process::id()));
            let library = source.with_extension("so");
            std::fs::write(&source, LIBRARY).unwrap();
            let output = Command::new("cc")
                .args(["-shared", "-fPIC", "-O1", "-ftls-model=global-dynamic"])
                .arg(format!("-mtls-dialect={dialect}"))
                .arg("-o")
                .args([&library, &source])
                .output()
                .expect("the test needs a C compiler named cc");
            assert!(output.status.success(), "{output:?}");
            let path = CString::new(library.to_str().unwrap()).unwrap();
            // SAFETY: loads the library built above; it has no constructors.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW) };
            assert!(!handle.is_null(), "dlopen failed for {library:?}");
            std::fs::remove_file(&source).unwrap();
            std::fs::remove_file(&library).unwrap();
            let handle = handle as usize;
            let module = module_of(symbol(handle, "set")).unwrap();
            Library { handle, module }
        }

        fn set(&self, value: usize) {
            // SAFETY: `set` has this signature in `LIBRARY`.
            let set: extern "C" fn(usize) =
                unsafe { core::mem::transmute(symbol(self.handle, "set")) };
            set(value);
        }

        fn address(&self) -> usize {
            // SAFETY: `address` has this signature in `LIBRARY`.
            let address: extern "C" fn() -> usize =
                unsafe { core::mem::transmute(symbol(self.handle, "address")) };
            address()
        }
    }

    fn symbol(handle: usize, name: &str) -> usize {
        let name = CString::new(name).unwrap();
        // SAFETY: `handle` comes from a successful dlopen.
        let symbol = unsafe { libc::dlsym(handle as *mut c_void, name.as_ptr()) };
        assert!(!symbol.is_null());
        symbol as usize
    }

    /// A thread that keeps running across the `dlopen` of a library.
    struct Worker {
        calls: mpsc::Sender<Box<dyn FnOnce() -> Option<usize> + Send>>,
        results: mpsc::Receiver<Option<usize>>,
    }

    impl Worker {
        fn spawn() -> Worker {
            let (calls, pending) = mpsc::channel::<Box<dyn FnOnce() -> Option<usize> + Send>>();
            let (done, results) = mpsc::channel();
            thread::spawn(move || {
                for call in pending {
                    done.send(call()).unwrap();
                }
            });
            Worker { calls, results }
        }

        fn run(&self, call: impl FnOnce() -> Option<usize> + Send + 'static) -> Option<usize> {
            self.calls.send(Box::new(call)).unwrap();
            self.results.recv().unwrap()
        }
    }

    fn locate_slot(library: &Library) -> Location {
        let marker = 0x2392_0000_0000_0239;
        library.set(marker);
        let offset = find_value(library.module, marker).unwrap();
        assert_eq!(block(library.module.id) + offset, library.address());
        let location = locate(library.module, offset).unwrap();
        assert_eq!(location.read(), Some(marker));
        location
    }

    #[cfg(target_arch = "x86_64")]
    const DIALECTS: (&str, &str) = ("gnu", "gnu2");
    #[cfg(target_arch = "aarch64")]
    const DIALECTS: (&str, &str) = ("trad", "desc");

    #[test]
    fn dlopened_module_in_dynamic_tls_is_read_through_the_dtv() {
        init_recovery();
        let before = Worker::spawn();
        // `__tls_get_addr` accesses keep the module in dynamic TLS.
        let library = Library::load("dynamic", DIALECTS.0);
        let location = locate_slot(&library);
        assert!(matches!(location, Location::Dtv { .. }), "{location:?}");

        let module = library.module;
        // Threads that never accessed the variable have no block; reading must not allocate one.
        assert_eq!(before.run(move || location.read()), None);
        assert_eq!(before.run(move || dtv_block(module.id)), None);
        assert_eq!(thread::spawn(move || location.read()).join().unwrap(), None);
        for (worker, value) in [(&before, 239), (&Worker::spawn(), 2390)] {
            let read = worker.run(move || {
                library.set(value);
                location.read()
            });
            assert_eq!(read, Some(value));
        }
    }

    #[test]
    fn dlopened_module_in_the_static_surplus_is_read_at_a_fixed_offset() {
        init_recovery();
        let before = Worker::spawn();
        // glibc places a small TLS-descriptor module in the static surplus.
        let library = Library::load("surplus", DIALECTS.1);
        let location = locate_slot(&library);
        assert!(matches!(location, Location::Fixed { .. }), "{location:?}");

        let module = library.module;
        assert_eq!(before.run(move || location.read()), Some(0));
        // The thread that predates the dlopen uses the variable, yet its DTV has no entry.
        let read = before.run(move || {
            library.set(239);
            location.read()
        });
        assert_eq!(read, Some(239));
        assert_eq!(before.run(move || dtv_block(module.id)), None);
        let read = thread::spawn(move || {
            library.set(2390);
            location.read()
        });
        assert_eq!(read.join().unwrap(), Some(2390));
    }
}
