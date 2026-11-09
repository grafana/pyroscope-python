use super::*;

#[test]
fn incremental_table_preserves_complete_stacks_weights_and_labels() {
    let modules = Modules {
        unwinder: NativeUnwinder::new(),
        images: Vec::new(),
    };
    let mut table = Table::new();
    let mut stack = Stack {
        len: 3,
        count: 1,
        ..Stack::default()
    };
    stack.addresses[..3].copy_from_slice(&[30, 20, 10]);
    table.record(stack);
    stack.count = 4;
    table.record(stack);
    stack.addresses[0] = 31;
    stack.count = 2;
    stack.end = 1;
    table.record(stack);
    table.dropped = 3;
    let bytes = profile(
        &modules,
        Duration::from_millis(10),
        UNIX_EPOCH,
        UNIX_EPOCH + Duration::from_secs(1),
        &[(0, table, 2)],
    )
    .encode_to_vec();
    let p = Profile::decode(bytes.as_slice()).unwrap();
    let mut actual = Vec::new();
    for sample in &p.sample {
        assert_eq!(
            p.string_table[sample.label[0].str as usize], ROLES[0],
            "{p:#?}"
        );
        let stack: Vec<_> = sample
            .location_id
            .iter()
            .rev()
            .map(|&id| {
                let l = p.location.iter().find(|l| l.id == id).unwrap();
                (
                    l.address,
                    l.line
                        .first()
                        .map(|l| {
                            let f = p.function.iter().find(|f| f.id == l.function_id).unwrap();
                            p.string_table[f.name as usize].as_str()
                        })
                        .unwrap_or(""),
                )
            })
            .collect();
        let status = sample
            .label
            .get(1)
            .map(|l| p.string_table[l.str as usize].as_str())
            .unwrap_or("");
        actual.push((stack, sample.value.clone(), status));
    }
    actual.sort();
    let mut expected = vec![
        (
            vec![(10, ""), (20, ""), (30, "")],
            vec![50_000_000],
            "complete",
        ),
        (
            vec![(10, ""), (20, ""), (31, "")],
            vec![20_000_000],
            "error",
        ),
        (vec![(0, "")], vec![30_000_000], "table-full"),
        (vec![(0, "")], vec![20_000_000], "collector-busy"),
    ];
    expected.sort();
    assert_eq!(actual, expected, "{p:#?}");
    assert_eq!(
        p.sample.iter().map(|s| s.value[0]).sum::<i64>(),
        120_000_000,
        "{p:#?}"
    );
    assert_eq!(p.period, 10_000_000);
    assert_eq!(p.duration_nanos, 1_000_000_000);
}

#[test]
fn bounded_table_accounts_for_every_tick_when_saturated() {
    let mut table = Table::new();
    for address in 1..=20_000 {
        let mut stack = Stack {
            len: 1,
            count: 1,
            ..Stack::default()
        };
        stack.addresses[0] = address;
        table.record(stack);
    }
    assert!(table.dropped > 0);
    assert_eq!(
        table.stacks.iter().map(|s| s.count).sum::<u64>() + table.dropped,
        20_000
    );
}

// Instrument the actual handler's allocations while the assembly fixture runs.
// The flag is initialized on the worker before it is interrupted, and const TLS
// has no destructor. All other test threads pass straight through to System.
struct CheckedAllocator;
thread_local! { static CHECK_ALLOC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
#[global_allocator]
static ALLOCATOR: CheckedAllocator = CheckedAllocator;
unsafe impl std::alloc::GlobalAlloc for CheckedAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if CHECK_ALLOC.try_with(|c| c.get()).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        if CHECK_ALLOC.try_with(|c| c.get()).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe {
            std::alloc::System.dealloc(pointer, layout);
        }
    }
}

// Declare an explicit unwind root so the complete expected stack is independent
// of libc's private thread-start functions and the Rust test harness version.
#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    r#"
    .text
    .global native_profile_test_root
    .type native_profile_test_root,@function
native_profile_test_root:
    .cfi_startproc
    .cfi_undefined rip
    sub rsp, 8
    .cfi_adjust_cfa_offset 8
    call native_profile_test_leaf
    add rsp, 8
    ret
    .cfi_endproc
    .global native_profile_test_root_end
native_profile_test_root_end:
    .size native_profile_test_root, .-native_profile_test_root
    .global native_profile_test_leaf
    .type native_profile_test_leaf,@function
native_profile_test_leaf:
    .cfi_startproc
