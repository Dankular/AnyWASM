#include <intrin.h>

typedef unsigned long DWORD;
typedef void *HANDLE;
typedef long LONG;

typedef struct ExceptionRecordPrefix {
    DWORD code;
} ExceptionRecordPrefix;

typedef struct ExceptionPointers {
    ExceptionRecordPrefix *record;
    void *context;
} ExceptionPointers;

typedef LONG (__stdcall *VectoredHandler)(ExceptionPointers *);

__declspec(dllimport) HANDLE __stdcall AddVectoredExceptionHandler(DWORD first, VectoredHandler handler);
__declspec(dllimport) DWORD __stdcall RemoveVectoredExceptionHandler(HANDLE handler);
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD which);
__declspec(dllimport) int __stdcall WriteFile(HANDLE file, const void *buffer, DWORD length, DWORD *written, void *overlapped);
__declspec(dllimport) void __stdcall ExitProcess(unsigned code);

static volatile DWORD handled_breakpoints;

static LONG __stdcall handle_breakpoint(ExceptionPointers *exception) {
    if (exception && exception->record && exception->record->code == 0x80000003u) {
        handled_breakpoints++;
        return -1; /* EXCEPTION_CONTINUE_EXECUTION */
    }
    return 0; /* EXCEPTION_CONTINUE_SEARCH */
}

void mainCRTStartup(void) {
    HANDLE registration = AddVectoredExceptionHandler(1, handle_breakpoint);
    if (!registration) ExitProcess(1);

    __debugbreak();

    if (handled_breakpoints != 1) ExitProcess(2);
    if (!RemoveVectoredExceptionHandler(registration)) ExitProcess(4);
    static const char message[] = "fault handled\r\n";
    DWORD written = 0;
    if (!WriteFile(GetStdHandle((DWORD)-11), message, sizeof(message) - 1, &written, 0) ||
        written != sizeof(message) - 1) ExitProcess(3);
    ExitProcess(0);
}
