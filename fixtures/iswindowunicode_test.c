/* Freestanding Win64 check that window character mode follows class registration. */
typedef void *HWND;
typedef void *HINSTANCE;
typedef unsigned int UINT;
typedef unsigned long DWORD;
typedef unsigned long long WPARAM;
typedef long long LPARAM;
typedef long long LRESULT;
typedef LRESULT (__stdcall *WNDPROC)(HWND, UINT, WPARAM, LPARAM);

typedef struct {
    UINT cbSize;
    UINT style;
    WNDPROC lpfnWndProc;
    int cbClsExtra;
    int cbWndExtra;
    HINSTANCE hInstance;
    void *hIcon;
    void *hCursor;
    void *hbrBackground;
    const void *lpszMenuName;
    const void *lpszClassName;
    void *hIconSm;
} WNDCLASSEX;

__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) HINSTANCE __stdcall GetModuleHandleW(const unsigned short *);
__declspec(dllimport) unsigned short __stdcall RegisterClassExA(const WNDCLASSEX *);
__declspec(dllimport) unsigned short __stdcall RegisterClassExW(const WNDCLASSEX *);
__declspec(dllimport) HWND __stdcall CreateWindowExA(DWORD, const char *, const char *, DWORD,
    int, int, int, int, HWND, void *, HINSTANCE, void *);
__declspec(dllimport) HWND __stdcall CreateWindowExW(DWORD, const unsigned short *, const unsigned short *, DWORD,
    int, int, int, int, HWND, void *, HINSTANCE, void *);
__declspec(dllimport) int __stdcall IsWindowUnicode(HWND);

static LRESULT __stdcall WindowProc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam) {
    (void)hwnd;
    (void)wparam;
    (void)lparam;
    return message == 0x0081 ? 1 : 0; /* WM_NCCREATE */
}

static const char ansi_name[] = "AnsiWindowProbe";
static const unsigned short ansi_name_w[] = {
    'A', 'n', 's', 'i', 'W', 'i', 'n', 'd', 'o', 'w', 'P', 'r', 'o', 'b', 'e', 0
};
static const char wide_name[] = "WideWindowProbe";
static const unsigned short wide_name_w[] = {
    'W', 'i', 'd', 'e', 'W', 'i', 'n', 'd', 'o', 'w', 'P', 'r', 'o', 'b', 'e', 0
};
static const unsigned short static_name_w[] = { 'S', 'T', 'A', 'T', 'I', 'C', 0 };
static const unsigned short title_w[] = { 'p', 'r', 'o', 'b', 'e', 0 };

void mainCRTStartup(void) {
    HINSTANCE instance = GetModuleHandleW(0);
    if (IsWindowUnicode(0))
        ExitProcess(1);

    WNDCLASSEX ansi_class = {
        sizeof(WNDCLASSEX), 0, WindowProc, 0, 0, instance, 0, 0, 0, 0, ansi_name, 0
    };
    if (!RegisterClassExA(&ansi_class))
        ExitProcess(2);
    HWND ansi_window = CreateWindowExW(0, ansi_name_w, title_w, 0,
        0, 0, 64, 64, 0, 0, instance, 0);
    if (!ansi_window)
        ExitProcess(3);
    if (IsWindowUnicode(ansi_window))
        ExitProcess(4);

    WNDCLASSEX wide_class = {
        sizeof(WNDCLASSEX), 0, WindowProc, 0, 0, instance, 0, 0, 0, 0, wide_name_w, 0
    };
    if (!RegisterClassExW(&wide_class))
        ExitProcess(5);
    HWND wide_window = CreateWindowExA(0, wide_name, "probe", 0,
        0, 0, 64, 64, 0, 0, instance, 0);
    if (!wide_window)
        ExitProcess(6);
    if (!IsWindowUnicode(wide_window))
        ExitProcess(7);

    HWND builtin_window = CreateWindowExW(0, static_name_w, title_w, 0,
        0, 0, 64, 64, 0, 0, instance, 0);
    if (!builtin_window || !IsWindowUnicode(builtin_window))
        ExitProcess(8);

    ExitProcess(0);
}
