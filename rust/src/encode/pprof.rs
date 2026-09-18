use crate::backend::StackTrace;
use crate::backend::types::Report;
use crate::encode::r#gen::google::{Function, Label, Line, Location, Profile, Sample, ValueType};
use crate::encode::pprof::ffi::FFIInternedString;
use crate::encode::pprof::ffi::{FFIFrame, FFISampleValues};
use crate::utils::TimeRange;
use hashbrown::hash_map::EntryRef;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum PprofBuilderType {
    Memory,
    /// py-spy.
    Cpu,
    /// The vendored dd-trace-py stack sampler.
    CpuWall,
}

/// What a `PProfBuilder` is building: the sample types it emits, and the value
/// layout of one accumulated row.
pub trait ProfileKind {
    /// One slot per `sample_type`, in the same order.
    type Values: Copy + AsRef<[i64]> + AsMut<[i64]>;
    /// What `period` is derived from: a sample rate in Hz, a byte interval, ...
    type PeriodConfig: Copy;

    /// Assigns rather than appends, so a dump path may re-set it every window
    /// after `take_profile_and_reset` has emptied the profile.
    fn set_profile_type(
        profile: &mut Profile,
        strings: &mut StringTable,
        period: Self::PeriodConfig,
    );
}

/// A kind whose samples cross the FFI boundary as `FFISampleValues`.
pub trait FfiProfileKind: ProfileKind {
    fn value_slots(values: &FFISampleValues) -> Self::Values;
}

pub struct MemoryProfile;
pub struct PySpyProfile;
/// The vendored dd-trace-py stack sampler.
pub struct CpuWallProfile;

pub struct PProfBuilder<K: ProfileKind> {
    profile: Profile,
    functions: HashMap<FunctionMirror, u64>,
    locations: HashMap<LocationMirror, u64>,
    ffi_samples: hashbrown::HashMap<Vec<u64>, K::Values>,
    ffi_locations_scratch: Vec<u64>,
}
#[derive(Hash, PartialEq, Eq, Clone)]
pub struct LocationMirror {
    pub function_id: u64,
    pub line: i64,
}

#[derive(Hash, PartialEq, Eq, Clone)]
pub struct FunctionMirror {
    pub name: StringID,
    pub filename: StringID,
}

impl ProfileKind for MemoryProfile {
    type Values = [i64; 4];
    type PeriodConfig = u64;

    fn set_profile_type(profile: &mut Profile, strings: &mut StringTable, heap_sample_rate: u64) {
        profile.sample_type = vec![
            ValueType {
                r#type: strings.add("alloc_objects").pprof(),
                unit: strings.add("count").pprof(),
            },
            ValueType {
                r#type: strings.add("alloc_space").pprof(),
                unit: strings.add("bytes").pprof(),
            },
            ValueType {
                r#type: strings.add("inuse_objects").pprof(),
                unit: strings.add("count").pprof(),
            },
            ValueType {
                r#type: strings.add("inuse_space").pprof(),
                unit: strings.add("bytes").pprof(),
            },
        ];
        profile.period = heap_sample_rate as i64;
        profile.period_type = Some(ValueType {
            r#type: strings.add("space").pprof(),
            unit: strings.add("bytes").pprof(),
        });
    }
}

impl FfiProfileKind for MemoryProfile {
    fn value_slots(values: &FFISampleValues) -> Self::Values {
        [
            values.alloc_count as i64,
            values.alloc_space as i64,
            values.heap_count as i64,
            values.heap_space as i64,
        ]
    }
}

impl ProfileKind for CpuWallProfile {
    type Values = [i64; 2];
    type PeriodConfig = u32;

    fn set_profile_type(profile: &mut Profile, strings: &mut StringTable, sample_rate: u32) {
        profile.sample_type = vec![
            ValueType {
                r#type: strings.add("cpu").pprof(),
                unit: strings.add("nanoseconds").pprof(),
            },
            ValueType {
                r#type: strings.add("wall").pprof(),
                unit: strings.add("nanoseconds").pprof(),
            },
        ];
        profile.period = 1_000_000_000 / sample_rate as i64;
        profile.period_type = Some(ValueType {
            r#type: strings.add("cpu").pprof(),
            unit: strings.add("nanoseconds").pprof(),
        });
    }
}

