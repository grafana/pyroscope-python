use super::*;

#[cfg(not(miri))]
fn frame_ids(frames: &[FFIFrame]) -> Vec<(u32, u32, i32)> {
    frames
        .iter()
        .map(|frame| (frame.function_name.index, frame.file_name.index, frame.line))
        .collect()
}

#[cfg(not(miri))]
pub(super) struct TestTypes;

#[cfg(not(miri))]
impl Types for TestTypes {
    fn code() -> usize {
        1
    }

    fn string() -> usize {
        2
    }

    fn bytes() -> usize {
        3
    }
}

#[cfg(not(miri))]
pub(super) fn put_word(bytes: &mut [u8], offset: usize, value: usize) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_ne_bytes());
}

#[cfg(not(miri))]
pub(super) fn string_object(value: &[u8], kind: usize, ascii: bool) -> Vec<u8> {
    let header = if ascii {
        layout::ASCII_SIZE
    } else {
        layout::COMPACT_SIZE
    };
    let mut object = vec![0; header + value.len()];
    put_word(&mut object, layout::OB_TYPE, TestTypes::string());
    put_word(&mut object, layout::UNICODE_LENGTH, value.len() / kind);
    let mut state = ((kind as u64) << layout::STATE_KIND_SHIFT) | layout::STATE_COMPACT;
    if ascii {
        state |= layout::STATE_ASCII;
    }
    #[cfg(not(Py_3_11))]
    {
        state |= layout::STATE_READY;
    }
    put_word(&mut object, layout::UNICODE_STATE, state as usize);
    object[header..].copy_from_slice(value);
    object
}

pub(super) fn recorded_frame(code: usize, byte_offset: usize) -> Frame {
    #[cfg(Py_3_11)]
    let instr = code + layout::CO_CODE_ADAPTIVE + byte_offset;
    #[cfg(not(Py_3_11))]
    let instr = byte_offset / 2;
    Frame { code, instr }
}

#[cfg(not(miri))]
pub(super) fn recovery() {
    // SAFETY: tests preserve kindasafe's handlers and do not install other handlers.
    unsafe { kindasafe_init::init() }.unwrap();
}

#[cfg(not(miri))]
#[test]
fn empty_slice_reads_do_not_access_the_source() {
    recovery();
    for address in [0, usize::MAX] {
        read_into(address, &mut []).unwrap();
        assert!(copy_bytes(address, 0, &mut []).unwrap().is_empty());
    }
}

#[cfg(not(miri))]
#[test]
fn reads_and_recovers_after_faults() {
    recovery();
    assert!(read_u64(0).is_err());
    assert!(copy_bytes(0, 4, &mut [0; 4]).is_err());
    assert!(matches!(
        copy_bytes(usize::MAX - 1, 4, &mut [0; 4]),
        Err(Error::ReadMemory)
    ));
    assert!(matches!(word(usize::MAX, 1), Err(Error::ReadMemory)));
    assert!(matches!(integer(usize::MAX, 1), Err(Error::ReadMemory)));
    assert!(copy_bytes(0, 0, &mut [0; 4]).unwrap().is_empty());
    assert!(matches!(
        copy_bytes(0, usize::MAX, &mut [0; 4]),
        Err(Error::Length)
    ));
    let value = 0x8123_4567_89ab_cdefu64;
    let address = &raw const value as usize;
    assert_eq!(read_u64(address).unwrap(), value);
    assert_eq!(word(address, 0).unwrap(), value as usize);
    for value in [i32::MIN, -1, 0, i32::MAX] {
        assert_eq!(integer(&raw const value as usize, 0).unwrap(), value);
    }
}

