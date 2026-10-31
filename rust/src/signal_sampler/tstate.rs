//! Reads the thread state of the calling thread from a signal handler on CPython 3.15+.
//!
//! Up to 3.14 `PyGILState_GetThisThreadState` is `pthread_getspecific`, which only reads memory.
//! From 3.15 it reads the `_Py_tss_gilstate` thread local instead. In a shared libpython that is a
//! general-dynamic TLS access: `__tls_get_addr` or a TLS descriptor, which may allocate the
//! thread's TLS block or grow its DTV with `malloc` when libpython was `dlopen`ed or another module
//! with TLS was loaded since the thread last ran. Neither is async-signal-safe.
//!
//! `_Py_tss_gilstate` is not exported, so [`locate`] finds it by value in libpython's TLS block and
//! keeps its offset from the thread pointer. [`gilstate`] then reads one word at that offset. This
//! works only when libpython's TLS lives in the static TLS area, where the offset is the same for
//! every thread, which holds when python or libpython is loaded at startup. [`locate`] checks that.

use super::Error;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

const WORD: usize = size_of::<usize>();

/// `PyGILState_GetThisThreadState`, returning the thread state of the calling thread or 0.
pub type ThreadStateGetter = extern "C" fn() -> usize;

static OFFSET: AtomicUsize = AtomicUsize::new(0);

/// `_Py_tss_gilstate` of the calling thread, or 0. Async-signal-safe once [`locate`] succeeded.
pub extern "C" fn gilstate() -> usize {
    match OFFSET.load(Ordering::Relaxed) {
        0 => 0,
        offset => read(offset),
    }
}

/// Finds `_Py_tss_gilstate` in the TLS block of the module that defines `getter`, which must be
/// `PyGILState_GetThisThreadState`.
///
/// The calling thread must have a thread state and must not hold the GIL: then `_Py_tss_tstate`,
/// which otherwise holds the same pointer, is null.
pub fn locate(getter: ThreadStateGetter) -> Result<(), Error> {
    let tstate = getter();
    if tstate == 0 {
        return Err("the calling thread has no thread state".to_string());
    }
    let offset = find(getter as usize, tstate)?;
    OFFSET.store(offset, Ordering::Relaxed);
    Ok(())
}

fn read(offset: usize) -> usize {
    let addr = thread_pointer().wrapping_add(offset);
    kindasafe::u64(addr as u64).map_or(0, |word| word as usize)
}

/// The offset from the thread pointer of the only word that holds `value` in the calling thread's
/// TLS block of the module containing `addr`.
fn find(addr: usize, value: usize) -> Result<usize, Error> {
    let (block, size) = tls_block(addr)?;
    let found: Vec<usize> = (0..size / WORD)
        .map(|i| block + i * WORD)
        // SAFETY: the TLS block of the calling thread is `size` bytes long.
        .filter(|&word| unsafe { *(word as *const usize) } == value)
        .collect();
    let [word] = found[..] else {
        return Err(format!(
            "expected one thread state in the TLS block of libpython, found {}",
            found.len()
        ));
    };
    let block_offset = block.wrapping_sub(thread_pointer());
    let other = std::thread::spawn(move || {
        tls_block(addr).map(|(block, _)| block.wrapping_sub(thread_pointer()))
    })
    .join()
    .map_err(|_| "the TLS check thread panicked".to_string())?;
    if other != Ok(block_offset) {
        return Err("the TLS block of libpython is not in the static TLS area".to_string());
    }
    let offset = word.wrapping_sub(thread_pointer());
    if read(offset) != value {
        return Err("the thread state does not read back".to_string());
    }
    Ok(offset)
}

/// The address and size of the calling thread's TLS block of the module containing `addr`.
fn tls_block(addr: usize) -> Result<(usize, usize), Error> {
    struct Search {
        target: usize,
        found: Option<(usize, usize)>,
    }

    unsafe extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _: libc::size_t,
        data: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: `dl_iterate_phdr` passes a valid `info`, `data` is the `Search` below.
        let (info, search) = unsafe { (&*info, &mut *(data as *mut Search)) };
        // SAFETY: `dlpi_phdr` points to `dlpi_phnum` headers.
        let phdrs = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
        let base = info.dlpi_addr as usize;
        let contains = phdrs.iter().any(|ph| {
            let start = base + ph.p_vaddr as usize;
            ph.p_type == libc::PT_LOAD
                && (start..start + ph.p_memsz as usize).contains(&search.target)
        });
        if !contains {
            return 0;
        }
        let tls = phdrs.iter().find(|ph| ph.p_type == libc::PT_TLS);
        if let Some(tls) = tls
            && !info.dlpi_tls_data.is_null()
        {
            search.found = Some((info.dlpi_tls_data as usize, tls.p_memsz as usize));
        }
        1
    }

    let mut search = Search {
        target: addr,
        found: None,
    };
    // SAFETY: `visit` matches the callback type and only uses `search` while it is alive.
    unsafe { libc::dl_iterate_phdr(Some(visit), &mut search as *mut Search as *mut c_void) };
    search
        .found
        .ok_or_else(|| "libpython has no TLS block in this thread".to_string())
}

