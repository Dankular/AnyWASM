/* Guest regression for the virtual display's current mode and enumerated modes. */
typedef unsigned int DWORD;
typedef unsigned short WORD;

__declspec(dllimport) int __stdcall EnumDisplaySettingsW(
    const unsigned short *device, DWORD mode_num, void *devmode);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

static DWORD read_dword(const unsigned char *bytes, unsigned int offset)
{
    return *(const DWORD *)(bytes + offset);
}

void mainCRTStartup(void)
{
    unsigned char mode[256];
    *(WORD *)(mode + 68) = 220;
    if (!EnumDisplaySettingsW(0, (DWORD)-1, mode))
        ExitProcess(1);
    if (*(WORD *)(mode + 68) != 220 || read_dword(mode, 168) != 32
        || read_dword(mode, 172) != 1920 || read_dword(mode, 176) != 1080
        || read_dword(mode, 184) != 60)
        ExitProcess(2);

    if (!EnumDisplaySettingsW(0, 1, mode)
        || read_dword(mode, 172) != 1280 || read_dword(mode, 176) != 720)
        ExitProcess(3);
    if (EnumDisplaySettingsW(0, 3, mode))
        ExitProcess(4);
    ExitProcess(0);
}