#[cfg(not(miri))]
#[test]
fn resolves_whole_stacks_and_counts_failures_once() {
    let _guard = crate::METRICS_LOCK.lock().unwrap();
    recovery();
    let mut strings = StringTable::new();
    let name = string_object(b"outer.inner", 1, true);
    let filename = string_object(b"source.py", 1, true);
    let name_id = strings.add("outer.inner");
    let filename_id = strings.add("source.py");
    #[cfg(Py_3_11)]
    let table = [0xe8, 0x02]; // no columns, one code unit, line delta +1
    #[cfg(not(Py_3_11))]
    let table = [2, 1];
    let mut bytes = vec![0; layout::BYTES_DATA + table.len()];
    put_word(&mut bytes, layout::OB_TYPE, TestTypes::bytes());
    put_word(&mut bytes, layout::BYTES_SIZE, table.len());
    bytes[layout::BYTES_DATA..].copy_from_slice(&table);
    let mut code = vec![0; layout::CO_LINETABLE + 8];
    put_word(&mut code, layout::OB_TYPE, TestTypes::code());
    put_word(&mut code, layout::CO_QUALNAME, name.as_ptr() as usize);
    put_word(&mut code, layout::CO_FILENAME, filename.as_ptr() as usize);
    put_word(&mut code, layout::CO_LINETABLE, bytes.as_ptr() as usize);
    code[layout::CO_FIRSTLINENO..layout::CO_FIRSTLINENO + 4].copy_from_slice(&20i32.to_ne_bytes());
    let frame = recorded_frame(code.as_ptr() as usize, 0);
    for (mode, line) in [
        (Lines::LastInstruction, 21),
        (Lines::First, 20),
        (Lines::NoLine, 0),
    ] {
        let mut resolver = Resolver::<TestTypes>::new(mode);
        assert_eq!(
            frame_ids(
                resolver
                    .resolve_stack(&[frame, frame], 1, &mut strings)
                    .unwrap()
            ),
            vec![(name_id.index, filename_id.index, line); 2]
        );
    }
    let mut resolver = Resolver::<TestTypes>::new(Lines::LastInstruction);
    let before = METRICS.resolve_error.load(Ordering::Relaxed);
    let invalid = Frame {
        code: usize::MAX,
        instr: usize::MAX,
    };
    assert!(matches!(
        resolver.resolve_stack(&[frame, invalid, invalid], 7, &mut strings),
        Err(Error::ReadMemory)
    ));
    assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 7);
    // Do not read line tables for First/NoLine. LastInstruction must validate the bytes type.
    put_word(&mut bytes, layout::OB_TYPE, 999);
    assert!(matches!(
        resolver.resolve_stack(&[frame], 11, &mut strings),
        Err(Error::ObjectType(_))
    ));
    assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 18);
    assert!(
        Resolver::<TestTypes>::new(Lines::First)
            .resolve_stack(&[frame], 11, &mut strings)
            .is_ok()
    );
    assert!(
        Resolver::<TestTypes>::new(Lines::NoLine)
            .resolve_stack(&[frame], 11, &mut strings)
            .is_ok()
    );
    assert!(
        resolver
            .resolve_stack(&[], 13, &mut strings)
            .unwrap()
            .is_empty()
    );
    assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 18);
}

#[cfg(not(miri))]
#[test]
fn rejects_recorded_code_addresses_reused_for_another_type() {
    let _guard = crate::METRICS_LOCK.lock().unwrap();
    recovery();
    let mut strings = StringTable::new();
    let name = string_object(b"original", 1, true);
    let filename = string_object(b"original.py", 1, true);
    let name_id = strings.add("original");
    let filename_id = strings.add("original.py");
    #[cfg(Py_3_11)]
    let table = [0xe8, 0x02];
    #[cfg(not(Py_3_11))]
    let table = [2, 1];
    let mut bytes = vec![0; layout::BYTES_DATA + table.len()];
    put_word(&mut bytes, layout::OB_TYPE, TestTypes::bytes());
    put_word(&mut bytes, layout::BYTES_SIZE, table.len());
    bytes[layout::BYTES_DATA..].copy_from_slice(&table);
    let mut code = vec![0; layout::CO_LINETABLE + 8];
    put_word(&mut code, layout::CO_QUALNAME, name.as_ptr() as usize);
    put_word(&mut code, layout::CO_FILENAME, filename.as_ptr() as usize);
    put_word(&mut code, layout::CO_LINETABLE, bytes.as_ptr() as usize);
    code[layout::CO_FIRSTLINENO..layout::CO_FIRSTLINENO + 4].copy_from_slice(&20i32.to_ne_bytes());
    let frame = recorded_frame(code.as_ptr() as usize, 0);

    for (mode, line) in [
        (Lines::LastInstruction, 21),
        (Lines::First, 20),
        (Lines::NoLine, 0),
    ] {
        let mut resolver = Resolver::<TestTypes>::new(mode);
        put_word(&mut code, layout::OB_TYPE, TestTypes::code());
        assert_eq!(
            frame_ids(resolver.resolve_stack(&[frame], 1, &mut strings).unwrap()),
            vec![(name_id.index, filename_id.index, line)]
        );

        // Model reuse after recording: the address is unchanged and every field the
        // decoder needs still looks valid, but ob_type now identifies another object.
        put_word(&mut code, layout::OB_TYPE, TestTypes::bytes());
        let before = METRICS.resolve_error.load(Ordering::Relaxed);
        let result = resolver
            .resolve_stack(&[frame, frame], 7, &mut strings)
            .map(frame_ids);
        assert!(
            matches!(result, Err(Error::ObjectType(address)) if address == frame.code),
            "reused code storage must be rejected for {mode:?}: {result:?}"
        );
        assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 7);
    }
}

