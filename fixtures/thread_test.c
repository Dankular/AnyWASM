typedef unsigned long DWORD;
typedef unsigned long long SIZE_T;
typedef void *HANDLE;
typedef DWORD (__stdcall *ThreadProc)(void *);

__declspec(dllimport) HANDLE __stdcall CreateThread(void *, SIZE_T, ThreadProc, void *, DWORD, DWORD *);
__declspec(dllimport) DWORD __stdcall GetCurrentThreadId(void);
__declspec(dllimport) DWORD __stdcall GetThreadId(HANDLE);
__declspec(dllimport) int __stdcall GetExitCodeThread(HANDLE, DWORD *);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE, DWORD);
__declspec(dllimport) DWORD __stdcall ResumeThread(HANDLE);
__declspec(dllimport) int __stdcall CloseHandle(HANDLE);
__declspec(dllimport) int __stdcall SleepConditionVariableSRW(void *, void *, DWORD, DWORD);
__declspec(dllimport) DWORD __stdcall GetLastError(void);
__declspec(dllimport) void __stdcall AcquireSRWLockExclusive(void *);
__declspec(dllimport) void __stdcall ReleaseSRWLockExclusive(void *);
__declspec(dllimport) DWORD __stdcall TlsAlloc(void);
__declspec(dllimport) void *__stdcall TlsGetValue(DWORD);
__declspec(dllimport) int __stdcall TlsSetValue(DWORD, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int);
__declspec(dllimport) void __stdcall ExitThread(DWORD);

static DWORD tls_index;
static volatile DWORD worker_id;
static volatile DWORD worker_initial_tls;
static unsigned char condition_variable[8];
static unsigned long long lock;

static DWORD __stdcall worker(void *arg) {
    worker_id = GetCurrentThreadId();
    worker_initial_tls = (DWORD)(unsigned long long)TlsGetValue(tls_index);
    TlsSetValue(tls_index, arg);
    ExitThread((DWORD)(unsigned long long)arg + 5);
    return 0;
}

void mainCRTStartup(void) {
    DWORD thread_id = 0;
    DWORD exit_code = 0;
    DWORD main_id;
    HANDLE thread;
    tls_index = TlsAlloc();
    TlsSetValue(tls_index, (void *)0x1234);
    main_id = GetCurrentThreadId();
    thread = CreateThread(0, 0, worker, (void *)7, 0, &thread_id);
    if (!thread || !thread_id || thread_id != GetThreadId(thread)) ExitProcess(1);
    if (GetCurrentThreadId() != main_id) ExitProcess(2);
    if (WaitForSingleObject(thread, 0xffffffff) != 0) ExitProcess(4);
    if (worker_id != thread_id || worker_id == main_id) ExitProcess(2);
    if (worker_initial_tls != 0 || (DWORD)(unsigned long long)TlsGetValue(tls_index) != 0x1234) ExitProcess(3);
    if (!GetExitCodeThread(thread, &exit_code) || exit_code != 12) ExitProcess(5);
    AcquireSRWLockExclusive(&lock);
    if (SleepConditionVariableSRW(condition_variable, &lock, 0, 0) || GetLastError() != 1460) ExitProcess(6);
    ReleaseSRWLockExclusive(&lock);
    CloseHandle(thread);
    ExitProcess(0);
}
