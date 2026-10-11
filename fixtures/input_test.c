/* Freestanding Win64 GUI fixture: exit only after the guest sees browser input. */
typedef void *HANDLE;
typedef void *HWND;
typedef unsigned int UINT;
typedef unsigned long DWORD;
typedef unsigned long long WPARAM;
typedef unsigned long long ULONG_PTR;
typedef long long LPARAM;
typedef long long LRESULT;
typedef int BOOL;

typedef struct {
    DWORD cbSize;
    DWORD dwFlags;
    HWND hwndTrack;
    DWORD dwHoverTime;
} TRACKMOUSEEVENT;

typedef LRESULT (__stdcall *WNDPROC)(HWND, UINT, WPARAM, LPARAM);

typedef struct {
    UINT cbSize;
    UINT style;
    WNDPROC lpfnWndProc;
    int cbClsExtra;
    int cbWndExtra;
    void *hInstance;
    void *hIcon;
    void *hCursor;
    void *hbrBackground;
    const char *lpszMenuName;
    const char *lpszClassName;
    void *hIconSm;
} WNDCLASSEXA;

typedef struct {
    HWND hwnd;
    UINT message;
    UINT padding;
    WPARAM wParam;
    LPARAM lParam;
    DWORD time;
    struct { long x, y; } pt;
} MSG;

__declspec(dllimport) unsigned short __stdcall RegisterClassExA(const WNDCLASSEXA *);
__declspec(dllimport) HWND __stdcall CreateWindowExA(unsigned long, const char *, const char *, unsigned long,
    int, int, int, int, HWND, void *, void *, void *);
__declspec(dllimport) int __stdcall ShowWindow(HWND, int);
__declspec(dllimport) int __stdcall UpdateWindow(HWND);
__declspec(dllimport) int __stdcall GetMessageA(MSG *, HWND, UINT, UINT);
__declspec(dllimport) int __stdcall TranslateMessage(const MSG *);
__declspec(dllimport) LRESULT __stdcall DispatchMessageA(const MSG *);
__declspec(dllimport) void __stdcall PostQuitMessage(int);
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) BOOL __stdcall TrackMouseEvent(TRACKMOUSEEVENT *);

enum { WM_NCCREATE = 0x0081, WM_KEYDOWN = 0x0100, WM_MOUSEMOVE = 0x0200, WM_LBUTTONDOWN = 0x0201, WM_MOUSELEAVE = 0x02a3 };

static int saw_a_key;
static int saw_scaled_mouse;
static int saw_mouse_tracking;
static int saw_left_down;
static int saw_mouse_leave;
static int reported;

static void write_text(const char *text, DWORD length) {
    DWORD written = 0;
    WriteFile(GetStdHandle((DWORD)-11), text, length, &written, 0);
}

static void report_if_complete(void) {
    if (saw_a_key && saw_scaled_mouse && saw_mouse_tracking && saw_left_down && saw_mouse_leave && !reported) {
        static const char result[] = "guest input complete\r\n";
        reported = 1;
        write_text(result, sizeof result - 1);
        PostQuitMessage(0);
    }
}

static LRESULT __stdcall WindowProc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_NCCREATE)
        return 1;
    if (message == WM_KEYDOWN && wparam == 'A') {
        static const char key[] = "guest saw keydown A\r\n";
        saw_a_key = 1;
        write_text(key, sizeof key - 1);
    }
    if (message == WM_MOUSEMOVE) {
        const ULONG_PTR packed = (ULONG_PTR)lparam;
        if ((packed & 0xffff) == 520 && ((packed >> 16) & 0xffff) == 404) {
            static const char mouse_move[] = "guest saw scaled mousemove (520,404)\r\n";
            static const char tracking[] = "guest TrackMouseEvent(TME_LEAVE) succeeded\r\n";
            TRACKMOUSEEVENT request = { sizeof(TRACKMOUSEEVENT), 2, hwnd, 0 };
            saw_scaled_mouse = 1;
            write_text(mouse_move, sizeof mouse_move - 1);
            if (TrackMouseEvent(&request)) {
                saw_mouse_tracking = 1;
                write_text(tracking, sizeof tracking - 1);
            }
        }
    }
    if (message == WM_MOUSELEAVE) {
        static const char leave[] = "guest saw WM_MOUSELEAVE\r\n";
        saw_mouse_leave = 1;
        write_text(leave, sizeof leave - 1);
    }
    if (message == WM_LBUTTONDOWN) {
        static const char mouse[] = "guest saw left-down\r\n";
        saw_left_down = 1;
        write_text(mouse, sizeof mouse - 1);
    }
    report_if_complete();
    (void)hwnd;
    return 0;
}

static const char class_name[] = "AnyWasmInputTest";

void mainCRTStartup(void) {
    static const char ready[] = "guest input ready\r\n";
    WNDCLASSEXA wc = { sizeof(WNDCLASSEXA), 0, WindowProc, 0, 0, 0, 0, 0, 0, 0, class_name, 0 };
    MSG msg;
    DWORD written = 0;
    if (!RegisterClassExA(&wc))
        ExitProcess(2);
    HWND window = CreateWindowExA(0, class_name, "input test", 0x00cf0000, 0, 0, 1040, 807, 0, 0, 0, 0);
    if (!window)
        ExitProcess(2);
    ShowWindow(window, 1);
    UpdateWindow(window);
    if (!WriteFile(GetStdHandle((DWORD)-11), ready, sizeof ready - 1, &written, 0) || written != sizeof ready - 1)
        ExitProcess(4);
    while (GetMessageA(&msg, 0, 0, 0) > 0) {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    ExitProcess(0);
}
