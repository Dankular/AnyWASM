// Minimal /MD fixture for exercising native MSVC EH3 and EH4 metadata.
// Keep the caught type small and trivially copyable so the runtime work is about
// exception dispatch, type matching, and unwind actions rather than STL behavior.

static const int kBaseTag = 0x1357;
static const int kDerivedTag = 0x2468;
static volatile int g_destructorRuns = 0;
static volatile int g_copyRuns = 0;

struct Prefix {
    unsigned long long words[2];
};

struct Base {
    int tag;
};

struct Derived : Prefix, Base {
    int derivedTag;

    Derived()
        : Prefix{{0x0123456789abcdefULL, 0xfedcba9876543210ULL}},
          Base{kBaseTag},
          derivedTag(kDerivedTag) {}
};

struct DestructorProbe {
    ~DestructorProbe() { ++g_destructorRuns; }
};

struct CopyProbe {
    int tag;

    explicit CopyProbe(int value) : tag(value) {}
    CopyProbe(const CopyProbe& other) : tag(other.tag) { ++g_copyRuns; }
};

__declspec(noinline) static int catch_derived_by_value() {
    try {
        throw Derived{};
    } catch (Derived value) {
        return value.tag == kBaseTag && value.derivedTag == kDerivedTag ? 0 : 1;
    } catch (...) {
        return 2;
    }
}

__declspec(noinline) static int catch_base_by_reference() {
    try {
        throw Derived{};
    } catch (const Base& value) {
        return value.tag == kBaseTag ? 0 : 1;
    } catch (...) {
        return 2;
    }
}

__declspec(noinline) static int catch_all_second_exception() {
    try {
        throw 0x7654;
    } catch (...) {
        return 0;
    }
}

__declspec(noinline) static int nested_catch() {
    int caught = 0;
    try {
        try {
            throw Derived{};
        } catch (const Base& value) {
            caught = value.tag == kBaseTag ? 1 : 0;
        }
    } catch (...) {
        return 1;
    }
    return caught ? 0 : 2;
}

__declspec(noinline) static int destructor_during_unwind() {
    g_destructorRuns = 0;
    try {
        DestructorProbe probe;
        throw Derived{};
    } catch (const Base&) {
    } catch (...) {
        return 1;
    }
    return g_destructorRuns == 1 ? 0 : 2;
}

__declspec(noinline) static void throw_from_inner_frame() {
    DestructorProbe probe;
    throw Derived{};
}

__declspec(noinline) static int catch_from_outer_frame() {
    g_destructorRuns = 0;
    try {
        throw_from_inner_frame();
    } catch (const Base& value) {
        if (value.tag != kBaseTag) return 1;
    } catch (...) {
        return 2;
    }
    return g_destructorRuns == 1 ? 0 : 3;
}

__declspec(noinline) static int catch_nontrivial_by_value() {
    g_copyRuns = 0;
    try {
        throw CopyProbe(kDerivedTag);
    } catch (CopyProbe value) {
        if (value.tag != kDerivedTag) return 1;
        return g_copyRuns > 0 ? 0 : 2;
    } catch (...) {
        return 2;
    }
}

#if defined(_M_IX86)
__declspec(noinline) static unsigned caller_stack_pointer() {
    unsigned value;
    __asm {
        mov eax, ebp
        add eax, 8
        mov value, eax
    }
    return value;
}
#endif

int main() {
#if defined(_M_IX86)
    const unsigned mainStackPointer = caller_stack_pointer();
#define CHECK_MAIN_STACK(code) \
    do { \
        if (caller_stack_pointer() != mainStackPointer) return (code); \
    } while (0)
#endif
    if (catch_derived_by_value() != 0) return 10;
#if defined(_M_IX86)
    CHECK_MAIN_STACK(81);
#endif
    if (catch_base_by_reference() != 0) return 20;
#if defined(_M_IX86)
    CHECK_MAIN_STACK(82);
#endif
    if (catch_all_second_exception() != 0) return 30;
#if defined(_M_IX86)
    CHECK_MAIN_STACK(83);
#endif
    if (nested_catch() != 0) return 40;
#if defined(_M_IX86)
    CHECK_MAIN_STACK(84);
#endif
    if (destructor_during_unwind() != 0) return 50;
#if defined(_M_IX86)
    CHECK_MAIN_STACK(85);
#endif
    if (catch_from_outer_frame() != 0) return 60;
#if defined(_M_IX86)
    CHECK_MAIN_STACK(86);
#endif
    const int copyResult = catch_nontrivial_by_value();
    if (copyResult != 0) return 70 + copyResult;
#if defined(_M_IX86)
    CHECK_MAIN_STACK(87);
#undef CHECK_MAIN_STACK
#endif
    return 0;
}
