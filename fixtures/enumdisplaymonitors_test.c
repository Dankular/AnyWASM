/* Guest regression for EnumDisplayMonitors invoking a guest callback. */
typedef void *HANDLE;
typedef long BOOL;
typedef long long LPARAM;
typedef struct { long left, top, right, bottom; } RECT;
typedef BOOL (__stdcall *MONITORENUMPROC)(HANDLE, HANDLE, RECT *, LPARAM);

__declspec(dllimport) BOOL __stdcall EnumDisplayMonitors(
    HANDLE dc, const RECT *clip, MONITORENUMPROC callback, LPARAM data);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

static unsigned int calls;

static BOOL __stdcall monitor_callback(HANDLE monitor, HANDLE dc, RECT *rect, LPARAM data)
{
    ++calls;
    if ((unsigned long long)monitor != 0x5000 || dc != 0 || data != 0x1234
        || rect->left != 0 || rect->top != 0 || rect->right != 1920 || rect->bottom != 1080)
        return 0;
    return 1;
}

void mainCRTStartup(void)
{
    if (!EnumDisplayMonitors(0, 0, monitor_callback, 0x1234) || calls != 1)
        ExitProcess(1);
    RECT outside = {2000, 0, 2100, 100};
    if (!EnumDisplayMonitors(0, &outside, monitor_callback, 0x1234) || calls != 1)
        ExitProcess(2);
    ExitProcess(0);
}
