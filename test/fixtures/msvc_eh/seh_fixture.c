#include <windows.h>

static volatile LONG g_finally_runs;
static const DWORD kCaughtCode = 0xE0424242;
static const DWORD kSearchCode = 0xE0424245;
static const DWORD kContinueCode = 0xE0424244;
static const DWORD kUnwindCode = 0xE0424343;

static void raise_through_finally(void) {
    __try {
        RaiseException(kUnwindCode, 0, 0, NULL);
    } __finally {
        ++g_finally_runs;
    }
}

int main(void) {
    volatile LONG caught_code = 0;
    g_finally_runs = 0;

    __try {
        __try {
            RaiseException(kCaughtCode, 0, 0, NULL);
        } __except (EXCEPTION_EXECUTE_HANDLER) {
            caught_code = (LONG)GetExceptionCode();
        }
    } __finally {
        ++g_finally_runs;
    }

    if (caught_code != (LONG)kCaughtCode || g_finally_runs != 1) return 1;

    caught_code = 0;
    __try {
        __try {
            RaiseException(kSearchCode, 0, 0, NULL);
        } __except (EXCEPTION_CONTINUE_SEARCH) {
            return 2;
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        caught_code = (LONG)GetExceptionCode();
    }
    if (caught_code != (LONG)kSearchCode) return 3;

    volatile LONG continued = 0;
    __try {
        RaiseException(kContinueCode, 0, 0, NULL);
        continued = 1;
    } __except (EXCEPTION_CONTINUE_EXECUTION) {
        return 4;
    }
    if (continued != 1) return 5;

    caught_code = 0;
    g_finally_runs = 0;
    __try {
        raise_through_finally();
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        caught_code = (LONG)GetExceptionCode();
    }

    return caught_code == (LONG)kUnwindCode && g_finally_runs == 1 ? 0 : 6;
}