2:
    cmp byte ptr [rdi], 0
    je 2b
    ret
    .cfi_endproc
    .global native_profile_test_leaf_end
native_profile_test_leaf_end:
    .size native_profile_test_leaf, .-native_profile_test_leaf
"#
);

#[cfg(target_arch = "x86_64")]
#[test]
fn cpu_timers_unwind_all_workers_without_allocating_and_flush_on_restart() {
    use pyroscope_signal_timer::{Mode, SignalHandler};
    extern "C" fn handler(_: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
        unsafe {
            sample(info, context);
        }
    }
    unsafe extern "C" {
        fn native_profile_test_root(stop: *const AtomicBool);
        fn native_profile_test_leaf(stop: *const AtomicBool);
        static native_profile_test_root_end: u8;
        static native_profile_test_leaf_end: u8;
    }
    unsafe {
        kindasafe_init::init().unwrap();
    }
    let directory =
        std::env::temp_dir().join(format!("pyroscope-native-test-{}", uuid::Uuid::new_v4()));
    let _handler = SignalHandler::install(Timer::SIGNAL, handler).unwrap();
    for mode in [Mode::CTimer, Mode::ITimer] {
        let session = Session::start_in(1000, directory.clone()).unwrap();
        let timer = Timer::start(mode, Duration::from_millis(1)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..3)
            .map(|role| {
                let stop = stop.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    let _guard = register(role).expect("worker timer");
                    ready.wait();
                    CHECK_ALLOC.with(|c| c.set(true));
                    unsafe {
                        native_profile_test_root(Arc::as_ptr(&stop));
                    }
                    CHECK_ALLOC.with(|c| c.set(false));
                })
            })
            .collect();
        ready.wait();
        std::thread::sleep(Duration::from_millis(600));
        stop.store(true, Ordering::Relaxed);
        for worker in workers {
            worker.join().unwrap();
        }
        drop(timer);
        drop(session); // final partial window is written here
    }
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "handler allocated or freed memory"
    );
    let mut paths: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert_eq!(paths[0].file_name().unwrap(), "1.pprof");
    assert_eq!(paths[1].file_name().unwrap(), "2.pprof");
    for path in paths {
        let p = Profile::decode(fs::read(path).unwrap().as_slice()).unwrap();
        assert!(
            p.function.is_empty(),
            "raw profiles must have no symbols: {p:#?}"
        );
        assert!(p.location.iter().all(|l| l.line.is_empty()), "{p:#?}");
        let root_range = native_profile_test_root as *const () as u64
            ..(&raw const native_profile_test_root_end) as u64;
        let leaf_range = native_profile_test_leaf as *const () as u64
            ..(&raw const native_profile_test_leaf_end) as u64;
        let mut totals = [0i64; 3];
        let mut expected = [0i64; 3];
        for sample in &p.sample {
            let label = sample
                .label
                .iter()
                .find(|l| p.string_table[l.key as usize] == "thread")
                .unwrap();
            let role = ROLES
                .iter()
                .position(|r| *r == p.string_table[label.str as usize])
                .unwrap();
            totals[role] += sample.value[0];
            let frames: Vec<_> = sample
                .location_id
                .iter()
                .rev()
                .map(|id| {
                    let location = p.location.iter().find(|l| l.id == *id).unwrap();
                    // Resolve only our known fixture address ranges in the test,
                    // after reading the raw pprof. Compare the entire stack below.
                    if root_range.contains(&location.address) {
                        "native_profile_test_root".to_owned()
                    } else if leaf_range.contains(&location.address) {
                        "native_profile_test_leaf".to_owned()
                    } else {
                        format!("0x{:x}", location.address)
                    }
                })
                .collect();
            if frames == ["native_profile_test_root", "native_profile_test_leaf"] {
                expected[role] += sample.value[0];
            }
        }
        // Setup/teardown can receive ticks. At least 95% of each worker's total
        // weight must be the full two-frame fixture, with enough CPU samples to
        // exclude a single lucky stack. Other 5% allows scheduler/sampling noise.
        for role in 0..3 {
            assert!(
                totals[role] >= 20_000_000,
                "role {role}, totals {totals:?}, {p:#?}"
            );
            assert!(
                expected[role] * 100 >= totals[role] * 95,
                "role {role}, expected {expected:?}, totals {totals:?}, {p:#?}"
            );
        }
    }
    fs::remove_dir_all(directory).unwrap();
}
