/* Guest regression for process current-directory and relative VFS paths. */
typedef unsigned long DWORD;
typedef unsigned short WCHAR;
typedef unsigned long long size_t;
typedef struct _FILE FILE;

__declspec(dllimport) void __stdcall ExitProcess(unsigned int);
__declspec(dllimport) int __stdcall SetCurrentDirectoryA(const char *);
__declspec(dllimport) int __stdcall SetCurrentDirectoryW(const WCHAR *);
__declspec(dllimport) DWORD __stdcall GetCurrentDirectoryA(DWORD, char *);
__declspec(dllimport) DWORD __stdcall GetCurrentDirectoryW(DWORD, WCHAR *);
__declspec(dllimport) DWORD __stdcall GetFullPathNameA(const char *, DWORD, char *, char **);
__declspec(dllimport) void *__stdcall CreateFileW(const WCHAR *, DWORD, DWORD, void *, DWORD, DWORD, void *);
__declspec(dllimport) int __stdcall CloseHandle(void *);
__declspec(dllimport) DWORD __stdcall GetFileAttributesW(const WCHAR *);
__declspec(dllimport) unsigned long __cdecl _wfopen_s(FILE **, const WCHAR *, const WCHAR *);
__declspec(dllimport) size_t __cdecl fread(void *, size_t, size_t, FILE *);
__declspec(dllimport) int __cdecl fclose(FILE *);

static const WCHAR workdir[] = {'C', ':', '\\', 'g', 'a', 'm', 'e', '\\', 'a', 's', 's', 'e', 't', 's', 0};
static const WCHAR relative_file[] = {'c', 'o', 'n', 'f', 'i', 'g', '.', 't', 'x', 't', 0};
static const WCHAR read_mode[] = {'r', 'b', 0};

static int same(const char *a, const char *b) {
    while (*a && *a == *b) { ++a; ++b; }
    return *a == *b;
}

void mainCRTStartup(void) {
    WCHAR wide_cwd[32] = {0};
    char cwd[32] = {0};
    char full[64] = {0};
    char contents[6] = {0};
    FILE *stream = 0;

    if (GetCurrentDirectoryA(4, cwd) != 3 || !same(cwd, "C:\\")) ExitProcess(17);
    if (GetCurrentDirectoryW(4, wide_cwd) != 3 || wide_cwd[0] != 'C' || wide_cwd[1] != ':' || wide_cwd[2] != '\\' || wide_cwd[3] != 0) ExitProcess(18);
    if (!SetCurrentDirectoryW(workdir)) ExitProcess(1);
    if (GetCurrentDirectoryA(32, cwd) != 14 || !same(cwd, "C:\\game\\assets")) ExitProcess(2);
    if (GetCurrentDirectoryW(32, wide_cwd) != 14 || wide_cwd[0] != 'C' || wide_cwd[13] != 's' || wide_cwd[14] != 0) ExitProcess(3);
    if (GetCurrentDirectoryA(4, cwd) != 15) ExitProcess(4); /* required size includes NUL */
    if (GetFullPathNameA("..\\config.txt", 64, full, 0) != 18 || !same(full, "C:\\game\\config.txt")) ExitProcess(5);
    if (GetFileAttributesW(relative_file) != 0x80) ExitProcess(6);

    void *file = CreateFileW(relative_file, 0x80000000, 1, 0, 3, 0, 0);
    if (file == (void *)-1) ExitProcess(7);
    if (!CloseHandle(file)) ExitProcess(8);

    if (_wfopen_s(&stream, relative_file, read_mode) != 0 || !stream) ExitProcess(9);
    if (fread(contents, 1, 5, stream) != 5) ExitProcess(10);
    if (!same(contents, "vfs!!")) ExitProcess(11);
    if (fclose(stream) != 0) ExitProcess(12);

    if (SetCurrentDirectoryA("C:\\game\\missing")) ExitProcess(13);
    if (GetCurrentDirectoryA(32, cwd) != 14 || !same(cwd, "C:\\game\\assets")) ExitProcess(14);
    if (!SetCurrentDirectoryA("..")) ExitProcess(15);
    if (GetCurrentDirectoryA(32, cwd) != 7 || !same(cwd, "C:\\game")) ExitProcess(16);

    ExitProcess(0);
}
