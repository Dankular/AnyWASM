typedef unsigned int DWORD;
typedef int BOOL;

__declspec(dllimport) BOOL __stdcall PathFileExistsW(const unsigned short *path);
__declspec(dllimport) DWORD __stdcall GetLastError(void);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

static const unsigned short present[] = {
    'C', ':', '\\', 'g', 'a', 'm', 'e', '\\', 'd', 'a', 't', 'a', '\\',
    'p', 'r', 'e', 's', 'e', 'n', 't', '.', 't', 'x', 't', 0
};
static const unsigned short directory[] = {'d', 'a', 't', 'a', 0};
static const unsigned short missing[] = {'d', 'a', 't', 'a', '\\', 'm', 'i', 's', 's', 'i', 'n', 'g', 0};

void mainCRTStartup(void)
{
    if (!PathFileExistsW(present))
        ExitProcess(1);
    if (!PathFileExistsW(directory))
        ExitProcess(2);
    if (PathFileExistsW(missing) || GetLastError() != 2)
        ExitProcess(3);
    ExitProcess(0);
}
