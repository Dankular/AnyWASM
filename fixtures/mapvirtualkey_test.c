/* Guest regression for common Win32 US-layout MapVirtualKeyW modes. */
typedef unsigned int UINT;

__declspec(dllimport) UINT __stdcall MapVirtualKeyW(UINT code, UINT map_type);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    if (MapVirtualKeyW(0x41, 0) != 0x1e) /* VK_A -> scan code */
        ExitProcess(10);
    if (MapVirtualKeyW(0x1e, 1) != 0x41) /* scan code -> VK_A */
        ExitProcess(11);
    if (MapVirtualKeyW(0xe04b, 3) != 0x25) /* extended left arrow scan -> VK_LEFT */
        ExitProcess(12);
    if (MapVirtualKeyW(0x41, 2) != 0x41) /* VK_A -> uppercase character */
        ExitProcess(13);
    if (MapVirtualKeyW(0xa3, 4) != 0xe01d) /* VK_RCONTROL -> extended scan code */
        ExitProcess(14);
    if (MapVirtualKeyW(0xff, 0) != 0) /* no mapping */
        ExitProcess(15);
    ExitProcess(0);
}
