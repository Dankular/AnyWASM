typedef unsigned char BOOLEAN;
typedef unsigned long ULONG;
typedef void *HMODULE;
typedef void *FARPROC;

__declspec(dllimport) HMODULE __stdcall LoadLibraryA(const char *name);
__declspec(dllimport) FARPROC __stdcall GetProcAddress(HMODULE module, const char *name);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

typedef BOOLEAN(__stdcall *SystemFunction036Fn)(void *buffer, ULONG length);

static const char advapi32_name[] = "advapi32.dll";
static const char system_function_036_name[] = "SystemFunction036";
static unsigned char random_bytes[32];

void mainCRTStartup(void) {
    HMODULE advapi32 = LoadLibraryA(advapi32_name);
    if (!advapi32)
        ExitProcess(1);
    SystemFunction036Fn system_function_036 =
        (SystemFunction036Fn)GetProcAddress(advapi32, system_function_036_name);
    if (!system_function_036)
        ExitProcess(2);
    if (!system_function_036(0, 0))
        ExitProcess(3);
    if (!system_function_036(random_bytes, sizeof random_bytes))
        ExitProcess(4);
    ExitProcess(0);
}
