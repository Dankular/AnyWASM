/* Freestanding Win64 console program: no CRT, only kernel32. */
typedef void *HANDLE;
typedef unsigned long DWORD;
typedef int BOOL;
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);

static const char msg[] = "Hello from a Windows PE running inside AnyWASM!\r\n";

void mainCRTStartup(void) {
    DWORD written = 0;
    WriteFile(GetStdHandle((DWORD)-11), msg, sizeof msg - 1, &written, 0);
    ExitProcess(written == sizeof msg - 1 ? 0 : 3);
}