#[cfg(target_arch = "x86_64")]
fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: on x86_64 Linux `fs:0` holds the address of the thread control block itself.
    unsafe {
        core::arch::asm!("mov {}, qword ptr fs:[0]", out(reg) tp, options(nostack, readonly, preserves_flags))
    };
    tp
}

#[cfg(target_arch = "aarch64")]
fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reads the thread pointer register.
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el0", out(reg) tp, options(nomem, nostack, preserves_flags))
    };
    tp
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static PROBE: Cell<usize> = const { Cell::new(0) };
    }

    #[test]
    fn finds_a_static_thread_local() {
        PROBE.set(0x2390_2390_2390);
        let offset = find(
            finds_a_static_thread_local as *const () as usize,
            0x2390_2390_2390,
        )
        .unwrap();
        assert_eq!(read(offset), 0x2390_2390_2390);
        let there = std::thread::spawn(move || {
            PROBE.set(0x1239);
            read(offset)
        });
        assert_eq!(there.join().unwrap(), 0x1239);
        assert_eq!(read(offset), 0x2390_2390_2390);
    }

    #[test]
    fn rejects_a_missing_value() {
        assert!(
            find(
                rejects_a_missing_value as *const () as usize,
                0x2390_0239_2390_0239
            )
            .is_err()
        );
    }

    const LIBPYTHON: &str = "PYROSCOPE_TEST_LIBPYTHON";

    /// Runs [`locate_in_dlopened_libpython`] in a child process with `tunables` and returns
    /// its output.
    fn run_dlopened(libpython: &str, tunables: &str) -> String {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "signal_sampler::tstate::tests::locate_in_dlopened_libpython",
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("PYROSCOPE_TEST_LIBPYTHON_CHILD", libpython)
            .env("GLIBC_TUNABLES", tunables)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "child failed: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }

    /// libpython `dlopen`ed by a host that does not link it, like an embedding plugin.
    ///
    /// Without spare static TLS its TLS is dynamic, which [`locate`] must refuse. By default
    /// glibc may put it in the spare static TLS area, where [`locate`] may refuse or succeed,
    /// and the child checks the reads when it succeeds.
    #[test]
    fn dlopened_libpython() {
        let Ok(libpython) = std::env::var(LIBPYTHON) else {
            eprintln!("{LIBPYTHON} is not set, skipping");
            return;
        };
        let spare_static_tls = run_dlopened(&libpython, "");
        assert!(spare_static_tls.contains("locate: "), "{spare_static_tls}");
        let dynamic_tls = run_dlopened(&libpython, "glibc.rtld.optional_static_tls=0");
        assert!(dynamic_tls.contains("locate: Err"), "{dynamic_tls}");
    }

    #[test]
    #[ignore = "run by dlopened_libpython in a child process"]
    fn locate_in_dlopened_libpython() {
        let libpython =
            std::ffi::CString::new(std::env::var("PYROSCOPE_TEST_LIBPYTHON_CHILD").unwrap())
                .unwrap();
        // SAFETY: plain libdl and CPython embedding calls with the signatures they have.
        unsafe {
            let lib = libc::dlopen(libpython.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
            assert!(!lib.is_null(), "dlopen failed");
            let sym = |name: &std::ffi::CStr| {
                let addr = libc::dlsym(lib, name.as_ptr());
                assert!(!addr.is_null(), "{name:?} not found");
                addr as usize
            };
            let initialize: extern "C" fn(libc::c_int) =
                std::mem::transmute(sym(c"Py_InitializeEx"));
            let save_thread: extern "C" fn() -> usize =
                std::mem::transmute(sym(c"PyEval_SaveThread"));
            let getter: ThreadStateGetter =
                std::mem::transmute(sym(c"PyGILState_GetThisThreadState"));
            initialize(0);
            let tstate = save_thread();
            let result = locate(getter);
            println!("locate: {result:?}");
            if result.is_ok() {
                assert_eq!(gilstate(), tstate);
                assert_eq!(std::thread::spawn(|| gilstate()).join().unwrap(), 0);
            }
        }
    }
}
