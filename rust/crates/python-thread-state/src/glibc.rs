//! 3.15 keeps the thread state in a thread-local variable that is not exported. [`init`]
//! locates it so that `pyroscope_python_unwind::thread_state::glibc` reads it with plain
//! memory loads, without entering the dynamic loader. Nothing here is async-signal-safe.
//!
//! The TCB and DTV layouts are private to glibc. [`locate`] accepts a location only after
//! checking it on two threads against the loader's own answer. The second thread runs on a
//! stack mapped here: glibc carves the TCB and static TLS of a thread out of a stack the caller
//! provides, so a variable in static TLS must lie inside that stack.
//!
//! The module is compiled for every CPython version, so that the search is tested on all of
//! them, but [`init`] is used only on 3.15.

use crate::ThreadStateError;
use core::ffi::{c_int, c_void};
use core::mem::{MaybeUninit, size_of};
use core::ops::Range;
use core::ptr;
use pyo3::Python;
use pyroscope_python_unwind::Error;
use pyroscope_python_unwind::thread_state::glibc::{
    self as reader, Location, dtv_block, thread_pointer,
};

/// Locates CPython's thread-local thread state and stores it in
/// `pyroscope_python_unwind::thread_state::glibc`, so that the handler reads it without
/// calling into CPython or the dynamic loader. Until this succeeds the handler finds no thread
/// state. The location is cached for the process.
///
/// Briefly detaches from the interpreter, and spawns and joins a short-lived thread while
/// detached.
///
/// # Safety
/// kindasafe recovery must be installed.
pub unsafe fn init(py: Python<'_>) -> Result<(), ThreadStateError> {
    if reader::location().is_some() {
        return Ok(());
    }
    let module = module_of(pyo3_ffi::PyGILState_GetThisThreadState as *const () as usize)
        .ok_or(ThreadStateError::NoTlsSegment)?;
    // SAFETY: `py` proves the thread is attached, so it has a thread state.
    let tstate = unsafe { pyo3_ffi::PyGILState_GetThisThreadState() } as usize;
    // Detached during the search because the attached thread state is also cached in a second
    // thread-local variable, and so that joining the probe thread in `locate` never waits
    // while holding the GIL.
    let location = py.detach(|| {
        find_value(module, tstate)
            .ok_or(ThreadStateError::VariableNotFound)
            .and_then(|offset| locate(module, offset).ok_or(ThreadStateError::UnknownTlsLayout))
    })?;
    // SAFETY: `locate` verified the location on two threads. A concurrent call may have
    // stored its own verified location first; either one holds.
    unsafe { reader::set_location(location) };
    Ok(())
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
fn read(address: usize) -> Result<usize, Error> {
    // SAFETY: a pointer-sized load under kindasafe's recovery preconditions, documented at
    // crate level. No Rust reference to the source memory is created.
    unsafe { kindasafe::u64(address as u64) }
        .map(|word| word as usize)
        .map_err(|_| Error::Read)
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
        .filter(|offset| read(block + offset) == Ok(value));
    let offset = offsets.next()?;
    offsets.next().is_none().then_some(offset)
}

#[derive(Clone, Copy)]
struct Probe {
    /// The block the loader returned.
    block: usize,
    /// Block address relative to the thread pointer.
    delta: isize,
    /// Whether the DTV records the block the loader returned.
    in_dtv: bool,
}

impl Probe {
    fn current(module: usize) -> Probe {
        let block = block(module);
        Probe {
            block,
            delta: block.wrapping_sub(thread_pointer()) as isize,
            in_dtv: dtv_block(module) == Ok(block),
        }
    }

    /// Probes on a new thread running on a [`Stack`]. Returns the probe and the addresses of
    /// the stack.
    fn on_new_thread(module: usize) -> Option<(Probe, Range<usize>)> {
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
        let stack = Stack::map()?;
        let mut call = Call {
            module,
            probe: None,
        };
        let mut attr = Attr::new()?;
        // SAFETY: `stack` is a readable and writable mapping that outlives the joined thread.
        if unsafe {
            libc::pthread_attr_setstack(&mut attr.0, stack.base as *mut c_void, stack.size)
        } != 0
        {
            return None;
        }
        // SAFETY: `start` only uses `call`, which outlives the joined thread.
        unsafe {
            let mut thread: libc::pthread_t = 0;
            if libc::pthread_create(&mut thread, &attr.0, start, (&raw mut call).cast()) != 0 {
                return None;
            }
            // The thread may still use the stack if the join fails: never unmap it then.
            if libc::pthread_join(thread, ptr::null_mut()) != 0 {
                core::mem::forget(stack);
                return None;
            }
        }
        Some((call.probe?, stack.base..stack.base + stack.size))
    }
}

/// Initialized `pthread_attr_t`, destroyed on drop.
struct Attr(libc::pthread_attr_t);

impl Attr {
    fn new() -> Option<Attr> {
        let mut attr = MaybeUninit::uninit();
        // SAFETY: initializes `attr`.
        if unsafe { libc::pthread_attr_init(attr.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: pthread_attr_init succeeded.
        Some(Attr(unsafe { attr.assume_init() }))
    }
}

impl Drop for Attr {
    fn drop(&mut self) {
        // SAFETY: `self.0` was initialized by pthread_attr_init.
        unsafe { libc::pthread_attr_destroy(&mut self.0) };
    }
}

/// A thread stack of the default size with a `PROT_NONE` guard page on each side, because
/// glibc adds no guard to a stack the caller provides. Unmapped on drop.
struct Stack {
    /// Lowest address of the usable stack, one page above the mapping.
    base: usize,
    size: usize,
    page: usize,
}

impl Stack {
    fn map() -> Option<Stack> {
        // SAFETY: sysconf has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        // The default size leaves the room glibc needs for the TCB and static TLS.
        let mut size = 0;
        // SAFETY: `attr` is initialized and `size` is writable.
        if unsafe { libc::pthread_attr_getstacksize(&Attr::new()?.0, &mut size) } != 0 {
            return None;
        }
        let size = size
            .max(libc::PTHREAD_STACK_MIN)
            .checked_next_multiple_of(page)?;
        let len = size.checked_add(2 * page)?;
        // SAFETY: a new anonymous mapping, inaccessible until the stack pages are opened.
        let mapping = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return None;
        }
        let stack = Stack {
            base: mapping as usize + page,
            size,
            page,
        };
        // SAFETY: opens the pages between the two guard pages of the mapping above.
        let opened = unsafe {
            libc::mprotect(
                stack.base as *mut c_void,
                size,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        (opened == 0).then_some(stack)
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // SAFETY: unmaps the whole mapping, guard pages included; no thread runs on it.
        unsafe {
            libc::munmap(
                (self.base - self.page) as *mut c_void,
                self.size + 2 * self.page,
            )
        };
    }
}

/// Finds where the variable at `offset` in `module`'s block lives in every thread. Spawns a
/// short-lived thread on its own stack to compare with the calling one. A fixed offset is
/// accepted only if the variable is at the same offset from the thread pointer on both
/// threads and inside the new thread's stack, which holds its static TLS. `None` if neither
/// location holds on both threads. Not async-signal-safe.
pub fn locate(module: Module, offset: usize) -> Option<Location> {
    let here = Probe::current(module.id);
    let (there, stack) = Probe::on_new_thread(module.id)?;
    choose(module.id, offset, here, there, stack)
}

/// The location that holds for the variable at `offset` on both probes. `there` ran on
/// `stack`.
fn choose(
    module: usize,
    offset: usize,
    here: Probe,
    there: Probe,
    stack: Range<usize>,
) -> Option<Location> {
    let variable = there.block.wrapping_add(offset);
    let in_stack = variable >= stack.start
        && variable
            .checked_add(size_of::<usize>())
            .is_some_and(|end| end <= stack.end);
    if here.delta == there.delta && in_stack {
        Some(Location::Fixed {
            offset: here.delta.wrapping_add_unsigned(offset),
        })
    } else if here.in_dtv && there.in_dtv {
        Some(Location::Dtv { module, offset })
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
        assert_eq!(location.read(), Ok(marker));
        for value in [0, 239] {
            let read = thread::spawn(move || {
                if value != 0 {
                    SLOT.set(value);
                }
                location.read()
            });
            assert_eq!(read.join().unwrap(), Ok(value));
        }
    }

    #[test]
    fn probe_stack_is_between_guard_pages() {
        init_recovery();
        let stack = Stack::map().unwrap();
        let (base, end) = (stack.base, stack.base + stack.size);
        assert_eq!(base % stack.page, 0);
        assert!(stack.size >= libc::PTHREAD_STACK_MIN);
        // SAFETY: kindasafe reads; the guard pages fault and are recovered.
        let readable = |address: usize| unsafe { kindasafe::u8(address as u64) }.is_ok();
        assert_eq!(
            [base - 1, base, end - 1, end].map(readable),
            [false, true, true, false]
        );
    }

    #[test]
    fn the_new_thread_runs_on_its_own_stack() {
        init_recovery();
        let module = module_of(init_recovery as fn() as usize).unwrap();
        let (probe, stack) = Probe::on_new_thread(module.id).unwrap();
        // glibc carves the static TLS of the test binary out of the provided stack.
        assert!(
            stack.contains(&probe.block),
            "{:#x} {stack:x?}",
            probe.block
        );
        assert!(!stack.contains(&block(module.id)));
    }

    #[test]
    fn a_fixed_offset_needs_the_variable_inside_the_new_thread_stack() {
        let stack = 0x10_0000..0x20_0000;
        let probe = |block: usize, in_dtv| Probe {
            block,
            delta: -0x100,
            in_dtv,
        };
        let (module, offset) = (7, 0x18);
        // Static TLS: same offset from the thread pointer, inside the stack.
        let fixed = Some(Location::Fixed { offset: -0xe8 });
        let inside = probe(0x1f_ff00, false);
        assert_eq!(choose(module, offset, inside, inside, stack.clone()), fixed);
        let last = probe(stack.end - offset - size_of::<usize>(), false);
        assert_eq!(choose(module, offset, last, last, stack.clone()), fixed);
        // The same offset by coincidence, outside the stack: only the DTV may hold.
        for block in [stack.start - offset - 1, stack.end - offset - 1, 0x40_0000] {
            let outside = probe(block, false);
            assert_eq!(
                choose(module, offset, outside, outside, stack.clone()),
                None
            );
            let outside = probe(block, true);
            assert_eq!(
                choose(module, offset, outside, outside, stack.clone()),
                Some(Location::Dtv { module, offset })
            );
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

    fn locate_slot(library: &Library) -> Location {
        let marker = 0x2392_0000_0000_0239;
        library.set(marker);
        let offset = find_value(library.module, marker).unwrap();
        assert_eq!(block(library.module.id) + offset, library.address());
        let location = locate(library.module, offset).unwrap();
        assert_eq!(location.read(), Ok(marker));
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
        assert_eq!(
            before.run(move || location.read()),
            Err(Error::NoThreadState)
        );
        assert_eq!(
            before.run(move || dtv_block(module.id)),
            Err(Error::NoThreadState)
        );
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
    fn dlopened_module_in_the_static_surplus_is_read_at_a_fixed_offset() {
        init_recovery();
        let before = Worker::spawn();
        // glibc places a small TLS-descriptor module in the static surplus.
        let library = Library::load("surplus", DIALECTS.1);
        let location = locate_slot(&library);
        assert!(matches!(location, Location::Fixed { .. }), "{location:?}");

        let module = library.module;
        assert_eq!(before.run(move || location.read()), Ok(0));
        // The thread that predates the dlopen uses the variable, yet its DTV has no entry.
        let read = before.run(move || {
            library.set(239);
            location.read()
        });
        assert_eq!(read, Ok(239));
        assert_eq!(
            before.run(move || dtv_block(module.id)),
            Err(Error::NoThreadState)
        );
        let read = thread::spawn(move || {
            library.set(2390);
            location.read()
        });
        assert_eq!(read.join().unwrap(), Ok(2390));
    }
}
