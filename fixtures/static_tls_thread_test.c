typedef unsigned long DWORD;
typedef void *HANDLE;
typedef DWORD (__stdcall *ThreadStart)(void *);

__declspec(thread) volatile DWORD thread_value = 7;

__declspec(dllimport) HANDLE __stdcall CreateThread(
    void *attributes, DWORD stack_size, ThreadStart start, void *parameter,
    DWORD creation_flags, DWORD *thread_id);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE handle, DWORD milliseconds);
__declspec(dllimport) int __stdcall GetExitCodeThread(HANDLE handle, DWORD *exit_code);
__declspec(dllimport) int __stdcall CloseHandle(HANDLE handle);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

static DWORD __stdcall worker(void *parameter)
{
    (void)parameter;
    if (thread_value != 7)
        return 1;
    thread_value = 0x2222;
    return 0;
}

void mainCRTStartup(void)
{
    if (thread_value != 7)
        ExitProcess(10);
    thread_value = 0x1111;

    HANDLE thread = CreateThread(0, 0, worker, 0, 0, 0);
    if (!thread)
        ExitProcess(11);
    if (WaitForSingleObject(thread, 0xffffffffu) != 0)
        ExitProcess(12);

    DWORD exit_code = 0xffffffffu;
    if (!GetExitCodeThread(thread, &exit_code) || exit_code != 0)
        ExitProcess(13);
    CloseHandle(thread);
    if (thread_value != 0x1111)
        ExitProcess(14);
    ExitProcess(0);
}
