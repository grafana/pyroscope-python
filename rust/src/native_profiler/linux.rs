use crate::encode::r#gen::google::{Label, Location, Mapping, Profile, Sample, ValueType};
use framehop::{
    CacheNative, MustNotAllocateDuringUnwind, UnwindRegsNative, Unwinder, UnwinderNative,
};
use prost::Message;
use pyroscope_signal_timer::{CTimer, Timer, gettid};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod modules;
use modules::Modules;

type NativeUnwinder = UnwinderNative<Vec<u8>, MustNotAllocateDuringUnwind>;
const DEPTH: usize = 128;
const CAPACITY: usize = 4096;
const ROLES: [&str; 3] = [
    "ring-buffer-reader",
    "report-builder",
    "pprof-encoder-uploader",
];
const DUMP_INTERVAL: Duration = Duration::from_secs(10);

// Process-lifetime slots avoid TLS initialization and pointer reclamation in the handler.
// The gate protects both the pointer's lifetime and every access to State. A handler
// tries it once and drops the tick on contention; it never spins or waits.
struct Slot {
    tid: AtomicI32,
    busy: AtomicBool,
    state: AtomicPtr<State>,
    missed: AtomicU64,
}
impl Slot {
    const fn new() -> Self {
        Self {
            tid: AtomicI32::new(0),
            busy: AtomicBool::new(false),
            state: AtomicPtr::new(ptr::null_mut()),
            missed: AtomicU64::new(0),
        }
    }
    fn try_lock(&self) -> Option<Gate<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Gate(self))
    }
    // Only used outside the handler. In particular, registration/removal runs with
    // SIGPROF blocked, so an interrupted owner cannot wait for its own handler.
    fn lock(&self) -> Gate<'_> {
        loop {
            if let Some(gate) = self.try_lock() {
                return gate;
            }
            std::thread::yield_now();
        }
    }
}
struct Gate<'a>(&'a Slot);
impl Drop for Gate<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}
static SLOTS: [Slot; 3] = [const { Slot::new() }; 3];
static ACTIVE: Mutex<Option<Duration>> = Mutex::new(None);
static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
struct Stack {
    addresses: [u64; DEPTH],
    len: usize,
    count: u64,
    // 0 = complete, 1 = unwind error, 2 = depth limit.
    end: u8,
}
impl Default for Stack {
    fn default() -> Self {
        Self {
            addresses: [0; DEPTH],
            len: 0,
            count: 0,
            end: 0,
        }
    }
}
struct Table {
    stacks: Box<[Stack]>,
    dropped: u64,
}
impl Table {
    fn new() -> Self {
        Self {
            stacks: vec![Stack::default(); CAPACITY].into_boxed_slice(),
            dropped: 0,
        }
    }
    // Incremental profile aggregation: fixed capacity, bounded probing, no allocator,
    // hashing seed initialization, locks, symbol lookup, or protobuf encoding.
    fn record(&mut self, sample: Stack) {
        let mut hash = 0xcbf29ce484222325u64;
        for &address in &sample.addresses[..sample.len] {
            hash = (hash ^ address).wrapping_mul(0x100000001b3);
        }
        hash ^= sample.end as u64;
        for probe in 0..64 {
            let slot = &mut self.stacks[(hash as usize).wrapping_add(probe) & (CAPACITY - 1)];
            if slot.count == 0 {
                *slot = sample;
                return;
            }
            if slot.len == sample.len
                && slot.end == sample.end
                && slot.addresses[..slot.len] == sample.addresses[..sample.len]
            {
                slot.count = slot.count.saturating_add(sample.count);
                return;
            }
        }
        self.dropped = self.dropped.saturating_add(sample.count);
    }
}
struct State {
    modules: Arc<Modules>,
    cache: CacheNative<MustNotAllocateDuringUnwind>,
    table: Table,
    timer: Option<CTimer>,
    stack_range: std::ops::Range<u64>,
}

