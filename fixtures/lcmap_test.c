/* Guest regression for Win32 string conversion int arguments and count-only queries. */
typedef unsigned long DWORD;
typedef unsigned short WCHAR;
typedef long long LPARAM;

__declspec(dllimport) int __stdcall LCMapStringEx(
    const WCHAR *locale_name, DWORD flags, const WCHAR *source, int source_chars,
    WCHAR *destination, int destination_chars, void *version, void *reserved,
    LPARAM sort_handle);
__declspec(dllimport) int __stdcall WideCharToMultiByte(
    unsigned int code_page, DWORD flags, const WCHAR *source, int source_chars,
    char *destination, int destination_bytes, const char *default_char,
    int *used_default_char);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    static const WCHAR source[] = {'a', 'b', 'c', 0};
    WCHAR destination[4] = {0, 0, 0, 0};
    char bytes[4] = {0, 0, 0, 0};
    int result;

    result = LCMapStringEx((const WCHAR *)0, 0x200, source, -1,
                           (WCHAR *)0, 0, (void *)0, (void *)0, 0);
    if (result != 4)
        ExitProcess(10);

    result = LCMapStringEx((const WCHAR *)0, 0x200, source, -1,
                           destination, 4, (void *)0, (void *)0, 0);
    if (result != 4 || destination[0] != 'A' || destination[1] != 'B' ||
        destination[2] != 'C' || destination[3] != 0)
        ExitProcess(11);

    result = WideCharToMultiByte(1252, 0, source, -1, (char *)0, 0,
                                 (const char *)0, (int *)0);
    if (result != 4)
        ExitProcess(12);

    result = WideCharToMultiByte(1252, 0, source, -1, bytes, 4,
                                 (const char *)0, (int *)0);
    if (result != 4 || bytes[0] != 'a' || bytes[1] != 'b' ||
        bytes[2] != 'c' || bytes[3] != 0)
        ExitProcess(13);

    ExitProcess(0);
}