impl FfiProfileKind for CpuWallProfile {
    fn value_slots(values: &FFISampleValues) -> Self::Values {
        [values.cpu_time, values.wall_time]
    }
}

impl ProfileKind for PySpyProfile {
    type Values = [i64; 1];
    type PeriodConfig = u32;

    fn set_profile_type(profile: &mut Profile, strings: &mut StringTable, sample_rate: u32) {
        profile.sample_type = vec![ValueType {
            r#type: strings.add("cpu").pprof(),
            unit: strings.add("nanoseconds").pprof(),
        }];
        profile.period = 1_000_000_000 / sample_rate as i64;
        profile.period_type = Some(ValueType {
            r#type: strings.add("cpu").pprof(),
            unit: strings.add("nanoseconds").pprof(),
        });
    }
}

impl<K: ProfileKind> Default for PProfBuilder<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: ProfileKind> PProfBuilder<K> {
    pub fn new() -> Self {
        PProfBuilder {
            functions: HashMap::new(),
            locations: HashMap::new(),
            ffi_samples: hashbrown::HashMap::new(),
            ffi_locations_scratch: Vec::new(),
            profile: Profile {
                sample_type: vec![],
                sample: vec![],
                mapping: vec![],
                location: vec![],
                function: vec![],
                string_table: vec![],
                drop_frames: 0,
                keep_frames: 0,
                time_nanos: 0,
                duration_nanos: 0,
                period_type: None,
                period: 0,
                comment: vec![],
                default_sample_type: 0,
            },
        }
    }

    pub fn set_time_range(&mut self, time_range: &TimeRange) {
        //todo fix casts before April 2262
        let start_time_nanos = time_range.start_time_unix().as_nanos() as u64;
        let duration_nanos = time_range.duration().as_nanos() as u64;
        self.profile.time_nanos = start_time_nanos as i64;
        self.profile.duration_nanos = duration_nanos as i64;
    }

    pub fn set_profile_type(&mut self, strings: &mut StringTable, period: K::PeriodConfig) {
        K::set_profile_type(&mut self.profile, strings, period);
    }

    fn flush_ffi_samples(&mut self) {
        self.profile.sample.reserve(self.ffi_samples.len());
        for (location_id, value) in self.ffi_samples.drain() {
            self.profile.sample.push(Sample {
                location_id,
                value: value.as_ref().to_vec(),
                label: vec![],
            });
        }
    }

    pub fn add_function_mirror(&mut self, fm: FunctionMirror) -> u64 {
        let v = self.functions.get(&fm);
        if let Some(v) = v {
            return *v;
        }
        assert_ne!(self.functions.len(), self.profile.function.len() + 1);
        let id: u64 = self.functions.len() as u64 + 1;
        let f = Function {
            id,
            name: fm.name.pprof(),
            system_name: 0,
            filename: fm.filename.pprof(),
            start_line: 0,
        };
        self.functions.insert(fm, id);
        self.profile.function.push(f);
        id
    }

    pub fn add_location_mirror(&mut self, lm: LocationMirror) -> u64 {
        let v = self.locations.get(&lm);
        if let Some(v) = v {
            return *v;
        }
        assert_ne!(self.locations.len(), self.profile.location.len() + 1);
        let id: u64 = self.locations.len() as u64 + 1;
        let l = Location {
            id,
            mapping_id: 0,
            address: 0,
            line: vec![Line {
                function_id: lm.function_id,
                line: lm.line,
            }],
            is_folded: false,
        };
        self.locations.insert(lm, id);
        self.profile.location.push(l);
        id
    }

    pub fn reset(&mut self) {
        self.profile.sample.clear();
        self.profile.function.clear();
        self.profile.location.clear();
        self.profile.string_table.clear();
        self.profile.time_nanos = 0;
        self.profile.duration_nanos = 0;
        self.locations.clear();
        self.functions.clear();
        self.ffi_samples.clear();
        self.ffi_locations_scratch.clear();
    }
    pub fn take_profile_and_reset(
        &mut self,
        st: &StringTable,
        time_range: &TimeRange,
    ) -> Option<Profile> {
        self.flush_ffi_samples();
        if self.profile.sample.is_empty() {
            self.reset();
            return None;
        }
        self.set_time_range(time_range);
        st.clone_pprof_table(&mut self.profile.string_table);
        let profile = std::mem::take(&mut self.profile);
        self.reset();
        Some(profile)
    }
}

