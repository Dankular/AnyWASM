/* Freestanding Win64 check for child-window IDs and USER32!GetDlgItem. */
typedef void *HWND;
typedef void *HINSTANCE;
typedef unsigned long DWORD;
typedef unsigned long long UINT_PTR;

__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) HINSTANCE __stdcall GetModuleHandleW(const unsigned short *);
__declspec(dllimport) HWND __stdcall CreateWindowExA(DWORD, const char *, const char *, DWORD,
    int, int, int, int, HWND, void *, HINSTANCE, void *);
__declspec(dllimport) HWND __stdcall GetDlgItem(HWND, int);

enum { WS_CHILD = 0x40000000u };

void mainCRTStartup(void) {
    HINSTANCE instance = GetModuleHandleW(0);
    HWND parent = CreateWindowExA(0, "STATIC", "parent", 0,
        0, 0, 100, 100, 0, 0, instance, 0);
    if (!parent)
        ExitProcess(1);

    HWND first = CreateWindowExA(0, "STATIC", "first", WS_CHILD,
        0, 0, 20, 20, parent, (void *)(UINT_PTR)42, instance, 0);
    HWND second = CreateWindowExA(0, "STATIC", "second", WS_CHILD,
        20, 0, 20, 20, parent, (void *)(UINT_PTR)7, instance, 0);
    if (!first || !second)
        ExitProcess(2);

    if (GetDlgItem(parent, 42) != first)
        ExitProcess(3);
    if (GetDlgItem(parent, 7) != second)
        ExitProcess(4);
    if (GetDlgItem(parent, 99) != 0)
        ExitProcess(5);
    if (GetDlgItem(0, 42) != 0)
        ExitProcess(6);
    if (GetDlgItem(first, 42) != 0)
        ExitProcess(7);

    ExitProcess(0);
}
