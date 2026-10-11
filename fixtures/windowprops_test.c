/* Guest regression for per-window USER32 property values. */
typedef void *HWND;
typedef void *HINSTANCE;
typedef void *HANDLE;
typedef unsigned int DWORD;
typedef unsigned short WCHAR;

__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);
__declspec(dllimport) HINSTANCE __stdcall GetModuleHandleW(const WCHAR *name);
__declspec(dllimport) HWND __stdcall CreateWindowExW(DWORD ex_style, const WCHAR *class_name,
    const WCHAR *title, DWORD style, int x, int y, int width, int height,
    HWND parent, void *menu, HINSTANCE instance, void *param);
__declspec(dllimport) int __stdcall SetPropW(HWND window, const WCHAR *name, HANDLE value);
__declspec(dllimport) HANDLE __stdcall GetPropW(HWND window, const WCHAR *name);
__declspec(dllimport) HANDLE __stdcall RemovePropW(HWND window, const WCHAR *name);

static const WCHAR class_name[] = { 'S','T','A','T','I','C',0 };
static const WCHAR title[] = { 'p','r','o','p','e','r','t','y',0 };
static const WCHAR property_name[] = { 'A','n','y','W','A','S','M','.','P','r','o','p',0 };

void mainCRTStartup(void)
{
    HWND window = CreateWindowExW(0, class_name, title, 0,
        0, 0, 64, 64, 0, 0, GetModuleHandleW(0), 0);
    if (!window)
        ExitProcess(1);
    if (!SetPropW(window, property_name, (HANDLE)0x12345678))
        ExitProcess(2);
    if (GetPropW(window, property_name) != (HANDLE)0x12345678)
        ExitProcess(3);
    if (RemovePropW(window, property_name) != (HANDLE)0x12345678
        || GetPropW(window, property_name) != 0)
        ExitProcess(4);
    ExitProcess(0);
}
