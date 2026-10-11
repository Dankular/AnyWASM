/* Global pointer tables force DIR64 base relocations; also exercises TLS-free statics. */
typedef void *HANDLE;
typedef unsigned long DWORD;
typedef int BOOL;
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);

static const char s0[] = "alpha ", s1[] = "beta ", s2[] = "gamma\r\n";
static const char *volatile table[] = { s0, s1, s2 };
static void (*volatile hook)(const char *);

static void say(const char *s) {
    DWORD n = 0, w;
    while (s[n]) n++;
    WriteFile(GetStdHandle((DWORD)-11), s, n, &w, 0);
}
void mainCRTStartup(void) {
    hook = say;
    for (int i = 0; i < 3; i++) hook(table[i]);
    ExitProcess(0);
}
