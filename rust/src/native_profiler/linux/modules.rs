use super::{Location, NativeUnwinder, Profile};
use framehop::{ExplicitModuleSectionInfo, Module, Unwinder};
use object::{Object, ObjectSection, ObjectSegment};
use std::{fs, io};

pub(super) struct Modules {
    pub unwinder: NativeUnwinder,
    pub images: Vec<Image>,
}
pub(super) struct Image {
    pub path: String,
    pub start: u64,
    pub end: u64,
    pub offset: u64,
}

impl Modules {
    // Snapshot ELF images once, before arming timers. All unwind bytes are owned,
    // so framehop never consults the loader or reads a possibly unmapped DSO.
    // This experiment does not refresh modules loaded/unloaded during a session.
    pub fn load() -> io::Result<Self> {
        let mut this = Self {
            unwinder: NativeUnwinder::new(),
            images: Vec::new(),
        };
        for line in fs::read_to_string("/proc/self/maps")?.lines() {
            let mut fields = line.split_whitespace();
            let (Some(range), Some(perms), Some(offset), Some(_dev), Some(_inode)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            if !perms.as_bytes().get(2).is_some_and(|&p| p == b'x') {
                continue;
            }
            let path = fields.collect::<Vec<_>>().join(" ");
            if !path.starts_with('/') {
                continue;
            }
            let Some((start, end)) = range.split_once('-') else {
                continue;
            };
            let (Ok(start), Ok(end), Ok(offset)) = (
                u64::from_str_radix(start, 16),
                u64::from_str_radix(end, 16),
                u64::from_str_radix(offset, 16),
            ) else {
                continue;
            };
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(object) = object::File::parse(bytes.as_slice()) else {
                continue;
            };
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
            let Some(svma) = object.segments().find_map(|segment| {
                if !matches!(segment.flags(), object::SegmentFlags::Elf { p_flags, .. } if p_flags.0 & object::elf::PF_X.0 != 0) {
                    return None;
                }
                let (file_offset, size) = segment.file_range();
                // A PT_LOAD can begin in the middle of its first mapped page.
                if offset < file_offset / page_size * page_size || offset >= file_offset + size {
                    return None;
                }
                segment.address().checked_sub(file_offset)?.checked_add(offset)
            }) else {
                continue;
            };
            let Some(bias) = start.checked_sub(svma) else {
                continue;
            };
            let section = |name| {
                object.section_by_name(name).and_then(|s| {
                    Some((s.address()..s.address() + s.size(), s.data().ok()?.to_vec()))
                })
            };
            let mut info = ExplicitModuleSectionInfo::default();
            if let Some((range, bytes)) = section(".eh_frame") {
                info.eh_frame_svma = Some(range);
                info.eh_frame = Some(bytes);
            }
            if let Some((range, bytes)) = section(".eh_frame_hdr") {
                info.eh_frame_hdr_svma = Some(range);
                info.eh_frame_hdr = Some(bytes);
            }
            if let Some((range, bytes)) = section(".text") {
                info.text_svma = Some(range);
                info.text = Some(bytes);
            }
            this.unwinder
                .add_module(Module::new(path.clone(), start..end, bias, info));
            this.images.push(Image {
                path,
                start,
                end,
                offset,
            });
        }
        Ok(this)
    }
    pub fn location(&self, p: &mut Profile, address: u64) -> u64 {
        let image = self
            .images
            .iter()
            .enumerate()
            .find(|(_, image)| address >= image.start && address < image.end);
        let mut location = Location {
            id: p.location.len() as u64 + 1,
            address,
            ..Location::default()
        };
        if let Some((index, _)) = image {
            location.mapping_id = index as u64 + 1;
        }
        let id = location.id;
        p.location.push(location);
        id
    }
}
