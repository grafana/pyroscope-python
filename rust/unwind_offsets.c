// Run by build.rs against the target Python's headers. Prints the Rust constants of
// src/signal_sampler/unwind/layout.rs.

#define Py_BUILD_CORE
#include <Python.h>
#include <stddef.h>
#include <stdio.h>

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

static void print_usize(const char *name, size_t value) {
    printf("pub const %s: usize = %zu;\n", name, value);
}

static void print_option(const char *name, int some, size_t value) {
    if (some) {
        printf("pub const %s: Option<usize> = Some(%zu);\n", name, value);
    } else {
        printf("pub const %s: Option<usize> = None;\n", name);
    }
}

int main(void) {
    printf("pub const PY_VERSION: (u8, u8) = (%d, %d);\n", PY_MAJOR_VERSION, PY_MINOR_VERSION);
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
    printf("pub const SKIP_OWNERS: u8 = 0;\n");
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
    printf("pub const SKIP_OWNERS: u8 = 0;\n");
#else
    print_option("OWNER", 1, offsetof(_PyInterpreterFrame, owner));
#if PY_VERSION_HEX >= 0x030e0000
    printf("pub const SKIP_OWNERS: u8 = %d;\n",
           (1 << FRAME_OWNED_BY_INTERPRETER) | (1 << FRAME_OWNED_BY_CSTACK));
#else
    printf("pub const SKIP_OWNERS: u8 = %d;\n", 1 << FRAME_OWNED_BY_CSTACK);
#endif
#endif
#endif
    return 0;
}