impl<K: FfiProfileKind> PProfBuilder<K> {
    pub fn add_ffi_sample(&mut self, frames: &[FFIFrame], values: &FFISampleValues) {
        let sample_values = K::value_slots(values);
        let mut location_ids = std::mem::take(&mut self.ffi_locations_scratch);
        location_ids.clear();
        location_ids.reserve(frames.len());

        for f in frames {
            let line = f.line as i64;
            let function_id = self.add_function_mirror(FunctionMirror {
                name: (&f.function_name).into(),
                filename: (&f.file_name).into(),
            });
            location_ids.push(self.add_location_mirror(LocationMirror { function_id, line }));
        }

        match self.ffi_samples.entry_ref(location_ids.as_slice()) {
            EntryRef::Occupied(mut entry) => {
                let accumulated = entry.get_mut().as_mut();
                for (accumulated, value) in accumulated.iter_mut().zip(sample_values.as_ref()) {
                    *accumulated = accumulated.saturating_add(*value);
                }
            }
            EntryRef::Vacant(entry) => {
                entry.insert_entry_with_key(location_ids.clone(), sample_values);
            }
        }
        self.ffi_locations_scratch = location_ids;
    }
}

impl PProfBuilder<PySpyProfile> {
    pub fn add_stacktrace(
        &mut self,
        strings: &mut StringTable,
        stacktrace: StackTrace,
        value: usize,
    ) {
        let mut sample = Sample {
            location_id: vec![],
            value: vec![value as i64 * self.profile.period],
            label: vec![],
        };
        for sf in stacktrace.frames {
            let name = strings.add(&sf.name); //todo move
            let filename = strings.add(&sf.filename); //todo move
            let line = sf.line as i64;
            let function_id = self.add_function_mirror(FunctionMirror { name, filename });
            let location_id = self.add_location_mirror(LocationMirror { function_id, line });
            sample.location_id.push(location_id);
        }
        for l in stacktrace.metadata.tags {
            sample.label.push(Label {
                key: strings.add(&l.key).pprof(),   //todo move
                str: strings.add(&l.value).pprof(), //todo move
                num: 0,
                num_unit: 0,
            });
        }
        self.profile.sample.push(sample);
    }
}

pub fn encode(reports: Vec<Report>, sample_rate: u32, time_range: TimeRange) -> Profile {
    let mut strings: StringTable = StringTable::new();
    let mut b = PProfBuilder::<PySpyProfile>::new();
    b.set_time_range(&time_range);
    b.set_profile_type(&mut strings, sample_rate);
    for report in reports {
        for (stacktrace, value) in report.data {
            b.add_stacktrace(&mut strings, stacktrace, value);
        }
    }
    b.profile.string_table = strings.into_pprof_table();
    b.profile
}

#[derive(Hash, PartialEq, Eq, Clone)]
pub struct StringID {
    pub index: u32,
}

impl StringID {
    pub(crate) fn pprof(&self) -> i64 {
        self.index as i64
    }
}

impl StringID {
    pub fn new(vec: &hashbrown::HashMap<StringSetKey, ()>) -> Self {
        let id: usize = vec.len();
        assert!(id < u32::MAX as usize);
        let id: u32 = id as u32;
        Self { index: id }
    }
    pub fn empty_ffi_string() -> FFIInternedString {
        FFIInternedString { index: 0 }
    }
}

impl From<&FFIInternedString> for StringID {
    fn from(value: &FFIInternedString) -> Self {
        Self { index: value.index }
    }
}

impl From<&StringID> for FFIInternedString {
    fn from(value: &StringID) -> Self {
        Self { index: value.index }
    }
}

pub struct StringTable {
    pub set: hashbrown::HashMap<StringSetKey, ()>,
}

pub struct StringSetKey {
    pub str: String,
    pub index: StringID,
}

impl Borrow<str> for StringSetKey {
    fn borrow(&self) -> &str {
        &self.str
    }
}

// impl Borrow<String> for StringSetKey {
//     fn borrow(&self) -> &String {
//         &self.str
//     }
// }

impl Hash for StringSetKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.str.hash(state)
    }
}

impl PartialEq<Self> for StringSetKey {
    fn eq(&self, other: &Self) -> bool {
        self.str == other.str
    }
}

impl Eq for StringSetKey {}

