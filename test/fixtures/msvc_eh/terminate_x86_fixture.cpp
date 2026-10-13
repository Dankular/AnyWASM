// A destructor exception that escapes during stack unwinding must call std::terminate.
struct ThrowingDuringUnwind {
    ~ThrowingDuringUnwind() noexcept(false) {
        throw 0x12345678;
    }
};

#if defined(TERMINATE_UNHANDLED)
__declspec(noinline) static void throw_original_with_probe() {
    ThrowingDuringUnwind probe;
    throw 0x76543210LL;
}
#endif

int main() {
#if defined(TERMINATE_UNHANDLED)
    try {
        throw_original_with_probe();
    } catch (long long) {
        return 1;
    }
    return 2;
#else
    try {
        ThrowingDuringUnwind probe;
        throw 0x76543210;
    } catch (...) {
        return 1;
    }
    return 2;
#endif
}
