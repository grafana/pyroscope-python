#include <atomic>
#include <mutex>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#define PY_SSIZE_T_CLEAN
#include <Python.h>

#include "_pymacro.h"

extern "C" {
#include "pyroscope_ffi.h"
}

// Pyroscope patch: Pyroscope uses its Rust profile builder and does not provide
// Datadog's ddup interface.
// #include "ddup_interface.hpp"

/* Pyroscope patch: the per-domain context struct is gone. It carried
   max_nframe, which now lives in the Rust heap tracker and is passed once to
   pyroscope_memprof_heap_init, and a domain field the C++ never read. The
   hooks are installed with a null ctx; the OBJ and MEM domains are still
   distinguished by having separate hook functions, as before. */

static bool memalloc_enabled = false;
#ifdef _PY312_AND_LATER
/* true only while MEM-domain hooks are installed (between start() with the
 * caller's mem_domain_enabled=true and the next stop()). */
static bool memalloc_mem_installed = false;
#endif // _PY312_AND_LATER
// Pyroscope patch: Rust registers the handler with os.register_at_fork, so this
// Datadog-only registration guard is intentionally disabled.
// static std::once_flag memalloc_fork_handler_once_flag;

/* Two-slot buffer for atomically publishing the saved (original) allocator.
 *
 * Each start() cycle writes into the slot at index g_saved_alloc_slot (then
 * flips the index) and then atomically publishes the pointer via
 * g_saved_alloc_pub.  Hooks load the pointer atomically, so a hook from
 * cycle N always refers to g_saved_alloc_buf[N%2] while start() for cycle
 * N+1 writes to g_saved_alloc_buf[(N+1)%2].  These are disjoint memory
 * locations, so no data race exists on the struct fields themselves. */
static PyMemAllocatorEx g_saved_alloc_buf[2];
static int g_saved_alloc_slot = 0;
static std::atomic<const PyMemAllocatorEx*> g_saved_alloc_pub{ nullptr };

#ifdef _PY312_AND_LATER
/* MEM-domain saved allocator — same two-slot scheme as OBJ above, kept as
 * an independent buffer + atomic pair so each domain's lifecycle is fully
 * isolated from OBJ. */
static PyMemAllocatorEx g_saved_alloc_mem_buf[2];
static int g_saved_alloc_mem_slot = 0;
static std::atomic<const PyMemAllocatorEx*> g_saved_alloc_mem_pub{ nullptr };
#endif // _PY312_AND_LATER

static void
memalloc_free(void* Py_UNUSED(ctx), void* ptr)
{
    if (ptr == NULL)
        return;


    /* Load atomically so we see a consistent slot written by start() even if
     * a concurrent restart is in progress.  A NULL guard matches alloc and
     * realloc: leaking is safer than crashing through a torn pointer. */
    const PyMemAllocatorEx* const saved = g_saved_alloc_pub.load(std::memory_order_acquire);
    if (!saved)
        return;
    PyMemAllocatorEx alloc = *saved;
    if (!alloc.free)
        return;
    pyroscope_memprof_heap_untrack(ptr);
    alloc.free(alloc.ctx, ptr);
}

static void*
memalloc_alloc(int use_calloc, void* Py_UNUSED(ctx), size_t nelem, size_t elsize)
{
    void* ptr;

    /* Load the saved allocator atomically.  g_saved_alloc_pub points into a
     * two-slot buffer; start() always writes to the slot that no in-flight
     * hook from the previous cycle is reading, so the struct copy below is
     * data-race-free.  A NULL guard matches realloc and free. */
    const PyMemAllocatorEx* const saved = g_saved_alloc_pub.load(std::memory_order_acquire);
    if (!saved)
        return nullptr;
    PyMemAllocatorEx alloc = *saved;

    if (use_calloc) {
        if (!alloc.calloc)
            return nullptr;
        ptr = alloc.calloc(alloc.ctx, nelem, elsize);
    } else {
        if (!alloc.malloc)
            return nullptr;
        ptr = alloc.malloc(alloc.ctx, nelem * elsize);
    }

    if (ptr) {
        pyroscope_memprof_heap_track(ptr, nelem * elsize);
    }

    return ptr;
}