pub struct Session {
    stop: Sender<()>,
    writer: Option<JoinHandle<()>>,
}
impl Session {
    pub fn start(rate: u32) -> Result<Self, String> {
        let directory = std::env::var_os("PYROSCOPE_NATIVE_PROFILE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("native-profiles"));
        Self::start_in(rate, directory)
    }
    fn start_in(rate: u32, directory: PathBuf) -> Result<Self, String> {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if active.is_some() {
            return Err("native profiling already active".into());
        }
        fs::create_dir_all(&directory).map_err(|e| format!("native profile directory: {e}"))?;
        let modules = Arc::new(Modules::load().map_err(|e| format!("native unwind modules: {e}"))?);
        let period = Duration::from_secs_f64(1.0 / rate as f64);
        let (stop, receiver) = mpsc::channel();
        for slot in &SLOTS {
            let state = Box::new(State {
                modules: modules.clone(),
                cache: CacheNative::new_in(),
                table: Table::new(),
                timer: None,
                stack_range: 0..0,
            });
            let _gate = slot.lock();
            slot.missed.store(0, Ordering::Relaxed);
            slot.state.store(Box::into_raw(state), Ordering::Relaxed);
        }
        let writer = std::thread::Builder::new()
            .name("pyroscope-native-dump".into())
            .spawn(move || {
                let mut start = SystemTime::now();
                loop {
                    let stopping = receiver.recv_timeout(DUMP_INTERVAL)
                        != Err(mpsc::RecvTimeoutError::Timeout);
                    let end = SystemTime::now();
                    if let Err(e) = dump(&directory, &modules, period, start, end) {
                        log::warn!("native profile dump failed: {e}");
                    }
                    start = end;
                    if stopping {
                        break;
                    }
                }
            });
        match writer {
            Ok(writer) => {
                *active = Some(period);
                Ok(Self {
                    stop,
                    writer: Some(writer),
                })
            }
            Err(e) => {
                clear_slots();
                Err(format!("native profile writer: {e}"))
            }
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Serialize against registration and stop every timer before retiring the
        // signal handler. Pending handlers recheck tid after acquiring the gate.
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        *active = None;
        for slot in &SLOTS {
            let _gate = slot.lock();
            slot.tid.store(0, Ordering::Relaxed);
            let state = slot.state.load(Ordering::Relaxed);
            if !state.is_null() {
                unsafe {
                    (*state).timer.take();
                }
            }
        }
        let _ = self.stop.send(());
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        clear_slots();
    }
}
fn clear_slots() {
    for slot in &SLOTS {
        let _gate = slot.lock();
        let state = slot.state.swap(ptr::null_mut(), Ordering::Relaxed);
        if !state.is_null() {
            unsafe {
                drop(Box::from_raw(state));
            }
        }
    }
}

// Only constructed/dropped on the sampled thread, and dropped before its session.
// Keep it !Send so removing the timer cannot accidentally unregister another thread.
pub struct ThreadGuard {
    index: usize,
    _local: std::marker::PhantomData<*mut ()>,
}
pub fn register(index: usize) -> Option<ThreadGuard> {
    let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    let period = (*active)?;
    let slot = SLOTS.get(index)?;
    let _blocked = BlockSignal::new();
    let _gate = slot.lock();
    let state = slot.state.load(Ordering::Relaxed);
    if state.is_null() || slot.tid.load(Ordering::Relaxed) != 0 {
        return None;
    }
    let tid = gettid();
    let stack_range = match stack_range() {
        Ok(range) => range,
        Err(e) => {
            log::warn!("native stack bounds: {e}");
            return None;
        }
    };
    unsafe {
        (*state).stack_range = stack_range;
    }
    match CTimer::start(tid, Timer::SIGNAL, period) {
        Ok(timer) => {
            unsafe {
                (*state).timer = Some(timer);
            }
            slot.tid.store(tid, Ordering::Release);
            Some(ThreadGuard {
                index,
                _local: std::marker::PhantomData,
            })
        }
        Err(e) => {
            log::warn!("native profile timer for {}: {e}", ROLES[index]);
            None
        }
    }
}
impl Drop for ThreadGuard {
    fn drop(&mut self) {
        let _blocked = BlockSignal::new();
        let slot = &SLOTS[self.index];
        let _gate = slot.lock();
        if slot.tid.load(Ordering::Relaxed) == gettid() {
            slot.tid.store(0, Ordering::Relaxed);
            let state = slot.state.load(Ordering::Relaxed);
            if !state.is_null() {
                unsafe {
                    (*state).timer.take();
                }
            }
        }
    }
}
fn stack_range() -> io::Result<std::ops::Range<u64>> {
    unsafe {
        let mut attr = std::mem::zeroed();
        let error = libc::pthread_getattr_np(libc::pthread_self(), &mut attr);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let mut base = ptr::null_mut();
        let mut size = 0;
        let error = libc::pthread_attr_getstack(&attr, &mut base, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        Ok(base as u64..base as u64 + size as u64)
    }
}
struct BlockSignal(libc::sigset_t);
impl BlockSignal {
    fn new() -> Self {
        unsafe {
            let mut set = std::mem::zeroed();
            let mut old = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, Timer::SIGNAL);
            assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old), 0);
            Self(old)
        }
    }
}
impl Drop for BlockSignal {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, ptr::null_mut());
        }
    }
}