impl Default for StringTable {
    fn default() -> Self {
        Self::new()
    }
}

impl StringTable {
    pub fn new() -> Self {
        let mut s = Self {
            set: hashbrown::HashMap::new(),
        };
        s.add("");
        s
    }

    pub fn add(&mut self, s: &str) -> StringID {
        let next_id = StringID::new(&self.set);
        let k = self.set.entry_ref(s);
        match k {
            EntryRef::Occupied(v) => v.key().index.clone(),
            EntryRef::Vacant(v) => {
                let k = StringSetKey {
                    str: s.to_owned(),
                    index: next_id.clone(),
                };
                v.insert_entry_with_key(k, ());
                next_id
            }
        }
    }

    #[cfg(debug_assertions)]
    pub fn debug_get_ffi(&self, id: &FFIInternedString) -> &str {
        self.set
            .iter()
            .find(|it| it.0.index.index == id.index)
            .map(|it| it.0.str.as_str())
            .unwrap_or("NOT FOUND WTF")
    }

    pub fn memory_size_bytes(&self) -> usize {
        let mut sz: usize = self.set.allocation_size();
        for x in self.set.keys() {
            sz += x.str.len();
        }
        sz
    }

    pub fn into_pprof_table(self) -> Vec<String> {
        let mut v = vec!["".to_string(); self.set.len()];
        for x in self.set.into_keys() {
            let idx = x.index.index as usize;
            v[idx] = x.str;
        }
        v
    }

    pub fn clone_pprof_table(&self, dst: &mut Vec<String>) {
        dst.clear();
        dst.resize(self.set.len(), String::new());
        for x in self.set.keys() {
            let idx = x.index.index as usize;
            dst[idx] = x.str.clone();
        }
    }
}

pub mod ffi {
    use std::ffi::{c_char, c_int};

    #[repr(C)]
    pub struct FFIFrame {
        pub function_name: FFIInternedString,
        pub file_name: FFIInternedString,
        pub line: c_int,
    }

    #[repr(C)]
    pub struct FFIStringView {
        pub data: *const c_char,
        pub len: usize,
    }

    #[repr(C)]
    pub struct FFISampleValues {
        pub cpu_time: i64,
        pub wall_time: i64,
        pub alloc_space: usize,
        pub alloc_count: usize,
        pub heap_space: usize,
        pub heap_count: usize,
    }

    #[repr(C)]
    pub struct FFIInternedString {
        pub index: u32,
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::{FFIFrame, FFIInternedString, FFISampleValues};
    use super::{
        CpuWallProfile, FunctionMirror, LocationMirror, MemoryProfile, PProfBuilder, StringID,
        StringTable,
    };
    use crate::utils::TimeRange;
    use std::time::{Duration, UNIX_EPOCH};

    fn frame(function_name: u32, file_name: u32, line: i32) -> FFIFrame {
        FFIFrame {
            function_name: FFIInternedString {
                index: function_name,
            },
            file_name: FFIInternedString { index: file_name },
            line,
        }
    }

    fn values(
        heap_space: usize,
        heap_count: usize,
        alloc_space: usize,
        alloc_count: usize,
    ) -> FFISampleValues {
        FFISampleValues {
            cpu_time: 0,
            wall_time: 0,
            heap_space,
            heap_count,
            alloc_space,
            alloc_count,
        }
    }

    fn cpu_wall_values(cpu_time: i64, wall_time: i64) -> FFISampleValues {
        FFISampleValues {
            cpu_time,
            wall_time,
            heap_space: 0,
            heap_count: 0,
            alloc_space: 0,
            alloc_count: 0,
        }
    }

    #[test]
    fn equal_ffi_stacks_are_accumulated_element_wise() {
        let mut builder = PProfBuilder::<MemoryProfile>::new();
        let frames = [frame(1, 2, 10), frame(3, 4, 20)];

        builder.add_ffi_sample(&frames, &values(0, 0, 100, 2));
        builder.add_ffi_sample(&frames, &values(300, 4, 0, 0));

        assert_eq!(builder.ffi_samples.len(), 1);
        assert!(builder.profile.sample.is_empty());

        builder.flush_ffi_samples();

        assert_eq!(builder.profile.sample.len(), 1);
        assert_eq!(builder.profile.sample[0].value, vec![2, 100, 4, 300]);
    }

