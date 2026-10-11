/* Guest regression for the virtual adapter and monitor returned by EnumDisplayDevicesW. */
typedef unsigned int DWORD;
typedef unsigned short WCHAR;

typedef struct {
    DWORD cb;
    WCHAR device_name[32];
    WCHAR device_string[128];
    DWORD state_flags;
    WCHAR device_id[128];
    WCHAR device_key[128];
} DISPLAY_DEVICEW;

__declspec(dllimport) int __stdcall EnumDisplayDevicesW(
    const WCHAR *device, DWORD index, DISPLAY_DEVICEW *out, DWORD flags);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    DISPLAY_DEVICEW display;
    display.cb = sizeof(display);
    if (!EnumDisplayDevicesW(0, 0, &display, 0))
        ExitProcess(1);
    if (display.cb != sizeof(display) || display.device_name[0] != '\\'
        || (display.state_flags & 5) != 5)
        ExitProcess(2);

    if (!EnumDisplayDevicesW(display.device_name, 0, &display, 1))
        ExitProcess(3);
    if (EnumDisplayDevicesW(0, 1, &display, 0))
        ExitProcess(4);
    ExitProcess(0);
}