static void*
memalloc_malloc(void* ctx, size_t size)
{
    return memalloc_alloc(0, ctx, 1, size);
}

static void*
memalloc_calloc(void* ctx, size_t nelem, size_t elsize)
{
    return memalloc_alloc(1, ctx, nelem, elsize);
}

static void*
memalloc_realloc(void* Py_UNUSED(ctx), void* ptr, size_t new_size)
{
    /* Load atomically — same two-slot scheme as memalloc_alloc. */
    const PyMemAllocatorEx* const saved = g_saved_alloc_pub.load(std::memory_order_acquire);
    if (!saved)
        return nullptr;
    PyMemAllocatorEx alloc = *saved;
    if (!alloc.realloc)
        return nullptr;
    void* ptr2 = alloc.realloc(alloc.ctx, ptr, new_size);
    // The GIL is held here since we're using PYMEM_DOMAIN_OBJ.
    // TODO(dsn): With Python free-threading, allocators must be thread-safe even for non-RAW domains.
    // We may need to add synchronization here in the future to avoid races between realloc and untrack.
    if (ptr2) {
        pyroscope_memprof_heap_untrack(ptr);
        pyroscope_memprof_heap_track(ptr2, new_size);
    } else if (new_size == 0 && ptr != NULL) {
        // realloc(ptr, 0) is implementation-defined: some allocators (including
        // glibc) free ptr and return NULL.  In that case ptr is gone and must be
        // untracked so allocs_m doesn't keep a dangling/stale entry forever.
        // When new_size > 0 and ptr2 == NULL the allocation failed; ptr is
        // still valid and must stay tracked, so we only act on new_size == 0.
        pyroscope_memprof_heap_untrack(ptr);
    }

    return ptr2;
}

#ifdef _PY312_AND_LATER
/* ---------------------------------------------------------------------------
 * PYMEM_DOMAIN_MEM hooks — direct copies of the OBJ hooks above, reading
 * the MEM-domain saved-allocator atomic pair. Kept as separate function
 * definitions (rather than parameterizing hooks) so each function
 * can be reverted independently of the other.
 * --------------------------------------------------------------------------- */

static void
memalloc_free_mem(void* Py_UNUSED(ctx), void* ptr)
{
    if (ptr == NULL)
        return;


    const PyMemAllocatorEx* const saved = g_saved_alloc_mem_pub.load(std::memory_order_acquire);
    if (!saved)
        return;
    PyMemAllocatorEx alloc = *saved;
    if (!alloc.free)
        return;
    pyroscope_memprof_heap_untrack(ptr);
    alloc.free(alloc.ctx, ptr);
}

static void*
memalloc_alloc_mem(int use_calloc, void* Py_UNUSED(ctx), size_t nelem, size_t elsize)
{
    void* ptr;

    const PyMemAllocatorEx* const saved = g_saved_alloc_mem_pub.load(std::memory_order_acquire);
    if (!saved)
        return nullptr;
    PyMemAllocatorEx alloc = *saved;

    if (use_calloc) {
        if (!alloc.calloc)
            return nullptr;
        ptr = alloc.calloc(alloc.ctx, nelem, elsize);
    } else {
        if (!alloc.malloc)
            return nullptr;
        ptr = alloc.malloc(alloc.ctx, nelem * elsize);
    }

    if (ptr) {
        pyroscope_memprof_heap_track(ptr, nelem * elsize);
    }

    return ptr;
}

static void*
memalloc_malloc_mem(void* ctx, size_t size)
{
    return memalloc_alloc_mem(0, ctx, 1, size);
}