/// Called only from SIGPROF with its kernel-provided context. SIGPROF is masked
/// by sigaction during the callback. SIGSEGV/SIGBUS must remain unblocked, as for
/// the Python unwinder. No allocation, waiting, logging, or loader calls here.
pub unsafe fn sample(info: *mut libc::siginfo_t, context: *mut libc::c_void) -> bool {
    let tid = gettid();
    let Some(slot) = SLOTS
        .iter()
        .find(|slot| slot.tid.load(Ordering::Acquire) == tid)
    else {
        return false;
    };
    // ITIMER_PROF can also interrupt these workers. Only count their own CPU
    // timer ticks, otherwise native CPU time would be counted twice.
    if info.is_null() || unsafe { (*info).si_code } != libc::SI_TIMER {
        return true;
    }
    let Some(_gate) = slot.try_lock() else {
        slot.missed.fetch_add(1, Ordering::Relaxed);
        return true;
    };
    if slot.tid.load(Ordering::Relaxed) != tid || context.is_null() {
        return true;
    }
    let state = slot.state.load(Ordering::Relaxed);
    if state.is_null() {
        return true;
    }
    let state = unsafe { &mut *state };
    let (pc, regs) = unsafe { registers(&*(context as *const libc::ucontext_t)) };
    // Reject CFI reads outside the interrupted thread's stack. In particular,
    // untrusted unwind values cannot alias our exclusively borrowed heap state.
    let range = &state.stack_range;
    let mut read = |address: u64| {
        if address < range.start || address.checked_add(8).is_none_or(|end| end > range.end) {
            return Err(());
        }
        unsafe { kindasafe::u64(address).map_err(|_| ()) }
    };
    let mut frames = state
        .modules
        .unwinder
        .iter_frames(pc, regs, &mut state.cache, &mut read);
    let mut stack = Stack {
        count: 1,
        ..Stack::default()
    };
    while stack.len < DEPTH {
        match frames.next() {
            Ok(Some(frame)) => {
                stack.addresses[stack.len] = frame.address_for_lookup();
                stack.len += 1;
            }
            Ok(None) => break,
            Err(_) => {
                stack.end = 1;
                break;
            }
        }
    }
    if stack.len == DEPTH {
        stack.end = 2;
    }
    state.table.record(stack);
    true
}
#[cfg(target_arch = "x86_64")]
unsafe fn registers(context: &libc::ucontext_t) -> (u64, UnwindRegsNative) {
    let g = &context.uc_mcontext.gregs;
    let pc = g[libc::REG_RIP as usize] as u64;
    (
        pc,
        UnwindRegsNative::new(
            pc,
            g[libc::REG_RSP as usize] as u64,
            g[libc::REG_RBP as usize] as u64,
        ),
    )
}
#[cfg(target_arch = "aarch64")]
unsafe fn registers(context: &libc::ucontext_t) -> (u64, UnwindRegsNative) {
    let g = &context.uc_mcontext;
    (g.pc, UnwindRegsNative::new(g.regs[30], g.sp, g.regs[29]))
}

