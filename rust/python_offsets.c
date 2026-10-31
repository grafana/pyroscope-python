// Run by build.rs against the target Python's headers. Writes the Rust constants of
// src/signal_sampler/unwind/layout.rs and src/signal_sampler/resolve/layout.rs into the
// directory given as the only argument.

#define Py_BUILD_CORE
#include <Python.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#if PY_VERSION_HEX >= 0x030b0000 && PY_VERSION_HEX < 0x030f0000
#define SUPPORTED 1
#if PY_VERSION_HEX >= 0x030e0000
#include <internal/pycore_interpframe_structs.h>
#include <internal/pycore_stackref.h>
#else
#include <internal/pycore_frame.h>
#endif
#else
#define SUPPORTED 0
#endif

static FILE *out;

static void print_usize(const char *name, size_t value) {
    fprintf(out, "pub const %s: usize = %zu;\n", name, value);
}

static void print_u64(const char *name, uint64_t value) {
    fprintf(out, "pub const %s: u64 = %#llx;\n", name, (unsigned long long)value);
}

static void print_option(const char *name, int some, size_t value) {
    if (some) {
        fprintf(out, "pub const %s: Option<usize> = Some(%zu);\n", name, value);
    } else {
        fprintf(out, "pub const %s: Option<usize> = None;\n", name);
    }
}

static FILE *open_output(const char *dir, const char *name) {
    char path[4096];
    snprintf(path, sizeof(path), "%s/%s", dir, name);
    FILE *file = fopen(path, "w");
    if (file == NULL) {
        perror(path);
    }
    return file;
}

#if SUPPORTED
// The resolver reads `state` as the 64-bit word at its offset, so a bit field's mask is the word
// with only that field set.
static uint64_t state_word(const PyASCIIObject *object) {
    uint64_t word;
    memcpy(&word, (const char *)object + offsetof(PyASCIIObject, state), sizeof(word));
    return word;
}
#endif

static void write_unwind(void) {
    fprintf(out, "pub const PY_VERSION: (u8, u8) = (%d, %d);\n", PY_MAJOR_VERSION, PY_MINOR_VERSION);
    print_usize("OB_TYPE", offsetof(PyObject, ob_type));
#if !SUPPORTED
    // Never read: the sampler refuses to start on these versions.
    print_usize("THREAD_FRAME", 0);
    print_option("CFRAME_FRAME", 0, 0);
    print_usize("EXECUTABLE", 0);
    print_usize("EXECUTABLE_TAG_BITS", 0);
    print_usize("PREVIOUS", 0);
    print_usize("INSTR", 0);
    print_option("OWNER", 0, 0);
    fprintf(out, "pub const SKIP_OWNERS: u8 = 0;\n");
#else
#if PY_VERSION_HEX < 0x030d0000
    print_usize("THREAD_FRAME", offsetof(PyThreadState, cframe));
    print_option("CFRAME_FRAME", 1, offsetof(_PyCFrame, current_frame));
#else
    print_usize("THREAD_FRAME", offsetof(PyThreadState, current_frame));
    print_option("CFRAME_FRAME", 0, 0);
#endif
#if PY_VERSION_HEX < 0x030d0000
    print_usize("EXECUTABLE", offsetof(_PyInterpreterFrame, f_code));
#else
    print_usize("EXECUTABLE", offsetof(_PyInterpreterFrame, f_executable));
#endif
#if PY_VERSION_HEX >= 0x030e0000
    print_usize("EXECUTABLE_TAG_BITS", Py_TAG_BITS);
#else
    print_usize("EXECUTABLE_TAG_BITS", 0);
#endif
    print_usize("PREVIOUS", offsetof(_PyInterpreterFrame, previous));
#if PY_VERSION_HEX < 0x030d0000
    print_usize("INSTR", offsetof(_PyInterpreterFrame, prev_instr));
#else
    print_usize("INSTR", offsetof(_PyInterpreterFrame, instr_ptr));
#endif
    // 3.11 has no frames owned by the C stack: its entry frames are Python frames.
#if PY_VERSION_HEX < 0x030c0000
    print_option("OWNER", 0, 0);
    fprintf(out, "pub const SKIP_OWNERS: u8 = 0;\n");
#else
    print_option("OWNER", 1, offsetof(_PyInterpreterFrame, owner));
#if PY_VERSION_HEX >= 0x030e0000
    fprintf(out, "pub const SKIP_OWNERS: u8 = %d;\n",
            (1 << FRAME_OWNED_BY_INTERPRETER) | (1 << FRAME_OWNED_BY_CSTACK));
#else
    fprintf(out, "pub const SKIP_OWNERS: u8 = %d;\n", 1 << FRAME_OWNED_BY_CSTACK);
#endif
#endif
#endif
}

static void write_resolve(void) {
#if !SUPPORTED
    // Never read: the sampler refuses to start on these versions.
    const char *names[] = {
        "CO_FILENAME", "CO_QUALNAME", "CO_FIRSTLINENO", "CO_LINETABLE", "CO_CODE_ADAPTIVE",
        "UNICODE_LENGTH", "UNICODE_STATE", "ASCII_SIZE", "COMPACT_SIZE", "BYTES_SIZE", "BYTES_DATA",
    };
    for (size_t i = 0; i < sizeof(names) / sizeof(names[0]); i++) {
        print_usize(names[i], 0);
    }
    print_u64("STATE_KIND_MASK", 0);
    print_u64("STATE_KIND_SHIFT", 0);
    print_u64("STATE_COMPACT", 0);
    print_u64("STATE_ASCII", 0);
#else
    print_usize("CO_FILENAME", offsetof(PyCodeObject, co_filename));
    print_usize("CO_QUALNAME", offsetof(PyCodeObject, co_qualname));
    print_usize("CO_FIRSTLINENO", offsetof(PyCodeObject, co_firstlineno));
    print_usize("CO_LINETABLE", offsetof(PyCodeObject, co_linetable));
    print_usize("CO_CODE_ADAPTIVE", offsetof(PyCodeObject, co_code_adaptive));
    print_usize("UNICODE_LENGTH", offsetof(PyASCIIObject, length));
    print_usize("UNICODE_STATE", offsetof(PyASCIIObject, state));
    print_usize("ASCII_SIZE", sizeof(PyASCIIObject));
    print_usize("COMPACT_SIZE", sizeof(PyCompactUnicodeObject));
    print_usize("BYTES_SIZE", offsetof(PyBytesObject, ob_base.ob_size));
    print_usize("BYTES_DATA", offsetof(PyBytesObject, ob_sval));

    PyASCIIObject object;
    memset(&object, 0, sizeof(object));
    object.state.kind = 7;
    uint64_t kind_mask = state_word(&object);
    print_u64("STATE_KIND_MASK", kind_mask);
    print_u64("STATE_KIND_SHIFT", __builtin_ctzll(kind_mask));
    memset(&object, 0, sizeof(object));
    object.state.compact = 1;
    print_u64("STATE_COMPACT", state_word(&object));
    memset(&object, 0, sizeof(object));
    object.state.ascii = 1;
    print_u64("STATE_ASCII", state_word(&object));
#endif
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s <out dir>\n", argv[0]);
        return 1;
    }
    if ((out = open_output(argv[1], "unwind_layout.rs")) == NULL) {
        return 1;
    }
    write_unwind();
    if (fclose(out) != 0) {
        return 1;
    }
    if ((out = open_output(argv[1], "resolve_layout.rs")) == NULL) {
        return 1;
    }
    write_resolve();
    return fclose(out) == 0 ? 0 : 1;
}
