/* Freestanding Win64 guest check for USER32!GetCapture's HWND return value. */
typedef void *HWND;
typedef unsigned long DWORD;

__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) HWND __stdcall CreateWindowExA(DWORD, const char *, const char *, DWORD,
    int, int, int, int, HWND, void *, void *, void *);
__declspec(dllimport) int __stdcall ShowWindow(HWND, int);
__declspec(dllimport) HWND __stdcall GetCapture(void);
__declspec(dllimport) HWND __stdcall SetCapture(HWND);
__declspec(dllimport) int __stdcall ReleaseCapture(void);

void mainCRTStartup(void) {
    if (GetCapture() != 0)
        ExitProcess(1);

    HWND window = CreateWindowExA(0, "STATIC", "capture test", 0,
        0, 0, 64, 64, 0, 0, 0, 0);
    if (window == 0)
        ExitProcess(2);
    ShowWindow(window, 1);

    if (SetCapture(window) != 0)
        ExitProcess(3);
    if (GetCapture() != window)
        ExitProcess(4);
    if (SetCapture(window) != window)
        ExitProcess(5);
    if (GetCapture() != window)
        ExitProcess(6);
    if (!ReleaseCapture())
        ExitProcess(7);
    if (GetCapture() != 0)
        ExitProcess(8);

    ExitProcess(0);
}