fn dump(
    directory: &std::path::Path,
    modules: &Modules,
    period: Duration,
    start: SystemTime,
    end: SystemTime,
) -> io::Result<()> {
    let mut tables = Vec::new();
    for (index, slot) in SLOTS.iter().enumerate() {
        // Allocate BEFORE taking the gate; the protected operation only swaps
        // pointers. The writer never borrows the interrupted thread's stack.
        let mut table = Table::new();
        let missed;
        {
            let _gate = slot.lock();
            let state = slot.state.load(Ordering::Relaxed);
            if state.is_null() {
                continue;
            }
            unsafe {
                std::mem::swap(&mut (*state).table, &mut table);
            }
            missed = slot.missed.swap(0, Ordering::Relaxed);
        }
        tables.push((index, table, missed));
    }
    let profile = profile(modules, period, start, end, &tables);
    if profile.sample.is_empty() {
        return Ok(());
    }
    // create_new also avoids overwriting profiles after a restart or when several
    // processes share a directory. Numbering is independent of uploaded profiles.
    let mut file = loop {
        let number = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("{number}.pprof")))
        {
            Ok(file) => break file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    file.write_all(&profile.encode_to_vec())
}

fn profile(
    modules: &Modules,
    period: Duration,
    start: SystemTime,
    end: SystemTime,
    tables: &[(usize, Table, u64)],
) -> Profile {
    let mut p = Profile {
        string_table: vec![
            "".into(),
            "cpu".into(),
            "nanoseconds".into(),
            "thread".into(),
            "unwind".into(),
        ],
        sample_type: vec![ValueType { r#type: 1, unit: 2 }],
        period_type: Some(ValueType { r#type: 1, unit: 2 }),
        period: period.as_nanos() as i64,
        time_nanos: start
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64,
        duration_nanos: end.duration_since(start).unwrap_or_default().as_nanos() as i64,
        ..Profile::default()
    };
    for module in &modules.images {
        let filename = string(&mut p, module.path.clone());
        p.mapping.push(Mapping {
            id: p.mapping.len() as u64 + 1,
            memory_start: module.start,
            memory_limit: module.end,
            file_offset: module.offset,
            filename,
            ..Mapping::default()
        });
    }
    let mut locations = HashMap::new();
    for (role, table, missed) in tables {
        let thread = string(&mut p, ROLES[*role].into());
        for stack in table.stacks.iter().filter(|s| s.count != 0) {
            let mut ids = Vec::with_capacity(stack.len);
            for &address in &stack.addresses[..stack.len] {
                ids.push(
                    *locations
                        .entry(address)
                        .or_insert_with(|| modules.location(&mut p, address)),
                );
            }
            let status = string(
                &mut p,
                match stack.end {
                    1 => "error",
                    2 => "depth-limit",
                    _ => "complete",
                }
                .into(),
            );
            p.sample.push(Sample {
                location_id: ids,
                value: vec![weight(stack.count, period)],
                label: vec![
                    Label {
                        key: 3,
                        str: thread,
                        ..Label::default()
                    },
                    Label {
                        key: 4,
                        str: status,
                        ..Label::default()
                    },
                ],
            });
        }
        for (name, count) in [("table-full", table.dropped), ("collector-busy", *missed)] {
            if count != 0 {
                let id = modules.location(&mut p, 0);
                let reason_key = string(&mut p, "dropped_reason".into());
                let reason = string(&mut p, name.into());
                p.sample.push(Sample {
                    location_id: vec![id],
                    value: vec![weight(count, period)],
                    label: vec![
                        Label {
                            key: 3,
                            str: thread,
                            ..Label::default()
                        },
                        Label {
                            key: reason_key,
                            str: reason,
                            ..Label::default()
                        },
                    ],
                });
            }
        }
    }
    p
}
fn weight(count: u64, period: Duration) -> i64 {
    (count as u128 * period.as_nanos()).min(i64::MAX as u128) as i64
}
fn string(p: &mut Profile, value: String) -> i64 {
    let id = p.string_table.len() as i64;
    p.string_table.push(value);
    id
}

#[cfg(test)]
mod tests;
