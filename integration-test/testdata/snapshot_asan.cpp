// Exercise the sampler's guarded copy with real ASAN poisoning and page faults.
#include <echion/danger.h>
#include <cassert>
#include <cerrno>
#include <cstring>
#include <sanitizer/asan_interface.h>

ssize_t safe_memcpy(void* destination, const void* source, size_t size);

static const char* volatile returned_stack;

__attribute__((noinline)) static void
save_stack_address()
{
    char source[32] = "snapshot";
    returned_stack = source;
}

int main(int argc, char**)
{
    assert(init_segv_catcher() == 0);
    char source[32] = "snapshot";
    char destination[32];
    assert(safe_memcpy(destination, source, sizeof(source)) == sizeof(source));
    assert(std::memcmp(destination, source, sizeof(source)) == 0);

    if (argc > 1) {
        // This must still fail under ASAN: destination writes are instrumented.
        auto* too_small = new char[4];
        safe_memcpy(too_small, source, sizeof(source));
        delete[] too_small;
        return 0;
    }

    auto* freed = new char[32];
    delete[] freed;
    errno = 0;
    assert(safe_memcpy(destination, freed, sizeof(destination)) == -1);
    assert(errno == EFAULT);

    save_stack_address();
    errno = 0;
    assert(safe_memcpy(destination, returned_stack, sizeof(destination)) == -1);
    assert(errno == EFAULT);

    __asan_poison_memory_region(source + 16, 16);
    assert(safe_memcpy(destination, source, sizeof(source)) == -1);
    __asan_unpoison_memory_region(source + 16, 16);

    auto* inaccessible = mmap(nullptr, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(inaccessible != MAP_FAILED);
    errno = 0;
    assert(safe_memcpy(destination, inaccessible, sizeof(destination)) == -1);
    assert(errno == EFAULT);
    assert(munmap(inaccessible, 4096) == 0);
}
