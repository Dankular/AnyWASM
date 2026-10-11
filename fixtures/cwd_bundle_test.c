/* Verify that an .awb manifest sets the process current directory. */
typedef unsigned long DWORD;
typedef unsigned short WCHAR;

__declspec(dllimport) void __stdcall ExitProcess(unsigned int);
__declspec(dllimport) DWORD __stdcall GetCurrentDirectoryW(DWORD, WCHAR *);
__declspec(dllimport) DWORD __stdcall GetFileAttributesW(const WCHAR *);

static const WCHAR expected[] = {'C', ':', '\\', 'g', 'a', 'm', 'e', '\\', 'a', 's', 's', 'e', 't', 's', 0};
static const WCHAR relative_file[] = {'c', 'o', 'n', 'f', 'i', 'g', '.', 't', 'x', 't', 0};

void mainCRTStartup(void) {
    WCHAR cwd[32] = {0};
    if (GetCurrentDirectoryW(32, cwd) != 14) ExitProcess(1);
    for (unsigned int i = 0; i < 15; ++i) {
        if (cwd[i] != expected[i]) ExitProcess(2);
    }
    if (GetFileAttributesW(relative_file) != 0x80) ExitProcess(3);
    ExitProcess(0);
}