    /// cpu/wall values are larger than every memory value here, so a leak
    /// into a memory slot cannot pass for a correct number.
    #[test]
    fn memory_projection_reads_only_the_memory_slots() {
        let mut builder = PProfBuilder::<MemoryProfile>::new();
        let frames = [frame(1, 2, 10)];

        builder.add_ffi_sample(
            &frames,
            &FFISampleValues {
                cpu_time: 7_000_000,
                wall_time: 9_000_000,
                alloc_space: 100,
                alloc_count: 2,
                heap_space: 300,
                heap_count: 4,
            },
        );

        builder.flush_ffi_samples();

        assert_eq!(builder.profile.sample.len(), 1);
        // [alloc_objects, alloc_space, inuse_objects, inuse_space]
        assert_eq!(builder.profile.sample[0].value, vec![2, 100, 4, 300]);
    }

    /// The memory values are larger than every time value here, so a leak
    /// into a time slot cannot pass for a correct number.
    #[test]
    fn cpu_wall_projection_reads_only_the_time_slots() {
        let mut builder = PProfBuilder::<CpuWallProfile>::new();
        let frames = [frame(1, 2, 10)];

        builder.add_ffi_sample(
            &frames,
            &FFISampleValues {
                cpu_time: 7,
                wall_time: 9,
                alloc_space: 100_000,
                alloc_count: 200_000,
                heap_space: 300_000,
                heap_count: 400_000,
            },
        );

        builder.flush_ffi_samples();

        assert_eq!(builder.profile.sample.len(), 1);
        // [cpu, wall]
        assert_eq!(builder.profile.sample[0].value, vec![7, 9]);
    }

    #[test]
    fn equal_cpu_wall_stacks_are_accumulated_element_wise() {
        let mut builder = PProfBuilder::<CpuWallProfile>::new();
        let frames = [frame(1, 2, 10), frame(3, 4, 20)];

        builder.add_ffi_sample(&frames, &cpu_wall_values(3, 5));
        builder.add_ffi_sample(&frames, &cpu_wall_values(7, 11));
        builder.add_ffi_sample(&[frame(1, 2, 10)], &cpu_wall_values(1, 2));

        builder.flush_ffi_samples();

        let mut sample_values: Vec<_> = builder
            .profile
            .sample
            .iter()
            .map(|sample| sample.value.clone())
            .collect();
        sample_values.sort();
        assert_eq!(sample_values, vec![vec![1, 2], vec![10, 16]]);
    }

    /// take_profile_and_reset mem::takes the profile, so the dump path re-sets
    /// the sample types every window. Assigning rather than pushing them is
    /// what keeps that idempotent.
    #[test]
    fn cpu_wall_profile_type_is_idempotent() {
        let mut builder = PProfBuilder::<CpuWallProfile>::new();
        let mut strings = StringTable::new();

        builder.set_profile_type(&mut strings, 100);
        builder.set_profile_type(&mut strings, 100);

        assert_eq!(builder.profile.sample_type.len(), 2);
        assert_eq!(builder.profile.period, 10_000_000);
    }

    #[test]
    fn distinct_ffi_stacks_remain_distinct() {
        let mut builder = PProfBuilder::<MemoryProfile>::new();

        builder.add_ffi_sample(&[frame(1, 2, 10)], &values(0, 0, 100, 1));
        builder.add_ffi_sample(&[frame(1, 2, 20)], &values(0, 0, 200, 2));
        builder.flush_ffi_samples();

        assert_eq!(builder.profile.sample.len(), 2);
        let mut sample_values: Vec<_> = builder
            .profile
            .sample
            .iter()
            .map(|sample| sample.value.clone())
            .collect();
        sample_values.sort();
        assert_eq!(sample_values, vec![vec![1, 100, 0, 0], vec![2, 200, 0, 0]]);
    }

    #[test]
    fn take_profile_and_reset_moves_samples_and_resets() {
        let mut builder = PProfBuilder::<MemoryProfile>::new();
        let mut strings = StringTable::new();
        let time_range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();

        builder.set_profile_type(&mut strings, 512 * 1024);
        builder.add_ffi_sample(&[frame(1, 2, 10)], &values(300, 2, 100, 1));

        let profile = builder
            .take_profile_and_reset(&strings, &time_range)
            .expect("expected a profile with samples");
        assert_eq!(profile.sample.len(), 1);
        assert_eq!(profile.sample[0].value, vec![1, 100, 2, 300]);
        assert_eq!(profile.sample_type.len(), 4);
        assert_eq!(profile.string_table.len(), strings.set.len());
        assert_eq!(profile.duration_nanos, 10_000_000_000);

        assert!(
            builder
                .take_profile_and_reset(&strings, &time_range)
                .is_none()
        );
    }