static void*
memalloc_calloc_mem(void* ctx, size_t nelem, size_t elsize)
{
    return memalloc_alloc_mem(1, ctx, nelem, elsize);
}

static void*
memalloc_realloc_mem(void* Py_UNUSED(ctx), void* ptr, size_t new_size)
{
    const PyMemAllocatorEx* const saved = g_saved_alloc_mem_pub.load(std::memory_order_acquire);
    if (!saved)
        return nullptr;
    PyMemAllocatorEx alloc = *saved;
    if (!alloc.realloc)
        return nullptr;
    void* ptr2 = alloc.realloc(alloc.ctx, ptr, new_size);
    if (ptr2) {
        pyroscope_memprof_heap_untrack(ptr);
        pyroscope_memprof_heap_track(ptr2, new_size);
    } else if (new_size == 0 && ptr != NULL) {
        pyroscope_memprof_heap_untrack(ptr);
    }

    return ptr2;
}
#endif // _PY312_AND_LATER

// Pyroscope patch: expose a typed C ABI entrypoint for the Rust integration
// instead of defining a Python extension-module callback. Return 0 on success
// and -1 with a Python exception on failure so Rust can distinguish the paths.
extern "C" int memalloc_start(    uint16_t max_nframe,
    uint64_t heap_sample_size,
    bool enable_mem_domain)
{
    if (memalloc_enabled) {
        PyErr_SetString(PyExc_RuntimeError, "the memalloc module is already started");
        return -1;
    }

    // Pyroscope patch: the Rust profile builder owns profiler initialization,
    // so Datadog's ddup state must not be started here.
    // ddup_start();

    // Register fork handler
    // Mainly to clear the heap tracker state before running any Python code,
    // otherwise it can lead to undefined behaviors and/or crashes, ref:
    // incident-48649.
    // We use std::call_once as registered fork handlers persist after fork, and
    // we want to ensure that the fork handlers are registered only once per
    // process, even when the memory profiler is restarted after fork.
    // Pyroscope patch: Rust registers this handler with os.register_at_fork so
    // it follows Python's fork lifecycle and is not invoked twice.
    // std::call_once(memalloc_fork_handler_once_flag,
    //                []() { pthread_atfork(nullptr, nullptr, memalloc_heap_postfork_child); });

    char* val = getenv("_DD_MEMALLOC_DEBUG_RNG_SEED");
    if (val) {
        /* NB: we don't bother checking whether val is actually a valid integer.
         * Doesn't really matter as long as it's consistent */
        srand(atoi(val));
    }




    /* Pyroscope patch: kept in sync with TRACEBACK_MAX_NFRAME in
       rust/src/memalloc/limits.rs, which is now the definition. */
    static const uint16_t MAX_NFRAME = 600;
    if (max_nframe < 1 || max_nframe > MAX_NFRAME) {
        PyErr_Format(PyExc_ValueError, "the number of frames must be in range [1; %u]", MAX_NFRAME);
        return -1;
    }

    /* Pyroscope patch: kept in sync with MAX_HEAP_SAMPLE_SIZE in
       rust/src/memalloc/limits.rs. */
    if (heap_sample_size > UINT32_MAX) {
        PyErr_Format(PyExc_ValueError, "the heap sample size must be in range [0; %u]", UINT32_MAX);
        return -1;
    }

    if (!pyroscope_memprof_heap_init((uint32_t)heap_sample_size, max_nframe)) {
        PyErr_SetString(PyExc_RuntimeError, "failed to initialize heap tracker");
        return -1;
    }

    PyMemAllocatorEx alloc;

    alloc.malloc = memalloc_malloc;
    alloc.calloc = memalloc_calloc;
    alloc.realloc = memalloc_realloc;
    alloc.free = memalloc_free;

    alloc.ctx = nullptr;

    /* Write the saved (original) allocator into whichever slot is NOT
     * currently being read by hooks from the previous cycle, then publish the
     * pointer atomically.  Because start() and the previous stop() hold the
     * GIL sequentially, all in-flight hooks from the prior cycle are already
     * using the *old* slot; writing to the new slot is therefore data-race-free. */
    const int slot = g_saved_alloc_slot;
    g_saved_alloc_slot = 1 - slot;
    PyMem_GetAllocator(PYMEM_DOMAIN_OBJ, &g_saved_alloc_buf[slot]);
    g_saved_alloc_pub.store(&g_saved_alloc_buf[slot], std::memory_order_release);
    PyMem_SetAllocator(PYMEM_DOMAIN_OBJ, &alloc);

#ifdef _PY312_AND_LATER
    if (enable_mem_domain) {
        PyMemAllocatorEx alloc_mem;
        alloc_mem.malloc = memalloc_malloc_mem;
        alloc_mem.calloc = memalloc_calloc_mem;
        alloc_mem.realloc = memalloc_realloc_mem;
        alloc_mem.free = memalloc_free_mem;
        alloc_mem.ctx = nullptr;

        const int mem_slot = g_saved_alloc_mem_slot;
        g_saved_alloc_mem_slot = 1 - mem_slot;
        PyMem_GetAllocator(PYMEM_DOMAIN_MEM, &g_saved_alloc_mem_buf[mem_slot]);
        g_saved_alloc_mem_pub.store(&g_saved_alloc_mem_buf[mem_slot], std::memory_order_release);
        PyMem_SetAllocator(PYMEM_DOMAIN_MEM, &alloc_mem);
        memalloc_mem_installed = true;
    }
#else
    (void)enable_mem_domain; // silence -Wunused-variable on Python < 3.12
#endif // _PY312_AND_LATER

    memalloc_enabled = true;

    return 0;
}


