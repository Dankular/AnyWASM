/* Guest regression for VerSetConditionMask's version-comparison fields. */
typedef unsigned int DWORD;
typedef unsigned char BYTE;
typedef unsigned short WORD;
typedef unsigned long long ULONGLONG;
typedef long NTSTATUS;

typedef struct {
    DWORD size;
    DWORD major;
    DWORD minor;
    DWORD build;
    DWORD platform;
    unsigned short service_pack[128];
    WORD service_pack_major;
    WORD service_pack_minor;
    WORD suite_mask;
    BYTE product_type;
    BYTE reserved;
} RTL_OSVERSIONINFOEXW;

__declspec(dllimport) void *__stdcall LoadLibraryA(const char *name);
__declspec(dllimport) void *__stdcall GetProcAddress(void *module, const char *name);

__declspec(dllimport) ULONGLONG __stdcall VerSetConditionMask(
    ULONGLONG mask, DWORD type_mask, BYTE condition);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

typedef NTSTATUS (__stdcall *RtlVerifyVersionInfoFn)(
    RTL_OSVERSIONINFOEXW *version, DWORD type_mask, ULONGLONG condition_mask);

void mainCRTStartup(void)
{
    ULONGLONG mask = VerSetConditionMask(0, 0x1, 3);
    if (mask != 0x8000000000000003ULL)
        ExitProcess(10);

    mask = VerSetConditionMask(mask, 0x2, 5);
    if (mask != 0x800000000000002bULL)
        ExitProcess(11);

    mask = VerSetConditionMask(0, 0x20, 3);
    if (mask != 0x8000000000018000ULL)
        ExitProcess(12);

    void *ntdll = LoadLibraryA("ntdll.dll");
    if (!ntdll)
        ExitProcess(13);
    RtlVerifyVersionInfoFn verify = (RtlVerifyVersionInfoFn)GetProcAddress(ntdll, "RtlVerifyVersionInfo");
    if (!verify)
        ExitProcess(14);

    RTL_OSVERSIONINFOEXW version;
    version.size = sizeof(version);
    version.major = 10;
    version.minor = 0;
    version.build = 19000;
    ULONGLONG conditions = 0;
    conditions = VerSetConditionMask(conditions, 0x1, 3); // major >= 10
    conditions = VerSetConditionMask(conditions, 0x2, 3); // minor >= 0
    conditions = VerSetConditionMask(conditions, 0x4, 3); // build >= 19000
    if (verify(&version, 0x7, conditions) != 0)
        ExitProcess(15);

    version.build = 99999;
    if ((unsigned int)verify(&version, 0x7, conditions) != 0xC0000059u)
        ExitProcess(16);

    ExitProcess(0);
}