    #[test]
    fn reset_discards_accumulated_ffi_samples() {
        let mut builder = PProfBuilder::<MemoryProfile>::new();
        let frames = [frame(1, 2, 10)];

        builder.add_ffi_sample(&frames, &values(0, 0, 100, 1));
        builder.reset();
        builder.add_ffi_sample(&frames, &values(0, 0, 200, 2));
        builder.flush_ffi_samples();

        assert_eq!(builder.profile.sample.len(), 1);
        assert_eq!(builder.profile.sample[0].value, vec![2, 200, 0, 0]);
    }

    #[test]
    fn new_string_table_interns_empty_at_zero() {
        // Every "interning failed, so index 0" path in encode::interner (and
        // the FFI contract documented on Pyroscope::intern_string) depends on
        // index 0 being the empty string.
        let mut strings = StringTable::new();
        assert_eq!(strings.add("").index, 0);
    }

    #[test]
    fn take_profile_and_reset_leaves_the_string_table_intact() {
        // The master table is shared by every profiler and must survive every
        // upload window: indices handed out to C++ in one window still have to
        // resolve in the next, because live tracebacks keep holding them.
        // dump_pprof is feature-gated and needs Python attached, so the
        // property is tested directly here.
        let mut builder = PProfBuilder::<MemoryProfile>::new();
        let mut strings = StringTable::new();
        let time_range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();

        let kept = strings.add("some.module:some_function");
        let len_before = strings.set.len();

        builder.set_profile_type(&mut strings, 512 * 1024);
        builder.add_ffi_sample(&[frame(kept.index, 2, 10)], &values(300, 2, 100, 1));
        builder
            .take_profile_and_reset(&strings, &time_range)
            .expect("a profile with one sample");

        // The table only ever grows here. set_memory_profile_type adds seven
        // distinct strings -- alloc_objects, alloc_space, inuse_objects,
        // inuse_space, count, bytes, space -- with "count" and "bytes" reused
        // across the four value types rather than re-added.
        assert_eq!(strings.set.len(), len_before + 7);
        assert_eq!(strings.add("some.module:some_function").index, kept.index);
    }

    #[test]
    fn functions_dedupe_on_name_and_filename_while_locations_keep_the_line() {
        // This is the guarantee that replaces Datadog::intern_function. The
        // renderer no longer interns functions or caches function ids; it just
        // pushes (name_id, file_id, line) and relies on add_function_mirror
        // deduping on exactly (name, filename) -- upstream's key, modulo
        // system_name, which both sides leave empty.
        let mut builder = PProfBuilder::<MemoryProfile>::new();

        let a = builder.add_function_mirror(FunctionMirror {
            name: StringID { index: 1 },
            filename: StringID { index: 2 },
        });
        let b = builder.add_function_mirror(FunctionMirror {
            name: StringID { index: 1 },
            filename: StringID { index: 2 },
        });
        let c = builder.add_function_mirror(FunctionMirror {
            name: StringID { index: 1 },
            filename: StringID { index: 3 },
        });

        assert_eq!(a, b, "same (name, filename) must be one function");
        assert_ne!(a, c, "a different filename must be a different function");
        assert_eq!(
            builder.profile.function.len(),
            2,
            "one emitted Function per distinct (name, filename)"
        );

        // The line lives on the location, not the function, so the same
        // function at two lines is two locations.
        let l10 = builder.add_location_mirror(LocationMirror {
            function_id: a,
            line: 10,
        });
        let l20 = builder.add_location_mirror(LocationMirror {
            function_id: a,
            line: 20,
        });
        let l10_again = builder.add_location_mirror(LocationMirror {
            function_id: a,
            line: 10,
        });

        assert_ne!(l10, l20, "same function at two lines is two locations");
        assert_eq!(l10, l10_again, "identical locations must dedupe");
        assert_eq!(builder.profile.location.len(), 2);
    }
}
