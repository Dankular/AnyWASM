typedef unsigned int DWORD;
typedef void *HANDLE;
typedef DWORD (__stdcall *ThreadProc)(void *);

__declspec(dllimport) HANDLE __stdcall CreateThread(void *, unsigned long long, ThreadProc, void *, DWORD, DWORD *);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE, DWORD);
__declspec(dllimport) int __stdcall GetExitCodeThread(HANDLE, DWORD *);
__declspec(dllimport) int __stdcall CloseHandle(HANDLE);
__declspec(dllimport) void __stdcall AcquireSRWLockExclusive(void *);
__declspec(dllimport) void __stdcall ReleaseSRWLockExclusive(void *);
__declspec(dllimport) int __stdcall SleepConditionVariableSRW(void *, void *, DWORD, DWORD);
__declspec(dllimport) void __stdcall WakeConditionVariable(void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int);
__declspec(dllimport) void __stdcall ExitThread(DWORD);

static unsigned long long lock;
static unsigned char condition_variable[8];
static volatile DWORD ready;
static volatile DWORD wake_result;

static DWORD __stdcall waiter(void *unused) {
    (void)unused;
    AcquireSRWLockExclusive(&lock);
    if (!SleepConditionVariableSRW(condition_variable, &lock, 0xffffffffu, 0)) {
        ExitThread(10);
    }
    wake_result = ready;
    ReleaseSRWLockExclusive(&lock);
    ExitThread(wake_result ? 11 : 12);
    return 0;
}

static DWORD __stdcall producer(void *unused) {
    (void)unused;
    AcquireSRWLockExclusive(&lock);
    ready = 1;
    WakeConditionVariable(condition_variable);
    ReleaseSRWLockExclusive(&lock);
    ExitThread(13);
    return 0;
}

void mainCRTStartup(void) {
    HANDLE waiting = CreateThread(0, 0, waiter, 0, 0, 0);
    if (!waiting) ExitProcess(1);
    HANDLE signaling = CreateThread(0, 0, producer, 0, 0, 0);
    if (!signaling) ExitProcess(2);
    if (WaitForSingleObject(waiting, 0xffffffffu) != 0) ExitProcess(3);
    if (WaitForSingleObject(signaling, 0xffffffffu) != 0) ExitProcess(4);
    DWORD exit_code;
    if (!GetExitCodeThread(waiting, &exit_code) || exit_code != 11 || wake_result != 1) ExitProcess(5);
    if (!GetExitCodeThread(signaling, &exit_code) || exit_code != 13) ExitProcess(6);
    CloseHandle(waiting);
    CloseHandle(signaling);
    ExitProcess(0);
}
