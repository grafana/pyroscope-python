use crate::backend::StackTrace;
use crate::backend::types::Report;
use crate::encode::r#gen::google::{Function, Label, Line, Location, Profile, Sample, ValueType};
use crate::encode::pprof::ffi::FFIInternedString;
use crate::encode::pprof::ffi::{FFIFrame, FFIHeapSampleValues};
use crate::utils::TimeRange;
use hashbrown::hash_map::EntryRef;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

pub struct PProfBuilder {
    profile: Profile,
    functions: HashMap<FunctionMirror, u64>,
    locations: HashMap<LocationMirror, u64>,
    memory_samples: hashbrown::HashMap<Vec<u64>, [i64; 4]>,
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

impl Default for PProfBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PProfBuilder {
    pub fn new() -> Self {
        PProfBuilder {
            functions: HashMap::new(),
            locations: HashMap::new(),
            memory_samples: hashbrown::HashMap::new(),
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
    pub fn set_cpu_profile_type(&mut self, strings: &mut StringTable, sample_rate: u32) {
        self.profile.sample_type.push(ValueType {
            r#type: strings.add("cpu").pprof(),
            unit: strings.add("nanoseconds").pprof(),
        });
        self.profile.period = 1_000_000_000 / sample_rate as i64;
        self.profile.period_type = Some(ValueType {
            r#type: strings.add("cpu").pprof(),
            unit: strings.add("nanoseconds").pprof(),
        });
    }

    pub fn set_memory_profile_type(&mut self, strings: &mut StringTable, heap_sample_rate: u64) {
        self.profile.sample_type = vec![
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
        self.profile.period = heap_sample_rate as i64;
        self.profile.period_type = Some(ValueType {
            r#type: strings.add("space").pprof(),
            unit: strings.add("bytes").pprof(),
        });
    }

    /// Add one stack trace with an explicit, final sample value.
    ///
    /// `value_nanos` is written to the profile as-is. Callers that count
    /// sampling ticks must multiply by the period first (see [`encode`]);
    /// callers whose sampler measures CPU time directly pass that measurement
    /// through (see [`encode_cpu_nanos`]).
    pub fn add_stacktrace(
        &mut self,
        strings: &mut StringTable,
        stacktrace: StackTrace,
        value_nanos: i64,
    ) {
        let mut sample = Sample {
            location_id: vec![],
            value: vec![value_nanos],
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

    pub fn add_ffi_sample(&mut self, frames: &[FFIFrame], values: &FFIHeapSampleValues) {
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

        // Order must match the sample_type order in set_memory_profile_type:
        // alloc_objects, alloc_space, inuse_objects, inuse_space.
        let sample_values = [
            values.alloc_count as i64,
            values.alloc_space as i64,
            values.heap_count as i64,
            values.heap_space as i64,
        ];
        match self.memory_samples.entry_ref(location_ids.as_slice()) {
            EntryRef::Occupied(mut entry) => {
                for (accumulated, value) in entry.get_mut().iter_mut().zip(sample_values) {
                    *accumulated = accumulated.saturating_add(value);
                }
            }
            EntryRef::Vacant(entry) => {
                entry.insert_entry_with_key(location_ids.clone(), sample_values);
            }
        }
        self.ffi_locations_scratch = location_ids;
    }

    fn flush_memory_samples(&mut self) {
        self.profile.sample.reserve(self.memory_samples.len());
        for (location_id, value) in self.memory_samples.drain() {
            self.profile.sample.push(Sample {
                location_id,
                value: value.to_vec(),
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
        self.memory_samples.clear();
        self.ffi_locations_scratch.clear();
    }
    pub fn take_profile_and_reset(
        &mut self,
        st: &StringTable,
        time_range: &TimeRange,
    ) -> Option<Profile> {
        self.flush_memory_samples();
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

/// Encode reports whose values are counts of sampling ticks.
///
/// Each tick is worth one sampling period of CPU. This is correct for a sampler
/// that fires once per period of CPU actually consumed: py-spy, which with
/// `gil_only` only ever records the thread holding the GIL.
pub fn encode(reports: Vec<Report>, sample_rate: u32, time_range: TimeRange) -> Profile {
    encode_inner(reports, sample_rate, time_range, |value, period| {
        value as i64 * period
    })
}

/// Encode reports whose values are already CPU nanoseconds.
///
/// Used by samplers that measure per-thread CPU time themselves rather than
/// inferring it from a tick count. The distinction matters: a wall-clock
/// sampler that walks every thread produces one tick per thread per period, so
/// treating those ticks as CPU would report several times more CPU than the
/// process actually consumed, and would count blocked threads as busy.
pub fn encode_cpu_nanos(reports: Vec<Report>, sample_rate: u32, time_range: TimeRange) -> Profile {
    encode_inner(reports, sample_rate, time_range, |value, _period| {
        value as i64
    })
}

fn encode_inner(
    reports: Vec<Report>,
    sample_rate: u32,
    time_range: TimeRange,
    to_nanos: impl Fn(usize, i64) -> i64,
) -> Profile {
    let mut strings: StringTable = StringTable::new();
    let mut b = PProfBuilder::new();
    b.set_time_range(&time_range);
    b.set_cpu_profile_type(&mut strings, sample_rate);
    let period = b.profile.period;
    for report in reports {
        for (stacktrace, value) in report.data {
            b.add_stacktrace(&mut strings, stacktrace, to_nanos(value, period));
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
    pub struct FFISample {
        pub frames: *const FFIFrame,
        pub len: usize,
        pub values: FFIHeapSampleValues,
    }

    #[repr(C)]
    pub struct FFIHeapSampleValues {
        pub heap_space: usize,
        pub heap_count: usize,
        pub alloc_space: usize,
        pub alloc_count: usize,
    }

    #[repr(C)]
    pub struct FFIInternedString {
        pub index: u32,
    }

    /// A single CPU stack frame.
    ///
    /// Unlike [`FFIFrame`] (the memory path) the strings are not interned.
    /// The CPU sink builds owned `String`s so it can feed the existing
    /// `StackBuffer`/`StackFrame` types unchanged, which is what keeps a
    /// vendored CPU sampler on the same reporting path as py-spy.
    ///
    /// The string data is only borrowed for the duration of the
    /// `pyroscope_cpu_push_sample` call; the sink copies it.
    #[repr(C)]
    pub struct FFICpuFrame {
        pub function_name: FFIStringView,
        pub file_name: FFIStringView,
        pub line: c_int,
    }

    /// One aggregated CPU stack trace.
    ///
    /// `frames` is leaf-first, matching py-spy's ordering.
    #[repr(C)]
    pub struct FFICpuSample {
        pub frames: *const FFICpuFrame,
        pub len: usize,
        pub pid: u32,
        /// `pthread_t` of the sampled thread, so thread tag rules keep working.
        /// Zero when the sampler cannot attribute the sample to a thread.
        pub thread_id: u64,
        /// May be empty when the sampler does not know the thread name.
        pub thread_name: FFIStringView,
        /// CPU nanoseconds this sample accounts for.
        ///
        /// Deliberately CPU time rather than a tick count. A wall-clock sampler
        /// that walks every thread produces one tick per thread per period; if
        /// each of those were credited a full period of CPU, the profile would
        /// report several times more CPU than the process actually consumed and
        /// would count blocked threads as busy. A sampler that genuinely fires
        /// once per period of CPU would simply pass `ticks * period` here.
        ///
        /// A sample with zero CPU is dropped.
        pub cpu_nanos: u64,
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::{FFIFrame, FFIHeapSampleValues, FFIInternedString};
    use super::{PProfBuilder, StringTable};
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
    ) -> FFIHeapSampleValues {
        FFIHeapSampleValues {
            heap_space,
            heap_count,
            alloc_space,
            alloc_count,
        }
    }

    #[test]
    fn equal_ffi_stacks_are_accumulated_element_wise() {
        let mut builder = PProfBuilder::new();
        let frames = [frame(1, 2, 10), frame(3, 4, 20)];

        builder.add_ffi_sample(&frames, &values(0, 0, 100, 2));
        builder.add_ffi_sample(&frames, &values(300, 4, 0, 0));

        assert_eq!(builder.memory_samples.len(), 1);
        assert!(builder.profile.sample.is_empty());

        builder.flush_memory_samples();

        assert_eq!(builder.profile.sample.len(), 1);
        assert_eq!(builder.profile.sample[0].value, vec![2, 100, 4, 300]);
    }

    #[test]
    fn distinct_ffi_stacks_remain_distinct() {
        let mut builder = PProfBuilder::new();

        builder.add_ffi_sample(&[frame(1, 2, 10)], &values(0, 0, 100, 1));
        builder.add_ffi_sample(&[frame(1, 2, 20)], &values(0, 0, 200, 2));
        builder.flush_memory_samples();

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
        let mut builder = PProfBuilder::new();
        let mut strings = StringTable::new();
        let time_range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();

        builder.set_memory_profile_type(&mut strings, 512 * 1024);
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
        let mut builder = PProfBuilder::new();
        let frames = [frame(1, 2, 10)];

        builder.add_ffi_sample(&frames, &values(0, 0, 100, 1));
        builder.reset();
        builder.add_ffi_sample(&frames, &values(0, 0, 200, 2));
        builder.flush_memory_samples();

        assert_eq!(builder.profile.sample.len(), 1);
        assert_eq!(builder.profile.sample[0].value, vec![2, 200, 0, 0]);
    }
}