#[cfg(not(miri))]
#[test]
fn interns_unicode_by_content_and_rereads_reused_string_storage() {
    recovery();
    let mut strings = StringTable::new();
    let mut name = string_object(&[0xe9], 1, false);
    let filename = string_object(&0x1f40du32.to_ne_bytes(), 4, false);
    let mut code = vec![0; layout::CO_LINETABLE + 8];
    put_word(&mut code, layout::OB_TYPE, TestTypes::code());
    put_word(&mut code, layout::CO_QUALNAME, name.as_ptr() as usize);
    put_word(&mut code, layout::CO_FILENAME, filename.as_ptr() as usize);
    let frame = recorded_frame(code.as_ptr() as usize, 0);
    let mut resolver = Resolver::<TestTypes>::new(Lines::NoLine);
    for (byte, text) in [(0xe9, "é"), (0xff, "ÿ"), (0xe9, "é")] {
        name[layout::COMPACT_SIZE] = byte;
        let resolved = resolver.resolve_stack(&[frame], 1, &mut strings).unwrap();
        let expected = (strings.add(text).index, strings.add("🐍").index, 0);
        assert_eq!(frame_ids(resolved), [expected]);
    }
    assert_eq!(strings.set.len(), 4); // Empty, two distinct names and one filename.
}

#[cfg(not(miri))]
#[test]
fn limits_both_function_names_and_filenames_to_max_string_bytes() {
    let _guard = crate::METRICS_LOCK.lock().unwrap();
    recovery();
    let mut strings = StringTable::new();
    let short = string_object(b"short", 1, true);
    let boundary_text = "a".repeat(strings::MAX_STRING_BYTES);
    let boundary = string_object(boundary_text.as_bytes(), 1, true);
    let oversized = string_object(&vec![b'a'; strings::MAX_STRING_BYTES + 1], 1, true);
    let mut code = vec![0; layout::CO_LINETABLE + 8];
    put_word(&mut code, layout::OB_TYPE, TestTypes::code());
    let frame = recorded_frame(code.as_ptr() as usize, 0);
    let mut resolver = Resolver::<TestTypes>::new(Lines::NoLine);
    for field in [layout::CO_QUALNAME, layout::CO_FILENAME] {
        put_word(&mut code, layout::CO_QUALNAME, short.as_ptr() as usize);
        put_word(&mut code, layout::CO_FILENAME, short.as_ptr() as usize);
        put_word(&mut code, field, oversized.as_ptr() as usize);
        let before = METRICS.resolve_error.load(Ordering::Relaxed);
        assert!(matches!(
            resolver.resolve_stack(&[frame, frame], 7, &mut strings),
            Err(Error::Length)
        ));
        assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 7);
        put_word(&mut code, field, boundary.as_ptr() as usize);
        let actual = frame_ids(resolver.resolve_stack(&[frame], 1, &mut strings).unwrap());
        let short_id = strings.add("short").index;
        let boundary_id = strings.add(&boundary_text).index;
        let expected = if field == layout::CO_QUALNAME {
            (boundary_id, short_id, 0)
        } else {
            (short_id, boundary_id, 0)
        };
        assert_eq!(actual, [expected]);
    }
}
