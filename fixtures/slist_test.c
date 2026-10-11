/* Freestanding Win64 check for InitializeSListHead. */
__declspec(dllimport) void __stdcall InitializeSListHead(void *head);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);

static __declspec(align(16)) volatile unsigned long long head[2] = {
    0x1122334455667788ULL,
    0x99aabbccddeeff00ULL,
};

void mainCRTStartup(void) {
    InitializeSListHead((void *)head);
    ExitProcess(head[0] == 0 && head[1] == 0 ? 0 : 1);
}
