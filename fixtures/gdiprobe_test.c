/* Guest regression for virtual display DC creation and GetDeviceCaps. */
typedef void *HDC;
typedef unsigned short WCHAR;

__declspec(dllimport) HDC __stdcall CreateDCW(
    const WCHAR *driver, const WCHAR *device, const WCHAR *output, const void *init_data);
__declspec(dllimport) int __stdcall DeleteDC(HDC dc);
__declspec(dllimport) int __stdcall GetDeviceCaps(HDC dc, int index);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

static const WCHAR display_driver[] = { 'D','I','S','P','L','A','Y',0 };
static const WCHAR display_device[] = { '\\','\\','.','\\','D','I','S','P','L','A','Y','1',0 };

void mainCRTStartup(void)
{
    HDC dc = CreateDCW(display_driver, display_device, 0, 0);
    if (!dc)
        ExitProcess(1);
    if (GetDeviceCaps(dc, 8) != 1920 || GetDeviceCaps(dc, 10) != 1080
        || GetDeviceCaps(dc, 12) != 32 || GetDeviceCaps(dc, 88) != 96)
        ExitProcess(2);
    if (!DeleteDC(dc) || DeleteDC(dc))
        ExitProcess(3);
    ExitProcess(0);
}