// Pyroscope patch: expose an idempotent C ABI stop entrypoint for Rust instead
// of a Python extension-module callback that raises when already stopped.
extern "C" void memalloc_stop()
{
    if (!memalloc_enabled) {
        return;
    }

    /* First, uninstall our wrappers. There may still be calls to our wrapper in progress,
     * if they happened to release the GIL.
     * NB: We're assuming here that this is not called concurrently with iter_events
     * or memalloc_heap. The higher-level collector deals with this.
     *
     * Load atomically so we see the fully-written slot published by start(). */
    const PyMemAllocatorEx* saved = g_saved_alloc_pub.load(std::memory_order_acquire);
    if (saved) {
        PyMemAllocatorEx restore = *saved;
        PyMem_SetAllocator(PYMEM_DOMAIN_OBJ, &restore);
    }

#ifdef _PY312_AND_LATER
    if (memalloc_mem_installed) {
        const PyMemAllocatorEx* saved_mem = g_saved_alloc_mem_pub.load(std::memory_order_acquire);
        if (saved_mem) {
            PyMemAllocatorEx restore_mem = *saved_mem;
            PyMem_SetAllocator(PYMEM_DOMAIN_MEM, &restore_mem);
        }
        /* Pyroscope patch: deliberately leave g_saved_alloc_mem_pub pointing at the valid saved
         * allocator (mirroring the OBJ path above, which never nulls
         * g_saved_alloc_pub). Once PyMem_SetAllocator has restored the real MEM
         * allocator, CPython no longer dispatches frees to our hook, so a stale
         * pointer is harmless. Nulling it here would make an in-flight
         * memalloc_free_mem load NULL and fast-exit without delegating to the
         * underlying free, leaking that allocation. */
        memalloc_mem_installed = false;
    }
#endif // _PY312_AND_LATER

    pyroscope_memprof_heap_deinit();

    memalloc_enabled = false;

}


// Pyroscope patch: expose an idempotent C ABI heap-export entrypoint for Rust
// instead of a Python extension-module callback that raises when not started.
extern "C" void memalloc_heap_py()
{
    if (!memalloc_enabled) {
        return;
    }

    pyroscope_memprof_heap_flush();
}
