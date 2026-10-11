/* Guest regression for Shell32 drag/drop registration. */
typedef void *HWND;
typedef void *HINSTANCE;
typedef unsigned int DWORD;
typedef unsigned short WCHAR;

__declspec(dllimport) HINSTANCE __stdcall GetModuleHandleW(const WCHAR *name);
__declspec(dllimport) HWND __stdcall CreateWindowExW(DWORD ex_style, const WCHAR *class_name,
    const WCHAR *title, DWORD style, int x, int y, int width, int height,
    HWND parent, void *menu, HINSTANCE instance, void *param);
__declspec(dllimport) void __stdcall DragAcceptFiles(HWND window, int accept);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

static const WCHAR class_name[] = { 'S','T','A','T','I','C',0 };
static const WCHAR title[] = { 'd','r','o','p',0 };

void mainCRTStartup(void)
{
    HWND window = CreateWindowExW(0, class_name, title, 0,
        0, 0, 32, 32, 0, 0, GetModuleHandleW(0), 0);
    if (!window)
        ExitProcess(1);
    DragAcceptFiles(window, 1);
    DragAcceptFiles(window, 0);
    ExitProcess(0);
}
