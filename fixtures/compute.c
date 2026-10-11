/* Exercises heap, integer + SSE2 double math, indirect calls, and GetProcAddress. */
typedef void *HANDLE;
typedef unsigned long DWORD;
typedef int BOOL;
typedef long long i64;
typedef unsigned long long u64;
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) HANDLE __stdcall GetProcessHeap(void);
__declspec(dllimport) void *__stdcall HeapAlloc(HANDLE, DWORD, u64);
__declspec(dllimport) BOOL __stdcall HeapFree(HANDLE, DWORD, void *);
__declspec(dllimport) HANDLE __stdcall GetModuleHandleA(const char *);
__declspec(dllimport) void *__stdcall GetProcAddress(HANDLE, const char *);
__declspec(dllimport) BOOL __stdcall QueryPerformanceCounter(i64 *);

int _fltused = 0;
static void out(const char *s) {
    DWORD n = 0, w;
    while (s[n]) n++;
    WriteFile(GetStdHandle((DWORD)-11), s, n, &w, 0);
}
static void out_u(u64 v) {
    char b[24]; int i = 23; b[i] = 0;
    if (!v) b[--i] = '0';
    while (v) { b[--i] = '0' + v % 10; v /= 10; }
    out(b + i);
}

static int is_prime(unsigned n) {
    if (n < 2) return 0;
    for (unsigned d = 2; d * d <= n; d++) if (n % d == 0) return 0;
    return 1;
}

typedef int (*binop)(int, int);
static int add(int a, int b) { return a + b; }
static int mul(int a, int b) { return a * b; }

void mainCRTStartup(void) {
    unsigned primes = 0;
    for (unsigned n = 0; n < 20000; n++) primes += is_prime(n);
    out("primes<20000="); out_u(primes); out("\r\n");

    HANDLE h = GetProcessHeap();
    unsigned *arr = HeapAlloc(h, 0, 1000 * sizeof(unsigned));
    for (unsigned i = 0; i < 1000; i++) arr[i] = i * i;
    u64 sum = 0;
    for (unsigned i = 0; i < 1000; i++) sum += arr[i];
    HeapFree(h, 0, arr);
    out("sumsq="); out_u(sum); out("\r\n");

    double x = 1.0;
    for (int i = 0; i < 100; i++) x = x * 1.5 + 0.25;
    out("fp="); out_u((u64)(x / 1e15)); out("\r\n");

    binop ops[2] = { add, mul };
    out("ind="); out_u((u64)(ops[0](6, 7) * 100 + ops[1](6, 7))); out("\r\n");

    void *p = GetProcAddress(GetModuleHandleA("kernel32.dll"), "GetProcessHeap");
    out(p == (void *)GetProcessHeap ? "gpa=ok\r\n" : "gpa=BAD\r\n");
    ExitProcess(primes == 2262 ? 0 : 1);
}
