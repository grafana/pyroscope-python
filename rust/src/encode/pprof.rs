use crate::backend::types::Report;
use crate::backend::StackTrace;
use crate::encode::gen::google::{Function, Label, Line, Location, Profile, Sample, ValueType};
use crate::encode::pprof::ffi::FFIInternedString;
use crate::encode::pprof::ffi::{FFIFrame, FFIHeapSampleValues};
use crate::utils::TimeRange;
use hashbrown::hash_map::EntryRef;
use prost::Message;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

pub struct PProfBuilder {
    profile: Profile,
    functions: HashMap<FunctionMirror, u64>,
    locations: HashMap<LocationMirror, u64>,
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
        let start_time_nanos = time_range.from * 1_000_000_000;
        let duration_nanos = (time_range.until - time_range.from) * 1_000_000_000;
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

    pub fn add_ffi_sample(&mut self, frames: &[FFIFrame], values: &FFIHeapSampleValues) {
        let mut sample = Sample {
            location_id: Vec::with_capacity(frames.len()),
            value: vec![
                values.alloc_count as i64,
                values.alloc_space as i64,
                values.heap_space as i64,
            ],
            label: vec![],
        };

        for f in frames {
            let line = f.line as i64;
            let function_id = self.add_function_mirror(FunctionMirror {
                name: (&f.function_name).into(),
                filename: (&f.file_name).into(),
            });
            let location_id = self.add_location_mirror(LocationMirror { function_id, line });
            sample.location_id.push(location_id);
        }
        self.profile.sample.push(sample);
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
    }
    pub fn encode_and_reset(
        &mut self,
        st: &StringTable,
        time_range: &TimeRange,
    ) -> Option<Vec<u8>> {
        if self.profile.sample.is_empty() {
            self.reset();
            None
        } else {
            self.set_time_range(time_range);
            st.clone_pprof_table(&mut self.profile.string_table);
            let res = self.profile.encode_to_vec();
            self.reset();
            Some(res)
        }
    }
}

pub fn encode(reports: Vec<Report>, sample_rate: u32, time_range: TimeRange) -> Profile {
    let mut strings: StringTable = StringTable::new();
    let mut b = PProfBuilder::new();
    b.set_time_range(&time_range);
    b.set_cpu_profile_type(&mut strings, sample_rate);
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
    pub struct FFISample {
        pub frames: *const FFIFrame,
        pub len: usize,
        pub values: FFIHeapSampleValues,
    }

    #[repr(C)]
    pub struct FFIHeapSampleValues {
        pub heap_space: usize,
        pub alloc_space: usize,
        pub alloc_count: usize,
    }

    #[repr(C)]
    pub struct FFIInternedString {
        pub index: u32,
    }
}
