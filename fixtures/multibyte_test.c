/* Guest regression for Win32 MultiByteToWideChar buffer sizing and errors. */
typedef unsigned long DWORD;
typedef unsigned short WCHAR;

__declspec(dllimport) int __stdcall MultiByteToWideChar(
    unsigned int code_page, DWORD flags, const char *source, int source_bytes,
    WCHAR *destination, int destination_chars);
__declspec(dllimport) DWORD __stdcall GetLastError(void);
__declspec(dllimport) void __stdcall SetLastError(DWORD error);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    char source[256];
    WCHAR destination[256];
    int result;
    unsigned int i;

    for (i = 0; i < 255; ++i)
        source[i] = 'a';
    source[255] = '\0';

    /* An explicit byte count includes the terminator: 256 UTF-16 chars. */
    result = MultiByteToWideChar(1252, 1, source, 256, (WCHAR *)0, 0);
    if (result != 256)
        ExitProcess(10);

    /* A NULL output with nonzero capacity is an invalid parameter on Windows. */
    SetLastError(0);
    result = MultiByteToWideChar(1252, 1, source, 256, (WCHAR *)0, 256);
    if (result != 0)
        ExitProcess(21);
    if (GetLastError() != 87)
        ExitProcess(GetLastError());

    /* A non-NULL but undersized buffer must take the same error path. */
    SetLastError(0);
    result = MultiByteToWideChar(1252, 1, source, 256, destination, 255);
    if (result != 0 || GetLastError() != 122)
        ExitProcess(12);

    SetLastError(0);
    result = MultiByteToWideChar(1252, 1, source, 256, destination, -1);
    if (result != 0 || GetLastError() != 87)
        ExitProcess(13);

    SetLastError(0);
    result = MultiByteToWideChar(1252, 1, source, 0, destination, 256);
    if (result != 0 || GetLastError() != 87)
        ExitProcess(14);

    /* cbMultiByte=-1 reads through the NUL and includes it in the result. */
    result = MultiByteToWideChar(1252, 1, source, -1, destination, 256);
    if (result != 256 || destination[0] != 'a' || destination[255] != 0)
        ExitProcess(15);

    ExitProcess(0);
}
